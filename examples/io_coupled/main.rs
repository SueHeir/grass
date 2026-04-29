//! # `io_coupled` — explicit coupling driven by ONE main TOML file.
//!
//! Two damped harmonic oscillators with an interface spring, the same
//! physics as `examples/coupling/explicit`, but every parameter (per-
//! oscillator material + initial state, parent-level clock / term_out /
//! dump cadence, output dir, total steps) comes from `main.toml`.
//!
//! The trick is [`grass_io::Config::for_subapp`]: parent reads
//! `main.toml`, and for each sub-App pulls a slice — `[a.*]` becomes
//! sub-App `a`'s Config; `[b.*]` becomes sub-App `b`'s. Plugin code on
//! the sub-App then reads `[oscillator]` from its own local Config,
//! never knowing the namespace prefix existed.
//!
//! Cross-namespace observability stays on the parent: `TermOut` columns
//! and `Dump` payloads use `MultiRes<T, NS>` to read each sub-App's
//! state directly.
//!
//! ## Running
//!
//! ```sh
//! cargo run --example io_coupled -- examples/io_coupled/main.toml
//! cargo run --example io_coupled -- --generate-config
//! ```

use grass_app::prelude::*;
use grass_io::{
    advance_step, every_n_steps, DumpBuffer, DumpPlugin, DumpSchedule, InputPlugin, MultiIoExt,
    RunPlugin, SimClock, TermOut, TermOutPlugin, TermOutSchedule,
};
use grass_multi::{tick_subapp, MultiRes, Namespace};
use grass_scheduler::prelude::*;
use oscillator_demo::{exchange_positions, OscState, OscillatorPlugin, StepSize, A, B};
use serde::Serialize;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum OuterStep {
    TickA,
    TickB,
    Exchange,
}

fn set_term_columns(a: MultiRes<OscState, A>, b: MultiRes<OscState, B>, mut term: ResMut<TermOut>) {
    term.set("x_a", a.x);
    term.set("v_a", a.v);
    term.set("x_b", b.x);
    term.set("v_b", b.v);
}

#[derive(Serialize)]
struct DumpFrame {
    step: u64,
    time: f64,
    a: OscStateOut,
    b: OscStateOut,
}
#[derive(Serialize)]
struct OscStateOut {
    x: f64,
    v: f64,
}

fn build_dump_payload(
    clock: Res<SimClock>,
    a: MultiRes<OscState, A>,
    b: MultiRes<OscState, B>,
    mut buffer: ResMut<DumpBuffer>,
) {
    let frame = DumpFrame {
        step: clock.step,
        time: clock.time,
        a: OscStateOut { x: a.x, v: a.v },
        b: OscStateOut { x: b.x, v: b.v },
    };
    buffer.payload = serde_json::to_vec_pretty(&frame).expect("serialize DumpFrame to JSON");
}

fn advance_time(mut clock: ResMut<SimClock>, dt_a: MultiRes<StepSize, A>) {
    // Both oscillators run at the same dt in this scenario; pull from A.
    clock.time += dt_a.dt;
}

fn main() {
    let mut parent = App::new();
    parent.add_plugins(InputPlugin);

    parent.add_subapp_with_config(A::NAME, |app| {
        app.add_plugins(OscillatorPlugin);
    });
    parent.add_subapp_with_config(B::NAME, |app| {
        app.add_plugins(OscillatorPlugin);
    });

    parent.add_plugins(TermOutPlugin);
    parent.add_plugins(DumpPlugin::default());

    parent.add_update_system(tick_subapp(A::NAME, 1), OuterStep::TickA);
    parent.add_update_system(tick_subapp(B::NAME, 1), OuterStep::TickB);
    parent.add_update_system(exchange_positions, OuterStep::Exchange);
    parent.add_update_system(advance_step, OuterStep::Exchange);
    parent.add_update_system(advance_time, OuterStep::Exchange);
    parent.add_update_system(set_term_columns, TermOutSchedule::Compute);
    parent.add_update_system(
        build_dump_payload.run_if(every_n_steps(50)),
        DumpSchedule::Build,
    );

    parent.add_plugins(RunPlugin);
    parent.start();

    if parent.get_resource_ref::<GenerateConfigFlag>().is_some() {
        return;
    }

    let clock = parent.get_resource_ref::<SimClock>().unwrap();
    println!(
        "\nfinal state @ step {} (t = {:.6}):",
        clock.step, clock.time
    );
}
