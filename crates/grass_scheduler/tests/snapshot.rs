//! Tests for the Phase 2 `Snapshot<T>` primitive (local, no Multi).

use grass_scheduler::prelude::*;
use grass_scheduler::{restore_resource, snapshot_resource, Snapshot};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Temperature(pub f64);

#[derive(Debug, Clone, Copy)]
enum Phase {
    Save,
    Mutate,
    Restore,
}
impl ScheduleSet for Phase {
    fn to_index(&self) -> u32 {
        match self {
            Self::Save => 0,
            Self::Mutate => 1,
            Self::Restore => 2,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::Save => "Save",
            Self::Mutate => "Mutate",
            Self::Restore => "Restore",
        }
    }
}

fn mutate(mut t: ResMut<Temperature>) {
    t.0 = 999.0;
}

#[test]
fn save_then_mutate_then_restore_returns_original() {
    let mut s = Scheduler::default();
    s.add_resource(Temperature(300.0));
    s.add_resource(Snapshot::<Temperature>::default());

    s.add_update_system(snapshot_resource::<Temperature>(), Phase::Save);
    s.add_update_system(mutate, Phase::Mutate);
    s.add_update_system(restore_resource::<Temperature>(), Phase::Restore);

    s.organize_systems();
    s.run();

    let cell = s
        .resource_cell(std::any::TypeId::of::<Temperature>())
        .unwrap();
    let temp = cell.borrow();
    let temp = temp.downcast_ref::<Temperature>().unwrap();
    assert_eq!(
        *temp,
        Temperature(300.0),
        "restore brought back the saved value"
    );

    // Snapshot should be cleared (take semantics).
    let snap_cell = s
        .resource_cell(std::any::TypeId::of::<Snapshot<Temperature>>())
        .unwrap();
    let snap = snap_cell.borrow();
    let snap = snap.downcast_ref::<Snapshot<Temperature>>().unwrap();
    assert!(!snap.has_saved(), "restore should consume the saved value");
}

#[test]
fn restore_without_saved_is_a_noop() {
    let mut s = Scheduler::default();
    s.add_resource(Temperature(42.0));
    s.add_resource(Snapshot::<Temperature>::default());

    // Mutate first, then attempt to restore. With no saved value the
    // mutated value should persist — restore must not zero or panic.
    s.add_update_system(mutate, Phase::Mutate);
    s.add_update_system(restore_resource::<Temperature>(), Phase::Restore);

    s.organize_systems();
    s.run();

    let cell = s
        .resource_cell(std::any::TypeId::of::<Temperature>())
        .unwrap();
    let temp = cell.borrow();
    let temp = temp.downcast_ref::<Temperature>().unwrap();
    assert_eq!(*temp, Temperature(999.0), "no saved value → no restore");
}

#[test]
fn snapshot_default_is_empty_regardless_of_t() {
    // Snapshot<T>::default() must NOT require T: Default. Verify by using
    // a type that has no Default impl.
    struct NoDefault(#[allow(dead_code)] u32);
    let snap: Snapshot<NoDefault> = Snapshot::default();
    assert!(!snap.has_saved());
}

#[test]
fn snapshot_in_loop_for_tentative_step_pattern() {
    // Demonstrates the canonical use case: a Schedule::Loop that takes a
    // tentative step, then either accepts (loop-end converged) or
    // implicitly rejects on the next iter. We just verify the Snapshot
    // primitive holds up across multiple save/mutate/restore cycles.
    use grass_scheduler::{OnMax, Schedule};

    #[derive(Debug, Clone, Copy)]
    enum LoopPhase {
        Save,
        Mutate,
        RestoreAlways,
    }
    impl ScheduleSet for LoopPhase {
        fn to_index(&self) -> u32 {
            match self {
                Self::Save => 0,
                Self::Mutate => 1,
                Self::RestoreAlways => 2,
            }
        }
        fn name(&self) -> &'static str {
            match self {
                Self::Save => "Save",
                Self::Mutate => "Mutate",
                Self::RestoreAlways => "RestoreAlways",
            }
        }
    }

    #[derive(Default)]
    struct Iter(u32);
    fn count(mut i: ResMut<Iter>) {
        i.0 += 1;
    }
    fn converge_at_3(i: Res<Iter>) -> bool {
        i.0 >= 3
    }

    let mut s = Scheduler::default();
    s.add_resource(Temperature(300.0));
    s.add_resource(Snapshot::<Temperature>::default());
    s.add_resource(Iter::default());

    s.add_update_system(snapshot_resource::<Temperature>(), LoopPhase::Save);
    s.add_update_system(mutate, LoopPhase::Mutate);
    s.add_update_system(count, LoopPhase::Mutate);
    s.add_update_system(restore_resource::<Temperature>(), LoopPhase::RestoreAlways);

    let sched = Schedule::builder()
        .loop_until(converge_at_3, 5, OnMax::Panic, |body| {
            body.then::<LoopPhase>()
        })
        .build();
    s.set_schedule(sched);

    s.organize_systems();
    s.run();

    // After 3 iterations, Temperature must be back to 300.0 (every iter
    // saves, mutates, restores). Iter must equal 3.
    let cell = s
        .resource_cell(std::any::TypeId::of::<Temperature>())
        .unwrap();
    let temp = cell.borrow();
    let temp = temp.downcast_ref::<Temperature>().unwrap();
    assert_eq!(*temp, Temperature(300.0));

    let iter_cell = s.resource_cell(std::any::TypeId::of::<Iter>()).unwrap();
    let iter = iter_cell.borrow();
    let iter = iter.downcast_ref::<Iter>().unwrap();
    assert_eq!(iter.0, 3);
}
