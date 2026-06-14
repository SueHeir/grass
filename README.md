# GRASS — General Rust App System Scheduler

Bevy-style App + Plugin framework for explicit, time-stepping solvers,
plus a small toolkit for coupling several App-shaped solvers together
(in-process or across MPI binaries). The framework knows nothing about
particles or physics — it is the App, scheduler, IO, MPI, and coupling
layer that domain crates build on.

It suits solvers whose state is resource-shaped with separable
read/write sets — it is not proven for implicit global solvers (FEM,
spectral, Newton-Krylov) where state is one large coupled matrix.

This is a pure library workspace (no examples).

## Crates

| crate | role |
|---|---|
| [`grass_app`](crates/grass_app/README.md) | `App` / `Plugin` / `PluginGroup` — top-level container and lifecycle |
| [`grass_scheduler`](crates/grass_scheduler/README.md) | typed-resource scheduler; `Schedule { Phase, Sequence, Loop, Branch }` tree; run conditions; states and stages |
| [`grass_derive`](crates/grass_derive/README.md) | `#[derive(ScheduleSet)]`, `#[derive(StageEnum)]`, `#[derive(Namespace)]` |
| [`grass_multi`](crates/grass_multi/README.md) | cross-namespace coupling — `MultiRes<T, NS>` / `MultiResMut<T, NS>` SystemParams, `add_subapp` / `add_remote_subapp`, `Wire` / `Transport` / `MpiInterCommTransport` |
| [`grass_io`](crates/grass_io/README.md) | optional companion: TOML config loading (`Config` + `InputPlugin`) plus `SimClock`, `RunPlugin`, `TermOut`, `Dump` plugins |
| [`grass_mpi`](crates/grass_mpi/README.md) | thin MPI abstraction (`CommBackend`); powers `MpiInterCommTransport` |

## Architecture

GRASS is the framework tier of a three-repo stack. Lower tiers never
depend on higher ones:

- **GRASS** (this repo) — framework: App + Plugin + dependency-injection
  scheduler, IO, MPI, coupling primitives. No particles, no physics.
- [**SOIL**](https://github.com/SueHeir/soil) — substrate: a
  method-agnostic particle layer on GRASS (base `Atom`, `AtomData`
  registry, domain decomposition, communication, neighbor lists). No
  physics.
- [**DIRT**](https://github.com/SueHeir/dirt) — DEM physics: the
  Discrete Element Method on the SOIL substrate (contact, parallel
  bonds, walls, clumps, …). `dirt_core` is the batteries-included
  umbrella crate users depend on.

The App + scheduler crates here were extracted from that particle
codebase; GRASS retains nothing particle- or physics-specific.

## License

MIT OR Apache-2.0
