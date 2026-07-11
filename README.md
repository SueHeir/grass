# GRASS: General Rust App System Scheduler

<!-- disclaimer-banner -->
> **Research-software status:** This ecosystem was created with heavy usage of AI. GRASS, SOIL, and DIRT are the core architecture and DEM implementation; repositories prefixed `dev_` are experimental method demonstrations outside the author's domain expertise. Treat claims according to their linked evidence and documented limitations. See [DISCLAIMER.md](DISCLAIMER.md).
<!-- /disclaimer-banner -->

Most scientific frameworks help you build a solver. **GRASS is for building scientific libraries that can coexist.** It makes a solver a Rust library of typed state, scheduled functions, and plugins; an application then selects and composes those libraries. The claim is about a disciplined interface and ownership boundary—not about making arbitrary existing libraries compatible.

## 30-second pitch

A conventional solver often grows into a monolith: its state layout, timestep driver, I/O, parallelism, and extension points are private to one executable. Adding a capability means editing that application; coupling two such programs means preserving an adapter between two private worlds.

GRASS separates those concerns before they harden together. A library owns its resources and systems, packages them as plugins, and exposes only the contracts another library needs. A parent application owns the schedule and the exchange between independently useful solvers. That lets the same library be reused in a new application without turning GRASS into a particle, mesh, or physics framework.

The small, tested evidence is the coupled-oscillator example: two unchanged oscillator libraries are composed both through direct typed access and through a stable position port, and the validation requires the two paths to have an identical final fingerprint. Run it with:

```bash
source ~/projects/.build-env
cargo run --example oscillator_demo
$BENCH_PYTHON examples/oscillator_demo/sweep.py
```

The sweep also checks an uncoupled oscillator against the analytical `x = cos(t), v = -sin(t)` solution at `t = 1`, with maximum component error below `1e-4`. Its committed result figure and precise pass criterion are in [the example README](examples/oscillator_demo/README.md). For the complete composition story, start with the [coupled-oscillator walkthrough](docs/src/tutorial/coupled-oscillator-walkthrough.md).

## The problem before the implementation

The difficulty is not that a timestep needs a loop. It is that a useful solver usually owns too much of its own world: a private state model, lifecycle, scheduling convention, configuration vocabulary, and data exchange mechanism. Those choices couple a scientific capability to one application. Later reuse or coupling is possible only if the owners agree on a seam—or if someone maintains knowledge of both private implementations forever.

GRASS gives library authors a place to put a capability without claiming its application. It does not infer an interface from two arbitrary codebases, turn private resource types into public APIs, or decide a physically meaningful exchange, interpolation, cadence, convergence rule, or termination policy. Those are explicit work for the library and coupling authors.

## The contracts that make coexistence possible

```text
 library A                           coupling/application owner                 library B
 ┌─────────────────┐                ┌───────────────────────────┐             ┌─────────────────┐
 │ private state   │                │ parent schedule            │             │ private state   │
 │ resources       │─ expose ─────► │ Port<Exchange>             │ ◄──── consume ─│ resources   │
 │ systems/plugins │                │ cadence + stop policy      │             │ systems/plugins │
 └─────────────────┘                └───────────────────────────┘             └─────────────────┘
         │                                          │                                      │
         └──────────────────── GRASS: App • Plugin • typed access • scheduler ───────────┘
```

The public contracts are intentionally narrow:

- A library owns typed **resources**, **systems**, and **plugins**. Systems declare `Res<T>`/`ResMut<T>` access; the current scheduler runs sequentially and deterministically.
- A plugin declares concrete dependencies and optional capability contracts; its defaults are declarative TOML, not a registration script.
- A coupling owner owns the parent schedule and stop policy. A stable exchange crosses a `Port<T>` whose `T` is an interface-owned contract, rather than a consumer naming a producer's private state.

The normative rules, runnable checks, and current limitations are collected in the [library composition contract](docs/src/reference/library-composition-contract.md). That contract is a compatibility target for libraries built for it; it is not a promise that any library can be plugged in automatically.

## A concrete library stack: GRASS → SOIL → DIRT

The strongest present case is not a generic promise; it is one layered path. GRASS supplies composition infrastructure. [SOIL](https://github.com/SueHeir/soil) supplies reusable particle plumbing. [DIRT](https://github.com/SueHeir/dirt) supplies DEM state, laws, and validation evidence. DIRT's cited LAMMPS and theory checks support the DEM layer; they do not validate every future method or coupling.

```text
DIRT  ── DEM contact, rotation, bonds, walls, materials
  │     owns method-specific meaning and validation
SOIL  ── AtomData, migration, ghosts, neighbour lists
  │     owns particle infrastructure, not a force law
GRASS ── App, Plugin, typed scheduling, I/O, MPI, ports
        owns composition, not particles, meshes, or physics
```

This separation lets a particle-method author extend SOIL with typed columns without putting DEM vocabulary into its base types, while DIRT remains a library above it. Read the evidence, ownership boundaries, and limits in the [Grass → SOIL → DIRT case study](docs/src/stack/grass-soil-dirt-case-study.md).

## A runnable path in five minutes

If you want to see GRASS rather than read its architecture, run the small non-particle examples:

```bash
source ~/projects/.build-env
cargo run --example hello_app
cargo run --example oscillator_demo
```

Then follow [GRASS in 5 minutes](docs/src/quickstart.md): define a resource, write a system, name a phase, package the result as a plugin. The tutorial keeps configuration declarative and behavior in typed Rust code.

## Choose your route

- **Library authors:** begin with the [composition contract](docs/src/reference/library-composition-contract.md), then [write your own solver](docs/src/tutorial/write-your-own-solver.md). Define what your library owns and the small contracts it exports.
- **Application authors:** begin with [GRASS in 5 minutes](docs/src/quickstart.md), then assemble the plugins and policies your executable owns.
- **Coupling authors:** begin with [coupling two solvers](docs/src/tutorial/coupling-two-solvers.md). Keep the seam in the parent application or dedicated coupling package; use a `Port<T>` for a stable exchange boundary.
- **Particle-method authors:** begin with [SOIL](https://sueheir.github.io/soil). **DEM users and authors:** begin with [DIRT](https://sueheir.github.io/dirt), where the method-specific evidence lives.

GRASS is deliberately small: an `App`, plugin boundary, scheduler, I/O, MPI, and coupling tools. Its value depends on authors keeping those contracts explicit enough that scientific libraries remain independently useful.
