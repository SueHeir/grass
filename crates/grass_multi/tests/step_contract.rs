//! A coupler can drive any sub-App by whole physical steps, and ticking a
//! finished sub-App is harmless.

use grass_app::prelude::*;
use grass_multi::{advance_subapp, tick_subapp, MultiAppExt, SubApps};
use grass_scheduler::prelude::*;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Phase {
    Step,
}

/// Counts `run()` calls and finished physical steps.
#[derive(Default)]
struct Log {
    runs: u32,
    steps: u32,
}

/// One `run()` is one of `stages` substeps; ends after `max_steps` steps.
struct Stages {
    stages: u32,
    max_steps: u32,
}

fn substep(
    mut log: ResMut<Log>,
    cfg: Res<Stages>,
    mut boundary: ResMut<StepBoundary>,
    mut sm: ResMut<SchedulerManager>,
) {
    log.runs += 1;
    boundary.completed_full_step = log.runs.is_multiple_of(cfg.stages);
    if boundary.completed_full_step {
        log.steps += 1;
        if log.steps >= cfg.max_steps {
            sm.state = SchedulerState::End;
        }
    }
}

fn code(stages: u32, max_steps: u32) -> App {
    let mut app = App::new();
    app.add_resource(Log::default());
    app.add_resource(Stages { stages, max_steps });
    app.add_resource(StepBoundary::default());
    app.add_update_system(substep, Phase::Step);
    app
}

fn log(subs: &SubApps, name: &str) -> (u32, u32) {
    let physics = subs.find(name).expect("registered");
    let cell = physics
        .resource_cell(std::any::TypeId::of::<Log>())
        .expect("Log resource");
    let any = cell.borrow();
    let app_log = any.downcast_ref::<Log>().unwrap();
    (app_log.runs, app_log.steps)
}

#[test]
fn advance_step_runs_every_substep_of_one_physical_step() {
    let mut subs = SubApps::new();
    subs.register(Box::new(grass_multi::AppPhysics::new("rk3", code(3, 10))));
    subs.register(Box::new(grass_multi::AppPhysics::new(
        "verlet",
        code(1, 10),
    )));

    for _ in 0..4 {
        subs.advance_step("rk3");
        subs.advance_step("verlet");
    }
    // Both codes are at the same physical time, whatever their substeps.
    assert_eq!(log(&subs, "rk3"), (12, 4));
    assert_eq!(log(&subs, "verlet"), (4, 4));
}

#[test]
fn ticking_a_finished_subapp_does_nothing() {
    let mut subs = SubApps::new();
    subs.register(Box::new(grass_multi::AppPhysics::new("short", code(1, 2))));
    for _ in 0..5 {
        subs.tick("short");
    }
    assert!(subs.any_done());
    assert_eq!(log(&subs, "short"), (2, 2));
    subs.advance_step("short");
    assert_eq!(log(&subs, "short"), (2, 2));
}

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Parent {
    Tick,
}

#[test]
fn parent_advances_mixed_codes_in_lockstep() {
    let mut parent = App::new();
    parent.add_subapp("rk3", code(3, 3));
    parent.add_subapp("verlet", code(1, 100));
    parent.add_update_system(advance_subapp("rk3", 1), Parent::Tick);
    parent.add_update_system(advance_subapp("verlet", 1), Parent::Tick);
    parent.prepare();
    for _ in 0..3 {
        parent.run();
    }
    // A fourth outer step: rk3 is done and is left alone.
    parent.run();
    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    assert_eq!(log(&subs, "rk3"), (9, 3));
    assert_eq!(log(&subs, "verlet"), (4, 4));
    let _ = tick_subapp; // both constructors stay available
}
