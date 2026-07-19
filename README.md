# GRASS: General Rust App System Scheduler

**A Rust framework for building scientific simulation applications from typed
state, scheduled systems, and reusable plugins.**

<!-- disclaimer-banner -->
> **Research-software status:** This ecosystem was created with heavy usage of
> AI. GRASS, SOIL, and DIRT are the core architecture and DEM implementation;
> repositories prefixed `dev_` are experimental method demonstrations outside
> the author's domain expertise. Treat claims according to their linked evidence
> and documented limitations. See [DISCLAIMER.md](DISCLAIMER.md).
<!-- /disclaimer-banner -->

GRASS supplies the application structure around scientific algorithms. It does
not prescribe particles, meshes, equations, or force laws. A solver defines its
state as ordinary Rust types and its operations as ordinary functions; GRASS
stores that state, injects it into systems, and executes those systems through an
explicit lifecycle and schedule.

## What GRASS provides

- **Typed resources** for simulation state, accessed through `Res<T>` and
  `ResMut<T>` system parameters.
- **Scheduled systems** with phases, typed ordering, run conditions, loops,
  branches, and scheduler states.
- **Apps and lifecycle** covering organization, setup, repeated updates,
  stopping, cleanup, and fallible error propagation.
- **Plugins and plugin groups** for packaging capabilities, declaring
  dependencies, validating contracts, and replacing defaults.
- **TOML configuration and observability** with generated configuration metadata,
  simulation clocks, run control, terminal output, and dumps.
- **A shared MPI runtime** with single-process and MPI communication backends,
  communicator lifecycle, and topology bootstrap for libraries built on GRASS.
- **Optional sub-app composition** for applications that need several complete
  scheduled components under one parent.

GRASS is useful when a scientific code is growing beyond one hard-coded loop:
when capabilities need to be reused, replaced, tested independently, or assembled
into several related applications.

## A complete application

```rust
use grass_app::prelude::*;
use grass_scheduler::prelude::*;

#[derive(Debug, Clone, Copy)]
enum Step {
    Tick,
    CheckDone,
}

impl ScheduleSet for Step {
    fn to_index(&self) -> u32 {
        match self {
            Step::Tick => 0,
            Step::CheckDone => 1,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Step::Tick => "Tick",
            Step::CheckDone => "CheckDone",
        }
    }
}

struct Counter {
    steps: u32,
}

fn tick(mut counter: ResMut<Counter>) {
    counter.steps += 1;
}

fn check_done(counter: Res<Counter>, mut scheduler: ResMut<SchedulerManager>) {
    if counter.steps >= 5 {
        scheduler.state = SchedulerState::End;
    }
}

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

This is the runnable [`hello_app`](examples/hello_app/main.rs):

```console
cargo run --example hello_app
```

The example is intentionally small, but it contains the complete model:
resources hold state, systems transform it, schedules make execution order
explicit, and plugins package reusable behavior.

## Programming model

### Resources make ownership visible

Shared application state is stored by Rust type. A system requests `Res<T>` for a
shared read or `ResMut<T>` for an exclusive write. Those parameters are both the
function's inputs and its access declaration; the scheduler supplies the actual
resource borrows when the system runs.

GRASS does not define scientific state such as particles or mesh fields. Those
types remain owned by the solver library that understands them.

### Schedules make execution visible

A `ScheduleSet` commonly describes the ordered phases of one simulation step.
Applications can also build schedule trees from sequences, loops, branches, and
phases, then add typed `.before()` and `.after()` relationships where causal
ordering matters.

Execution within one process is deterministic by phase, namespace, explicit
dependencies, and registration order. The scheduler does not dispatch systems
concurrently within a rank. That is separate from distributed solver execution:
`grass_mpi` provides a shared MPI lifecycle and `CommResource` abstraction so
libraries built on GRASS can use the same launched MPI world, or split it into
solver-local communicators, without each library independently owning MPI
initialization and finalization. The library above GRASS still owns its actual
parallel algorithm; for example, SOIL owns particle-domain decomposition,
migration, and halo exchange across ranks.

### Plugins make capabilities replaceable

A plugin registers the resources and systems for one capability. Plugin groups
provide useful defaults while allowing individual members to be disabled or
replaced. Capability and dependency contracts can fail during preparation rather
than producing a mysterious error halfway through a run.

Configuration is declarative TOML, while behavior remains typed Rust. Extending
a solver therefore means adding or replacing code that can be read, tested, and
debugged without inventing a second simulation language.

### Apps own the lifecycle

An `App` organizes plugins, runs setup, advances the update schedule until a stop
condition is reached, and performs cleanup. Fallible lifecycle APIs preserve
structured errors and prevent updates from continuing after setup fails.

Sub-apps are an optional extension of this model. They let a parent own several
complete Apps and access their resources through namespaces without making
either child depend on the other.

## What is demonstrated

Runnable examples and their acceptance criteria live together in the repository:

| Example | Demonstrates |
|---|---|
| [`hello_app`](examples/hello_app/README.md) | Minimal App lifecycle and scheduled execution |
| [`verlet_minisolver`](examples/verlet_minisolver/README.md) | Explicit time integration assembled from resources and systems |
| [`heat_diffusion_1d`](examples/heat_diffusion_1d/README.md) | Mesh-style updates checked against an analytical result |
| [`matrix_free_poisson_cg`](examples/matrix_free_poisson_cg/README.md) | Iterative global solve and convergence history |
| [`typed_system_labels`](examples/typed_system_labels/README.md) | Typed ordering, dependencies, and failure diagnostics |
| [`fallible_lifecycle`](examples/fallible_lifecycle/README.md) | Setup-error propagation, update short-circuiting, and cleanup |
| [`observed_oscillator`](examples/observed_oscillator/README.md) | Configuration, clocks, terminal output, and dumps |

The complete evidence index is [Example Validation](examples/VALIDATION.md).
These checks validate framework behavior and the particular examples; they do
not validate physics packages built on GRASS.

## Where GRASS fits—and where it does not

GRASS is a good fit when a code can expose typed state and decompose its work
into scheduled operations. It may be unnecessary for a permanently small,
single-purpose calculation, and it cannot make an existing closed program
modular without that program exposing a useful library boundary.

Current limitations include:

- GRASS does not decide how a scientific problem is partitioned or parallelized.
  Its scheduler orders systems within each rank, while `grass_mpi` supplies the
  common MPI runtime and communicator resources used by parallel libraries such
  as SOIL.
- Resource borrowing is runtime-checked, so some access errors appear during
  preparation or execution rather than at compile time.
- MPI and sub-app infrastructure are framework capabilities, not evidence that
  an arbitrary multiphysics combination is scientifically valid.
- Host/device coherence has interfaces and tests, not a demonstrated production
  GPU backend.

## Install and run

You need stable [Rust](https://rustup.rs/). Clone the repository and run the
smallest examples:

```console
git clone https://github.com/SueHeir/grass
cd grass
cargo run --example hello_app
cargo run --example verlet_minisolver
cargo run --example heat_diffusion_1d
```

GRASS is a library workspace. Applications can depend only on the pieces they
need:

```toml
[dependencies]
grass_app       = { git = "https://github.com/SueHeir/grass", tag = "v0.1.1" }
grass_scheduler = { git = "https://github.com/SueHeir/grass", tag = "v0.1.1" }

# Optional infrastructure
grass_io        = { git = "https://github.com/SueHeir/grass", tag = "v0.1.1" }
grass_mpi       = { git = "https://github.com/SueHeir/grass", tag = "v0.1.1" }
grass_multi     = { git = "https://github.com/SueHeir/grass", tag = "v0.1.1" }
```

Start with the runnable [`hello_app`](examples/hello_app/README.md) and
[`verlet_minisolver`](examples/verlet_minisolver/README.md) examples.

## Crate map

| Crate | Role |
|---|---|
| [`grass_app`](crates/grass_app/README.md) | `App`, lifecycle, plugins, groups, and capability validation |
| [`grass_scheduler`](crates/grass_scheduler/README.md) | Typed resources and systems, schedule trees, ordering, conditions, and states |
| [`grass_derive`](crates/grass_derive/README.md) | Derives for schedules, stages, namespaces, and configuration metadata |
| [`grass_io`](crates/grass_io/README.md) | TOML configuration, clocks, run control, terminal output, and dumps |
| [`grass_mpi`](crates/grass_mpi/README.md) | Single-process and MPI communication backends and topology bootstrap |
| [`grass_multi`](crates/grass_multi/README.md) | Optional sub-apps, namespaced access, and local or distributed orchestration |

Runnable guidance lives beside the examples. Public API, scheduling, MPI, and
composition details live in the crate READMEs and Rust documentation.

## Ecosystem

The three core repositories form a one-way dependency stack:

```text
GRASS   scientific application framework
  └── SOIL   distributed particle infrastructure
        └── DIRT   discrete-element-method physics and applications
```

- [SOIL](https://github.com/SueHeir/soil) adds particle storage, spatial
  algorithms, and distributed-memory communication.
- [DIRT](https://github.com/SueHeir/dirt) adds granular and bonded-particle DEM
  physics with an explicit validation ledger.

Because the layers expose typed state and scheduled behavior, they can also be
used in larger composed applications. Those application-specific exchanges and
their scientific validation belong outside the core responsibilities above.

## License

MIT OR Apache-2.0
