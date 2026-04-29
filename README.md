# GRASS — General Rust App System Scheduler

Bevy-style App + Plugin framework for scientific simulations, plus a
small toolkit for coupling several App-shaped solvers together
(in-process or across MPI binaries).

## Crates

| crate | role |
|---|---|
| [`grass_app`](crates/grass_app/) | `App` / `Plugin` / `PluginGroup` — top-level container |
| [`grass_scheduler`](crates/grass_scheduler/) | typed-resource scheduler; `Schedule { Phase, Sequence, Loop, Branch }` tree; run conditions; state machines |
| [`grass_derive`](crates/grass_derive/) | `#[derive(ScheduleSet)]`, `#[derive(StageEnum)]`, `#[derive(Namespace)]` |
| [`grass_multi`](crates/grass_multi/) | cross-namespace coupling primitives — `Multi` / `MultiRes<T, NS>` SystemParams, `add_subapp` / `add_remote_subapp`, `Wire` / `Transport` / `MpiInterCommTransport` |
| [`grass_io`](crates/grass_io/) | optional companion: TOML config loading (`Config` + `InputPlugin`) plus `SimClock`, `TermOut`, `Dump` plugins |
| [`grass_mpi`](crates/grass_mpi/) | thin MPI abstraction; powers `MpiInterCommTransport` |
| [`grass_precice`](crates/grass_precice/) | preCICE participant plugin (Pattern A; behind the `precice` Cargo feature) |
| [`oscillator_demo`](crates/oscillator_demo/) | shared physics for the worked examples |

## Examples

[`examples/coupling/`](examples/coupling/) — five worked examples that
build up coupled-oscillator simulation one schedule at a time. The
README in that directory walks them through as a story:

> Same physics, same coupling function. Only the schedule changes.

Every numeric parameter for every example lives in a `main.toml`
alongside the `main.rs`; each example takes that file as `args[1]`:

```sh
cargo run --release --example single_oscillator -- examples/coupling/single_oscillator/main.toml
cargo run --release --example explicit          -- examples/coupling/explicit/main.toml
cargo run --release --example implicit          -- examples/coupling/implicit/main.toml
cargo run --release --example adaptive          -- examples/coupling/adaptive/main.toml

# MPI two-binary version (both binaries load the same TOML):
cargo build --features mpi --example explicit_mpi_a --example explicit_mpi_b
mpirun -np 1 ./target/debug/examples/explicit_mpi_a examples/coupling/explicit_mpi/main.toml \
     : -np 1 ./target/debug/examples/explicit_mpi_b examples/coupling/explicit_mpi/main.toml

# Or generate a starter config for any of them:
cargo run --example explicit -- --generate-config
```

[`examples/io/`](examples/io/) — single oscillator wired to every
[`grass_io`](crates/grass_io/) plugin (`InputPlugin`, `TermOutPlugin`,
`DumpPlugin`, `RunPlugin` — the latter auto-installs `SimClockPlugin`).
Demonstrates the configure-after-construct flow with periodic terminal
output and per-frame JSON dumps:

```sh
cargo run --example io -- examples/io/main.toml
```

[`examples/io_coupled/`](examples/io_coupled/) — same explicit-coupling
physics as `examples/coupling/explicit`, plus `TermOut` + `Dump` on the
parent reading both sub-Apps via `MultiRes<T, NS>`. Shows
parent-level observability over a coupled simulation, all driven by
one `main.toml`:

```sh
cargo run --example io_coupled -- examples/io_coupled/main.toml
```

## Origin

The core App + scheduler crates were extracted from
[MDDEM](https://github.com/SueHeir/MDDEM);
The crates here are consumed by:

- [**MDDEM**](https://github.com/SueHeir/MDDEM) — discrete
  element method (granular contact, parallel bonds, walls, thermal,
  clumps). Uses `grass_app` + `grass_scheduler` as its plugin substrate.
- [**toy-cfd**](https://github.com/SueHeir/toy-cfd) — RK3 +
  CFL compressible CFD with cfd_state / cfd_eos / cfd_grid / cfd_solver
  / cfd_output crates. Same substrate.
- [**toy-cfd-mddem**](https://github.com/SueHeir/toy-cfd-mddem)
  — DEM↔CFD fluid–solid coupling examples (drag, immersed boundary,
  ghost-cell IBM). Uses `grass_multi`'s primitives to compose
  the two codes above into a single coupled simulation.



