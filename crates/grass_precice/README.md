# grass_precice

Run a grass `App` as a [preCICE](https://precice.org) participant, so it can couple to OpenFOAM, CalculiX, SU2, FEniCS or any other preCICE adapter through a normal preCICE XML configuration.

```rust
use grass_precice::{CouplingStep, PreciceAppExt, PrecicePlugin};

let mut app = App::new();
app.add_resource(Shell::new());
app.add_plugins(PrecicePlugin::participant("Solid", "precice-config.xml", 0, 1).solver_dt(1e-3));
app.precice_mesh::<Shell>("Solid-Mesh", |s| s.surface_positions());
app.precice_write::<Shell>("Solid-Mesh", "Temperature", |s| s.wall_temperature());
app.precice_read::<Shell>("Solid-Mesh", "Heat-Flux", |s, q| s.set_wall_flux(q));
app.precice_checkpoint::<Shell>();                       // implicit coupling
app.add_update_system(advance_shell, ShellPhase::Step);  // uses Res<CouplingStep>.dt
app.start();
```

## What the plugin does

| when | what |
|---|---|
| setup | defines every mesh (optionally with triangles), writes initial data if the configuration asks, calls `initialize`, saves checkpoints and reads data for the first step |
| every step, after the solver (namespace 50) | writes data, calls `advance(dt)`, restores checkpoints if preCICE repeats the window, saves them at the start of a new window, picks the next `dt` and reads data for it |
| end of coupling | calls `finalize` and stops the App |

The solver takes its step size from the `CouplingStep` resource: the smaller of the solver's preferred step and what is left of the coupling window, so subcycling inside a window happens automatically. `CouplingStep` also reports the time, the window count, the implicit iteration and whether the last step was accepted. Gate end-of-step output with the `step_accepted` run condition so repeated implicit iterations are not reported.

The participant can be a whole multi-physics App: solvers coupled inside a grass parent App (through `grass_multi` ports, mapped ports and implicit windows) appear to preCICE as one participant.

## Building

The `precice` feature links preCICE v3.2 through the official Rust bindings (`precice` crate, found with `pkg-config`). Without the feature the crate builds with no preCICE installed and exposes the `CouplingInterface` trait, which you can implement for a stand-in.

On Ubuntu 24.04, install the preCICE package from the [preCICE releases](https://github.com/precice/precice/releases):

```sh
curl -LO https://github.com/precice/precice/releases/download/v3.2.0/libprecice3_3.2.0_noble.deb
sudo apt-get install ./libprecice3_3.2.0_noble.deb
cargo test -p grass_precice --features precice
```

The bindings crate and preCICE itself are LGPL-3.0. Building with the `precice` feature links them into your binary, so the LGPL's conditions apply to binaries you distribute.

## Tests

* `tests/two_body_heat.rs` (needs `--features precice`): two grass participants, run as separate processes over preCICE sockets, exchange temperatures on non-trivial meshes under serial-implicit coupling with IQN-ILS. The converged result matches monolithic backward Euler to 1e-8 and conserves energy. A second run subcycles one participant four times per window.
* `tests/scripted.rs`: the plugin lifecycle (mesh, initial data, checkpoint save and restore, subcycling, finalize) against a scripted stand-in, without preCICE.
