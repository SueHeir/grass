# Write Your Own Solver

This tutorial builds a complete explicit time-stepping solver on GRASS alone —
no particles, no physics crates — to show the framework standalone. We'll
integrate a cloud of point masses falling under gravity with semi-implicit Euler,
and stop when they hit the floor.

It mirrors the structure every GRASS-based solver shares: **resources hold state,
systems transform it, plugins wire it up, the scheduler runs it.**

## 1. Define the state as resources

```rust
use grass_app::prelude::*;
use grass_scheduler::prelude::*;

/// All bodies, struct-of-arrays style.
struct Bodies {
    pos: Vec<[f64; 3]>,
    vel: Vec<[f64; 3]>,
}

/// The clock.
struct Clock { t: f64, dt: f64 }

/// A constant.
struct Gravity(f64);
```

## 2. Write the systems

Each system is a plain function; its parameters say what it reads and writes.

```rust
fn apply_gravity(mut bodies: ResMut<Bodies>, g: Res<Gravity>, clock: Res<Clock>) {
    for v in bodies.vel.iter_mut() {
        v[2] += g.0 * clock.dt;        // semi-implicit: update velocity first
    }
}

fn integrate(mut bodies: ResMut<Bodies>, clock: Res<Clock>) {
    let dt = clock.dt;
    // Split the borrow so we can read vel while writing pos.
    let Bodies { pos, vel } = &mut *bodies;
    for (p, v) in pos.iter_mut().zip(vel.iter()) {
        p[0] += v[0] * dt;
        p[1] += v[1] * dt;
        p[2] += v[2] * dt;
    }
}

fn advance_clock(mut clock: ResMut<Clock>) {
    clock.t += clock.dt;
}
```

## 3. Name the phases

A `ScheduleSet` enum names the phases of one timestep. Declaration order plus the
systems' data dependencies fix the execution order.

```rust
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Step {
    Forces,     // apply_gravity
    Integrate,  // integrate, advance_clock
    Stop,       // stop_when_landed (added in step 4)
}
```

The variant order is load-bearing: it is the per-step execution order. `Stop`
comes last so it tests the positions *after* this step's integration. See
[Derive Macros](../reference/derives.md#scheduleset) for the full `ScheduleSet`
contract.

## 4. Decide when to stop

The `start()` loop keeps calling `run()` until a system sets the scheduler's
state to `End`. That state lives on the `SchedulerManager` resource, so a stop
condition is just a system that takes it `ResMut` and flips the flag when the run
is over — here, when every body has fallen below the floor:

```rust
fn stop_when_landed(
    bodies: Res<Bodies>,
    mut manager: ResMut<SchedulerManager>,
) {
    if bodies.pos.iter().all(|p| p[2] <= 0.0) {
        manager.state = SchedulerState::End;
    }
}
```

`SchedulerManager` and `SchedulerState` come from
`grass_scheduler::prelude::*`, already imported above. Register this system in a
late phase so it sees each step's final positions (we add a `Stop` phase in
step 5). If you drive the loop yourself instead of calling `start()`, poll
`app.is_done()` — it reports the same `state == End` — and remember to call
`app.run_cleanup()` after the loop.

## 5. Bundle it as a plugin

```rust
struct FallingBodiesPlugin {
    count: usize,
}

impl Plugin for FallingBodiesPlugin {
    fn build(&self, app: &mut App) {
        app.add_resource(Bodies {
            pos: vec![[0.0, 0.0, 10.0]; self.count],
            vel: vec![[0.0, 0.0, 0.0]; self.count],
        });
        app.add_resource(Clock { t: 0.0, dt: 1e-3 });
        app.add_resource(Gravity(-9.81));

        app.add_update_system(apply_gravity, Step::Forces);
        app.add_update_system(integrate, Step::Integrate);
        app.add_update_system(advance_clock, Step::Integrate);
        app.add_update_system(stop_when_landed, Step::Stop);
    }
}
```

## 6. Run it

```rust
fn main() {
    let mut app = App::new();
    app.add_plugins(FallingBodiesPlugin { count: 100 });
    app.start();
}
```

That is a complete solver. You wrote three resources, three systems, and a
plugin; the framework gave you the lifecycle, the per-step loop, the scheduling,
and the dependency injection.

## Where to go from here

- To make the bodies **interact** (forces between them) and run in **parallel
  across MPI ranks**, you want the particle substrate — that is exactly what
  [SOIL](https://sueheir.github.io/soil) adds on top of this framework. Its
  [Write Your Own Particle Physics](https://sueheir.github.io/soil/tutorial/write-your-own-physics.html)
  tutorial picks up where this one leaves off.
- To **couple** several solvers (this one to a fluid solver, say), see
  [MPI and Coupling](../model/mpi-coupling.md#tutorial-coupling-two-solvers-in-process),
  which walks through coupling two sub-Apps under one parent with `add_subapp` /
  `add_remote_subapp` and the `Wire` / `Transport` primitives.
- For periodic terminal output, file dumps, and TOML-driven multi-stage runs,
  add the `grass_io` plugins — see [I/O and Configuration](../model/io.md).
