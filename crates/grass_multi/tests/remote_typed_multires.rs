//! Typed `MultiRes` / `MultiResMut` across a **remote** sub-App boundary.
//!
//! `multi_phase3` proves the remote-mirror pump with the untyped [`Multi`]
//! accessor. This test pins the same explicit-synchronization contract for the
//! typed [`MultiRes`] / [`MultiResMut`] SystemParams: a parent-scheduler system
//! reads and writes a remote peer's mirrored resource through the typed handles
//! exactly as if it were local, and the value it sees is governed solely by the
//! explicit pump systems — never by the parameter retrieval.
//!
//! This does **not** prove that a system running inside a child scheduler can
//! request `MultiRes`; child-scheduler access needs a composition context that
//! remains available while that child is executing and is covered separately.
//!
//! This is the load-bearing invariant behind single-binary SPMD coupling:
//! `MultiRes::retrieve` / `MultiResMut::retrieve` only borrow a local resource
//! cell (see `typed_multi.rs`). They perform no transport work, so no MPI ever
//! happens inside SystemParam retrieval — remote data is moved only by the
//! explicit `tick_subapp(peer)` pump. The two tests below encode both halves:
//! the wired case converges through the typed handles, and a never-pumped
//! mirror stays at its default *even though the local side advanced*, which is
//! only possible if retrieval does not synchronize.

use grass_app::prelude::*;
use grass_multi::{namespace, tick_subapp, LocalTransport, MultiAppExt, MultiRes, MultiResMut};
use grass_multi::{Transport, Wire};
use grass_scheduler::prelude::*;
use std::thread;

namespace!(LocalNs = "local");
namespace!(PeerNs = "peer");

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

#[derive(Debug, Clone, Copy)]
struct StepSize(u64);

#[derive(Debug, Clone, Copy, Default)]
struct LastSeenPeer(u64);

#[derive(Debug, Clone, Copy)]
enum LocalTick {
    Tick,
}
impl ScheduleSet for LocalTick {
    fn to_index(&self) -> u32 {
        0
    }
    fn name(&self) -> &'static str {
        "Tick"
    }
}

fn local_tick(mut c: ResMut<Counter>, step: Res<StepSize>) {
    c.0 += step.0;
}

fn build_local(step_size: u64) -> App {
    let mut app = App::new();
    app.add_resource(Counter(0));
    app.add_resource(StepSize(step_size));
    app.add_update_system(local_tick, LocalTick::Tick);
    app
}

// Typed cross-namespace copy: read the local sub-App's Counter, write the
// remote peer mirror's Counter. Both handles resolve to local resource cells;
// the write is what `tick_subapp(peer)` later sends over the wire. This is the
// documented `MultiRes` + `MultiResMut` coupling shape, now spanning a remote
// mirror rather than two local sub-apps.
fn export_local_to_peer(local: MultiRes<Counter, LocalNs>, mut peer: MultiResMut<Counter, PeerNs>) {
    peer.0 = local.0;
}

// Typed read of the freshly-pumped peer mirror into a parent-local resource.
fn import_peer(peer: MultiRes<Counter, PeerNs>, mut last: ResMut<LastSeenPeer>) {
    last.0 = peer.0;
}

#[derive(Debug, Clone, Copy)]
enum Schedule {
    TickLocal,
    Export,
    TickPeer,
    Import,
}
impl ScheduleSet for Schedule {
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

fn run_binary<Tr: Transport + 'static>(transport: Tr, step_size: u64, n_iters: usize) -> u64 {
    let mut parent = App::new();
    parent.add_subapp("local", build_local(step_size));
    parent
        .add_remote_subapp("peer", transport)
        .send_each_iter::<Counter>()
        .recv_each_iter::<Counter>()
        .finish();
    parent.add_resource(LastSeenPeer::default());

    parent.add_update_system(tick_subapp("local", 1), Schedule::TickLocal);
    parent.add_update_system(export_local_to_peer, Schedule::Export);
    parent.add_update_system(tick_subapp("peer", 1), Schedule::TickPeer);
    parent.add_update_system(import_peer, Schedule::Import);

    parent.prepare();
    for _ in 0..n_iters {
        parent.run();
    }
    let last = parent.get_resource_ref::<LastSeenPeer>().unwrap();
    last.0
}

#[test]
fn typed_multires_reads_remote_peer_through_explicit_pump() {
    const N: usize = 5;
    let (server_t, client_t) = LocalTransport::pair();
    let a = thread::spawn(move || run_binary(server_t, 1, N));
    let b = thread::spawn(move || run_binary(client_t, 10, N));
    let a_seen = a.join().unwrap();
    let b_seen = b.join().unwrap();

    // Send-first/recv-second symmetry: each side's iter-k recv lands what the
    // peer exported on iter k, so after N iters each typed reader converges to
    // the peer's local-at-iter-N. Identical to the untyped `Multi` result in
    // multi_phase3 — the typed handles add types, not behavior.
    assert_eq!(
        a_seen,
        N as u64 * 10,
        "A sees B's final counter via MultiRes"
    );
    assert_eq!(b_seen, N as u64, "B sees A's final counter via MultiRes");
}

#[test]
fn typed_multires_stale_read_fails_closed_without_synchronization() {
    // A mirror with a receive slot that is never pumped. "peer" is never ticked. The
    // local side advances every iter; if `MultiRes::retrieve` synchronized it
    // would have to observe that. It cannot — it borrows only the mirror's
    // local cell, which nothing wrote — so the typed reader stays at default.
    let (_unused_peer, mirror_t) = LocalTransport::pair();
    let mut parent = App::new();
    parent.add_subapp("local", build_local(7));
    parent
        .add_remote_subapp("peer", mirror_t)
        .recv_each_iter::<Counter>()
        .finish();
    parent.add_resource(LastSeenPeer::default());
    parent.add_update_system(tick_subapp("local", 1), Schedule::TickLocal);
    parent.add_update_system(import_peer, Schedule::Import);

    parent.prepare();
    let panic = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        parent.run();
    })) {
        Ok(_) => panic!("an unpumped receive mirror must reject stale reads"),
        Err(panic) => panic,
    };
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        message.contains("stale read of"),
        "unexpected panic: {message}"
    );
    // The unused transport peer never sends. Reaching this assertion proves
    // retrieval failed from local metadata instead of attempting a recv.
}
