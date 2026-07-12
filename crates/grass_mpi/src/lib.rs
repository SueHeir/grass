//! Pure MPI abstraction layer.
//!
//! Provides [`CommBackend`] as the communication interface, [`CommResource`] as a
//! resource wrapper, and two backends:
//! - [`SingleProcessComm`]: no-op backend for serial runs
//! - [`MpiCommBackend`]: real MPI backend (behind the `mpi_backend` feature)
//!
//! # How to wire a backend
//!
//! Consumers don't construct a backend by hand and pass it around — they put a
//! `CommResource` into the scheduler so systems can take it as
//! `Res<CommResource>`. There is no `CommPlugin` in this crate; the wiring
//! lives in the consumer (e.g. a setup system in your `App`).
//!
//! **Parallel path** (with the `mpi_backend` feature):
//!
//! ```rust,ignore
//! use grass_mpi::*;
//!
//! // 1. (MPMD only) split MPI_COMM_WORLD once, before any get_mpi_world().
//! init_app_color(0);
//!
//! // 2. Grab this app's communicator (the color-split intra-comm if step 1 ran).
//! let world = get_mpi_world();
//!
//! // 3. Build the real backend and wrap it as a resource.
//! let comm = CommResource(Box::new(MpiCommBackend::new(world)));
//! scheduler.add_resource(comm); // now available as Res<CommResource>
//! ```
//!
//! **Serial path** (no feature, or a single-process run): use the no-op
//! backend — every collective is the identity and point-to-point is never
//! reached (see the [`SingleProcessComm`] contract below):
//!
//! ```rust,ignore
//! use grass_mpi::{CommResource, SingleProcessComm};
//! let comm = CommResource(Box::new(SingleProcessComm::new()));
//! scheduler.add_resource(comm);
//! ```
//!
//! # Lifecycle & ordering
//!
//! 1. **`init_app_color(color)` once, before the first `get_mpi_world()`.**
//!    It color-splits `MPI_COMM_WORLD` for MPMD launches
//!    (`mpirun -np N1 ./a : -np N2 ./b`); calling it after a backend already
//!    captured the world is too late and is rejected. Repeating the call with
//!    the same color is a no-op; repeating it with a different color is rejected.
//!    Skip it entirely for SPMD/single-binary runs.
//! 2. **Two communicator views:**
//!    - [`get_mpi_world`] returns the **color-split intra-comm** (this binary's
//!      own ranks) when `init_app_color` ran, else raw WORLD. This is what a
//!      backend should normally capture.
//!    - [`get_mpi_world_raw`] / [`world_rank`] / [`world_size`] always go to
//!      the **raw `MPI_COMM_WORLD`**, so MPMD couplers can address peers in
//!      *other* binaries by absolute world rank (this is what
//!      `grass_multi`'s transport uses).
//! 3. **`finalize_mpi()` after every `CommResource` has dropped** (i.e. after
//!    the last `App` is finished). It calls `MPI_Finalize`; using any comm
//!    afterward is undefined.
//!
//! ## The `unsafe impl Send/Sync` promise
//!
//! [`MpiCommBackend`] (and the internal intra-comm storage) carry hand-written
//! `unsafe impl Send for .. {}` / `Sync` so they can live in the scheduler's
//! resource table and a `static`. The soundness rests on a usage promise, not
//! on MPI's own thread-safety: **all MPI calls happen on a single thread**
//! (the simulation is single-threaded per rank). Do not share a
//! `CommResource` across OS threads.

#![warn(missing_docs)]

use std::ops::{Deref, DerefMut};

mod runtime;
#[cfg(feature = "mpi_backend")]
pub use runtime::{Bootstrap, MpiRuntime, MpiRuntimeError};
pub use runtime::{
    config_digest, BootstrapError, RoleAssignment, RoleConfig, RoleSpec, RoleTopology,
    RoleTopologyError, TopologyConfig, TopologyMode, TopologyPlan,
};

/// The `grass_mpi` application prelude.
///
/// Import this when wiring a communication resource into an application. A
/// custom communication backend implements [`CommBackend`] explicitly at the
/// crate root; it is intentionally not hidden behind a broader convenience
/// import.
///
/// The prelude does not put [`CommBackend`] in scope. This compile-fail example
/// is also a regression test for that boundary: an application that only needs
/// the serial resource can use the prelude, while a library that calls the
/// backend trait must name that contract explicitly.
///
/// ```compile_fail
/// use grass_mpi::prelude::*;
///
/// let comm = SingleProcessComm::new();
/// let _rank = comm.rank(); // `CommBackend` is not imported by the prelude.
/// ```
///
/// With the `mpi_backend` feature, the prelude includes the complete ordinary
/// application-wiring lifecycle: optionally split an MPMD application before
/// acquiring its communicator, build the resource, and finalize after the
/// resource is dropped. This `no_run` example is a feature-gated compile-pass
/// regression for that promise.
///
/// ```no_run
/// # #[cfg(feature = "mpi_backend")]
/// # {
/// use grass_mpi::prelude::*;
///
/// init_app_color(0);
/// let comm = CommResource(Box::new(MpiCommBackend::new(get_mpi_world())));
/// drop(comm);
/// finalize_mpi();
/// # }
/// ```
pub mod prelude {
    #[cfg(feature = "mpi_backend")]
    pub use crate::{
        finalize_mpi, get_mpi_world, init_app_color, Bootstrap, MpiCommBackend, MpiRuntime,
        MpiRuntimeError,
    };
    pub use crate::{
        config_digest, CommResource, RoleAssignment, RoleConfig, RoleSpec, RoleTopology,
        SingleProcessComm, TopologyConfig, TopologyMode, TopologyPlan,
    };
}

#[cfg(feature = "mpi_backend")]
use std::sync::Mutex;

#[cfg(feature = "mpi_backend")]
use mpi::collective::SystemOperation;
#[cfg(feature = "mpi_backend")]
use mpi::traits::{Communicator, CommunicatorCollectives, Destination, Source};

// ── Batched non-blocking sendrecv ────────────────────────────────────────────

/// One element of a batched non-blocking sendrecv (see
/// [`CommBackend::sendrecv_batch_f64_into`]). Each op sends `send_buf` to `dest`
/// while receiving from `source` into `recv_buf`. A `dest`/`source` of `-1`
/// disables that half (send-only or recv-only at a non-periodic boundary).
///
/// The caller must size `recv_buf` to the exact expected element count — like
/// [`CommBackend::sendrecv_f64_into`], no probe is performed. All ops in a batch
/// must be mutually independent (disjoint `recv_buf`s, and no send may depend on
/// another op's receive completing): the backend posts every send and receive
/// concurrently and only then waits on all of them.
pub struct SendRecvOp<'a> {
    /// Destination rank the send buffer is delivered to.
    pub dest: i32,
    /// Elements sent to `dest`.
    pub send_buf: &'a [f64],
    /// Source rank the receive buffer is filled from.
    pub source: i32,
    /// Pre-sized destination buffer for elements received from `source`.
    pub recv_buf: &'a mut [f64],
}

// ── CommBackend trait ────────────────────────────────────────────────────────

/// Abstraction over MPI or single-process communication.
///
/// # Serial-fallback contract
///
/// [`SingleProcessComm`] implements the collectives as the identity
/// (`all_reduce_*` return their input, `barrier` is a no-op) but leaves every
/// point-to-point method as `unreachable!`. That is deliberate, not a stub:
/// on one rank every neighbor *is* this rank, so callers must take their
/// `to_proc == rank` local-copy branch and never reach `send_f64` / `recv_f64`
/// / `sendrecv_f64*`. The lone exception is
/// [`sendrecv_batch_f64_into`](Self::sendrecv_batch_f64_into), which the serial
/// backend services directly by copying each op's send buffer into its recv
/// buffer (periodic self-exchange).
pub trait CommBackend: Send + Sync + 'static {
    /// This process's rank within the communicator (`0` for serial).
    fn rank(&self) -> i32;
    /// Number of ranks in the communicator (`1` for serial).
    fn size(&self) -> i32;
    /// Cartesian process-grid dimensions `[nx, ny, nz]` of the rank decomposition.
    fn processor_decomposition(&self) -> [i32; 3];
    /// This rank's `[ix, iy, iz]` coordinate within the process grid.
    fn processor_position(&self) -> [i32; 3];
    /// Record the process-grid shape and this rank's position in it.
    fn set_processor_grid(&mut self, decomp: [i32; 3], position: [i32; 3]);
    /// Sum `local` across all ranks and return the result to every rank.
    fn all_reduce_sum_f64(&self, local: f64) -> f64;
    /// Min of `local` across all ranks, returned to every rank (e.g. global dt).
    fn all_reduce_min_f64(&self, local: f64) -> f64;
    /// Block until every rank reaches this point.
    fn barrier(&self);

    // Point-to-point communication for borders/exchange/reverse_send_force
    /// Send `buf` to rank `dest`. `unreachable!` on the serial backend.
    fn send_f64(&self, dest: i32, buf: &[f64]);
    /// Receive a `Vec<f64>` from rank `source`. `unreachable!` on the serial backend.
    fn recv_f64(&self, source: i32) -> Vec<f64>;
    /// Receive a `Vec<f64>` from any rank. `unreachable!` on the serial backend.
    fn recv_f64_any(&self) -> Vec<f64>;
    /// Deadlock-free combined send-to-`dest` / receive-from-`source` (probes for
    /// the recv length, then allocates). `unreachable!` on the serial backend.
    fn sendrecv_f64(&self, dest: i32, send_buf: &[f64], source: i32) -> Vec<f64>;
    /// Deadlock-free sendrecv with a **known** receive length, into a caller-owned
    /// buffer. Avoids the `MPI_Probe` + per-call heap allocation that `sendrecv_f64`
    /// incurs: the caller resizes `recv_buf` to the exact expected element count and
    /// the message is received directly into it. Used by the per-step ghost
    /// forward/reverse comm, where `SwapData` already records the recv count.
    fn sendrecv_f64_into(&self, dest: i32, send_buf: &[f64], source: i32, recv_buf: &mut [f64]);
    /// Post a batch of non-blocking sendrecv ops and wait for all to complete.
    ///
    /// Each op's send (`Isend`) and receive (`Irecv`) are posted up front and
    /// all are in flight concurrently, so the latency of mutually-independent
    /// swaps overlaps instead of serializing one `sendrecv` at a time. This is
    /// the overlap counterpart to [`sendrecv_f64_into`](Self::sendrecv_f64_into):
    /// same probe-free, caller-sized `recv_buf` contract, applied to a whole
    /// round of swaps at once. The caller is responsible for batching only
    /// independent ops together (see [`SendRecvOp`]).
    fn sendrecv_batch_f64_into(&self, ops: &mut [SendRecvOp<'_>]);

    /// Like [`sendrecv_batch_f64_into`](Self::sendrecv_batch_f64_into) but runs
    /// `overlap` *while the swaps are in flight* — the interior/boundary overlap
    /// primitive (roadmap step 4): post every Isend/Irecv, run independent local
    /// work (e.g. computing forces on interior atoms that need no ghosts), then
    /// wait. `overlap` must not touch the ops' send/recv buffers. The default
    /// impl runs `overlap` then a blocking batch (correct, but no concurrency);
    /// the MPI backend overrides it to truly overlap.
    fn sendrecv_batch_overlap_f64_into(
        &self,
        ops: &mut [SendRecvOp<'_>],
        overlap: &mut dyn FnMut(),
    ) {
        overlap();
        self.sendrecv_batch_f64_into(ops);
    }
}

// ── CommResource ─────────────────────────────────────────────────────────────

/// Wraps a [`CommBackend`] implementation, used as `Res<CommResource>` in systems.
pub struct CommResource(pub Box<dyn CommBackend>);

impl Deref for CommResource {
    type Target = dyn CommBackend;
    fn deref(&self) -> &(dyn CommBackend + 'static) {
        &*self.0
    }
}

impl DerefMut for CommResource {
    fn deref_mut(&mut self) -> &mut (dyn CommBackend + 'static) {
        &mut *self.0
    }
}

// ── SingleProcessComm backend ────────────────────────────────────────────────

/// No-op communication backend for single-process simulations.
pub struct SingleProcessComm {
    processor_decomposition: [i32; 3],
    processor_position: [i32; 3],
}

impl Default for SingleProcessComm {
    fn default() -> Self {
        Self::new()
    }
}

impl SingleProcessComm {
    /// Creates a single-process (no-op) backend with a 1×1×1 decomposition.
    pub fn new() -> Self {
        SingleProcessComm {
            processor_decomposition: [1, 1, 1],
            processor_position: [0, 0, 0],
        }
    }
}

impl CommBackend for SingleProcessComm {
    fn rank(&self) -> i32 {
        0
    }
    fn size(&self) -> i32 {
        1
    }
    fn processor_decomposition(&self) -> [i32; 3] {
        self.processor_decomposition
    }
    fn processor_position(&self) -> [i32; 3] {
        self.processor_position
    }

    fn set_processor_grid(&mut self, decomp: [i32; 3], position: [i32; 3]) {
        self.processor_decomposition = decomp;
        self.processor_position = position;
    }

    fn all_reduce_sum_f64(&self, local: f64) -> f64 {
        local
    }
    fn all_reduce_min_f64(&self, local: f64) -> f64 {
        local
    }
    fn barrier(&self) {}

    // Single-process always hits the to_proc == rank (local copy) branch,
    // so actual send/recv is never called.
    fn send_f64(&self, _dest: i32, _buf: &[f64]) {
        unreachable!("SingleProcessComm::send_f64 should never be called");
    }
    fn recv_f64(&self, _source: i32) -> Vec<f64> {
        unreachable!("SingleProcessComm::recv_f64 should never be called");
    }
    fn recv_f64_any(&self) -> Vec<f64> {
        unreachable!("SingleProcessComm::recv_f64_any should never be called");
    }
    fn sendrecv_f64(&self, _dest: i32, _send_buf: &[f64], _source: i32) -> Vec<f64> {
        unreachable!("SingleProcessComm::sendrecv_f64 should never be called");
    }
    fn sendrecv_f64_into(
        &self,
        _dest: i32,
        _send_buf: &[f64],
        _source: i32,
        _recv_buf: &mut [f64],
    ) {
        unreachable!("SingleProcessComm::sendrecv_f64_into should never be called");
    }
    fn sendrecv_batch_f64_into(&self, ops: &mut [SendRecvOp<'_>]) {
        // Single process: every op is a self-exchange (periodic wrap onto the same
        // rank), so copy each op's send buffer into its recv buffer. min() guards
        // send-only / recv-only ops (to_proc == -1) whose buffers differ in length.
        for op in ops.iter_mut() {
            let n = op.send_buf.len().min(op.recv_buf.len());
            op.recv_buf[..n].copy_from_slice(&op.send_buf[..n]);
        }
    }
}

// ── MPI backend ──────────────────────────────────────────────────────────────

#[cfg(feature = "mpi_backend")]
static MPI_UNIVERSE: Mutex<Option<mpi::environment::Universe>> = Mutex::new(None);

/// MPMD intra-comm: when set, [`get_mpi_world`] returns this color-split
/// sub-communicator instead of the raw `MPI_COMM_WORLD`. Set once at
/// process startup via [`init_app_color`]. The bootstrap accessors
/// [`world_rank`] / [`world_size`] always go to raw WORLD regardless, so
/// MPMD couplers can still address peers by absolute world rank.
///
/// The wrapper makes `SimpleCommunicator` `Sync` for static storage. Same
/// hand-promise as `MpiCommBackend` below — single-threaded MPI use only.
#[cfg(feature = "mpi_backend")]
struct IntraComm {
    color: i32,
    comm: mpi::topology::SimpleCommunicator,
}

#[cfg(feature = "mpi_backend")]
unsafe impl Send for IntraComm {}
#[cfg(feature = "mpi_backend")]
unsafe impl Sync for IntraComm {}

#[cfg(feature = "mpi_backend")]
#[derive(Default)]
struct MpiLifecycle {
    intra: Option<IntraComm>,
    raw_world_fixed: bool,
}

#[cfg(feature = "mpi_backend")]
static MPI_LIFECYCLE: Mutex<MpiLifecycle> = Mutex::new(MpiLifecycle {
    intra: None,
    raw_world_fixed: false,
});

/// Error returned when [`try_init_app_color`] would violate the MPI bootstrap
/// lifecycle.
#[cfg(feature = "mpi_backend")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitAppColorError {
    /// `get_mpi_world()` already returned raw `MPI_COMM_WORLD`, so a later
    /// color split would disagree with the communicator already handed to the
    /// app.
    CommunicatorAlreadyFixed {
        /// The requested MPMD color.
        requested: i32,
    },
    /// The process was already initialized with another color.
    DifferentColor {
        /// The color already used to split `MPI_COMM_WORLD`.
        existing: i32,
        /// The newly requested color.
        requested: i32,
    },
}

#[cfg(feature = "mpi_backend")]
impl std::fmt::Display for InitAppColorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CommunicatorAlreadyFixed { requested } => write!(
                f,
                "init_app_color({requested}) must run before get_mpi_world(); \
                 raw MPI_COMM_WORLD has already been handed out"
            ),
            Self::DifferentColor {
                existing,
                requested,
            } => write!(
                f,
                "init_app_color({requested}) conflicts with the existing app color \
                 {existing}; repeated calls are only idempotent for the same color"
            ),
        }
    }
}

#[cfg(feature = "mpi_backend")]
impl std::error::Error for InitAppColorError {}

#[cfg(feature = "mpi_backend")]
fn world_from_universe_or_external_init(
    guard: &mut Option<mpi::environment::Universe>,
) -> mpi::topology::SimpleCommunicator {
    if guard.is_none() {
        if let Some(universe) = mpi::initialize() {
            *guard = Some(universe);
        } else {
            // MPI was initialized outside rsmpi (for example by a C library such
            // as p4est). In that case rsmpi cannot own a `Universe`, but it can
            // still wrap the live system communicator.
            return mpi::topology::SimpleCommunicator::world();
        }
    }
    guard.as_ref().unwrap().world()
}

/// Returns this app's communicator: the intra-comm registered by
/// [`init_app_color`] if MPMD-style bootstrap was performed, otherwise raw
/// `MPI_COMM_WORLD`. The code that builds the [`CommResource`] (typically a
/// backend-wiring setup system in your `App`; see the crate-level "How to
/// wire a backend" example) should call this so each binary in an MPMD launch
/// sees only its own subset of ranks.
#[cfg(feature = "mpi_backend")]
pub fn get_mpi_world() -> mpi::topology::SimpleCommunicator {
    let mut lifecycle = MPI_LIFECYCLE.lock().unwrap();
    if let Some(intra) = lifecycle.intra.as_ref() {
        // Each `SimpleCommunicator` owns and frees a user communicator. An MPI
        // duplicate gives the caller independent ownership while the lifecycle
        // retains the original split communicator.
        use mpi::topology::Communicator;
        return intra.comm.duplicate();
    }

    let mut guard = MPI_UNIVERSE.lock().unwrap();
    lifecycle.raw_world_fixed = true;
    world_from_universe_or_external_init(&mut guard)
}

/// Return the process-owned solver communicator as an MPI Fortran handle.
///
/// The handle remains valid until [`finalize_mpi`] and is intended for native
/// solver libraries that convert it back with `MPI_Comm_f2c`.
#[cfg(feature = "mpi_backend")]
pub fn get_mpi_world_fortran_handle() -> std::os::raw::c_int {
    use mpi::raw::AsRaw;

    let lifecycle = MPI_LIFECYCLE.lock().unwrap();
    if let Some(intra) = lifecycle.intra.as_ref() {
        return unsafe { mpi::ffi::RSMPI_Comm_c2f(intra.comm.as_raw()) };
    }
    drop(lifecycle);

    let mut guard = MPI_UNIVERSE.lock().unwrap();
    let world = world_from_universe_or_external_init(&mut guard);
    unsafe { mpi::ffi::RSMPI_Comm_c2f(world.as_raw()) }
}

/// Fallible form of [`init_app_color`].
///
/// This is idempotent only when called again with the same `color`. It returns
/// [`InitAppColorError::DifferentColor`] for a second, different color and
/// [`InitAppColorError::CommunicatorAlreadyFixed`] if [`get_mpi_world`] already
/// returned raw `MPI_COMM_WORLD`.
#[cfg(feature = "mpi_backend")]
pub fn try_init_app_color(color: i32) -> Result<(), InitAppColorError> {
    use mpi::topology::Communicator;
    let mut lifecycle = MPI_LIFECYCLE.lock().unwrap();
    if let Some(intra) = lifecycle.intra.as_ref() {
        return if intra.color == color {
            Ok(())
        } else {
            Err(InitAppColorError::DifferentColor {
                existing: intra.color,
                requested: color,
            })
        };
    }
    if lifecycle.raw_world_fixed {
        return Err(InitAppColorError::CommunicatorAlreadyFixed { requested: color });
    }

    let world = {
        let mut universe_guard = MPI_UNIVERSE.lock().unwrap();
        world_from_universe_or_external_init(&mut universe_guard)
    };
    let key = world.rank();
    let intra = world
        .split_by_color_with_key(
            mpi::topology::Color::with_value(color),
            key as mpi::topology::Key,
        )
        .expect("init_app_color: split_by_color returned no communicator (color undefined?)");
    lifecycle.intra = Some(IntraComm { color, comm: intra });
    Ok(())
}

/// MPMD bootstrap: split `MPI_COMM_WORLD` by `color` so each binary in a
/// `mpirun -np N1 ./a : -np N2 ./b` launch sees only its own intra-comm
/// from [`get_mpi_world`]. Each color value yields a disjoint sub-communicator
/// — by convention `color = 0` for the first binary, `1` for the second, etc.
///
/// Call **once**, **before** the first [`get_mpi_world`] (so the code that
/// builds the [`CommResource`] picks up the intra-comm). Idempotent if called
/// twice with the same color. Panics with an actionable message if called with a
/// different color, or after [`get_mpi_world`] already returned raw
/// `MPI_COMM_WORLD`. Use [`try_init_app_color`] to handle those violations.
#[cfg(feature = "mpi_backend")]
pub fn init_app_color(color: i32) {
    try_init_app_color(color).expect("init_app_color lifecycle violation");
}

/// Drop the MPI universe, calling MPI_Finalize. Must be called after all
/// `Comm` resources have been dropped (i.e. after the last `App` is done).
#[cfg(feature = "mpi_backend")]
pub fn finalize_mpi() {
    let mut lifecycle = MPI_LIFECYCLE.lock().unwrap();
    *lifecycle = MpiLifecycle::default();
    let mut guard = MPI_UNIVERSE.lock().unwrap();
    *guard = None;
}

/// Raw `MPI_COMM_WORLD` regardless of any [`init_app_color`] split — for
/// MPMD couplings that need to address peers in other binaries by absolute
/// world rank.
#[cfg(feature = "mpi_backend")]
pub fn get_mpi_world_raw() -> mpi::topology::SimpleCommunicator {
    let mut guard = MPI_UNIVERSE.lock().unwrap();
    world_from_universe_or_external_init(&mut guard)
}

/// This rank's absolute position in `MPI_COMM_WORLD` — always raw WORLD,
/// never the color-split intra-comm. For MPMD bootstrap code that needs to
/// address a peer in another binary by world rank.
#[cfg(feature = "mpi_backend")]
pub fn world_rank() -> i32 {
    use mpi::topology::Communicator;
    let mut guard = MPI_UNIVERSE.lock().unwrap();
    world_from_universe_or_external_init(&mut guard).rank()
}

/// Total ranks in `MPI_COMM_WORLD` (raw WORLD, not the intra-comm). See
/// [`world_rank`].
#[cfg(feature = "mpi_backend")]
pub fn world_size() -> i32 {
    use mpi::topology::Communicator;
    let mut guard = MPI_UNIVERSE.lock().unwrap();
    world_from_universe_or_external_init(&mut guard).size()
}

/// Return `true` iff every rank of raw `MPI_COMM_WORLD` passed the same
/// `value`. Implemented as a min/max all-reduce over raw WORLD, so it is a
/// collective: every rank must call it. Used by the bootstrap to verify that
/// a single-binary launch parsed an identical configuration on every rank.
#[cfg(feature = "mpi_backend")]
pub fn all_uniform_u64(value: u64) -> bool {
    let world = get_mpi_world_raw();
    let mut min = value;
    let mut max = value;
    world.all_reduce_into(&value, &mut min, SystemOperation::min());
    world.all_reduce_into(&value, &mut max, SystemOperation::max());
    min == max
}

#[cfg(not(feature = "mpi_backend"))]
/// No-op MPI finalizer used when the real MPI backend is disabled.
pub fn finalize_mpi() {}

#[cfg(feature = "mpi_backend")]
/// Real MPI backend wrapping an mpi `SimpleCommunicator` (behind the `mpi_backend` feature).
pub struct MpiCommBackend {
    world: mpi::topology::SimpleCommunicator,
    rank: i32,
    size: i32,
    processor_decomposition: [i32; 3],
    processor_position: [i32; 3],
}

#[cfg(feature = "mpi_backend")]
unsafe impl Send for MpiCommBackend {}
#[cfg(feature = "mpi_backend")]
unsafe impl Sync for MpiCommBackend {}

#[cfg(feature = "mpi_backend")]
impl MpiCommBackend {
    /// Wraps an existing MPI communicator, caching its rank and size.
    pub fn new(world: mpi::topology::SimpleCommunicator) -> Self {
        let rank = world.rank();
        let size = world.size();
        MpiCommBackend {
            world,
            rank,
            size,
            processor_decomposition: [0; 3],
            processor_position: [0; 3],
        }
    }
}

#[cfg(feature = "mpi_backend")]
impl CommBackend for MpiCommBackend {
    fn rank(&self) -> i32 {
        self.rank
    }
    fn size(&self) -> i32 {
        self.size
    }
    fn processor_decomposition(&self) -> [i32; 3] {
        self.processor_decomposition
    }
    fn processor_position(&self) -> [i32; 3] {
        self.processor_position
    }

    fn set_processor_grid(&mut self, decomp: [i32; 3], position: [i32; 3]) {
        self.processor_decomposition = decomp;
        self.processor_position = position;
    }

    fn all_reduce_sum_f64(&self, local: f64) -> f64 {
        let mut result = 0.0f64;
        self.world
            .all_reduce_into(&local, &mut result, SystemOperation::sum());
        result
    }

    fn all_reduce_min_f64(&self, local: f64) -> f64 {
        let mut result = 0.0f64;
        self.world
            .all_reduce_into(&local, &mut result, SystemOperation::min());
        result
    }

    fn barrier(&self) {
        self.world.barrier();
    }

    fn send_f64(&self, dest: i32, buf: &[f64]) {
        self.world.process_at_rank(dest).send(buf);
    }

    fn recv_f64(&self, source: i32) -> Vec<f64> {
        let (msg, _status) = self.world.process_at_rank(source).receive_vec::<f64>();
        msg
    }

    fn recv_f64_any(&self) -> Vec<f64> {
        let (msg, _status) = self.world.any_process().receive_vec::<f64>();
        msg
    }

    fn sendrecv_f64(&self, dest: i32, send_buf: &[f64], source: i32) -> Vec<f64> {
        // Non-blocking send + blocking recv: deadlock-free for any dest/source combination
        let world = &self.world;
        mpi::request::scope(|scope| {
            let sreq = world.process_at_rank(dest).immediate_send(scope, send_buf);
            let (msg, _status) = world.process_at_rank(source).receive_vec::<f64>();
            sreq.wait();
            msg
        })
    }

    fn sendrecv_f64_into(&self, dest: i32, send_buf: &[f64], source: i32, recv_buf: &mut [f64]) {
        // Probe-free, allocation-free counterpart to sendrecv_f64: the caller knows
        // the exact receive length and provides a correctly-sized buffer, so we skip
        // the MPI_Probe round-trip and receive directly. Deadlock-free via immediate_send.
        let world = &self.world;
        mpi::request::scope(|scope| {
            let sreq = world.process_at_rank(dest).immediate_send(scope, send_buf);
            world.process_at_rank(source).receive_into(recv_buf);
            sreq.wait();
        });
    }

    fn sendrecv_batch_f64_into(&self, ops: &mut [SendRecvOp<'_>]) {
        // Post every receive (Irecv) and send (Isend) before waiting on any of
        // them, so the latency of independent swaps overlaps. Receives are posted
        // first to give MPI a matching buffer ready when the sender's data lands,
        // avoiding unexpected-message buffering. A `dest`/`source` of -1 skips
        // that half (non-periodic boundary). Self-sends are handled by the caller
        // and never reach here.
        let world = &self.world;
        // Up to 2 requests (one send, one recv) per op.
        let max_reqs = ops.len() * 2;
        mpi::request::multiple_scope(max_reqs, |scope, coll| {
            for op in ops.iter_mut() {
                // Copy the scalar/shared-ref fields out before mutably borrowing
                // recv_buf, so the send and receive borrow disjoint state.
                let dest = op.dest;
                let source = op.source;
                let send_buf = op.send_buf;
                if source != -1 {
                    let rreq = world
                        .process_at_rank(source)
                        .immediate_receive_into(scope, &mut *op.recv_buf);
                    coll.add(rreq);
                }
                if dest != -1 {
                    let sreq = world.process_at_rank(dest).immediate_send(scope, send_buf);
                    coll.add(sreq);
                }
            }
            // Wait for all posted requests. `wait_all` drains the collection so
            // neither it nor the scope panics on drop.
            let mut completed = Vec::with_capacity(max_reqs);
            coll.wait_all(&mut completed);
        });
    }

    fn sendrecv_batch_overlap_f64_into(
        &self,
        ops: &mut [SendRecvOp<'_>],
        overlap: &mut dyn FnMut(),
    ) {
        // Post every Isend/Irecv, run the caller's independent local work while
        // the swaps are in flight, then wait. `overlap` touches caller state
        // (e.g. force arrays) disjoint from the ops' send/recv buffers.
        let world = &self.world;
        let max_reqs = ops.len() * 2;
        mpi::request::multiple_scope(max_reqs, |scope, coll| {
            for op in ops.iter_mut() {
                let dest = op.dest;
                let source = op.source;
                let send_buf = op.send_buf;
                if source != -1 {
                    let rreq = world
                        .process_at_rank(source)
                        .immediate_receive_into(scope, &mut *op.recv_buf);
                    coll.add(rreq);
                }
                if dest != -1 {
                    let sreq = world.process_at_rank(dest).immediate_send(scope, send_buf);
                    coll.add(sreq);
                }
            }
            // Swaps are in flight — do the caller's independent local work now.
            overlap();
            let mut completed = Vec::with_capacity(max_reqs);
            coll.wait_all(&mut completed);
        });
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_process_comm_rank_and_size() {
        let comm = SingleProcessComm::new();
        assert_eq!(comm.rank(), 0);
        assert_eq!(comm.size(), 1);
    }

    #[test]
    fn single_process_comm_reduce_identity() {
        let comm = SingleProcessComm::new();
        assert_eq!(comm.all_reduce_sum_f64(42.0), 42.0);
        assert_eq!(comm.all_reduce_min_f64(7.5), 7.5);
    }

    #[test]
    fn single_process_comm_set_grid() {
        let mut comm = SingleProcessComm::new();
        let decomp = [1, 1, 1];
        let pos = [0, 0, 0];
        comm.set_processor_grid(decomp, pos);
        assert_eq!(comm.processor_decomposition(), decomp);
        assert_eq!(comm.processor_position(), pos);
    }
}

// ── MPI multi-rank overlap-equivalence test ───────────────────────────────────
//
// Proves the real MPI backend's `sendrecv_batch_overlap_f64_into` is correct:
//   (1) the overlapped batch of Isend/Irecv delivers results BIT-IDENTICAL to
//       issuing the same swaps one at a time via `sendrecv_f64_into`, on the same
//       seeded buffers, and
//   (2) the caller's local work run in the in-flight window does not corrupt the
//       send/recv scratch buffers.
//
// `cargo test` runs the test binary as a single process, so `world.size()` would
// be 1 and no genuine inter-rank routing would be exercised. To get a real
// multi-rank layout the coordinating run re-launches THIS test binary under
// `mpirun -np N`, guarded by an env var so each spawned child runs the body once
// instead of re-spawning. Requires `mpirun` on PATH; if it is absent the test
// skips with a visible note (the `mpi_backend` build still compiles the check).
#[cfg(all(test, feature = "mpi_backend"))]
mod mpi_overlap_tests {
    use super::*;
    use std::process::Command;

    const CHILD_ENV: &str = "GRASS_MPI_OVERLAP_CHILD";
    const NRANKS: usize = 4; // >=3 so each rank's left/right neighbors are distinct
    const NPER: usize = 17; // f64 elements per swap buffer (odd, small)

    // The buffer a rank sends toward its +1 (right) neighbor …
    const TAG_PLUS: u64 = 1;
    // … and the buffer it sends toward its -1 (left) neighbor.
    const TAG_MINUS: u64 = 2;

    // Deterministic (rank, direction, index)-specific payload. Distinct bit
    // patterns per element so a mis-routed or corrupted value is detectable, with
    // a fractional part that makes bit-for-bit comparison meaningful. A pure
    // function of its inputs => an INDEPENDENT reference, not a self-consistent
    // echo of whatever the comm happened to move. Always finite / non-NaN.
    fn payload(rank: i32, tag: u64, i: usize) -> f64 {
        // splitmix64-style hash of (rank, tag, i) -> a [0,1) fraction.
        let mut z = (rank as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(tag.wrapping_mul(0xD1B5_4A32_D192_ED03))
            .wrapping_add(i as u64);
        z ^= z >> 30;
        z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z ^= z >> 27;
        let frac = (z >> 11) as f64 / (1u64 << 53) as f64; // [0,1)
        (rank as f64) * 1000.0 + (tag as f64) * 10.0 + i as f64 + frac
    }

    fn bits_eq(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
    }

    // The real multi-rank check, run once per rank under mpirun.
    fn run_body() {
        let world = get_mpi_world();
        let comm = MpiCommBackend::new(world);
        let rank = comm.rank();
        let size = comm.size();
        assert!(size >= 2, "overlap equivalence needs >=2 ranks, got {size}");

        let right = (rank + 1).rem_euclid(size);
        let left = (rank - 1).rem_euclid(size);

        // Seeded send buffers, held immutable across the whole swap.
        let send_plus: Vec<f64> = (0..NPER).map(|i| payload(rank, TAG_PLUS, i)).collect();
        let send_minus: Vec<f64> = (0..NPER).map(|i| payload(rank, TAG_MINUS, i)).collect();

        // ── Reference: issue the two swaps SERIALLY, one sendrecv at a time ──
        //   Op A: send TAG_PLUS  to right, recv from left.
        //   Op B: send TAG_MINUS to left,  recv from right.
        let mut ref_from_left = vec![0.0f64; NPER];
        let mut ref_from_right = vec![0.0f64; NPER];
        comm.sendrecv_f64_into(right, &send_plus, left, &mut ref_from_left);
        comm.sendrecv_f64_into(left, &send_minus, right, &mut ref_from_right);

        // ── Overlapped batch on freshly-seeded, byte-identical buffers ──
        let send_plus2: Vec<f64> = (0..NPER).map(|i| payload(rank, TAG_PLUS, i)).collect();
        let send_minus2: Vec<f64> = (0..NPER).map(|i| payload(rank, TAG_MINUS, i)).collect();
        assert!(
            bits_eq(&send_plus, &send_plus2) && bits_eq(&send_minus, &send_minus2),
            "rank {rank}: reseeded send buffers must be bit-identical to the originals"
        );

        let mut batch_from_left = vec![0.0f64; NPER];
        let mut batch_from_right = vec![0.0f64; NPER];

        // Independent local work performed WHILE the swaps are in flight. It
        // touches only `interior`, never the ops' send/recv scratch, and records
        // that it actually ran so we can prove the overlap fired.
        let mut interior = vec![0.0f64; 64];
        let mut overlap_ran = 0u32;
        {
            let mut ops = [
                SendRecvOp {
                    dest: right,
                    send_buf: &send_plus2,
                    source: left,
                    recv_buf: &mut batch_from_left,
                },
                SendRecvOp {
                    dest: left,
                    send_buf: &send_minus2,
                    source: right,
                    recv_buf: &mut batch_from_right,
                },
            ];
            let mut overlap = || {
                overlap_ran += 1;
                for (i, s) in interior.iter_mut().enumerate() {
                    *s = (i as f64).mul_add(1.5, rank as f64);
                }
            };
            comm.sendrecv_batch_overlap_f64_into(&mut ops, &mut overlap);
        }

        // (2) The overlap closure ran exactly once and its result is intact.
        assert_eq!(
            overlap_ran, 1,
            "rank {rank}: overlap closure must run exactly once"
        );
        for (i, s) in interior.iter().enumerate() {
            assert_eq!(
                s.to_bits(),
                (i as f64).mul_add(1.5, rank as f64).to_bits(),
                "rank {rank}: overlap local work corrupted at index {i}"
            );
        }

        // (2) The send scratch buffers were NOT mutated by the in-flight window.
        assert!(
            bits_eq(&send_plus2, &send_plus),
            "rank {rank}: send_plus scratch mutated during overlap"
        );
        assert!(
            bits_eq(&send_minus2, &send_minus),
            "rank {rank}: send_minus scratch mutated during overlap"
        );

        // (1) The overlapped recv buffers are BIT-IDENTICAL to the serial ones.
        assert!(
            bits_eq(&batch_from_left, &ref_from_left),
            "rank {rank}: overlap recv(from left) != serial sendrecv_f64_into reference"
        );
        assert!(
            bits_eq(&batch_from_right, &ref_from_right),
            "rank {rank}: overlap recv(from right) != serial sendrecv_f64_into reference"
        );

        // Independent analytic cross-check (guards against both paths sharing the
        // same routing bug): the +1 buffer we received from `left` must equal what
        // `left` sent toward its right (== us), and the -1 buffer from `right` must
        // equal what `right` sent toward its left (== us).
        let expect_from_left: Vec<f64> = (0..NPER).map(|i| payload(left, TAG_PLUS, i)).collect();
        let expect_from_right: Vec<f64> = (0..NPER).map(|i| payload(right, TAG_MINUS, i)).collect();
        assert!(
            bits_eq(&batch_from_left, &expect_from_left),
            "rank {rank}: from-left buffer routed/corrupted (expected left rank's TAG_PLUS payload)"
        );
        assert!(
            bits_eq(&batch_from_right, &expect_from_right),
            "rank {rank}: from-right buffer routed/corrupted (expected right rank's TAG_MINUS payload)"
        );

        comm.barrier();
        if rank == 0 {
            eprintln!(
                "grass_mpi overlap-equivalence PASS: {size} ranks, {NPER} elems/swap \u{2014} \
                 overlap batch == serial sendrecv (bit-identical), scratch intact, overlap fired"
            );
        }
        // Honor the finalize-after-comm-dropped contract: drop the backend's
        // communicator handle before MPI_Finalize.
        drop(comm);
        finalize_mpi();
    }

    #[test]
    fn batch_overlap_matches_serial_across_ranks() {
        // Child leg: we were re-launched under mpirun — run the real body once.
        if std::env::var_os(CHILD_ENV).is_some() {
            run_body();
            return;
        }
        // Coordinator leg: re-launch THIS test binary under `mpirun -np N`.
        if Command::new("mpirun").arg("--version").output().is_err() {
            eprintln!(
                "SKIP batch_overlap_matches_serial_across_ranks: `mpirun` not found on PATH \
                 (multi-rank overlap-equivalence check not exercised)"
            );
            return;
        }
        let exe = std::env::current_exe().expect("locate current test binary");
        let status = Command::new("mpirun")
            .args(["--oversubscribe", "-np", &NRANKS.to_string()])
            .arg(&exe)
            .args([
                "--exact",
                "mpi_overlap_tests::batch_overlap_matches_serial_across_ranks",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, "1")
            .status()
            .expect("spawn mpirun for multi-rank overlap-equivalence run");
        assert!(
            status.success(),
            "multi-rank overlap-equivalence run under mpirun failed: {status}"
        );
    }
}
