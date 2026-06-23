//! Minimal "hello App" — the smallest complete `grass_app` program.
//!
//! Shows every moving part of the plugin lifecycle in one file:
//!   - a `ScheduleSet` enum defining the per-step phases,
//!   - a resource (`Counter`),
//!   - a system that mutates it,
//!   - a `Plugin` that wires the resource + systems into the `App`,
//!   - a done-condition system that ends the run after a fixed number of steps,
//!   - `App::new().add_plugins(..).start()` (the self-driving lifecycle).
//!
//! Run with: `cargo run -p grass_app --example hello_app`

use grass_app::prelude::*;
use grass_scheduler::prelude::*;

/// Per-step phases. Declaration order = schedule index.
#[derive(Debug, Clone, Copy)]
enum Step {
    Tick,
    CheckDone,
}

impl ScheduleSet for Step {
    fn to_index(&self) -> u32 {
        match self {
            Step::Tick => 0,
            Step::CheckDone => 1,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Step::Tick => "Tick",
            Step::CheckDone => "CheckDone",
        }
    }
}

/// The one piece of simulation state.
struct Counter {
    steps: u32,
}

/// Runs every step: advance the counter.
fn tick(mut counter: ResMut<Counter>) {
    counter.steps += 1;
}

/// Done-condition: stop the run loop once we've taken 5 steps.
fn check_done(counter: Res<Counter>, mut sm: ResMut<SchedulerManager>) {
    if counter.steps >= 5 {
        sm.state = SchedulerState::End;
    }
}

/// Bundles the resource and systems into one reusable unit.
struct CounterPlugin;

impl Plugin for CounterPlugin {
    fn build(&self, app: &mut App) {
        app.add_resource(Counter { steps: 0 })
            .add_update_system(tick, Step::Tick)
            .add_update_system(check_done, Step::CheckDone);
    }
}

fn main() {
    let mut app = App::new();
    app.add_plugins(CounterPlugin);
    // `start()` is the self-driving path: organize → setup → run-until-End → cleanup.
    app.start();

    let counter = app.get_resource_ref::<Counter>().expect("Counter resource");
    println!("hello_app: ran {} steps", counter.steps);
    assert_eq!(counter.steps, 5, "expected the done-condition to stop at 5 steps");
}
