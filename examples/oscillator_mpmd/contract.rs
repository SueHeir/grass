#![allow(dead_code)] // Each MPMD binary compiles this shared local-replay helper.
//! Shared composition for the two MPMD oscillator binaries and its local replay.
//!
//! The solver library owns `OscillatorState` and `PeerPosition`.  This example
//! owns `RemotePosition`, the narrow wire exchange contract: no solver-private
//! state crosses the process boundary.

use grass_app::prelude::*;
use grass_io::Config;
use grass_multi::{tick_subapp, LocalTransport, Multi, MultiAppExt, SubApps, Transport, Wire};
use grass_scheduler::prelude::*;
use oscillator_demo::{OscillatorPlugin, OscillatorState, PeerPosition};
use serde::Deserialize;
use std::any::TypeId;
use std::thread;

pub const A: &str = "a";
pub const B: &str = "b";
const LOCAL: &str = "solver";
const REMOTE: &str = "peer";

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct Case {
    steps: usize,
}

/// The complete, versioned-in-place exchange contract for this example.
/// Only the position used by the interface spring is mirrored remotely.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RemotePosition(pub f64);

impl Wire for RemotePosition {
    fn pack(&self) -> Vec<u8> {
        self.0.to_le_bytes().to_vec()
    }
    fn try_unpack(buf: &[u8]) -> Result<Self, grass_multi::WireUnpackError> {
        if buf.len() != 8 {
            return Err(grass_multi::WireUnpackError::new::<Self>(
                buf.len(),
                "expected one IEEE-754 f64 position (8 bytes)",
            ));
        }
        let mut bytes = [0; 8];
        bytes.copy_from_slice(buf);
        Ok(Self(f64::from_le_bytes(bytes)))
    }
    fn unpack(buf: &[u8]) -> Self {
        Self::try_unpack(buf).expect("RemotePosition wire contract")
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SideResult {
    pub state: OscillatorState,
    pub mirrored_peer: RemotePosition,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairResult {
    pub a: SideResult,
    pub b: SideResult,
}

/// One completed coupling iteration, retained by the example runner so a
/// transport implementation can be compared over its whole trajectory.
#[derive(Clone, Debug, PartialEq)]
pub struct PairTrace {
    pub result: PairResult,
    pub steps: Vec<PairResult>,
}

impl PairResult {
    pub fn fingerprint(self) -> [u64; 4] {
        [
            self.a.state.x.to_bits(),
            self.a.state.v.to_bits(),
            self.b.state.x.to_bits(),
            self.b.state.v.to_bits(),
        ]
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    TickLocal,
    ExportBeforeRemoteTick,
    TickRemote,
    ImportAfterRemoteTick,
}
impl ScheduleSet for Phase {
    fn to_index(&self) -> u32 {
        match self {
            Self::TickLocal => 0,
            Self::ExportBeforeRemoteTick => 1,
            Self::TickRemote => 2,
            Self::ImportAfterRemoteTick => 3,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::TickLocal => "TickLocal",
            Self::ExportBeforeRemoteTick => "ExportBeforeRemoteTick",
            Self::TickRemote => "TickRemote",
            Self::ImportAfterRemoteTick => "ImportAfterRemoteTick",
        }
    }
}

fn export_before_remote_tick(world: Multi) {
    // The mirror sends its own resource cell, so copy the just-advanced local
    // solver export before ticking it.  This is deliberately separate from
    // solver state and from the import below.
    let position = world.expect_read::<OscillatorState>(LOCAL).x;
    world.expect_write::<RemotePosition>(REMOTE).0 = position;
}

fn import_after_remote_tick(world: Multi) {
    // The receive pump has just overwritten the remote mirror.  The solver
    // consumes this value on its next local tick.
    let peer = world.expect_read::<RemotePosition>(REMOTE).0;
    world.expect_write::<PeerPosition>(LOCAL).0 = peer;
}

fn get<T: Copy + 'static>(parent: &App, subapp: &str) -> T {
    let subs = parent
        .get_resource_ref::<SubApps>()
        .expect("registered sub-apps");
    let cell = subs
        .find(subapp)
        .expect("known sub-app")
        .resource_cell(TypeId::of::<T>())
        .expect("registered resource")
        .borrow();
    *cell.downcast_ref::<T>().expect("resource type")
}

fn put<T: Copy + 'static>(parent: &mut App, subapp: &str, value: T) {
    let subs = parent
        .get_mut_resource(TypeId::of::<SubApps>())
        .expect("registered sub-apps");
    let mut subs = subs.borrow_mut();
    let cell = subs
        .downcast_mut::<SubApps>()
        .expect("SubApps resource")
        .find(subapp)
        .expect("known sub-app")
        .resource_cell(TypeId::of::<T>())
        .expect("registered resource");
    *cell
        .borrow_mut()
        .downcast_mut::<T>()
        .expect("resource type") = value;
}

/// Build one independent binary's parent: local solver plus remote mirror.
/// Setup sends the initial export and receives the initial peer value; fixed
/// `steps` is the shared termination contract, so neither rank waits for an
/// out-of-band stop message.
pub fn build_side<Tr: Transport + 'static>(name: &str, transport: Tr) -> (App, usize) {
    let mut parent = App::new();
    parent.add_resource(Config::from_str(include_str!("config.toml")));
    let steps = Config::load::<Case>(&mut parent, "case").steps;
    // The parent config remains declarative.  Unlike the local sub-App name,
    // its `a`/`b` table selects this binary's physical initial condition.
    let solver_config = Config::from_str(include_str!("config.toml")).for_subapp(name, None);
    let mut solver = App::new();
    solver.add_resource(solver_config);
    solver.add_plugins(OscillatorPlugin);
    parent.add_subapp(LOCAL, solver);
    parent
        .add_remote_subapp(REMOTE, transport)
        .send_at_setup::<RemotePosition>()
        .recv_at_setup::<RemotePosition>()
        .send_each_iter::<RemotePosition>()
        .recv_each_iter::<RemotePosition>()
        .finish();

    // Initialise the setup export from the local solver.  The reciprocal
    // receive establishes the same peer_x0 that the in-process case uses.
    let initial = get::<OscillatorState>(&parent, LOCAL).x;
    assert_eq!(
        get::<PeerPosition>(&parent, LOCAL).0,
        if name == A { -0.5 } else { 1.0 },
        "the declarative initial peer position must reach the local solver"
    );
    put(&mut parent, REMOTE, RemotePosition(initial));
    parent.add_update_system(tick_subapp(LOCAL, 1), Phase::TickLocal);
    parent.add_update_system(export_before_remote_tick, Phase::ExportBeforeRemoteTick);
    parent.add_update_system(tick_subapp(REMOTE, 1), Phase::TickRemote);
    parent.add_update_system(import_after_remote_tick, Phase::ImportAfterRemoteTick);
    (parent, steps)
}

pub fn run_side_with_trace<Tr: Transport + 'static>(
    name: &str,
    transport: Tr,
) -> (SideResult, Vec<SideResult>) {
    let (mut parent, steps) = build_side(name, transport);
    parent.prepare();
    // Unlike a parent App's scheduler setup, sub-App preparation is lazy.
    // Complete this mirror's declared setup exchange before the first local
    // tick, without advancing either solver or consuming an iteration slot.
    parent
        .get_mut_resource(TypeId::of::<SubApps>())
        .expect("registered sub-apps")
        .borrow_mut()
        .downcast_mut::<SubApps>()
        .expect("SubApps resource")
        .prepare(REMOTE);
    let mut trace = Vec::with_capacity(steps);
    for _ in 0..steps {
        parent.run();
        trace.push(SideResult {
            state: get(&parent, LOCAL),
            mirrored_peer: get(&parent, REMOTE),
        });
    }
    (
        trace.last().copied().expect("at least one coupling step"),
        trace,
    )
}

pub fn run_side<Tr: Transport + 'static>(name: &str, transport: Tr) -> SideResult {
    run_side_with_trace(name, transport).0
}

/// In-process reference using the exact same `RemotePosition` wire contract
/// and parent schedule, with `LocalTransport` replacing MPI.
pub fn run_local_pair() -> PairResult {
    run_local_pair_trace().result
}

/// Replay both MPMD parents with `LocalTransport`, preserving every completed
/// coupling step for comparison with the independently written recurrence.
pub fn run_local_pair_trace() -> PairTrace {
    let (a_transport, b_transport) = LocalTransport::pair();
    let a = thread::spawn(move || run_side_with_trace(A, a_transport));
    let b = thread::spawn(move || run_side_with_trace(B, b_transport));
    let (a, a_steps) = a.join().expect("local A thread");
    let (b, b_steps) = b.join().expect("local B thread");
    assert_eq!(a_steps.len(), b_steps.len(), "two-sided trace length");
    let steps = a_steps
        .into_iter()
        .zip(b_steps)
        .map(|(a, b)| PairResult { a, b })
        .collect();
    PairTrace {
        result: PairResult { a, b },
        steps,
    }
}

pub fn print_pair(label: &str, result: PairResult) {
    println!(
        "{label} a={:.17e},{:.17e} b={:.17e},{:.17e} mirrors={:.17e},{:.17e} fingerprint={:016x?}",
        result.a.state.x,
        result.a.state.v,
        result.b.state.x,
        result.b.state.v,
        result.a.mirrored_peer.0,
        result.b.mirrored_peer.0,
        result.fingerprint()
    );
}

pub fn print_trace(label: &str, steps: &[PairResult]) {
    for (step, result) in steps.iter().enumerate() {
        println!(
            "{label}_TRACE step={} a={:.17e},{:.17e} b={:.17e},{:.17e}",
            step + 1,
            result.a.state.x,
            result.a.state.v,
            result.b.state.x,
            result.b.state.v,
        );
    }
}
