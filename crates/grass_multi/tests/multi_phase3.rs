//! Phase 3 integration: two parent Apps in the same process linked via
//! `LocalTransport`. Each registers the OTHER as a remote sub-App via
//! `add_remote_subapp`. They send_each_iter / recv_each_iter a `Counter`
//! resource bidirectionally. The test asserts that, after N parent iters,
//! each side's mirror sees the peer's locally-incremented counter — which
//! demonstrates the full Phase 3 pump infrastructure end to end.

use grass_app::prelude::*;
use grass_multi::{tick_subapp, Multi, MultiAppExt, SubApps, Wire};
use grass_multi::{LocalTransport, RemoteMirrorPhysics, RemotePumpPhase, Transport};
use grass_scheduler::prelude::*;
use std::thread;

// ─── Counter resource (implements Wire) ─────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Counter(pub u64);

impl Wire for Counter {
    fn pack(&self) -> Vec<u8> {
        self.0.to_le_bytes().to_vec()
    }
    fn unpack(buf: &[u8]) -> Self {
        let mut a = [0u8; 8];
        a.copy_from_slice(&buf[..8]);
        Counter(u64::from_le_bytes(a))
    }
}

// ─── Local "logic" sub-App: increments Counter by `step_size` each tick ────

#[derive(Debug, Clone, Copy)]
enum LocalSchedule {
    Tick,
}
impl ScheduleSet for LocalSchedule {
    fn to_index(&self) -> u32 {
        0
    }
    fn name(&self) -> &'static str {
        "Tick"
    }
}

#[derive(Debug, Clone, Copy)]
struct StepSize(u64);

fn local_tick(mut c: ResMut<Counter>, step: Res<StepSize>) {
    c.0 += step.0;
}

fn build_local(step_size: u64) -> App {
    let mut app = App::new();
    app.add_resource(Counter(0));
    app.add_resource(StepSize(step_size));
    app.add_update_system(local_tick, LocalSchedule::Tick);
    app
}

// ─── Parent schedule ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum ParentSchedule {
    /// Tick the local logic (Counter increments).
    TickLocal,
    /// Tick the remote mirror — sends our Counter, recvs peer's Counter.
    TickPeer,
    /// Multi-using "import" system: read peer's mirrored Counter into a
    /// parent-local resource so we can assert on it after the loop.
    Import,
}
impl ScheduleSet for ParentSchedule {
    fn to_index(&self) -> u32 {
        match self {
            Self::TickLocal => 0,
            Self::TickPeer => 1,
            Self::Import => 2,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::TickLocal => "TickLocal",
            Self::TickPeer => "TickPeer",
            Self::Import => "Import",
        }
    }
}

/// Pulls the most recent Counter from the remote peer's mirror into a
/// parent-local `LastSeenPeer` so the test can read it after run().
#[derive(Debug, Clone, Copy, Default)]
struct LastSeenPeer(u64);

fn import_peer(world: Multi, mut last: ResMut<LastSeenPeer>) {
    last.0 = world.expect_read::<Counter>("peer").0;
}

// ─── One "binary": runs in its own thread ───────────────────────────────────

/// Builds the parent App, runs it for `n_iters`, returns
/// `(local_counter_final, last_seen_peer)`.
fn run_binary<Tr: Transport + 'static>(
    transport: Tr,
    step_size: u64,
    n_iters: usize,
) -> (u64, u64) {
    let local = build_local(step_size);

    let mut parent = App::new();
    parent.add_subapp("local", local);
    parent
        .add_remote_subapp("peer", transport)
        .send_each_iter::<Counter>()
        .recv_each_iter::<Counter>();
    parent.add_resource(LastSeenPeer::default());

    parent.add_update_system(tick_subapp("local", 1), ParentSchedule::TickLocal);
    parent.add_update_system(tick_subapp("peer", 1), ParentSchedule::TickPeer);
    parent.add_update_system(import_peer, ParentSchedule::Import);

    parent.prepare();
    for _ in 0..n_iters {
        parent.run();
    }

    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let local_cell = subs
        .find("local")
        .unwrap()
        .resource_cell(std::any::TypeId::of::<Counter>())
        .unwrap()
        .borrow();
    let local_val = local_cell.downcast_ref::<Counter>().unwrap().0;
    drop(local_cell);
    drop(subs);

    let last = parent.get_resource_ref::<LastSeenPeer>().unwrap().0;
    (local_val, last)
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn missing_export_before_tickpeer_leaves_remote_mirror_stale() {
    const N: usize = 5;
    let (server_t, client_t) = LocalTransport::pair();
    let h_a = thread::spawn(move || run_binary(server_t, 1, N));
    let h_b = thread::spawn(move || run_binary(client_t, 10, N));

    let (a_local, a_seen_peer) = h_a.join().unwrap();
    let (b_local, b_seen_peer) = h_b.join().unwrap();

    assert_eq!(a_local, N as u64, "A's local counter ticked N times");
    assert_eq!(
        b_local,
        N as u64 * 10,
        "B's local counter ticked N×10 times"
    );

    // `RemoteMirrorPhysics::step` sends the remote mirror's current resource
    // cells. Without an Export system copying local.Counter into peer.Counter
    // before TickPeer, both sides keep sending the mirror's default value
    // instead of their local counters. This is the intended stale-mirror
    // semantics documented for the export-before-tick hazard.
    assert_eq!(
        a_seen_peer, 0,
        "A never sees B's local counter without Export before TickPeer"
    );
    assert_eq!(
        b_seen_peer, 0,
        "B never sees A's local counter without Export before TickPeer"
    );
}

// ─── Properly-wired test: copy local→mirror BEFORE TickPeer ────────────────

fn export_local_to_peer(world: Multi) {
    let v = world.expect_read::<Counter>("local").0;
    world.expect_write::<Counter>("peer").0 = v;
}

#[derive(Debug, Clone, Copy)]
enum ParentScheduleV2 {
    TickLocal,
    Export,
    TickPeer,
    Import,
}
impl ScheduleSet for ParentScheduleV2 {
    fn to_index(&self) -> u32 {
        match self {
            Self::TickLocal => 0,
            Self::Export => 1,
            Self::TickPeer => 2,
            Self::Import => 3,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::TickLocal => "TickLocal",
            Self::Export => "Export",
            Self::TickPeer => "TickPeer",
            Self::Import => "Import",
        }
    }
}

fn run_binary_v2<Tr: Transport + 'static>(
    transport: Tr,
    step_size: u64,
    n_iters: usize,
) -> (u64, u64) {
    let local = build_local(step_size);

    let mut parent = App::new();
    parent.add_subapp("local", local);
    parent
        .add_remote_subapp("peer", transport)
        .send_each_iter::<Counter>()
        .recv_each_iter::<Counter>();
    parent.add_resource(LastSeenPeer::default());

    parent.add_update_system(tick_subapp("local", 1), ParentScheduleV2::TickLocal);
    parent.add_update_system(export_local_to_peer, ParentScheduleV2::Export);
    parent.add_update_system(tick_subapp("peer", 1), ParentScheduleV2::TickPeer);
    parent.add_update_system(import_peer, ParentScheduleV2::Import);

    parent.prepare();
    for _ in 0..n_iters {
        parent.run();
    }

    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let local_cell = subs
        .find("local")
        .unwrap()
        .resource_cell(std::any::TypeId::of::<Counter>())
        .unwrap()
        .borrow();
    let local_val = local_cell.downcast_ref::<Counter>().unwrap().0;
    drop(local_cell);
    drop(subs);

    let last = parent.get_resource_ref::<LastSeenPeer>().unwrap().0;
    (local_val, last)
}

#[test]
fn properly_wired_export_makes_each_side_see_peer_local() {
    // With Export running before TickPeer, each iter sends the latest
    // local.Counter. The Import system reads peer.Counter AFTER TickPeer's
    // recv pump landed, so on iter k:
    //   - Local advances to k * step_size
    //   - Export copies local → mirror
    //   - TickPeer sends mirror, recvs peer (whose Export just ran on iter k too)
    //   - Import reads mirror → LastSeenPeer
    //
    // After N iters, each side's LastSeenPeer == peer's local at iter N.
    const N: usize = 5;
    let (server_t, client_t) = LocalTransport::pair();
    let h_a = thread::spawn(move || run_binary_v2(server_t, 1, N));
    let h_b = thread::spawn(move || run_binary_v2(client_t, 10, N));

    let (a_local, a_seen_peer) = h_a.join().unwrap();
    let (b_local, b_seen_peer) = h_b.join().unwrap();

    assert_eq!(a_local, N as u64);
    assert_eq!(b_local, N as u64 * 10);

    // The handshake symmetry of "send first then recv" means each side's
    // recv on iter k receives what the peer just sent on iter k. So both
    // sides converge to the same view as their peer's local-at-iter-N.
    assert_eq!(a_seen_peer, b_local, "A sees B's final counter");
    assert_eq!(b_seen_peer, a_local, "B sees A's final counter");
}

// ─── Setup-time handshake test ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct CritDt(pub f64);

impl Wire for CritDt {
    fn pack(&self) -> Vec<u8> {
        self.0.to_le_bytes().to_vec()
    }
    fn unpack(buf: &[u8]) -> Self {
        let mut a = [0u8; 8];
        a.copy_from_slice(&buf[..8]);
        CritDt(f64::from_le_bytes(a))
    }
}

fn run_handshake<Tr: Transport + 'static>(transport: Tr, my_crit: f64) -> f64 {
    let mut parent = App::new();
    let mut local = App::new();
    local.add_resource(CritDt(my_crit));
    parent.add_subapp("local", local);

    parent
        .add_remote_subapp("peer", transport)
        .send_at_setup::<CritDt>()
        .recv_at_setup::<CritDt>();

    // Seed the mirror's CritDt to the local value before the handshake fires.
    fn export_crit_for_handshake(world: Multi) {
        let v = world.expect_read::<CritDt>("local").0;
        world.expect_write::<CritDt>("peer").0 = v;
    }
    #[derive(Debug, Clone, Copy)]
    enum Phase {
        SeedMirror,
        Handshake,
    }
    impl ScheduleSet for Phase {
        fn to_index(&self) -> u32 {
            match self {
                Self::SeedMirror => 0,
                Self::Handshake => 1,
            }
        }
        fn name(&self) -> &'static str {
            match self {
                Self::SeedMirror => "SeedMirror",
                Self::Handshake => "Handshake",
            }
        }
    }
    parent.add_update_system(export_crit_for_handshake, Phase::SeedMirror);
    parent.add_update_system(tick_subapp("peer", 1), Phase::Handshake);

    parent.prepare();
    parent.run(); // first run triggers RemoteMirrorPhysics::prepare → setup pumps

    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let cell = subs
        .find("peer")
        .unwrap()
        .resource_cell(std::any::TypeId::of::<CritDt>())
        .unwrap()
        .borrow();
    cell.downcast_ref::<CritDt>().unwrap().0
}

#[test]
fn send_at_setup_handshake_exchanges_critical_dt() {
    let (server_t, client_t) = LocalTransport::pair();
    let h_a = thread::spawn(move || run_handshake(server_t, 1.0e-7));
    let h_b = thread::spawn(move || run_handshake(client_t, 5.0e-6));

    let a_seen = h_a.join().unwrap();
    let b_seen = h_b.join().unwrap();

    assert_eq!(a_seen, 5.0e-6, "A's mirror sees B's CritDt after handshake");
    assert_eq!(b_seen, 1.0e-7, "B's mirror sees A's CritDt after handshake");
}

// ─── In-process vs remote-transport equivalence ────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct CoupledQuantity {
    value: f64,
    flux: f64,
}

impl Wire for CoupledQuantity {
    fn pack(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        out.extend_from_slice(&self.value.to_le_bytes());
        out.extend_from_slice(&self.flux.to_le_bytes());
        out
    }

    fn unpack(buf: &[u8]) -> Self {
        let mut value = [0u8; 8];
        let mut flux = [0u8; 8];
        value.copy_from_slice(&buf[..8]);
        flux.copy_from_slice(&buf[8..16]);
        Self {
            value: f64::from_le_bytes(value),
            flux: f64::from_le_bytes(flux),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct PeerQuantity(CoupledQuantity);

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct SolverState {
    value: f64,
    velocity: f64,
    accumulated_peer_flux: f64,
}

#[derive(Debug, Clone, Copy)]
struct SolverParams {
    stiffness: f64,
    damping: f64,
    dt: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct EquivalenceResult {
    state: SolverState,
    local_quantity: CoupledQuantity,
    seen_peer: CoupledQuantity,
}

#[derive(Debug, Clone, Copy)]
enum SolverSchedule {
    Step,
}

impl ScheduleSet for SolverSchedule {
    fn to_index(&self) -> u32 {
        0
    }

    fn name(&self) -> &'static str {
        "Step"
    }
}

fn coupled_step(
    mut state: ResMut<SolverState>,
    params: Res<SolverParams>,
    peer: Res<PeerQuantity>,
    mut output: ResMut<CoupledQuantity>,
) {
    let forcing = 0.25 * peer.0.flux - params.stiffness * state.value;
    state.velocity = (state.velocity + params.dt * forcing) * params.damping;
    state.value += params.dt * state.velocity + 0.05 * peer.0.value;
    state.accumulated_peer_flux += peer.0.flux;
    output.value = state.value;
    output.flux = params.stiffness * state.value + state.velocity;
}

fn build_coupled_solver(initial_value: f64, initial_velocity: f64, params: SolverParams) -> App {
    let mut app = App::new();
    app.add_resource(SolverState {
        value: initial_value,
        velocity: initial_velocity,
        accumulated_peer_flux: 0.0,
    });
    app.add_resource(params);
    app.add_resource(PeerQuantity::default());
    app.add_resource(CoupledQuantity {
        value: initial_value,
        flux: params.stiffness * initial_value + initial_velocity,
    });
    app.add_update_system(coupled_step, SolverSchedule::Step);
    app
}

#[derive(Debug, Clone, Copy)]
enum EquivalenceSchedule {
    TickA,
    TickB,
    Exchange,
}

impl ScheduleSet for EquivalenceSchedule {
    fn to_index(&self) -> u32 {
        match self {
            Self::TickA => 0,
            Self::TickB => 1,
            Self::Exchange => 2,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::TickA => "TickA",
            Self::TickB => "TickB",
            Self::Exchange => "Exchange",
        }
    }
}

fn exchange_in_process(world: Multi) {
    let a = *world.expect_read::<CoupledQuantity>("a");
    let b = *world.expect_read::<CoupledQuantity>("b");
    world.expect_write::<PeerQuantity>("a").0 = b;
    world.expect_write::<PeerQuantity>("b").0 = a;
}

fn read_equivalence_result(parent: &App, ns: &str) -> EquivalenceResult {
    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let physics = subs.find(ns).unwrap();

    let state = {
        let cell = physics
            .resource_cell(std::any::TypeId::of::<SolverState>())
            .unwrap()
            .borrow();
        *cell.downcast_ref::<SolverState>().unwrap()
    };
    let local_quantity = {
        let cell = physics
            .resource_cell(std::any::TypeId::of::<CoupledQuantity>())
            .unwrap()
            .borrow();
        *cell.downcast_ref::<CoupledQuantity>().unwrap()
    };
    let seen_peer = {
        let cell = physics
            .resource_cell(std::any::TypeId::of::<PeerQuantity>())
            .unwrap()
            .borrow();
        cell.downcast_ref::<PeerQuantity>().unwrap().0
    };

    EquivalenceResult {
        state,
        local_quantity,
        seen_peer,
    }
}

fn run_in_process_equivalence(n_iters: usize) -> (EquivalenceResult, EquivalenceResult) {
    let mut parent = App::new();
    parent.add_subapp(
        "a",
        build_coupled_solver(
            1.25,
            -0.375,
            SolverParams {
                stiffness: 1.5,
                damping: 0.93,
                dt: 0.125,
            },
        ),
    );
    parent.add_subapp(
        "b",
        build_coupled_solver(
            -0.75,
            0.5,
            SolverParams {
                stiffness: 0.85,
                damping: 0.97,
                dt: 0.125,
            },
        ),
    );

    parent.add_update_system(tick_subapp("a", 1), EquivalenceSchedule::TickA);
    parent.add_update_system(tick_subapp("b", 1), EquivalenceSchedule::TickB);
    parent.add_update_system(exchange_in_process, EquivalenceSchedule::Exchange);

    parent.prepare();
    for _ in 0..n_iters {
        parent.run();
    }

    (
        read_equivalence_result(&parent, "a"),
        read_equivalence_result(&parent, "b"),
    )
}

#[derive(Debug, Clone, Copy)]
enum RemoteEquivalenceSchedule {
    TickLocal,
    Export,
    TickPeer,
    Import,
}

impl ScheduleSet for RemoteEquivalenceSchedule {
    fn to_index(&self) -> u32 {
        match self {
            Self::TickLocal => 0,
            Self::Export => 1,
            Self::TickPeer => 2,
            Self::Import => 3,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::TickLocal => "TickLocal",
            Self::Export => "Export",
            Self::TickPeer => "TickPeer",
            Self::Import => "Import",
        }
    }
}

fn export_local_quantity(world: Multi) {
    let local = *world.expect_read::<CoupledQuantity>("local");
    *world.expect_write::<CoupledQuantity>("peer") = local;
}

fn import_peer_quantity(world: Multi) {
    let peer = *world.expect_read::<CoupledQuantity>("peer");
    world.expect_write::<PeerQuantity>("local").0 = peer;
}

fn run_remote_equivalence_side<Tr: Transport + 'static>(
    transport: Tr,
    initial_value: f64,
    initial_velocity: f64,
    params: SolverParams,
    n_iters: usize,
) -> EquivalenceResult {
    let mut parent = App::new();
    parent.add_subapp(
        "local",
        build_coupled_solver(initial_value, initial_velocity, params),
    );
    parent
        .add_remote_subapp("peer", transport)
        .send_each_iter::<CoupledQuantity>()
        .recv_each_iter::<CoupledQuantity>();

    parent.add_update_system(
        tick_subapp("local", 1),
        RemoteEquivalenceSchedule::TickLocal,
    );
    parent.add_update_system(export_local_quantity, RemoteEquivalenceSchedule::Export);
    parent.add_update_system(tick_subapp("peer", 1), RemoteEquivalenceSchedule::TickPeer);
    parent.add_update_system(import_peer_quantity, RemoteEquivalenceSchedule::Import);

    parent.prepare();
    for _ in 0..n_iters {
        parent.run();
    }

    read_equivalence_result(&parent, "local")
}

fn assert_near_eq(actual: f64, expected: f64, label: &str) {
    let tol = 4.0 * f64::EPSILON * actual.abs().max(expected.abs()).max(1.0);
    assert!(
        (actual - expected).abs() <= tol,
        "{label}: actual {actual:?} expected {expected:?} tol {tol:?}"
    );
}

fn assert_equivalence_result(actual: EquivalenceResult, expected: EquivalenceResult, label: &str) {
    assert_near_eq(
        actual.state.value,
        expected.state.value,
        &format!("{label}.state.value"),
    );
    assert_near_eq(
        actual.state.velocity,
        expected.state.velocity,
        &format!("{label}.state.velocity"),
    );
    assert_near_eq(
        actual.state.accumulated_peer_flux,
        expected.state.accumulated_peer_flux,
        &format!("{label}.state.accumulated_peer_flux"),
    );
    assert_eq!(
        actual.local_quantity, expected.local_quantity,
        "{label}.local_quantity should be bitwise identical"
    );
    assert_eq!(
        actual.seen_peer, expected.seen_peer,
        "{label}.seen_peer should be bitwise identical"
    );
}

#[test]
fn in_process_and_remote_transport_coupling_are_equivalent() {
    const N: usize = 8;

    let expected = run_in_process_equivalence(N);

    let (server_t, client_t) = LocalTransport::pair();
    let h_a = thread::spawn(move || {
        run_remote_equivalence_side(
            server_t,
            1.25,
            -0.375,
            SolverParams {
                stiffness: 1.5,
                damping: 0.93,
                dt: 0.125,
            },
            N,
        )
    });
    let h_b = thread::spawn(move || {
        run_remote_equivalence_side(
            client_t,
            -0.75,
            0.5,
            SolverParams {
                stiffness: 0.85,
                damping: 0.97,
                dt: 0.125,
            },
            N,
        )
    });

    let remote = (h_a.join().unwrap(), h_b.join().unwrap());

    assert_equivalence_result(remote.0, expected.0, "side A");
    assert_equivalence_result(remote.1, expected.1, "side B");
}

#[test]
fn remote_mirror_reports_truncated_string_payload_with_context() {
    let (peer_t, mirror_t) = LocalTransport::pair();
    let mut payload = 5u32.to_le_bytes().to_vec();
    payload.extend_from_slice(b"abc");
    peer_t.send(&payload);

    let mut mirror = RemoteMirrorPhysics::new("peer", Box::new(mirror_t));
    mirror.add_recv_each_iter::<String>();

    let err = mirror.try_step().unwrap_err();

    assert_eq!(err.mirror_name(), "peer");
    assert_eq!(err.phase(), RemotePumpPhase::EachIter);
    assert_eq!(err.recv_index(), 0);
    assert_eq!(err.resource_type(), "alloc::string::String");
    assert_eq!(err.payload_len(), 7);
    assert!(err.source().detail().contains("declares 5 UTF-8 bytes"));
    assert!(err
        .to_string()
        .contains("RemoteMirrorPhysics `peer` failed to unpack each-iter recv slot #0"));
}

#[test]
fn remote_mirror_reports_non_utf8_string_payload_with_context() {
    let (peer_t, mirror_t) = LocalTransport::pair();
    let mut payload = 2u32.to_le_bytes().to_vec();
    payload.extend_from_slice(&[0xff, 0xff]);
    peer_t.send(&payload);

    let mut mirror = RemoteMirrorPhysics::new("peer", Box::new(mirror_t));
    mirror.add_recv_each_iter::<String>();

    let err = mirror.try_step().unwrap_err();

    assert_eq!(err.mirror_name(), "peer");
    assert_eq!(err.phase(), RemotePumpPhase::EachIter);
    assert_eq!(err.recv_index(), 0);
    assert_eq!(err.resource_type(), "alloc::string::String");
    assert_eq!(err.payload_len(), 6);
    assert!(err.source().detail().contains("not UTF-8"));
}
