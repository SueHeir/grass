# GRASS — General Rust App System Scheduler

Bevy-style App + Plugin framework for explicit, time-stepping particle
and grid solvers, plus a small toolkit for coupling several App-shaped
solvers together (in-process or across MPI binaries). It suits solvers
whose state is resource-shaped with separable read/write sets — it is
not proven for implicit global solvers (FEM, spectral, Newton-Krylov)
where state is one large coupled matrix.

## Crates

| crate | role |
|---|---|
| [`grass_app`](crates/grass_app/) | `App` / `Plugin` / `PluginGroup` — top-level container |
| [`grass_scheduler`](crates/grass_scheduler/) | typed-resource scheduler; `Schedule { Phase, Sequence, Loop, Branch }` tree; run conditions; state machines |
| [`grass_derive`](crates/grass_derive/) | `#[derive(ScheduleSet)]`, `#[derive(StageEnum)]`, `#[derive(Namespace)]` |
| [`grass_multi`](crates/grass_multi/) | cross-namespace coupling primitives — `Multi` / `MultiRes<T, NS>` SystemParams, `add_subapp` / `add_remote_subapp`, `Wire` / `Transport` / `MpiInterCommTransport` |
| [`grass_io`](crates/grass_io/) | optional companion: TOML config loading (`Config` + `InputPlugin`) plus `SimClock`, `TermOut`, `Dump` plugins |
| [`grass_mpi`](crates/grass_mpi/) | thin MPI abstraction; powers `MpiInterCommTransport` |

## Origin

The core App + scheduler crates were extracted from the particle-simulation
codebase now split into [SOIL](https://github.com/SueHeir/soil) (substrate) and
[DIRT](https://github.com/SueHeir/dirt) (DEM). The crates here are consumed by:

- [**DIRT**](https://github.com/SueHeir/dirt) — discrete
  element method (granular contact, parallel bonds, walls, thermal,
  clumps). Uses `grass_app` + `grass_scheduler` as its plugin substrate.
- [**toy-cfd**](https://github.com/SueHeir/toy-cfd) — RK3 +
  CFL compressible CFD with cfd_state / cfd_eos / cfd_grid / cfd_solver
  / cfd_output crates. Same substrate.
- [**toy-cfd-mddem**](https://github.com/SueHeir/toy-cfd-mddem)
  — DEM↔CFD fluid–solid coupling examples (drag, immersed boundary,
  ghost-cell IBM). Uses `grass_multi`'s primitives to compose
  the two codes above into a single coupled simulation.



