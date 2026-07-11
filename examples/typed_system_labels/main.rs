//! Typed scheduler-label validation matrix.
//!
//! Run with `cargo run --example typed_system_labels`.

use grass_scheduler::prelude::*;
use std::any::TypeId;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Clone, Copy, Debug)]
enum Update {
    Run,
}

impl ScheduleSet for Update {
    fn to_index(&self) -> u32 {
        0
    }

    fn name(&self) -> &'static str {
        "Update"
    }
}

struct Integrate;
impl SystemLabel for Integrate {
    const NAME: &'static str = "integrate";
}
const INTEGRATE: SystemKey<Integrate> = SystemKey::new();

#[derive(Default)]
struct EventLog(Vec<&'static str>);

fn integrate(mut log: ResMut<EventLog>) {
    log.0.push("integrate");
}

fn process(mut log: ResMut<EventLog>) {
    log.0.push("process");
}

fn log(scheduler: &Scheduler) -> Vec<&'static str> {
    scheduler
        .resource_cell(TypeId::of::<EventLog>())
        .expect("example registered EventLog")
        .borrow()
        .downcast_ref::<EventLog>()
        .expect("EventLog has its concrete type")
        .0
        .clone()
}

fn expected(case: &str) -> String {
    let value: toml::Value = include_str!("config.toml")
        .parse()
        .expect("example config is valid TOML");
    value["checks"][case]
        .as_str()
        .expect("each case has a declarative expected outcome")
        .to_string()
}

fn typed_required_order() -> bool {
    let mut scheduler = Scheduler::default();
    scheduler.add_resource(EventLog::default());
    scheduler.add_update_system(process.requires(INTEGRATE), Update::Run);
    scheduler.add_update_system(integrate.label(INTEGRATE), Update::Run);
    scheduler.organize_systems();
    scheduler.run();
    log(&scheduler) == ["integrate", "process"]
}

fn required_missing_target() -> bool {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut scheduler = Scheduler::default();
        scheduler.add_resource(EventLog::default());
        scheduler.add_update_system(process.requires(INTEGRATE), Update::Run);
        scheduler.organize_systems();
    }));
    result
        .err()
        .and_then(|panic| panic.downcast::<String>().ok())
        .is_some_and(|message| {
            message.contains("requires label \"integrate\"")
                && message.contains("use `.after(...)` for optional ordering")
        })
}

fn optional_missing_target() -> bool {
    let mut scheduler = Scheduler::default();
    scheduler.add_resource(EventLog::default());
    scheduler.add_update_system(process.after(INTEGRATE), Update::Run);
    scheduler.organize_systems();
    scheduler.run();
    log(&scheduler) == ["process"]
}

fn legacy_string_compatibility() -> bool {
    let mut scheduler = Scheduler::default();
    scheduler.add_resource(EventLog::default());
    scheduler.add_update_system(process.after("legacy_integrate"), Update::Run);
    scheduler.add_update_system(integrate.label("legacy_integrate"), Update::Run);
    scheduler.organize_systems();
    scheduler.run();
    log(&scheduler) == ["integrate", "process"]
}

fn main() {
    let cases: [(&str, fn() -> bool); 4] = [
        ("typed_required_order", typed_required_order),
        ("required_missing_target", required_missing_target),
        ("optional_missing_target", optional_missing_target),
        ("legacy_string_compatibility", legacy_string_compatibility),
    ];
    let mut passed = 0;
    for (case, check) in cases {
        let wanted = expected(case);
        let actual = if check() { "PASS" } else { "FAIL" };
        let ok = actual == wanted;
        println!("{case}: expected={wanted} actual={actual} pass={ok}");
        passed += usize::from(ok);
    }
    assert_eq!(
        passed, 4,
        "every scheduler-label case must match its expectation"
    );
    println!("typed_system_labels: passed={passed}/4");
}
