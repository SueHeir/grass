//! Outer-iter termination — counts loop iterations and signals
//! [`SchedulerState::End`] once the configured target is reached.
//!
//! Drop [`OuterIterStopPlugin`] into the parent App with the phase
//! variant the check should run in. Useful when sub-Apps integrate
//! forever and don't naturally signal done — the plugin gives the parent
//! schedule a clean stopping condition.
//!
//! [`OuterIter`] and [`NIters`] are public so user systems can read the
//! current iter index (e.g. for logging) without reaching into the
//! plugin.

use grass_app::prelude::*;
use grass_scheduler::prelude::*;

/// Counter that increments each time [`check_done_outer_iter`] runs.
/// The plugin slots that system into the user-supplied phase, so the
/// counter ticks once per outer iter.
#[derive(Debug, Default)]
pub struct OuterIter(pub u32);

/// Target iteration count. The check fires `SchedulerState::End` once
/// [`OuterIter`] reaches this value.
#[derive(Debug, Clone, Copy)]
pub struct NIters(pub u32);

/// System: increment [`OuterIter`]; signal end at [`NIters`].
pub fn check_done_outer_iter(
    mut iter: ResMut<OuterIter>,
    n: Res<NIters>,
    mut sm: ResMut<SchedulerManager>,
) {
    iter.0 += 1;
    if iter.0 >= n.0 {
        sm.state = SchedulerState::End;
    }
}

/// Plugin form: registers [`OuterIter`] / [`NIters`] resources and slots
/// [`check_done_outer_iter`] into the user-supplied `phase`.
///
/// ```rust,ignore
/// parent.add_plugins(OuterIterStopPlugin {
///     n_iters: 200,
///     phase: Stage::Check,
/// });
/// ```
///
/// `phase` is generic so callers can target any
/// [`ScheduleSet`](grass_scheduler::ScheduleSet) variant they like.
pub struct OuterIterStopPlugin<P: ScheduleSet + Copy + Send + Sync + 'static> {
    pub n_iters: u32,
    pub phase: P,
}

impl<P: ScheduleSet + Copy + Send + Sync + 'static> Plugin for OuterIterStopPlugin<P> {
    fn build(&self, app: &mut App) {
        app.add_resource(OuterIter::default());
        app.add_resource(NIters(self.n_iters));
        app.add_update_system(check_done_outer_iter, self.phase);
    }
}
