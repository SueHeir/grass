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
use std::fmt;

use grass_app::{
    App, ConfigDescription, ConfigFieldDescription, Plugin, ScheduleSetupSet, StageNames,
};
use grass_scheduler::{
    first_stage_only, prelude::*, Res, ResMut, SchedulerManager, SchedulerState, SystemKey,
    SystemLabel,
};
use serde::{Deserialize, Serialize};

use crate::clock::SimClock;
use crate::config::{deep_merge, Config, DescribedConfig};
use crate::{advance_step, SimClockPlugin};

/// Schedule namespace `RunSchedule` sorts at — high enough that it
/// always runs AFTER user phase enums (which default to namespace 0).
pub const RUN_NAMESPACE: u32 = 1000;

/// Type-level label for the run driver's cycle-counter update system.
pub struct UpdateCycleSystem;

impl SystemLabel for UpdateCycleSystem {
    const NAME: &'static str = "update_cycle";
}

/// Stable scheduler key for [`update_cycle`].
///
/// Systems that must run before the run driver advances its cycle counters can
/// order against this key without repeating its string label.
pub const UPDATE_CYCLE: SystemKey<UpdateCycleSystem> = SystemKey::new();

/// Schedule sets owned by the run driver.
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

impl DescribedConfig for StageConfig {
    fn description() -> ConfigDescription {
        ConfigDescription { section: "run", array_table: true, narrative: "One run stage. Repeat [[run]] for multi-stage workflows; unrecognised keys are preserved as stage overrides.", fields: &[
            ConfigFieldDescription { name: "name", ty: "string", default: None, required: false, choices: &[], description: "Human-readable stage name when stage validation is enabled.", source: "crates/grass_io/src/run.rs:StageConfig.name" },
            ConfigFieldDescription { name: "steps", ty: "integer", default: Some("1000"), required: false, choices: &[], description: "Number of timesteps in this stage.", source: "crates/grass_io/src/run.rs:StageConfig.steps" },
            ConfigFieldDescription { name: "dt", ty: "float", default: Some("0.0"), required: false, choices: &[], description: "Stage timestep; 0.0 leaves the integrator setting unchanged.", source: "crates/grass_io/src/run.rs:StageConfig.dt" },
            ConfigFieldDescription { name: "skip", ty: "boolean", default: Some("false"), required: false, choices: &[], description: "Advance past this stage immediately.", source: "crates/grass_io/src/run.rs:StageConfig.skip" },
            ConfigFieldDescription { name: "save_at_end", ty: "boolean", default: Some("false"), required: false, choices: &[], description: "Hint that plugins may use to write final output at the stage end.", source: "crates/grass_io/src/run.rs:StageConfig.save_at_end" },
        ] }
    }
}

/// All run stages. `[run]` (single table) yields one stage; `[[run]]`
/// (array of tables) yields N.
#[derive(Clone, Debug)]
pub struct RunConfig {
    /// The ordered stages parsed from `[run]` / `[[run]]`.
    pub stages: Vec<StageConfig>,
}

impl RunConfig {
    /// Per-stage lookup, clamped to the last stage if `index` overshoots
    /// (useful when a system fires once after the final stage's last
    /// iteration).
    pub fn current_stage(&self, index: usize) -> &StageConfig {
        &self.stages[index.min(self.stages.len() - 1)]
    }
    /// Number of configured stages.
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
    /// Total cycles executed across all stages so far.
    pub total_cycle: usize,
    /// Cycles executed in each stage, indexed by stage.
    pub cycle_count: Vec<u32>,
    /// Cycles still remaining in each stage, indexed by stage.
    pub cycle_remaining: Vec<u32>,
}

impl Default for RunState {
    fn default() -> Self {
        Self::new()
    }
}

impl RunState {
    /// Creates a `RunState` with zeroed counters and empty per-stage vectors.
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
    /// The stage-merged TOML table (global config deep-merged with the stage `overrides`).
    pub table: toml::Table,
}

/// Error returned when a present [`StageOverrides`] section cannot be parsed.
#[derive(Debug)]
pub struct StageOverrideSectionError {
    key: String,
    source: toml::de::Error,
}

impl StageOverrideSectionError {
    /// TOML section key that failed to deserialize.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Underlying TOML deserialization error.
    pub fn source(&self) -> &toml::de::Error {
        &self.source
    }
}

impl fmt::Display for StageOverrideSectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to parse [{}] section in StageOverrides: {}",
            self.key, self.source
        )
    }
}

impl std::error::Error for StageOverrideSectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl StageOverrides {
    /// Fallibly deserializes the `[key]` section from the merged table.
    ///
    /// Returns `Ok(None)` when the section is absent, and `Err` when the
    /// section is present but cannot be parsed as `T`.
    pub fn try_section<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, StageOverrideSectionError> {
        match self.table.get(key) {
            None => Ok(None),
            Some(value) => value.clone().try_into::<T>().map(Some).map_err(|source| {
                StageOverrideSectionError {
                    key: key.to_string(),
                    source,
                }
            }),
        }
    }

    /// Deserializes the `[key]` section from the merged table, or `T::default()`
    /// if it is absent. Prints an actionable error and exits if a present
    /// section cannot be parsed.
    pub fn section<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> T {
        match self.try_section(key) {
            Ok(Some(value)) => value,
            Ok(None) => T::default(),
            Err(e) => {
                eprintln!();
                eprintln!(
                    "ERROR: Failed to parse [{}] section in StageOverrides.",
                    e.key()
                );
                eprintln!("  {}", e.source());
                eprintln!();
                eprintln!(
                    "  Hint: this section is the global config deep-merged with the current \
                     [[run]] stage overrides. Check per-stage override field names and value types."
                );
                eprintln!(
                    "  Run with --generate-config to see the base configuration, then compare \
                     the override shape for this stage."
                );
                std::process::exit(1);
            }
        }
    }

    /// Deserializes the `[key]` section from the merged table, returning
    /// `T::default()` when the section is absent or malformed.
    ///
    /// This preserves the old fully-silent convenience behavior for callers
    /// that deliberately want best-effort reads. Prefer [`Self::try_section`]
    /// or [`Self::section`] for user-facing config.
    pub fn section_or_default<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> T {
        self.try_section(key).ok().flatten().unwrap_or_default()
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

/// Structured error returned when TOML `[[run]]` stages do not match the
/// [`StageNames`] registered by `StageAdvancePlugin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageValidationError {
    /// The number of TOML stages differs from the number of `StageEnum` variants.
    StageCountMismatch {
        /// Number of `[run]` / `[[run]]` stages parsed from TOML.
        toml_count: usize,
        /// Number of variants in the `StageEnum`.
        enum_count: usize,
        /// Stage names declared by `StageEnum`, in order.
        expected_names: Vec<String>,
        /// Stage names found in TOML, in order. `None` means missing `name`.
        toml_names: Vec<Option<String>>,
    },
    /// A TOML stage has a `name`, but it does not match the enum at that index.
    StageNameMismatch {
        /// Zero-based stage index.
        index: usize,
        /// Name declared by `StageEnum` for this index.
        expected: String,
        /// Name found in TOML for this index.
        actual: String,
    },
    /// A TOML stage is missing `name` while `StageAdvancePlugin` is active.
    MissingStageName {
        /// Zero-based stage index.
        index: usize,
        /// Name declared by `StageEnum` for this index.
        expected: String,
    },
}

impl fmt::Display for StageValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StageValidationError::StageCountMismatch {
                toml_count,
                enum_count,
                expected_names,
                toml_names,
            } => write!(
                f,
                "stage count mismatch: {} [[run]] stages in TOML, but StageEnum has {} variants. \
                 Expected stage names: {:?}. TOML stage names: {:?}.",
                toml_count, enum_count, expected_names, toml_names
            ),
            StageValidationError::StageNameMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "stage {} name mismatch: TOML has \"{}\", but StageEnum expects \"{}\".",
                index, actual, expected
            ),
            StageValidationError::MissingStageName { index, expected } => write!(
                f,
                "stage {} is missing a name in TOML. Expected name: \"{}\".",
                index, expected
            ),
        }
    }
}

impl std::error::Error for StageValidationError {}

// ─── RunPlugin ──────────────────────────────────────────────────────────────

/// Reads `[run]` / `[[run]]` from `Config`, installs [`RunConfig`],
/// [`RunState`], [`StageOverrides`], and the per-iter cycle update.
///
/// Auto-installs:
///   - [`SimClockPlugin`] if not already present.
///   - [`advance_step`] in [`RunSchedule::Cycle`] (skipped if user
///     pre-registered it).
///   - [`update_cycle`] in [`RunSchedule::Cycle`], labelled
///     [`UPDATE_CYCLE`] so other systems can `.before(UPDATE_CYCLE)`.
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
        app.add_update_system(update_cycle.label(UPDATE_CYCLE), RunSchedule::Cycle);

        if app.get_resource_ref::<StageNames>().is_some() {
            app.add_setup_system(
                validate_stages.run_if(first_stage_only()),
                ScheduleSetupSet::PreSetup,
            );
        }
    }

    fn config_description(&self) -> Option<ConfigDescription> {
        Some(StageConfig::description())
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

/// Fallible validation that the count and order of TOML `[[run]]` stages match
/// the `StageEnum` variants registered through [`StageNames`].
pub fn try_validate_stages(
    run_config: &RunConfig,
    stage_names: &StageNames,
) -> Result<(), StageValidationError> {
    let expected = stage_names.0;
    let actual: Vec<Option<String>> = run_config
        .stages
        .iter()
        .map(|s| s.name.as_deref().map(str::to_string))
        .collect();

    if run_config.stages.len() != expected.len() {
        return Err(StageValidationError::StageCountMismatch {
            toml_count: run_config.stages.len(),
            enum_count: expected.len(),
            expected_names: expected.iter().map(|name| (*name).to_string()).collect(),
            toml_names: actual,
        });
    }

    for (i, (expected_name, stage)) in expected.iter().zip(run_config.stages.iter()).enumerate() {
        match &stage.name {
            Some(name) if name != expected_name => {
                return Err(StageValidationError::StageNameMismatch {
                    index: i,
                    expected: (*expected_name).to_string(),
                    actual: name.clone(),
                });
            }
            None => {
                return Err(StageValidationError::MissingStageName {
                    index: i,
                    expected: (*expected_name).to_string(),
                });
            }
            _ => {}
        }
    }

    Ok(())
}

fn report_stage_validation_error(error: &StageValidationError) -> ! {
    eprintln!();
    eprintln!("ERROR: Failed to validate [[run]] stages against StageEnum.");
    eprintln!("  {}", error);
    eprintln!();
    eprintln!(
        "  Hint: when using StageAdvancePlugin, every [[run]] stage must have a \
         `name`, and the stage count and names must match the #[stage(\"...\")] \
         declarations in order."
    );
    eprintln!("  Run with --generate-config to inspect the expected run-stage shape.");
    std::process::exit(1);
}

/// Setup system (registered only when [`StageNames`] is present):
/// validates that the count and order of TOML `[[run]]` stages match
/// the `StageEnum` variants. Reports an actionable diagnostic and exits
/// on mismatch.
pub fn validate_stages(
    run_config: Res<RunConfig>,
    stage_names: Res<StageNames>,
    scheduler_manager: Res<SchedulerManager>,
) {
    if scheduler_manager.index != 0 {
        return;
    }

    try_validate_stages(&run_config, &stage_names)
        .unwrap_or_else(|error| report_stage_validation_error(&error));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_cycle_has_a_stable_typed_system_key() {
        assert_eq!(UPDATE_CYCLE.name(), "update_cycle");
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct SolverKnobs {
        every: u32,
    }

    fn stage_overrides_for(config: &Config, stage_index: usize) -> StageOverrides {
        let run_config = RunConfig::from_config(config);
        let stage = run_config.current_stage(stage_index);
        let mut merged = config.table.clone();
        merged.remove("run");
        deep_merge(&mut merged, &stage.overrides);
        StageOverrides { table: merged }
    }

    #[test]
    fn stage_overrides_try_section_distinguishes_absent_section() {
        let overrides = StageOverrides {
            table: toml::Table::new(),
        };

        let result = overrides.try_section::<SolverKnobs>("solver").unwrap();
        assert_eq!(result, None);

        let value: SolverKnobs = overrides.section("solver");
        assert_eq!(value, SolverKnobs::default());
    }

    #[test]
    fn stage_overrides_try_section_reads_valid_multistage_overrides() {
        let config = Config::from_str(
            r#"
            [solver]
            every = 10

            [[run]]
            name = "settle"
            steps = 100
            solver = { every = 25 }

            [[run]]
            name = "flow"
            steps = 200
            solver = { every = 50 }
            "#,
        );

        let settle = stage_overrides_for(&config, 0);
        let flow = stage_overrides_for(&config, 1);

        assert_eq!(
            settle.try_section::<SolverKnobs>("solver").unwrap(),
            Some(SolverKnobs { every: 25 })
        );
        assert_eq!(
            flow.try_section::<SolverKnobs>("solver").unwrap(),
            Some(SolverKnobs { every: 50 })
        );
    }

    #[test]
    fn stage_overrides_try_section_reports_malformed_per_stage_override() {
        let config = Config::from_str(
            r#"
            [solver]
            every = 10

            [[run]]
            name = "bad-stage"
            steps = 100
            solver = { evrey = 25 }
            "#,
        );
        let overrides = stage_overrides_for(&config, 0);

        let err = overrides.try_section::<SolverKnobs>("solver").unwrap_err();
        assert_eq!(err.key(), "solver");
        assert!(err.to_string().contains("StageOverrides"));
        assert!(err.source().to_string().contains("unknown field `evrey`"));

        let silent: SolverKnobs = overrides.section_or_default("solver");
        assert_eq!(silent, SolverKnobs::default());
    }

    fn run_config(toml: &str) -> RunConfig {
        RunConfig::from_config(&Config::from_str(toml))
    }

    fn expected_stages() -> StageNames {
        StageNames(&["settle", "flow"])
    }

    #[test]
    fn stage_validation_reports_stage_count_mismatch() {
        let run_config = run_config(
            r#"
            [[run]]
            name = "settle"
            steps = 100
            "#,
        );

        let err = try_validate_stages(&run_config, &expected_stages()).unwrap_err();
        assert_eq!(
            err,
            StageValidationError::StageCountMismatch {
                toml_count: 1,
                enum_count: 2,
                expected_names: vec!["settle".to_string(), "flow".to_string()],
                toml_names: vec![Some("settle".to_string())],
            }
        );
        assert!(err.to_string().contains("stage count mismatch"));
        assert!(err.to_string().contains("StageEnum has 2 variants"));
    }

    #[test]
    fn stage_validation_reports_wrong_stage_name() {
        let run_config = run_config(
            r#"
            [[run]]
            name = "settle"
            steps = 100

            [[run]]
            name = "production"
            steps = 200
            "#,
        );

        let err = try_validate_stages(&run_config, &expected_stages()).unwrap_err();
        assert_eq!(
            err,
            StageValidationError::StageNameMismatch {
                index: 1,
                expected: "flow".to_string(),
                actual: "production".to_string(),
            }
        );
        assert!(err.to_string().contains("stage 1 name mismatch"));
        assert!(err.to_string().contains("StageEnum expects \"flow\""));
    }

    #[test]
    fn stage_validation_reports_missing_stage_name() {
        let run_config = run_config(
            r#"
            [[run]]
            name = "settle"
            steps = 100

            [[run]]
            steps = 200
            "#,
        );

        let err = try_validate_stages(&run_config, &expected_stages()).unwrap_err();
        assert_eq!(
            err,
            StageValidationError::MissingStageName {
                index: 1,
                expected: "flow".to_string(),
            }
        );
        assert!(err.to_string().contains("stage 1 is missing a name"));
        assert!(err.to_string().contains("Expected name: \"flow\""));
    }

    #[test]
    fn stage_validation_accepts_valid_multistage_run() {
        let run_config = run_config(
            r#"
            [[run]]
            name = "settle"
            steps = 100

            [[run]]
            name = "flow"
            steps = 200
            "#,
        );

        assert!(try_validate_stages(&run_config, &expected_stages()).is_ok());
    }
}
