# grass_scheduler

A lightweight, Bevy-inspired dependency-injection scheduler for explicit, time-stepping particle and grid solvers.

## What It Does

Systems are plain functions whose parameters declare the resources they need. The scheduler resolves typed resources at startup and executes systems in user-defined phase order each timestep. It supports `before`/`after` ordering, run conditions, simulation states and stages, composite `SystemGroup`s, and an optional `Schedule` tree for hierarchical loop/branch control.

This crate is part of the GRASS framework; it knows nothing about particles or physics.

## Key Types

| Item | Purpose |
| --- | --- |
| `Scheduler` | Owns resources and registered systems; runs them each timestep. |
| `ScheduleSet` | Trait for user-defined execution phases. Implement by hand or `#[derive(ScheduleSet)]`. |
| `Res<T>` / `ResMut<T>` | Read-only / exclusive resource access (validated before run). |
| `Local<T>` | Per-system persistent state, unshared with other systems. |
| `Option<Res<T>>` | Optional resource access; the system still runs if the resource is missing. |
| `SystemExt` | Adds `.run_if()`, `.label()`, `.before()`, `.after()` to systems. |
| `SystemGroup` | Bundles systems into one composite unit with inner phase ordering and optional looping. |
| `Schedule` / `ScheduleBuilder` | Tree of `Phase` / `Sequence` / `Loop` / `Branch` nodes for hierarchical run-loop control. |
| `Snapshot<T>` | Single-slot save buffer for per-resource opt-in reversibility. |
| `CurrentState<S>` / `NextState<S>` | State-machine resources, with `in_state()` / `on_enter_state()` run conditions. |

## Ordering & Conditions

Within a phase, systems are topologically sorted by `.before()` / `.after()` constraints. Run conditions gate execution:

```rust
struct Timestep { current: u64 }

fn every_n_steps(n: u64) -> impl Fn(Res<Timestep>) -> bool {
    move |ts: Res<Timestep>| ts.current % n == 0
}

scheduler.add_update_system(
    dump_state.run_if(every_n_steps(1_000)),
    Phase::PostStep,
);
```

## Quick Example

```rust
use grass_scheduler::prelude::*;

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    Integrate,
    Force,
    Finalize,
}

struct State { pos: Vec<[f64; 3]>, vel: Vec<[f64; 3]>, force: Vec<[f64; 3]> }

fn integrate(mut s: ResMut<State>) { /* first half-step */ }
fn compute_forces(mut s: ResMut<State>) { /* force loop */ }
fn finalize(mut s: ResMut<State>) { /* second half-step */ }

let mut scheduler = Scheduler::default();
scheduler.add_resource(State { pos: vec![], vel: vec![], force: vec![] });
scheduler.add_update_system(integrate, Phase::Integrate);
scheduler.add_update_system(compute_forces, Phase::Force);
scheduler.add_update_system(finalize, Phase::Finalize);

scheduler.run(); // executes phases in order each timestep
```

## System Groups

`SystemGroup` bundles multiple systems into a single composite unit with its own inner phase ordering and optional looping. Because it implements `IntoSystem`, it gets the same API as ordinary systems — `.run_if()`, `.label()`, `.before()`, `.after()`.

```rust
#[derive(Clone, Copy, Debug, ScheduleSet)]
enum RelaxPhase { ComputeForces, MoveParticles, CheckOverlap }

scheduler.add_update_system(
    SystemGroup::new("overlap_relaxation")
        .add_system(compute_repulsion, RelaxPhase::ComputeForces)
        .add_system(move_particles,    RelaxPhase::MoveParticles)
        .add_system(measure_overlap,   RelaxPhase::CheckOverlap)
        .loop_while(has_overlap, 100),   // iterate until resolved or 100 cap
    Phase::PostStep,
);
```

Each outer timestep runs the inner systems in phase order, repeating until `has_overlap` returns `false` or the iteration cap is hit. Groups may nest, at a small per-call overhead.

## Schedule Trees

For schedules with iteration or state-conditional fragments, build a `Schedule` tree and install it with `Scheduler::set_schedule`. Phases compose by `then`; `loop_until` re-executes a body until a condition flips (with `OnMax::AcceptUnconverged` or `OnMax::Panic` on hitting `max_iters`); `branch` provides first-match-wins state-conditional dispatch. `loop_with_rollback` pairs a loop with a rollback fragment (`OnMax::Rollback`) that runs once on non-convergence.

```rust
use grass_scheduler::{OnMax, Schedule};

let schedule = Schedule::builder()
    .then::<CouplingPre>()
    .loop_until(check_converged, 20, OnMax::Panic, |body| {
        body.then::<DemTick>()
            .then::<CfdTick>()
            .then::<ResidualUpdate>()
    })
    .then::<CouplingPost>()
    .build();

scheduler.set_schedule(schedule);
```

Schedulers that never call `set_schedule` keep the flat `(namespace, index)` ordering.

## Reversibility

`Snapshot<T>` opts a resource into reversibility. Register `Snapshot::<T>::default()` alongside the resource, then place `snapshot_resource::<T>()` / `restore_resource::<T>()` systems where save/restore points are needed (e.g. a loop accepting a tentative step that may be rolled back). The default helpers require `T: Clone`; for expensive clones, fill the snapshot slot with a custom save/restore pair.

## States & Stages

`CurrentState<S>` / `NextState<S>` drive state-machine simulations; `apply_state_transitions::<S>` applies queued transitions, and `in_state()` / `on_enter_state()` gate systems by state. Named stages (`set_stage_names`) support `in_stage()`, `on_enter_stage()`, `first_stage_only()`, and `check_stage_advance` for multi-stage runs.

## Diagnostics

Per-system wall-clock timing is recorded automatically and printed as a sorted breakdown at the end of a run, with `SystemGroup`s shown as indented sub-breakdowns. Call `enable_schedule_print()` to also emit a Graphviz DOT file (`schedule.dot`) of the execution graph, with groups as subgraph clusters and loop back-edges.

See the inline crate documentation for full details on system parameters, labels, and run conditions.

## License

MIT OR Apache-2.0
