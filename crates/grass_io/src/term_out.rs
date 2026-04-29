//! [`TermOutPlugin`] — periodic terminal output, LAMMPS-style.
//!
//! Each iter, user systems push named values into the [`TermOut`]
//! resource via [`TermOut::set`]. Every `interval` steps the plugin
//! prints one aligned line containing the columns named in
//! `[term_out] columns`. Header is printed once at the first matching step.
//!
//! ## Setup
//!
//! ```rust,ignore
//! app.add_plugins(TermOutPlugin);
//!
//! fn set_my_columns(state: Res<OscState>, mut term_out: ResMut<TermOut>) {
//!     term_out.set("x", state.x);
//!     term_out.set("v", state.v);
//! }
//! app.add_update_system(set_my_columns, TermOutSchedule::Compute);
//! ```
//!
//! ## Built-in columns
//!
//! `step` and `time` are auto-populated from [`SimClock`] each iter.
//! Anything else is user-pushed.
//!
//! ## TOML
//!
//! ```toml
//! [term_out]
//! every = 100
//! columns = ["step", "time", "x", "v"]
//! ```
//!
//! Empty / missing `columns` defaults to `["step", "time"]`. `every = 0`
//! disables term_out output entirely.

use std::collections::HashMap;

use grass_app::{App, Plugin};
use grass_scheduler::{prelude::*, Res, ResMut};
use serde::Deserialize;

use crate::{every_n_steps, Config, SimClock, SimClockPlugin};

/// Schedule namespace for [`TermOutSchedule`]. Between user phases
/// (default 0) and [`crate::RUN_NAMESPACE`] so term_out runs after
/// the work but before the run-end check.
pub const TERM_OUT_NAMESPACE: u32 = 100;

// ─── Resource ───────────────────────────────────────────────────────────────

/// Holds the configured column list, the current iter's values, and a
/// flag tracking whether the header has been printed.
pub struct TermOut {
    pub every: u64,
    pub columns: Vec<String>,
    /// Per-column width used by the printer. Same width applied to
    /// header and data; integers right-aligned to the same width.
    pub width: usize,
    pub values: HashMap<String, f64>,
    header_printed: bool,
}

impl TermOut {
    /// Push a named value. Plugins call this from systems registered in
    /// [`TermOutSchedule::Compute`] each iter; the value is read by the
    /// print system in [`TermOutSchedule::Print`] and cleared after.
    pub fn set(&mut self, name: &str, value: f64) {
        self.values.insert(name.to_string(), value);
    }
}

// ─── Config ─────────────────────────────────────────────────────────────────

fn default_every() -> u64 {
    100
}

fn default_width() -> usize {
    14
}

fn default_columns() -> Vec<String> {
    vec!["step".to_string(), "time".to_string()]
}

/// `[term_out]` section. All fields optional; defaults give every-100
/// printing of `step` and `time`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TermOutConfig {
    /// Print every N steps. 0 disables term_out output.
    #[serde(default = "default_every")]
    pub every: u64,
    /// Column names to print, in order. Auto-populated columns: `step`,
    /// `time`. Anything else must be pushed by a user system via
    /// [`TermOut::set`].
    #[serde(default = "default_columns")]
    pub columns: Vec<String>,
    /// Per-column field width. Default 14.
    #[serde(default = "default_width")]
    pub width: usize,
}

impl Default for TermOutConfig {
    fn default() -> Self {
        Self {
            every: default_every(),
            columns: default_columns(),
            width: default_width(),
        }
    }
}

// ─── Schedule ───────────────────────────────────────────────────────────────

/// Where TermOut's two systems run. Plugins register their column-setter
/// systems in [`TermOutSchedule::Compute`]; the plugin's print runs in
/// [`TermOutSchedule::Print`].
#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum TermOutSchedule {
    Compute,
    Print,
}

// ─── Plugin ─────────────────────────────────────────────────────────────────

/// Periodic terminal output. Depends on [`SimClockPlugin`] (registered
/// automatically). User registers column-setter systems in
/// [`TermOutSchedule::Compute`].
pub struct TermOutPlugin;

impl Plugin for TermOutPlugin {
    fn build(&self, app: &mut App) {
        let cfg = Config::load::<TermOutConfig>(app, "term_out");

        // Auto-add SimClock if not present — TermOut gates on it.
        if app.get_resource_ref::<SimClock>().is_none() {
            app.add_plugins(SimClockPlugin);
        }

        app.add_resource(TermOut {
            every: cfg.every,
            columns: cfg.columns.clone(),
            width: cfg.width,
            values: HashMap::new(),
            header_printed: false,
        });

        app.set_schedule_namespace::<TermOutSchedule>(TERM_OUT_NAMESPACE);
        app.add_update_system(populate_builtin_columns, TermOutSchedule::Compute);
        app.add_update_system(
            print_term_out.run_if(every_n_steps(cfg.every)),
            TermOutSchedule::Print,
        );
    }

    fn default_config(&self) -> Option<&str> {
        Some(
            r#"# [term_out] — periodic terminal log line.
[term_out]
# Print every N steps. 0 disables.
every = 100
# Column names. `step` and `time` are auto-populated from SimClock;
# anything else must be pushed by a user system via TermOut::set.
columns = ["step", "time"]
# Per-column field width.
width = 14
"#,
        )
    }
}

// ─── Systems ────────────────────────────────────────────────────────────────

fn populate_builtin_columns(clock: Res<SimClock>, mut term_out: ResMut<TermOut>) {
    term_out.set("step", clock.step as f64);
    term_out.set("time", clock.time);
}

fn print_term_out(mut term_out: ResMut<TermOut>) {
    let width = term_out.width;
    if !term_out.header_printed {
        let header = term_out
            .columns
            .iter()
            .map(|c| format!("{:>w$}", c, w = width))
            .collect::<Vec<_>>()
            .join(" ");
        println!("{}", header);
        term_out.header_printed = true;
    }
    let line = term_out
        .columns
        .iter()
        .map(|c| match term_out.values.get(c) {
            Some(v) => format_value(c, *v, width),
            None => format!("{:>w$}", "—", w = width),
        })
        .collect::<Vec<_>>()
        .join(" ");
    println!("{}", line);
}

fn format_value(col: &str, v: f64, width: usize) -> String {
    // Step is integral; print without decimals.
    if col == "step" {
        return format!("{:>w$}", v as u64, w = width);
    }
    // Otherwise: scientific for very large/small, fixed otherwise.
    let abs = v.abs();
    if abs != 0.0 && !(1e-3..1e6).contains(&abs) {
        format!("{:>w$.6e}", v, w = width)
    } else {
        format!("{:>w$.6}", v, w = width)
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_installs_term_out_with_defaults() {
        let mut app = App::new();
        app.add_plugins(TermOutPlugin);
        let t = app.get_resource_ref::<TermOut>().expect("TermOut present");
        assert_eq!(t.every, 100);
        assert_eq!(t.columns, vec!["step".to_string(), "time".to_string()]);
        assert_eq!(t.width, 14);
        // SimClock auto-installed too:
        assert!(app.get_resource_ref::<SimClock>().is_some());
    }

    #[test]
    fn plugin_seeds_from_config() {
        let mut app = App::new();
        app.add_resource(Config::from_str(
            r#"
            [term_out]
            every = 50
            columns = ["step", "x", "v"]
            width = 10
            "#,
        ));
        app.add_plugins(TermOutPlugin);
        let t = app.get_resource_ref::<TermOut>().expect("TermOut present");
        assert_eq!(t.every, 50);
        assert_eq!(
            t.columns,
            vec!["step".to_string(), "x".to_string(), "v".to_string()]
        );
        assert_eq!(t.width, 10);
    }

    #[test]
    fn set_and_format_value() {
        let mut t = TermOut {
            every: 100,
            columns: vec!["step".into(), "x".into()],
            width: 14,
            values: HashMap::new(),
            header_printed: false,
        };
        t.set("step", 42.0);
        t.set("x", 1.5);
        assert_eq!(t.values.get("step"), Some(&42.0));
        assert_eq!(t.values.get("x"), Some(&1.5));
    }

    #[test]
    fn format_value_picks_format() {
        // step is always integral
        assert_eq!(format_value("step", 1234.0, 8), format!("{:>8}", 1234u64));
        // small magnitudes -> fixed
        assert!(format_value("x", 1.5, 14).contains("1.500000"));
        // huge / tiny -> scientific
        assert!(format_value("x", 1e10, 14).contains("e10"));
        assert!(format_value("x", 1e-8, 14).contains("e-8"));
        // zero -> fixed
        assert!(format_value("x", 0.0, 14).contains("0.000000"));
    }
}
