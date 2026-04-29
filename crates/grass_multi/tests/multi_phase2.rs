//! Phase 2 integration tests: `Namespace` typed markers + `tick_n_times`
//! + cross-namespace `Snapshot<T>` via [`snapshot_subapp_resource`] /
//!   [`restore_subapp_resource`].

use grass_app::prelude::*;
// `Namespace` is brought into scope by the `namespace!` macro expansion
// (the macro path `$crate::Namespace` references the trait). Importing the
// macro is enough; the trait import would be unused at the call site.
use grass_multi::{
    namespace, restore_subapp_resource, snapshot_subapp_resource, tick_n_times, Multi, MultiAppExt,
    SubApps,
};
use grass_scheduler::prelude::*;
use grass_scheduler::Snapshot;

// ─── Namespace markers (declared via the namespace! macro) ──────────────────

namespace!(pub ProducerNs = "producer");
namespace!(pub ConsumerNs = "consumer");

// ─── Resources ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
struct Counter(pub u32);

#[derive(Debug, Clone, Copy)]
struct StopAfter(pub u32);

// ─── Producer: Counter increments each step, ends after N ──────────────────

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

fn producer_tick(mut c: ResMut<Counter>) {
    c.0 += 1;
}

fn producer_check_done(c: Res<Counter>, stop: Res<StopAfter>, mut sm: ResMut<SchedulerManager>) {
    if c.0 >= stop.0 {
        sm.state = SchedulerState::End;
    }
}

fn build_producer(stop: u32) -> App {
    let mut app = App::new();
    app.add_resource(Counter(0));
    app.add_resource(StopAfter(stop));
    app.add_resource(Snapshot::<Counter>::default());
    app.add_update_system(producer_tick, ProducerSchedule::Tick);
    app.add_update_system(producer_check_done, ProducerSchedule::Tick);
    app
}

// ─── Consumer: passive Counter the parent will write into ──────────────────

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
    app.add_resource(Counter(999));
    app.add_resource(Snapshot::<Counter>::default());
    app.add_update_system(consumer_noop, ConsumerSchedule::Tick);
    app
}

// ─── Parent schedule ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum ParentSchedule {
    TickProducer,
    Mirror,
    TickConsumer,
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

fn mirror(world: Multi) {
    let src = world.expect_read::<Counter>("producer").0;
    world.expect_write::<Counter>("consumer").0 = src;
}

fn parent_check_done(subs: Res<SubApps>, mut sm: ResMut<SchedulerManager>) {
    if subs.any_done() {
        sm.state = SchedulerState::End;
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn typed_add_subapp_and_tick_n_times_round_trip() {
    const STOP: u32 = 4;

    let producer = build_producer(STOP);
    let consumer = build_consumer();

    let mut parent = App::new();
    parent.add_subapp_typed::<ProducerNs>(producer);
    parent.add_subapp_typed::<ConsumerNs>(consumer);

    parent.add_update_system(tick_n_times::<ProducerNs>(1), ParentSchedule::TickProducer);
    parent.add_update_system(mirror, ParentSchedule::Mirror);
    parent.add_update_system(tick_n_times::<ConsumerNs>(1), ParentSchedule::TickConsumer);
    parent.add_update_system(parent_check_done, ParentSchedule::CheckDone);

    parent.start();

    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let cons_cell = subs
        .find("consumer")
        .unwrap()
        .resource_cell(std::any::TypeId::of::<Counter>())
        .unwrap()
        .borrow();
    let cons_val = cons_cell.downcast_ref::<Counter>().unwrap().0;
    assert_eq!(cons_val, STOP);
}

#[test]
fn tick_n_times_advances_n_per_outer_iter() {
    // Producer ticks 3× per outer iter; runs to STOP=6 → outer iters = 2.
    // Consumer mirrors after each outer iter, so at end consumer counter = 6.
    const STOP: u32 = 6;

    let producer = build_producer(STOP);
    let consumer = build_consumer();

    let mut parent = App::new();
    parent.add_subapp_typed::<ProducerNs>(producer);
    parent.add_subapp_typed::<ConsumerNs>(consumer);

    parent.add_update_system(tick_n_times::<ProducerNs>(3), ParentSchedule::TickProducer);
    parent.add_update_system(mirror, ParentSchedule::Mirror);
    parent.add_update_system(parent_check_done, ParentSchedule::CheckDone);

    parent.start();

    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let cons_cell = subs
        .find("consumer")
        .unwrap()
        .resource_cell(std::any::TypeId::of::<Counter>())
        .unwrap()
        .borrow();
    let cons_val = cons_cell.downcast_ref::<Counter>().unwrap().0;
    assert_eq!(cons_val, STOP);
}

#[test]
fn snapshot_subapp_resource_round_trips_a_subapp_resource() {
    // Goal: prove save → mutate → restore on a sub-App resource works
    // exactly like the local case. We hijack the producer's tick to make
    // it modify Counter, sandwich it between a save and a restore on the
    // parent, and assert Counter is unchanged after the iter.

    #[derive(Debug, Clone, Copy)]
    enum Phase {
        Save,
        TickProducer,
        Restore,
        CheckDone,
    }
    impl ScheduleSet for Phase {
        fn to_index(&self) -> u32 {
            match self {
                Self::Save => 0,
                Self::TickProducer => 1,
                Self::Restore => 2,
                Self::CheckDone => 3,
            }
        }
        fn name(&self) -> &'static str {
            match self {
                Self::Save => "Save",
                Self::TickProducer => "TickProducer",
                Self::Restore => "Restore",
                Self::CheckDone => "CheckDone",
            }
        }
    }

    let producer = build_producer(100); // big STOP so producer never ends; we control via parent.
    let mut parent = App::new();
    parent.add_subapp_typed::<ProducerNs>(producer);

    parent.add_update_system(snapshot_subapp_resource::<Counter>("producer"), Phase::Save);
    parent.add_update_system(tick_n_times::<ProducerNs>(1), Phase::TickProducer);
    parent.add_update_system(
        restore_subapp_resource::<Counter>("producer"),
        Phase::Restore,
    );
    parent.add_update_system(
        |mut sm: ResMut<SchedulerManager>| sm.state = SchedulerState::End,
        Phase::CheckDone,
    );

    parent.start();

    // Producer's Counter must be back to its pre-tick value (0). Without
    // the snapshot/restore it would be 1.
    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let prod_cell = subs
        .find("producer")
        .unwrap()
        .resource_cell(std::any::TypeId::of::<Counter>())
        .unwrap()
        .borrow();
    let prod_val = prod_cell.downcast_ref::<Counter>().unwrap().0;
    assert_eq!(
        prod_val, 0,
        "snapshot+restore should undo the tick's effect"
    );
}

#[test]
fn restore_subapp_resource_is_noop_when_nothing_saved() {
    // Run only the restore step (no save). Counter should reflect the
    // tick's effect — restore must not zero it or panic.
    #[derive(Debug, Clone, Copy)]
    enum Phase {
        TickProducer,
        Restore,
        CheckDone,
    }
    impl ScheduleSet for Phase {
        fn to_index(&self) -> u32 {
            match self {
                Self::TickProducer => 0,
                Self::Restore => 1,
                Self::CheckDone => 2,
            }
        }
        fn name(&self) -> &'static str {
            match self {
                Self::TickProducer => "TickProducer",
                Self::Restore => "Restore",
                Self::CheckDone => "CheckDone",
            }
        }
    }

    let producer = build_producer(100);
    let mut parent = App::new();
    parent.add_subapp_typed::<ProducerNs>(producer);

    parent.add_update_system(tick_n_times::<ProducerNs>(1), Phase::TickProducer);
    parent.add_update_system(
        restore_subapp_resource::<Counter>("producer"),
        Phase::Restore,
    );
    parent.add_update_system(
        |mut sm: ResMut<SchedulerManager>| sm.state = SchedulerState::End,
        Phase::CheckDone,
    );

    parent.start();

    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let prod_cell = subs
        .find("producer")
        .unwrap()
        .resource_cell(std::any::TypeId::of::<Counter>())
        .unwrap()
        .borrow();
    let prod_val = prod_cell.downcast_ref::<Counter>().unwrap().0;
    assert_eq!(prod_val, 1, "without a saved value, restore is a noop");
}
