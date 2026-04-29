//! # `single_oscillator` — one damped harmonic oscillator, no coupling.
//!
//! The simplest possible introduction to the GRASS stack. Three plugins
//! and `start()`. `OscillatorPlugin` reads `[oscillator]` from the TOML;
//! `RunPlugin` reads `[run] steps`, advances the clock each iter, and
//! ends the App when the count is reached.
//!
//! ```sh
//! cargo run --example single_oscillator -- examples/coupling/single_oscillator/main.toml
//! cargo run --example single_oscillator -- --generate-config
//! ```

use grass_app::prelude::*;
use grass_io::{InputPlugin, RunPlugin};
use oscillator_demo::{OscState, OscillatorPlugin};

fn main() {
    let mut app = App::new();
    app.add_plugins(InputPlugin);
    app.add_plugins(OscillatorPlugin);
    app.add_plugins(RunPlugin);
    app.start();

    if app.get_resource_ref::<GenerateConfigFlag>().is_some() {
        return;
    }
    let state = app.get_resource_ref::<OscState>().unwrap();
    println!(
        "single_oscillator: x = {:>+12.9}   v = {:>+12.9}",
        state.x, state.v
    );
}
