//! Cross-namespace [`Multi`] system param integration test.
//!
//! Two minimal sub-Apps registered on a parent App via
//! [`MultiAppExt::add_subapp`]. The parent's scheduler drives sub-App
//! ticks via [`tick_subapp`] and runs one cross-namespace [`Multi`]-
//! using system (the `mirror` system) that reads from the producer
//! sub-App and writes into the consumer sub-App.
//!
//! Validates that:
//!   - `Multi` works as a `SystemParam` on a normal grass system,
//!   - `MultiRef<T>` / `MultiMut<T>` deref correctly to `&T` / `&mut T`,
//!   - simultaneous read("producer") + write("consumer") in one statement
//!     is borrow-safe (different namespaces, different RefCells),
//!   - `tick_subapp(name, n)` advances each sub-App as expected,
//!   - the parent App's run loop terminates when a sub-App signals `is_done`.

use grass_app::prelude::*;
use grass_multi::{tick_subapp, Multi, MultiAppExt, SubApps};
use grass_scheduler::prelude::*;

// ─── Shared resource ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct Counter(pub u32);

#[derive(Debug, Clone, Copy)]
struct StopAfter(pub u32);

// ─── Producer sub-App: increments Counter each step, ends after N ───────────

#[derive(Debug, Clone, Copy)]
enum ProducerSchedule {
    Tick,
}
impl ScheduleSet for ProducerSchedule {
    fn to_index(&self) -> u32 {
        0
    }
    fn name(&self) -> &'static str {
        "Tick"
    }
}

fn producer_tick(mut counter: ResMut<Counter>) {
    counter.0 += 1;
}

fn producer_check_done(
    counter: Res<Counter>,
    stop: Res<StopAfter>,
    mut sm: ResMut<SchedulerManager>,
) {
    if counter.0 >= stop.0 {
        sm.state = SchedulerState::End;
    }
}

fn build_producer(stop_after: u32) -> App {
    let mut app = App::new();
    app.add_resource(Counter(0));
    app.add_resource(StopAfter(stop_after));
    app.add_update_system(producer_tick, ProducerSchedule::Tick);
    app.add_update_system(producer_check_done, ProducerSchedule::Tick);
    app
}

// ─── Consumer sub-App: holds a Counter the parent will overwrite ────────────

#[derive(Debug, Clone, Copy)]
enum ConsumerSchedule {
    Tick,
}
impl ScheduleSet for ConsumerSchedule {
    fn to_index(&self) -> u32 {
        0
    }
    fn name(&self) -> &'static str {
        "Tick"
    }
}

fn consumer_noop() {}

fn build_consumer() -> App {
    let mut app = App::new();
    app.add_resource(Counter(999)); // sentinel; mirror system overwrites
    app.add_update_system(consumer_noop, ConsumerSchedule::Tick);
    app
}

// ─── Parent App: mirror system + tick_subapp drivers ────────────────────────

#[derive(Debug, Clone, Copy)]
enum ParentSchedule {
    /// Tick producer (its Counter increments).
    TickProducer,
    /// Bridge: read producer.Counter → write consumer.Counter.
    Mirror,
    /// Tick consumer (no-op in this test, but proves the path).
    TickConsumer,
    /// Stop the parent's loop when any sub-App is done.
    CheckDone,
}
impl ScheduleSet for ParentSchedule {
    fn to_index(&self) -> u32 {
        match self {
            Self::TickProducer => 0,
            Self::Mirror => 1,
            Self::TickConsumer => 2,
            Self::CheckDone => 3,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::TickProducer => "TickProducer",
            Self::Mirror => "Mirror",
            Self::TickConsumer => "TickConsumer",
            Self::CheckDone => "CheckDone",
        }
    }
}

/// The whole point of Phase 0: a normal grass system reading from one
/// namespace and writing to another via `Multi`.
fn mirror(world: Multi) {
    let src = world.expect_read::<Counter>("producer").0;
    world.expect_write::<Counter>("consumer").0 = src;
}

fn parent_check_done(subs: Res<SubApps>, mut sm: ResMut<SchedulerManager>) {
    if subs.any_done() {
        sm.state = SchedulerState::End;
    }
}

// ─── Test ───────────────────────────────────────────────────────────────────

#[test]
fn multi_systemparam_mirrors_counter_across_subapps() {
    const STOP_AFTER: u32 = 5;

    let producer = build_producer(STOP_AFTER);
    let consumer = build_consumer();

    let mut parent = App::new();
    parent.add_subapp("producer", producer);
    parent.add_subapp("consumer", consumer);

    parent.add_update_system(tick_subapp("producer", 1), ParentSchedule::TickProducer);
    parent.add_update_system(mirror, ParentSchedule::Mirror);
    parent.add_update_system(tick_subapp("consumer", 1), ParentSchedule::TickConsumer);
    parent.add_update_system(parent_check_done, ParentSchedule::CheckDone);

    parent.start();

    // After the loop ends, both sub-Apps' Counters should match — the mirror
    // system copied producer's value into consumer's slot every iter.
    let subs = parent
        .get_resource_ref::<SubApps>()
        .expect("SubApps resource should exist on the parent");

    let prod_counter = subs
        .find("producer")
        .expect("producer namespace registered")
        .resource_cell(std::any::TypeId::of::<Counter>())
        .expect("producer has Counter")
        .borrow();
    let prod_val = prod_counter
        .downcast_ref::<Counter>()
        .expect("Counter type")
        .0;

    let cons_counter = subs
        .find("consumer")
        .expect("consumer namespace registered")
        .resource_cell(std::any::TypeId::of::<Counter>())
        .expect("consumer has Counter")
        .borrow();
    let cons_val = cons_counter
        .downcast_ref::<Counter>()
        .expect("Counter type")
        .0;

    assert_eq!(
        prod_val, STOP_AFTER,
        "producer ran exactly STOP_AFTER ticks"
    );
    assert_eq!(
        cons_val, STOP_AFTER,
        "consumer mirrored producer's final value"
    );
    assert_ne!(cons_val, 999, "sentinel was overwritten");
}

#[test]
fn multi_read_and_write_simultaneously_on_different_namespaces() {
    // Sanity check: a single statement that reads from one namespace AND
    // writes to another must not deadlock (different RefCells, no conflict).
    fn double_into_consumer(world: Multi) {
        let src = world.expect_read::<Counter>("producer").0;
        let mut dst = world.expect_write::<Counter>("consumer");
        dst.0 = src * 2;
        // Read still alive here; both borrows coexist on different cells.
        assert_eq!(world.expect_read::<Counter>("producer").0, src);
    }

    let producer = build_producer(2);
    let consumer = build_consumer();

    let mut parent = App::new();
    parent.add_subapp("producer", producer);
    parent.add_subapp("consumer", consumer);
    parent.add_update_system(tick_subapp("producer", 1), ParentSchedule::TickProducer);
    parent.add_update_system(double_into_consumer, ParentSchedule::Mirror);
    parent.add_update_system(parent_check_done, ParentSchedule::CheckDone);

    parent.start();

    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let cons = subs
        .find("consumer")
        .unwrap()
        .resource_cell(std::any::TypeId::of::<Counter>())
        .unwrap()
        .borrow();
    let cons_val = cons.downcast_ref::<Counter>().unwrap().0;

    // Producer ran twice → counter = 2; doubled into consumer → 4.
    assert_eq!(cons_val, 4);
}
