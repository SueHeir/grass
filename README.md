# GRASS

<!-- disclaimer-banner -->
> This code was fully written via **Claude 4.6,4.8 and Fable 5**, and stands as a proof of concept for a **bevy-like** ecosystem for physics simulation research, with the goal of testing if one scheduler/framework (**GRASS**) works for most scientific codes. **SOIL** and **FIELD** are particle- and mesh-based substrates for physics such as **DIRT** (DEM) or **dev_field_efvm**. Note that all other physics based repos I have start with **dev_**, as I do **NOT** know these methods. Please read, evaluate, use with a grain of salt, I have not personally read or reviewed everything here.
<!-- /disclaimer-banner -->


**Build solvers as composable plugins instead of a hand-rolled main loop —
explicit time-stepping or a single implicit global solve, particles or a mesh —
and couple several together, in-process or across MPI.**

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

It's the foundation a research ecosystem is built on, not a monolith you run.
[SOIL](https://github.com/SueHeir/soil) builds a parallel particle substrate on it;
[DIRT](https://github.com/SueHeir/dirt) builds a LAMMPS-validated DEM code on that;
[FIELD](https://github.com/SueHeir/field) builds a mesh substrate for PDE solvers —
and the same seams are open for a CFD, SPH, or peridynamics tier you write yourself.
Each is a library you read, extend, and customize in Rust, reusing the scheduler,
I/O, and coupling instead of a hand-rolled main loop.

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

Because a solver here *is* Rust — resources, systems, plugins — you extend or
change one the same way you read it: add a system, add a plugin, override a group.
There is no separate input-script language to learn or grow out of; a new feature
is type-checked code you can read, modify, and step through in a debugger. (`grass_io`
does offer TOML config for values you'd rather not hard-code — parameters, run
stages — but the *behavior* stays in Rust, where you can see it.)

**Nothing is first-class — every aspect of a solver is a plugin.** There are no
privileged, built-in parts of the physics: a one-line debug print is registered
the same way inter-particle communication is — as a **system** (with its
**resources**), bundled in a **plugin**. Plugins add systems and resources, and
they can also **remove** them — so any part of any simulation, from a core force
law to I/O to a diagnostic, can be added to, swapped for a replacement, or
deleted, without editing the code it changes. The model is lifted from
[Bevy](https://bevyengine.org)'s scheduler. That uniform flexibility is the whole
point — and an honest double edge: with no fixed skeleton, behavior lives across
many small plugins rather than one linear main loop, so the power comes with
indirection to trace. The goal it buys: write a capability **once** and reuse it
across every solver on the stack, instead of re-implementing it in each code.

Three things worth spotlighting:

- **The DI scheduler.** Systems declare what they touch by argument type; the
  scheduler records those read/write sets and injects the borrows into each
  system as it runs.
  Borrows are checked at *run time* (resources live in `RefCell`s), so two
  systems never race. Systems in the same phase that both touch a resource are
  not rejected automatically; use `.before()` / `.after()` when the data order
  matters. A single system taking the same resource as `Res` and `ResMut` at
  once panics with a `RefCell` borrow error. That trade-off is deliberate and
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

**Use GRASS when** your solver's step decomposes into **systems with separable
read/write sets** — each stage reads some resources and writes others. That is
the shape of a particle code, a finite-volume sweep, a cellular update, *and*
(as it turns out) an implicit assemble-and-solve: the payoff is that you write
the physics, not the plumbing, and you can couple your solver to someone else's.

**Implicit global solves work too — and it's proven.** These docs used to say
GRASS was "not proven for implicit global solvers (FEM, spectral, Newton–Krylov)".
That caveat is retired. FIELD's [`fem_poisson`](https://github.com/SueHeir/field)
example solves steady-state Poisson with P1 finite elements as a **single global
sparse solve** `K u = b` — every unknown coupled through one matrix, no
timestepping — expressed as an ordinary `Assemble → Solve → Validate` schedule on
GRASS. It converges at the theoretical 2nd order (observed mean L² order **1.995**
vs. theory 2.000). What GRASS gives you there is the schedule and the resources;
**you bring the sparse solver** (`fem_poisson` uses `nalgebra-sparse`). See
[Non-Particle Solvers on GRASS](https://sueheir.github.io/grass/model/non-particle-solvers.html).

**Still honestly scoped:** the landed proof is one *linear, symmetric,
single-solve* problem. Nonlinear Newton–Krylov iteration, spectral methods, and
large distributed sparse solves are consistent with the design but not yet
demonstrated with a validated example — we'll cite them here when they land, not
before.

## The stack

GRASS is the framework tier of a multi-repo stack. Lower tiers never depend on
higher ones. DIRT (DEM) is the validated proof that a full physics tier rides
this framework; the same seams are open for other methods.

```
GRASS    framework: App, Plugin, Scheduler, IO, coupling      (no particles, no mesh)
  ├─ SOIL    substrate: Atom, domain decomposition, comm, neighbor lists   (no physics)
  │    └─ DIRT   physics: Discrete Element Method
  └─ FIELD   substrate: Mesh, FieldData, halo, AMR                         (no equations)
       └─ dev_field_efvm   physics: compressible CFD (Riemann/EOS/IBM)  - in progress
```

- **GRASS** (this repo) — App + Plugin + dependency-injection scheduler, I/O,
  MPI, coupling primitives. No particles, no mesh, no physics.
- **[SOIL](https://github.com/SueHeir/soil)** — a method-agnostic particle
  substrate on GRASS (base `Atom`, `AtomData` registry, domain decomposition,
  communication, neighbor lists). See the [SOIL book](https://sueheir.github.io/soil).
- **[DIRT](https://github.com/SueHeir/dirt)** — the Discrete Element Method on
  the SOIL substrate (contact, parallel bonds, walls, clumps). See the
  [DIRT book](https://sueheir.github.io/dirt).
- **[FIELD](https://github.com/SueHeir/field)** — the mesh/Eulerian substrate on
  GRASS (`UniformMesh`, `FieldData`, halo), equation-agnostic the way SOIL is
  method-agnostic. It already hosts the `fem_poisson` implicit-solve proof; its
  compressible-CFD physics tier (**dev_field_efvm**) is in progress.

Several development-stage tiers also ride the stack as demonstrations of the
same substrate boundaries. They are not peer-reviewed or presented as
domain-validated; `dev_` marks that status plainly:

- **[dev_soil_sph](http://192.168.0.170:8082/SueHeir/dev_soil_sph)** —
  granular SPH (`mu(I)` continuum) on SOIL.
- **[dev_soil_peri](http://192.168.0.170:8082/SueHeir/dev_soil_peri)** —
  bond-based peridynamics on SOIL.
- **[dev_field_efvm](http://192.168.0.170:8082/SueHeir/dev_field_efvm)** —
  compressible CFD on the sibling FIELD mesh substrate.

Those examples keep GRASS honest about the abstraction: the framework is not a
particle code or a mesh code, it is the scheduler, plugin, I/O, and coupling
layer underneath both.

Today the stack has two substrates — **SOIL** (particles) and **FIELD** (meshes)
— and both are primarily **short-range/local**. Reaching **long-range / global**
methods (FMM/Ewald far-field for particles; multigrid and implicit/global solves
for meshes) is a second, orthogonal axis on the roadmap — and the current plan is
to grow that reach *inside* SOIL and FIELD themselves, not as separate substrates.
FIELD's `fem_poisson` example (one implicit `K u = b` solve) is an early proof in
that direction; a general long-range/global capability is still future work.

The App + scheduler crates here were extracted from that particle codebase; GRASS
retains nothing particle- or physics-specific.

## How the three fit together

GRASS gives you the `App`/scheduler/coupling; SOIL turns that into a parallel
particle substrate via one `AtomData` contract; DIRT is the proof that a full
LAMMPS-validated physics tier rides it — and the same seams are open for SPH,
peridynamics, or your own method.

One line per tier, worded identically wherever these three repos describe
themselves:

- **[GRASS](https://github.com/SueHeir/grass)** — Build solvers as composable
  plugins instead of a hand-rolled main loop — explicit time-stepping or a
  single implicit global solve, particles or a mesh — and couple several
  together, in-process or across MPI.
- **[SOIL](https://github.com/SueHeir/soil)** — Write your own particle method
  without hand-writing domain decomposition, halo exchange, migration, and
  neighbor lists — declare your state once, SOIL carries it through all of it.
- **[DIRT](https://github.com/SueHeir/dirt)** — A Rust granular-DEM code you read
  and extend as composable plugins — cross-checked against LAMMPS and closed-form
  theory.

**Where to start:** to *run* granular simulations, start at
[DIRT](https://github.com/SueHeir/dirt), the batteries-included physics tier; to
*write your own* particle method or solver, start at
[SOIL](https://github.com/SueHeir/soil) (the particle substrate) or
[GRASS](https://github.com/SueHeir/grass) (the framework). The full walkthrough
of how the tiers compose — one timestep end to end, and where the seams are — is
the canonical [How the stack fits together](https://sueheir.github.io/grass/stack/how-the-stack-fits-together.html)
page in the GRASS book.

## Depend on it

GRASS is a library workspace — the consumers are SOIL, DIRT, and your own
solver — but the workspace ships runnable top-level `cargo` examples, so you can
get your hands dirty before you depend on anything:

```console
cargo run --example hello_app            # the smallest App
cargo run --example verlet_minisolver    # a tiny falling-body solver (the quickstart, in code)
cargo run --example heat_diffusion_1d    # a non-particle 1D mesh solver, checked against theory
cargo run --example observed_oscillator  # clock + observer + dump
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
- [Non-Particle Solvers on GRASS](https://sueheir.github.io/grass/model/non-particle-solvers.html) — the mesh and implicit-FEM proofs that GRASS is discretization-agnostic.

## License

MIT OR Apache-2.0
