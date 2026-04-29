//! Cross-process [`Transport`] — a byte channel between two coupled
//! binaries.
//!
//! [`RemoteMirrorPhysics`](crate::RemoteMirrorPhysics) drives a `Transport`
//! to ship resource payloads to a peer process each iter. Two impls ship
//! in this crate:
//!
//!   - [`LocalTransport`] — paired in-memory `mpsc` channels, for tests
//!     that exercise the full register-pump-recv flow without spawning
//!     processes.
//!   - [`MpiInterCommTransport`] — point-to-point on `MPI_COMM_WORLD` via
//!     absolute rank; for MPMD launches like
//!     `mpirun -np 1 ./a : -np 1 ./b`. Behind the `mpi` feature.
//!
//! For other wires (TCP, ZeroMQ, shared memory), implement [`Transport`]
//! yourself and pass the impl to
//! [`MultiAppExt::add_remote_subapp`](crate::MultiAppExt::add_remote_subapp).
//!
//! ## Wire model
//!
//! `send(&[u8])` ships one opaque payload; `recv() -> Vec<u8>` blocks
//! until the peer ships one. Framing, ordering, and serialization are
//! the caller's problem — `RemoteMirrorPhysics` handles them via
//! [`Wire`](crate::Wire) impls on each registered resource type. A
//! transport just shuffles bytes.
//!
//! Errors aren't modeled — implementations may `panic!` on disconnect,
//! since recovering from a partner-process crash mid-coupling is
//! application-specific.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

/// A bidirectional byte channel between this process and a peer.
pub trait Transport: Send + Sync + 'static {
    /// Send one payload to the peer.
    fn send(&self, payload: &[u8]);
    /// Block until the peer sends a payload; return its bytes.
    fn recv(&self) -> Vec<u8>;
}

// ─── LocalTransport: in-memory bidirectional channel ─────────────────────────

/// Two ends of an in-memory transport, for testing without a real network.
///
/// `LocalTransport::pair()` returns `(server_side, client_side)`; the
/// two behave like a paired socket: `server.send(...)` is read by
/// `client.recv()` and vice versa.
pub struct LocalTransport {
    incoming: Mutex<Receiver<Vec<u8>>>,
    outgoing: Mutex<Sender<Vec<u8>>>,
}

impl LocalTransport {
    /// Returns a paired (server, client) transport.
    pub fn pair() -> (Self, Self) {
        let (s_to_c, c_from_s) = channel::<Vec<u8>>();
        let (c_to_s, s_from_c) = channel::<Vec<u8>>();
        let server = Self {
            incoming: Mutex::new(s_from_c),
            outgoing: Mutex::new(s_to_c),
        };
        let client = Self {
            incoming: Mutex::new(c_from_s),
            outgoing: Mutex::new(c_to_s),
        };
        (server, client)
    }
}

impl Transport for LocalTransport {
    fn send(&self, payload: &[u8]) {
        let tx = self.outgoing.lock().unwrap();
        tx.send(payload.to_vec())
            .expect("LocalTransport: peer dropped");
    }

    fn recv(&self) -> Vec<u8> {
        let rx = self.incoming.lock().unwrap();
        rx.recv().expect("LocalTransport: peer dropped before send")
    }
}

// ─── MpiInterCommTransport: MPMD launch, point-to-point on MPI_COMM_WORLD ──

/// MPMD-style coupling transport over MPI. Use when both binaries launch
/// via a single `mpirun -np N1 ./a : -np N2 ./b` so they share
/// `MPI_COMM_WORLD`. Each side constructs an `MpiInterCommTransport`
/// pointing at its peer's rank in the shared world. No true
/// `MPI_Intercomm_create` is used — rsmpi's intercomm support is uneven,
/// and for explicit coupling addressing the peer by absolute world rank
/// is enough.
///
/// MVP assumes single coupling rank pair (rank 0 of each app talks to
/// its counterpart). For multi-rank participants construct one transport
/// per coupling pair on each rank.
#[cfg(feature = "mpi")]
pub struct MpiInterCommTransport {
    world: mpi::topology::SimpleCommunicator,
    peer_rank: i32,
}

// SAFETY: rsmpi's SimpleCommunicator is `!Send`/`!Sync` because it wraps
// a raw `MPI_Comm` handle. In practice every grass App is
// single-threaded — MPI calls happen from the same thread the universe
// was initialized on, and `Transport`'s send/recv take `&self`, with the
// orchestrator holding the only reference.
#[cfg(feature = "mpi")]
unsafe impl Send for MpiInterCommTransport {}
#[cfg(feature = "mpi")]
unsafe impl Sync for MpiInterCommTransport {}

#[cfg(feature = "mpi")]
impl MpiInterCommTransport {
    /// Construct from the peer's **absolute** rank in `MPI_COMM_WORLD`.
    /// Uses `grass_mpi::get_mpi_world_raw` so the transport always
    /// addresses raw WORLD ranks even after `init_app_color` has split
    /// this binary's intra-comm out.
    pub fn new(peer_rank: i32) -> Self {
        let world = grass_mpi::get_mpi_world_raw();
        Self { world, peer_rank }
    }
}

#[cfg(feature = "mpi")]
impl Transport for MpiInterCommTransport {
    fn send(&self, payload: &[u8]) {
        use mpi::topology::Communicator;
        use mpi::traits::Destination;
        let process = self.world.process_at_rank(self.peer_rank);
        process.send(payload);
    }

    fn recv(&self) -> Vec<u8> {
        use mpi::topology::Communicator;
        use mpi::traits::Source;
        let process = self.world.process_at_rank(self.peer_rank);
        let (data, _status) = process.receive_vec::<u8>();
        data
    }
}
