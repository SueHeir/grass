//! [`DumpPlugin`] — periodic file output, LAMMPS-style.
//!
//! Each iter, a user system fills [`DumpBuffer::payload`] with whatever
//! bytes should be written this frame (JSON, CSV, raw binary — caller
//! decides). Every `interval` steps the plugin resolves a per-frame path
//! from a template, creates parent dirs, and writes the bytes through a
//! pluggable [`DumpFormat`].
//!
//! ## Setup
//!
//! ```rust,ignore
//! app.add_plugins(DumpPlugin::new(RawFrameWriter));
//!
//! fn build_dump(state: Res<MyState>, mut buf: ResMut<DumpBuffer>) {
//!     buf.payload = serde_json::to_vec(&*state).unwrap();
//! }
//! app.add_update_system(build_dump, DumpSchedule::Build);
//! ```
//!
//! ## TOML
//!
//! ```toml
//! [dump]
//! interval = 100
//! path_template = "frame_{step:06}.json"
//! ```
//!
//! Relative paths are resolved against `Input.output_dir` (if present),
//! else against the current working directory. `interval = 0` disables
//! the dump entirely. `{step}` and `{step:0N}` placeholders expand to
//! the current step (zero-padded if requested); `{time}` expands to the
//! current sim time.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use grass_app::{App, Plugin};
use grass_scheduler::{prelude::*, Res, ResMut};
use serde::Deserialize;

use crate::{every_n_steps, Config, Input, SimClock, SimClockPlugin};

/// Schedule namespace for [`DumpSchedule`]. Between [`crate::TERM_OUT_NAMESPACE`]
/// and [`crate::RUN_NAMESPACE`] so dump runs after term_out's print
/// but before the run-end check.
pub const DUMP_NAMESPACE: u32 = 200;

// ─── DumpFormat trait + concrete RawFrameWriter ─────────────────────────────

/// How payload bytes turn into a file frame on disk. Most users want
/// [`RawFrameWriter`] (writes bytes verbatim — JSON / CSV / binary all
/// work) but custom impls can wrap headers, compress, etc.
pub trait DumpFormat: Send + Sync + 'static {
    fn write_frame(
        &mut self,
        path: &Path,
        step: u64,
        time: f64,
        payload: &[u8],
    ) -> std::io::Result<()>;
}

/// Default [`DumpFormat`]: write `payload` to `path` verbatim. Caller
/// decides what those bytes are (JSON, CSV, binary, …).
pub struct RawFrameWriter;

impl DumpFormat for RawFrameWriter {
    fn write_frame(
        &mut self,
        path: &Path,
        _step: u64,
        _time: f64,
        payload: &[u8],
    ) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, payload)
    }
}

// ─── Buffer resource ────────────────────────────────────────────────────────

/// User systems fill `payload` in [`DumpSchedule::Build`]; the plugin's
/// write system reads it in [`DumpSchedule::Write`].
#[derive(Default)]
pub struct DumpBuffer {
    pub payload: Vec<u8>,
}

// ─── Config ─────────────────────────────────────────────────────────────────

fn default_path_template() -> String {
    "frame_{step:06}.bin".to_string()
}

/// `[dump]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DumpConfig {
    /// Write a frame every N steps. 0 disables.
    #[serde(default)]
    pub interval: u64,
    /// Per-frame path. `{step}` / `{step:0N}` / `{time}` expand at
    /// write time. Relative paths resolve against `Input.output_dir`.
    #[serde(default = "default_path_template")]
    pub path_template: String,
}

impl Default for DumpConfig {
    fn default() -> Self {
        Self {
            interval: 0,
            path_template: default_path_template(),
        }
    }
}

// ─── Schedule ───────────────────────────────────────────────────────────────

/// User fills the buffer in `Build`; plugin writes in `Write`.
#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum DumpSchedule {
    Build,
    Write,
}

// ─── Plugin ─────────────────────────────────────────────────────────────────

/// Periodic file output. Generic over [`DumpFormat`] so users can pick
/// JSON / CSV / VTP / custom binary at construction time.
///
/// Depends on [`SimClockPlugin`] (auto-installed).
pub struct DumpPlugin<F: DumpFormat> {
    /// `Plugin: Send + Sync`, and `Plugin::build` takes `&self`, so we
    /// stash the format in a `Mutex<Option<F>>` and `take()` it out
    /// during `build`. Build is called exactly once per plugin instance.
    format: Mutex<Option<F>>,
}

impl<F: DumpFormat> DumpPlugin<F> {
    pub fn new(format: F) -> Self {
        Self {
            format: Mutex::new(Some(format)),
        }
    }
}

impl Default for DumpPlugin<RawFrameWriter> {
    fn default() -> Self {
        Self::new(RawFrameWriter)
    }
}

impl<F: DumpFormat> Plugin for DumpPlugin<F> {
    fn build(&self, app: &mut App) {
        let cfg = Config::load::<DumpConfig>(app, "dump");

        if app.get_resource_ref::<SimClock>().is_none() {
            app.add_plugins(SimClockPlugin);
        }

        app.add_resource(DumpBuffer::default());
        app.set_schedule_namespace::<DumpSchedule>(DUMP_NAMESPACE);

        if cfg.interval > 0 {
            let format = self
                .format
                .lock()
                .expect("DumpPlugin: format mutex poisoned")
                .take()
                .expect("DumpPlugin: build called twice on the same plugin instance");
            app.add_resource(DumpWriter { format });
            app.add_update_system(
                write_dump::<F>.run_if(every_n_steps(cfg.interval)),
                DumpSchedule::Write,
            );
        }
    }

    fn default_config(&self) -> Option<&str> {
        Some(
            r#"# [dump] — periodic per-frame file output.
[dump]
# Write every N steps. 0 disables.
interval = 0
# Per-frame path. {step} / {step:0N} / {time} expand at write time.
# Relative paths resolve against Input.output_dir.
path_template = "frame_{step:06}.bin"
"#,
        )
    }
}

/// Resource holding the live `DumpFormat` instance the write system
/// dispatches to.
struct DumpWriter<F: DumpFormat> {
    format: F,
}

// ─── Systems ────────────────────────────────────────────────────────────────

fn write_dump<F: DumpFormat>(
    clock: Res<SimClock>,
    buffer: Res<DumpBuffer>,
    config: Res<DumpConfig>,
    input: Option<Res<Input>>,
    mut writer: ResMut<DumpWriter<F>>,
) {
    let path = resolve_path(&config.path_template, clock.step, clock.time);
    let path = match input.and_then(|i| i.output_dir.clone()) {
        Some(dir) if path.is_relative() => Path::new(&dir).join(path),
        _ => path,
    };

    if let Err(e) = writer
        .format
        .write_frame(&path, clock.step, clock.time, &buffer.payload)
    {
        eprintln!("dump: failed to write {}: {}", path.display(), e);
    }
}

/// Replace `{step}`, `{step:0N}`, and `{time}` in a template.
fn resolve_path(template: &str, step: u64, time: f64) -> PathBuf {
    let mut out = String::with_capacity(template.len() + 16);
    let mut s = template;
    while let Some(open) = s.find('{') {
        out.push_str(&s[..open]);
        let rest = &s[open + 1..];
        let close = match rest.find('}') {
            Some(c) => c,
            None => {
                out.push_str(&s[open..]);
                return PathBuf::from(out);
            }
        };
        let spec = &rest[..close];
        s = &rest[close + 1..];
        match parse_spec(spec, step, time) {
            Some(formatted) => out.push_str(&formatted),
            None => {
                out.push('{');
                out.push_str(spec);
                out.push('}');
            }
        }
    }
    out.push_str(s);
    PathBuf::from(out)
}

fn parse_spec(spec: &str, step: u64, time: f64) -> Option<String> {
    if let Some(width) = spec.strip_prefix("step:0") {
        let w: usize = width.parse().ok()?;
        Some(format!("{:0w$}", step, w = w))
    } else if spec == "step" {
        Some(format!("{}", step))
    } else if spec == "time" {
        Some(format!("{}", time))
    } else {
        None
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_handles_step_padding() {
        let p = resolve_path("out/frame_{step:06}.json", 42, 0.0);
        assert_eq!(p, PathBuf::from("out/frame_000042.json"));
    }

    #[test]
    fn resolve_path_handles_plain_step() {
        let p = resolve_path("frame_{step}.bin", 7, 0.0);
        assert_eq!(p, PathBuf::from("frame_7.bin"));
    }

    #[test]
    fn resolve_path_handles_time() {
        let p = resolve_path("at_{time}.bin", 0, 0.5);
        assert_eq!(p.to_string_lossy(), "at_0.5.bin");
    }

    #[test]
    fn resolve_path_passes_unknown_specs() {
        let p = resolve_path("foo_{bogus}.bin", 0, 0.0);
        assert_eq!(p, PathBuf::from("foo_{bogus}.bin"));
    }

    #[test]
    fn raw_frame_writer_writes_bytes() {
        let dir = std::env::temp_dir().join("grass_io_dump_test");
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("a.bin");
        let mut w = RawFrameWriter;
        w.write_frame(&path, 0, 0.0, b"hello").unwrap();
        let read = std::fs::read(&path).unwrap();
        assert_eq!(read, b"hello");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plugin_installs_buffer_and_seeds_config() {
        let mut app = App::new();
        app.add_resource(Config::from_str(
            r#"
            [dump]
            interval = 50
            path_template = "x_{step:04}.bin"
            "#,
        ));
        app.add_plugins(DumpPlugin::default());
        let cfg = app
            .get_resource_ref::<DumpConfig>()
            .expect("DumpConfig present");
        assert_eq!(cfg.interval, 50);
        assert_eq!(cfg.path_template, "x_{step:04}.bin");
        assert!(app.get_resource_ref::<DumpBuffer>().is_some());
    }

    #[test]
    fn plugin_no_writer_when_disabled() {
        let mut app = App::new();
        // No [dump] section -> interval = 0 -> writer not registered.
        app.add_plugins(DumpPlugin::default());
        // DumpConfig still installed (with default interval=0):
        let cfg = app
            .get_resource_ref::<DumpConfig>()
            .expect("DumpConfig present");
        assert_eq!(cfg.interval, 0);
        assert!(app.get_resource_ref::<DumpBuffer>().is_some());
    }
}
