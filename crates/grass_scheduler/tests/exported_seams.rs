use grass_scheduler::{ResMut, Schedule, ScheduleProgress, ScheduleSet, Scheduler};

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

#[test]
#[should_panic(expected = "inside a Loop")]
fn rejects_seams_inside_loop_with_actionable_diagnostic() {
    let mut s = Scheduler::default();
    s.set_schedule(
        Schedule::builder()
            .loop_until(
                || true,
                1,
                grass_scheduler::OnMax::Panic,
                |body| body.then_variant(Phase::A).export_seam("inside-loop"),
            )
            .build(),
    );
}
