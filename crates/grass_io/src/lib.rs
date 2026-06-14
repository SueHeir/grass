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
//!   - (coming) `SimClockPlugin` — `step` / `time` resource that everything
//!     periodic gates against.
//!   - (coming) `ThermoPlugin` — periodic terminal output, LAMMPS-style.
//!   - (coming) `DumpPlugin` — periodic file output, LAMMPS-style.
//!
//! Apps that don't want any of this don't depend on `grass_io` at all.
//! Apps that want to swap one piece (e.g. roll their own thermo) skip
//! that plugin and add their own — every piece here is a plugin, not
//! a hardwired assumption.

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
