//! Dependency-injection scheduler with [`Res`]/[`ResMut`] resource access and ordered system execution.
//!
//! # Overview
//!
//! This crate provides a lightweight, Bevy-inspired scheduler for explicit, time-stepping
//! particle and grid solvers.
//! Systems are plain functions whose parameters implement [`SystemParam`]. The scheduler
//! resolves resource indices at startup and executes systems in [`ScheduleSet`] order each
//! timestep.
//!
//! # Architecture
//!
//! - **Resources**: Typed values stored in a flat `Vec<RefCell<Box<dyn Any>>>`, indexed by
//!   [`TypeId`](std::any::TypeId). Systems access them via [`Res<T>`] (shared) or
//!   [`ResMut<T>`] (exclusive).
//! - **Systems**: Functions with up to 10 [`SystemParam`] parameters, automatically converted
//!   via [`IntoSystem`]. Systems are grouped into [`ScheduleSet`] phases and topologically
//!   sorted within each phase using `before`/`after` constraints.
//! - **Conditions**: Boolean functions attached via [`.run_if()`](SystemExt::run_if) that gate
//!   whether a system executes on a given timestep.
//! - **States**: [`CurrentState<S>`] / [`NextState<S>`] pairs for state-machine-driven
//!   simulations, with [`in_state()`] and [`in_stage()`] run conditions.
//!
//! # How execution order is decided
//!
//! Each timestep, every registered update system runs exactly once, in an
//! order computed by three layered rules — applied in this sequence:
//!
//! 1. **Sort by `(namespace, index)`.** Each system carries a [`StoredPhase`]
//!    whose `index` comes from its `ScheduleSet` variant's
//!    [`to_index()`](ScheduleSet::to_index) and whose `namespace` defaults to
//!    `0` (see the footgun below). Systems are ordered first by namespace,
//!    then by phase index.
//! 2. **Topologically sort within each `(namespace, index)` group** using
//!    Kahn's algorithm over the `.before()` / `.after()` constraints declared
//!    on those systems.
//! 3. **Registration-order tie-break.** Systems left unordered by steps 1–2
//!    (same `(namespace, index)`, no relative constraint) run in the order
//!    they were registered.
//!
//! ## Footgun #1: every `ScheduleSet` enum defaults to namespace 0
//!
//! Phase indices come from `to_index()`, which a derived `ScheduleSet`
//! numbers `0, 1, 2, …` *per enum*. The namespace, however, defaults to `0`
//! for **every** enum. So if two solvers each define their own phase enum —
//! say `FluidPhase::Force` (index 0) and `SolidPhase::Force` (index 0) —
//! both land at `(namespace 0, index 0)` and their systems **interleave**
//! instead of one solver running fully before the other. There is no error;
//! the ordering is just silently wrong.
//!
//! Three ways to separate them (pick one):
//!
//! - **Per-enum:** [`Scheduler::set_schedule_namespace::<E>(n)`](Scheduler::set_schedule_namespace)
//!   assigns namespace `n` to every system registered under enum `E`.
//! - **Bulk, ordered:** the [`chain_namespaces!`] macro assigns `0, 1, 2, …`
//!   to the enums you list, left to right.
//! - **Explicit tree:** build a [`Schedule`] and install it with
//!   [`Scheduler::set_schedule`] — the tree's walk order *is* the namespace
//!   assignment, and it additionally supports loops and branches.
//!
//! ```rust
//! # use grass_scheduler::prelude::*;
//! # use grass_scheduler::chain_namespaces;
//! # #[derive(Debug, Clone, Copy)] enum FluidPhase { Force }
//! # impl ScheduleSet for FluidPhase { fn to_index(&self) -> u32 { 0 } fn name(&self) -> &'static str { "Force" } }
//! # #[derive(Debug, Clone, Copy)] enum SolidPhase { Force }
//! # impl ScheduleSet for SolidPhase { fn to_index(&self) -> u32 { 0 } fn name(&self) -> &'static str { "Force" } }
//! # let mut scheduler = Scheduler::default();
//! // FluidPhase systems run entirely before SolidPhase systems:
//! chain_namespaces!(scheduler, FluidPhase, SolidPhase);
//! ```
//!
//! # Choosing a scheduling primitive
//!
//! | Primitive | Use when |
//! |-----------|----------|
//! | Plain phases (`add_update_system(sys, Phase::X)`) | One linear pass per step; ordering is just `(namespace, index)` + before/after. |
//! | [`SystemGroup`] (`.loop_while(cond, max)`) | A *block of systems inside one phase* must iterate (e.g. a fixed-point coupling sub-loop). |
//! | [`Schedule`] tree (`set_schedule`) | The whole step needs structure: ordered phases plus `loop_until` / `branch` over them. |
//!
//! [`SystemGroup::loop_while`] repeats **while** its condition stays `true`;
//! [`Schedule`]'s `loop_until` repeats **until** its condition becomes `true`
//! (the inverse). In both cases the condition is evaluated **after** each
//! iteration, so the loop body always runs at least once.
//!
//! # Diagnostics
//!
//! - `SIM_TRACE` env var — when set, prints each system name to stderr as it
//!   executes (via [`run_flat`](Scheduler::run_flat)).
//! - `SIM_SUPPRESS_WARNINGS` env var — when set, suppresses the schedule
//!   validation warnings normally printed at the end of
//!   [`organize_systems`](Scheduler::organize_systems).
//! - [`Scheduler::enable_schedule_print`] — writes a Graphviz `schedule.dot`
//!   after organizing (render with `dot -Tpng schedule.dot`).
//! - [`Scheduler::start`] prints a per-system timing table when the run loop
//!   ends.
//!
//! # Example
//!
//! ```rust
//! use grass_scheduler::prelude::*;
//!
//! #[derive(Debug, Clone, Copy)]
//! enum MySchedule { Force }
//!
//! impl ScheduleSet for MySchedule {
//!     fn to_index(&self) -> u32 { 0 }
//!     fn name(&self) -> &'static str { "Force" }
//! }
//!
//! struct Temperature(f64);
//!
//! fn compute_forces(temp: Res<Temperature>) {
//!     // Access temperature as &Temperature via Deref
//!     let _t = temp.0;
//! }
//!
//! let mut scheduler = Scheduler::default();
//! scheduler.add_resource(Temperature(300.0));
//! scheduler.add_update_system(compute_forces, MySchedule::Force);
//! ```

#![allow(clippy::too_many_arguments)]
#![warn(missing_docs)]
// ANCHOR: All
pub mod coherence;
pub mod iterative;
pub mod log;
pub mod manager;
pub mod param;
pub mod schedule;
pub mod snapshot;

pub use coherence::{CoherenceRegistry, MirrorBridge, MirrorState};
pub use iterative::{
    conjugate_gradient, picard_iteration, ConvergenceState, IterationError, IterationReport,
    LinearOperator,
};
pub use manager::*;
pub use param::*;
pub use schedule::{BranchBuilder, OnMax, Schedule, ScheduleBuilder, ScheduleNode};
pub use snapshot::{restore_resource, snapshot_resource, Snapshot};

// ─── Prelude ──────────────────────────────────────────────────────────────────

/// The `grass_scheduler` prelude.
///
/// Re-exports the scheduler types most systems need — [`Res`], [`ResMut`],
/// [`Local`], [`ScheduleSet`], the state/stage run conditions, and
/// [`Scheduler`] itself — so a solver can pull them in with a single
/// `use grass_scheduler::prelude::*;`.
pub mod prelude {
    pub use crate::{
        apply_state_transitions,
        check_stage_advance,
        // Simulation states
        conjugate_gradient,
        first_stage_only,
        in_stage,
        in_state,
        on_enter_stage,
        on_enter_state,
        picard_iteration,
        // Resource access kind (read/write) surfaced to coherence mediation
        AccessKind,
        // Host<->device coherence mediation
        CoherenceRegistry,
        ConditionalSystem,
        ConvergenceState,
        CurrentState,
        IntoScheduledSystem,
        // System label (function-handle or string ordering)
        IntoSystemLabel,
        IterationError,
        IterationReport,
        LinearOperator,
        Local,
        MirrorBridge,
        MirrorState,
        NextState,
        Res,
        ResMut,
        // Schedule phase trait (for user-definable schedule phases)
        ScheduleSet,
        // Core DI
        Scheduler,
        SchedulerManager,
        SchedulerState,
        // Stage enum trait
        StageName,
        StoredPhase,
        // System ordering
        SystemDescriptor,
        // Run conditions
        SystemExt,
        // System groups
        SystemGroup,
        SystemGroupInfo,
    };
    // Proc-macro derive (re-exported so users get it via the prelude).
    pub use grass_derive::ScheduleSet;
}

#[cfg(test)]
#[allow(
    clippy::field_reassign_with_default,
    clippy::manual_contains,
    dead_code
)]
mod tests {
    use super::*;
    use std::any::TypeId;
    use std::collections::HashMap;

    // Test schedule that mirrors the standard Verlet phase names (for warning tests)
    #[derive(Clone, Copy, Debug)]
    enum TestSchedule {
        InitialIntegration,
        Exchange,
        Force,
        PostForce,
        FinalIntegration,
        PostFinalIntegration,
    }

    impl ScheduleSet for TestSchedule {
        fn to_index(&self) -> u32 {
            match self {
                TestSchedule::InitialIntegration => 2,
                TestSchedule::Exchange => 5,
                TestSchedule::Force => 9,
                TestSchedule::PostForce => 10,
                TestSchedule::FinalIntegration => 12,
                TestSchedule::PostFinalIntegration => 13,
            }
        }
        fn name(&self) -> &'static str {
            match self {
                TestSchedule::InitialIntegration => "InitialIntegration",
                TestSchedule::Exchange => "Exchange",
                TestSchedule::Force => "Force",
                TestSchedule::PostForce => "PostForce",
                TestSchedule::FinalIntegration => "FinalIntegration",
                TestSchedule::PostFinalIntegration => "PostFinalIntegration",
            }
        }
    }

    struct MyResource(i32);

    fn system_requiring_resource(_res: Res<MyResource>) {}

    #[test]
    #[should_panic(expected = "Schedule validation errors")]
    fn missing_resource_panics_at_organize() {
        let mut scheduler = Scheduler::default();
        scheduler.add_update_system(system_requiring_resource, TestSchedule::Force);
        scheduler.organize_systems();
    }

    fn system_with_optional_resource(res: Option<Res<MyResource>>) {
        assert!(res.is_none());
    }

    #[test]
    fn optional_resource_works_when_missing() {
        let mut scheduler = Scheduler::default();
        scheduler.add_update_system(system_with_optional_resource, TestSchedule::Force);
        scheduler.organize_systems();
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
    }

    fn system_with_optional_present(res: Option<Res<MyResource>>) {
        assert!(res.is_some());
        assert_eq!(res.unwrap().0, 42);
    }

    // ── Phase 1: access metadata (read/write surfaced to the scheduler) ──────────

    #[test]
    fn access_kind_reported_for_res_resmut_and_local() {
        struct A(i32);
        struct B(i32);
        fn sys(_a: Res<A>, _b: ResMut<B>, _l: Local<u32>) {}
        let mut s = sys.into_system();
        let mut index = HashMap::new();
        index.insert(TypeId::of::<A>(), 7usize);
        index.insert(TypeId::of::<B>(), 3usize);
        let missing = s.prepare(&index);
        assert!(missing.is_empty());
        let acc = s.accesses();
        // Local contributes no tracked access; A reads slot 7, B writes slot 3.
        assert_eq!(acc.len(), 2);
        assert!(acc.contains(&(7, AccessKind::Read)));
        assert!(acc.contains(&(3, AccessKind::Write)));
    }

    #[test]
    fn optional_access_present_recorded_missing_skipped_and_reprepare_is_idempotent() {
        struct C(i32);
        fn sys(_c: Option<ResMut<C>>) {}
        let mut s = sys.into_system();
        // Missing optional: no validation error, and nothing recorded (idx == MAX).
        let empty = HashMap::new();
        assert!(s.prepare(&empty).is_empty());
        assert!(s.accesses().is_empty());
        // Present: recorded as a Write at its slot.
        let mut index = HashMap::new();
        index.insert(TypeId::of::<C>(), 1usize);
        s.prepare(&index);
        assert_eq!(s.accesses(), &[(1, AccessKind::Write)]);
        // Re-prepare must not duplicate (accesses cleared each time).
        s.prepare(&index);
        assert_eq!(s.accesses(), &[(1, AccessKind::Write)]);
    }

    #[test]
    fn same_phase_res_resmut_conflict_is_ordered_not_rejected() {
        #[derive(Default)]
        struct Shared(i32);
        #[derive(Default)]
        struct BorrowLog(Vec<i32>);

        fn write_shared(mut shared: ResMut<Shared>) {
            shared.0 = 7;
        }

        fn read_shared(shared: Res<Shared>, mut log: ResMut<BorrowLog>) {
            log.0.push(shared.0);
        }

        let mut scheduler = Scheduler::default();
        scheduler.add_resource(Shared::default());
        scheduler.add_resource(BorrowLog::default());
        scheduler.add_update_system(write_shared.label("write_shared"), TestSchedule::Force);
        scheduler.add_update_system(read_shared.after("write_shared"), TestSchedule::Force);

        scheduler.organize_systems();
        scheduler.run();

        let cell = scheduler
            .resource_cell(TypeId::of::<BorrowLog>())
            .expect("BorrowLog should be registered");
        let guard = cell.borrow();
        assert_eq!(guard.downcast_ref::<BorrowLog>().unwrap().0, vec![7]);
    }

    #[test]
    fn optional_resource_works_when_present() {
        let mut scheduler = Scheduler::default();
        scheduler.add_resource(MyResource(42));
        scheduler.add_update_system(system_with_optional_present, TestSchedule::Force);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
    }

    #[test]
    fn remove_update_system_by_label() {
        let mut scheduler = Scheduler::default();
        scheduler.add_update_system(
            system_requiring_resource.label("my_system"),
            TestSchedule::Force,
        );
        assert_eq!(scheduler.update_systems.len(), 1);
        scheduler.remove_update_system_by_label("my_system");
        assert_eq!(scheduler.update_systems.len(), 0);
    }

    fn dummy_system(_res: Res<MyResource>) {}

    #[test]
    fn remove_update_system_by_function() {
        let mut scheduler = Scheduler::default();
        scheduler.add_update_system(dummy_system, TestSchedule::Force);
        assert_eq!(scheduler.update_systems.len(), 1);
        scheduler.remove_update_system(dummy_system);
        assert_eq!(scheduler.update_systems.len(), 0);
    }

    // ─── requires_label tests ────────────────────────────────────────────────

    fn force_a() {}
    fn force_b() {}

    #[test]
    #[should_panic(expected = "requires label")]
    fn requires_label_panics_when_missing() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_update_system(
            force_b.label("force_b").requires_label("force_a"),
            TestSchedule::Force,
        );
        scheduler.organize_systems();
    }

    #[test]
    fn requires_label_passes_when_present() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_update_system(force_a.label("force_a"), TestSchedule::Force);
        scheduler.add_update_system(
            force_b.label("force_b").requires_label("force_a"),
            TestSchedule::Force,
        );
        scheduler.organize_systems();
    }

    fn always_true() -> bool {
        true
    }

    #[test]
    #[should_panic(expected = "requires label")]
    fn requires_label_works_with_run_if() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_update_system(
            force_b
                .label("force_b")
                .requires_label("force_a")
                .run_if(always_true),
            TestSchedule::Force,
        );
        scheduler.organize_systems();
    }

    #[test]
    fn requires_label_passes_with_run_if() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_update_system(force_a.label("force_a"), TestSchedule::Force);
        scheduler.add_update_system(
            force_b
                .label("force_b")
                .requires_label("force_a")
                .run_if(always_true),
            TestSchedule::Force,
        );
        scheduler.organize_systems();
    }

    // ─── warning callback tests ─────────────────────────────────────

    #[test]
    fn no_warnings_without_callback() {
        let mut scheduler = Scheduler::default();
        scheduler.add_update_system(force_a, TestSchedule::InitialIntegration);
        scheduler.organize_systems();
        let warnings = scheduler.schedule_warnings();
        assert!(
            warnings.is_empty(),
            "Expected no warnings without callback, got: {:?}",
            warnings
        );
    }

    #[test]
    fn warning_callback_receives_phase_names() {
        let mut scheduler = Scheduler::default();
        scheduler.set_warning_fn(|phases: &[&str]| {
            if phases.iter().any(|p| *p == "Force") {
                vec!["found Force".to_string()]
            } else {
                vec!["no Force".to_string()]
            }
        });
        scheduler.add_update_system(force_a, TestSchedule::Force);
        scheduler.organize_systems();
        let warnings = scheduler.schedule_warnings();
        assert_eq!(warnings, vec!["found Force"]);
    }

    // ─── function-handle-based ordering tests ─────────────────────────────────

    struct Counter(i32);

    fn sys_first(mut c: ResMut<Counter>) {
        assert_eq!(c.0, 0, "sys_first should run first");
        c.0 = 1;
    }

    fn sys_second(mut c: ResMut<Counter>) {
        assert_eq!(c.0, 1, "sys_second should run after sys_first");
        c.0 = 2;
    }

    #[test]
    fn fn_handle_after_ordering() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(Counter(0));
        scheduler.add_update_system(sys_second.after(sys_first), TestSchedule::Force);
        scheduler.add_update_system(sys_first, TestSchedule::Force);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let c = scheduler.get_resource_ref::<Counter>().unwrap();
        assert_eq!(c.0, 2);
    }

    #[test]
    fn fn_handle_before_ordering() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(Counter(0));
        scheduler.add_update_system(sys_second, TestSchedule::Force);
        scheduler.add_update_system(sys_first.before(sys_second), TestSchedule::Force);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let c = scheduler.get_resource_ref::<Counter>().unwrap();
        assert_eq!(c.0, 2);
    }

    #[test]
    fn fn_handle_requires_label_passes() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_update_system(force_a, TestSchedule::Force);
        scheduler.add_update_system(force_b.requires_label(force_a), TestSchedule::Force);
        scheduler.organize_systems();
    }

    #[test]
    #[should_panic(expected = "requires label")]
    fn fn_handle_requires_label_panics_when_missing() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_update_system(force_b.requires_label(force_a), TestSchedule::Force);
        scheduler.organize_systems();
    }

    #[test]
    fn string_labels_still_work() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(Counter(0));
        scheduler.add_update_system(sys_second.after("first"), TestSchedule::Force);
        scheduler.add_update_system(sys_first.label("first"), TestSchedule::Force);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let c = scheduler.get_resource_ref::<Counter>().unwrap();
        assert_eq!(c.0, 2);
    }

    // ─── Custom ScheduleSet tests ─────────────────────────────────────────

    #[derive(Clone, Copy, Debug)]
    enum CustomSchedule {
        PhaseA,
        PhaseB,
        PhaseC,
    }

    impl ScheduleSet for CustomSchedule {
        fn to_index(&self) -> u32 {
            match self {
                CustomSchedule::PhaseA => 0,
                CustomSchedule::PhaseB => 1,
                CustomSchedule::PhaseC => 2,
            }
        }
        fn name(&self) -> &'static str {
            match self {
                CustomSchedule::PhaseA => "PhaseA",
                CustomSchedule::PhaseB => "PhaseB",
                CustomSchedule::PhaseC => "PhaseC",
            }
        }
    }

    struct ExecutionLog(Vec<&'static str>);

    fn custom_phase_a(mut log: ResMut<ExecutionLog>) {
        log.0.push("A");
    }

    fn custom_phase_b(mut log: ResMut<ExecutionLog>) {
        log.0.push("B");
    }

    fn custom_phase_c(mut log: ResMut<ExecutionLog>) {
        log.0.push("C");
    }

    #[test]
    fn custom_schedule_phase_ordering() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(ExecutionLog(Vec::new()));
        // Register in reverse order to verify sorting
        scheduler.add_update_system(custom_phase_c, CustomSchedule::PhaseC);
        scheduler.add_update_system(custom_phase_a, CustomSchedule::PhaseA);
        scheduler.add_update_system(custom_phase_b, CustomSchedule::PhaseB);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let log = scheduler.get_resource_ref::<ExecutionLog>().unwrap();
        assert_eq!(log.0, vec!["A", "B", "C"]);
    }

    #[test]
    fn custom_schedule_no_warnings_without_callback() {
        let mut scheduler = Scheduler::default();
        scheduler.add_resource(ExecutionLog(Vec::new()));
        scheduler.add_update_system(custom_phase_a, CustomSchedule::PhaseA);
        scheduler.add_update_system(custom_phase_b, CustomSchedule::PhaseB);
        // No warnings should fire — no warning callback registered
        let warnings = scheduler.schedule_warnings();
        assert!(
            warnings.is_empty(),
            "Expected no warnings without callback, got: {:?}",
            warnings
        );
    }

    // ─── SystemGroup tests ──────────────────────────────────────────────────

    #[derive(Clone, Copy, Debug)]
    enum GroupPhase {
        First,
        Second,
        Third,
    }

    impl ScheduleSet for GroupPhase {
        fn to_index(&self) -> u32 {
            match self {
                GroupPhase::First => 0,
                GroupPhase::Second => 1,
                GroupPhase::Third => 2,
            }
        }
        fn name(&self) -> &'static str {
            match self {
                GroupPhase::First => "First",
                GroupPhase::Second => "Second",
                GroupPhase::Third => "Third",
            }
        }
    }

    fn group_sys_a(mut log: ResMut<ExecutionLog>) {
        log.0.push("A");
    }
    fn group_sys_b(mut log: ResMut<ExecutionLog>) {
        log.0.push("B");
    }
    fn group_sys_c(mut log: ResMut<ExecutionLog>) {
        log.0.push("C");
    }

    #[test]
    fn system_group_basic_execution_order() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(ExecutionLog(Vec::new()));
        // Register in reverse phase order to test sorting
        let group = SystemGroup::new("test_group")
            .add_system(group_sys_c, GroupPhase::Third)
            .add_system(group_sys_a, GroupPhase::First)
            .add_system(group_sys_b, GroupPhase::Second);
        scheduler.add_update_system(group, CustomSchedule::PhaseA);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let log = scheduler.get_resource_ref::<ExecutionLog>().unwrap();
        assert_eq!(log.0, vec!["A", "B", "C"]);
    }

    struct LoopCounter(usize);

    fn loop_condition(counter: Res<LoopCounter>) -> bool {
        counter.0 < 3
    }

    fn increment_counter(mut counter: ResMut<LoopCounter>, mut log: ResMut<ExecutionLog>) {
        counter.0 += 1;
        log.0.push("tick");
    }

    #[test]
    fn system_group_looping() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(ExecutionLog(Vec::new()));
        scheduler.add_resource(LoopCounter(0));
        let group = SystemGroup::new("loop_group")
            .add_system(increment_counter, GroupPhase::First)
            .loop_while(loop_condition, 10);
        scheduler.add_update_system(group, CustomSchedule::PhaseA);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let log = scheduler.get_resource_ref::<ExecutionLog>().unwrap();
        // Loop runs: iter 1 (counter 0→1, cond true), iter 2 (counter 1→2, cond true),
        // iter 3 (counter 2→3, cond false) → stops. 3 ticks total.
        assert_eq!(log.0, vec!["tick", "tick", "tick"]);
        let counter = scheduler.get_resource_ref::<LoopCounter>().unwrap();
        assert_eq!(counter.0, 3);
    }

    fn always_true_condition() -> bool {
        true
    }

    #[test]
    fn system_group_max_iterations_cap() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(ExecutionLog(Vec::new()));
        let group = SystemGroup::new("capped_group")
            .add_system(group_sys_a, GroupPhase::First)
            .loop_while(always_true_condition, 5);
        scheduler.add_update_system(group, CustomSchedule::PhaseA);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let log = scheduler.get_resource_ref::<ExecutionLog>().unwrap();
        assert_eq!(log.0, vec!["A", "A", "A", "A", "A"]);
    }

    #[test]
    fn system_group_no_loop_single_pass() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(ExecutionLog(Vec::new()));
        let group = SystemGroup::new("single_pass")
            .add_system(group_sys_a, GroupPhase::First)
            .add_system(group_sys_b, GroupPhase::Second);
        scheduler.add_update_system(group, CustomSchedule::PhaseA);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let log = scheduler.get_resource_ref::<ExecutionLog>().unwrap();
        assert_eq!(log.0, vec!["A", "B"]);
    }

    #[test]
    fn system_group_nested() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(ExecutionLog(Vec::new()));
        let inner = SystemGroup::new("inner")
            .add_system(group_sys_a, GroupPhase::First)
            .add_system(group_sys_b, GroupPhase::Second);
        let outer = SystemGroup::new("outer")
            .add_group(inner, GroupPhase::First)
            .add_system(group_sys_c, GroupPhase::Second);
        scheduler.add_update_system(outer, CustomSchedule::PhaseA);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let log = scheduler.get_resource_ref::<ExecutionLog>().unwrap();
        assert_eq!(log.0, vec!["A", "B", "C"]);
    }

    fn never_run_condition() -> bool {
        false
    }

    #[test]
    fn system_group_composability_run_if() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(ExecutionLog(Vec::new()));
        let group = SystemGroup::new("conditional_group")
            .add_system(group_sys_a, GroupPhase::First)
            .add_system(group_sys_b, GroupPhase::Second)
            .run_if(never_run_condition);
        scheduler.add_update_system(group, CustomSchedule::PhaseA);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let log = scheduler.get_resource_ref::<ExecutionLog>().unwrap();
        assert!(
            log.0.is_empty(),
            "Group should not run when condition is false"
        );
    }

    #[test]
    fn system_group_timing() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(ExecutionLog(Vec::new()));
        let group = SystemGroup::new("timed_group")
            .add_system(group_sys_a, GroupPhase::First)
            .add_system(group_sys_b, GroupPhase::Second);
        scheduler.add_update_system(group, CustomSchedule::PhaseA);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        // Access group_info through the stored system
        let info = scheduler.update_systems[0].0.system.group_info().unwrap();
        assert_eq!(info.inner_systems.len(), 2);
        assert_eq!(info.inner_timings.len(), 2);
        assert_eq!(info.total_iterations, 1);
        assert!(info.inner_timings.iter().all(|t| *t >= 0.0));
    }

    #[test]
    fn system_group_dot_output_contains_subgraph() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(ExecutionLog(Vec::new()));
        let group = SystemGroup::new("dot_group")
            .add_system(group_sys_a, GroupPhase::First)
            .add_system(group_sys_b, GroupPhase::Second)
            .loop_while(always_true_condition, 5);
        scheduler.add_update_system(group, CustomSchedule::PhaseA);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();

        let path = "/tmp/test_group_schedule.dot";
        scheduler.write_dot(path);
        let dot_content = std::fs::read_to_string(path).unwrap();
        assert!(
            dot_content.contains("subgraph cluster_group_"),
            "DOT should contain group subgraph"
        );
        assert!(
            dot_content.contains("loop while"),
            "DOT should contain loop back-edge"
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn system_group_label_and_before() {
        let mut scheduler = Scheduler::default();
        scheduler.suppress_warnings = true;
        scheduler.add_resource(ExecutionLog(Vec::new()));
        // Group with label, another system after it
        let group = SystemGroup::new("labeled_group")
            .add_system(group_sys_a, GroupPhase::First)
            .label("my_group");
        scheduler.add_update_system(group, CustomSchedule::PhaseA);
        scheduler.add_update_system(group_sys_b.after("my_group"), CustomSchedule::PhaseA);
        scheduler.add_scheduler_manager();
        scheduler.organize_systems();
        scheduler.run();
        let log = scheduler.get_resource_ref::<ExecutionLog>().unwrap();
        assert_eq!(log.0, vec!["A", "B"]);
    }
}
