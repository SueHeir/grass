//! [`CheckpointPlugin`] — periodic checkpoints and restart.
//!
//! Register every resource that makes up the simulation state; the plugin
//! saves them (with the [`SimClock`] and, if present, the run's
//! [`Provenance`]) every `every` steps, and restores them at startup when
//! `restart` names a checkpoint file. A run restarted from step `k` and
//! continued to step `n` gives bit-identical state to an uninterrupted run,
//! provided every piece of state is registered.
//!
//! ```rust,ignore
//! app.add_plugins(CheckpointPlugin);
//! app.checkpoint::<Particles>("particles");
//! app.checkpoint::<FlowField>("flow");
//! ```
//!
//! ```toml
//! [checkpoint]
//! every = 1000                 # 0 disables periodic checkpoints
//! dir = "checkpoints"          # relative to the output directory
//! keep = 3                     # newest files kept; 0 keeps all
//! restart = "checkpoints/checkpoint_0000005000.grass"   # optional
//! ```
//!
//! ## File format
//!
//! `GRASSCKPT 1\n`, a little-endian `u64` header length, a TOML header
//! (step, time, provenance, and per-entry name, offset, length and FNV-1a
//! checksum), then each entry's `bincode` bytes. Files are written to a
//! temporary name and renamed, so a crash never leaves a half-written
//! checkpoint under the final name.

use std::path::{Path, PathBuf};

use grass_app::{App, ConfigDescription, Plugin};
use grass_derive::ConfigDescription as DeriveConfigDescription;
use grass_mpi::CommResource;
use grass_scheduler::prelude::*;
use grass_scheduler::{Res, ResMut};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::provenance::fnv1a64;
use crate::{Config, DescribedConfig, Input, Provenance, SimClock, SimClockPlugin};

/// Schedule namespace of the checkpoint systems: after the run plugin's
/// step counter and end check (1000), so a checkpoint records the number of
/// completed steps and the last step of a run can be checkpointed.
pub const CHECKPOINT_NAMESPACE: u32 = 1100;

const MAGIC: &[u8] = b"GRASSCKPT 1\n";

/// `[checkpoint]` section.
#[derive(Debug, Clone, Deserialize, Serialize, DeriveConfigDescription)]
#[serde(deny_unknown_fields)]
#[config_description(
    section = "checkpoint",
    narrative = "Periodic checkpoints of every registered resource, and restart from one."
)]
pub struct CheckpointConfig {
    /// Write a checkpoint every this many steps. 0 disables periodic checkpoints.
    #[serde(default)]
    pub every: u64,
    /// Directory for checkpoint files, relative to the output directory.
    #[serde(default = "default_dir")]
    pub dir: String,
    /// Number of newest checkpoint files to keep. 0 keeps all.
    #[serde(default = "default_keep")]
    pub keep: usize,
    /// Checkpoint file to restart from. Relative paths resolve against the output directory.
    /// In a run with more than one MPI rank, each rank writes its own file
    /// (`checkpoint_<step>.rank<r>.grass`), so this path must contain `{rank}`,
    /// which is replaced by the zero-padded rank (e.g.
    /// `checkpoints/checkpoint_0000000020.rank{rank}.grass`).
    #[serde(default)]
    pub restart: Option<String>,
}

fn default_dir() -> String {
    "checkpoints".to_string()
}
fn default_keep() -> usize {
    2
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            every: 0,
            dir: default_dir(),
            keep: default_keep(),
            restart: None,
        }
    }
}

/// One entry of a checkpoint header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointEntry {
    /// Registered name.
    pub name: String,
    /// Byte offset after the header.
    pub offset: u64,
    /// Byte length.
    pub len: u64,
    /// FNV-1a 64 of the bytes, hex.
    pub fnv1a64: String,
}

/// A checkpoint file's header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointHeader {
    /// Step at which it was written.
    pub step: u64,
    /// Simulation time at which it was written.
    pub time: f64,
    /// The run that wrote it, if a [`Provenance`] resource was present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Entries in file order.
    pub entries: Vec<CheckpointEntry>,
}

/// A checkpoint held in memory: the header plus each entry's bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Checkpoint {
    /// Header.
    pub header: CheckpointHeader,
    /// Entry bytes, in header order.
    pub blobs: Vec<Vec<u8>>,
}

/// Errors reading a checkpoint.
#[derive(Debug)]
pub enum CheckpointError {
    /// The file could not be read or written.
    Io(std::io::Error),
    /// The file is not a grass checkpoint or is truncated or corrupt.
    Format(String),
    /// A registered resource is missing from the file.
    Missing(String),
}

impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "checkpoint I/O: {e}"),
            Self::Format(m) => write!(f, "bad checkpoint: {m}"),
            Self::Missing(n) => write!(f, "checkpoint has no entry `{n}`"),
        }
    }
}

impl std::error::Error for CheckpointError {}

impl From<std::io::Error> for CheckpointError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl Checkpoint {
    fn new(step: u64, time: f64, provenance: Option<Provenance>) -> Self {
        Self {
            header: CheckpointHeader {
                step,
                time,
                provenance,
                entries: Vec::new(),
            },
            blobs: Vec::new(),
        }
    }

    /// Add `value` under `name`.
    pub fn put<T: Serialize>(&mut self, name: &str, value: &T) {
        let bytes = bincode::serialize(value).expect("checkpoint entry serializes");
        let offset = self.blobs.iter().map(|b| b.len() as u64).sum();
        self.header.entries.push(CheckpointEntry {
            name: name.to_string(),
            offset,
            len: bytes.len() as u64,
            fnv1a64: format!("{:016x}", fnv1a64(&bytes)),
        });
        self.blobs.push(bytes);
    }

    /// The value stored under `name`.
    pub fn get<T: DeserializeOwned>(&self, name: &str) -> Result<T, CheckpointError> {
        let i = self
            .header
            .entries
            .iter()
            .position(|e| e.name == name)
            .ok_or_else(|| CheckpointError::Missing(name.to_string()))?;
        bincode::deserialize(&self.blobs[i])
            .map_err(|e| CheckpointError::Format(format!("entry `{name}`: {e}")))
    }

    /// Write atomically to `path` (via a temporary file and a rename).
    pub fn write(&self, path: &Path) -> Result<(), CheckpointError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let header = toml::to_string(&self.header)
            .map_err(|e| CheckpointError::Format(format!("header: {e}")))?;
        let mut out = Vec::with_capacity(
            MAGIC.len() + 8 + header.len() + self.blobs.iter().map(Vec::len).sum::<usize>(),
        );
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(header.len() as u64).to_le_bytes());
        out.extend_from_slice(header.as_bytes());
        for b in &self.blobs {
            out.extend_from_slice(b);
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &out)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Read and verify the checkpoint at `path`.
    pub fn read(path: &Path) -> Result<Self, CheckpointError> {
        let data = std::fs::read(path)?;
        let bad = |m: &str| CheckpointError::Format(format!("{}: {m}", path.display()));
        let rest = data
            .strip_prefix(MAGIC)
            .ok_or_else(|| bad("not a grass checkpoint"))?;
        let (len, rest) = rest.split_at_checked(8).ok_or_else(|| bad("truncated"))?;
        let len = u64::from_le_bytes(len.try_into().unwrap()) as usize;
        let (header, body) = rest
            .split_at_checked(len)
            .ok_or_else(|| bad("truncated header"))?;
        let header: CheckpointHeader =
            toml::from_str(std::str::from_utf8(header).map_err(|_| bad("header is not UTF-8"))?)
                .map_err(|e| bad(&format!("header: {e}")))?;
        let mut blobs = Vec::with_capacity(header.entries.len());
        for e in &header.entries {
            let (a, b) = (e.offset as usize, (e.offset + e.len) as usize);
            let bytes = body
                .get(a..b)
                .ok_or_else(|| bad(&format!("entry `{}` truncated", e.name)))?;
            if format!("{:016x}", fnv1a64(bytes)) != e.fnv1a64 {
                return Err(bad(&format!("entry `{}` checksum mismatch", e.name)));
            }
            blobs.push(bytes.to_vec());
        }
        Ok(Self { header, blobs })
    }
}

// ─── Plugin ─────────────────────────────────────────────────────────────────

/// Checkpoint phases (namespace [`CHECKPOINT_NAMESPACE`]).
#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum CheckpointSchedule {
    /// Registered resources serialize themselves into the pending checkpoint.
    Collect,
    /// The pending checkpoint is written.
    Write,
}

/// Restart phases (setup, namespace [`CHECKPOINT_NAMESPACE`], so they run
/// after the solver's own setup and overwrite its initial state).
#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum RestartSchedule {
    /// The restart file is read.
    Load,
    /// Registered resources are restored from it.
    Apply,
}

/// Checkpoint state: the pending checkpoint while one is being written,
/// the restart file while one is being applied, and every file written.
#[derive(Default)]
pub struct Checkpoints {
    pending: Option<Checkpoint>,
    restart: Option<Checkpoint>,
    /// Paths of checkpoints written by this run, oldest first.
    pub written: Vec<PathBuf>,
    /// Header of the checkpoint this run restarted from, if any.
    pub restarted_from: Option<CheckpointHeader>,
    /// Set to write a checkpoint at the end of this step regardless of `every`.
    pub request: bool,
}

/// Periodic checkpoints of every resource registered with
/// [`CheckpointAppExt::checkpoint`], and restart from a checkpoint file.
pub struct CheckpointPlugin;

impl Plugin for CheckpointPlugin {
    fn build(&self, app: &mut App) {
        let cfg = Config::load_described::<CheckpointConfig>(app);
        if app.get_resource_ref::<SimClock>().is_none() {
            app.add_plugins(SimClockPlugin);
        }
        app.add_resource(Checkpoints::default());
        app.set_schedule_namespace::<CheckpointSchedule>(CHECKPOINT_NAMESPACE);
        app.set_schedule_namespace::<RestartSchedule>(CHECKPOINT_NAMESPACE);
        app.add_update_system(begin_checkpoint, CheckpointSchedule::Collect);
        app.add_update_system(write_checkpoint, CheckpointSchedule::Write);
        if cfg.restart.is_some() {
            app.add_setup_system(load_restart, RestartSchedule::Load);
        }
    }

    fn config_description(&self) -> Option<ConfigDescription> {
        Some(CheckpointConfig::description())
    }
}

fn output_dir(input: &Option<Res<Input>>) -> PathBuf {
    input
        .as_ref()
        .and_then(|i| i.output_dir.clone())
        .map(PathBuf::from)
        .unwrap_or_default()
}

// Runs first in Collect: every resource collector is ordered after it.
fn begin_checkpoint(
    clock: Res<SimClock>,
    cfg: Res<CheckpointConfig>,
    mut ck: ResMut<Checkpoints>,
    prov: Option<Res<Provenance>>,
) {
    let due = cfg.every > 0 && clock.step > 0 && clock.step.is_multiple_of(cfg.every);
    if due || ck.request {
        ck.pending = Some(Checkpoint::new(
            clock.step,
            clock.time,
            prov.map(|p| p.clone()),
        ));
        ck.request = false;
    }
}

/// `Some(rank)` when the run has more than one MPI rank, so each rank needs
/// its own checkpoint file.
fn multi_rank(comm: &Option<Res<CommResource>>) -> Option<i32> {
    comm.as_ref().filter(|c| c.size() > 1).map(|c| c.rank())
}

fn write_checkpoint(
    cfg: Res<CheckpointConfig>,
    mut ck: ResMut<Checkpoints>,
    input: Option<Res<Input>>,
    comm: Option<Res<CommResource>>,
) {
    let Some(c) = ck.pending.take() else {
        return;
    };
    let dir = output_dir(&input).join(&cfg.dir);
    // Every rank holds different data; one shared file name would let the
    // ranks overwrite each other.
    let name = match multi_rank(&comm) {
        Some(rank) => format!("checkpoint_{:010}.rank{rank:05}.grass", c.header.step),
        None => format!("checkpoint_{:010}.grass", c.header.step),
    };
    let path = dir.join(name);
    c.write(&path)
        .unwrap_or_else(|e| panic!("grass_io: writing {}: {e}", path.display()));
    ck.written.push(path);
    if cfg.keep > 0 {
        while ck.written.len() > cfg.keep {
            let old = ck.written.remove(0);
            std::fs::remove_file(&old).ok();
        }
    }
}

fn load_restart(
    cfg: Res<CheckpointConfig>,
    mut ck: ResMut<Checkpoints>,
    mut clock: ResMut<SimClock>,
    input: Option<Res<Input>>,
    comm: Option<Res<CommResource>>,
) {
    let file = cfg.restart.as_ref().expect("restart configured");
    let file = match multi_rank(&comm) {
        Some(rank) if file.contains("{rank}") => file.replace("{rank}", &format!("{rank:05}")),
        Some(_) => panic!(
            "grass_io: restart: `{file}` names one file, but this run has several MPI ranks \
             and each rank wrote its own checkpoint; put `{{rank}}` in the path \
             (e.g. `checkpoint_<step>.rank{{rank}}.grass`)"
        ),
        None => file.replace("{rank}", "00000"),
    };
    let mut path = PathBuf::from(file);
    if path.is_relative() {
        path = output_dir(&input).join(path);
    }
    let c = Checkpoint::read(&path).unwrap_or_else(|e| panic!("grass_io: restart: {e}"));
    clock.step = c.header.step;
    clock.time = c.header.time;
    ck.restarted_from = Some(c.header.clone());
    ck.restart = Some(c);
}

/// Registers resources for checkpointing.
pub trait CheckpointAppExt {
    /// Save resource `T` under `name` in every checkpoint, and restore it on
    /// restart. Requires [`CheckpointPlugin`].
    fn checkpoint<T: Serialize + DeserializeOwned + 'static>(
        &mut self,
        name: &'static str,
    ) -> &mut Self;
}

impl CheckpointAppExt for App {
    fn checkpoint<T: Serialize + DeserializeOwned + 'static>(
        &mut self,
        name: &'static str,
    ) -> &mut Self {
        // Ordered after `begin_checkpoint`, which opens the pending checkpoint,
        // so this works whether it is registered before or after the plugin.
        let collect = move |t: Res<T>, mut ck: ResMut<Checkpoints>| {
            if let Some(c) = ck.pending.as_mut() {
                c.put(name, &*t);
            }
        };
        self.add_update_system(collect.after(begin_checkpoint), CheckpointSchedule::Collect);
        self.add_setup_system(
            move |mut t: ResMut<T>, ck: Res<Checkpoints>| {
                if let Some(c) = ck.restart.as_ref() {
                    *t = c
                        .get(name)
                        .unwrap_or_else(|e| panic!("grass_io: restart: {e}"));
                }
            },
            RestartSchedule::Apply,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trips_and_detects_corruption() {
        let dir = std::env::temp_dir().join(format!("grass_ckpt_unit_{}", std::process::id()));
        let path = dir.join("c.grass");
        let mut c = Checkpoint::new(7, 0.25, None);
        c.put("v", &vec![1.0f64, 2.5, -3.0]);
        c.put("n", &42u32);
        c.write(&path).unwrap();
        let back = Checkpoint::read(&path).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.get::<Vec<f64>>("v").unwrap(), vec![1.0, 2.5, -3.0]);
        assert!(matches!(
            back.get::<u32>("x"),
            Err(CheckpointError::Missing(_))
        ));

        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();
        let err = Checkpoint::read(&path).unwrap_err().to_string();
        assert!(err.contains("checksum"), "{err}");
        std::fs::write(&path, b"nope").unwrap();
        assert!(Checkpoint::read(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
