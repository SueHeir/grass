//! A coupled run saved mid-way and restored into freshly built Apps ends
//! bit-identical to an uninterrupted run, coupler state included.

use grass_app::prelude::*;
use grass_io::{restore_coupled, save_coupled, CheckpointAppExt, Config, RunPlugin, RunState};
use grass_multi::{tick_subapp, MultiAppExt, SubApps};
use grass_scheduler::prelude::*;
use serde::{Deserialize, Serialize};

/// A chaotic map, so any state not restored exactly changes the answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct State {
    x: f64,
    forcing: f64,
}

/// What the coupler last handed over: lives in the parent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
struct Coupler {
    exchanges: u64,
    last: f64,
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    Step,
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Parent {
    Tick,
    Exchange,
}

fn step(mut s: ResMut<State>) {
    s.x = (3.97 * s.x * (1.0 - s.x) + 1e-3 * s.forcing).clamp(0.0, 1.0);
}

fn code(x0: f64) -> App {
    let mut app = App::new();
    app.add_resource(Config::from_str("[[run]]\nsteps = 100\n"));
    app.add_plugins(RunPlugin);
    app.add_resource(State { x: x0, forcing: 0.0 });
    app.checkpoint::<State>("state");
    app.add_update_system(step, Phase::Step);
    app
}

/// Mutable access to a resource from `&App`, through its cell.
fn with<T: 'static, R>(app: &App, f: impl FnOnce(&mut T) -> R) -> R {
    let cell = app.resource_cell(std::any::TypeId::of::<T>()).unwrap();
    let mut any = cell.borrow_mut();
    f(any.downcast_mut::<T>().unwrap())
}

fn exchange(subs: Res<SubApps>, mut coupler: ResMut<Coupler>) {
    let x = |name: &str| with(subs.find(name).unwrap().local_app().unwrap(), |s: &mut State| s.x);
    let (a, b) = (x("a"), x("b"));
    // The coupler feeds back what it saw one exchange ago: state that only
    // the parent holds.
    let last = coupler.last;
    coupler.exchanges += 1;
    coupler.last = a - b;
    for (name, f) in [("a", last), ("b", -last)] {
        with(subs.find(name).unwrap().local_app().unwrap(), |s: &mut State| s.forcing = f);
    }
}

fn parent() -> App {
    let mut p = App::new();
    p.add_subapp("a", code(0.31));
    p.add_subapp("b", code(0.72));
    p.add_resource(Coupler::default());
    p.checkpoint::<Coupler>("coupler");
    p.add_update_system(tick_subapp("a", 1), Parent::Tick);
    p.add_update_system(tick_subapp("b", 1), Parent::Tick);
    p.add_update_system(exchange, Parent::Exchange);
    p.prepare();
    with(&p, |subs: &mut SubApps| {
        subs.prepare("a");
        subs.prepare("b");
    });
    p
}

fn snapshot(p: &App) -> (State, State, Coupler, usize) {
    let subs = p.get_resource_ref::<SubApps>().unwrap();
    let get = |n: &str| {
        let f = subs.find(n).unwrap();
        let app = f.local_app().unwrap();
        let s = app.get_resource_ref::<State>().unwrap().clone();
        let cycles = app.get_resource_ref::<RunState>().unwrap().total_cycle;
        (s, cycles)
    };
    let (a, ca) = get("a");
    let (b, _) = get("b");
    let c = p.get_resource_ref::<Coupler>().unwrap().clone();
    (a, b, c, ca)
}

#[test]
fn coupled_restart_matches_an_uninterrupted_run() {
    let mut straight = parent();
    for _ in 0..40 {
        straight.run();
    }

    let dir = std::env::temp_dir().join(format!("grass_coupled_restart_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut first = parent();
    for _ in 0..17 {
        first.run();
    }
    save_coupled(&first, &dir).unwrap();
    drop(first);

    let mut resumed = parent();
    restore_coupled(&resumed, &dir).unwrap();
    for _ in 17..40 {
        resumed.run();
    }

    assert_eq!(snapshot(&resumed), snapshot(&straight));
    assert_eq!(snapshot(&resumed).3, 40, "step counters restored too");
    std::fs::remove_dir_all(dir).ok();
}
