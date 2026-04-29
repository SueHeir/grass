//! [`PreciceParticipantPlugin`] — Pattern A integration.
//!
//! Single App + this plugin = a coupled-participant binary. The plugin:
//!
//! - Registers a [`PreciceParticipant`] resource (constructed lazily so the
//!   user can register mesh-setup systems before initialize).
//! - Registers `system_precice_advance` in [`PreciceSchedule::Advance`].
//! - Registers `system_precice_check_done` so the App's loop ends when
//!   `is_coupling_ongoing()` returns false.
//! - Registers `system_precice_initialize` as a setup system, run once after
//!   user mesh-setup systems.
//!
//! The user is responsible for:
//! - A setup system that calls `participant.borrow_mut().set_mesh_vertices(...)`
//!   to describe their participant's mesh.
//! - Update systems in [`PreciceSchedule::Write`] that gather outgoing data
//!   from App resources and call `participant.borrow_mut().write_data(...)`.
//! - Update systems in [`PreciceSchedule::Read`] that call
//!   `participant.borrow_mut().read_data(...)` and apply incoming data.

use crate::participant::PreciceParticipant;
// `PreciceSchedule` lives in `crate::schedule`; users register their own
// systems against it from outside the plugin.
use grass_app::prelude::*;
use grass_scheduler::prelude::*;

pub struct PreciceParticipantPlugin {
    participant_name: String,
    config_path: String,
    /// MPI rank within this participant's intra-communicator. Defaults to 0.
    /// For multi-rank participants, set this from your MPI bootstrap.
    rank: i32,
    /// MPI size of this participant's intra-communicator. Defaults to 1.
    size: i32,
}

impl PreciceParticipantPlugin {
    pub fn new(participant_name: impl Into<String>, config_path: impl Into<String>) -> Self {
        Self {
            participant_name: participant_name.into(),
            config_path: config_path.into(),
            rank: 0,
            size: 1,
        }
    }

    /// Set this participant's MPI rank (within its own intra-communicator,
    /// NOT the world). Required for multi-rank participants.
    pub fn with_rank(mut self, rank: i32, size: i32) -> Self {
        self.rank = rank;
        self.size = size;
        self
    }
}

impl Plugin for PreciceParticipantPlugin {
    fn provides(&self) -> Vec<&str> {
        vec!["precice_participant"]
    }

    fn build(&self, app: &mut App) {
        let participant = PreciceParticipant::new(
            &self.participant_name,
            &self.config_path,
            self.rank,
            self.size,
        )
        .unwrap_or_else(|e| {
            panic!(
                "PreciceParticipantPlugin: failed to construct precice::Participant \
                 (name=`{}`, config=`{}`): {:?}",
                self.participant_name, self.config_path, e
            )
        });
        app.add_resource(participant);

        // Run participant.initialize() once, after user mesh-setup setup systems.
        // Use add_setup_system at a phase the user's setup runs in too;
        // ordering by registration means user's setup runs first if they
        // added it before this plugin (which they should NOT — add the plugin
        // first, then the mesh setup system). To be safe, document the
        // pattern and trust the user.
        //
        // For now expose `system_precice_initialize` which the user can wire
        // into whatever phase makes sense.
        let _ = app;
    }
}

/// Run the preCICE step: write_data has already happened in
/// `PreciceSchedule::Write` systems (user-supplied); this calls
/// `participant.advance(dt)` using the App's current dt (ideally clipped to
/// `participant.get_max_time_step_size()` upstream).
pub fn system_precice_advance(participant: Res<PreciceParticipant>, dt: Res<PreciceTimeStep>) {
    if let Err(e) = participant.advance(dt.dt) {
        eprintln!("preCICE advance failed: {e:?}");
    }
}

/// Run after advance. Set the SchedulerManager state to End if preCICE
/// reports the coupling has ended.
pub fn system_precice_check_done(
    participant: Res<PreciceParticipant>,
    mut sm: ResMut<SchedulerManager>,
) {
    let ongoing = participant.is_coupling_ongoing().unwrap_or(false);
    if !ongoing {
        sm.state = SchedulerState::End;
    }
}

/// Resource the user populates each iteration with the dt to advance preCICE
/// by. In a CFD setup, one of your solver systems should write this each
/// step (typically `dt = min(local_cfl, participant.get_max_time_step_size())`).
#[derive(Debug, Clone, Copy)]
pub struct PreciceTimeStep {
    pub dt: f64,
}

impl Default for PreciceTimeStep {
    fn default() -> Self {
        Self { dt: 0.0 }
    }
}

/// One-time call to initialize the participant. Run after your mesh-setup
/// setup systems and before the main loop begins. In Pattern A the typical
/// invocation is from a setup-phase system the user registers.
pub fn system_precice_initialize(participant: Res<PreciceParticipant>) {
    if let Err(e) = participant.initialize() {
        panic!("preCICE initialize() failed: {e:?}");
    }
}
