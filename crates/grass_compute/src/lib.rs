//! Compute-device abstraction for the grass simulation framework.
//!
//! Stage 1 of the CubeCL migration. This crate is to CubeCL what `grass_mpi`
//! is to MPI: the framework layer owns the handle on an external execution
//! resource, and knows nothing about what is computed with it. No physics
//! lives here, and none is meant to — the physics tiers are SOIL and DIRT,
//! which sit above.
//!
//! It provides four things:
//!
//! 1. [`ComputeDevice<R>`] — a runtime/device resource, generic over the
//!    CubeCL [`Runtime`](cubecl::prelude::Runtime), registerable on an
//!    [`App`](grass_app::App) via [`ComputeDevicePlugin`].
//! 2. [`DeviceBuffer<R, E>`] — a typed, length-carrying handle on a CubeCL
//!    allocation, so kernel arguments can be built safely.
//! 3. Explicit transfer entry points —
//!    [`to_device`](ComputeDevice::to_device),
//!    [`from_device`](ComputeDevice::from_device),
//!    [`from_device_into`](ComputeDevice::from_device_into), and
//!    [`barrier`](ComputeDevice::barrier). Nothing crosses implicitly, and
//!    every crossing is counted ([`TransferCounts`]).
//! 4. [`ComputeSyncSet`] — named schedule sets saying *where in a step* host
//!    and device state must be coherent, so a later physics tier can order
//!    against them without this crate knowing what it is ordering.
//!
//! # The `cubecl` feature is off by default
//!
//! With no features, this crate compiles to the vocabulary only —
//! [`ComputeSyncSet`], [`TransferCounts`], [`TransferSnapshot`] — and pulls in
//! no CubeCL code at all. The existing CPU path of the whole stack builds and
//! tests exactly as before. Turning the feature on adds the device, the buffer,
//! the plugin and the sample kernel; it replaces nothing.
//!
//! # No performance claim is made anywhere in this crate
//!
//! elizabeth-hpc has no discrete GPU. `cubecl-cpu` is the only runtime that can
//! be executed here, and it exists so that a kernel can be shown to be
//! *correct* against the equivalent Rust — see
//! `tests/elementwise_equivalence.rs`. Whether any of this is faster than the
//! scalar path is unmeasured and unmeasurable on this machine.
//!
//! # Ordering
//!
//! [`ComputeSyncSet`] is an ordinary `ScheduleSet` enum, so it shares the
//! scheduler's namespace footgun: every phase enum defaults to namespace `0`,
//! and two enums at namespace `0` interleave silently. An application that
//! mixes these sets with a solver's own phases must separate them with
//! [`grass_scheduler::chain_namespaces!`] or an explicit
//! [`Schedule`](grass_scheduler::Schedule).

#![warn(missing_docs)]
// With the `cubecl` feature off, the doc links below point at items that are
// not compiled. The prose is still the right description of the crate.
#![cfg_attr(not(feature = "cubecl"), allow(rustdoc::broken_intra_doc_links))]

mod sync;
pub use sync::{ComputeSyncSet, TransferCounts, TransferSnapshot};

#[cfg(feature = "cubecl")]
mod buffer;
#[cfg(feature = "cubecl")]
mod device;
#[cfg(feature = "cubecl")]
mod kernel;
#[cfg(feature = "cubecl")]
mod plugin;

#[cfg(feature = "cubecl")]
pub use buffer::DeviceBuffer;
#[cfg(feature = "cubecl")]
pub use device::{ComputeDevice, ComputeError};
#[cfg(feature = "cubecl")]
pub use kernel::{elementwise_add, elementwise_add_host, kernels};
#[cfg(feature = "cubecl")]
pub use plugin::{device_barrier, ComputeDevicePlugin, COMPUTE_DEVICE};

/// The `grass_compute` prelude.
///
/// Imports what an application needs to wire a device onto an `App` and move
/// data across the boundary. It does **not** re-export CubeCL: a crate that
/// writes kernels depends on `cubecl` itself and names that contract
/// explicitly, the same way a crate that implements `grass_mpi::CommBackend`
/// has to import the trait by hand.
pub mod prelude {
    pub use crate::{ComputeSyncSet, TransferCounts, TransferSnapshot};

    #[cfg(feature = "cubecl")]
    pub use crate::{
        ComputeDevice, ComputeDevicePlugin, ComputeError, DeviceBuffer, COMPUTE_DEVICE,
    };
}
