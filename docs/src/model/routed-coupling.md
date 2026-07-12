# Routed Coupling Without Geometry in GRASS

Domain-decomposed couplings need to move interface records to the ranks that
own their targets. GRASS owns the communication mechanism, but not the rule
that turns a position, surface patch, or interpolation support into a rank.

The boundary is:

```text
SOIL/FIELD expose decomposition data
            ↓
coupling package builds scientific routes
            ↓
GRASS transports opaque routed records
```

## Responsibilities

GRASS knows:

- the local and peer role sizes;
- role-local source and destination ranks;
- stable opaque entity IDs;
- coupling epochs;
- variable-size payload framing;
- deterministic delivery order;
- communicator isolation and transport errors.

GRASS does **not** know:

- particle coordinates or radii;
- cells, faces, kernels, or interpolation stencils;
- periodic geometry or boundary ownership;
- whether a payload represents force, heat, mass, or fracture traction;
- whether one entity should route to one rank or several.

FIELD owns queries such as `owner_rank(point)` or
`overlapping_ranks(bounds)`. SOIL owns particle identity, position, support,
and its current owning DEM rank. A coupling package combines those facts and
states the conservation policy.

## Unequal-role example

For three DEM ranks and two CFD ranks decomposed along x:

```text
DEM 0: [0.00, 0.33)       CFD 0: [0.00, 0.50)
DEM 1: [0.33, 0.66)       CFD 1: [0.50, 1.00)
DEM 2: [0.66, 1.00)
```

The route graph is determined by current particle positions and support, not
by pairing rank numbers:

```text
DEM 0 → CFD 0
DEM 1 → CFD 0 and CFD 1
DEM 2 → CFD 1
```

A containing-cell map sends a particle at x=0.49 only to CFD 0. A finite
kernel crossing x=0.5 may send weighted contributions to both CFD ranks. The
coupling package makes that choice. Returned forces use the stable entity ID
and recorded DEM owner.

## Routed exchange API

`RoutedRoleExchange` accepts records addressed to peer *role ranks*:

```rust,ignore
let exchange = launch.into_routed_exchange();
let incoming = exchange.exchange(
    CouplingEpoch(step),
    &[
        RoutedPayload::new(cfd_owner, EntityId(particle_id), packed_kinematics),
    ],
)?;
```

Delivery order is deterministic: peer source role-rank order, then the
original order within each source. `CouplingEpoch` fails closed when peers are
executing different exchanges. `EntityId` is opaque to GRASS and may identify
an entity or one contribution to an entity.

The initial implementation deliberately filters the already-validated root
bridge: every rank receives the peer frames and keeps only records addressed
to itself. This is the correctness oracle. A later owner-to-owner backend can
replace it behind the same routed contract and must reproduce its delivered
records and scientific conservation residuals.

## Required coupling validation

A coupling package using routed exchange should demonstrate:

1. ownership and overlap at partition boundaries;
2. empty and nonuniform partitions;
3. one-to-many contributions and deterministic reduction;
4. local versus unequal-rank MPI trajectory parity;
5. conservation of the relevant integral or interface work;
6. rejection of stale epochs and invalid destinations;
7. unchanged results when the root-bridge oracle is replaced by sparse MPI.

Route construction belongs with the scientific coupling because only it can
state which of those quantities must be conserved. GRASS guarantees that the
resulting opaque plan is executed in the correct communication domain.
