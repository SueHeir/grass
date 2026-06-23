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
    fn step(&mut self) -> StepResult;

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
}

impl AppPhysics {
    /// Wrap an existing App. Plugins must already be added; the orchestrator
    /// will call [`prepare`](Physics::prepare) before the first step.
    pub fn new(name: impl Into<String>, app: App) -> Self {
        Self {
            name: name.into(),
            app,
            prepared: false,
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
            self.app.prepare();
            self.prepared = true;
        }
    }

    fn step(&mut self) -> StepResult {
        // SchedulerManager state-driven full-step detection is integrator-
        // specific; without that hook we conservatively return true (every
        // call is the boundary). Callers that care about RK substeps can
        // peek at SolverState directly through `Multi` / `MultiRes`.
        self.app.run();
        StepResult {
            completed_full_step: true,
        }
    }

    fn is_done(&self) -> bool {
        self.app.is_done()
    }

    fn cleanup(&mut self) {
        self.app.run_cleanup();
    }

    fn resource_cell(&self, ty: TypeId) -> Option<&RefCell<Box<dyn Any>>> {
        self.app.resource_cell(ty)
    }
}
