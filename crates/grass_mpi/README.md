# grass_mpi

Shared MPI lifecycle and communicator layer for libraries built on GRASS. It
provides a `CommBackend` trait with a no-op single-process backend and an
optional real-MPI backend behind the `mpi_backend` feature.

`grass_mpi` does not parallelize a solver or make the GRASS scheduler concurrent.
It lets multiple libraries share one launched MPI world without independently
owning MPI initialization and finalization, and can split that world into
solver-local communicators. Each library above GRASS still owns its parallel
algorithm; for example, SOIL owns particle decomposition, migration, and halo
exchange.

## Surface

| item | what it does |
|---|---|
| [`CommBackend`](src/lib.rs) | trait abstracting MPI: rank/size, processor decomposition, allreduce sum/min, point-to-point f64 send/recv, sendrecv |
| [`CommResource`](src/lib.rs) | `Box<dyn CommBackend>` wrapped for use as a `Res<CommResource>` in systems |
| [`SingleProcessComm`](src/lib.rs) | no-op backend; rank=0, size=1, send/recv unreachable. Always available. |
| [`MpiCommBackend`](src/lib.rs) | real MPI backend via [`mpi`](https://crates.io/crates/mpi) crate; behind the `mpi_backend` feature |
| [`get_mpi_world`](src/lib.rs) | returns this app's communicator: the color-split intra-comm if [`init_app_color`](src/lib.rs) was called (MPMD bootstrap), otherwise raw `MPI_COMM_WORLD` |
| [`get_mpi_world_raw`](src/lib.rs) | always returns raw `MPI_COMM_WORLD`, even after a color split — for MPMD couplings that address peers in other binaries by absolute world rank |
| [`init_app_color`](src/lib.rs) | MPMD bootstrap: split `MPI_COMM_WORLD` by `color`; same-color repeats are no-ops, different colors or calls after `get_mpi_world` are rejected |
| [`try_init_app_color`](src/lib.rs) | fallible form of `init_app_color` for callers that want to handle lifecycle violations |
| [`finalize_mpi`](src/lib.rs) | drop the MPI universe; call after all `Comm` resources have been dropped |
| [`world_rank`](src/lib.rs) / [`world_size`](src/lib.rs) | this rank / total ranks in raw `MPI_COMM_WORLD` |

## Build

```sh
# Without MPI (uses SingleProcessComm only):
cargo build

# With real MPI:
cargo build --features mpi_backend
```

## See also

- [`grass_multi::MpiInterCommTransport`](../grass_multi/src/transport.rs)
  — uses `get_mpi_world_raw()` to address peers in MPMD launches.

## License

MIT OR Apache-2.0
