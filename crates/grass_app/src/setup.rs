//! [`ScheduleSetupSet`] — generic 3-phase ordering for one-time setup systems.
//!
//! Setup systems run once per stage in order:
//! `PreSetup` → `Setup` → `PostSetup`.
//!
//! ```rust,ignore
//! use grass_app::ScheduleSetupSet;
//!
//! app.add_setup_system(load_config_resource, ScheduleSetupSet::PreSetup);
//! app.add_setup_system(initialize_state,    ScheduleSetupSet::Setup);
//! app.add_setup_system(emit_initial_output, ScheduleSetupSet::PostSetup);
//! ```
//!
//! This is the generic equivalent of physics-domain "execute every step"
//! enums (e.g. a velocity-Verlet `ParticleSimScheduleSet`); both kinds of
//! enum are first-class to the scheduler. Plugins that want to be
//! reusable across simulation domains should reach for this enum
//! rather than declaring their own per-codebase setup-ordering enum.

use grass_scheduler::ScheduleSet;

/// Execution phase during one-time setup (before the run loop begins).
#[derive(Debug, Clone, Copy)]
pub enum ScheduleSetupSet {
    /// Runs before the main setup phase (e.g., early resource initialization,
    /// loading config sections that other setup systems read).
    PreSetup,
    /// Main setup phase (e.g., creating neighbor lists, reading restart files).
    Setup,
    /// Runs after setup (e.g., initial force computation, diagnostics).
    PostSetup,
}

impl ScheduleSet for ScheduleSetupSet {
    fn to_index(&self) -> u32 {
        match self {
            ScheduleSetupSet::PreSetup => 0,
            ScheduleSetupSet::Setup => 1,
            ScheduleSetupSet::PostSetup => 2,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            ScheduleSetupSet::PreSetup => "PreSetup",
            ScheduleSetupSet::Setup => "Setup",
            ScheduleSetupSet::PostSetup => "PostSetup",
        }
    }
}
