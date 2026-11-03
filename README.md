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
| [`mapping_tour`](examples/mapping_tour/README.md) | Mapping between a fluid grid, a non-matching shield surface and particles, with conservation checks |
| [`mock_icarus`](examples/mock_icarus/README.md) | Wrapping a Fortran ablator and a C aeroheating code as plugins, then coupling them explicitly, implicitly (IQN-ILS) and across MPI roles. Its own Cargo workspace; needs `gfortran` and a C compiler |

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
| [`grass_gpu`](crates/grass_gpu/README.md) | Optional shared GPU device, per-column CPU/GPU copies, and per-rank or per-sub-App device choice |
| [`grass_linalg`](crates/grass_linalg/README.md) | Shared matrix-free solvers (CG, preconditioned CG, Picard) with inner products all-reduced over `grass_mpi` |
| [`grass_multi`](crates/grass_multi/README.md) | Optional sub-apps, namespaced access, and local or distributed orchestration |
| [`grass_precice`](crates/grass_precice/README.md) | Run a grass App as a preCICE participant (meshes, data, implicit checkpoints, time-step control) |
| [`grass_map`](crates/grass_map/README.md) | Mapping data between meshes, surfaces, particles and point clouds: consistent, conservative and adjoint operators |

Runnable guidance lives beside the examples. Public API, scheduling, MPI, and
composition details live in the crate READMEs and Rust documentation.

## Ecosystem

GRASS is the base of a family of repositories. Two substrate libraries sit on
it: SOIL for particles and FIELD for meshes. Physics codes sit on a substrate,
and coupling repositories join codes from both sides without changing them.

```text
GRASS   scientific application framework
  ├── SOIL    distributed particle infrastructure
  │     ├── DIRT            discrete element method (DEM)
  │     ├── dev_soil_sph    granular SPH
  │     ├── dev_soil_peri   bond-based peridynamics
  │     ├── dev_soil_dsmc   direct simulation Monte Carlo        (private)
  │     ├── dev_soil_mpm    material point method, uses FIELD    (private)
  │     └── dev_soil_pic    particle-in-cell, uses FIELD         (private)
  ├── FIELD   mesh substrate
  │     ├── dev_field_efvm  explicit finite-volume CFD
  │     ├── dev_field_lbm   lattice Boltzmann                    (private)
  │     └── dev_field_fem   Q1 finite elements                   (private)
  └── couplings
        ├── dev_couple_dem_cfd   DEM ↔ CFD
        ├── dev_couple_sph_cfd   SPH ↔ CFD
        └── dev_couple_matrix    pairwise coupling test bench    (private)
```

Repositories prefixed `dev_` are experimental method demonstrations outside the
author's domain expertise (see the disclaimer above). Repositories marked
**(private)** are not yet available. They are currently only a few hundred lines
of test code, each testing a single simulation type (or, for
`dev_couple_matrix`, pairings of the existing codes).

### Core

| Repository | What it can do today |
|---|---|
| [SOIL](https://github.com/SueHeir/soil) | Particle substrate: extensible structure-of-arrays particle storage, fixed/periodic/shrink-wrapped/triclinic boxes, MPI domain decomposition with migration and ghost exchange, binned neighbor search, groups and regions, and optional velocity-Verlet, thermo output, dumps and restart. Owns no physics. |
| [DIRT](https://github.com/SueHeir/dirt) | DEM code on SOIL: granular contact mechanics, walls, bonded particles, rigid multisphere clumps, fibers, particle insertion and loading, and MPI runs, with a validation ledger of analytical, cross-code and empirical checks. |
| [FIELD](https://github.com/SueHeir/field) | Mesh substrate: a finite-volume mesh trait family with a structured `UniformMesh`, per-cell field storage and registry, MPI decomposition with face and corner halo exchange, periodic axes, particle↔grid transfer shape functions, and a partition directory for coupled runs. Owns no equations. |

### Method demonstrations

| Repository | What it can do today |
|---|---|
| [dev_field_efvm](https://github.com/SueHeir/dev_field_efvm) | Compressible Euler/Navier–Stokes finite volume on FIELD: ideal-gas EOS, Rusanov and HLLC fluxes with MUSCL reconstruction, explicit Euler and SSP-RK3 stepping, viscous terms, boundary conditions, and a sharp-interface immersed boundary for analytic bodies. |
| [dev_soil_sph](https://github.com/SueHeir/dev_soil_sph) | Granular SPH on SOIL with an elasto-viscoplastic μ(I) Drucker–Prager stress update. Passes μ(I) recovery, rest/hydrostatic stability and footpad sinkage gates; the column-collapse sweep misses the reference run-out for some aspect ratios. |
| [dev_soil_peri](https://github.com/SueHeir/dev_soil_peri) | Bond-based peridynamics on SOIL: constant-micromodulus bonds, critical-stretch breakage and damage, with bar-tension and pre-notched plate mode-I fracture examples. Single-process only. |
| dev_soil_dsmc (private) | DSMC for rarefied gases on SOIL: NTC collisions with VHS/hard-sphere cross sections, diffuse and specular walls, moment and wall-flux sampling. Collision-rate, Couette and Fourier test cases; about 540 new lines. |
| dev_soil_mpm (private) | Explicit material point method using SOIL for material points and FIELD for the grid: P2G, grid update, MUSL G2P with FLIP/PIC, linear elasticity. Validated on the 1-D vibrating bar (second-order period convergence); about 470 new lines. |
| dev_soil_pic (private) | Electrostatic particle-in-cell on SOIL and FIELD: cloud-in-cell deposit and gather, conjugate-gradient Poisson solve via `grass_linalg`, leapfrog push. Validated on Langmuir oscillation and two-stream instability growth; about 560 new lines. |
| dev_field_lbm (private) | Lattice Boltzmann on FIELD: D2Q9 and D3Q19, BGK and TRT collisions, forcing, bounce-back walls, MPI. Validated on lid-driven cavity, Taylor–Green decay and Poiseuille flow; about 550 new lines. |
| dev_field_fem (private) | Matrix-free Q1 finite elements on FIELD for scalar Poisson on a box with Dirichlet data, solved with CG over MPI. Passes patch test and manufactured-solution second-order convergence; no preconditioner, elasticity or time dependence yet. |

### Couplings

| Repository | What it can do today |
|---|---|
| [dev_couple_dem_cfd](https://github.com/SueHeir/dev_couple_dem_cfd) | Couples SOIL/DIRT grains to dev_field_efvm through `grass_multi` with Wen–Yu/Gidaspow drag and an equal-and-opposite momentum sink. Fluidized-bed minimum fluidization velocity is within 4.5% of Wen & Yu; includes a routed 3 DEM + 2 CFD rank MPI test. |
| [dev_couple_sph_cfd](https://github.com/SueHeir/dev_couple_sph_cfd) | Couples dev_soil_sph to dev_field_efvm: a packed-bed drag/pressure-gradient seam against an imposed gas velocity, with a Wen–Yu comparator. It does not advance a CFD solution. |
| dev_couple_matrix (private) | Runs the codes above together, unmodified, as `grass_multi` sub-Apps and tests each pair against a known answer: particle–particle contact (DEM, SPH, peridynamics), dusty-box drag for five particle codes in an efvm gas, particles and FVM sources into FEM gravity, a 2:1 FVM–FVM interface, coupled restart, and split MPI. |

Because the layers expose typed state and scheduled behavior, they can be
composed into larger applications. Those application-specific exchanges and
their scientific validation belong outside GRASS itself.

## License

MIT OR Apache-2.0
