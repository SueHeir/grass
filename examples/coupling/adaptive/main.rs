//! # `adaptive` — adaptive‑dt + implicit Picard coupling.
//!
//! Two nested `Schedule::Loop`s expressing a textbook adaptive coupling:
//!
//!   - **Inner loop** = Picard fixed‑point on `OtherX` at the current
//!     dt. Body refines the boundary guess each iter; exits when the
//!     residual drops below tol.
//!   - **Outer loop** = retries the inner loop with a halved dt if
//!     Picard couldn't converge in the inner budget. Each retry restarts
//!     from the saved outer state with the same `OtherX` guess but a
//!     smaller dt.
//!   - After the outer loop, `grow_dt` gently restores dt back toward
//!     the configured ceiling.
//!
//! All parameters (per-oscillator material + initial state, adaptive
//! tolerances + retry budget, total steps) come from `main.toml`.

use grass_app::prelude::*;
use grass_io::{Config, InputPlugin, MultiIoExt, RunPlugin, RunSchedule};
use grass_multi::{tick_subapp, MultiRes, MultiResMut, Namespace};
use grass_scheduler::prelude::*;
use grass_scheduler::{OnMax, Schedule};
use oscillator_demo::{
    exchange_positions, extract_final_state, OscState, OscillatorPlugin, OtherX, StepSize, A, B,
};
use serde::Deserialize;

// ─── Config + parent resources ────────────────────────────────────────────

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct AdaptiveConfig {
    tol: f64,
    max_inner_iters: u32,
    max_outer_retries: u32,
    initial_dt: f64,
    max_dt: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct OuterState {
    osc_a: OscState,
    osc_b: OscState,
}

#[derive(Debug, Clone, Copy, Default)]
struct Residual(f64);

#[derive(Debug, Clone, Copy)]
struct Tol(f64);

#[derive(Debug, Clone, Copy)]
struct ParentDt(f64);

#[derive(Debug, Clone, Copy)]
struct MaxDt(f64);

// ─── Phase enums ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Stage {
    Save,
    Halve,
    Grow,
}

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum BodyStep {
    Restore,
    ApplyDt,
    TickA,
    TickB,
    Residual,
    UpdateOtherX,
}

// ─── Systems ────────────────────────────────────────────────────────────────

fn save_outer(a: MultiRes<OscState, A>, b: MultiRes<OscState, B>, mut outer: ResMut<OuterState>) {
    outer.osc_a = *a;
    outer.osc_b = *b;
}

fn restore_outer(
    mut a: MultiResMut<OscState, A>,
    mut b: MultiResMut<OscState, B>,
    outer: Res<OuterState>,
) {
    *a = outer.osc_a;
    *b = outer.osc_b;
}

fn apply_dt(
    mut a: MultiResMut<StepSize, A>,
    mut b: MultiResMut<StepSize, B>,
    parent_dt: Res<ParentDt>,
) {
    a.dt = parent_dt.0;
    b.dt = parent_dt.0;
}

fn compute_residual(
    a_state: MultiRes<OscState, A>,
    b_state: MultiRes<OscState, B>,
    a_other: MultiRes<OtherX, A>,
    b_other: MultiRes<OtherX, B>,
    mut residual: ResMut<Residual>,
) {
    residual.0 = (b_state.x - a_other.0).abs() + (a_state.x - b_other.0).abs();
}

fn halve_dt(mut parent_dt: ResMut<ParentDt>) {
    parent_dt.0 *= 0.5;
}

fn grow_dt(mut parent_dt: ResMut<ParentDt>, max_dt: Res<MaxDt>) {
    parent_dt.0 = (parent_dt.0 * 1.5).min(max_dt.0);
}

fn dt_should_shrink(r: Res<Residual>, t: Res<Tol>) -> bool {
    r.0 > t.0
}

fn picard_converged(r: Res<Residual>, t: Res<Tol>) -> bool {
    r.0 < t.0
}

// ─── Main ───────────────────────────────────────────────────────────────────

fn main() {
    let mut parent = App::new();
    parent.add_plugins(InputPlugin);

    parent.add_subapp_with_config(A::NAME, |app| {
        app.add_plugins(OscillatorPlugin);
    });
    parent.add_subapp_with_config(B::NAME, |app| {
        app.add_plugins(OscillatorPlugin);
    });

    let adaptive: AdaptiveConfig = Config::load(&mut parent, "adaptive");
    parent.add_resource(OuterState::default());
    parent.add_resource(Residual::default());
    parent.add_resource(Tol(adaptive.tol));
    parent.add_resource(ParentDt(adaptive.initial_dt));
    parent.add_resource(MaxDt(adaptive.max_dt));

    parent.add_update_system(save_outer, Stage::Save);
    parent.add_update_system(restore_outer, BodyStep::Restore);
    parent.add_update_system(apply_dt, BodyStep::ApplyDt);
    parent.add_update_system(tick_subapp(A::NAME, 1), BodyStep::TickA);
    parent.add_update_system(tick_subapp(B::NAME, 1), BodyStep::TickB);
    parent.add_update_system(compute_residual, BodyStep::Residual);
    parent.add_update_system(exchange_positions, BodyStep::UpdateOtherX);
    parent.add_update_system(halve_dt.run_if(dt_should_shrink), Stage::Halve);
    parent.add_update_system(grow_dt, Stage::Grow);

    parent.add_plugins(RunPlugin);

    let max_inner = adaptive.max_inner_iters as usize;
    let max_outer = adaptive.max_outer_retries as usize;
    let schedule = Schedule::builder()
        .then_variant(Stage::Save)
        .loop_until(
            picard_converged,
            max_outer,
            OnMax::AcceptUnconverged,
            |outer_body| {
                outer_body
                    .loop_until(
                        picard_converged,
                        max_inner,
                        OnMax::AcceptUnconverged,
                        |inner_body| inner_body.then::<BodyStep>(),
                    )
                    .then_variant(Stage::Halve)
            },
        )
        .then_variant(Stage::Grow)
        .then::<RunSchedule>()
        .build();
    parent.set_schedule(schedule);
    parent.start();

    if parent.get_resource_ref::<GenerateConfigFlag>().is_some() {
        return;
    }
    extract_final_state(&parent).print("adaptive");
}
