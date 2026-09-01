//! [`ComputeDevice`] — the runtime/device resource and the two explicit
//! transfer entry points.

use cubecl::client::ComputeClient;
use cubecl::prelude::*;

use crate::buffer::DeviceBuffer;
use crate::sync::{TransferCounts, TransferSnapshot};

/// Something that went wrong at the compute boundary.
///
/// Deliberately small. Kernel-launch failures are CubeCL's own business and do
/// not pass through here; these are the errors this crate itself can detect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputeError {
    /// A host slice and a device buffer disagreed about their length.
    LengthMismatch {
        /// What the operation needed.
        expected: usize,
        /// What it was handed.
        actual: usize,
    },
    /// The runtime reported a failure while moving data.
    Transfer(String),
}

impl std::fmt::Display for ComputeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComputeError::LengthMismatch { expected, actual } => write!(
                f,
                "length mismatch: expected {expected} elements, got {actual}"
            ),
            ComputeError::Transfer(reason) => write!(f, "device transfer failed: {reason}"),
        }
    }
}

impl std::error::Error for ComputeError {}

/// The compute device an application runs kernels on, held as a scheduler
/// resource and read by systems as `Res<ComputeDevice<R>>`.
///
/// This is to CubeCL what `grass_mpi::CommResource` is to MPI: the framework
/// layer owns the handle on the outside world, and knows nothing about what is
/// computed with it. It is generic over the CubeCL [`Runtime`] rather than
/// boxed behind a trait object, because CubeCL's kernel-launch API is generic
/// over `R` at the call site — a `dyn` device could hold a client but could not
/// launch anything with it.
///
/// # Wiring
///
/// Applications do not construct one and pass it around; they register it as a
/// resource, normally through
/// [`ComputeDevicePlugin`](crate::ComputeDevicePlugin):
///
/// ```rust,ignore
/// use grass_compute::prelude::*;
/// use cubecl::cpu::CpuRuntime;
///
/// app.add_plugins(ComputeDevicePlugin::<CpuRuntime>::default_device());
/// ```
///
/// # Transfers are explicit
///
/// There is no implicit "make it current". [`to_device`](Self::to_device) and
/// [`from_device`](Self::from_device) are the only two ways data crosses, and
/// every crossing is counted (see [`transfers`](Self::transfers)). That is on
/// purpose: this stack is MPI-parallel, every ghost exchange needs host
/// pointers, and the number of round trips per step is the quantity that will
/// decide whether the migration was worth anything. It cannot be measured on a
/// box with no discrete GPU, so the least this layer can do is make it
/// countable.
pub struct ComputeDevice<R: Runtime> {
    device: R::Device,
    client: ComputeClient<R>,
    counts: TransferCounts,
}

impl<R: Runtime> ComputeDevice<R> {
    /// Acquires the client for `device`.
    pub fn new(device: R::Device) -> Self {
        let client = R::client(&device);
        Self {
            device,
            client,
            counts: TransferCounts::new(),
        }
    }

    /// Acquires the client for the runtime's default device.
    pub fn default_device() -> Self {
        Self::new(R::Device::default())
    }

    /// The CubeCL client, for launching kernels.
    pub fn client(&self) -> &ComputeClient<R> {
        &self.client
    }

    /// The device this client is bound to.
    pub fn device(&self) -> &R::Device {
        &self.device
    }

    /// The runtime's own name for this device (e.g. `"cpu"`), for logging.
    pub fn runtime_name(&self) -> &'static str {
        R::name(&self.client)
    }

    /// The largest 1-D cube dimension this device accepts, clamped to `max`.
    ///
    /// Kernels in this crate size their launch with it rather than hard-coding
    /// a width, because the CPU runtime reports `available_parallelism()` as
    /// its per-cube unit limit and a hard-coded 256 would be rejected there.
    pub fn cube_dim_1d(&self, max: u32) -> CubeDim {
        let units = self
            .client
            .properties()
            .hardware
            .max_units_per_cube
            .clamp(1, max.max(1));
        CubeDim::new_1d(units)
    }

    /// Reserves an uninitialised device buffer of `len` elements.
    ///
    /// No transfer happens, so nothing is counted.
    pub fn alloc<E: CubeElement>(&self, len: usize) -> DeviceBuffer<R, E> {
        let handle = self.client.empty(len * core::mem::size_of::<E>());
        // SAFETY: the allocation was just made at exactly `len` elements' worth
        // of bytes for `E`.
        unsafe { DeviceBuffer::from_raw_parts(handle, len) }
    }

    /// **Host → device.** Copies `host` into a fresh device buffer.
    ///
    /// One of the two points where data crosses; counted as one host→device
    /// transfer of `host.len() * size_of::<E>()` bytes.
    ///
    /// This allocates on every call. Writing into an *existing* device buffer
    /// in place is what a resident step loop actually wants and is deliberately
    /// not here yet: there is no caller for it until device-resident columns
    /// land in stage 2, and adding an unused second path now would be an
    /// untested one.
    pub fn to_device<E: CubeElement>(&self, host: &[E]) -> DeviceBuffer<R, E> {
        let handle = self.client.create_from_slice(E::as_bytes(host));
        self.counts
            .record_host_to_device(core::mem::size_of_val(host) as u64);
        // SAFETY: the allocation was created from exactly `host.len()` elements.
        unsafe { DeviceBuffer::from_raw_parts(handle, host.len()) }
    }

    /// **Device → host.** Reads `buffer` back into a fresh `Vec`.
    ///
    /// The other point where data crosses; counted as one device→host transfer.
    /// Blocks until the read completes, which also drains any kernel the buffer
    /// depends on.
    pub fn from_device<E: CubeElement>(
        &self,
        buffer: &DeviceBuffer<R, E>,
    ) -> Result<Vec<E>, ComputeError> {
        let bytes = self
            .client
            .read_one(buffer.handle().clone())
            .map_err(|e| ComputeError::Transfer(e.to_string()))?;
        let elems = E::from_bytes(&bytes);
        // A runtime may pad an allocation, so the read can be longer than the
        // data. `buffer.len()` is the element count, not the allocation size.
        if elems.len() < buffer.len() {
            return Err(ComputeError::LengthMismatch {
                expected: buffer.len(),
                actual: elems.len(),
            });
        }
        self.counts
            .record_device_to_host(buffer.size_bytes() as u64);
        Ok(elems[..buffer.len()].to_vec())
    }

    /// **Device → host**, into a caller-owned slice.
    ///
    /// Same transfer as [`from_device`](Self::from_device), without the `Vec`
    /// allocation. `host.len()` must equal `buffer.len()`.
    pub fn from_device_into<E: CubeElement>(
        &self,
        buffer: &DeviceBuffer<R, E>,
        host: &mut [E],
    ) -> Result<(), ComputeError> {
        if host.len() != buffer.len() {
            return Err(ComputeError::LengthMismatch {
                expected: buffer.len(),
                actual: host.len(),
            });
        }
        let bytes = self
            .client
            .read_one(buffer.handle().clone())
            .map_err(|e| ComputeError::Transfer(e.to_string()))?;
        let elems = E::from_bytes(&bytes);
        if elems.len() < buffer.len() {
            return Err(ComputeError::LengthMismatch {
                expected: buffer.len(),
                actual: elems.len(),
            });
        }
        host.clone_from_slice(&elems[..buffer.len()]);
        self.counts
            .record_device_to_host(buffer.size_bytes() as u64);
        Ok(())
    }

    /// Blocks until every operation submitted to this device has completed.
    ///
    /// This is the barrier a schedule puts at
    /// [`ComputeSyncSet::HostCoherent`](crate::ComputeSyncSet::HostCoherent):
    /// after it returns, the device queue is drained and an MPI call or a dump
    /// may proceed.
    pub fn barrier(&self) -> Result<(), ComputeError> {
        let result = cubecl::future::block_on(self.client.sync());
        self.counts.record_barrier();
        result.map_err(|e| ComputeError::Transfer(e.to_string()))
    }

    /// The running host↔device traffic tally.
    pub fn counts(&self) -> &TransferCounts {
        &self.counts
    }

    /// A snapshot of the traffic tally.
    pub fn transfers(&self) -> TransferSnapshot {
        self.counts.snapshot()
    }
}

impl<R: Runtime> core::fmt::Debug for ComputeDevice<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ComputeDevice")
            .field("device", &self.device)
            .field("transfers", &self.counts.snapshot())
            .finish()
    }
}
