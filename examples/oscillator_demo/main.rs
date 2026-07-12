//! Pedagogical oscillator composition: direct typed coupling and port coupling.

use grass_app::prelude::*;
use grass_io::{Config, InputPlugin, MultiIoExt};
use grass_multi::{
    consume_field, expose_field, tick_subapp, MultiAppExt, MultiRes, MultiResMut, Namespace,
    OuterIterStopPlugin, SubApps,
};
use grass_scheduler::prelude::*;
#[cfg(test)]
use oscillator_demo::analytical_uncoupled;
use oscillator_demo::{FinalState, OscillatorPlugin, OscillatorState, PeerPosition, PositionPort};

#[derive(Namespace)]
struct A;
#[derive(Namespace)]
struct B;

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    TickA,
    TickB,
    Couple,
    Check,
}

fn direct_exchange(
    a: MultiRes<OscillatorState, A>,
    b: MultiRes<OscillatorState, B>,
    mut a_peer: MultiResMut<PeerPosition, A>,
    mut b_peer: MultiResMut<PeerPosition, B>,
) {
    a_peer.0 = b.x;
    b_peer.0 = a.x;
}

fn final_state(parent: &App) -> FinalState {
    let subs = parent.get_resource_ref::<SubApps>().expect("sub-apps");
    let read = |name| {
        let participant = subs.find(name).expect("known sub-app");
        let cell = participant
            .resource_cell(std::any::TypeId::of::<OscillatorState>())
            .expect("oscillator state")
            .borrow();
        *cell
            .downcast_ref::<OscillatorState>()
            .expect("correct state")
    };
    FinalState {
        a: read(A::NAME),
        b: read(B::NAME),
    }
}

fn build_parent(port_based: bool) -> App {
    let mut parent = App::new();
    if std::env::args().len() == 1 {
        parent.add_resource(Config::from_str(include_str!("config.toml")));
    }
    parent.add_plugins(InputPlugin);
    parent.add_subapp_with_config(A::NAME, |app| {
        app.add_plugins(OscillatorPlugin);
    });
    parent.add_subapp_with_config(B::NAME, |app| {
        app.add_plugins(OscillatorPlugin);
    });
    parent.add_update_system(tick_subapp(A::NAME, 1), Phase::TickA);
    if port_based {
        // The application owns these two directional contracts; the library only
        // exports PositionPort<Source>, not either peer's private state type.
        parent.add_port::<PositionPort<A>>();
        parent.add_port::<PositionPort<B>>();
        parent.add_update_system(
            expose_field::<OscillatorState, PositionPort<A>>(
                A::NAME,
                PositionPort::<A>::from_state,
            ),
            Phase::Couple,
        );
        parent.add_update_system(
            expose_field::<OscillatorState, PositionPort<B>>(
                B::NAME,
                PositionPort::<B>::from_state,
            ),
            Phase::Couple,
        );
        parent.add_update_system(
            consume_field::<PeerPosition, PositionPort<A>>(B::NAME, |peer, value| {
                peer.0 = value.position.0
            }),
            Phase::Couple,
        );
        parent.add_update_system(
            consume_field::<PeerPosition, PositionPort<B>>(A::NAME, |peer, value| {
                peer.0 = value.position.0
            }),
            Phase::Couple,
        );
    } else {
        // Experimental, tightly-bound composition: this app names both
        // instances' concrete resources through compile-time namespaces.
        parent.add_update_system(direct_exchange, Phase::Couple);
    }
    parent.add_update_system(tick_subapp(B::NAME, 1), Phase::TickB);
    parent.add_plugins(OuterIterStopPlugin {
        n_iters: 400,
        phase: Phase::Check,
    });
    parent
}

fn run(port_based: bool) -> FinalState {
    let mut parent = build_parent(port_based);
    parent.start();
    final_state(&parent)
}

fn main() {
    if std::env::args().any(|arg| arg == "--generate-config") {
        build_parent(false).start();
        return;
    }
    let direct = run(false);
    let port = run(true);
    println!("direct  fingerprint={:016x?}", direct.fingerprint());
    println!("port    fingerprint={:016x?}", port.fingerprint());
    assert_eq!(
        direct, port,
        "same explicit schedule must give identical results"
    );
    println!("PASS direct and port explicit composition agree");
}

#[test]
fn uncoupled_integrator_tracks_analytical_oscillator() {
    let mut app = App::new();
    app.add_resource(Config::from_str(
        "[oscillator]\nx0=1.0\nv0=0.0\nstiffness=1.0\nmass=1.0\ndt=0.0001\n",
    ));
    app.add_plugins(OscillatorPlugin);
    app.prepare();
    for _ in 0..10_000 {
        app.run();
    }
    let actual = *app.get_resource_ref::<OscillatorState>().unwrap();
    let expected = analytical_uncoupled(OscillatorState { x: 1.0, v: 0.0 }, 1.0, 1.0, 1.0);
    let error = (actual.x - expected.x)
        .abs()
        .max((actual.v - expected.v).abs());
    println!("uncoupled max_error={error:.3e}");
    assert!(error < 1.0e-4, "semi-implicit Euler error {error}");
}
