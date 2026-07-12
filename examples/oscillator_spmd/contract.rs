#![allow(dead_code)] // The single binary uses one of the two composition paths per launch.
//! Shared composition for the single-binary SPMD oscillator and its local replay.
//!
//! This is the same coupled-oscillator exchange contract as the MPMD example,
//! with one deliberate difference: **nothing here decides whether the two roles
//! run in one process or one-per-rank.** That decision is made entirely by the
//! declarative `[topology]` table (see `main.rs`), so the composition below is
//! byte-identical between the serial local run and the MPI role split.
//!
//! The solver library owns `OscillatorState` and `PeerPosition`. This example
//! owns `RemotePosition`, the narrow wire exchange contract: no solver-private
//! state crosses the process boundary.

use grass_app::prelude::*;
use grass_io::Config;
use grass_multi::{tick_subapp, Multi, MultiAppExt, SubApps, Transport, Wire};
use grass_scheduler::prelude::*;
use oscillator_demo::{OscillatorPlugin, OscillatorState, PeerPosition};
use serde::Deserialize;
use std::any::TypeId;

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
    let position = world.expect_read::<OscillatorState>(LOCAL).x;
    world.expect_write::<RemotePosition>(REMOTE).0 = position;
}

fn import_after_remote_tick(world: Multi) {
    let peer = world.expect_read::<RemotePosition>(REMOTE).0;
    world.expect_write::<PeerPosition>(LOCAL).0 = peer;
}

fn get<T: Copy + 'static>(parent: &App, subapp: &str) -> T {
    let subs = parent
        .get_resource_ref::<SubApps>()
        .expect("registered sub-apps");
    let physics = subs.find(subapp).expect("known sub-app");
    let cell = physics
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
    let physics = subs
        .downcast_mut::<SubApps>()
        .expect("SubApps resource")
        .find(subapp)
        .expect("known sub-app");
    let cell = physics
        .resource_cell(TypeId::of::<T>())
        .expect("registered resource");
    *cell
        .borrow_mut()
        .downcast_mut::<T>()
        .expect("resource type") = value;
}

/// Build one role's parent App: local solver plus remote mirror. `config_str`
/// is the whole declarative document; the `a`/`b` sub-table selects this role's
/// physical initial condition. Fixed `steps` is the shared termination
/// contract, so neither role waits for an out-of-band stop message.
pub fn build_side<Tr: Transport + 'static>(
    name: &str,
    transport: Tr,
    config_str: &str,
) -> (App, usize) {
    let mut parent = App::new();
    parent.add_resource(Config::from_str(config_str));
    let steps = Config::load::<Case>(&mut parent, "case").steps;
    let solver_config = Config::from_str(config_str).for_subapp(name, None);
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
    config_str: &str,
) -> (SideResult, Vec<SideResult>) {
    let (mut parent, steps) = build_side(name, transport, config_str);
    parent.prepare();
    // Sub-App preparation is lazy: complete this mirror's declared setup
    // exchange before the first local tick, without advancing either solver or
    // consuming an iteration slot.
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

pub fn print_local_trace(a_steps: &[SideResult], b_steps: &[SideResult]) {
    assert_eq!(a_steps.len(), b_steps.len(), "two-sided trace length");
    for (step, (a, b)) in a_steps.iter().zip(b_steps).enumerate() {
        println!(
            "LOCAL_TRACE step={} a={:.17e},{:.17e} b={:.17e},{:.17e}",
            step + 1,
            a.state.x,
            a.state.v,
            b.state.x,
            b.state.v,
        );
    }
}

pub fn print_side_trace(name: &str, steps: &[SideResult]) {
    for (step, state) in steps.iter().enumerate() {
        println!(
            "MPI_TRACE side={name} step={} local={:.17e},{:.17e}",
            step + 1,
            state.state.x,
            state.state.v,
        );
    }
}
