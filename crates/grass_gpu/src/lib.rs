//! The GPU layer shared by every library built on GRASS.
//!
//! `grass_gpu` is to the GPU what [`grass_mpi`] is to MPI: the framework owns the
//! handle on an external resource, and knows nothing about what is computed with
//! it. SOIL, FIELD, and any physics tier share one [`GpuDevice`] so their buffers
//! can be bound into the same kernel. No physics lives here, and the scheduler
//! does not change: a device is an ordinary resource, and kernels are launched
//! from ordinary systems.
//!
//! # What it provides
//!
//! - [`GpuDevice`]: a device and queue, one per adapter per process.
//! - [`GpuPlugin`]: installs the device on an App, once, like `MpiPlugin`.
//! - [`DeviceSelection`]: which adapter this rank or sub-App uses.
//! - [`DualBuffer`]: one column with a CPU copy and a GPU copy, tracking which is
//!   current and transferring only when needed.
//! - [`TransferCounts`]: every crossing is counted, so "this run stays on the GPU"
//!   is a number a test can assert.
//!
//! # Which copy is current
//!
//! There are two levels, and they compose.
//!
//! 1. **Per column**, [`DualBuffer`] follows the Kokkos `DualView` model. Asking
//!    for the CPU slice downloads if the GPU copy is newer; asking for the GPU
//!    buffer uploads if the CPU copy is newer. A chain of kernels transfers nothing.
//! 2. **Per resource**, [`grass_scheduler::CoherenceRegistry`] (which [`GpuPlugin`]
//!    installs) covers CPU systems that know nothing about the GPU. A plugin
//!    registers a [`grass_scheduler::MirrorBridge`] naming the ordinary resource
//!    those systems read. After its kernels run it calls `mark_device_dirty`, and
//!    the scheduler then downloads before any system that touches that resource,
//!    counting the transfer and naming the system that forced it.
//!
//! Halo exchange goes through CPU memory for now, because
//! [`CommBackend`](grass_mpi::CommBackend) takes CPU slices. A halo is just another
//! sync point: download the boundary, exchange, upload the ghosts.
//!
//! # Precision
//!
//! Apple GPUs have no `f64`, and it is optional elsewhere, so kernels written for
//! this crate should assume `f32` unless [`GpuDevice::supports_f64`] says otherwise.
//! Compare results against a CPU run within the `f32` band, not to `f64` accuracy.
//!
//! # CPU and GPU kernels are separate code
//!
//! Nothing here tries to run one kernel on both. The fast CPU path and the fast GPU
//! path end up written differently in every large code that has tried otherwise, so
//! a library keeps its CPU systems and adds GPU systems beside them.

#![warn(missing_docs)]

mod buffer;
mod counts;
mod device;
mod plugin;

pub use buffer::{Current, DualBuffer};
pub use counts::{TransferCounts, TransferSnapshot};
pub use device::{DeviceSelection, GpuDevice, GpuError, DEVICE_ENV_VAR};
pub use plugin::GpuPlugin;

pub use bytemuck;
pub use wgpu;
