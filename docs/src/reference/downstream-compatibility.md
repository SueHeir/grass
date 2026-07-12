# Downstream compatibility contract

GRASS is only useful if a scheduler change survives the solvers built on it.
Before a GRASS mainline release, test the sibling stack as a coherent checkout:

| repository | contract exercised |
|---|---|
| SOIL | particle resources, setup stages, serial/MPI communication |
| FIELD | mesh resources, partition ownership, halo communication |
| DIRT | multi-stage input, DEM plugins, current SOIL revision |
| dev_field_efvm | FIELD schedule and numerical validation tests |
| dev_soil_peri / dev_soil_sph | non-DEM particle methods |
| dev_couple_dem_cfd | parent coupling plus topology-driven 3+2 routed MPI |
| dev_couple_sph_cfd | parent resource access and local coupled step |

The minimum gate is `cargo test --workspace` in each repository. Coupling changes
also require the live distributed DEM-CFD gates:

```bash
cargo test -p dem_cfd --features mpi-routing --test routed_3x2 -- --nocapture
cargo test -p dem_cfd --features mpi-routing --test routed_trajectory_3x2 -- --nocapture
```

Passing a downstream default build is not enough when its MPI feature is gated
off or its lockfile points to an older GRASS/SOIL commit. The audit must record
the resolved dependency revisions and deliberately test the coherent sibling
stack.

## Current composition rules

- Scientific libraries define resource types; an App holds resource instances.
- Parent coupling systems own cross-solver `Multi` / `MultiRes` access.
- Child systems use ordinary `Res` / `ResMut` for their own state.
- Whole-step coupling uses child ticks. Coupling inside a loop, branch, or
  rollback region uses an exported scheduler seam and parent `resume()` calls.
- A single TOML topology selects local or split-MPI roles for the same binary.
- Domain libraries decide ownership (for example FIELD's
  `PartitionDirectory`); GRASS transports addressed opaque records.

The runnable source links in the main README and coupling tutorials are part of
this contract. Update them with the implementation, not after it.

## Coordinated public release

With all nine repositories checked out as siblings, run:

```bash
grass/ci/public-release.sh --check
grass/ci/public-release.sh --push
```

The check refuses dirty trees, private URLs, sibling-only dependency paths, or
a GitHub branch containing history absent locally. `--push` uses normal
fast-forward pushes in dependency order and never force-pushes.
