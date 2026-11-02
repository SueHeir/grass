//! The [`Physics`] trait — uniform interface for one steppable sub-App.
//!
//! Concrete impls in this crate: [`AppPhysics`] wraps a
//! [`grass_app::App`] for the local case;
//! [`RemoteMirrorPhysics`](crate::RemoteMirrorPhysics) wraps a
//! [`Transport`](crate::Transport) for the cross-process case. Both slot
//! into the parent's [`SubApps`](crate::SubApps) machinery uniformly, so
//! `tick_subapp` and the [`Multi`](crate::Multi) /
//! [`MultiRes`](crate::MultiRes) / [`MultiResMut`](crate::MultiResMut)
//! SystemParams treat them identically.
//!
//! Cross-namespace SystemParams reach into a physics's resources via
//! [`Physics::resource_cell`], which exposes the raw
//! `RefCell<Box<dyn Any>>` so the typed accessors can downcast.

use grass_app::App;
use std::any::{Any, TypeId};
use std::cell::RefCell;

/// Progress made while advancing one physics participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicsProgress {
    /// A local participant yielded at an exported schedule seam.
    Yielded(grass_scheduler::ScheduleSeam),
    /// One complete participant timestep finished.
    Complete,
}

/// Outcome of one [`Physics::step`] call.
#[derive(Debug, Clone, Copy)]
pub struct StepResult {
    /// **Reserved — not yet consumed by this crate.** Intended to read `true`
    /// when this substep finished a full timestep, so the orchestrator could
    /// fire couplers / write outputs / advance shared time on that boundary
    /// (single-stage Euler → every substep; RK3 → every third). Today every
    /// [`Physics::step`] impl hard-codes `true` and nothing reads the field;
    /// couplers run on the parent schedule's phase boundaries instead. Don't
    /// rely on it discriminating substeps until a consumer lands.
    pub completed_full_step: bool,
}

impl Default for StepResult {
    fn default() -> Self {
        Self {
            completed_full_step: true,
        }
    }
}

/// A self-contained physics subsystem driven by the multi-physics orchestrator.
///
/// Implementors expose:
/// - **Identity** (`name`) for accessor lookups in couplers.
/// - **Lifecycle** (`prepare` once, then repeated `step` until `is_done`,
///   then `cleanup`).
/// - **Resource access** (`resource_cell`) so cross-physics couplers can read
///   and write internal state.
/// - **Optional time / stability hooks** ([`time`](Self::time),
///   [`max_stable_dt`](Self::max_stable_dt), [`set_dt`](Self::set_dt)) that
///   adaptive strategies *will* use. Default implementations return `None`
///   / no-op, signalling "I don't expose this — treat me as fixed-rate".
///   **Reserved — not yet consumed:** no code in `grass_multi` currently
///   calls these three; they define the interface an adaptive-dt
///   orchestrator will read once one exists.
pub trait Physics: 'static {
    /// Stable identifier (e.g. `"cfd"`, `"dem"`) used for accessor lookups.
    fn name(&self) -> &str;

    /// Run any one-shot setup (organize systems, run setup-phase systems,
    /// initialize SchedulerManager). Called once by the orchestrator before
    /// the first [`step`](Self::step).
    fn prepare(&mut self);

    /// Advance one internal substep. Whoever drives the parent schedule
    /// decides how many times per outer iter this is called — typically
    /// once per [`tick_subapp(name, n)`](crate::tick_subapp) registration.
    /// A finished subsystem must not advance; it returns a full-step result.
    fn step(&mut self) -> StepResult;

    /// Advance one whole physical time step: [`step`](Self::step) until a
    /// step reports `completed_full_step`, or the subsystem is done.
    fn advance_step(&mut self) {
        while !self.is_done() {
            if self.step().completed_full_step {
                break;
            }
        }
    }

    /// Advance to the next exported seam or finish one timestep. Physics
    /// implementations without seam support remain atomic by default.
    fn resume(&mut self) -> PhysicsProgress {
        self.step();
        PhysicsProgress::Complete
    }

    /// Returns `true` if this subsystem has signalled it's done (e.g. by a
    /// `system_check_done` setting `SchedulerManager::state = End`).
    fn is_done(&self) -> bool;

    /// Run any cleanup hooks (e.g. `finalize_mpi`). Called once by the
    /// orchestrator after the loop ends.
    fn cleanup(&mut self);

    /// Raw access to a resource cell by [`TypeId`]. Returns `None` if no
    /// resource of that type is registered. Cross-namespace SystemParams
    /// (`Multi` / `MultiRes` / `MultiResMut`) downcast through this.
    fn resource_cell(&self, ty: TypeId) -> Option<&RefCell<Box<dyn Any>>>;

    /// Validate that a cross-namespace read may observe this resource.
    /// Local physics are always readable; remote mirrors override this to
    /// reject reads whose receive slot has not completed a pump.
    fn validate_resource_read(&self, _ty: TypeId, _type_name: &'static str) {}

    /// Record a cross-namespace mutable borrow. Remote mirrors use this to
    /// track values changed locally since their last successful send.
    fn mark_resource_written(&self, _ty: TypeId) {}

    /// Mutable access to the underlying local [`App`], when this physics is
    /// backed by one. Remote physics return `None`.
    ///
    /// This hook exists so a parent-level coupling plugin can install the
    /// adapter resources and systems required at a seam without either solver
    /// having to know about its eventual coupling partner. Configuration must
    /// happen before the sub-App is prepared.
    fn local_app_mut(&mut self) -> Option<&mut App> {
        None
    }

    /// Shared access to the underlying local [`App`], when there is one
    /// (used to save and restore coupled checkpoints).
    fn local_app(&self) -> Option<&App> {
        None
    }

    /// Send this subsystem's terminal output ([`grass_app::say!`]) to
    /// `output`. Default: ignored (remote physics print on their own rank).
    fn set_output(&mut self, _output: grass_app::Output) {}

    /// Current physical time of this subsystem (seconds), if it tracks
    /// one. Default: `None` — subsystem doesn't expose a clock.
    ///
    /// **Reserved — not yet consumed by this crate.**
    fn time(&self) -> Option<f64> {
        None
    }

    /// Largest stable timestep this subsystem can take *right now* given
    /// its current state (CFL bound, contact-time bound, etc.). Default:
    /// `None` — subsystem is fixed-rate or doesn't report stability.
    ///
    /// **Reserved — not yet consumed by this crate.**
    fn max_stable_dt(&self) -> Option<f64> {
        None
    }

    /// Externally-imposed timestep. Default: no-op — subsystem ignores
    /// external dt.
    ///
    /// **Reserved — not yet consumed by this crate.**
    fn set_dt(&mut self, _dt: f64) {}
}

/// Wraps a [`grass_app::App`] as a [`Physics`]. Each physics owns its own
/// App, its own scheduler, and its own resource store — the "isolated" mode.
pub struct AppPhysics {
    name: String,
    app: App,
    prepared: bool,
    output: grass_app::Output,
}

impl AppPhysics {
    /// Wrap an existing App. Plugins must already be added; the orchestrator
    /// will call [`prepare`](Physics::prepare) before the first step.
    /// Its terminal output is labeled `[name] ` until
    /// [`set_output`](Physics::set_output) says otherwise.
    pub fn new(name: impl Into<String>, app: App) -> Self {
        let name = name.into();
        let output = grass_app::Output::prefixed(format!("[{name}] "));
        Self {
            name,
            app,
            prepared: false,
            output,
        }
    }

    /// Borrow the underlying App. Useful from couplers and tests when raw
    /// access is needed (e.g. to register an additional resource after
    /// construction).
    pub fn app(&self) -> &App {
        &self.app
    }

    /// Borrow the underlying App mutably. Most users should not need this —
    /// the orchestrator drives the lifecycle.
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }
}

impl Physics for AppPhysics {
    fn name(&self) -> &str {
        &self.name
    }

    fn prepare(&mut self) {
        if !self.prepared {
            let app = &mut self.app;
            self.output.scope(|| {
                app.prepare();
            });
            self.prepared = true;
        }
    }

    fn step(&mut self) -> StepResult {
        // A finished App is not run again: its `[run]` stages are used up,
        // and stepping it further would index past them. Report a boundary
        // so callers that loop to a full step stop.
        if self.app.is_done() {
            return StepResult {
                completed_full_step: true,
            };
        }
        let app = &mut self.app;
        self.output.scope(|| {
            app.run();
        });
        let completed_full_step = self
            .app
            .get_resource_ref::<grass_app::StepBoundary>()
            .is_none_or(|boundary| boundary.completed_full_step);
        StepResult {
            completed_full_step,
        }
    }

    fn resume(&mut self) -> PhysicsProgress {
        if self.app.is_done() {
            return PhysicsProgress::Complete;
        }
        let app = &mut self.app;
        match self.output.scope(|| app.resume()) {
            grass_scheduler::ScheduleProgress::Yielded(seam) => PhysicsProgress::Yielded(seam),
            grass_scheduler::ScheduleProgress::Complete => PhysicsProgress::Complete,
        }
    }

    fn is_done(&self) -> bool {
        self.app.is_done()
    }

    fn cleanup(&mut self) {
        let app = &mut self.app;
        self.output.scope(|| app.run_cleanup());
    }

    fn set_output(&mut self, output: grass_app::Output) {
        self.output = output;
    }

    fn resource_cell(&self, ty: TypeId) -> Option<&RefCell<Box<dyn Any>>> {
        self.app.resource_cell(ty)
    }

    fn local_app_mut(&mut self) -> Option<&mut App> {
        Some(&mut self.app)
    }

    fn local_app(&self) -> Option<&App> {
        Some(&self.app)
    }
}
