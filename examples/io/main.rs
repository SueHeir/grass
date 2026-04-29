//! # `io` — single oscillator + clock + term_out + dump, all driven from
//! one TOML file.
//!
//! Demonstrates the `grass_io` plugin trio (`SimClock` / `TermOut` /
//! `Dump`) plus `RunPlugin` and the TOML-configured `OscillatorPlugin`,
//! all wired against a plain (no-coupling) `App`. Each plugin reads its
//! own section from the main config; user-side wiring is two tiny
//! systems for column-pushing + dump-payload building.
//!
//! Run:
//!
//! ```sh
//! cargo run --example io -- examples/io/main.toml
//! ```
//!
//! Generate a starter config from every plugin's defaults:
//!
//! ```sh
//! cargo run --example io -- --generate-config
//! ```

use grass_app::prelude::*;
use grass_io::{
    advance_step, every_n_steps, DumpBuffer, DumpPlugin, DumpSchedule, InputPlugin, RunPlugin,
    SimClock, TermOut, TermOutPlugin, TermOutSchedule,
};
use grass_scheduler::prelude::*;
use oscillator_demo::{OscParams, OscSchedule, OscState, OscillatorPlugin, StepSize};
use serde::Serialize;

// ─── User systems ──────────────────────────────────────────────────────────

fn set_columns(state: Res<OscState>, params: Res<OscParams>, mut term: ResMut<TermOut>) {
    let kinetic = 0.5 * params.mass * state.v * state.v;
    let potential = 0.5 * params.k_self * state.x * state.x;
    term.set("x", state.x);
    term.set("v", state.v);
    term.set("energy", kinetic + potential);
}

#[derive(Serialize)]
struct DumpFrame {
    step: u64,
    time: f64,
    x: f64,
    v: f64,
}

fn build_dump_payload(clock: Res<SimClock>, state: Res<OscState>, mut buffer: ResMut<DumpBuffer>) {
    let frame = DumpFrame {
        step: clock.step,
        time: clock.time,
        x: state.x,
        v: state.v,
    };
    buffer.payload = serde_json::to_vec_pretty(&frame).expect("serialize DumpFrame to JSON");
}

fn advance_time(mut clock: ResMut<SimClock>, step: Res<StepSize>) {
    clock.time += step.dt;
}

// ─── Main ──────────────────────────────────────────────────────────────────

fn main() {
    let mut app = App::new();

    app.add_plugins(InputPlugin);
    app.add_plugins(OscillatorPlugin);
    app.add_plugins(TermOutPlugin);
    app.add_plugins(DumpPlugin::default());

    // Wire advance_step in OscSchedule::Step BEFORE adding RunPlugin so
    // its has_update_system guard sees the existing registration and
    // skips its default auto-placement (in RunSchedule::Check). We want
    // step++ before TermOut reads it, so it shows the just-finished step.
    app.add_update_system(advance_step, OscSchedule::Step);
    app.add_update_system(advance_time, OscSchedule::Step);
    app.add_update_system(set_columns, TermOutSchedule::Compute);
    app.add_update_system(
        build_dump_payload.run_if(every_n_steps(50)),
        DumpSchedule::Build,
    );

    app.add_plugins(RunPlugin);
    app.start();

    if app.get_resource_ref::<GenerateConfigFlag>().is_some() {
        return;
    }

    let final_state = app.get_resource_ref::<OscState>().unwrap();
    let final_clock = app.get_resource_ref::<SimClock>().unwrap();
    println!(
        "\nfinal state @ step {}: x = {:+.6}  v = {:+.6}  time = {:.6}",
        final_clock.step, final_state.x, final_state.v, final_clock.time
    );
}
