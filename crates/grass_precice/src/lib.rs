//! # grass_precice — preCICE integration for the grass simulation framework
//!
//! Wraps the [`precice`](https://docs.rs/precice) Rust bindings as a single
//! plugin that registers a `PreciceParticipant` resource on a normal
//! [`grass_app::App`] and slots three systems —
//! `system_precice_initialize`, `system_precice_advance`,
//! `system_precice_check_done` — into the schedule, so each iter
//! automatically does `Write → Advance → Read` and the App's "done" signal
//! is `precice.is_coupling_ongoing()`.
//!
//! You write the `Write` and `Read` systems yourself: read App resources
//! and call `PreciceParticipant::write_data`; call
//! `PreciceParticipant::read_data` and apply the values to App resources.
//! The plugin glues them together in the right order.
//!
//! ```rust,ignore
//! let mut app = App::default();
//! app.add_plugins(MyCfdPlugins::from_config(cfg));
//! app.add_plugins(PreciceParticipantPlugin::new("FluidSolver", "precice-config.xml"));
//! app.add_update_system(write_pressure_to_precice, PreciceSchedule::Write);
//! app.add_update_system(read_forces_from_precice,  PreciceSchedule::Read);
//! app.start();
//! ```
//!
//! ## Feature flag
//!
//! The `precice` feature gates the actual `precice` crate dependency. With
//! it off (the default), this crate exports stub types that explain at use
//! time how to enable real preCICE. With it on, the real `Participant`
//! wrapper compiles. Use `cargo build --features precice` (or
//! `--features grass_precice/precice` from a parent crate) when you have
//! libprecice installed.

#[cfg(feature = "precice")]
mod participant;
#[cfg(feature = "precice")]
mod plugin;
#[cfg(feature = "precice")]
mod schedule;

#[cfg(feature = "precice")]
pub use participant::PreciceParticipant;
#[cfg(feature = "precice")]
pub use plugin::{
    system_precice_advance, system_precice_check_done, system_precice_initialize,
    PreciceParticipantPlugin, PreciceTimeStep,
};
#[cfg(feature = "precice")]
pub use schedule::PreciceSchedule;

/// Re-export the `precice` crate so users don't need to declare it as a
/// separate dep just to call methods on `Participant` directly.
#[cfg(feature = "precice")]
pub use precice;

// ─── Stub mode (no precice feature) ─────────────────────────────────────────

/// Compiled when the `precice` feature is **off**. Trying to instantiate this
/// type prints a helpful diagnostic and panics. Enable the feature with
/// `cargo build --features grass_precice/precice` and ensure libprecice C++
/// is installed (Homebrew: `brew install precice`; Linux: see
/// <https://precice.org/installation-overview.html>).
#[cfg(not(feature = "precice"))]
pub struct PreciceParticipantPlugin {
    _disabled: (),
}

#[cfg(not(feature = "precice"))]
impl PreciceParticipantPlugin {
    pub fn new(_name: impl Into<String>, _config_path: impl Into<String>) -> Self {
        panic!(
            "grass_precice: built without the `precice` feature. \
             Enable with `--features grass_precice/precice` and install libprecice. \
             See https://precice.org/installation-overview.html"
        );
    }
}
