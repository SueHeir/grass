# grass_gpu

The GPU layer shared by libraries built on GRASS. It is to the GPU what
[`grass_mpi`](../grass_mpi/) is to MPI: the framework owns the handle on an external
resource and knows nothing about what is computed with it. SOIL, FIELD and physics
tiers share one device, so their buffers can go in the same kernel.

The scheduler does not change. A device is an ordinary resource, kernels launch from
ordinary systems, and choosing CPU or GPU is choosing which plugins to add. Nothing
depends on this crate unless it asks to.

## Surface

| item | what it does |
|---|---|
| [`GpuDevice`](src/device.rs) | a wgpu device and queue, cached per adapter per process, so everything on one adapter can share buffers |
| [`GpuPlugin`](src/plugin.rs) | installs the device once per App, and the `CoherenceRegistry`. Addable repeatedly, like `MpiPlugin` |
| [`DeviceSelection`](src/device.rs) | which adapter a rank or sub-App uses |
| [`DualBuffer`](src/buffer.rs) | one column with a CPU copy and a GPU copy; transfers only when the other side is newer |
| [`TransferCounts`](src/counts.rs) | every transfer and queue drain, so a test can assert that a run stays on the GPU |

## Which GPU

By default, one rank per GPU: each rank takes its position on its node, modulo the
number of adapters. `GRASS_GPU_DEVICE=<index>` overrides that for job scripts, and when
the scheduler exposes one GPU per task (`srun --gpus-per-task=1`) that one is the only
choice anyway.

Each sub-App can choose for itself, and GRASS assumes neither layout:

```rust,ignore
// Every GPU runs both solvers on its own patch of space. Drag stays on one device.
fluid.add_plugins(GpuPlugin::default());
solid.add_plugins(GpuPlugin::default());

// Each solver gets its own GPUs, sized to its cost. Coupling data crosses devices.
fluid.add_plugins(GpuPlugin::on_adapter(0));
solid.add_plugins(GpuPlugin::on_adapter(1));
```

Sub-Apps that pick the same adapter get the same device. Add the choice before any
library plugin; asking for a different adapter after one is installed panics. So does
asking for a GPU on a machine with none, because a silent fallback to the CPU would be
an order of magnitude slower without saying so.

## Which copy is current

There are two levels, and they compose.

**Per column,** `DualBuffer` follows the Kokkos `DualView` model, except that only one
side can be newer: writing a side first brings it up to date, so data cannot be lost by
writing both between syncs.

| before launching a kernel that... | call |
|---|---|
| reads the column | `gpu_read` |
| reads and writes it | `gpu_write` |
| replaces every element, reading none | `gpu_overwrite` |

The CPU side has `host`, `host_mut` and `host_overwrite`. A chain of kernels transfers
nothing, and a CPU reader downloads only the column it asks for.

**Per resource,** `grass_scheduler::CoherenceRegistry` covers CPU systems that have never
heard of a GPU. A plugin registers a `MirrorBridge` naming the ordinary resource those
systems read, and calls `mark_device_dirty` after its kernels run. The scheduler then
downloads before any system touching that resource, counts it, and prints which system
forced it. A system that takes `ResMut<CoherenceRegistry>` manages its own coherence and
is left alone. `tests/end_to_end.rs` has a complete example.

## What this does not do yet

- **f64 on Apple GPUs.** They have none, and it is optional elsewhere. Assume `f32` unless
  `GpuDevice::supports_f64` says otherwise, and compare to CPU runs within the `f32` band.
- **CUDA and HIP.** wgpu is the laptop and development backend. Cluster compute nodes
  often have no Vulkan drivers, so those will need their own backend.
- **GPU-aware MPI.** `CommBackend` takes CPU slices, so a halo downloads the boundary,
  exchanges, and uploads the ghosts with `DualBuffer::upload_range`.
- **Mapping on the GPU.** `grass_map` and `Transport` work in CPU memory. Coupling two
  sub-Apps through `grass_multi` works unchanged, with one transfer per exchange.

## Build

```sh
cargo test -p grass_gpu
cargo run -p grass_gpu --features mpi_backend --example which_gpu   # or under mpirun
```

Tests that need a GPU skip with a note when there is no adapter. CI runs them on Mesa's
software Vulkan driver.
