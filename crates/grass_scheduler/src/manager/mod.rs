//! Scheduler runtime, lifecycle manager, and state/stage run conditions.

use crate::{
    coherence, schedule, CoherenceRegistry, Condition, IntoCondition, IntoScheduledSystem,
    IntoSystem, Local, OnMax, ResMut, Schedule, ScheduleNode, ScheduleSet, StoredPhase,
    StoredSystemEntry, System,
};
use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;

use crate::param::topo_sort_group;

// ─── Simulation states ────────────────────────────────────────────────────────

/// The currently active simulation state.
///
/// Registered as a resource. Read by [`in_state()`] conditions to gate system execution.
/// Updated automatically by [`apply_state_transitions`] at the end of each step.
pub struct CurrentState<S: Clone + PartialEq + 'static>(pub S);

/// The pending next state, applied at the end of the step by [`apply_state_transitions`].
///
/// Call [`set()`](NextState::set) from any system to request a state transition.
pub struct NextState<S: Clone + PartialEq + 'static>(pub Option<S>);

impl<S: Clone + PartialEq + 'static> NextState<S> {
    /// Requests a transition to the given state at the end of the current step.
    pub fn set(&mut self, state: S) {
        self.0 = Some(state);
    }
    /// Cancels any pending state transition.
    pub fn clear(&mut self) {
        self.0 = None;
    }
}

/// Named condition struct for `in_state()`.
pub struct InStateCondition<S: Clone + PartialEq + 'static> {
    target: S,
    cond_name: String,
    index: usize,
}

/// Disambiguation marker for the [`InStateCondition`] [`IntoCondition`] impl.
pub struct InStateMarker;

impl<S: Clone + PartialEq + 'static> Condition for InStateCondition<S> {
    fn evaluate(&mut self, resources: &[RefCell<Box<dyn Any>>]) -> bool {
        let borrow = resources[self.index].borrow();
        let current = borrow
            .downcast_ref::<CurrentState<S>>()
            .expect("in_state: CurrentState<S> resource type mismatch");
        current.0 == self.target
    }
    fn prepare(&mut self, index: &HashMap<TypeId, usize>) -> Vec<String> {
        let tid = TypeId::of::<CurrentState<S>>();
        match index.get(&tid) {
            Some(&idx) => {
                self.index = idx;
                vec![]
            }
            None => vec![std::any::type_name::<CurrentState<S>>().to_string()],
        }
    }
    fn name(&self) -> &str {
        &self.cond_name
    }
}

impl<S: Clone + PartialEq + 'static> IntoCondition<InStateMarker> for InStateCondition<S> {
    type Condition = Self;
    fn into_condition(self) -> Self {
        self
    }
}

/// Run condition: returns true when the current state equals `target`.
///
/// Reads `CurrentState<S>`. Transitions are applied by
/// [`apply_state_transitions`] and kept in sync with `[[run]]` stages by
/// [`check_stage_advance`] — both of which `grass_app` wires up via
/// `StatesPlugin<S>` / `update_cycle`. Register those (or run them manually
/// at end-of-step) or `CurrentState<S>` never changes.
pub fn in_state<S: Clone + PartialEq + std::fmt::Debug + 'static>(
    target: S,
) -> InStateCondition<S> {
    let cond_name = format!("in_state({:?})", target);
    InStateCondition {
        target,
        cond_name,
        index: usize::MAX,
    }
}

/// Named condition struct for `on_enter_state()`.
///
/// Returns `true` only on the first timestep after transitioning into the target state.
/// Works regardless of whether the transition was triggered by user code (`next_state.set()`)
/// or by stage step exhaustion.
pub struct OnEnterStateCondition<S: Clone + PartialEq + 'static> {
    target: S,
    cond_name: String,
    index: usize,
    was_active: bool,
}

/// Disambiguation marker for the [`OnEnterStateCondition`] [`IntoCondition`] impl.
pub struct OnEnterStateMarker;

impl<S: Clone + PartialEq + 'static> Condition for OnEnterStateCondition<S> {
    fn evaluate(&mut self, resources: &[RefCell<Box<dyn Any>>]) -> bool {
        let borrow = resources[self.index].borrow();
        let current = borrow
            .downcast_ref::<CurrentState<S>>()
            .expect("on_enter_state: CurrentState<S> resource type mismatch");
        let is_active = current.0 == self.target;
        let just_entered = is_active && !self.was_active;
        self.was_active = is_active;
        just_entered
    }
    fn prepare(&mut self, index: &HashMap<TypeId, usize>) -> Vec<String> {
        let tid = TypeId::of::<CurrentState<S>>();
        match index.get(&tid) {
            Some(&idx) => {
                self.index = idx;
                vec![]
            }
            None => vec![std::any::type_name::<CurrentState<S>>().to_string()],
        }
    }
    fn name(&self) -> &str {
        &self.cond_name
    }
}

impl<S: Clone + PartialEq + 'static> IntoCondition<OnEnterStateMarker>
    for OnEnterStateCondition<S>
{
    type Condition = Self;
    fn into_condition(self) -> Self {
        self
    }
}

/// Run condition: returns true only on the **first timestep** after entering the target state.
///
/// Works for both code-triggered transitions (`next_state.set()`) and automatic
/// transitions when a stage runs out of steps. Use this for one-shot actions like
/// deactivating walls, printing messages, or changing parameters at stage boundaries.
///
/// # Example
/// ```rust,ignore
/// app.add_update_system(
///     open_gate.run_if(on_enter_state(Phase::Drain)),
///     ScheduleSet::PostFinalIntegration,
/// );
/// ```
pub fn on_enter_state<S: Clone + PartialEq + std::fmt::Debug + 'static>(
    target: S,
) -> OnEnterStateCondition<S> {
    let cond_name = format!("on_enter_state({:?})", target);
    OnEnterStateCondition {
        target,
        cond_name,
        index: usize::MAX,
        was_active: false,
    }
}

/// System that applies pending state transitions at end of step.
/// Register via `StatesPlugin<S>` or manually at `PostFinalIntegration`.
pub fn apply_state_transitions<S: Clone + PartialEq + 'static>(
    mut current: ResMut<CurrentState<S>>,
    mut next: ResMut<NextState<S>>,
) {
    if let Some(new_state) = next.0.take() {
        current.0 = new_state;
    }
}

/// Trait for enums that map 1:1 to named `[[run]]` stages.
///
/// Derive with `#[derive(StageEnum)]` and `#[stage("name")]` attributes.
pub trait StageName: Sized {
    /// Returns the stage name string for this variant.
    fn stage_name(&self) -> &'static str;
    /// Returns all stage names in variant order.
    fn stage_names() -> &'static [&'static str];
    /// Returns the number of stages.
    fn num_stages() -> usize;
    /// Returns the variant corresponding to stage index `i`, or `None` if out of range.
    fn from_index(i: usize) -> Option<Self>;
}

/// Named condition struct for `in_stage()`.
pub struct InStageCondition {
    stage: String,
    cond_name: String,
    index: usize,
}

/// Disambiguation marker for the [`InStageCondition`] [`IntoCondition`] impl.
pub struct InStageMarker;

impl Condition for InStageCondition {
    fn evaluate(&mut self, resources: &[RefCell<Box<dyn Any>>]) -> bool {
        let borrow = resources[self.index].borrow();
        let sm = borrow
            .downcast_ref::<SchedulerManager>()
            .expect("in_stage: SchedulerManager resource type mismatch");
        sm.stage_name.as_deref() == Some(self.stage.as_str())
    }
    fn prepare(&mut self, index: &HashMap<TypeId, usize>) -> Vec<String> {
        let tid = TypeId::of::<SchedulerManager>();
        match index.get(&tid) {
            Some(&idx) => {
                self.index = idx;
                vec![]
            }
            None => vec![std::any::type_name::<SchedulerManager>().to_string()],
        }
    }
    fn name(&self) -> &str {
        &self.cond_name
    }
}

impl IntoCondition<InStageMarker> for InStageCondition {
    type Condition = Self;
    fn into_condition(self) -> Self {
        self
    }
}

/// Run condition: returns true when the current stage name matches.
///
/// Reads `SchedulerManager::stage_name`. That field is **only** populated by
/// the run-stage driver in `grass_app` (`update_cycle` walking the `[[run]]`
/// config's stages); a bare `Scheduler` leaves it `None`, so `in_stage(..)`
/// is always `false` unless something sets the stage. See `grass_app`'s
/// `RunPlugin` / `StatesPlugin`.
pub fn in_stage(name: &str) -> InStageCondition {
    let cond_name = format!("in_stage({})", name);
    InStageCondition {
        stage: name.to_string(),
        cond_name,
        index: usize::MAX,
    }
}

/// Named condition struct for `on_enter_stage()`.
///
/// Returns `true` only on the first timestep after entering the named stage.
pub struct OnEnterStageCondition {
    stage: String,
    cond_name: String,
    index: usize,
    was_active: bool,
}

/// Disambiguation marker for the [`OnEnterStageCondition`] [`IntoCondition`] impl.
pub struct OnEnterStageMarker;

impl Condition for OnEnterStageCondition {
    fn evaluate(&mut self, resources: &[RefCell<Box<dyn Any>>]) -> bool {
        let borrow = resources[self.index].borrow();
        let sm = borrow
            .downcast_ref::<SchedulerManager>()
            .expect("on_enter_stage: SchedulerManager resource type mismatch");
        let is_active = sm.stage_name.as_deref() == Some(self.stage.as_str());
        let just_entered = is_active && !self.was_active;
        self.was_active = is_active;
        just_entered
    }
    fn prepare(&mut self, index: &HashMap<TypeId, usize>) -> Vec<String> {
        let tid = TypeId::of::<SchedulerManager>();
        match index.get(&tid) {
            Some(&idx) => {
                self.index = idx;
                vec![]
            }
            None => vec![std::any::type_name::<SchedulerManager>().to_string()],
        }
    }
    fn name(&self) -> &str {
        &self.cond_name
    }
}

impl IntoCondition<OnEnterStageMarker> for OnEnterStageCondition {
    type Condition = Self;
    fn into_condition(self) -> Self {
        self
    }
}

/// Run condition: returns true only on the **first timestep** after entering the named stage.
///
/// Works for both code-triggered transitions and automatic transitions when a
/// stage runs out of steps.
///
/// # Example
/// ```rust,ignore
/// app.add_update_system(
///     open_gate.run_if(on_enter_stage("drain")),
///     ScheduleSet::PostFinalIntegration,
/// );
/// ```
pub fn on_enter_stage(name: &str) -> OnEnterStageCondition {
    let cond_name = format!("on_enter_stage({})", name);
    OnEnterStageCondition {
        stage: name.to_string(),
        cond_name,
        index: usize::MAX,
        was_active: false,
    }
}

/// Named condition struct for `first_stage_only()`.
pub struct FirstStageOnlyCondition {
    index: usize,
}

/// Disambiguation marker for the [`FirstStageOnlyCondition`] [`IntoCondition`] impl.
pub struct FirstStageOnlyMarker;

impl Condition for FirstStageOnlyCondition {
    fn evaluate(&mut self, resources: &[RefCell<Box<dyn Any>>]) -> bool {
        let borrow = resources[self.index].borrow();
        let sm = borrow
            .downcast_ref::<SchedulerManager>()
            .expect("first_stage_only: SchedulerManager resource type mismatch");
        sm.index == 0
    }
    fn prepare(&mut self, index: &HashMap<TypeId, usize>) -> Vec<String> {
        let tid = TypeId::of::<SchedulerManager>();
        match index.get(&tid) {
            Some(&idx) => {
                self.index = idx;
                vec![]
            }
            None => vec![std::any::type_name::<SchedulerManager>().to_string()],
        }
    }
    fn name(&self) -> &str {
        "first_stage_only()"
    }
}

impl IntoCondition<FirstStageOnlyMarker> for FirstStageOnlyCondition {
    type Condition = Self;
    fn into_condition(self) -> Self {
        self
    }
}

/// Run condition: returns true only during the first stage (index == 0).
pub fn first_stage_only() -> FirstStageOnlyCondition {
    FirstStageOnlyCondition { index: usize::MAX }
}

/// System that keeps `CurrentState<S>` in sync with the scheduler stage and
/// detects user-initiated state transitions to request early stage advancement.
///
/// Two responsibilities:
/// 1. **User transition → advance**: When user code sets `NextState<S>` and
///    `apply_state_transitions` updates `CurrentState`, this system detects
///    the change and sets `advance_requested = true` so `update_cycle` skips
///    remaining steps.
/// 2. **Stage exhaustion → sync**: When `update_cycle` advances the stage index
///    because steps ran out, this system sets `CurrentState` to match the new
///    stage so `in_state()` conditions work correctly.
pub fn check_stage_advance<S: StageName + Clone + PartialEq + 'static>(
    mut current: ResMut<CurrentState<S>>,
    mut next: ResMut<NextState<S>>,
    mut sm: ResMut<SchedulerManager>,
    mut prev: Local<Option<S>>,
    mut prev_index: Local<Option<usize>>,
) {
    // ── Detect stage-index changes (steps exhausted) and sync state ──
    let idx = sm.index;
    if prev_index.as_ref() != Some(&idx) {
        if prev_index.is_some() {
            // Stage index advanced — sync CurrentState to match
            if let Some(target) = S::from_index(idx) {
                if current.0 != target {
                    current.0 = target.clone();
                    next.clear(); // cancel any stale pending transition
                                  // Update prev so we don't misinterpret this as a user transition
                    *prev = Some(target);
                }
            }
        }
        *prev_index = Some(idx);
    }

    // ── Detect user-initiated state transitions → request advance ──
    if prev.as_ref() != Some(&current.0) {
        if prev.is_some() {
            // State changed by user code — request advance to next stage
            sm.advance_requested = true;
        }
        *prev = Some(current.0.clone());
    }
}

// ─── Scheduler ────────────────────────────────────────────────────────────────

// ANCHOR: Scheduler
/// The central scheduler: manages system registration, resource storage, ordering, and
/// per-step execution.
///
/// # Usage
///
/// 1. Create a scheduler with [`Scheduler::default()`].
/// 2. Register resources with [`add_resource`](Scheduler::add_resource).
/// 3. Register systems with [`add_setup_system`](Scheduler::add_setup_system) and
///    [`add_update_system`](Scheduler::add_update_system).
/// 4. Call [`start`](Scheduler::start) to run the simulation loop.
///
/// # Environment Variables
///
/// - `SIM_TRACE` — when set, prints each system name to stderr as it executes.
/// - `SIM_SUPPRESS_WARNINGS` — when set, suppresses schedule validation warnings.
pub struct Scheduler {
    /// Setup systems, run once per stage before the main loop.
    pub(crate) setup_systems: Vec<(StoredSystemEntry, StoredPhase)>,
    /// Update systems, run every timestep in the main loop.
    pub(crate) update_systems: Vec<(StoredSystemEntry, StoredPhase)>,
    /// Persistent namespace assignment per `ScheduleSet` enum type. Set by
    /// [`set_schedule_namespace`](Self::set_schedule_namespace); consulted
    /// by [`add_setup_system`](Self::add_setup_system) /
    /// [`add_update_system`](Self::add_update_system) so registrations
    /// made AFTER a namespace assignment pick up that namespace too.
    pub(crate) phase_namespaces: HashMap<TypeId, u32>,
    /// Flat resource storage, indexed by position.
    pub resources: Vec<RefCell<Box<dyn Any>>>,
    /// Maps `TypeId` → index into `resources`.
    pub(crate) resource_index: HashMap<TypeId, usize>,
    /// Whether to write a DOT file of the schedule after organizing.
    pub(crate) print_schedule: bool,
    /// Cumulative per-system wall-clock timing (seconds), indexed by update system position.
    pub(crate) system_timings: Vec<f64>,
    /// Number of timesteps completed (for timing averages).
    pub(crate) timing_steps: usize,
    /// When true, prints system names to stderr during execution.
    pub(crate) trace: bool,
    /// When true, suppresses schedule validation warnings.
    pub(crate) suppress_warnings: bool,
    /// Stage names from `[[run]]` config sections (for multi-stage simulations).
    pub(crate) stage_names: Vec<String>,
    /// Optional callback that produces domain-specific schedule warnings.
    /// Receives the list of phase names from registered update systems.
    #[allow(clippy::type_complexity)]
    pub(crate) warning_fn: Option<Box<dyn Fn(&[&str]) -> Vec<String>>>,
    /// Optional hierarchical schedule. When `Some`, [`run`](Self::run) walks
    /// the tree (re-iterating `Loop` nodes, dispatching `Phase` nodes to a
    /// filtered run pass); when `None`, falls back to today's flat
    /// `(namespace, index)`-sorted run. Set via [`set_schedule`](Self::set_schedule).
    pub(crate) schedule: Option<Schedule>,
    /// Cached resource index of the [`CoherenceRegistry`], if one is registered.
    /// `None` (the default) makes the per-system coherence hooks a no-op, so
    /// CPU-only runs pay nothing. Resolved in [`organize_systems`](Self::organize_systems).
    pub(crate) coherence_index: Option<usize>,
}
// ANCHOR_END: Scheduler

// Scheduler::default() is manually implemented because it appears in the book's ANCHOR blocks.
#[allow(clippy::derivable_impls)]
impl Default for Scheduler {
    fn default() -> Self {
        Scheduler {
            setup_systems: Vec::new(),
            update_systems: Vec::new(),
            phase_namespaces: HashMap::new(),
            resources: Vec::new(),
            resource_index: HashMap::new(),
            print_schedule: false,
            system_timings: Vec::new(),
            timing_steps: 0,
            trace: std::env::var("SIM_TRACE").is_ok(),
            suppress_warnings: std::env::var("SIM_SUPPRESS_WARNINGS").is_ok(),
            stage_names: Vec::new(),
            warning_fn: None,
            schedule: None,
            coherence_index: None,
        }
    }
}

// ANCHOR: SchedulerImpl
impl Scheduler {
    /// Sorts all registered systems by [`ScheduleSet`] phase, topologically sorts within
    /// each phase, resolves resource indices, and validates the schedule.
    ///
    /// # Panics
    ///
    /// - If any non-optional system parameter references a resource that hasn't been registered.
    /// - If a `requires_label` constraint references a label not present in its [`ScheduleSet`].
    /// - If a cycle is detected in `before`/`after` ordering constraints.
    pub fn organize_systems(&mut self) {
        self.setup_systems
            .sort_by_key(|(_, phase)| phase.sort_key());
        self.update_systems
            .sort_by_key(|(_, phase)| phase.sort_key());

        // Topo sort within each phase group (same sort_key = same group)
        let all = std::mem::take(&mut self.update_systems);
        if all.is_empty() {
            // Still prepare setup systems
            let mut errors: Vec<String> = Vec::new();
            for (entry, _) in &mut self.setup_systems {
                for missing in entry.system.prepare(&self.resource_index) {
                    errors.push(format!(
                        "  System \"{}\" requires `{}`",
                        entry.name, missing
                    ));
                }
            }
            if !errors.is_empty() {
                panic!("Schedule validation errors:\n{}", errors.join("\n"));
            }
            return;
        }

        let mut groups: Vec<Vec<(StoredSystemEntry, StoredPhase)>> = Vec::new();
        for entry in all {
            let key = entry.1.sort_key();
            if let Some(last) = groups.last_mut() {
                if last[0].1.sort_key() == key {
                    last.push(entry);
                    continue;
                }
            }
            groups.push(vec![entry]);
        }

        let mut errors: Vec<String> = Vec::new();
        for mut group in groups {
            topo_sort_group(&mut group);

            // Validate requires_label: every required label/name must exist in this group
            let mut known_labels: Vec<&str> = Vec::new();
            for (entry, _) in &group {
                known_labels.push(&entry.name);
                if let Some(lbl) = &entry.label {
                    known_labels.push(lbl);
                }
            }
            for (entry, phase) in &group {
                for req in &entry.requires {
                    if !known_labels.contains(&req.as_str()) {
                        errors.push(format!(
                            "  System \"{}\" in {} requires label \"{}\" which is not present in that ScheduleSet",
                            entry.name, phase.phase_name(), req
                        ));
                    }
                }
            }

            self.update_systems.extend(group);
        }

        // Prepare all systems with cached resource indices, collecting missing resource errors
        for (entry, _) in &mut self.setup_systems {
            for missing in entry.system.prepare(&self.resource_index) {
                errors.push(format!(
                    "  System \"{}\" requires `{}`",
                    entry.name, missing
                ));
            }
        }
        for (entry, _) in &mut self.update_systems {
            for missing in entry.system.prepare(&self.resource_index) {
                errors.push(format!(
                    "  System \"{}\" requires `{}`",
                    entry.name, missing
                ));
            }
        }
        if !errors.is_empty() {
            panic!("Schedule validation errors:\n{}", errors.join("\n"));
        }

        // Coherence (coherence_plan.md): cache the CoherenceRegistry slot and
        // resolve each mirror's trigger index. Absent → the run-loop hooks no-op.
        self.coherence_index = self
            .resource_index
            .get(&TypeId::of::<CoherenceRegistry>())
            .copied();
        if let Some(ci) = self.coherence_index {
            let mut guard = self.resources[ci].borrow_mut();
            guard
                .downcast_mut::<CoherenceRegistry>()
                .expect("coherence: slot is not a CoherenceRegistry")
                .resolve_indices(&self.resource_index);
        }

        // Initialize index-based timing vector
        self.system_timings = vec![0.0; self.update_systems.len()];

        // Emit non-blocking warnings for suspicious schedule configurations
        self.validate_schedule();
    }

    /// Returns warning strings for suspicious schedule configurations.
    /// Called at the end of `organize_systems()` to print warnings to stderr.
    ///
    /// Delegates to the registered `warning_fn` callback if one has been set
    /// via [`set_warning_fn`](Self::set_warning_fn). Returns an empty list if
    /// no callback is registered.
    pub fn schedule_warnings(&self) -> Vec<String> {
        if self.update_systems.is_empty() {
            return Vec::new();
        }
        match &self.warning_fn {
            Some(f) => {
                let phase_names: Vec<&str> = self
                    .update_systems
                    .iter()
                    .map(|(_, phase)| phase.phase_name())
                    .collect();
                f(&phase_names)
            }
            None => Vec::new(),
        }
    }

    fn validate_schedule(&self) {
        if self.suppress_warnings {
            return;
        }
        for warning in self.schedule_warnings() {
            eprintln!("{}", warning);
        }
    }

    /// Runs all setup systems in order. Called once per stage before the run loop.
    pub fn setup(&mut self) {
        for (entry, _) in self.setup_systems.iter_mut() {
            entry.system.run(&self.resources);
        }
    }

    /// Executes one timestep: runs all update systems in order, recording per-system timing.
    ///
    /// If a [`Schedule`] has been installed via [`set_schedule`](Self::set_schedule), this
    /// walks the tree (re-iterating `Loop` nodes, dispatching `Phase` nodes to a
    /// namespace-filtered pass). Otherwise it falls back to the flat
    /// `(namespace, index)`-sorted run that's been the default since 0.x —
    /// so existing schedules behave identically without opting in.
    pub fn run(&mut self) {
        if self.schedule.is_some() {
            self.run_with_schedule();
        } else {
            self.run_flat();
        }
    }

    /// The flat, sorted-by-`(namespace, index)` run loop. Public accessor for
    /// the no-schedule path; use [`run`](Self::run) unless you want to bypass
    /// any installed [`Schedule`].
    pub fn run_flat(&mut self) {
        let coh = self.coherence_index;
        for (idx, (entry, phase)) in self.update_systems.iter_mut().enumerate() {
            if self.trace {
                eprintln!(
                    "[step {}] {}: {}",
                    self.timing_steps,
                    phase.phase_name(),
                    entry.name
                );
            }
            if let Some(ci) = coh {
                coherence::ensure_coherent(
                    &self.resources,
                    ci,
                    entry.system.accesses(),
                    &entry.name,
                );
            }
            let t0 = std::time::Instant::now();
            entry.system.run(&self.resources);
            self.system_timings[idx] += t0.elapsed().as_secs_f64();
            if let Some(ci) = coh {
                coherence::mark_writes(&self.resources, ci, entry.system.accesses());
            }
        }
        self.timing_steps += 1;
    }

    /// Run one timestep following the installed [`Schedule`]. Panics if no
    /// schedule has been set; prefer [`run`](Self::run) which falls back.
    pub fn run_with_schedule(&mut self) {
        let mut sched = self
            .schedule
            .take()
            .expect("Scheduler::run_with_schedule called without an installed Schedule");
        self.run_node(&mut sched.root);
        self.timing_steps += 1;
        self.schedule = Some(sched);
    }

    /// Recursive walker. `node` is borrowed from a `Schedule` that's been
    /// taken out of `self.schedule` for the duration of the run, so the
    /// borrow checker accepts simultaneous `&mut self` (for resources +
    /// update_systems) and `&mut node` (a separate heap allocation).
    fn run_node(&mut self, node: &mut ScheduleNode) {
        match node {
            ScheduleNode::Phase { namespace, .. } => {
                self.run_namespace_filtered(*namespace);
            }
            ScheduleNode::Sequence(children) => {
                for c in children.iter_mut() {
                    self.run_node(c);
                }
            }
            ScheduleNode::Loop {
                body,
                until,
                max_iters,
                on_max,
            } => {
                let max = *max_iters;
                for _iter in 0..max {
                    self.run_node(body);
                    if until.evaluate(&self.resources) {
                        return;
                    }
                }
                match on_max {
                    OnMax::AcceptUnconverged => {}
                    OnMax::Panic => panic!(
                        "Schedule Loop did not converge in {} iterations (until = `{}`)",
                        max,
                        until.name()
                    ),
                    OnMax::Rollback(rollback) => {
                        self.run_node(rollback);
                    }
                }
            }
            ScheduleNode::Branch { arms } => {
                for (cond, body) in arms.iter_mut() {
                    if cond.evaluate(&self.resources) {
                        self.run_node(body);
                        return;
                    }
                }
                // No arm matched — graceful no-op.
            }
        }
    }

    /// Run only update systems whose namespace equals `ns`. Used by
    /// `run_node` for `ScheduleNode::Phase` dispatch.
    fn run_namespace_filtered(&mut self, ns: u32) {
        let coh = self.coherence_index;
        for (idx, (entry, phase)) in self.update_systems.iter_mut().enumerate() {
            if phase.namespace != ns {
                continue;
            }
            if self.trace {
                eprintln!(
                    "[step {}] {}: {}",
                    self.timing_steps,
                    phase.phase_name(),
                    entry.name
                );
            }
            if let Some(ci) = coh {
                coherence::ensure_coherent(
                    &self.resources,
                    ci,
                    entry.system.accesses(),
                    &entry.name,
                );
            }
            let t0 = std::time::Instant::now();
            entry.system.run(&self.resources);
            self.system_timings[idx] += t0.elapsed().as_secs_f64();
            if let Some(ci) = coh {
                coherence::mark_writes(&self.resources, ci, entry.system.accesses());
            }
        }
    }

    /// Install a hierarchical [`Schedule`] for this scheduler.
    ///
    /// Lowering happens here:
    /// 1. Tree-walk-order namespace assignment (`assign_namespaces`).
    /// 2. Already-registered systems matching each `Phase`'s `TypeId` get
    ///    their `namespace` field rewritten (so the flat `update_systems`
    ///    Vec carries the right `(namespace, index)` keys for the namespace-
    ///    filtered run pass).
    /// 3. Every `Loop`'s `until` condition is `prepare`d against the current
    ///    `resource_index`, so it can resolve resource slots when invoked.
    ///
    /// Call **after** all systems have been registered (and after
    /// [`add_resource`](Self::add_resource) for any resource a `Loop`'s
    /// `until` condition reads). Repeat calls replace the previously-installed
    /// schedule.
    ///
    /// # Panics
    ///
    /// - If the tree references the same `ScheduleSet` enum type both as a
    ///   whole-enum phase (`.then::<E>()`) and as a per-variant phase
    ///   (`.then_variant(E::V)`) — the namespace assignment would be
    ///   ambiguous.
    /// - If any `Loop`/`Branch` condition needs a resource that has not been
    ///   registered yet (conditions are `prepare`d here against the current
    ///   resource index).
    pub fn set_schedule(&mut self, mut schedule: Schedule) {
        // 1. Assign namespaces in tree-walk order.
        let mut counter: u32 = 0;
        schedule::assign_namespaces(&mut schedule.root, &mut counter);

        // 2. Collect (type_id, variant, namespace) tuples from the tree.
        let mut assignments: Vec<schedule::PhaseAssignment> = Vec::new();
        schedule::collect_phase_assignments(&schedule.root, &mut assignments);

        // 3. Conflict check: a single TypeId can't appear both as a
        //    whole-enum (`then::<E>()`) AND a per-variant
        //    (`then_variant(E::V)`) reference in the same tree —
        //    systems would match both, and namespace assignment would be
        //    ambiguous.
        for a in &assignments {
            if a.variant.is_none() {
                if let Some(other) = assignments
                    .iter()
                    .find(|b| b.type_id == a.type_id && b.variant.is_some())
                {
                    panic!(
                        "Schedule validation: phase type appears both as whole-enum \
                         (`then::<E>()`) and per-variant (`then_variant(E::V)`) \
                         in one tree. namespaces {} and {} would both claim systems.",
                        a.namespace, other.namespace,
                    );
                }
            }
        }

        // 4. Apply assignments to already-registered systems'
        //    StoredPhases. Per-variant assignments take precedence: a
        //    system whose StoredPhase matches both `Some(idx)` and a
        //    fallback `None` would only be matched by `Some(idx)` —
        //    but step 3 already rejects that mix per TypeId.
        for a in &assignments {
            for (_, phase) in &mut self.setup_systems {
                if phase.schedule_type_id == a.type_id
                    && match a.variant {
                        Some(v) => phase.index == v,
                        None => true,
                    }
                {
                    phase.namespace = a.namespace;
                }
            }
            for (_, phase) in &mut self.update_systems {
                if phase.schedule_type_id == a.type_id
                    && match a.variant {
                        Some(v) => phase.index == v,
                        None => true,
                    }
                {
                    phase.namespace = a.namespace;
                }
            }
        }

        // 5. Prepare conditions so `evaluate` can find its resources.
        let errors = schedule::prepare_conditions(&mut schedule.root, &self.resource_index);
        if !errors.is_empty() {
            panic!(
                "Schedule validation errors (Loop conditions):\n  {}",
                errors.join("\n  ")
            );
        }

        self.schedule = Some(schedule);
    }

    /// Returns `true` if a [`Schedule`] has been installed via
    /// [`set_schedule`](Self::set_schedule).
    pub fn has_schedule(&self) -> bool {
        self.schedule.is_some()
    }

    /// Runs the full simulation lifecycle: organize → setup → run loop → timing summary.
    ///
    /// The loop continues until a system sets [`SchedulerManager::state`] to
    /// [`SchedulerState::End`].
    pub fn start(&mut self) {
        self.add_scheduler_manager();
        let mut schedule_state = SchedulerState::Setup;
        while !matches!(schedule_state, SchedulerState::End) {
            if matches!(schedule_state, SchedulerState::Setup) {
                self.organize_systems();
                if self.print_schedule {
                    self.write_dot("schedule.dot");
                }
                self.setup();

                let sm_idx = self.resource_index[&TypeId::of::<SchedulerManager>()];
                let mut binding = self.resources[sm_idx].borrow_mut();
                let sm = binding
                    .downcast_mut::<SchedulerManager>()
                    .expect("SchedulerManager resource missing or wrong type");
                sm.state = SchedulerState::Run;
            }

            if matches!(schedule_state, SchedulerState::Run) {
                self.run();
            }

            let sm_idx = self.resource_index[&TypeId::of::<SchedulerManager>()];
            let mut binding = self.resources[sm_idx].borrow_mut();
            let sm = binding
                .downcast_mut::<SchedulerManager>()
                .expect("SchedulerManager resource missing or wrong type");
            schedule_state = sm.state;
        }

        // Print per-system timing breakdown
        if self.timing_steps > 0 && !self.system_timings.is_empty() {
            let total: f64 = self.system_timings.iter().sum();
            let mut sorted: Vec<_> = self
                .update_systems
                .iter()
                .zip(self.system_timings.iter())
                .map(|((entry, _), &time)| (&entry.name, time, entry.system.group_info()))
                .collect::<Vec<_>>();
            sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            println!("\n--- Per-system timing ({} steps) ---", self.timing_steps);
            println!("{:<50} {:>10} {:>8}", "System", "Time(s)", "%");
            println!("{}", "-".repeat(70));
            for (name, time, group_info) in &sorted {
                let pct = *time / total * 100.0;
                println!("{:<50} {:>10.4} {:>7.1}%", name, time, pct);
                if let Some(info) = group_info {
                    for (j, ((sys_name, phase_name), inner_time)) in info
                        .inner_systems
                        .iter()
                        .zip(info.inner_timings.iter())
                        .enumerate()
                    {
                        let _ = j;
                        let inner_pct = *inner_time / total * 100.0;
                        let label = format!("  {}: {}", phase_name, sys_name);
                        println!("{:<50} {:>10.4} {:>7.1}%", label, inner_time, inner_pct);
                    }
                }
            }
            println!("{}", "-".repeat(70));
            println!("{:<50} {:>10.4} {:>7.1}%", "TOTAL", total, 100.0);
        }
    }

    /// Registers a system to run once during the setup phase.
    pub fn add_setup_system<M>(
        &mut self,
        system: impl IntoScheduledSystem<M>,
        schedule_set: impl ScheduleSet,
    ) {
        let mut phase = StoredPhase::new(schedule_set);
        if let Some(&ns) = self.phase_namespaces.get(&phase.schedule_type_id) {
            phase.namespace = ns;
        }
        self.setup_systems.push((system.into_stored(), phase));
    }

    /// Registers a system to run every timestep in the given schedule set phase.
    pub fn add_update_system<M>(
        &mut self,
        system: impl IntoScheduledSystem<M>,
        schedule_set: impl ScheduleSet,
    ) {
        let mut phase = StoredPhase::new(schedule_set);
        if let Some(&ns) = self.phase_namespaces.get(&phase.schedule_type_id) {
            phase.namespace = ns;
        }
        self.update_systems.push((system.into_stored(), phase));
    }

    /// Returns `true` if a system with the same name (== `type_name` of
    /// the function) has already been registered as an update system.
    /// Plugins use this to avoid duplicate auto-registration when the
    /// user has already wired the system manually.
    pub fn has_update_system<I, S: System + 'static>(
        &self,
        system: impl IntoSystem<I, System = S>,
    ) -> bool {
        let sys = system.into_system();
        let name = sys.name();
        self.update_systems
            .iter()
            .any(|(entry, _)| entry.name == name)
    }

    /// Assigns a namespace to all systems registered under the given [`ScheduleSet`] enum type.
    ///
    /// Systems are sorted by `(namespace, index)` during [`organize_systems`](Self::organize_systems).
    /// Use this to control cross-solver ordering. For example, a coupling plugin might do:
    ///
    /// ```rust,ignore
    /// app.set_schedule_namespace::<CouplingPrePhase>(0);
    /// app.set_schedule_namespace::<FluidPhase>(1);
    /// app.set_schedule_namespace::<MaterialPhase>(2);
    /// app.set_schedule_namespace::<CouplingPostPhase>(3);
    /// ```
    pub fn set_schedule_namespace<P: ScheduleSet + 'static>(&mut self, namespace: u32) {
        let target = TypeId::of::<P>();
        // Persist the assignment so registrations made AFTER this call
        // also pick up the namespace.
        self.phase_namespaces.insert(target, namespace);
        // Retroactively update systems registered BEFORE this call.
        for (_, phase) in &mut self.setup_systems {
            if phase.schedule_type_id == target {
                phase.namespace = namespace;
            }
        }
        for (_, phase) in &mut self.update_systems {
            if phase.schedule_type_id == target {
                phase.namespace = namespace;
            }
        }
    }

    /// Removes an update system by its function handle (matching on `type_name`).
    pub fn remove_update_system<I, S: System + 'static>(
        &mut self,
        system: impl IntoSystem<I, System = S>,
    ) {
        let sys = system.into_system();
        let name = sys.name();
        self.update_systems.retain(|(entry, _)| entry.name != name);
    }

    /// Removes all update systems whose explicit label matches `label`.
    pub fn remove_update_system_by_label(&mut self, label: &str) {
        self.update_systems
            .retain(|(entry, _)| entry.label.as_deref() != Some(label));
    }

    /// Registers a default [`SchedulerManager`] resource (called automatically by [`start`](Self::start)).
    pub fn add_scheduler_manager(&mut self) {
        self.add_resource(SchedulerManager::new());
    }

    /// Inserts or replaces a typed resource. If a resource of type `R` already exists,
    /// it is overwritten in place.
    pub fn add_resource<R: 'static>(&mut self, res: R) {
        let type_id = TypeId::of::<R>();
        if let Some(&idx) = self.resource_index.get(&type_id) {
            self.resources[idx] = RefCell::new(Box::new(res));
        } else {
            let idx = self.resources.len();
            self.resources.push(RefCell::new(Box::new(res)));
            self.resource_index.insert(type_id, idx);
        }
    }

    /// Returns a reference to the [`RefCell`]-wrapped resource for the given [`TypeId`],
    /// or `None` if no resource of that type has been registered.
    pub fn get_mut_resource(&mut self, res: TypeId) -> Option<&RefCell<Box<dyn Any>>> {
        self.resource_index
            .get(&res)
            .map(|&idx| &self.resources[idx])
    }

    /// Same as [`get_mut_resource`](Self::get_mut_resource) but with shared (`&self`)
    /// receiver. The returned `&RefCell` is itself mutability-bearing, so this
    /// is safe — callers borrow_mut on the cell, not on the scheduler.
    ///
    /// Used by `grass_multi`'s cross-namespace SystemParams (`Multi` /
    /// `MultiRes` / `MultiResMut`) to read or write a resource from a
    /// named sub-App's resource store.
    pub fn resource_cell(&self, res: TypeId) -> Option<&RefCell<Box<dyn Any>>> {
        self.resource_index
            .get(&res)
            .map(|&idx| &self.resources[idx])
    }

    /// Returns a shared borrow of a resource by type, or `None` if not registered.
    ///
    /// Useful for inspecting resources outside the system execution context (e.g., in tests).
    pub fn get_resource_ref<R: 'static>(&self) -> Option<std::cell::Ref<'_, R>> {
        self.resource_index.get(&TypeId::of::<R>()).map(|&idx| {
            std::cell::Ref::map(self.resources[idx].borrow(), |b| {
                b.downcast_ref::<R>()
                    .expect("get_resource_ref: resource type mismatch during downcast")
            })
        })
    }

    /// Enables writing a Graphviz DOT file (`schedule.dot`) after organizing systems.
    pub fn enable_schedule_print(&mut self) {
        self.print_schedule = true;
    }

    /// Sets the stage names for multi-stage simulations (from `[[run]]` config sections).
    ///
    /// When set, the DOT output uses per-stage sub-graphs, and conditions like
    /// [`in_stage()`] and [`first_stage_only()`] can filter systems by stage.
    pub fn set_stage_names(&mut self, names: &[&str]) {
        self.stage_names = names.iter().map(|s| s.to_string()).collect();
    }

    /// Registers a callback that produces domain-specific schedule warnings.
    ///
    /// The callback receives a slice of phase names (one per registered update system)
    /// and should return a `Vec<String>` of warning messages. These are printed to
    /// stderr at the end of [`organize_systems`](Self::organize_systems) unless
    /// warnings are suppressed.
    pub fn set_warning_fn(&mut self, f: impl Fn(&[&str]) -> Vec<String> + 'static) {
        self.warning_fn = Some(Box::new(f));
    }
}

// ─── SchedulerState / SchedulerManager ───────────────────────────────────────

/// The lifecycle phase of the scheduler's main loop.
///
/// Systems can read and write this via [`SchedulerManager`] to control the simulation lifecycle.
#[derive(Debug, Clone, Copy)]
pub enum SchedulerState {
    /// Initial phase: organizing systems and running setup systems.
    Setup,
    /// Main simulation loop: executing update systems each timestep.
    Run,
    /// Signals the scheduler to exit the main loop and print timing results.
    End,
}

/// Tracks the current run stage index and scheduler state (Setup/Run/End).
pub struct SchedulerManager {
    /// Current lifecycle phase (`Setup`/`Run`/`End`).
    pub state: SchedulerState,
    /// Index of the current run stage within the `[[run]]` schedule.
    pub index: usize,
    /// Name of the current stage from `[[run]]` config (if set).
    pub stage_name: Option<String>,
    /// When true, the scheduler will advance to the next stage at end of step.
    pub advance_requested: bool,
}

impl Default for SchedulerManager {
    fn default() -> Self {
        SchedulerManager {
            state: SchedulerState::Setup,
            index: 0,
            stage_name: None,
            advance_requested: false,
        }
    }
}

impl SchedulerManager {
    /// Creates a `SchedulerManager` in the initial `Setup` state at stage 0.
    /// Equivalent to [`SchedulerManager::default`].
    pub fn new() -> Self {
        Self::default()
    }
}
