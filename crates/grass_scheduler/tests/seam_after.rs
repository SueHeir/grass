//! `add_seam_after` opens a code's timestep at one phase without rebuilding
//! its schedule, and a hand-built schedule that leaves phases out never runs
//! them at the wrong point.

use grass_scheduler::prelude::*;
use grass_scheduler::{Schedule, ScheduleProgress};

#[derive(Default)]
struct Log(Vec<&'static str>);

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Particle {
    PreForce,
    Force,
}

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Output {
    Write,
}

fn pre_force(mut log: ResMut<Log>) {
    log.0.push("pre_force");
}
fn force(mut log: ResMut<Log>) {
    log.0.push("force");
}
fn write(mut log: ResMut<Log>) {
    log.0.push("write");
}

fn scheduler() -> Scheduler {
    let mut s = Scheduler::default();
    s.add_resource(Log::default());
    s.add_update_system(pre_force, Particle::PreForce);
    s.add_update_system(force, Particle::Force);
    s.add_update_system(write, Output::Write);
    s.set_schedule_namespace::<Output>(1);
    s
}

fn take_log(s: &Scheduler) -> Vec<&'static str> {
    let cell = s.resource_cell(std::any::TypeId::of::<Log>()).unwrap();
    let mut any = cell.borrow_mut();
    std::mem::take(&mut any.downcast_mut::<Log>().unwrap().0)
}

#[test]
fn seam_after_keeps_every_phase_in_order() {
    let mut s = scheduler();
    s.add_seam_after(Particle::PreForce, "pre_force");
    s.organize_systems();

    match s.resume() {
        ScheduleProgress::Yielded(seam) => assert_eq!(seam.id(), "pre_force"),
        ScheduleProgress::Complete => panic!("expected the seam"),
    }
    assert_eq!(take_log(&s), vec!["pre_force"]);
    assert!(matches!(s.resume(), ScheduleProgress::Complete));
    assert_eq!(take_log(&s), vec!["force", "write"]);
}

#[test]
#[should_panic(expected = "no update system is registered under phase `Force`")]
fn seam_after_an_unused_phase_is_rejected() {
    let mut s = Scheduler::default();
    s.add_resource(Log::default());
    s.add_update_system(pre_force, Particle::PreForce);
    s.add_seam_after(Particle::Force, "after_force");
    s.organize_systems();
}

#[test]
fn phases_left_out_of_a_schedule_never_run() {
    let mut s = scheduler();
    // `Output::Write` is left out. Before, its systems kept namespace 1 and
    // ran inside whichever tree phase was given namespace 1 (`Force` here).
    s.set_schedule(
        Schedule::builder()
            .then_variant(Particle::PreForce)
            .then_variant(Particle::Force)
            .build(),
    );
    s.organize_systems();
    s.run();
    assert_eq!(take_log(&s), vec!["pre_force", "force"]);
}
