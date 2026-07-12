use grass_scheduler::{Res, ResMut, Schedule, ScheduleProgress, ScheduleSet, Scheduler};

#[derive(Debug, Clone, Copy)]
enum Phase {
    A,
    B,
    C,
}
impl ScheduleSet for Phase {
    fn to_index(&self) -> u32 {
        match self {
            Self::A => 0,
            Self::B => 1,
            Self::C => 2,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
        }
    }
}

#[derive(Default)]
struct Log(Vec<&'static str>);
fn a(mut log: ResMut<Log>) {
    log.0.push("a");
}
fn b(mut log: ResMut<Log>) {
    log.0.push("b");
}
fn c(mut log: ResMut<Log>) {
    log.0.push("c");
}
fn scheduler() -> Scheduler {
    let mut s = Scheduler::default();
    s.add_resource(Log::default());
    s.add_update_system(a, Phase::A);
    s.add_update_system(b, Phase::B);
    s.add_update_system(c, Phase::C);
    s.set_schedule(
        Schedule::builder()
            .then_variant(Phase::A)
            .export_seam("after-a")
            .then_variant(Phase::B)
            .export_seam("after-b")
            .then_variant(Phase::C)
            .build(),
    );
    s.organize_systems();
    s
}

fn log(s: &Scheduler) -> Vec<&'static str> {
    s.get_resource_ref::<Log>().unwrap().0.clone()
}

#[test]
fn yields_and_resumes_without_duplicate_or_skipped_systems() {
    let mut s = scheduler();
    assert_eq!(
        s.resume(),
        ScheduleProgress::Yielded(grass_scheduler::ScheduleSeam::new("after-a"))
    );
    assert_eq!(log(&s), ["a"]);
    assert_eq!(
        s.resume(),
        ScheduleProgress::Yielded(grass_scheduler::ScheduleSeam::new("after-b"))
    );
    assert_eq!(log(&s), ["a", "b"]);
    assert_eq!(s.resume(), ScheduleProgress::Complete);
    assert_eq!(log(&s), ["a", "b", "c"]);
    assert_eq!(s.timing_steps(), 1);
}

#[test]
fn cursor_resets_for_repeated_timesteps() {
    let mut s = scheduler();
    for _ in 0..2 {
        assert!(matches!(s.resume(), ScheduleProgress::Yielded(_)));
        assert!(matches!(s.resume(), ScheduleProgress::Yielded(_)));
        assert_eq!(s.resume(), ScheduleProgress::Complete);
    }
    assert_eq!(log(&s), ["a", "b", "c", "a", "b", "c"]);
    assert_eq!(s.timing_steps(), 2);
}

#[test]
#[should_panic(expected = "cannot cross exported schedule seams")]
fn ordinary_run_rejects_exported_seams() {
    let mut s = scheduler();
    s.run();
}

#[test]
fn consecutive_seams_are_each_reported_without_crossing() {
    let mut s = Scheduler::default();
    s.set_schedule(
        Schedule::builder()
            .export_seam("one")
            .export_seam("two")
            .build(),
    );
    assert_eq!(
        s.resume(),
        ScheduleProgress::Yielded(grass_scheduler::ScheduleSeam::new("one"))
    );
    assert_eq!(
        s.resume(),
        ScheduleProgress::Yielded(grass_scheduler::ScheduleSeam::new("two"))
    );
    assert_eq!(s.resume(), ScheduleProgress::Complete);
}

#[test]
#[should_panic(expected = "occurs more than once")]
fn duplicate_stable_ids_are_rejected() {
    let mut s = Scheduler::default();
    s.set_schedule(
        Schedule::builder()
            .export_seam("same")
            .export_seam("same")
            .build(),
    );
}

#[test]
#[should_panic(expected = "while a timestep is suspended")]
fn replacing_schedule_while_suspended_is_rejected() {
    let mut s = scheduler();
    assert!(matches!(s.resume(), ScheduleProgress::Yielded(_)));
    s.set_schedule(Schedule::builder().then_variant(Phase::A).build());
}

#[derive(Default)]
struct Count(usize);
fn count(mut n: ResMut<Count>) {
    n.0 += 1;
}
fn at_three(n: Res<Count>) -> bool {
    n.0 == 3
}

#[test]
fn seam_in_loop_preserves_iteration_cursor_and_condition_timing() {
    let mut s = Scheduler::default();
    s.add_resource(Log::default());
    s.add_resource(Count::default());
    s.add_update_system(a, Phase::A);
    s.add_update_system(count, Phase::B);
    s.add_update_system(c, Phase::C);
    s.set_schedule(
        Schedule::builder()
            .loop_until(at_three, 3, grass_scheduler::OnMax::Panic, |body| {
                body.then_variant(Phase::A)
                    .export_seam("inside-loop")
                    .then_variant(Phase::B)
                    .then_variant(Phase::C)
            })
            .build(),
    );
    s.organize_systems();
    for iteration in 1..=3 {
        assert_eq!(
            s.resume(),
            ScheduleProgress::Yielded(grass_scheduler::ScheduleSeam::new("inside-loop"))
        );
        assert_eq!(log(&s).len(), iteration * 2 - 1);
        assert_eq!(s.timing_steps(), 0);
    }
    assert_eq!(s.resume(), ScheduleProgress::Complete);
    assert_eq!(log(&s), ["a", "c", "a", "c", "a", "c"]);
    assert_eq!(s.get_resource_ref::<Count>().unwrap().0, 3);
    assert_eq!(s.timing_steps(), 1);
}

#[derive(Default)]
struct Choice(bool);
fn choose_true(choice: Res<Choice>) -> bool {
    choice.0
}
fn choose_false(choice: Res<Choice>) -> bool {
    !choice.0
}
fn flip(mut choice: ResMut<Choice>, mut log: ResMut<Log>) {
    choice.0 = false;
    log.0.push("a");
}

#[test]
fn branch_selection_is_latched_across_a_yield() {
    let mut s = Scheduler::default();
    s.add_resource(Log::default());
    s.add_resource(Choice(true));
    s.add_update_system(flip, Phase::A);
    s.add_update_system(b, Phase::B);
    s.add_update_system(c, Phase::C);
    s.set_schedule(
        Schedule::builder()
            .branch(|branch| {
                branch
                    .arm(choose_true, |body| {
                        body.then_variant(Phase::A)
                            .export_seam("chosen")
                            .then_variant(Phase::B)
                    })
                    .arm(choose_false, |body| body.then_variant(Phase::C))
            })
            .build(),
    );
    s.organize_systems();
    assert!(matches!(s.resume(), ScheduleProgress::Yielded(_)));
    assert_eq!(s.resume(), ScheduleProgress::Complete);
    assert_eq!(log(&s), ["a", "b"]);
}

#[test]
fn rollback_fragment_can_yield_and_runs_exactly_once() {
    let mut s = Scheduler::default();
    s.add_resource(Log::default());
    s.add_update_system(a, Phase::A);
    s.add_update_system(b, Phase::B);
    s.add_update_system(c, Phase::C);
    s.set_schedule(
        Schedule::builder()
            .loop_with_rollback(
                || false,
                1,
                |body| body.then_variant(Phase::A),
                |rollback| {
                    rollback
                        .then_variant(Phase::B)
                        .export_seam("rollback")
                        .then_variant(Phase::C)
                },
            )
            .build(),
    );
    s.organize_systems();
    assert_eq!(
        s.resume(),
        ScheduleProgress::Yielded(grass_scheduler::ScheduleSeam::new("rollback"))
    );
    assert_eq!(log(&s), ["a", "b"]);
    assert_eq!(s.resume(), ScheduleProgress::Complete);
    assert_eq!(log(&s), ["a", "b", "c"]);
}

#[test]
fn zero_iteration_loop_runs_rollback_but_not_body() {
    let mut s = Scheduler::default();
    s.add_resource(Log::default());
    s.add_update_system(a, Phase::A);
    s.add_update_system(b, Phase::B);
    s.set_schedule(
        Schedule::builder()
            .loop_with_rollback(
                || false,
                0,
                |body| body.then_variant(Phase::A),
                |rollback| rollback.export_seam("zero.rollback").then_variant(Phase::B),
            )
            .build(),
    );
    s.organize_systems();
    assert!(matches!(s.resume(), ScheduleProgress::Yielded(_)));
    assert!(log(&s).is_empty());
    assert_eq!(s.resume(), ScheduleProgress::Complete);
    assert_eq!(log(&s), ["b"]);
}

#[test]
fn no_match_branch_clears_its_frame_and_continues() {
    let mut s = Scheduler::default();
    s.add_resource(Log::default());
    s.add_resource(Choice(false));
    s.add_update_system(a, Phase::A);
    s.add_update_system(c, Phase::C);
    s.set_schedule(
        Schedule::builder()
            .branch(|branch| {
                branch.arm(choose_true, |body| {
                    body.export_seam("unreachable").then_variant(Phase::A)
                })
            })
            .then_variant(Phase::C)
            .build(),
    );
    s.organize_systems();
    assert_eq!(s.resume(), ScheduleProgress::Complete);
    assert_eq!(log(&s), ["c"]);
}

#[test]
fn consecutive_nested_seams_and_structural_mutation_are_fail_closed() {
    let mut s = Scheduler::default();
    s.set_schedule(
        Schedule::builder()
            .loop_until(
                || true,
                1,
                grass_scheduler::OnMax::Panic,
                |body| body.export_seam("nested.one").export_seam("nested.two"),
            )
            .build(),
    );
    assert_eq!(
        s.resume(),
        ScheduleProgress::Yielded(grass_scheduler::ScheduleSeam::new("nested.one"))
    );
    assert_eq!(
        s.resume(),
        ScheduleProgress::Yielded(grass_scheduler::ScheduleSeam::new("nested.two"))
    );
    assert_eq!(s.resume(), ScheduleProgress::Complete);
}

#[test]
#[should_panic(expected = "while a timestep is suspended")]
fn replacing_schedule_after_nested_yield_is_rejected() {
    let mut s = Scheduler::default();
    s.set_schedule(
        Schedule::builder()
            .loop_until(
                || true,
                1,
                grass_scheduler::OnMax::Panic,
                |body| body.export_seam("nested"),
            )
            .build(),
    );
    assert!(matches!(s.resume(), ScheduleProgress::Yielded(_)));
    s.set_schedule(Schedule::builder().build());
}
