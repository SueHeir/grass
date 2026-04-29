//! # `explicit` — explicit / CSS coupling
//!
//! Two damped oscillators connected by an interface spring, each in its
//! own sub-App, coupled by a parent-side `exchange_positions` system.
//! Standard "conventional sequential staggered" — first-order in
//! coupling time, simplest possible coupling pattern.
//!
//! All parameters (per-oscillator material + initial state, total
//! steps) come from `main.toml`. Parent reads main.toml; each sub-App
//! receives its `[<name>.oscillator]` slice via [`Config::for_subapp`].
//!
//! Run:
//!
//! ```sh
//! cargo run --example explicit -- examples/coupling/explicit/main.toml
//! ```

use grass_app::prelude::*;
use grass_io::{InputPlugin, MultiIoExt, RunPlugin};
use grass_multi::{tick_subapp, Namespace};
use grass_scheduler::prelude::*;
use oscillator_demo::{exchange_positions, extract_final_state, OscillatorPlugin, A, B};

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum OuterStep {
    TickA,
    TickB,
    Exchange,
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

    parent.add_plugins(RunPlugin);

    parent.add_update_system(tick_subapp(A::NAME, 1), OuterStep::TickA);
    parent.add_update_system(tick_subapp(B::NAME, 1), OuterStep::TickB);
    parent.add_update_system(exchange_positions, OuterStep::Exchange);

    parent.start();

    if parent.get_resource_ref::<GenerateConfigFlag>().is_some() {
        return;
    }
    extract_final_state(&parent).print("explicit");
}
