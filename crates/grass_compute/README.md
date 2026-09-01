# grass_compute

Compute-device abstraction for the grass framework. Stage 1 of the CubeCL
migration: it is to [CubeCL](https://github.com/tracel-ai/cubecl) what
[`grass_mpi`](../grass_mpi/) is to MPI — the framework layer owns the handle on
an external execution resource and knows nothing about what is computed with
it. No physics lives here.

Everything CubeCL-shaped is behind the `cubecl` cargo feature, **off by
default**. With the feature off the crate is the vocabulary only, and the rest
of the workspace builds and tests exactly as it did before this crate existed.

## Surface

| item | what it does | feature |
|---|---|---|
| [`ComputeSyncSet`](src/sync.rs) | named schedule sets saying where in a step host and device state must be coherent | always |
| [`TransferCounts`](src/sync.rs) / [`TransferSnapshot`](src/sync.rs) | running tally of host↔device transfers, bytes, and barriers | always |
| [`ComputeDevice<R>`](src/device.rs) | the runtime/device resource; holds the CubeCL client, read as `Res<ComputeDevice<R>>` | `cubecl` |
| [`ComputeDevicePlugin<R>`](src/plugin.rs) | registers the device, installs a `CoherenceRegistry`, adds the `HostCoherent` barrier system | `cubecl` |
| [`DeviceBuffer<R, E>`](src/buffer.rs) | typed, length-carrying handle on a CubeCL allocation; makes `BufferArg` construction safe | `cubecl` |
| [`ComputeError`](src/device.rs) | length mismatches and transfer failures | `cubecl` |
| [`kernels::elementwise_add_kernel`](src/kernel.rs) | the trivial proof kernel, `out[i] = lhs[i] + rhs[i]` | `cubecl` |
| [`elementwise_add_host`](src/kernel.rs) | the plain Rust loop the kernel is tested against | `cubecl` |

## The four sync points

`ComputeSyncSet` is the actual content of this stage. The kernels are the easy
part; the contract about *when* data is on the device is not.

| set | after it runs … |
|---|---|
| `HostToDevice` | the device copy reflects every host write made this step |
| `DeviceCompute` | the device copy is authoritative; the host copy is stale |
| `DeviceToHost` | device results have been pulled back |
| `HostCoherent` | host and device agree and the device queue is drained |

This stack is MPI-parallel, not thread-parallel, and MPI wants host pointers.
Every ghost/halo exchange, collective, and dump therefore belongs at or after
`HostCoherent`. A `forward_comm` placed in `DeviceCompute` reads a stale host
buffer — wrong answers, no crash, no diagnostic.

`ComputeSyncSet` is an ordinary `ScheduleSet` enum, so it inherits the
scheduler's namespace footgun: every phase enum defaults to namespace `0` and
two enums at namespace `0` interleave silently. The plugin deliberately does
**not** assign a namespace — an application that mixes these sets with a
solver's own phases separates them with `chain_namespaces!` or an explicit
`Schedule`.

## Transfers are explicit and counted

There is no implicit "make it current". `to_device`, `from_device`,
`from_device_into` and `barrier` are the only crossings, and each one is
tallied in `TransferCounts`. The migration plan asks that the number of
transfers per step be explicit and countable so that it can be measured the day
real hardware exists; this is that counter. It counts events and bytes, never
time.

## Runtimes and features

```sh
cargo build                             # feature off: no CubeCL at all
cargo test  -p grass_compute --features cubecl   # abstraction + cubecl-cpu
```

| feature | runtime | built here? |
|---|---|---|
| *(none)* | — | yes, and this is the default |
| `cubecl` | `cubecl-cpu` | yes — the only runtime this machine can execute |
| `cubecl-cuda` | `cubecl-cuda` | no: no CUDA on this box |
| `cubecl-hip` | `cubecl-hip` | no: no ROCm on this box |
| `cubecl-wgpu` | `cubecl-wgpu` | no |

CubeCL is pinned to exactly `0.11.0-pre.3`. It is a pre-release whose API moves
between pre-releases, so a bump is its own reviewed change rather than a side
effect of some other work.

### Why `cpu` is not its own feature

`cubecl` enables `cubecl/cpu` directly instead of leaving it to a separate
feature. The reason is that `cubecl-cpu` is the only runtime that can execute
on this machine, so a `cubecl` build without it is a build whose equivalence
test cannot run — and an unrunnable test is worse than no test. The cost is
that a consumer who only ever wants CUDA still compiles `cubecl-cpu` (which
pulls in a bundled LLVM). Splitting the two is a one-line change to
`Cargo.toml` the day such a consumer exists.

## No performance claim

There is no discrete GPU on the machine this was written on — an integrated
Radeon, no CUDA, no ROCm. `cubecl-cpu` is used here to show a kernel is
*correct* against the equivalent Rust loop, in `tests/elementwise_equivalence.rs`,
with a stated tolerance of exactly zero and a stated reason for it. Nothing in
this crate, its tests, or its commit history asserts that anything is faster.

## See also

- [`grass_mpi`](../grass_mpi/) — the same ownership pattern, for MPI.
- `grass_scheduler::CoherenceRegistry` — the scheduler-side hook that pulls a
  device copy back when a host system reads a stale mirror. The plugin installs
  one; registering mirrors into it is a physics tier's job.

## License

MIT OR Apache-2.0
