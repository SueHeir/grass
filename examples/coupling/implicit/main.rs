//! # `implicit` — implicit (Picard) coupling
//!
//! Same two oscillators as `explicit`, coupled implicitly: each outer
//! iter, iterate the full `[tick A, tick B, exchange]` step until each
//! side's view of the other agrees with the other's actual position to
//! within `tol`. At high coupling stiffness, Picard finds a self-
//! consistent fixed-point that explicit / CSS cannot.
//!
//! Demonstrates `Schedule::Loop` directly — the body restores the saved
//! pre-loop OscState every iter so each trial starts fresh, only the
//! boundary guess (`OtherX`) carries over.
//!
//! All parameters (per-oscillator material + initial state, Picard
//! tolerances, total steps) come from `main.toml`.

use grass_app::prelude::*;
use grass_io::{Config, InputPlugin, MultiIoExt, RunPlugin, RunSchedule};
use grass_multi::{tick_subapp, MultiRes, MultiResMut, Namespace};
use grass_scheduler::prelude::*;
use grass_scheduler::{OnMax, Schedule};
use oscillator_demo::{
    exchange_positions, extract_final_state, OscState, OscillatorPlugin, OtherX, A, B,
};
use serde::Deserialize;

// ─── Config + parent resources ────────────────────────────────────────────

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct ImplicitConfig {
    tol: f64,
    max_inner_iters: u32,
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

// ─── Phase enums ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Stage {
    Save,
}

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum BodyStep {
    Restore,
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

fn compute_residual(
    a_state: MultiRes<OscState, A>,
    b_state: MultiRes<OscState, B>,
    a_other: MultiRes<OtherX, A>,
    b_other: MultiRes<OtherX, B>,
    mut residual: ResMut<Residual>,
) {
    residual.0 = (b_state.x - a_other.0).abs() + (a_state.x - b_other.0).abs();
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

    let implicit: ImplicitConfig = Config::load(&mut parent, "implicit");
    parent.add_resource(OuterState::default());
    parent.add_resource(Residual::default());
    parent.add_resource(Tol(implicit.tol));

    parent.add_update_system(save_outer, Stage::Save);
    parent.add_update_system(restore_outer, BodyStep::Restore);
    parent.add_update_system(tick_subapp(A::NAME, 1), BodyStep::TickA);
    parent.add_update_system(tick_subapp(B::NAME, 1), BodyStep::TickB);
    parent.add_update_system(compute_residual, BodyStep::Residual);
    parent.add_update_system(exchange_positions, BodyStep::UpdateOtherX);

    parent.add_plugins(RunPlugin);

    let schedule = Schedule::builder()
        .then::<Stage>()
        .loop_until(
            picard_converged,
            implicit.max_inner_iters as usize,
            OnMax::AcceptUnconverged,
            |body| body.then::<BodyStep>(),
        )
        .then::<RunSchedule>()
        .build();
    parent.set_schedule(schedule);
    parent.start();

    if parent.get_resource_ref::<GenerateConfigFlag>().is_some() {
        return;
    }
    extract_final_state(&parent).print("implicit");
}
