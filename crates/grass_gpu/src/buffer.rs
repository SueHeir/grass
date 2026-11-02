//! [`DualBuffer`]: one column with a CPU copy and a GPU copy.

use bytemuck::Pod;

use crate::device::GpuDevice;

/// Which copy of a [`DualBuffer`] holds the latest data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Current {
    /// Both copies agree.
    Both,
    /// The CPU copy is newer; the GPU copy is stale.
    Cpu,
    /// The GPU copy is newer; the CPU copy is stale.
    Gpu,
}

/// One column mirrored on the CPU and the GPU, transferred only when needed.
///
/// This is the Kokkos `DualView` model with one change: only one side can be newer.
/// Asking to write one side while the other is newer first brings it up to date, so
/// data cannot be lost by writing both sides between syncs. A chain of kernels makes
/// no transfers. A CPU reader downloads this column once, and no other column.
///
/// # Kernels and bind groups
///
/// Building a bind group from [`raw`](Self::raw) touches no state. Say what each kernel
/// does just before launching it: [`gpu_read`](Self::gpu_read),
/// [`gpu_write`](Self::gpu_write), or [`gpu_overwrite`](Self::gpu_overwrite).
/// [`resize`](Self::resize) replaces the buffer when it grows, and bumps
/// [`generation`](Self::generation), so hold on to the generation to know when a
/// bind group must be rebuilt.
pub struct DualBuffer<T: Pod> {
    device: GpuDevice,
    usage: wgpu::BufferUsages,
    label: &'static str,
    host: Vec<T>,
    gpu: wgpu::Buffer,
    staging: Option<wgpu::Buffer>,
    current: Current,
    generation: u64,
}

fn byte_len<T>(n: usize) -> u64 {
    // A zero-sized binding is an error, so an empty column still owns one element.
    (n.max(1) * std::mem::size_of::<T>()) as u64
}

impl<T: Pod> DualBuffer<T> {
    /// A column holding `host` on the CPU, not yet uploaded. `usage` is added to the
    /// copy usages; use `STORAGE` for a kernel column.
    pub fn new(
        device: &GpuDevice,
        label: &'static str,
        usage: wgpu::BufferUsages,
        host: Vec<T>,
    ) -> Self {
        assert!(
            std::mem::size_of::<T>().is_multiple_of(4),
            "grass_gpu: GPU copies move in 4-byte units, so a column element must be too"
        );
        let usage = usage | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
        let gpu = Self::allocate(device, label, usage, host.len());
        Self {
            device: device.clone(),
            usage,
            label,
            host,
            gpu,
            staging: None,
            current: Current::Cpu,
            generation: 0,
        }
    }

    /// A zeroed storage column of `n` elements, with both copies agreeing.
    pub fn zeroed(device: &GpuDevice, label: &'static str, n: usize) -> Self {
        let mut column = Self::new(
            device,
            label,
            wgpu::BufferUsages::STORAGE,
            vec![T::zeroed(); n],
        );
        // wgpu zero-initializes buffers, so there is nothing to upload.
        column.current = Current::Both;
        column
    }

    fn allocate(
        device: &GpuDevice,
        label: &str,
        usage: wgpu::BufferUsages,
        n: usize,
    ) -> wgpu::Buffer {
        device.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: byte_len::<T>(n),
            usage,
            mapped_at_creation: false,
        })
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.host.len()
    }

    /// `true` when the column has no elements.
    pub fn is_empty(&self) -> bool {
        self.host.is_empty()
    }

    /// Which copy is current.
    pub fn current(&self) -> Current {
        self.current
    }

    /// Bumped whenever the GPU buffer is replaced, which invalidates bind groups.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The GPU buffer, for building bind groups. Touches no state.
    pub fn raw(&self) -> &wgpu::Buffer {
        &self.gpu
    }

    /// A kernel is about to read this column: uploads if the CPU copy is newer.
    pub fn gpu_read(&mut self) -> &wgpu::Buffer {
        self.sync_to_gpu();
        &self.gpu
    }

    /// A kernel is about to read and write this column: uploads if the CPU copy is
    /// newer, then marks the GPU copy as the current one.
    pub fn gpu_write(&mut self) -> &wgpu::Buffer {
        self.sync_to_gpu();
        self.current = Current::Gpu;
        &self.gpu
    }

    /// A kernel is about to replace every element without reading any: skips the
    /// upload, discards pending CPU writes, and marks the GPU copy as current.
    pub fn gpu_overwrite(&mut self) -> &wgpu::Buffer {
        self.current = Current::Gpu;
        &self.gpu
    }

    /// The CPU copy for reading: downloads if the GPU copy is newer.
    pub fn host(&mut self) -> &[T] {
        self.sync_to_host();
        &self.host
    }

    /// The CPU copy for changing some elements: downloads if the GPU copy is newer,
    /// then marks the CPU copy as the current one.
    pub fn host_mut(&mut self) -> &mut [T] {
        self.sync_to_host();
        self.current = Current::Cpu;
        &mut self.host
    }

    /// The CPU copy for replacing every element: skips the download, discards pending
    /// GPU writes, and marks the CPU copy as current.
    pub fn host_overwrite(&mut self) -> &mut [T] {
        self.current = Current::Cpu;
        &mut self.host
    }

    /// Makes both copies agree by uploading, if the CPU copy is newer.
    pub fn sync_to_gpu(&mut self) {
        if self.current == Current::Cpu {
            self.device
                .queue
                .write_buffer(&self.gpu, 0, bytemuck::cast_slice(&self.host));
            self.device
                .counts()
                .record_upload(std::mem::size_of_val(self.host.as_slice()) as u64);
            self.current = Current::Both;
        }
    }

    /// Makes both copies agree by downloading, if the GPU copy is newer. Blocks until
    /// every kernel already submitted has finished.
    pub fn sync_to_host(&mut self) {
        if self.current == Current::Gpu {
            self.download();
            self.current = Current::Both;
        }
    }

    /// Uploads only `start..start + values.len()`, for a part of the column that the
    /// CPU owns while the rest stays on the GPU (ghosts after a halo exchange). The CPU
    /// copy of that range is updated too, and the rest of the column is left as it is.
    pub fn upload_range(&mut self, start: usize, values: &[T]) {
        self.host[start..start + values.len()].copy_from_slice(values);
        self.device.queue.write_buffer(
            &self.gpu,
            (start * std::mem::size_of::<T>()) as u64,
            bytemuck::cast_slice(values),
        );
        self.device
            .counts()
            .record_upload(std::mem::size_of_val(values) as u64);
    }

    /// Changes the length, keeping the first elements of the current copy and filling
    /// new ones with zeros. The GPU copy is stale afterward. A larger column gets a new
    /// buffer, at least double the old one so that repeated growth stays cheap.
    pub fn resize(&mut self, n: usize) {
        self.sync_to_host();
        self.host.resize(n, T::zeroed());
        self.current = Current::Cpu;
        if byte_len::<T>(n) > self.gpu.size() {
            let doubled = self.gpu.size() as usize / std::mem::size_of::<T>() * 2;
            self.gpu = Self::allocate(&self.device, self.label, self.usage, n.max(doubled));
            self.staging = None;
            self.generation += 1;
        }
    }

    fn download(&mut self) {
        let bytes = std::mem::size_of_val(self.host.as_slice()) as u64;
        if bytes == 0 {
            return;
        }
        let size = self.gpu.size();
        let label = self.label;
        let device = &self.device;
        // Staging is allocated on first use: a column no CPU system reads never pays
        // for a second copy of itself in device memory.
        let staging = self.staging.get_or_insert_with(|| {
            device.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let mut encoder = device
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        encoder.copy_buffer_to_buffer(&self.gpu, 0, staging, 0, bytes);
        device.queue.submit(Some(encoder.finish()));
        let slice = staging.slice(0..bytes);
        slice.map_async(wgpu::MapMode::Read, |result| {
            result.expect("grass_gpu: could not map a staging buffer");
        });
        device
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("grass_gpu: device lost during a download");
        self.host
            .copy_from_slice(bytemuck::cast_slice(&slice.get_mapped_range()));
        staging.unmap();
        device.counts().record_download(bytes);
    }
}
