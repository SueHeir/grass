# Case Study: Grass → SOIL → DIRT

This is a concrete case study of three libraries with different jobs, rather
than a claim that a framework makes every scientific method interchangeable.
The present, evidence-backed path is **Grass → SOIL → DIRT**: Grass supplies
composition infrastructure, SOIL supplies particle-method infrastructure, and
DIRT supplies DEM physics. DIRT's validation is evidence for that DEM path; it
is not validation of CFD, SPH, or peridynamics.

## The dependency and ownership picture

```text
                         application / coupling owner
                         parent schedule + exchange contract
                                      │
                        grass_multi: Port<T>, SubApps, transport
                                      │
                                      ▼
 GRASS ────────────────────────────────────────────────────────────────
 App • Plugin • typed resources/systems • scheduler • I/O • MPI
 knows neither particles, meshes, nor a force law
         ▲                                  ▲
         │ depends on                       │ depends on
 SOIL ───┴─────────────────────────     other Grass libraries/solvers
 Atom • AtomData columns • neighbour lists • migration • ghost/reverse comm
 knows particles, but not a particle method or force/material law
         ▲
         │ depends on
 DIRT ───┴─────────────────────────────────────────────────────────────
 DEM contact • rotational state • bonds • walls • clumps • materials

Dependency direction: DIRT → SOIL → GRASS.  A cross-substrate seam is a
sibling application/repository depending on both partners, never a dependency
from GRASS, SOIL, or one solver into the other.
```

The arrows point toward infrastructure. This is more than a naming convention:
SOIL's `soil_core` declares dependencies on Grass crates
([manifest](http://192.168.0.170:8082/SueHeir/soil/src/commit/aaa39caac2ff5066d998c30165ae0422c496899e/crates/soil_core/Cargo.toml#L21-L30)),
whereas DIRT's physics crates use SOIL's `Atom` and `AtomDataRegistry`
([granular manifest](http://192.168.0.170:8082/SueHeir/dirt/src/commit/34c15b18ab0d69f1b3eefb2a22f7efec37a02beb/crates/dirt_granular/Cargo.toml#L12-L19)).
Neither is a Grass dependency. The direction is what lets a library author
replace or add physics without making the framework a particle or DEM package.

## Three public contracts, three owners

### 1. Grass owns composition, not a discretization

At the public boundary, a Grass library owns typed resources and registers
systems/plugins; the scheduler receives the declared `Res<T>`/`ResMut<T>`
accesses. The [composition contract](../reference/library-composition-contract.md)
states the normative boundary and links its executable checks. In particular,
the current scheduler is exercised by a non-particle
[heat-diffusion example](http://192.168.0.170:8082/SueHeir/grass/src/commit/2b432067596dcebe5138a59f4b3483014087eae2/examples/heat_diffusion_1d/main.rs#L65-L160),
so particles are not part of Grass's public model.

This gives a new library a small extension seam: define its own resources,
schedule sets, systems, and plugins. A plugin may also provide a TOML snippet
showing its configuration section and defaults through
[`Plugin::default_config`](http://192.168.0.170:8082/SueHeir/grass/src/commit/2b432067596dcebe5138a59f4b3483014087eae2/crates/grass_app/src/plugin.rs#L171-L176).

### 2. SOIL owns particle plumbing, not a force law

SOIL's public seam is `AtomData`: a physics library registers typed per-atom
columns, annotating how each moves (`forward` to ghosts, `reverse` back to an
owner, `zero` each step). The actual trait includes migration, ghost, reverse,
zero, and permutation hooks
([source](http://192.168.0.170:8082/SueHeir/soil/src/commit/aaa39caac2ff5066d998c30165ae0422c496899e/crates/soil_core/src/atom.rs#L130-L169));
SOIL's communication layer then moves registered extensions during exchange
([implementation](http://192.168.0.170:8082/SueHeir/soil/src/commit/aaa39caac2ff5066d998c30165ae0422c496899e/crates/soil_core/src/comm.rs#L1230-L1245)).

The owner of a new particle method therefore adds its own `AtomData` columns and
physics plugins. It does **not** add DEM-only fields to SOIL's base `Atom`. A
useful already-runnable counterexample to “SOIL means DEM” is SOIL's
[Lennard–Jones dimer](https://github.com/SueHeir/soil/tree/aaa39caac2ff5066d998c30165ae0422c496899e/crates/soil_verlet/examples/lj_dimer.rs),
which is a non-DEM method on the substrate. This establishes a substrate seam,
not scientific validation of every future particle method.

### 3. DIRT owns DEM meaning and evidence

DIRT fills that seam with DEM-specific state and laws: contact, rotational
quantities, material tables, bonds, walls, and clumps. For example,
[`DemAtom`](http://192.168.0.170:8082/SueHeir/dirt/src/commit/34c15b18ab0d69f1b3eefb2a22f7efec37a02beb/crates/dirt_atom/src/lib.rs#L1187-L1235)
is DIRT code, not a new SOIL base field, and the granular plugin registers it
with the substrate's registry
([plugin implementation](http://192.168.0.170:8082/SueHeir/dirt/src/commit/34c15b18ab0d69f1b3eefb2a22f7efec37a02beb/crates/dirt_atom/src/lib.rs#L1247-L1310)).

The physics evidence belongs at this layer too. DIRT's committed validation
index records a Hertz rebound case against analytical Hertz behaviour and a
LAMMPS comparison
([case and figures](http://192.168.0.170:8082/SueHeir/dirt/src/commit/34c15b18ab0d69f1b3eefb2a22f7efec37a02beb/examples/VALIDATION.md#L186-L205)),
and records the seeded Haff-cooling ensemble as passing all 39 checks
([results](http://192.168.0.170:8082/SueHeir/dirt/src/commit/34c15b18ab0d69f1b3eefb2a22f7efec37a02beb/examples/VALIDATION.md#L770-L818)).
Those are specific DEM validations, with their stated references and gates—not
a blanket validation of the framework or substrate.

## What has been rejected at the boundary

The layering rules make failure modes concrete.

| tempting change | rejected because | correct owner/seam |
|---|---|---|
| Put radius, angular velocity, torque, Hertz overlap, bond damage, or a contact history into Grass. | Grass must still make sense for a mesh, FEM, spectral, or non-particle solver. | SOIL's particle substrate for generic state; DIRT `AtomData` for DEM-only state. |
| Put Hertz/Mindlin contact, a material table, torque, damage, or bonds in SOIL's base `Atom`. | Those choose a particle method and would burden MD, SPH, and peridynamics with DEM vocabulary. | DIRT (or another physics tier) adds a typed `AtomData` column. |
| Make DIRT import a CFD solver's internal field resource, or make CFD import `DemAtom`, to exchange drag. | The two libraries would no longer be independently useful and their private state would become an accidental API. | A coupling application/repository owns a small exchange contract and parent schedule. |
| Put a soil↔field demo in either substrate's core crate. | It makes one substrate depend on its sibling and hides the owner of exchange/termination policy. | A dedicated `dev_couple_*` repository using `grass_multi`; example-specific code remains under that repository's `examples/`. |

The last row is an architectural rule, not merely an aspiration. The Grass
composition contract requires the coupling owner to own the parent schedule and
termination policy, and requires a stable `Port<T>` rather than a consumer naming
the producer's private resource
([normative section](../reference/library-composition-contract.md#4-coupling-ownership-and-exchange-ports)).

## The coupling seam in current code

`grass_multi` supplies the cross-library vocabulary: sub-apps plus a typed
`Port<T>` interface. Its integration test deliberately couples a mesh-style
field to a particle-style integrator. The only shared value is `Flux`; each
solver retains its own private resource type, while the parent owns the ordered
`TickProducer → Couple → TickConsumer → Check` schedule
([test](http://192.168.0.170:8082/SueHeir/grass/src/commit/2b432067596dcebe5138a59f4b3483014087eae2/crates/grass_multi/tests/coupling_port.rs#L1-L178)).
That test checks the producer's conserved total, the consumer's received force,
and its closed-form position/velocity. It is the current validated proof of the
composition mechanism; it is not a DEM↔CFD validation.

For a real cross-substrate application, the shape is:

```text
particle/DEM sub-App ── expose_field::<ParticleState, Exchange> ──┐
                                                                    ├─ coupling owner
mesh/CFD sub-App     ── consume_field::<FieldState, Exchange> ────┘  schedules/ticks/checks
```

The contract type should contain only the data both sides have agreed to
exchange (for example, a sampled force, momentum source, or void fraction),
not either side's private resource. The coupling owner chooses cadence,
interpolation, convergence/termination policy, and—in remote runs—the matching
wire direction and cadence. The remote requirements are specified with their
tests in the [composition contract](../reference/library-composition-contract.md#5-remote-coupling-wire-and-errors).

`dev_couple_dem_cfd` and `dev_couple_sph_cfd` are the concrete repository seams
for SOIL↔FIELD work. They are useful examples of ownership and dependency
placement, but both include development-stage CFD/SPH partners. Their existence
does not add CFD/SPH/peridynamics validation to the DIRT evidence above.

## What is established now, and what remains a roadmap

**Established here:** Grass's typed scheduling and port-coupling test; SOIL's
particle extension and communication contract; and DIRT's cited DEM benchmarks.
Together, these are evidence that a DEM physics library can reuse a particle
substrate on a composition framework without moving DEM concepts downward.

**Not established by this case study:** production CFD validation,
cross-substrate DEM↔CFD validation, broad SPH validation, or peridynamics
validation. `dev_field_efvm`, `dev_soil_sph`, `dev_soil_peri`, and the
`dev_couple_*` applications are extension directions. They should earn their own
method-specific references, runnable checks, and measured-versus-reference
evidence before being described as validated capabilities.

## Extension checklist

1. Put scheduler/application-neutral state and scheduling in a Grass library;
   do not mention a discretization there.
2. If the method is particle based, add generic particle machinery to SOIL only
   when it serves more than one method; otherwise add physics state as that
   method's `AtomData`.
3. Keep force laws, materials, and method-specific diagnostics in the physics
   tier, alongside its validation examples.
4. When two independently useful libraries exchange data, define a small
   `Port<T>` contract and put the parent schedule in the application/coupling
   owner. If it crosses SOIL and FIELD, create a `dev_couple_*` repository.
5. Treat a roadmap method as a roadmap method until it has a cited, runnable
   validation case; do not borrow DIRT's DEM results as its evidence.
