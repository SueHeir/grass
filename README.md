# GRASS

**Build explicit solvers as composable plugins instead of a hand-rolled main
loop — and couple several together, in-process or across MPI.**

You don't write a `main` loop. You register your state as **resources** and your
step logic as **systems** (plain functions that declare what they read and write
by argument type), bundle them into **plugins**, and let a dependency-injection
**scheduler** order and run them. Swap an integrator or output plugin without
touching the rest; wire two whole solvers together — a particle code to a fluid
code — under one parent and let them exchange state every step, in one process or
across MPI binaries.

GRASS — the **General Rust App System Scheduler** — is that framework tier. It
knows nothing about particles or physics; it is the App, scheduler, I/O, MPI, and
coupling layer that domain crates (and your own solver) build on.

## Ten seconds

```rust
use grass_app::prelude::*;
use grass_scheduler::prelude::*;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Step { Update }

struct Position(f64);

fn move_thing(mut pos: ResMut<Position>) {
    pos.0 += 1.0;                        // a system: it declares it writes Position
}

let mut app = App::new();
app.add_resource(Position(0.0));
app.add_update_system(move_thing, Step::Update);
app.start();                            // organize → setup → run loop → cleanup
```

No `main` loop, no manual dispatch: `move_thing` takes `ResMut<Position>`, so the
scheduler injects that borrow and runs it. Grow this into a real time-stepping
solver in the [Write Your Own Solver](https://sueheir.github.io/grass/tutorial/write-your-own-solver.html)
tutorial, or skim [GRASS in 5 minutes](https://sueheir.github.io/grass/quickstart.html).

## The deal

Everything is three primitives and one scheduler.

- **Resources** — your state, stored by type; reached from any system as
  `Res<T>` (shared read) or `ResMut<T>` (exclusive write).
- **Systems** — plain functions whose parameter types *are* their read/write
  declaration. The scheduler injects the borrows and picks the run order from
  them. Execution is single-threaded and deterministic — "order" is the sequence
  systems run in, not parallel dispatch.
- **Plugins & plugin groups** — the modular unit. A plugin's `build(&mut app)`
  wires its resources and systems; a group bundles plugins and lets a consumer
  `disable::<T>()` one and substitute their own. This is exactly how SOIL and
  DIRT layer on, and how you swap an integrator or output law.

Three things worth spotlighting:

- **The DI scheduler.** Systems declare what they touch by argument type; the
  scheduler computes an order from the read/write sets and injects the borrows.
  Borrows are checked at *run time* (resources live in `RefCell`s), so two
  systems never race — but a single system taking the same resource `Res` and
  `ResMut` at once panics. That trade-off is deliberate and
  [documented](https://sueheir.github.io/grass/model/scheduler.html).
- **The `Schedule { Phase, Sequence, Loop, Branch }` tree.** A timestep is a tree
  of nodes: a `Phase` is a named set of systems, a `Sequence` runs children in
  order, a `Loop` repeats one (the per-step loop), a `Branch` picks conditionally.
  You usually address phases through a `ScheduleSet` enum whose variant order
  fixes execution order — no graph wiring by hand.
- **Cross-MPI coupling (`grass_multi`).** Several `App`s run as sub-Apps under one
  parent; the parent's own schedule *is* the orchestrator (`Tick → Couple →
  Check`). `MultiRes<T, NS>` / `MultiResMut<T, NS>` move state across namespaces;
  `add_subapp` couples in-process, `add_remote_subapp` + `MpiInterCommTransport`
  couples across separate MPI binaries — the same `SubApps` machinery either way.

## When to use it — and when not

**Use GRASS when** your solver's state is **resource-shaped with separable
read/write sets**: an explicit, time-stepping method where each step is a
sequence of systems that read some resources and write others. That is exactly
the shape of a particle code, a finite-volume sweep, or a cellular update — and
the payoff is that you write the physics, not the plumbing, and you can couple
your solver to someone else's.

**Reach for something else when** your state is one large coupled matrix rather
than separable resources — implicit global solvers (FEM, spectral,
Newton–Krylov) where every unknown depends on every other through a global solve.
GRASS is **not proven** for that shape today, and we would rather say so than
imply a fit that isn't there. (This is scope, not a permanent verdict: a
mesh/implicit substrate is being worked on, and this section will grow when
there's a landed proof to point at.)

## The stack

GRASS is the framework tier of a three-repo stack. Lower tiers never depend on
higher ones:

```
GRASS    framework: App, Plugin, Scheduler, IO, coupling      (no particles)
  └─ SOIL   substrate: Atom, domain decomposition, comm, neighbor lists   (no physics)
       └─ DIRT   physics: Discrete Element Method
```

- **GRASS** (this repo) — App + Plugin + dependency-injection scheduler, I/O,
  MPI, coupling primitives. No particles, no physics.
- **[SOIL](https://github.com/SueHeir/soil)** — a method-agnostic particle
  substrate on GRASS (base `Atom`, `AtomData` registry, domain decomposition,
  communication, neighbor lists). See the [SOIL book](https://sueheir.github.io/soil).
- **[DIRT](https://github.com/SueHeir/dirt)** — the Discrete Element Method on
  the SOIL substrate (contact, parallel bonds, walls, clumps). See the
  [DIRT book](https://sueheir.github.io/dirt).

The App + scheduler crates here were extracted from that particle codebase; GRASS
retains nothing particle- or physics-specific.

## Depend on it

GRASS is a library workspace — the consumers are SOIL, DIRT, and your own
solver — but each crate ships runnable `cargo` examples, so you can get your
hands dirty before you depend on anything:

```console
cargo run -p grass_app       --example hello_app            # the smallest App
cargo run -p grass_scheduler --example verlet_minisolver    # a tiny falling-body solver (the quickstart, in code)
cargo run -p grass_scheduler --example heat_diffusion_1d    # a non-particle 1D mesh solver, checked against theory
cargo run -p grass_io        --example observed_oscillator  # clock + observer + dump
```

To build against it, point at the crates you need:

```toml
[dependencies]
grass_app       = { git = "https://github.com/SueHeir/grass" }
grass_scheduler = { git = "https://github.com/SueHeir/grass" }
# optional companions:
grass_io        = { git = "https://github.com/SueHeir/grass" }  # config, clock, dump
grass_multi     = { git = "https://github.com/SueHeir/grass" }  # coupling
```

Then **read the book** — start with
[GRASS in 5 minutes](https://sueheir.github.io/grass/quickstart.html), then
[Write Your Own Solver](https://sueheir.github.io/grass/tutorial/write-your-own-solver.html).
The book is the primary docs; it builds from `docs/` with `mdbook build`.

## Crate map

| crate | role |
|---|---|
| [`grass_app`](crates/grass_app/README.md) | `App` / `Plugin` / `PluginGroup` — container, lifecycle, plugin-group overrides |
| [`grass_scheduler`](crates/grass_scheduler/README.md) | typed-resource scheduler; `Schedule { Phase, Sequence, Loop, Branch }` tree; run conditions; states and stages |
| [`grass_derive`](crates/grass_derive/README.md) | `#[derive(ScheduleSet)]`, `#[derive(StageEnum)]`, `#[derive(Namespace)]` |
| [`grass_multi`](crates/grass_multi/README.md) | cross-namespace coupling — `MultiRes<T, NS>` / `MultiResMut<T, NS>`, `add_subapp` / `add_remote_subapp`, `Wire` / `Transport` / `MpiInterCommTransport` |
| [`grass_io`](crates/grass_io/README.md) | optional companion: TOML config (`Config` + `InputPlugin`), `SimClock`, `RunPlugin`, `TermOut`, `Dump` |
| [`grass_mpi`](crates/grass_mpi/README.md) | thin MPI abstraction (`CommBackend`); powers `MpiInterCommTransport` |

## Next

- [GRASS in 5 minutes](https://sueheir.github.io/grass/quickstart.html) — the fastest path from zero to a running step.
- [Write Your Own Solver](https://sueheir.github.io/grass/tutorial/write-your-own-solver.html) — a complete time-stepping solver, from scratch.
- [The Scheduler](https://sueheir.github.io/grass/model/scheduler.html) — resources, systems, the schedule tree, and the borrow rules.
- [MPI and Coupling](https://sueheir.github.io/grass/model/mpi-coupling.html) — running across processes and coupling several solvers.

## License

MIT OR Apache-2.0
