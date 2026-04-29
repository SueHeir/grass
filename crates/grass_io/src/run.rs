//! Multi-stage [`RunPlugin`] — drives a simulation through one or more
//! `[run]` (single table) or `[[run]]` (array of tables) blocks read
//! from the input TOML.
//!
//! Each stage defines its own step count, optional `name`, optional
//! per-stage `dt`, an opt-in `skip` flag, an opt-in `save_at_end`
//! marker, and an arbitrary `#[serde(flatten)] overrides: toml::Table`
//! catch-all for codebase-specific knobs (a DEM `thermo` interval,
//! a CFD `cfl` number, etc.). The catch-all is also deep-merged with
//! the global config to produce [`StageOverrides`] — a per-stage
//! merged table that any plugin can deserialize sections from with
//! [`StageOverrides::section`].
//!
//! ```rust,ignore
//! app.add_plugins(InputPlugin);
//! app.add_plugins(RunPlugin);
//! ```
//!
//! `RunPlugin` auto-installs [`SimClockPlugin`] (so `SimClock.step`
//! is a synchronized global counter), and registers
//! [`advance_step`] + [`update_cycle`] in [`RunSchedule::Cycle`]
//! (namespace 1000, sorts after user phases that default to 0).

use std::any::TypeId;

use grass_app::{App, Plugin, ScheduleSetupSet, StageNames};
use grass_scheduler::{
    first_stage_only, prelude::*, Res, ResMut, SchedulerManager, SchedulerState,
};
use serde::{Deserialize, Serialize};

use crate::clock::SimClock;
use crate::config::{deep_merge, Config};
use crate::{advance_step, SimClockPlugin};

/// Schedule namespace `RunSchedule` sorts at — high enough that it
/// always runs AFTER user phase enums (which default to namespace 0).
pub const RUN_NAMESPACE: u32 = 1000;

#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum RunSchedule {
    /// `RunPlugin` registers `update_cycle` here.
    Cycle,
}

// ─── StageConfig / RunConfig ────────────────────────────────────────────────

fn default_steps() -> u32 {
    1000
}

/// Per-stage settings: step count, optional name/dt, plus an arbitrary
/// `overrides` catch-all for codebase-specific keys.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StageConfig {
    /// Optional human-readable stage name. Used by [`StageNames`]
    /// validation when a `StageEnum` is wired.
    #[serde(default)]
    pub name: Option<String>,
    /// Number of timesteps to run in this stage.
    #[serde(default = "default_steps")]
    pub steps: u32,
    /// Per-stage timestep size (0.0 = use whatever the user/integrator
    /// already set).
    #[serde(default)]
    pub dt: f64,
    /// Skip this stage entirely (advance immediately on entry).
    #[serde(default)]
    pub skip: bool,
    /// Codebase-defined "write final dump/restart at stage end" hint.
    /// Plugins decide what to do with it.
    #[serde(default)]
    pub save_at_end: bool,
    /// Catch-all for any TOML keys not matched by an explicit field.
    /// Codebase-specific knobs (e.g. a DEM `thermo` interval) live
    /// here and consumers read them via `stage.overrides.get("...")`
    /// or by deserializing an own struct from the merged
    /// [`StageOverrides`].
    #[serde(flatten)]
    pub overrides: toml::Table,
}

impl Default for StageConfig {
    fn default() -> Self {
        Self {
            name: None,
            steps: default_steps(),
            dt: 0.0,
            skip: false,
            save_at_end: false,
            overrides: toml::Table::new(),
        }
    }
}

/// All run stages. `[run]` (single table) yields one stage; `[[run]]`
/// (array of tables) yields N.
#[derive(Clone, Debug)]
pub struct RunConfig {
    pub stages: Vec<StageConfig>,
}

impl RunConfig {
    /// Per-stage lookup, clamped to the last stage if `index` overshoots
    /// (useful when a system fires once after the final stage's last
    /// iteration).
    pub fn current_stage(&self, index: usize) -> &StageConfig {
        &self.stages[index.min(self.stages.len() - 1)]
    }
    pub fn num_stages(&self) -> usize {
        self.stages.len()
    }

    /// Construct a [`RunConfig`] from a [`Config`]. Handles both
    /// `[run]` (single table) and `[[run]]` (array of tables) syntax.
    pub fn from_config(config: &Config) -> Self {
        match config.table.get("run") {
            Some(toml::Value::Array(arr)) => {
                let stages: Vec<StageConfig> = arr
                    .iter()
                    .enumerate()
                    .map(|(idx, v)| match v.clone().try_into::<StageConfig>() {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!();
                            eprintln!(
                                "ERROR: Failed to parse [[run]] stage {} in config file.",
                                idx
                            );
                            eprintln!("  {}", e);
                            eprintln!();
                            eprintln!(
                                "  Hint: check that all field names are spelled correctly \
                                 and values have the right type."
                            );
                            std::process::exit(1);
                        }
                    })
                    .collect();
                RunConfig { stages }
            }
            Some(toml::Value::Table(_)) => {
                let stage: StageConfig = config.section("run");
                RunConfig {
                    stages: vec![stage],
                }
            }
            _ => RunConfig::default(),
        }
    }
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            stages: vec![StageConfig::default()],
        }
    }
}

/// Mutable state tracking cycle counts per stage and total. Maintained
/// by [`update_cycle`].
pub struct RunState {
    pub total_cycle: usize,
    pub cycle_count: Vec<u32>,
    pub cycle_remaining: Vec<u32>,
}

impl Default for RunState {
    fn default() -> Self {
        Self::new()
    }
}

impl RunState {
    pub fn new() -> Self {
        Self {
            total_cycle: 0,
            cycle_count: Vec::new(),
            cycle_remaining: Vec::new(),
        }
    }
}

/// Merged config table for the current stage: the global TOML deep-
/// merged with the current stage's `overrides` catch-all. Plugins read
/// stage-aware config via [`StageOverrides::section`].
pub struct StageOverrides {
    pub table: toml::Table,
}

impl StageOverrides {
    pub fn section<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> T {
        self.table
            .get(key)
            .and_then(|v| v.clone().try_into().ok())
            .unwrap_or_default()
    }
}

/// Pluggable list of `(section_key, description)` pairs. Sections in
/// this list are read only during the first stage; if a later
/// `[[run]]` block overrides one of them, [`set_stage_name`] emits a
/// warning. Default: empty (no warnings).
///
/// Codebases populate this in their bootstrap, e.g.:
///
/// ```rust,ignore
/// app.add_resource(FirstStageOnlyConfigs(vec![
///     ("lattice".into(), "lattice insertion".into()),
///     ("comm".into(),    "communicator setup".into()),
/// ]));
/// ```
#[derive(Default)]
pub struct FirstStageOnlyConfigs(pub Vec<(String, String)>);

// ─── RunPlugin ──────────────────────────────────────────────────────────────

/// Reads `[run]` / `[[run]]` from `Config`, installs [`RunConfig`],
/// [`RunState`], [`StageOverrides`], and the per-iter cycle update.
///
/// Auto-installs:
///   - [`SimClockPlugin`] if not already present.
///   - [`advance_step`] in [`RunSchedule::Cycle`] (skipped if user
///     pre-registered it).
///   - [`update_cycle`] in [`RunSchedule::Cycle`], labelled
///     `"update_cycle"` so other systems can `.before("update_cycle")`.
///
/// If [`StageNames`] is registered (i.e. a `StageAdvancePlugin` is in
/// use), [`validate_stages`] is also wired in `ScheduleSetupSet::PreSetup`
/// guarded by [`first_stage_only`].
pub struct RunPlugin;

impl Plugin for RunPlugin {
    fn build(&self, app: &mut App) {
        let run_config = if let Some(cell) = app.get_mut_resource(TypeId::of::<Config>()) {
            let raw = cell.borrow();
            let cfg = raw
                .downcast_ref::<Config>()
                .expect("Config resource has wrong type — this is a bug in grass_io");
            RunConfig::from_config(cfg)
        } else {
            RunConfig::default()
        };

        app.add_resource(run_config);
        app.add_resource(StageOverrides {
            table: toml::Table::new(),
        });
        app.add_resource(RunState::new());

        if app.get_resource_ref::<SimClock>().is_none() {
            app.add_plugins(SimClockPlugin);
        }

        app.set_schedule_namespace::<RunSchedule>(RUN_NAMESPACE);

        app.add_setup_system(set_stage_name, ScheduleSetupSet::PreSetup);
        app.add_setup_system(run_read_input, ScheduleSetupSet::Setup);

        if !app.has_update_system(advance_step) {
            app.add_update_system(advance_step, RunSchedule::Cycle);
        }
        app.add_update_system(update_cycle.label("update_cycle"), RunSchedule::Cycle);

        if app.get_resource_ref::<StageNames>().is_some() {
            app.add_setup_system(
                validate_stages.run_if(first_stage_only()),
                ScheduleSetupSet::PreSetup,
            );
        }
    }

    fn default_config(&self) -> Option<&str> {
        Some(
            r#"# Single-stage run:
[run]
steps = 1000
# name = "my_stage"     # optional stage name
# dt = 0.0              # per-stage dt (0 = leave whatever's set)
# skip = false          # advance past this stage immediately
# save_at_end = false   # codebase hint: write dump/restart on stage end
# Any other keys land in `overrides` and are visible via
# StageOverrides::section, e.g. an MD/DEM `thermo = 100`.

# Multi-stage run (use [[run]] instead of [run]):
# [[run]]
# name  = "settling"
# steps = 1000
#
# [[run]]
# name  = "production"
# steps = 5000
"#,
        )
    }
}

// ─── Systems ────────────────────────────────────────────────────────────────

/// Setup system: copies stage name into [`SchedulerManager`], applies
/// stage `overrides` (deep-merged onto the global config) into
/// [`StageOverrides`], and emits warnings if a later stage overrides
/// any [`FirstStageOnlyConfigs`] section.
pub fn set_stage_name(
    run_config: Res<RunConfig>,
    config: Res<Config>,
    first_stage_only: Option<Res<FirstStageOnlyConfigs>>,
    mut scheduler_manager: ResMut<SchedulerManager>,
    mut stage_overrides: ResMut<StageOverrides>,
) {
    let index = scheduler_manager.index;
    if index >= run_config.num_stages() {
        return;
    }
    let stage = run_config.current_stage(index);
    scheduler_manager.stage_name = stage.name.clone();

    if index > 0 {
        if let Some(list) = first_stage_only.as_deref() {
            for (section, description) in &list.0 {
                if stage.overrides.contains_key(section) {
                    let stage_label = stage.name.as_deref().unwrap_or("unnamed");
                    eprintln!(
                        "WARNING: Stage {} [{}] overrides [{}], but {} only runs in the first \
                         stage. This override will be ignored.",
                        index, stage_label, section, description
                    );
                }
            }
        }
    }

    let mut merged = config.table.clone();
    merged.remove("run");
    deep_merge(&mut merged, &stage.overrides);
    stage_overrides.table = merged;
}

/// Setup system: initialize per-stage cycle counters and print a
/// run-start banner.
pub fn run_read_input(
    config: Res<RunConfig>,
    scheduler_manager: Res<SchedulerManager>,
    mut run_state: ResMut<RunState>,
) {
    let index = scheduler_manager.index;
    if index >= config.num_stages() {
        return;
    }

    let stage = config.current_stage(index);
    let stage_label = stage.name.as_deref().unwrap_or("(unnamed)");

    if stage.skip {
        println!("Skipping stage {} [{}]", index, stage_label);
        run_state.cycle_count.push(0);
        run_state.cycle_remaining.push(0);
        return;
    }

    if config.num_stages() > 1 {
        println!(
            "Run stage {} [{}]: {} steps",
            index, stage_label, stage.steps
        );
    } else {
        println!("Run: {} steps", stage.steps);
    }
    run_state.cycle_count.push(0);
    run_state.cycle_remaining.push(stage.steps);
}

/// Update system: increment cycle counters, advance to the next stage
/// when steps are exhausted (or [`SchedulerManager::advance_requested`]
/// is set), and end the App after the final stage completes.
pub fn update_cycle(
    mut run_state: ResMut<RunState>,
    mut scheduler_manager: ResMut<SchedulerManager>,
    run_config: Res<RunConfig>,
) {
    let index = scheduler_manager.index;
    let remaining = run_state.cycle_remaining[index];

    // Skipped stage (remaining == 0): advance immediately without
    // running physics. Clear advance_requested in case another system
    // set it during the ghost iteration that runs first.
    if remaining == 0 {
        scheduler_manager.advance_requested = false;
        scheduler_manager.index += 1;
        scheduler_manager.state = SchedulerState::Setup;
        if scheduler_manager.index >= run_config.num_stages() {
            scheduler_manager.state = SchedulerState::End;
        }
        return;
    }

    run_state.cycle_count[index] += 1;
    run_state.total_cycle += 1;

    let steps_done = run_state.cycle_count[index] == run_state.cycle_remaining[index];
    let advance = scheduler_manager.advance_requested;

    if steps_done || advance {
        scheduler_manager.advance_requested = false;
        scheduler_manager.index += 1;
        scheduler_manager.state = SchedulerState::Setup;
        if scheduler_manager.index >= run_config.num_stages() {
            scheduler_manager.state = SchedulerState::End;
        }
    }
}

/// Setup system (registered only when [`StageNames`] is present):
/// validates that the count and order of TOML `[[run]]` stages match
/// the `StageEnum` variants. Panics with an actionable message on
/// mismatch.
pub fn validate_stages(
    run_config: Res<RunConfig>,
    stage_names: Res<StageNames>,
    scheduler_manager: Res<SchedulerManager>,
) {
    if scheduler_manager.index != 0 {
        return;
    }

    let expected = stage_names.0;
    let actual: Vec<Option<&str>> = run_config
        .stages
        .iter()
        .map(|s| s.name.as_deref())
        .collect();

    if run_config.stages.len() != expected.len() {
        panic!(
            "Stage count mismatch: {} [[run]] stages in TOML, but StageEnum has {} variants.\n\
             Expected stage names: {:?}\n\
             TOML stage names: {:?}",
            run_config.stages.len(),
            expected.len(),
            expected,
            actual,
        );
    }

    for (i, (expected_name, stage)) in expected.iter().zip(run_config.stages.iter()).enumerate() {
        match &stage.name {
            Some(name) if name != expected_name => {
                panic!(
                    "Stage {} name mismatch: TOML has \"{}\", but StageEnum expects \"{}\"",
                    i, name, expected_name,
                );
            }
            None => {
                panic!(
                    "Stage {} is missing a name in TOML. Expected name: \"{}\"\n\
                     All [[run]] stages must have a `name` when using StageAdvancePlugin.",
                    i, expected_name,
                );
            }
            _ => {}
        }
    }
}
