# grass_precice

[preCICE](https://precice.org/) integration for the
[GRASS](../../README.md) framework — wraps `precice::Participant` as a
[`grass_app`](../grass_app/) plugin so each binary in a preCICE coupling
runs as a normal `App` with one extra plugin.

> **Status — 2026-04-28:** the `MultiPhysicsApp`-based "Pattern B"
> Coupler that used to live here was deleted alongside the
> [convention layer](../grass_multi/). The crate now exposes only
> the participant plugin (formerly "Pattern A"). The plugin / participant
> code itself hasn't been retested against real preCICE since the rework
> — treat the example below as the intent, not a verified recipe.

## Build

`grass_precice` requires the `precice` Cargo feature **and** a working
preCICE C++ install (libprecice via pkg-config). Without the feature,
the crate exports stub types that panic with a helpful message at first
use — so the workspace stays buildable on machines without preCICE.

```sh
# Workspace builds clean without preCICE installed:
cargo build --workspace

# Enable the real implementation when libprecice is available:
brew install precice                    # macOS
# or apt install libprecice-dev         # Ubuntu/Debian
cargo build --features grass_precice/precice
```

## Surface

| item | what it does |
|---|---|
| [`PreciceParticipantPlugin`](src/plugin.rs) | one-liner plugin that registers a `PreciceParticipant` resource and slots `Initialize`, `Advance`, `CheckDone` systems into the schedule |
| [`PreciceParticipant`](src/participant.rs) | thin `Send + Sync` wrapper around `precice::Participant` |
| [`PreciceSchedule`](src/schedule.rs) | `Write → Advance → Read` phases. You write the `Write` and `Read` systems; the plugin owns `Advance`. |
| [`PreciceTimeStep`](src/plugin.rs) | App resource: dt to pass to `participant.advance(dt)`. Pull `participant.get_max_time_step_size()` (and optionally clip against your local CFL) into this resource each iter. |

## Example shape

```rust
use grass_app::prelude::*;
use grass_precice::{
    PreciceParticipant, PreciceParticipantPlugin, PreciceSchedule, PreciceTimeStep,
    system_precice_advance, system_precice_check_done, system_precice_initialize,
};

let mut app = App::default();
app.add_plugins(MyCfdPlugins::from_config(cfg.cfd));

app.add_plugins(PreciceParticipantPlugin::new(
    "FluidSolver",
    "precice-config.xml",
));
app.add_resource(PreciceTimeStep::default());

// Mesh setup runs once after the participant is constructed.
app.add_setup_system(setup_wetted_surface_mesh, MySetup::PreciceMesh);
app.add_setup_system(system_precice_initialize, MySetup::PreciceInit);

// Per-step write/read systems you provide:
app.add_update_system(pick_dt_for_precice, CfdSchedule::Setup);
app.add_update_system(write_pressure_to_precice, PreciceSchedule::Write);
app.add_update_system(system_precice_advance, PreciceSchedule::Advance);
app.add_update_system(read_forces_from_precice, PreciceSchedule::Read);
app.add_update_system(system_precice_check_done, PreciceSchedule::Read);

app.start();
```

Each binary runs its own `App` with this plugin; preCICE handles the
routing, mapping, and synchronization between them.

## See also

- [preCICE documentation](https://precice.org/)
- [`precice` Rust bindings](https://docs.rs/precice/)
