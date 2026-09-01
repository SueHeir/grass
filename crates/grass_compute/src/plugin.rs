//! [`ComputeDevicePlugin`] — registering a device on an [`App`].

use std::marker::PhantomData;

use cubecl::prelude::Runtime;
use grass_app::prelude::*;
use grass_scheduler::{CoherenceRegistry, Res};

use crate::device::ComputeDevice;
use crate::sync::ComputeSyncSet;

/// Capability tag announcing that some plugin has put a compute device on the
/// App. A physics tier that only works device-resident can require it.
pub const COMPUTE_DEVICE: CapabilityId = CapabilityId::new("compute_device");

/// Puts a [`ComputeDevice<R>`] on the App and installs the coherence barrier.
///
/// Mirrors `grass_mpi`'s division of labour: this crate owns the device handle
/// and the sync points; it registers no simulation systems and has no opinion
/// about what runs on the device.
///
/// What it does:
///
/// 1. adds `ComputeDevice<R>` as a resource, so systems can take
///    `Res<ComputeDevice<R>>`;
/// 2. adds a [`CoherenceRegistry`] if the App has none, so a later tier can
///    register host↔device mirrors and let the scheduler pull the device copy
///    back when a host system reads a stale resource;
/// 3. registers one update system at
///    [`ComputeSyncSet::HostCoherent`](crate::ComputeSyncSet::HostCoherent)
///    that drains the device queue.
///
/// It does **not** set a schedule namespace for [`ComputeSyncSet`]. Doing so
/// silently would fight with whatever ordering the application has chosen for
/// its own phase enums; see the ordering footgun on [`ComputeSyncSet`].
///
/// ```rust,ignore
/// use cubecl::cpu::CpuRuntime;
/// use grass_compute::prelude::*;
///
/// App::new()
///     .add_plugins(ComputeDevicePlugin::<CpuRuntime>::default_device())
///     .start();
/// ```
pub struct ComputeDevicePlugin<R: Runtime> {
    device: R::Device,
    _marker: PhantomData<fn() -> R>,
}

impl<R: Runtime> ComputeDevicePlugin<R> {
    /// Registers a specific device.
    pub fn new(device: R::Device) -> Self {
        Self {
            device,
            _marker: PhantomData,
        }
    }

    /// Registers the runtime's default device.
    pub fn default_device() -> Self {
        Self::new(R::Device::default())
    }
}

impl<R: Runtime> Default for ComputeDevicePlugin<R> {
    fn default() -> Self {
        Self::default_device()
    }
}

/// Drains the device queue so host and device agree from here on.
///
/// Registered at [`ComputeSyncSet::HostCoherent`](crate::ComputeSyncSet::HostCoherent).
/// A failure here means the device reported an error for work submitted
/// earlier in the step; there is nothing sensible to continue with, so it
/// panics rather than silently stepping on stale state.
pub fn device_barrier<R: Runtime>(device: Res<ComputeDevice<R>>) {
    device
        .barrier()
        .unwrap_or_else(|e| panic!("grass_compute: device barrier failed: {e}"));
}

impl<R: Runtime> Plugin for ComputeDevicePlugin<R> {
    fn build(&self, app: &mut App) {
        app.add_resource(ComputeDevice::<R>::new(self.device.clone()));
        if app.get_resource_ref::<CoherenceRegistry>().is_none() {
            app.add_resource(CoherenceRegistry::new());
        }
        app.add_update_system(device_barrier::<R>, ComputeSyncSet::HostCoherent);
    }

    fn provides_capabilities(&self) -> Vec<CapabilityId> {
        vec![COMPUTE_DEVICE]
    }

    fn schedule_labels(&self) -> Vec<&'static str> {
        vec!["grass_compute::device_barrier"]
    }
}
