# How the stack fits together

This is the canonical explanation of the stack — the one page the GRASS, SOIL,
and DIRT READMEs all point at. If you have landed here from a README, this is the
long version.

The stack is a framework tier with **two substrate branches** — one particle, one
mesh — each carrying its own physics tier on top. Lower tiers never depend on
higher ones, and each tier knows nothing about the tiers above it:

```
GRASS    framework: App, Plugin, Scheduler, IO, coupling      (no particles, no mesh)
  ├─ SOIL    substrate: Atom, domain decomposition, comm, neighbor lists   (no physics)
  │    └─ DIRT   DEM physics: contact, bonds, walls, clumps
  └─ FIELD   substrate: Mesh, FieldData, halo, AMR                         (no equations)
       └─ dev_field_efvm  physics: compressible CFD (Riemann/EOS/IBM)  — in progress
```

The particle branch (SOIL → DIRT) is the fully landed, LAMMPS-validated one, and
most of this page walks it through in detail because it is the worked-out
example. The mesh branch (FIELD → dev_field_efvm) is the same shape one level
over — FIELD is to a mesh what SOIL is to particles — and it already carries a
landed *implicit* proof; see [The mesh branch](#the-mesh-branch) below.

The one-sentence version, worded the same everywhere these repos describe
themselves:

> GRASS gives you the `App`/scheduler/coupling; SOIL turns that into a parallel
> particle substrate via one `AtomData` contract; DIRT is the proof that a full
> LAMMPS-validated physics tier rides it — and the same seams are open for
> dev_soil_sph, dev_soil_peri, or your own method.

## The tiers

| tier | repo | owns | knows nothing about |
|---|---|---|---|
| **GRASS** | [grass](https://github.com/SueHeir/grass) | `App`, `Plugin`, the DI scheduler, TOML config/IO, MPI, cross-solver coupling | particles, mesh, physics, discretization |
| **SOIL** | [soil](https://github.com/SueHeir/soil) | base `Atom`, the `AtomData` registry, domain decomposition, ghost/halo comm, atom migration, neighbor lists | contact forces, bonds, damage — any particle *method* |
| **DIRT** | [dirt](https://github.com/SueHeir/dirt) | Hertz–Mindlin contact, rolling/twisting, bonds, walls, clumps, heat — validated against LAMMPS and closed-form theory | — (it is a top tier) |
| **FIELD** | [field](https://github.com/SueHeir/field) | `UniformMesh`, `FieldData`, halo exchange, AMR — the mesh/Eulerian counterpart to SOIL | fluxes, EOS, boundary conditions — any mesh *equations* |
| **dev_soil_sph** *(in progress)* | [dev_soil_sph](https://github.com/SueHeir/dev_soil_sph) | granular SPH (`mu(I)` continuum) on SOIL | — (it is a top tier) |
| **dev_soil_peri** *(in progress)* | [dev_soil_peri](https://github.com/SueHeir/dev_soil_peri) | bond-based peridynamics on SOIL | — (it is a top tier) |
| **dev_field_efvm** *(in progress)* | [dev_field_efvm](https://github.com/SueHeir/dev_field_efvm) | compressible CFD on FIELD — Riemann solvers, EOS, immersed boundaries | — (it is a top tier) |

- **GRASS — the framework.** You don't write a `main` loop: you register state as
  **resources** and step logic as **systems** (functions that declare their
  read/write sets by argument type), bundle them into **plugins**, and let the
  dependency-injection **scheduler** order and run them. `grass_multi` couples
  several whole solvers under one parent, in-process or across MPI binaries. GRASS
  is agnostic to the entire discretization paradigm — and that is no longer
  hypothetical: FIELD's `fem_poisson` example hosts an implicit P1 finite-element
  Poisson solve (a single global `K u = b`, no particles at all) on this same
  scheduler, converging at 2nd order (observed L² order **1.995** vs. theory
  2.000). See [The mesh branch](#the-mesh-branch).
- **SOIL — the particle substrate.** SOIL turns the framework into a *parallel
  particle* layer. It owns the base `Atom` and every piece of machinery a
  Lagrangian method needs regardless of physics — domain decomposition, ghost
  exchange, migration, neighbor lists — and knows nothing about which force law
  runs on top. It is agnostic to the particle *method*: the same substrate must
  serve MD, SPH, peridynamics, or DEM.
- **DIRT — the physics proof.** DIRT is a full granular-DEM engine that rides
  SOIL and adds only the granular physics. It exists partly as proof that a
  serious, LAMMPS-cross-checked physics tier fits on the substrate through one
  narrow contract — which means *your* method can too.

## The seam: one `AtomData` contract

The reason the tiers stay cleanly separated is a single contract at the
SOIL↔physics seam. A physics tier registers its per-particle state as an
`AtomData` column and tags each field with how it moves across ranks
(`#[forward]` to ghosts, `#[reverse]` accumulated back to owners, `#[zero]`
cleared each step). SOIL then carries that column through **every** migration,
ghost exchange, permutation, and restart automatically — without knowing what the
data *means*.

Because that contract is physics-agnostic, the *same* substrate can carry a
completely different physics — SPH, peridynamics, your own force law — with **no
change to SOIL**. DIRT is simply the first, fully worked-out tenant. The
[SOIL AtomData contract](https://sueheir.github.io/soil) is the authoritative
description; DIRT's [stack overview](https://sueheir.github.io/dirt/stack/overview.html)
walks one DEM timestep end to end and shows exactly which steps are substrate
(SOIL moving columns) and which are physics (DIRT deciding their values).

## The mesh branch

FIELD and dev_field_efvm are the mesh branch. Particles are one substrate branch
off GRASS; **mesh** is the other. GRASS knows nothing about *either* — no
positions and neighbors, and no cells and fluxes — so a mesh/Eulerian substrate
rides it the same way SOIL does.

- **FIELD — the mesh substrate.** [FIELD](https://github.com/SueHeir/field) is to
  a mesh what SOIL is to particles: it owns `UniformMesh`, `FieldData`, halo
  exchange, and AMR, and is **agnostic to the equations** — it knows nothing about
  fluxes, EOS, or boundary conditions, exactly as SOIL knows nothing about force
  laws. That symmetry is the point: the two substrates are siblings, not layers.
- **An early mesh example — `fem_poisson`.** FIELD's `fem_poisson` example solves
  steady-state Poisson with continuous P1 finite elements as a *single global
  sparse solve* `K u = b` — every unknown coupled through one matrix, no
  timestepping — expressed as an ordinary `Assemble → Solve → Validate` schedule
  on GRASS. Method of manufactured solutions gives a mean observed L² order of
  **1.995** against the theoretical 2.000. GRASS supplies the schedule and
  resources; *you* bring the sparse solver (`fem_poisson` uses `nalgebra-sparse`).
  The mesh side of the ecosystem is still being built out, so this is an early
  worked example, not a claim of broad discretization coverage.
- **dev_field_efvm — the mesh physics tier (in progress).** The particle
  branch's landed physics tier is DIRT; the mesh branch's CFD counterpart is
  **dev_field_efvm**, compressible CFD (Riemann solvers, EOS, immersed
  boundaries) as GRASS plugins riding FIELD. It is being ported — named here as
  direction, not yet as validated evidence. `fem_poisson` is an early worked
  example on the mesh path; dev_field_efvm is the physics that will ride it.

## Where to start

- **You want to run granular simulations.** Start at
  [DIRT](https://github.com/SueHeir/dirt) — it is the batteries-included physics
  tier, with a zero-Rust config runner and a validated benchmark suite. You never
  need to check out GRASS or SOIL; DIRT pulls them in during the build.
- **You want to write your own particle method** (a new force law, an SPH kernel,
  a peridynamic bond model). Start at [SOIL](https://github.com/SueHeir/soil):
  learn the `AtomData` contract, then write your physics as plugins on the
  substrate. The [SOIL book](https://sueheir.github.io/soil) has a
  write-your-own-physics tutorial.
- **You want to write a solver that isn't particles at all** (a mesh sweep, an
  implicit assemble-and-solve, a coupled multi-physics driver). For a structured
  mesh, start at [FIELD](https://github.com/SueHeir/field) — the mesh substrate,
  where `fem_poisson` is a worked implicit-solve example. For something with no
  substrate at all, start straight at [GRASS](https://github.com/SueHeir/grass):
  the App/Plugin/scheduler model and coupling primitives are all you need. See
  [Write Your Own Solver](../tutorial/write-your-own-solver.html).

## Why split it this way

Three payoffs justify splitting framework, substrate, and physics:

1. **Agnosticism you can trust.** Each tier's contract is enforced by review: no
   physics leaks into SOIL, no particles leak into GRASS. That is what lets a new
   method reuse the substrate unchanged.
2. **A transferable mental model.** The schedule, the halo exchange, the system
   signatures are the same whether you are reading DIRT's DEM or writing a CFD
   code on GRASS. Learn the seams once and they carry across solvers.
3. **Honest layering.** DIRT's validation story (LAMMPS cross-checks, closed-form
   agreement, benchmarks that are red on purpose) only makes sense because the
   physics is cleanly separated from the plumbing it rides on.
