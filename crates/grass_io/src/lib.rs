//! # grass_io — TOML config loading + simulation observability for grass
//!
//! Optional companion crate to [`grass_app`]. Provides the things every
//! real simulation wants but the framework core shouldn't be required to
//! pull in:
//!
//!   - [`Config`] + [`InputPlugin`] — read a TOML file at startup, seed
//!     plugin parameters from it. Mirrors DIRT's `Config::load::<T>(app,
//!     "section")` convention so plugins port between the two with no
//!     reshape.
//!   - [`SimClockPlugin`] — `step` / `time` resource ([`SimClock`]) that
//!     everything periodic gates against (see [`every_n_steps`]).
//!   - [`TermOutPlugin`] — periodic terminal output, LAMMPS-style.
//!   - [`DumpPlugin`] — periodic file output, LAMMPS-style.
//!   - [`RunPlugin`] — drives multi-stage `[[run]]` workflows and the
//!     run-end check.
//!
//! All of these **ship today** — they are plugins you opt into, not stubs.
//! Apps that don't want any of this don't depend on `grass_io` at all.
//! Apps that want to swap one piece (e.g. roll their own terminal output)
//! skip that plugin and add their own — every piece here is a plugin, not
//! a hardwired assumption.
//!
//! # Schedule ordering & namespaces
//!
//! `grass_io`'s plugins each pin their schedule enum to a fixed namespace so
//! their systems sort *after* a solver's per-step work, in a deliberate
//! sequence (systems sort by `(namespace, index)`; see `grass_scheduler`):
//!
//! | Namespace | Owner | Runs |
//! |-----------|-------|------|
//! | `0` (default) | **your** solver phases | the actual physics each step |
//! | `100` ([`TERM_OUT_NAMESPACE`]) | [`TermOutPlugin`] | gather + print the terminal log line |
//! | `200` ([`DUMP_NAMESPACE`]) | [`DumpPlugin`] | write periodic dump files |
//! | `1000` ([`RUN_NAMESPACE`]) | [`RunPlugin`] | advance the `[[run]]` stage / signal end-of-run |
//!
//! The gaps are intentional: observability (term_out, dump) sees the
//! step's *final* state because it runs after the solver, and the run-end /
//! stage-advance check runs last so it acts on a fully-updated step.

mod clock;
mod config;
mod dump;
mod run;
mod term_out;

pub use clock::{advance_step, every_n_steps, ClockConfig, SimClock, SimClockPlugin};
pub use config::{deep_merge, load_toml, Config, Input, InputPlugin, MultiIoExt};
pub use dump::{
    DumpBuffer, DumpConfig, DumpFormat, DumpPlugin, DumpSchedule, RawFrameWriter, DUMP_NAMESPACE,
};
pub use run::{
    run_read_input, set_stage_name, update_cycle, validate_stages, FirstStageOnlyConfigs,
    RunConfig, RunPlugin, RunSchedule, RunState, StageConfig, StageOverrides, RUN_NAMESPACE,
};
pub use term_out::{TermOut, TermOutConfig, TermOutPlugin, TermOutSchedule, TERM_OUT_NAMESPACE};
