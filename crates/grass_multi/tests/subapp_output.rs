//! Each sub-App's terminal output can be labeled, silenced or sent to a file
//! by the parent, without the code knowing it is coupled.

use grass_app::prelude::*;
use grass_app::{say, Output};
use grass_multi::{AppPhysics, SubApps};
use grass_scheduler::prelude::*;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Phase {
    Report,
}

fn report(mut step: Local<u32>) {
    *step += 1;
    say!("step {}", *step);
}

fn code() -> App {
    let mut app = App::new();
    app.add_setup_system(|| say!("banner"), ScheduleSetupSet::Setup);
    app.add_update_system(report, Phase::Report);
    app
}

#[test]
fn each_subapp_writes_to_its_own_output() {
    let dir = std::env::temp_dir().join(format!("grass_subapp_output_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut subs = SubApps::new();
    subs.register(Box::new(AppPhysics::new("dem", code())));
    subs.register(Box::new(AppPhysics::new("cfd", code())));
    subs.register(Box::new(AppPhysics::new("quiet", code())));
    subs.set_output("dem", Output::file(dir.join("dem.log")).unwrap());
    subs.set_output("cfd", Output::file(dir.join("cfd.log")).unwrap());
    subs.set_output("quiet", Output::quiet());

    for _ in 0..2 {
        subs.tick("dem");
        subs.tick("cfd");
        subs.tick("quiet");
    }
    subs.tick("cfd");

    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
    assert_eq!(read("dem.log"), "banner\nstep 1\nstep 2\n");
    assert_eq!(read("cfd.log"), "banner\nstep 1\nstep 2\nstep 3\n");
    let _ = std::fs::remove_dir_all(dir);
}
