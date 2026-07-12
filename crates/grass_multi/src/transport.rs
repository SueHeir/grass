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
//! `try_send(&[u8])` ships one opaque payload; `try_recv() -> Vec<u8>`
//! blocks until the peer ships one. Framing, ordering, and serialization
//! are the caller's problem — `RemoteMirrorPhysics` handles them via
//! [`Wire`](crate::Wire) impls on each registered resource type. A
//! transport just shuffles bytes.
//!
//! The infallible `send` / `recv` helpers remain for simple call sites, but
//! they panic with the same diagnostic returned by the fallible API. Remote
//! coupling code should use `try_send` / `try_recv` so disconnects can be
//! reported with sub-App, pump phase, and direction context.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::{any, fmt};

/// Transport operation that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportOperation {
    /// Sending one payload to the peer failed.
    Send,
    /// Receiving one payload from the peer failed.
    Recv,
}

impl fmt::Display for TransportOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Send => f.write_str("send"),
            Self::Recv => f.write_str("recv"),
        }
    }
}

/// Actionable error from a byte transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    transport: String,
    operation: TransportOperation,
    detail: String,
}

impl TransportError {
    /// Construct a transport error from a named transport, failed operation,
    /// and implementation-specific detail.
    pub fn new(
        transport: impl Into<String>,
        operation: TransportOperation,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            transport: transport.into(),
            operation,
            detail: detail.into(),
        }
    }

    /// Human-readable transport name.
    pub fn transport(&self) -> &str {
        &self.transport
    }

    /// Operation that failed.
    pub fn operation(&self) -> TransportOperation {
        self.operation
    }

    /// Implementation-specific failure detail.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} failed: {}",
            self.transport, self.operation, self.detail
        )
    }
}

impl std::error::Error for TransportError {}

/// A bidirectional byte channel between this process and a peer.
pub trait Transport: Send + Sync + 'static {
    /// Human-readable transport name for diagnostics.
    fn transport_name(&self) -> &str {
        any::type_name::<Self>()
    }

    /// Send one payload to the peer.
    fn try_send(&self, payload: &[u8]) -> Result<(), TransportError>;

    /// Block until the peer sends a payload; return its bytes.
    fn try_recv(&self) -> Result<Vec<u8>, TransportError>;

    /// Send one payload to the peer, panicking with transport context on
    /// disconnect.
    fn send(&self, payload: &[u8]) {
        self.try_send(payload).unwrap_or_else(|err| panic!("{err}"));
    }

    /// Block until the peer sends a payload, panicking with transport context
    /// on disconnect.
    fn recv(&self) -> Vec<u8> {
        self.try_recv().unwrap_or_else(|err| panic!("{err}"))
    }
}

impl Transport for Box<dyn Transport> {
    fn transport_name(&self) -> &str {
        (**self).transport_name()
    }

    fn try_send(&self, payload: &[u8]) -> Result<(), TransportError> {
        (**self).try_send(payload)
    }

    fn try_recv(&self) -> Result<Vec<u8>, TransportError> {
        (**self).try_recv()
    }
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
    fn transport_name(&self) -> &str {
        "LocalTransport"
    }

    fn try_send(&self, payload: &[u8]) -> Result<(), TransportError> {
        let tx = self.outgoing.lock().unwrap();
        tx.send(payload.to_vec()).map_err(|_| {
            TransportError::new(
                self.transport_name(),
                TransportOperation::Send,
                "peer dropped before it could receive the payload",
            )
        })
    }

    fn try_recv(&self) -> Result<Vec<u8>, TransportError> {
        let rx = self.incoming.lock().unwrap();
        rx.recv().map_err(|_| {
            TransportError::new(
                self.transport_name(),
                TransportOperation::Recv,
                "peer dropped before sending a payload",
            )
        })
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
    fn transport_name(&self) -> &str {
        "MpiInterCommTransport"
    }

    fn try_send(&self, payload: &[u8]) -> Result<(), TransportError> {
        use mpi::topology::Communicator;
        use mpi::traits::Destination;
        let process = self.world.process_at_rank(self.peer_rank);
        process.send(payload);
        Ok(())
    }

    fn try_recv(&self) -> Result<Vec<u8>, TransportError> {
        use mpi::topology::Communicator;
        use mpi::traits::Source;
        let process = self.world.process_at_rank(self.peer_rank);
        let (data, _status) = process.receive_vec::<u8>();
        Ok(data)
    }
}
