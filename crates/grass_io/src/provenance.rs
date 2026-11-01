//! [`Provenance`] — a record of how a run was produced.
//!
//! [`ProvenancePlugin`] captures the program, its arguments, the working
//! directory, host, user, start time, platform, build profile, the grass
//! version, your application's name, version and git commit (as you pass
//! them in), and a hash of the input file. It installs the record as a
//! resource, writes it to `provenance.toml` in the output directory when the
//! run starts, and checkpoints embed it so a restart file says where it came
//! from.
//!
//! ```rust,ignore
//! app.add_plugins(InputPlugin);
//! app.add_plugins(
//!     ProvenancePlugin::new("heatshield", env!("CARGO_PKG_VERSION"))
//!         .git_commit(option_env!("GIT_COMMIT"))   // set by your build script or CI
//!         .note("case", "PICA arc-jet, 2 MW/m²"),
//! );
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use grass_app::{App, Plugin};
use grass_scheduler::prelude::*;
use grass_scheduler::Res;
use serde::{Deserialize, Serialize};

use crate::Input;

/// How a run was produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// Application name.
    pub app: String,
    /// Application version.
    pub app_version: String,
    /// Application git commit, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    /// Version of `grass_io` (and so of the grass release) the app was built with.
    pub grass_version: String,
    /// Command line, program first.
    pub command: Vec<String>,
    /// Working directory.
    pub cwd: String,
    /// Host name, if it could be determined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// User name, if it could be determined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Start time, seconds since the Unix epoch.
    pub started_unix: u64,
    /// Operating system and architecture, e.g. `linux-x86_64`.
    pub platform: String,
    /// `debug` or `release`.
    pub profile: String,
    /// Input file, if the app was started with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_file: Option<String>,
    /// FNV-1a 64-bit hash of the input file's bytes, in hex. Identifies the
    /// exact input; it is not a cryptographic hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_fnv1a64: Option<String>,
    /// Free-form notes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, String>,
}

impl Provenance {
    /// Capture the current process for application `app` at `version`.
    pub fn capture(app: &str, version: &str) -> Self {
        let host = std::env::var("HOSTNAME")
            .ok()
            .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty());
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok();
        Self {
            app: app.to_string(),
            app_version: version.to_string(),
            git_commit: None,
            grass_version: env!("CARGO_PKG_VERSION").to_string(),
            command: std::env::args().collect(),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            host,
            user,
            started_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_string(),
            input_file: None,
            input_fnv1a64: None,
            notes: BTreeMap::new(),
        }
    }

    /// Record `path` as the input file and hash its contents.
    pub fn with_input_file(mut self, path: &str) -> Self {
        self.input_file = Some(path.to_string());
        self.input_fnv1a64 = std::fs::read(path)
            .ok()
            .map(|b| format!("{:016x}", fnv1a64(&b)));
        self
    }

    /// The record as TOML.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("provenance serializes to TOML")
    }
}

/// FNV-1a, 64 bit.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Captures a [`Provenance`] record and writes it to
/// `<output dir>/provenance.toml` when the run starts.
pub struct ProvenancePlugin {
    app: String,
    version: String,
    git: Option<String>,
    notes: BTreeMap<String, String>,
    write: bool,
}

impl ProvenancePlugin {
    /// Provenance for application `app` at `version`
    /// (usually `env!("CARGO_PKG_VERSION")`).
    pub fn new(app: &str, version: &str) -> Self {
        Self {
            app: app.to_string(),
            version: version.to_string(),
            git: None,
            notes: BTreeMap::new(),
            write: true,
        }
    }

    /// The application's git commit, e.g. `option_env!("GIT_COMMIT")`.
    pub fn git_commit(mut self, commit: Option<&str>) -> Self {
        self.git = commit.map(str::to_string);
        self
    }

    /// Add a free-form note.
    pub fn note(mut self, key: &str, value: &str) -> Self {
        self.notes.insert(key.to_string(), value.to_string());
        self
    }

    /// Keep the record in memory only; do not write `provenance.toml`.
    pub fn in_memory(mut self) -> Self {
        self.write = false;
        self
    }
}

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum ProvenanceSetup {
    Write,
}

impl Plugin for ProvenancePlugin {
    fn build(&self, app: &mut App) {
        let mut p = Provenance::capture(&self.app, &self.version);
        p.git_commit = self.git.clone();
        p.notes = self.notes.clone();
        if let Some(input) = app.get_resource_ref::<Input>() {
            if !input.filename.is_empty() {
                let file = input.filename.clone();
                drop(input);
                p = p.with_input_file(&file);
            }
        }
        app.add_resource(p);
        if self.write {
            app.add_setup_system(write_provenance, ProvenanceSetup::Write);
        }
    }
}

fn write_provenance(p: Res<Provenance>, input: Option<Res<Input>>) {
    let dir = input
        .and_then(|i| i.output_dir.clone())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if let Err(e) = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(dir.join("provenance.toml"), p.to_toml()))
    {
        eprintln!(
            "grass_io: could not write provenance.toml in {}: {e}",
            dir.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_matches_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn record_round_trips_through_toml() {
        let mut p = Provenance::capture("demo", "1.2.3");
        p.git_commit = Some("abc123".into());
        p.notes.insert("case".into(), "baseline".into());
        let back: Provenance = toml::from_str(&p.to_toml()).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.grass_version, env!("CARGO_PKG_VERSION"));
        assert!(!back.command.is_empty());
    }

    #[test]
    fn input_file_is_hashed() {
        let dir = std::env::temp_dir().join(format!("grass_prov_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("in.toml");
        std::fs::write(&f, "foobar").unwrap();
        let p = Provenance::capture("x", "0").with_input_file(f.to_str().unwrap());
        assert_eq!(p.input_fnv1a64.as_deref(), Some("85944171f73967e8"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
