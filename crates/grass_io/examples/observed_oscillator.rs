//! End-to-end `grass_io` wiring: a trivial harmonic oscillator observed
//! through the full I/O plugin stack, driven by a multi-stage `[[run]]`.
//!
//! Plugins wired here, in the order their systems execute each step:
//!   - the user solver phase `Sim::Integrate` (namespace 0),
//!   - [`TermOutPlugin`] (namespace 100) — prints `step time x v`,
//!   - [`DumpPlugin`] (namespace 200) — writes a frame every N steps,
//!   - [`RunPlugin`] (namespace 1000) — walks the `[[run]]` stages and ends.
//!
//! [`InputPlugin`] is what reads the TOML in a real binary
//! (`myapp config.toml`), and `--generate-config` makes it install a
//! `GenerateConfigFlag` so `start()` prints every plugin's `default_config()`
//! and exits. Here we seed the same TOML programmatically with
//! `Config::from_str` so the example is self-contained — adding `InputPlugin`
//! afterwards is a no-op because a `Config` is already present.
//!
//! Run with: `cargo run -p grass_io --example observed_oscillator`
//!
//! The config below has two `[[run]]` stages (`settle`, then `production`).
//! Each stage's extra keys are deep-merged over the global config into the
//! `StageOverrides` resource by `set_stage_name` at the start of each stage;
//! plugins that re-read config per stage consume them via
//! `StageOverrides::section("...")`. (Note: `TermOutPlugin` / `DumpPlugin`
//! read their `[term_out]` / `[dump]` sections **once at build time**, so
//! their intervals are global — `StageOverrides` is for plugins that opt into
//! per-stage re-reading, e.g. a solver tweaking `dt` between stages.)

use grass_app::prelude::*;
use grass_io::{Config, DumpPlugin, DumpSchedule, InputPlugin, RunPlugin, TermOut, TermOutSchedule};
use grass_scheduler::prelude::*;
use grass_scheduler::{Res, ResMut};

const CONFIG: &str = r#"
[clock]
start_step = 0

[term_out]
every = 20
columns = ["step", "time", "x", "v"]

[dump]
interval = 25                      # write a frame every 25 steps
path_template = "frames/osc_{step:05}.json"

# Multi-stage run: a short settle stage, then a longer production stage.
# Per-stage `[run.<section>]` tables deep-merge over the global config into
# the StageOverrides resource (see the module doc for the caveat on which
# plugins actually re-read it per stage).
[[run]]
name  = "settle"
steps = 40

[[run]]
name  = "production"
steps = 100
[run.solver]                       # surfaced via StageOverrides::section("solver")
relax = 0.5
"#;

/// User solver phase (namespace 0 — runs before any grass_io plugin).
#[derive(Debug, Clone, Copy)]
enum Sim {
    Integrate,
}

impl ScheduleSet for Sim {
    fn to_index(&self) -> u32 {
        0
    }
    fn name(&self) -> &'static str {
        "Integrate"
    }
}

/// 1-D unit-mass spring (k = 1), symplectic-Euler stepped.
struct OscState {
    x: f64,
    v: f64,
    dt: f64,
}

fn integrate(mut s: ResMut<OscState>) {
    let dt = s.dt;
    let a = -s.x; // k = 1
    s.v += a * dt;
    let dv = s.v;
    s.x += dv * dt;
}

/// Push the solver state into TermOut as named columns (TermOutSchedule::Compute).
fn report_columns(s: Res<OscState>, mut term_out: ResMut<TermOut>) {
    term_out.set("x", s.x);
    term_out.set("v", s.v);
}

fn main() {
    let mut app = App::new();

    // Seed the config programmatically; InputPlugin then becomes a no-op.
    app.add_resource(Config::from_str(CONFIG));
    app.add_plugins(InputPlugin);

    app.add_resource(OscState {
        x: 1.0,
        v: 0.0,
        dt: 0.05,
    });
    app.add_update_system(integrate, Sim::Integrate);
    app.add_update_system(report_columns, TermOutSchedule::Compute);

    // Observability + run-driver stack. SimClock is auto-installed by these.
    app.add_plugins(grass_io::TermOutPlugin);
    app.add_plugins(DumpPlugin::default()); // RawFrameWriter

    // A dump-builder system: serialize current state to JSON bytes each frame.
    app.add_update_system(
        |s: Res<OscState>, mut buf: ResMut<grass_io::DumpBuffer>| {
            buf.payload = format!("{{\"x\":{},\"v\":{}}}", s.x, s.v).into_bytes();
        },
        DumpSchedule::Build,
    );

    app.add_plugins(RunPlugin);

    // Self-driving lifecycle: organize -> setup -> run both stages -> cleanup.
    app.start();

    let s = app.get_resource_ref::<OscState>().expect("OscState");
    println!("observed_oscillator: finished at x = {:.4}, v = {:.4}", s.x, s.v);
}
