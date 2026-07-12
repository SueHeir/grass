//! Genuine child-scheduler cross-namespace access.

use grass_app::prelude::*;
use grass_multi::{tick_n_times, MultiAppExt, MultiRes, MultiResMut, Namespace};
use grass_scheduler::prelude::*;

#[derive(Namespace)]
struct A;
#[derive(Namespace)]
struct B;

#[derive(Debug)]
struct Counter(u32);
#[derive(Debug, Default)]
struct Seen(u32);

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum ChildPhase {
    Observe,
    MutatePeer,
}

fn observe_peer(peer: MultiRes<Counter, B>, mut seen: ResMut<Seen>) {
    seen.0 = peer.0;
}

fn mutate_peer(mut peer: MultiResMut<Counter, B>) {
    peer.0 += 5;
}

fn increment(mut counter: ResMut<Counter>) {
    counter.0 += 1;
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum ParentPhase {
    TickB,
    TickA,
}

#[test]
fn child_system_reads_and_writes_peer_with_typed_multires() {
    let mut a = App::new();
    a.add_resource(Seen::default());
    a.add_update_system(observe_peer, ChildPhase::Observe);
    a.add_update_system(mutate_peer, ChildPhase::MutatePeer);

    let mut b = App::new();
    b.add_resource(Counter(10));
    b.add_update_system(increment, ChildPhase::Observe);

    let mut parent = App::new();
    parent.add_subapp_typed::<A>(a);
    parent.add_subapp_typed::<B>(b);
    parent.add_update_system(tick_n_times::<B>(1), ParentPhase::TickB);
    parent.add_update_system(tick_n_times::<A>(1), ParentPhase::TickA);

    parent.prepare();
    parent.run();

    let subs = parent.get_resource_ref::<grass_multi::SubApps>().unwrap();
    let a = subs.find(A::NAME).unwrap();
    let seen = a
        .resource_cell(std::any::TypeId::of::<Seen>())
        .unwrap()
        .borrow();
    assert_eq!(seen.downcast_ref::<Seen>().unwrap().0, 11);
    drop(seen);

    let b = subs.find(B::NAME).unwrap();
    let counter = b
        .resource_cell(std::any::TypeId::of::<Counter>())
        .unwrap()
        .borrow();
    assert_eq!(counter.downcast_ref::<Counter>().unwrap().0, 16);
}

#[test]
#[should_panic(expected = "MultiRes self-access in child `A` is not allowed")]
fn child_self_access_fails_with_actionable_diagnostic() {
    fn invalid(_own: MultiRes<Seen, A>) {}

    let mut a = App::new();
    a.add_resource(Seen::default());
    a.add_update_system(invalid, ChildPhase::Observe);

    let mut b = App::new();
    b.add_resource(Counter(0));

    let mut parent = App::new();
    parent.add_subapp_typed::<A>(a);
    parent.add_subapp_typed::<B>(b);
    parent.add_update_system(tick_n_times::<A>(1), ParentPhase::TickA);
    parent.prepare();
    parent.run();
}
