//! [`GpuPlugin`]: the one place a GRASS application gets its GPU device.

use grass_app::prelude::*;
use grass_mpi::{CommResource, MpiPlugin};
use grass_scheduler::CoherenceRegistry;

use crate::device::{DeviceSelection, GpuDevice};

/// Installs the shared [`GpuDevice`] if no plugin has yet, and the
/// [`CoherenceRegistry`] that lets CPU systems read GPU-resident data.
///
/// Every library that runs kernels adds this plugin itself. The first one creates the
/// device and the rest reuse it, so libraries written separately share a device in one
/// App with no edits to either of them. To put a sub-App on a particular GPU, add
/// [`GpuPlugin::on_adapter`] before any library plugin, or install a device you already
/// hold with `app.init_shared_resource(|| device)`:
///
/// ```no_run
/// use grass_app::prelude::*;
/// use grass_gpu::GpuPlugin;
///
/// let mut fluid = App::new();
/// fluid.add_plugins(GpuPlugin::on_adapter(0));
/// let mut solid = App::new();
/// solid.add_plugins(GpuPlugin::on_adapter(1));
/// ```
///
/// Asking for a GPU on a machine without one panics at startup. A run that silently
/// fell back to the CPU would be a surprise an order of magnitude slower. A library
/// offering both paths can check [`GpuDevice::default_device`] and choose its plugins.
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuPlugin {
    selection: DeviceSelection,
}

impl GpuPlugin {
    /// The capability name this plugin provides.
    pub const CAPABILITY: &'static str = "gpu";

    /// Uses this adapter index instead of the one-GPU-per-rank default.
    pub fn on_adapter(index: usize) -> Self {
        Self {
            selection: DeviceSelection::Adapter(index),
        }
    }
}

impl Plugin for GpuPlugin {
    fn build(&self, app: &mut App) {
        if let DeviceSelection::Adapter(wanted) = self.selection {
            if let Some(existing) = app.get_resource_ref::<GpuDevice>() {
                assert_eq!(
                    existing.adapter_index(),
                    wanted,
                    "grass_gpu: this App already uses GPU adapter {}, and a later plugin asked \
                     for adapter {wanted}. Add GpuPlugin::on_adapter before any library plugin.",
                    existing.adapter_index(),
                );
            }
        }
        if !app.has_resource::<GpuDevice>() {
            // The communicator must exist first, or every rank would look like rank 0
            // and pile onto one GPU.
            app.add_plugins(MpiPlugin);
            let node_rank = app
                .get_resource_ref::<CommResource>()
                .map_or(0, |comm| comm.0.node_rank() as usize);
            let device = GpuDevice::select(self.selection, node_rank)
                .unwrap_or_else(|e| panic!("grass_gpu: {e}"));
            app.init_shared_resource(|| device);
        }
        if !app.has_resource::<CoherenceRegistry>() {
            app.add_resource(CoherenceRegistry::new());
        }
    }

    fn provides(&self) -> Vec<&str> {
        vec![Self::CAPABILITY]
    }

    /// Every library that needs the device adds this plugin itself, so it must be
    /// addable more than once. Only the first add creates anything.
    fn is_unique(&self) -> bool {
        false
    }
}
