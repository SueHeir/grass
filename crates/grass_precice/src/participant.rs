//! [`PreciceParticipant`] — the resource wrapper around `precice::Participant`.
//!
//! Why a wrapper? `precice::Participant` is `!Send`/`!Sync` (cxx wraps a
//! C++ pointer), but `grass_app`'s resource store and `grass_multi`'s
//! `Coupler` trait both want `Send + Sync`. We hand-promise it via `unsafe
//! impl` because in practice each Participant is owned by one thread (the
//! main one) and accessed only via `borrow_mut()` through a `RefCell` — the
//! same way grass treats every other interior-mutable resource.

use std::cell::RefCell;

/// A `precice::Participant` made `Send + Sync` for resource storage.
///
/// Construct via [`Self::new`] and store in `App` resources or own from a
/// `Coupler`. All methods delegate to the underlying [`precice::Participant`].
pub struct PreciceParticipant {
    inner: RefCell<precice::Participant>,
    rank: i32,
    size: i32,
}

// SAFETY: each PreciceParticipant is owned by one thread (the orchestrator
// or App main thread). The RefCell ensures only one borrow at a time. preCICE
// itself is single-threaded per participant; we never hand the inner
// Participant to multiple threads.
unsafe impl Send for PreciceParticipant {}
unsafe impl Sync for PreciceParticipant {}

impl PreciceParticipant {
    /// Construct the underlying Participant. `rank` and `size` describe this
    /// participant's MPI position **within its own intra-communicator** (not
    /// the global MPI world). For a single-process participant pass `(0, 1)`.
    pub fn new(
        name: &str,
        config_path: &str,
        rank: i32,
        size: i32,
    ) -> Result<Self, precice::Error> {
        let p = precice::Participant::new(name, config_path, rank, size)?;
        Ok(Self {
            inner: RefCell::new(p),
            rank,
            size,
        })
    }

    pub fn rank(&self) -> i32 {
        self.rank
    }
    pub fn size(&self) -> i32 {
        self.size
    }

    /// Borrow the inner participant mutably. Most users go through this to
    /// call the rich preCICE API (set_mesh_vertices, write_data, read_data,
    /// requires_initial_data, etc.).
    pub fn borrow_mut(&self) -> std::cell::RefMut<'_, precice::Participant> {
        self.inner.borrow_mut()
    }

    /// Borrow the inner participant. Use for query methods like
    /// `is_coupling_ongoing` and `get_max_time_step_size` that take `&self`.
    pub fn borrow(&self) -> std::cell::Ref<'_, precice::Participant> {
        self.inner.borrow()
    }

    // ── Convenience wrappers around the most common calls ──────────────────

    pub fn initialize(&self) -> Result<(), precice::Error> {
        self.borrow_mut().initialize()
    }

    /// Advance preCICE by `dt`. **This blocks** until every coupled
    /// participant reaches the same simulation time.
    pub fn advance(&self, dt: f64) -> Result<(), precice::Error> {
        self.borrow_mut().advance(dt)
    }

    pub fn finalize(&self) -> Result<(), precice::Error> {
        self.borrow_mut().finalize()
    }

    pub fn is_coupling_ongoing(&self) -> Result<bool, precice::Error> {
        self.borrow().is_coupling_ongoing()
    }

    pub fn get_max_time_step_size(&self) -> Result<f64, precice::Error> {
        self.borrow().get_max_time_step_size()
    }

    pub fn requires_initial_data(&self) -> Result<bool, precice::Error> {
        self.borrow_mut().requires_initial_data()
    }
}
