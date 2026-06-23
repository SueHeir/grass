# Introduction

**GRASS** — the General Rust App System Scheduler — is a Bevy-style `App` +
`Plugin` framework for explicit, time-stepping solvers, plus a small toolkit for
coupling several App-shaped solvers together (in-process or across MPI binaries).

The framework knows nothing about particles or physics. It is the App,
scheduler, I/O, MPI, and coupling layer that domain crates build on:

```
GRASS    framework: App, Plugin, Scheduler, IO, coupling      (no particles)
  └─ SOIL   substrate: Atom, domain decomposition, comm, neighbor lists   (no physics)
       └─ DIRT   physics: Discrete Element Method
```

- **GRASS** (this repo) — framework: App + Plugin + dependency-injection
  scheduler, I/O, MPI, coupling primitives.
- **[SOIL](https://github.com/SueHeir/soil)** — a method-agnostic particle
  substrate on GRASS. See the [SOIL book](https://sueheir.github.io/soil).
- **[DIRT](https://github.com/SueHeir/dirt)** — DEM physics on the substrate. See
  the [DIRT book](https://sueheir.github.io/dirt).

This is a pure library workspace — no examples binaries; the consumers are SOIL,
DIRT, and your own solver.

## What kind of solver is GRASS for

GRASS suits solvers whose state is **resource-shaped with separable read/write
sets** — explicit, time-stepping methods where each step is a sequence of systems
that read some resources and write others. That is exactly the shape of a
particle code, a finite-volume sweep, or a cellular update.

It is **not** proven for implicit global solvers (FEM, spectral, Newton–Krylov)
where the state is one large coupled matrix rather than separable resources.

## The core idea

You don't write a `main` loop. You register **resources** (your state) and
**systems** (functions that transform state) with an `App`, group them into
**plugins**, and let the **scheduler** order and run them. A system declares what
it touches by its argument types (`Res<T>`, `ResMut<T>`); the scheduler injects
those and uses the read/write sets to order the systems. Execution is
single-threaded and deterministic — "order" is the sequence in which systems
run, not parallel dispatch.

```rust
use grass_app::prelude::*;
use grass_scheduler::prelude::*;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Step { Update }

struct Position(f64);

fn move_thing(mut pos: ResMut<Position>) {
    pos.0 += 1.0;
}

let mut app = App::new();
app.add_resource(Position(0.0));
app.add_update_system(move_thing, Step::Update);
app.start();
```

## The shape of the book

- **[App, Plugin, PluginGroup](./model/app-plugin.md)** — the container, the
  modular registration unit, and how to bundle and swap implementations.
- **[The Scheduler](./model/scheduler.md)** — resources, systems, the schedule
  tree, run conditions, states/stages, how execution order is decided, and the
  scheduling-primitive choices.
- **[I/O and Configuration](./model/io.md)** — the optional `grass_io` plugins
  (config, clock, terminal output, dump, run loop) and their namespace ordering.
- **[MPI and Coupling](./model/mpi-coupling.md)** — running across processes
  (`grass_mpi`) and coupling several solvers (`grass_multi`).
- **[Write Your Own Solver](./tutorial/write-your-own-solver.md)** — assemble a
  complete time-stepping solver from scratch.
- **[Derive Macros](./reference/derives.md)** — `ScheduleSet`, `StageEnum`, and
  `Namespace`, plus the invariants they carry.
