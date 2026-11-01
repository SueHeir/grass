//! Regression tests for the scheduler lifecycle, `Local<T>` slots, and the
//! access sets that drive host↔device coherence.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;

use grass_scheduler::prelude::*;

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    Main,
}

// ─── Local<T> ────────────────────────────────────────────────────────────────

fn two_locals_same_type(mut a: Local<u32>, b: Local<u32>) {
    *a += *b;
}

#[test]
#[should_panic(expected = "Local<u32> is declared more than once")]
fn duplicate_local_type_is_rejected_at_organize() {
    // Both parameters would otherwise be `&mut` to one slot (aliasing UB).
    let mut scheduler = Scheduler::default();
    scheduler.add_update_system(two_locals_same_type, Phase::Main);
    scheduler.organize_systems();
}

fn two_locals_distinct_types(mut a: Local<u32>, mut b: Local<u64>) {
    *a += 1;
    *b += 2;
}

#[test]
fn distinct_local_types_are_accepted() {
    let mut scheduler = Scheduler::default();
    scheduler.add_update_system(two_locals_distinct_types, Phase::Main);
    scheduler.organize_systems();
    scheduler.run();
}

fn cond_two_locals(a: Local<bool>, b: Local<bool>) -> bool {
    *a || *b
}

#[test]
#[should_panic(expected = "Local<bool> is declared more than once")]
fn duplicate_local_type_in_condition_is_rejected() {
    let mut scheduler = Scheduler::default();
    scheduler.add_update_system(
        two_locals_distinct_types.run_if(cond_two_locals),
        Phase::Main,
    );
    scheduler.organize_systems();
}

// ─── Lifecycle ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct Steps(u32);

fn end_during_setup(mut sm: ResMut<SchedulerManager>) {
    sm.state = SchedulerState::End;
}

fn count_to_five(mut steps: ResMut<Steps>, mut sm: ResMut<SchedulerManager>) {
    steps.0 += 1;
    if steps.0 >= 5 {
        sm.state = SchedulerState::End;
    }
}

#[test]
fn end_requested_during_setup_skips_the_update_loop() {
    let mut scheduler = Scheduler::default();
    scheduler.add_resource(Steps::default());
    scheduler.add_setup_system(end_during_setup, Phase::Main);
    scheduler.add_update_system(count_to_five, Phase::Main);
    scheduler.start();
    assert_eq!(scheduler.get_resource_ref::<Steps>().unwrap().0, 0);
}

#[test]
fn ordinary_setup_still_enters_run() {
    let mut scheduler = Scheduler::default();
    scheduler.add_resource(Steps::default());
    scheduler.add_update_system(count_to_five, Phase::Main);
    scheduler.start();
    assert_eq!(scheduler.get_resource_ref::<Steps>().unwrap().0, 5);
}

// ─── Coherence through SystemGroup and run conditions ───────────────────────

struct HostCopy(i32);
struct DeviceCopy(i32);
#[derive(Default)]
struct Seen(i32);

struct Bridge {
    host: usize,
    device: usize,
}

impl MirrorBridge for Bridge {
    fn resolve(&mut self, index: &HashMap<TypeId, usize>) {
        self.host = index[&TypeId::of::<HostCopy>()];
        self.device = index[&TypeId::of::<DeviceCopy>()];
    }
    fn download(&self, resources: &[RefCell<Box<dyn Any>>]) {
        let v = resources[self.device]
            .borrow()
            .downcast_ref::<DeviceCopy>()
            .unwrap()
            .0;
        resources[self.host]
            .borrow_mut()
            .downcast_mut::<HostCopy>()
            .unwrap()
            .0 = v;
    }
}

fn scheduler_with_device_dirty_mirror() -> Scheduler {
    let mut scheduler = Scheduler::default();
    scheduler.add_resource(HostCopy(0));
    scheduler.add_resource(DeviceCopy(99));
    scheduler.add_resource(Seen::default());
    let mut registry = CoherenceRegistry::new();
    registry.suppress_warnings = true;
    registry.register(
        TypeId::of::<HostCopy>(),
        Box::new(Bridge { host: 0, device: 0 }),
    );
    registry.mark_device_dirty(TypeId::of::<HostCopy>());
    scheduler.add_resource(registry);
    scheduler
}

fn read_host(host: Res<HostCopy>, mut seen: ResMut<Seen>) {
    seen.0 = host.0;
}

fn write_host(mut host: ResMut<HostCopy>) {
    host.0 += 1;
}

fn mirror_state(scheduler: &Scheduler) -> MirrorState {
    scheduler
        .get_resource_ref::<CoherenceRegistry>()
        .unwrap()
        .state(TypeId::of::<HostCopy>())
        .unwrap()
}

#[test]
fn system_group_reader_syncs_a_device_dirty_mirror() {
    let mut scheduler = scheduler_with_device_dirty_mirror();
    scheduler.add_update_system(
        SystemGroup::new("group").add_system(read_host, Phase::Main),
        Phase::Main,
    );
    scheduler.organize_systems();
    scheduler.run();
    assert_eq!(scheduler.get_resource_ref::<Seen>().unwrap().0, 99);
}

#[test]
fn system_group_writer_marks_the_mirror_host_dirty() {
    let mut scheduler = scheduler_with_device_dirty_mirror();
    scheduler.add_update_system(
        SystemGroup::new("group").add_system(write_host, Phase::Main),
        Phase::Main,
    );
    scheduler.organize_systems();
    scheduler.run();
    assert_eq!(scheduler.get_resource_ref::<HostCopy>().unwrap().0, 100);
    assert_eq!(mirror_state(&scheduler), MirrorState::HostDirty);
}

fn host_is_positive(host: Res<HostCopy>) -> bool {
    host.0 > 0
}

fn mark_ran(mut seen: ResMut<Seen>) {
    seen.0 = -1;
}

#[test]
fn run_condition_reads_a_synced_mirror() {
    // The gated system does not touch HostCopy itself; only its condition does.
    let mut scheduler = scheduler_with_device_dirty_mirror();
    scheduler.add_update_system(mark_ran.run_if(host_is_positive), Phase::Main);
    scheduler.organize_systems();
    scheduler.run();
    assert_eq!(scheduler.get_resource_ref::<Seen>().unwrap().0, -1);
}
