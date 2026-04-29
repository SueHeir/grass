//! Phase 1 lowering tests for `Schedule { Phase / Sequence / Loop }`.
//!
//! Each test registers a small set of phase enums, builds a Schedule with
//! `Schedule::builder()`, calls `set_schedule()` on the scheduler, then
//! `run()` once and inspects an `EventLog` resource that systems push into.
//! That gives us a deterministic execution trace to compare against.

use grass_scheduler::prelude::*;
use grass_scheduler::{restore_resource, OnMax, Schedule, Snapshot};

// ─── Test phase enums (each represents one Phase node in the Schedule) ─────

#[derive(Debug, Clone, Copy)]
enum A {
    Run,
}
impl ScheduleSet for A {
    fn to_index(&self) -> u32 {
        0
    }
    fn name(&self) -> &'static str {
        "A"
    }
}

#[derive(Debug, Clone, Copy)]
enum B {
    Run,
}
impl ScheduleSet for B {
    fn to_index(&self) -> u32 {
        0
    }
    fn name(&self) -> &'static str {
        "B"
    }
}

#[derive(Debug, Clone, Copy)]
enum C {
    Run,
}
impl ScheduleSet for C {
    fn to_index(&self) -> u32 {
        0
    }
    fn name(&self) -> &'static str {
        "C"
    }
}

#[derive(Debug, Clone, Copy)]
enum BodyPhase {
    Run,
}
impl ScheduleSet for BodyPhase {
    fn to_index(&self) -> u32 {
        0
    }
    fn name(&self) -> &'static str {
        "Body"
    }
}

// ─── Resources ──────────────────────────────────────────────────────────────

#[derive(Default)]
struct EventLog(Vec<&'static str>);

#[derive(Default)]
struct Counter(u32);

// ─── Helpers ────────────────────────────────────────────────────────────────

fn push_a(mut log: ResMut<EventLog>) {
    log.0.push("A");
}
fn push_b(mut log: ResMut<EventLog>) {
    log.0.push("B");
}
fn push_c(mut log: ResMut<EventLog>) {
    log.0.push("C");
}
fn body_tick(mut log: ResMut<EventLog>, mut counter: ResMut<Counter>) {
    counter.0 += 1;
    log.0.push("body");
}
fn converged_at_3(counter: Res<Counter>) -> bool {
    counter.0 >= 3
}
fn never_converges(_counter: Res<Counter>) -> bool {
    false
}

fn drain_log(scheduler: &Scheduler) -> Vec<&'static str> {
    let cell = scheduler
        .resource_cell(std::any::TypeId::of::<EventLog>())
        .expect("EventLog should be registered");
    let g = cell.borrow();
    g.downcast_ref::<EventLog>().expect("type").0.clone()
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn sequence_runs_phases_in_tree_order() {
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());

    // Register systems in REVERSE order of intended execution to prove the
    // Schedule controls ordering, not registration order.
    s.add_update_system(push_c, C::Run);
    s.add_update_system(push_b, B::Run);
    s.add_update_system(push_a, A::Run);

    let sched = Schedule::builder()
        .then::<A>()
        .then::<B>()
        .then::<C>()
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();

    assert_eq!(drain_log(&s), vec!["A", "B", "C"]);
}

#[test]
fn loop_re_runs_body_until_converged() {
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Counter::default());

    s.add_update_system(push_a, A::Run);
    s.add_update_system(body_tick, BodyPhase::Run);
    s.add_update_system(push_c, C::Run);

    // A → Loop(Body × until counter≥3) → C. Counter starts at 0;
    // each body iter increments it. We expect 3 body runs.
    let sched = Schedule::builder()
        .then::<A>()
        .loop_until(converged_at_3, 10, OnMax::Panic, |body| {
            body.then::<BodyPhase>()
        })
        .then::<C>()
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();

    assert_eq!(
        drain_log(&s),
        vec!["A", "body", "body", "body", "C"],
        "loop should run body 3 times before until() flips, then continue to C"
    );
}

#[test]
#[should_panic(expected = "did not converge")]
fn loop_panics_on_max_iters_when_on_max_panic() {
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Counter::default());

    s.add_update_system(body_tick, BodyPhase::Run);

    let sched = Schedule::builder()
        .loop_until(never_converges, 3, OnMax::Panic, |body| {
            body.then::<BodyPhase>()
        })
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run(); // expected to panic
}

#[test]
fn loop_continues_on_max_when_accept_unconverged() {
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Counter::default());

    s.add_update_system(body_tick, BodyPhase::Run);
    s.add_update_system(push_c, C::Run);

    let sched = Schedule::builder()
        .loop_until(never_converges, 3, OnMax::AcceptUnconverged, |body| {
            body.then::<BodyPhase>()
        })
        .then::<C>()
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();

    assert_eq!(
        drain_log(&s),
        vec!["body", "body", "body", "C"],
        "loop should hit max, accept, and run C"
    );
}

#[test]
fn nested_sequence_inside_loop_body() {
    // Body is itself a Sequence: A → BodyPhase. Loop max=2, never converges,
    // accept unconverged. Total: A,body,A,body,C.
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Counter::default());

    s.add_update_system(push_a, A::Run);
    s.add_update_system(body_tick, BodyPhase::Run);
    s.add_update_system(push_c, C::Run);

    let sched = Schedule::builder()
        .loop_until(never_converges, 2, OnMax::AcceptUnconverged, |body| {
            body.then::<A>().then::<BodyPhase>()
        })
        .then::<C>()
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();

    assert_eq!(drain_log(&s), vec!["A", "body", "A", "body", "C"]);
}

#[test]
fn no_schedule_falls_back_to_flat_run() {
    // When no Schedule is set, run() must still execute all systems in
    // namespace+index order (the pre-Phase-1 default behaviour).
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());

    s.add_update_system(push_a, A::Run);
    s.add_update_system(push_b, B::Run);
    s.add_update_system(push_c, C::Run);

    s.organize_systems();
    s.run();

    let log = drain_log(&s);
    assert_eq!(log.len(), 3, "all three systems should have run");
    // Without a Schedule and with all phases at namespace 0, A/B/C share the
    // same sort key — registration order breaks ties, so we get A, B, C.
    assert_eq!(log, vec!["A", "B", "C"]);
}

#[test]
fn set_schedule_after_systems_rewrites_namespaces() {
    // Order check: if we register systems first, then call set_schedule, the
    // namespace fields on registered systems must be retroactively rewritten
    // to match the Schedule's tree-walk order.
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());

    s.add_update_system(push_a, A::Run);
    s.add_update_system(push_b, B::Run);

    // Schedule reverses the order: B then A.
    let sched = Schedule::builder().then::<B>().then::<A>().build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();

    assert_eq!(drain_log(&s), vec!["B", "A"]);
}

// ─── Phase 1.5 — Branch ─────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Mode(pub u32); // 0 = Pre, 1 = Run, 2 = Post — keeps tests free of state-machine plumbing

#[test]
fn branch_runs_first_matching_arm() {
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Mode(1));

    s.add_update_system(push_a, A::Run);
    s.add_update_system(push_b, B::Run);
    s.add_update_system(push_c, C::Run);

    let sched = Schedule::builder()
        .branch(|b| {
            b.arm(|m: Res<Mode>| m.0 == 0, |arm| arm.then::<A>())
                .arm(|m: Res<Mode>| m.0 == 1, |arm| arm.then::<B>())
                .arm(|m: Res<Mode>| m.0 == 2, |arm| arm.then::<C>())
        })
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();
    assert_eq!(drain_log(&s), vec!["B"]);
}

#[test]
fn branch_with_no_match_is_a_noop() {
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Mode(99)); // matches no arm

    s.add_update_system(push_a, A::Run);
    s.add_update_system(push_c, C::Run);

    let sched = Schedule::builder()
        .then::<A>()
        .branch(|b| {
            b.arm(|m: Res<Mode>| m.0 == 0, |arm| arm.then::<B>())
                .arm(|m: Res<Mode>| m.0 == 1, |arm| arm.then::<B>())
        })
        .then::<C>()
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();
    assert_eq!(drain_log(&s), vec!["A", "C"], "branch is silent no-op");
}

#[test]
fn branch_first_match_wins_when_multiple_could_match() {
    // Both arms would match (|| true × 2). First wins.
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());

    s.add_update_system(push_a, A::Run);
    s.add_update_system(push_b, B::Run);

    let sched = Schedule::builder()
        .branch(|b| {
            b.arm(|| true, |arm| arm.then::<A>())
                .arm(|| true, |arm| arm.then::<B>())
        })
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();
    assert_eq!(drain_log(&s), vec!["A"]);
}

#[test]
fn branch_inside_loop_body() {
    // Loop body branches on parity of its iter counter; total trace
    // depends on how many iters the body runs.
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Counter::default());

    s.add_update_system(body_tick, BodyPhase::Run);
    s.add_update_system(push_a, A::Run);
    s.add_update_system(push_b, B::Run);
    s.add_update_system(push_c, C::Run);

    let sched = Schedule::builder()
        .loop_until(converged_at_3, 10, OnMax::Panic, |body| {
            body.then::<BodyPhase>().branch(|b| {
                b.arm(
                    |c: Res<Counter>| c.0.is_multiple_of(2),
                    |arm| arm.then::<A>(),
                )
                .arm(|| true, |arm| arm.then::<B>())
            })
        })
        .then::<C>()
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();
    // body_tick → counter 1 (odd → B); body_tick → 2 (even → A); body_tick → 3 (converged before branch)
    // Wait — the until check happens AFTER each body iter, so:
    //   iter 1: body=1, branch on counter=1 (odd) → B; until: 1>=3? no
    //   iter 2: body=2, branch on counter=2 (even) → A; until: 2>=3? no
    //   iter 3: body=3, branch on counter=3 (odd) → B; until: 3>=3? yes, return
    //   then C
    assert_eq!(
        drain_log(&s),
        vec!["body", "B", "body", "A", "body", "B", "C"]
    );
}

// ─── Phase 2.5 — OnMax::Rollback ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Tentative(pub u32);

#[derive(Debug, Clone, Copy)]
enum RollbackPhase {
    Restore,
    LogRollback,
}
impl ScheduleSet for RollbackPhase {
    fn to_index(&self) -> u32 {
        match self {
            Self::Restore => 0,
            Self::LogRollback => 1,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::Restore => "Restore",
            Self::LogRollback => "LogRollback",
        }
    }
}

fn mutate_tentative(mut t: ResMut<Tentative>) {
    t.0 += 1;
}
fn save_tentative(t: Res<Tentative>, mut snap: ResMut<Snapshot<Tentative>>) {
    snap.saved = Some(*t);
}
fn log_rollback(mut log: ResMut<EventLog>) {
    log.0.push("rolled-back");
}

#[test]
fn rollback_runs_only_when_loop_hits_max() {
    // Loop max=2, never converges → rollback fires.
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Counter::default());
    s.add_resource(Tentative::default());
    s.add_resource(Snapshot::<Tentative>::default());

    // Save once before loop; mutate inside; restore on rollback.
    s.add_update_system(save_tentative, A::Run);
    s.add_update_system(body_tick, BodyPhase::Run);
    s.add_update_system(mutate_tentative, BodyPhase::Run);
    s.add_update_system(restore_resource::<Tentative>(), RollbackPhase::Restore);
    s.add_update_system(log_rollback, RollbackPhase::LogRollback);
    s.add_update_system(push_c, C::Run);

    let sched = Schedule::builder()
        .then::<A>()
        .loop_with_rollback(
            never_converges,
            2,
            |body| body.then::<BodyPhase>(),
            |rb| rb.then::<RollbackPhase>(),
        )
        .then::<C>()
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();

    assert_eq!(
        drain_log(&s),
        vec!["body", "body", "rolled-back", "C"],
        "loop runs body twice, hits max, fires rollback, continues to C"
    );

    // Tentative was incremented twice (=2); rollback restored to 0.
    let cell = s
        .resource_cell(std::any::TypeId::of::<Tentative>())
        .unwrap();
    let t = cell.borrow();
    let t = t.downcast_ref::<Tentative>().unwrap();
    assert_eq!(*t, Tentative(0), "restore brought tentative back to 0");
}

#[test]
fn rollback_does_not_run_when_loop_converges_early() {
    // Loop max=10, converges_at_3 → body runs 3×, until flips, rollback NOT fired.
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Counter::default());

    s.add_update_system(body_tick, BodyPhase::Run);
    s.add_update_system(log_rollback, RollbackPhase::LogRollback);
    s.add_update_system(push_c, C::Run);

    let sched = Schedule::builder()
        .loop_with_rollback(
            converged_at_3,
            10,
            |body| body.then::<BodyPhase>(),
            |rb| rb.then::<RollbackPhase>(),
        )
        .then::<C>()
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();

    assert_eq!(
        drain_log(&s),
        vec!["body", "body", "body", "C"],
        "rollback should NOT fire when loop converges before max"
    );
}

#[test]
fn nested_loops_express_retry_with_rollback() {
    // Outer loop = "tries"; inner = "iterations within a try".
    // Each outer iter: snapshot → inner loop (may rollback) → check progress.
    // Progress is faked: a "success counter" only flips after 2 outer tries.
    //
    // This exercises the documented composition: retry-with-shrunken-dt is
    // a Loop-of-Loops where the inner has on_max = Rollback.

    #[derive(Debug, Clone, Copy)]
    enum OuterPhase {
        TryCount,
    }
    impl ScheduleSet for OuterPhase {
        fn to_index(&self) -> u32 {
            0
        }
        fn name(&self) -> &'static str {
            "TryCount"
        }
    }
    #[derive(Default)]
    struct OuterTries(pub u32);

    fn bump_outer(mut t: ResMut<OuterTries>) {
        t.0 += 1;
    }
    fn outer_done(t: Res<OuterTries>) -> bool {
        t.0 >= 2
    }

    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Counter::default());
    s.add_resource(OuterTries::default());

    s.add_update_system(body_tick, BodyPhase::Run);
    s.add_update_system(log_rollback, RollbackPhase::LogRollback);
    s.add_update_system(bump_outer, OuterPhase::TryCount);
    s.add_update_system(push_c, C::Run);

    let sched = Schedule::builder()
        .loop_until(outer_done, 5, OnMax::Panic, |outer| {
            outer
                .loop_with_rollback(
                    never_converges,
                    2,
                    |body| body.then::<BodyPhase>(),
                    |rb| rb.then::<RollbackPhase>(),
                )
                .then::<OuterPhase>()
        })
        .then::<C>()
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();

    // Outer iter 1: body, body (2× inner), rollback, bump_outer (→1)
    // Outer iter 2: body, body, rollback, bump_outer (→2 → outer_done = true → break)
    // Then C.
    assert_eq!(
        drain_log(&s),
        vec![
            "body",
            "body",
            "rolled-back",
            "body",
            "body",
            "rolled-back",
            "C"
        ]
    );
}

#[test]
fn rollback_subtree_phases_get_assigned_namespaces() {
    // Regression test for the lowering walk: phases inside an OnMax::Rollback
    // subtree must be visited by assign_namespaces / collect_phase_assignments
    // / prepare_conditions. If a Rollback phase wasn't given its namespace,
    // its systems would never run (still at namespace 0 with the body).
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Counter::default());

    s.add_update_system(body_tick, BodyPhase::Run);
    s.add_update_system(log_rollback, RollbackPhase::LogRollback);

    let sched = Schedule::builder()
        .loop_with_rollback(
            never_converges,
            1,
            |body| body.then::<BodyPhase>(),
            |rb| rb.then::<RollbackPhase>(),
        )
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();
    assert_eq!(drain_log(&s), vec!["body", "rolled-back"]);
}

// ─── Phase 7 — unit-struct derive + per-variant dispatch ───────────────────

/// Unit-struct ScheduleSet via derive.
#[derive(Debug, Clone, Copy, ScheduleSet)]
struct UnitMarker;

#[test]
fn unit_struct_derive_works_as_a_schedule_set() {
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());

    fn push_marker(mut log: ResMut<EventLog>) {
        log.0.push("marker");
    }
    s.add_update_system(push_marker, UnitMarker);
    s.organize_systems();
    s.run();
    assert_eq!(drain_log(&s), vec!["marker"]);
}

/// Multi-variant enum used to drive per-variant dispatch tests.
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Stage {
    First,
    Second,
    Third,
}

fn push_first(mut log: ResMut<EventLog>) {
    log.0.push("first");
}
fn push_second(mut log: ResMut<EventLog>) {
    log.0.push("second");
}
fn push_third(mut log: ResMut<EventLog>) {
    log.0.push("third");
}

#[test]
fn then_variant_dispatches_one_variant_only() {
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());

    s.add_update_system(push_first, Stage::First);
    s.add_update_system(push_second, Stage::Second);
    s.add_update_system(push_third, Stage::Third);

    // Reverse-order Schedule via per-variant dispatch — proves each
    // variant lands at its own namespace independently of declaration
    // order.
    let sched = Schedule::builder()
        .then_variant(Stage::Third)
        .then_variant(Stage::First)
        .then_variant(Stage::Second)
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();
    assert_eq!(drain_log(&s), vec!["third", "first", "second"]);
}

#[test]
fn then_variant_can_split_around_a_loop() {
    // The motivating case: one Stage enum at outer positions,
    // multi-variant LoopBody inside a Loop. Demonstrates the
    // single-enum-as-schedule-shape pattern primitives examples want.

    #[derive(Debug, Clone, Copy, ScheduleSet)]
    enum BodyStep {
        S1,
        S2,
    }

    fn push_b1(mut log: ResMut<EventLog>) {
        log.0.push("b1");
    }
    fn push_b2(mut log: ResMut<EventLog>) {
        log.0.push("b2");
    }

    fn converged_after_2_iters(mut count: ResMut<Counter>) -> bool {
        count.0 += 1;
        count.0 >= 2
    }

    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_resource(Counter::default());

    s.add_update_system(push_first, Stage::First);
    s.add_update_system(push_b1, BodyStep::S1);
    s.add_update_system(push_b2, BodyStep::S2);
    s.add_update_system(push_third, Stage::Third);

    let sched = Schedule::builder()
        .then_variant(Stage::First)
        .loop_until(converged_after_2_iters, 5, OnMax::Panic, |body| {
            body.then::<BodyStep>()
        })
        .then_variant(Stage::Third)
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();
    // First → (b1,b2) loop iter 1 → counter=1 not converged → (b1,b2) iter 2 → converged → Third
    assert_eq!(
        drain_log(&s),
        vec!["first", "b1", "b2", "b1", "b2", "third"]
    );
}

#[test]
#[should_panic(expected = "phase type appears both as whole-enum")]
fn mixing_whole_enum_and_per_variant_for_same_type_panics() {
    let mut s = Scheduler::default();
    s.add_resource(EventLog::default());
    s.add_update_system(push_first, Stage::First);
    s.add_update_system(push_second, Stage::Second);

    let sched = Schedule::builder()
        .then::<Stage>() // whole-enum
        .then_variant(Stage::Second) // per-variant on the SAME type
        .build();
    s.set_schedule(sched);
}
