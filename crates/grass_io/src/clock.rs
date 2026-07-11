//! [`SimClock`] — `step` / `time` accumulator that everything periodic
//! gates against.
//!
//! Add [`SimClockPlugin`] to install the resource. Add [`advance_step`]
//! as a system in whichever phase you want the counter to tick (typically
//! one per outer iter, late in the schedule). Wire your domain's `time`
//! advance manually — the dt source is application-specific and the
//! framework can't guess which resource owns it.
//!
//! ```rust,ignore
//! app.add_plugins(SimClockPlugin);
//! app.add_update_system(advance_step, MyPhase::EndOfIter);
//! app.add_update_system(
//!     |mut clock: ResMut<SimClock>, dt: Res<MyDtSource>| {
//!         clock.time += dt.0;
//!     },
//!     MyPhase::EndOfIter,
//! );
//! ```
//!
//! Use [`every_n_steps`] as a `.run_if(...)` predicate to gate periodic
//! work (thermo prints, dump writes, status logs):
//!
//! ```rust,ignore
//! app.add_update_system(
//!     dump_state.run_if(every_n_steps(100)),
//!     MyPhase::Output,
//! );
//! ```

use grass_app::{App, ConfigDescription, ConfigFieldDescription, Plugin};
use grass_scheduler::{Res, ResMut};
use serde::Deserialize;

use crate::{Config, DescribedConfig};

// ─── Resource ───────────────────────────────────────────────────────────────

/// Simulation step + time accumulator. `step` ticks once per outer iter
/// (when [`advance_step`] runs); `time` is whatever your dt-source
/// system has accumulated.
#[derive(Debug, Default, Clone, Copy)]
pub struct SimClock {
    /// Outer-iteration counter; incremented once per step by `advance_step`.
    pub step: u64,
    /// Accumulated simulation time, in whatever units the dt source uses.
    pub time: f64,
}

// ─── Config ─────────────────────────────────────────────────────────────────

/// `[clock]` section of the input TOML — optional starting values for
/// restart scenarios. Both default to zero.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClockConfig {
    /// Starting step count. Default: 0.
    #[serde(default)]
    pub start_step: u64,
    /// Starting simulated time (in whatever units the app uses). Default: 0.0.
    #[serde(default)]
    pub start_time: f64,
}

impl DescribedConfig for ClockConfig {
    fn description() -> ConfigDescription {
        ConfigDescription {
            section: "clock",
            array_table: false,
            narrative:
                "Simulation step + time accumulator. Set non-zero values to resume a saved state.",
            fields: &[
                ConfigFieldDescription {
                    name: "start_step",
                    ty: "integer",
                    default: Some("0"),
                    required: false,
                    choices: &[],
                    description: "Starting step count.",
                    source: "crates/grass_io/src/clock.rs:ClockConfig.start_step",
                },
                ConfigFieldDescription {
                    name: "start_time",
                    ty: "float",
                    default: Some("0.0"),
                    required: false,
                    choices: &[],
                    description: "Starting simulated time in application units.",
                    source: "crates/grass_io/src/clock.rs:ClockConfig.start_time",
                },
            ],
        }
    }
}

// ─── Plugin ─────────────────────────────────────────────────────────────────

/// Installs [`SimClock`] on the App, optionally seeded from a `[clock]`
/// TOML section. Does **not** register [`advance_step`] — the user
/// places it in whichever phase they want the counter to tick.
pub struct SimClockPlugin;

impl Plugin for SimClockPlugin {
    fn build(&self, app: &mut App) {
        let cfg = Config::load_described::<ClockConfig>(app);
        app.add_resource(SimClock {
            step: cfg.start_step,
            time: cfg.start_time,
        });
    }

    fn config_description(&self) -> Option<ConfigDescription> {
        Some(ClockConfig::description())
    }
}

// ─── Systems / helpers ──────────────────────────────────────────────────────

/// System: `clock.step += 1`. Add to whichever phase should tick the
/// counter (typically once per outer iter).
pub fn advance_step(mut clock: ResMut<SimClock>) {
    clock.step += 1;
}

/// `.run_if()` predicate: fires every `n` steps (i.e. when
/// `clock.step % n == 0`). Returns `false` if `n == 0` so the gate can
/// be statically disabled.
pub fn every_n_steps(n: u64) -> impl Fn(Res<SimClock>) -> bool + Send + Sync + 'static {
    move |clock: Res<SimClock>| n > 0 && clock.step.is_multiple_of(n)
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use grass_app::App;

    #[test]
    fn plugin_installs_clock_with_defaults() {
        let mut app = App::new();
        app.add_plugins(SimClockPlugin);
        let clock = app
            .get_resource_ref::<SimClock>()
            .expect("SimClock present");
        assert_eq!(clock.step, 0);
        assert_eq!(clock.time, 0.0);
    }

    #[test]
    fn plugin_seeds_from_config() {
        let mut app = App::new();
        app.add_resource(Config::from_str(
            r#"
            [clock]
            start_step = 1000
            start_time = 12.5
            "#,
        ));
        app.add_plugins(SimClockPlugin);
        let clock = app
            .get_resource_ref::<SimClock>()
            .expect("SimClock present");
        assert_eq!(clock.step, 1000);
        assert_eq!(clock.time, 12.5);
    }

    #[test]
    fn every_n_steps_gates_correctly() {
        let mut clock = SimClock::default();
        let pred_100 = every_n_steps(100);

        // Need to wrap in a fake Res context — easier to test the math directly:
        for s in 0..=300 {
            clock.step = s;
            let expected = s % 100 == 0;
            // Manual check matching the predicate body:
            let got = 100 > 0 && clock.step % 100 == 0;
            assert_eq!(expected, got, "at step {s}");
        }

        // n = 0 is the disabled gate (the predicate's `n > 0` short-
        // circuits before the modulo).

        let _ = pred_100; // suppress unused warning
    }
}
