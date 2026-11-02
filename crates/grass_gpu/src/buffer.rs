//! [`DualBuffer`]: one column with a CPU copy and a GPU copy.

use cubecl::bytes::Bytes;
use cubecl::prelude::*;
use cubecl::server::Handle;

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
/// # Kernels
///
/// Say what each kernel does just before launching it, with [`gpu_read`](Self::gpu_read),
/// [`gpu_write`](Self::gpu_write), or [`gpu_overwrite`](Self::gpu_overwrite). Each returns
/// the argument to pass to the kernel's launch.
pub struct DualBuffer<T: CubeElement + Copy> {
    device: GpuDevice,
    host: Vec<T>,
    gpu: Handle,
    current: Current,
}

fn byte_len<T>(n: usize) -> usize {
    // A kernel argument cannot be empty, so an empty column still owns one element.
    n.max(1) * std::mem::size_of::<T>()
}

impl<T: CubeElement + Copy> DualBuffer<T> {
    /// A column holding `host` on the CPU, not yet uploaded.
    pub fn new(device: &GpuDevice, host: Vec<T>) -> Self {
        let gpu = device.client.empty(byte_len::<T>(host.len()));
        Self {
            device: device.clone(),
            host,
            gpu,
            current: Current::Cpu,
        }
    }

    /// A column of `n` copies of `value`, not yet uploaded.
    pub fn filled(device: &GpuDevice, n: usize, value: T) -> Self {
        Self::new(device, vec![value; n])
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

    fn arg(&self) -> BufferArg {
        // SAFETY: the handle was allocated for this many elements of this type.
        unsafe { BufferArg::from_raw_parts(self.gpu.clone(), self.host.len().max(1)) }
    }

    /// A kernel is about to read this column: uploads if the CPU copy is newer.
    pub fn gpu_read(&mut self) -> BufferArg {
        self.sync_to_gpu();
        self.arg()
    }

    /// A kernel is about to read and write this column: uploads if the CPU copy is
    /// newer, then marks the GPU copy as the current one.
    pub fn gpu_write(&mut self) -> BufferArg {
        self.sync_to_gpu();
        self.current = Current::Gpu;
        self.arg()
    }

    /// A kernel is about to replace every element without reading any: skips the
    /// upload, discards pending CPU writes, and marks the GPU copy as current.
    pub fn gpu_overwrite(&mut self) -> BufferArg {
        self.current = Current::Gpu;
        self.arg()
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

    /// The CPU copy, or `None` if the GPU copy is newer. For code that holds only a
    /// shared reference and syncs earlier, such as a halo pack.
    pub fn host_if_current(&self) -> Option<&[T]> {
        (self.current != Current::Gpu).then_some(self.host.as_slice())
    }

    /// Makes both copies agree by uploading, if the CPU copy is newer.
    pub fn sync_to_gpu(&mut self) {
        if self.current == Current::Cpu {
            if !self.host.is_empty() {
                let bytes = T::as_bytes(&self.host);
                self.device
                    .client
                    .write(&self.gpu, Bytes::from_bytes_vec(bytes.to_vec()));
                self.device.counts().record_upload(bytes.len() as u64);
            }
            self.current = Current::Both;
        }
    }

    /// Makes both copies agree by downloading, if the GPU copy is newer. Blocks until
    /// every kernel already launched has finished.
    pub fn sync_to_host(&mut self) {
        if self.current == Current::Gpu {
            Self::sync_all_to_host(&mut [self]);
        }
    }

    /// Makes both copies agree by downloading, for every column whose GPU copy is newer,
    /// with one wait for the lot. Each separate download is a full round trip to the GPU,
    /// so columns needed at the same point should come down together.
    pub fn sync_all_to_host(columns: &mut [&mut Self]) {
        let mut stale: Vec<&mut Self> = columns
            .iter_mut()
            .filter(|c| c.current == Current::Gpu && !c.host.is_empty())
            .map(|c| &mut **c)
            .collect();
        if let Some(first) = stale.first() {
            let device = first.device.clone();
            device.counts().record_batch();
            let handles = stale.iter().map(|c| c.gpu.clone()).collect();
            let data = device
                .client
                .read(handles)
                .expect("grass_gpu: could not read a column");
            for (column, bytes) in stale.iter_mut().zip(&data) {
                let n = column.host.len();
                column.host.copy_from_slice(&T::from_bytes(bytes)[..n]);
                device
                    .counts()
                    .record_download(std::mem::size_of_val(column.host.as_slice()) as u64);
            }
        }
        for column in columns.iter_mut() {
            if column.current == Current::Gpu {
                column.current = Current::Both;
            }
        }
    }

    /// Uploads only `start..start + values.len()`, for a part of the column that the
    /// CPU owns while the rest stays on the GPU (ghosts after a halo exchange). The CPU
    /// copy of that range is updated too, and the rest of the column is left as it is.
    pub fn upload_range(&mut self, start: usize, values: &[T]) {
        if values.is_empty() {
            return;
        }
        self.host[start..start + values.len()].copy_from_slice(values);
        let size = std::mem::size_of::<T>();
        let total = self.gpu.size_in_used();
        let bytes = T::as_bytes(values);
        let range = self
            .gpu
            .clone()
            .offset_start((start * size) as u64)
            .offset_end(total - ((start * size) as u64 + bytes.len() as u64));
        self.device
            .client
            .write(&range, Bytes::from_bytes_vec(bytes.to_vec()));
        self.device.counts().record_upload(bytes.len() as u64);
    }

    /// Changes the length, keeping the first elements of the current copy and filling
    /// new ones with `fill`. The GPU copy is stale afterward.
    pub fn resize(&mut self, n: usize, fill: T) {
        self.sync_to_host();
        self.host.resize(n, fill);
        self.current = Current::Cpu;
        self.gpu = self.device.client.empty(byte_len::<T>(n));
    }

    /// Changes the length for a column about to be replaced: no download, and the CPU
    /// copy becomes current.
    pub fn resize_overwrite(&mut self, n: usize, fill: T) {
        self.current = Current::Cpu;
        self.resize(n, fill);
    }
}
