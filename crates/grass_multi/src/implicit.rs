//! Implicit coupling over time windows.
//!
//! [`converge_outer_iter`](crate::converge_outer_iter) solves *one* interface
//! fixed point and stops. A time-dependent strongly coupled run needs one
//! fixed point per coupling window: every solver advances over the window,
//! the interface residual is checked, and if it has not converged every
//! solver goes back to the start of the window and tries again with a better
//! interface guess. Once it converges the window is accepted and the next
//! one begins.
//!
//! [`ImplicitCoupling`] holds that state machine around an
//! [`OuterIteration`]; two system constructors drive it from the parent
//! schedule:
//!
//! * [`checkpoint_window`] runs **first** in each parent iteration. At the
//!   start of a new window it saves every solver's state; on a repeated
//!   window it restores it.
//! * [`converge_window`] runs **last**. It reads the raw interface output,
//!   applies the acceleration, writes the next guess back, and either repeats
//!   the window or accepts it (stopping the parent after the last window).
//!
//! ```rust,ignore
//! parent.add_resource(ImplicitCoupling::new(
//!     OuterIteration::new(x0, Relaxation::IqnIls { omega0: 0.1, reuse: 8, filter: 1e-6 }, 1e-8, 30),
//!     dt, n_windows,
//! ));
//! parent.add_update_system(checkpoint_window(save_both, restore_both), Phase::Checkpoint);
//! parent.add_update_system(tick_subapp("fluid", 4), Phase::TickFluid);   // 4 substeps
//! parent.add_update_system(/* fluid → solid port */, Phase::Couple);
//! parent.add_update_system(tick_subapp("solid", 1), Phase::TickSolid);
//! parent.add_update_system(converge_window(read_solid, write_fluid), Phase::Converge);
//! ```
//!
//! Subcycling is just a different substep count per solver; pass
//! [`Waveform`](crate::Waveform)s through the ports so the faster solver can
//! interpolate in time.

use grass_scheduler::prelude::{ResMut, SchedulerManager, SchedulerState};

use crate::multi::Multi;
use crate::relax::OuterIteration;

/// What [`converge_window`] decided after an iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowStep {
    /// The window did not converge and will be repeated from its checkpoint
    /// with the new interface guess.
    Repeat,
    /// The window was accepted. The next window starts from the solvers'
    /// current state, with the new interface guess as its first iterate.
    Advance,
}

/// The time-window state machine for implicit coupling. Add it to the parent
/// as a resource.
#[derive(Debug, Clone)]
pub struct ImplicitCoupling {
    iter: OuterIteration,
    dt: f64,
    window: u64,
    n_windows: u64,
    needs_checkpoint: bool,
    iters_per_window: Vec<u32>,
    unconverged: Vec<u64>,
}

impl ImplicitCoupling {
    /// Couple `n_windows` windows of length `dt`. `iter` sets the
    /// acceleration, the tolerance and the per-window iteration cap; a
    /// window that hits the cap is accepted anyway and recorded in
    /// [`unconverged_windows`](Self::unconverged_windows).
    pub fn new(iter: OuterIteration, dt: f64, n_windows: u64) -> Self {
        Self {
            iter,
            dt,
            window: 0,
            n_windows,
            needs_checkpoint: true,
            iters_per_window: Vec::new(),
            unconverged: Vec::new(),
        }
    }

    /// The outer iteration (current iterate, residual, scheme).
    pub fn iteration(&self) -> &OuterIteration {
        &self.iter
    }

    /// Index of the current window.
    pub fn window(&self) -> u64 {
        self.window
    }

    /// Window length.
    pub fn dt(&self) -> f64 {
        self.dt
    }

    /// Start time of the current window (windows start at `0`).
    pub fn window_start(&self) -> f64 {
        self.window as f64 * self.dt
    }

    /// Iterations each accepted window took.
    pub fn iterations_per_window(&self) -> &[u32] {
        &self.iters_per_window
    }

    /// Windows that hit the iteration cap without converging.
    pub fn unconverged_windows(&self) -> &[u64] {
        &self.unconverged
    }

    /// Whether every window has been accepted.
    pub fn finished(&self) -> bool {
        self.window >= self.n_windows
    }
}

/// System constructor: save every solver's state at the start of a window
/// and restore it when the window is repeated. Register it in the first
/// parent phase, before any solver ticks.
pub fn checkpoint_window<S, R>(save: S, restore: R) -> impl FnMut(Multi, ResMut<ImplicitCoupling>)
where
    S: Fn(&Multi) + 'static,
    R: Fn(&Multi) + 'static,
{
    move |world: Multi, mut ic: ResMut<ImplicitCoupling>| {
        if ic.needs_checkpoint {
            save(&world);
            ic.needs_checkpoint = false;
        } else {
            restore(&world);
        }
    }
}

/// System constructor: one implicit-coupling iteration. Register it in the
/// last parent phase, after every solver has advanced over the window.
///
/// `read_output` returns the raw interface output `x̃`. `write_input`
/// receives the next iterate and whether the window will be repeated or the
/// next one begins (on [`WindowStep::Advance`], shift any waveform inputs to
/// the new window). The parent stops after the last window.
pub fn converge_window<Rd, Wr>(
    read_output: Rd,
    write_input: Wr,
) -> impl FnMut(Multi, ResMut<ImplicitCoupling>, ResMut<SchedulerManager>)
where
    Rd: Fn(&Multi) -> Vec<f64> + 'static,
    Wr: Fn(&Multi, &[f64], WindowStep) + 'static,
{
    move |world: Multi, mut ic: ResMut<ImplicitCoupling>, mut sm: ResMut<SchedulerManager>| {
        let x_tilde = read_output(&world);
        let next = ic.iter.observe(&x_tilde).to_vec();
        if !ic.iter.done() {
            write_input(&world, &next, WindowStep::Repeat);
            return;
        }
        let ic = &mut *ic;
        ic.iters_per_window.push(ic.iter.iters());
        if !ic.iter.converged() {
            ic.unconverged.push(ic.window);
        }
        ic.iter.begin_window();
        ic.window += 1;
        ic.needs_checkpoint = true;
        write_input(&world, &next, WindowStep::Advance);
        if ic.finished() {
            sm.state = SchedulerState::End;
        }
    }
}
