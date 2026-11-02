//! [`GpuDevice`]: a device and queue, one per adapter per process.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use crate::counts::TransferCounts;

/// Environment variable that overrides [`DeviceSelection::Auto`] with an adapter
/// index, for job scripts that bind GPUs themselves.
pub const DEVICE_ENV_VAR: &str = "GRASS_GPU_DEVICE";

/// Which adapter a rank or sub-App uses.
///
/// Sub-Apps that select the same adapter share one [`GpuDevice`], so their buffers
/// can go in the same kernel. Sub-Apps that select different adapters run on
/// different GPUs. GRASS assumes neither layout.
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

/// Why a device could not be created.
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
    /// The adapter refused to create a device.
    RequestDevice(String),
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
            GpuError::RequestDevice(why) => write!(f, "could not create GPU device: {why}"),
        }
    }
}

impl std::error::Error for GpuError {}

/// A GPU device and queue, shared by everything in the process that picks the same
/// adapter. `Clone` is cheap.
///
/// Buffers belong to the device that made them. Two `wgpu::Device`s on one physical
/// GPU cannot bind each other's buffers, so devices are cached per adapter: asking
/// for adapter 0 twice returns the same device, and a SOIL column and a FIELD column
/// made in different sub-Apps can still go in one kernel.
#[derive(Clone)]
pub struct GpuDevice {
    /// The wgpu device.
    pub device: wgpu::Device,
    /// The wgpu queue.
    pub queue: wgpu::Queue,
    adapter_index: usize,
    adapter_name: String,
    supports_f64: bool,
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

/// Adapters of the first backend that has any hardware, software adapters last.
///
/// One backend only, because a GPU exposed through both Vulkan and GL would otherwise
/// count twice and `rank % n` would put two ranks on it. The stable sort keeps
/// enumeration order within a device type, so every rank on a node sees one numbering.
fn adapters() -> Vec<wgpu::Adapter> {
    let instance = wgpu::Instance::default();
    for backend in [
        wgpu::Backends::METAL,
        wgpu::Backends::VULKAN,
        wgpu::Backends::DX12,
    ] {
        let mut found = pollster::block_on(instance.enumerate_adapters(backend));
        if !found.is_empty() {
            found.sort_by_key(|a| a.get_info().device_type == wgpu::DeviceType::Cpu);
            return found;
        }
    }
    Vec::new()
}

fn cache() -> &'static Mutex<HashMap<usize, GpuDevice>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, GpuDevice>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

impl GpuDevice {
    /// The names of this machine's adapters, in index order.
    pub fn adapter_names() -> Vec<String> {
        adapters().iter().map(|a| a.get_info().name).collect()
    }

    /// The device for `selection`, where `node_rank` is this rank's position on its
    /// node. Creates it the first time and returns the same device after that.
    pub fn select(selection: DeviceSelection, node_rank: usize) -> Result<Self, GpuError> {
        let found = adapters();
        let env = std::env::var(DEVICE_ENV_VAR).ok();
        let index = selection.resolve(node_rank, found.len(), env.as_deref())?;
        let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(device) = cache.get(&index) {
            return Ok(device.clone());
        }
        let device = Self::create(&found[index], index)?;
        cache.insert(index, device.clone());
        Ok(device)
    }

    /// The device on the default adapter, for tests and single-rank programs.
    pub fn default_device() -> Result<Self, GpuError> {
        Self::select(DeviceSelection::Auto, 0)
    }

    fn create(adapter: &wgpu::Adapter, adapter_index: usize) -> Result<Self, GpuError> {
        let info = adapter.get_info();
        let supports_f64 = adapter.features().contains(wgpu::Features::SHADER_F64);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("grass_gpu device"),
            required_features: if supports_f64 {
                wgpu::Features::SHADER_F64
            } else {
                wgpu::Features::empty()
            },
            // Particle kernels bind many storage buffers at once, well past the
            // conservative default limit of 8.
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| GpuError::RequestDevice(e.to_string()))?;
        Ok(Self {
            device,
            queue,
            adapter_index,
            adapter_name: format!("{} ({:?})", info.name, info.backend),
            supports_f64,
            counts: Arc::default(),
        })
    }

    /// This adapter's index in [`adapter_names`](Self::adapter_names).
    pub fn adapter_index(&self) -> usize {
        self.adapter_index
    }

    /// The adapter's name and backend, for logs.
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Whether kernels on this device may use `f64`.
    pub fn supports_f64(&self) -> bool {
        self.supports_f64
    }

    /// Every transfer made through this device so far, shared by all its users.
    pub fn counts(&self) -> &TransferCounts {
        &self.counts
    }

    /// Compiles a compute pipeline from WGSL, with its layout taken from the shader.
    pub fn compute_pipeline(
        &self,
        label: &str,
        wgsl: &str,
        entry_point: &str,
    ) -> wgpu::ComputePipeline {
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(wgsl.into()),
            });
        self.device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: None,
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
    }

    /// Records one compute pass with `record` and submits it, without waiting.
    pub fn submit_compute(&self, label: &str, record: impl FnOnce(&mut wgpu::ComputePass<'_>)) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: None,
            });
            record(&mut pass);
        }
        self.queue.submit(Some(encoder.finish()));
    }

    /// Blocks until everything submitted so far has finished. Counted, because a wait
    /// inside a timestep is usually the thing a profile is looking for.
    pub fn wait(&self) {
        self.counts.record_wait();
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("grass_gpu: device lost while waiting for the queue");
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
