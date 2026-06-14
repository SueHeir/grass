# grass_mpi

Thin MPI abstraction layer used by [`grass_multi`](../grass_multi/) for
its `MpiInterCommTransport`. Provides a `CommBackend` trait with a
no-op single-process backend and an optional real-MPI backend behind
the `mpi_backend` feature.

## Surface

| item | what it does |
|---|---|
| [`CommBackend`](src/lib.rs) | trait abstracting MPI: rank/size, processor decomposition, allreduce sum/min, point-to-point f64 send/recv, sendrecv |
| [`CommResource`](src/lib.rs) | `Box<dyn CommBackend>` wrapped for use as a `Res<CommResource>` in systems |
| [`SingleProcessComm`](src/lib.rs) | no-op backend; rank=0, size=1, send/recv unreachable. Always available. |
| [`MpiCommBackend`](src/lib.rs) | real MPI backend via [`mpi`](https://crates.io/crates/mpi) crate; behind the `mpi_backend` feature |
| [`get_mpi_world`](src/lib.rs) | returns this app's communicator: the color-split intra-comm if [`init_app_color`](src/lib.rs) was called (MPMD bootstrap), otherwise raw `MPI_COMM_WORLD` |
| [`get_mpi_world_raw`](src/lib.rs) | always returns raw `MPI_COMM_WORLD`, even after a color split — for MPMD couplings that address peers in other binaries by absolute world rank |
| [`init_app_color`](src/lib.rs) | MPMD bootstrap: split `MPI_COMM_WORLD` by `color` so each binary in `mpirun -np N1 a : -np N2 b` sees only its own ranks |
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
- [`examples/coupling/explicit_mpmd_mpi/`](../../examples/coupling/explicit_mpmd_mpi/)
  — two-binary MPI coupling worked example.
