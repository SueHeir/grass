//! [`GpuDevice`]: a CubeCL device and its client, shared by a process.

use std::fmt;
use std::sync::Arc;

use cubecl::client::Client;
use cubecl::ir::{ElemType, FloatKind};
use cubecl::{Device, RuntimeId};

use crate::counts::TransferCounts;

/// Environment variable that overrides [`DeviceSelection::Auto`] with an adapter
/// index, for job scripts that bind GPUs themselves.
pub const DEVICE_ENV_VAR: &str = "GRASS_GPU_DEVICE";

/// Which adapter a rank or sub-App uses.
///
/// Sub-Apps that select the same adapter share one device, so their buffers can go in
/// the same kernel. Sub-Apps that select different adapters run on different GPUs.
/// GRASS assumes neither layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceSelection {
    /// One GPU per rank: [`DEVICE_ENV_VAR`] if set, otherwise this rank's position
    /// within its node, modulo the number of adapters. When a job scheduler exposes
    /// one GPU per task, that one adapter is the only choice and this is index 0.
    #[default]
    Auto,
    /// This adapter index, from [`GpuDevice::adapter_names`].
    Adapter(usize),
}

impl DeviceSelection {
    /// The adapter index this selection means for `node_rank`, given `n_adapters`.
    fn resolve(
        self,
        node_rank: usize,
        n_adapters: usize,
        env: Option<&str>,
    ) -> Result<usize, GpuError> {
        if n_adapters == 0 {
            return Err(GpuError::NoAdapter);
        }
        let check = |index| {
            if index < n_adapters {
                Ok(index)
            } else {
                Err(GpuError::AdapterOutOfRange { index, n_adapters })
            }
        };
        match (self, env) {
            (DeviceSelection::Adapter(index), _) => check(index),
            (DeviceSelection::Auto, Some(value)) => check(
                value
                    .trim()
                    .parse()
                    .map_err(|_| GpuError::BadEnvVar(value.to_string()))?,
            ),
            (DeviceSelection::Auto, None) => Ok(node_rank % n_adapters),
        }
    }
}

/// Why a device could not be chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuError {
    /// This machine exposes no usable adapter.
    NoAdapter,
    /// An explicit adapter index is past the end of the adapter list.
    AdapterOutOfRange {
        /// The index that was asked for.
        index: usize,
        /// How many adapters exist.
        n_adapters: usize,
    },
    /// [`DEVICE_ENV_VAR`] is set to something that is not an adapter index.
    BadEnvVar(String),
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuError::NoAdapter => write!(f, "no GPU adapter available"),
            GpuError::AdapterOutOfRange { index, n_adapters } => {
                write!(
                    f,
                    "GPU adapter {index} requested, but only {n_adapters} exist"
                )
            }
            GpuError::BadEnvVar(value) => {
                write!(f, "{DEVICE_ENV_VAR}={value:?} is not an adapter index")
            }
        }
    }
}

impl std::error::Error for GpuError {}

/// A GPU device and its CubeCL client, shared by everything in the process that picks
/// the same adapter. `Clone` is cheap, and CubeCL hands out one server per device, so
/// a SOIL column and a FIELD column made in different sub-Apps can go in one kernel.
#[derive(Clone)]
pub struct GpuDevice {
    /// The CubeCL client. Kernels launch on this.
    pub client: Client,
    adapter_index: usize,
    adapter_name: String,
    counts: Arc<TransferCounts>,
}

impl fmt::Debug for GpuDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuDevice")
            .field("adapter_index", &self.adapter_index)
            .field("adapter_name", &self.adapter_name)
            .finish_non_exhaustive()
    }
}

/// GPU adapters this build can reach, CUDA first. One runtime only, because a GPU
/// reachable through two would count twice and `rank % n` would put two ranks on it.
fn adapters() -> Vec<Device> {
    let all = Device::enumerate_all();
    for runtime in [RuntimeId::Cuda, RuntimeId::Wgpu] {
        let found: Vec<Device> = all
            .iter()
            .filter(|d| d.runtime() == runtime)
            .cloned()
            .collect();
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

fn counts_for(index: usize) -> Arc<TransferCounts> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static COUNTS: OnceLock<Mutex<HashMap<usize, Arc<TransferCounts>>>> = OnceLock::new();
    COUNTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(index)
        .or_default()
        .clone()
}

impl GpuDevice {
    /// The names of this machine's adapters, in index order.
    pub fn adapter_names() -> Vec<String> {
        adapters().iter().map(|d| format!("{d:?}")).collect()
    }

    /// The device for `selection`, where `node_rank` is this rank's position on its node.
    pub fn select(selection: DeviceSelection, node_rank: usize) -> Result<Self, GpuError> {
        let found = adapters();
        let env = std::env::var(DEVICE_ENV_VAR).ok();
        let index = selection.resolve(node_rank, found.len(), env.as_deref())?;
        Ok(Self {
            client: found[index].client(),
            adapter_index: index,
            adapter_name: format!("{:?}", found[index]),
            counts: counts_for(index),
        })
    }

    /// The device on the default adapter, for tests and single-rank programs.
    pub fn default_device() -> Result<Self, GpuError> {
        Self::select(DeviceSelection::Auto, 0)
    }

    /// This adapter's index in [`adapter_names`](Self::adapter_names).
    pub fn adapter_index(&self) -> usize {
        self.adapter_index
    }

    /// The adapter's name and runtime, for logs.
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Whether kernels on this device may use `f64`.
    pub fn supports_f64(&self) -> bool {
        self.client
            .properties()
            .supports_type(ElemType::Float(FloatKind::F64))
    }

    /// Every transfer made through this device so far, shared by all its users.
    pub fn counts(&self) -> &TransferCounts {
        &self.counts
    }

    /// Blocks until everything submitted so far has finished. Counted, because a wait
    /// inside a timestep is usually the thing a profile is looking for.
    pub fn wait(&self) {
        self.counts.record_wait();
        cubecl::future::block_on(self.client.sync()).expect("grass_gpu: the device failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_spreads_ranks_over_adapters() {
        let auto = DeviceSelection::Auto;
        assert_eq!(auto.resolve(0, 4, None), Ok(0));
        assert_eq!(auto.resolve(3, 4, None), Ok(3));
        assert_eq!(auto.resolve(5, 4, None), Ok(1));
        // A scheduler that exposes one GPU per task: every rank takes that one.
        assert_eq!(auto.resolve(7, 1, None), Ok(0));
    }

    #[test]
    fn env_var_overrides_auto_but_not_an_explicit_choice() {
        assert_eq!(DeviceSelection::Auto.resolve(3, 4, Some("1")), Ok(1));
        assert_eq!(DeviceSelection::Adapter(2).resolve(3, 4, Some("1")), Ok(2));
    }

    #[test]
    fn bad_selections_are_errors() {
        let auto = DeviceSelection::Auto;
        assert_eq!(auto.resolve(0, 0, None), Err(GpuError::NoAdapter));
        assert_eq!(
            auto.resolve(0, 2, Some("x")),
            Err(GpuError::BadEnvVar("x".into()))
        );
        assert_eq!(
            DeviceSelection::Adapter(2).resolve(0, 2, None),
            Err(GpuError::AdapterOutOfRange {
                index: 2,
                n_adapters: 2
            })
        );
    }
}
