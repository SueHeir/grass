# GRASS: General Rust App System Scheduler

<!-- disclaimer-banner -->
> **Research-software status:** This ecosystem was created with heavy usage of AI. GRASS, SOIL, and DIRT are the core architecture and DEM implementation; repositories prefixed `dev_` are experimental method demonstrations outside the author's domain expertise. Treat claims according to their linked evidence and documented limitations. See [DISCLAIMER.md](DISCLAIMER.md).
<!-- /disclaimer-banner -->

## Why GRASS exists

Most simulation codes begin the same way: define a state, call functions in a
particular order to edit the state, repeat. GRASS provides a formalized way to define your state (resources) and call functions (systems) via plugins, schedules, apps and sub-apps. (This is very similar to [Bevy's](https://bevyengine.org) ECS; however, it's just the S of ECS). Every aspect of a simulation's codebase is added as a plugin, making every aspect of the code swappable and replaceable via additional plugins.

GRASS stems from my frustration with editing scientific codebases. Scientific solvers are usually built as closed applications. Each grows
its own state containers, lifecycle, timestep driver, I/O, and communication
assumptions, etc. Editing existing simulation codebases is typically done through each codebase's unique "plugin" or "fix" system. These codebases have many core functionalities locked into the structure. Editing these core structures is typically not a frictionless path. Coupling two of them later means reconciling two private worlds and/or maintaining an adapter between them forever, both of which can require editing this locked structure.

With GRASS, there is no locked structure; however, it does ask you to get your hands a little dirty with some programming. GRASS is a library, and plugins you would write with GRASS are also libraries. If everything is a library, what do you run? The answer is you build your own executable to do exactly what you want! The following is a 'simulation' counting by one every step and checking if it has reached 5 every step.

### A complete app

```rust
use grass_app::prelude::*;
use grass_scheduler::prelude::*;
use grass_derive::prelude::*;

/// Per-step phases. Declaration order = schedule index.
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Step {
    Tick,
    CheckDone,
}

/// The one piece of simulation state.
struct Counter {
    steps: u32,
}

/// Runs every step: advance the counter.
fn tick(mut counter: ResMut<Counter>) {
    counter.steps += 1;
}

/// Done-condition: stop the run loop once we've taken 5 steps.
fn check_done(counter: Res<Counter>, mut sm: ResMut<SchedulerManager>) {
    if counter.steps >= 5 {
        sm.state = SchedulerState::End;
    }
}

/// Bundles the resource and systems into one reusable unit.
struct CounterPlugin;

impl Plugin for CounterPlugin {
    fn build(&self, app: &mut App) {
        app.add_resource(Counter { steps: 0 })
            .add_update_system(tick, Step::Tick)
            .add_update_system(check_done, Step::CheckDone);
    }
}

fn main() {
    let mut app = App::new();
    app.add_plugins(CounterPlugin);
    app.start();
}
```

This is a lot of code to count to 5, but it explains the following very well:

- **Resources** hold state.
- **Systems** are ordinary functions that declare the state they read and write.
- **Schedules** define when those functions run.
- **Plugins** package capabilities that can be added, replaced, or removed.

### From one solver to two

If that's how much code is required to count to 5, how much would be required to couple two independent codebases?

```rust
fn main() {
    let mut app = App::new();
    app.add_subapp("dem", setup::dem())
      .add_subapp("cfd", setup::cfd())
      .add_plugins(DemCfdCouplingPlugin::for_air(RADIUS, 200, DT, GRAVITY))
      .start();
}
```

Only 5 lines? Well not really, `DemCfdCouplingPlugin` does all the heavy lifting here. The important thing is that the code of the subapps for the DEM solver and CFD solver were NOT edited, both codes know nothing about each other. All changes to both codes and added systems for coupling codes to transfer information were done via the Coupling plugin we add.

GRASS knows nothing about particles,
meshes, or physics. It provides the App, scheduler, I/O, MPI, and coupling layer
that domain crates and complete solvers build on.

## What makes that composition possible

GRASS does not make two arbitrary closed programs compatible. The boundary has
to be designed into the libraries: each solver owns its internal state and
behavior, while the application that combines them owns the schedule, exchange,
and stopping policy.

```text
 solver A                    coupling application                    solver B
┌──────────────┐           ┌────────────────────────┐             ┌──────────────┐
│ private state│           │ parent schedule        │             │ private state│
│ systems      │── expose ─▶ exchange contract ────▶│ consume     │ systems      │
│ plugins      │           │ cadence + stop policy  │             │ plugins      │
└──────────────┘           └────────────────────────┘             └──────────────┘
       │                               │                                 │
       └──────────── GRASS App · resources · systems · plugins ──────────┘
```

That division of ownership is the important part:

- A **solver library** owns its resources, systems, phases, and plugins.
- A **coupling package** owns the data conversion and exchange systems.
- The **parent application** selects the plugins, orders the solver ticks and
  exchanges, and decides when the combined run is finished.
- A stable interface uses a `Port<T>` whose exchange type belongs to the
  interface—not either solver's private resource type.

The full, test-linked rules are in the
[Scientific Library Composition Contract](docs/src/reference/library-composition-contract.md).

## The programming model

### Resources make ownership visible

Shared simulation state is stored by Rust type. A system requests `Res<T>` for a
shared read or `ResMut<T>` for an exclusive write. Those function parameters are
both the system's inputs and its access declaration; the scheduler injects the
actual borrows when the system runs.

Resources use runtime-checked borrows. GRASS currently executes systems
sequentially, so there is no parallel system dispatch, but meaningful data order
still needs to be explicit. Put causal stages in different phases or use typed
`.before()` / `.after()` relationships. A single system that requests the same
resource mutably and immutably will panic with a `RefCell` borrow error.

### Schedules make time visible

A `ScheduleSet` enum usually describes the ordered phases of one step. More
complex applications can use a schedule tree built from `Phase`, `Sequence`,
`Loop`, and `Branch` nodes. Independently authored solvers can occupy separate
schedule namespaces, while a parent schedule places their ticks and exchanges in
one explicit order.

Current execution is deterministic: phase and namespace order, explicit system
dependencies, and finally registration order determine the sequence. This is a
sequential determinism contract, not a promise of reproducibility across every
transport, platform, compiler, or floating-point implementation.

### Plugins make capabilities replaceable

A plugin registers the resources and systems for one capability. A plugin group
provides a useful default collection while allowing an application to disable a
member and install a replacement. Dependencies and capability contracts can be
validated before the run starts, and the fallible lifecycle APIs return setup
errors instead of requiring a panic.

Plugin configuration is declarative TOML. Behavior remains ordinary typed Rust,
so extending a solver means adding or replacing code you can read, test, and
debug rather than growing a separate simulation scripting language. GRASS can
also generate configuration examples and field references from plugin metadata.

### Sub-apps keep complete solvers independent

`grass_multi` places several `App`s under a parent. For one-off local coupling,
`MultiRes<T, NS>` and `MultiResMut<T, NS>` provide namespaced access to a
sub-app's resources. For a stable library boundary, `Port<T>` exposes an
interface-owned exchange value without making the consumer depend on the
producer's private state.

The same parent/sub-app model supports separate MPI binaries through
`add_remote_subapp`, `Wire`, `Transport`, and `MpiInterCommTransport`. Remote
coupling is explicit: both peers must agree on message order, direction, cadence,
and wire representation.

## What is demonstrated today

The repository keeps runnable examples, tests, numerical acceptance criteria,
and committed result figures together. The evidence is intentionally narrower
than “GRASS can run every scientific method.” It demonstrates specific pieces of
the composition model:

| Example | What it demonstrates |
|---|---|
| [`hello_app`](examples/hello_app/README.md) | Minimal App lifecycle and scheduled execution |
| [`verlet_minisolver`](examples/verlet_minisolver/README.md) | A small explicit time-stepping solver built from resources and systems |
| [`heat_diffusion_1d`](examples/heat_diffusion_1d/README.md) | A non-particle mesh-style update checked against theory |
| [`matrix_free_poisson_cg`](examples/matrix_free_poisson_cg/README.md) | An iterative global solve and convergence history |
| [`oscillator_demo`](examples/oscillator_demo/README.md) | Independent solver libraries, sub-app composition, ports, and generated configuration |
| [`oscillator_coupling_schemes`](examples/oscillator_coupling_schemes/README.md) | Explicit, Picard, relaxed, and adaptive exchanges compared with an independent reference |
| [`oscillator_mpmd`](examples/oscillator_mpmd/README.md) | The coupling contract exercised across separate MPI binaries |
| [`typed_system_labels`](examples/typed_system_labels/README.md) | Typed ordering, required dependencies, and failure diagnostics |
| [`fallible_lifecycle`](examples/fallible_lifecycle/README.md) | Setup failure propagation, update short-circuiting, and cleanup |

The complete index is [Example Validation](examples/VALIDATION.md). These checks
validate framework behavior and the particular examples; they do not validate
every physics package built on GRASS.

## Where the claim stops

GRASS is a useful fit when a codebase can expose typed state and decompose its
work into scheduled operations. It earns the extra structure when you expect to
reuse capabilities, replace infrastructure, assemble a family of solvers, or
couple independently useful methods.

It may be the wrong fit when:

- a small calculation will remain permanently standalone;
- an existing solver cannot expose a meaningful library boundary without a
  major rewrite;
- the desired extension must depend on undocumented private state;
- a separate input language is a hard requirement;
- parallel execution of independent systems is required today.

Current limitations worth stating plainly:

- The scheduler is single-threaded. GRASS coordinates MPI and remote sub-apps,
  but it does not currently dispatch independent systems in parallel.
- Runtime resource borrowing moves some mistakes from compile time to startup or
  execution time.
- `Wire` is a deliberately small transport contract, not a version-negotiated
  protocol for independently evolving binaries.
- Host/device coherence has framework hooks and tests, not evidence of a
  production GPU backend.
- Non-DEM repositories prefixed `dev_` are architecture demonstrations outside
  the author's domain expertise; their own validation determines what can be
  claimed about their physics.

## The ecosystem

GRASS is the framework tier. Physics and discretization belong above it, so
lower tiers never depend on higher ones:

```text
GRASS   App, plugins, scheduling, I/O, MPI, coupling
  │
  ├── SOIL   particle state, decomposition, migration, ghosts, neighbors
  │     └── DIRT   granular DEM physics and DEM validation
  │
  └── FIELD  mesh and field substrate
        └── dev_field_efvm   experimental finite-volume physics
```

- **[SOIL](https://github.com/SueHeir/soil)** is the reusable particle substrate.
  It carries registered particle data through decomposition, migration, ghost
  exchange, and neighbor construction without owning a force law.
- **[DIRT](https://github.com/SueHeir/dirt)** is the granular DEM code built on
  SOIL. Its LAMMPS and closed-form comparisons support claims about the DEM tier.
- **[FIELD](https://github.com/SueHeir/field)** is the sibling mesh/field
  substrate. Its equation and physics tiers remain separate.

Experimental particle and mesh methods live in visibly named `dev_` repositories,
including [`dev_soil_sph`](https://github.com/SueHeir/dev_soil_sph),
[`dev_soil_peri`](https://github.com/SueHeir/dev_soil_peri), and
[`dev_field_efvm`](https://github.com/SueHeir/dev_field_efvm). They test whether
the boundaries are usable; they are not presented as domain-validated peers of
DIRT.

## Run something

Clone the repository and run the smallest examples:

```console
cargo run --example hello_app
cargo run --example verlet_minisolver
cargo run --example heat_diffusion_1d
cargo run --example oscillator_demo
```

Then follow [GRASS in 5 minutes](https://sueheir.github.io/grass/quickstart.html)
or build a complete solver with
[Write Your Own Solver](https://sueheir.github.io/grass/tutorial/write-your-own-solver.html).

GRASS is a library workspace. Depend only on the crates your application needs:

```toml
[dependencies]
grass_app       = { git = "https://github.com/SueHeir/grass" }
grass_scheduler = { git = "https://github.com/SueHeir/grass" }

# Optional companions
grass_io        = { git = "https://github.com/SueHeir/grass" }
grass_multi     = { git = "https://github.com/SueHeir/grass" }
grass_mpi       = { git = "https://github.com/SueHeir/grass" }
```

## Crate map

| Crate | Role |
|---|---|
| [`grass_app`](crates/grass_app/README.md) | `App`, lifecycle, `Plugin`, `PluginGroup`, dependency/capability validation, generated configuration |
| [`grass_scheduler`](crates/grass_scheduler/README.md) | Typed resources and systems, schedule tree, phases, labels, conditions, states, stages, coherence hooks |
| [`grass_derive`](crates/grass_derive/README.md) | Derives for schedules, stages, namespaces, and configuration metadata |
| [`grass_multi`](crates/grass_multi/README.md) | Sub-apps, namespaced resource access, ports, wire types, and local/remote transports |
| [`grass_io`](crates/grass_io/README.md) | TOML configuration, simulation clock, run control, terminal output, and dumps |
| [`grass_mpi`](crates/grass_mpi/README.md) | MPI backend abstraction used by remote coupling |

## Choose your route

- **Building your first App:** [GRASS in 5 minutes](https://sueheir.github.io/grass/quickstart.html)
- **Writing a solver library:** [Write Your Own Solver](https://sueheir.github.io/grass/tutorial/write-your-own-solver.html)
- **Coupling solvers:** [Coupling Two Solvers](https://sueheir.github.io/grass/tutorial/coupling-two-solvers.html)
- **Understanding execution:** [The Scheduler](https://sueheir.github.io/grass/model/scheduler.html)
- **Authoring compatible libraries:** [Scientific Library Composition Contract](docs/src/reference/library-composition-contract.md)
- **Writing a particle method:** start with [SOIL](https://sueheir.github.io/soil)
- **Running or extending DEM:** start with [DIRT](https://sueheir.github.io/dirt)

The book in `docs/` is the primary detailed documentation and builds with
`mdbook build`.

## License

MIT OR Apache-2.0
