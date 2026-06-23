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
//!    captured the world is too late. Skip it entirely for SPMD/single-binary
//!    runs.
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

use std::ops::{Deref, DerefMut};

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
    pub dest: i32,
    pub send_buf: &'a [f64],
    pub source: i32,
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
    fn sendrecv_f64_into(&self, _dest: i32, _send_buf: &[f64], _source: i32, _recv_buf: &mut [f64]) {
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
struct IntraComm(mpi::topology::SimpleCommunicator);

#[cfg(feature = "mpi_backend")]
unsafe impl Send for IntraComm {}
#[cfg(feature = "mpi_backend")]
unsafe impl Sync for IntraComm {}

#[cfg(feature = "mpi_backend")]
static MPI_INTRA: Mutex<Option<IntraComm>> = Mutex::new(None);

/// Returns this app's communicator: the intra-comm registered by
/// [`init_app_color`] if MPMD-style bootstrap was performed, otherwise raw
/// `MPI_COMM_WORLD`. The code that builds the [`CommResource`] (typically a
/// backend-wiring setup system in your `App`; see the crate-level "How to
/// wire a backend" example) should call this so each binary in an MPMD launch
/// sees only its own subset of ranks.
#[cfg(feature = "mpi_backend")]
pub fn get_mpi_world() -> mpi::topology::SimpleCommunicator {
    let mut guard = MPI_UNIVERSE.lock().unwrap();
    if guard.is_none() {
        *guard = Some(mpi::initialize().unwrap());
    }
    let universe = guard.as_ref().unwrap();

    // Hold the intra-comm guard separately to keep clean drop order.
    let intra_guard = MPI_INTRA.lock().unwrap();
    if let Some(intra) = intra_guard.as_ref() {
        // Clone the intra-comm handle for the caller. SimpleCommunicator's
        // CommunicatorHandle wraps an MPI_Comm raw handle that's safe to
        // duplicate; rsmpi handles the underlying refcount.
        use mpi::raw::AsRaw;
        let raw = intra.0.as_raw();
        return unsafe { mpi::raw::FromRaw::from_raw(raw) };
    }
    universe.world()
}

/// MPMD bootstrap: split `MPI_COMM_WORLD` by `color` so each binary in a
/// `mpirun -np N1 ./a : -np N2 ./b` launch sees only its own intra-comm
/// from [`get_mpi_world`]. Each color value yields a disjoint sub-communicator
/// — by convention `color = 0` for the first binary, `1` for the second, etc.
///
/// Call **once**, **before** the first [`get_mpi_world`] (so the code that
/// builds the [`CommResource`] picks up the intra-comm). Idempotent if called
/// twice with the same color.
#[cfg(feature = "mpi_backend")]
pub fn init_app_color(color: i32) {
    use mpi::topology::Communicator;
    let world = {
        let mut universe_guard = MPI_UNIVERSE.lock().unwrap();
        if universe_guard.is_none() {
            *universe_guard = Some(mpi::initialize().unwrap());
        }
        universe_guard.as_ref().unwrap().world()
    };
    let key = world.rank();
    let intra = world
        .split_by_color_with_key(
            mpi::topology::Color::with_value(color),
            key as mpi::topology::Key,
        )
        .expect("init_app_color: split_by_color returned no communicator (color undefined?)");
    let mut intra_guard = MPI_INTRA.lock().unwrap();
    *intra_guard = Some(IntraComm(intra));
}

/// Drop the MPI universe, calling MPI_Finalize. Must be called after all
/// `Comm` resources have been dropped (i.e. after the last `App` is done).
#[cfg(feature = "mpi_backend")]
pub fn finalize_mpi() {
    let mut guard = MPI_UNIVERSE.lock().unwrap();
    *guard = None;
}

/// Raw `MPI_COMM_WORLD` regardless of any [`init_app_color`] split — for
/// MPMD couplings that need to address peers in other binaries by absolute
/// world rank.
#[cfg(feature = "mpi_backend")]
pub fn get_mpi_world_raw() -> mpi::topology::SimpleCommunicator {
    let mut guard = MPI_UNIVERSE.lock().unwrap();
    if guard.is_none() {
        *guard = Some(mpi::initialize().unwrap());
    }
    guard.as_ref().unwrap().world()
}

/// This rank's absolute position in `MPI_COMM_WORLD` — always raw WORLD,
/// never the color-split intra-comm. For MPMD bootstrap code that needs to
/// address a peer in another binary by world rank.
#[cfg(feature = "mpi_backend")]
pub fn world_rank() -> i32 {
    use mpi::topology::Communicator;
    let mut guard = MPI_UNIVERSE.lock().unwrap();
    if guard.is_none() {
        *guard = Some(mpi::initialize().unwrap());
    }
    guard.as_ref().unwrap().world().rank()
}

/// Total ranks in `MPI_COMM_WORLD` (raw WORLD, not the intra-comm). See
/// [`world_rank`].
#[cfg(feature = "mpi_backend")]
pub fn world_size() -> i32 {
    use mpi::topology::Communicator;
    let mut guard = MPI_UNIVERSE.lock().unwrap();
    if guard.is_none() {
        *guard = Some(mpi::initialize().unwrap());
    }
    guard.as_ref().unwrap().world().size()
}

#[cfg(not(feature = "mpi_backend"))]
pub fn finalize_mpi() {}

#[cfg(feature = "mpi_backend")]
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
