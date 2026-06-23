# The Scheduler

The [`grass_scheduler`](https://github.com/SueHeir/grass/tree/main/crates/grass_scheduler)
crate is the engine `App` wraps. It is a typed-resource, dependency-injection
scheduler: systems declare what they touch by argument type, and the scheduler
orders and runs them accordingly.

## Resources

A *resource* is a piece of global state, stored by type. You register one and
reach it from systems by type:

```rust
struct SimClock { t: f64, dt: f64 }

app.add_resource(SimClock { t: 0.0, dt: 1e-3 });
```

## Systems

A *system* is a function whose parameters are `SystemParam`s — most commonly
`Res<T>` (shared read) and `ResMut<T>` (exclusive write). The scheduler injects
them from the resource store and runs every system on a single thread, one after
another, in a fully deterministic order (`run_flat`,
`grass_scheduler/src/lib.rs:1920`). There is no parallel execution: "ordering"
means the *sequence* in which systems run, never simultaneous execution. The
read/write sets a system declares are used to compute that sequence and to inject
borrows, not to dispatch work across cores.

```rust
fn advance_clock(mut clock: ResMut<SimClock>) {
    clock.t += clock.dt;
}
```

Resources live in a `Vec<RefCell<Box<dyn Any>>>`, so borrow rules are checked at
**run time, not compile time**. `Res<T>` calls `borrow()` and `ResMut<T>` calls
`borrow_mut()` on the same cell.

> **Warning: conflicting borrows of the same resource panic at run time.**
> If two systems that overlap in the same timestep both hold a `ResMut<T>` for
> the same `T` — or one holds `Res<T>` while another holds `ResMut<T>` — the
> second borrow panics with a `BorrowMutError`. The scheduler does **no** static
> conflict detection. Because a flat run executes systems strictly in sequence,
> two systems never literally run at once; the hazard is a *single* system that
> takes the same resource twice (e.g. `ResMut<T>` and `Res<T>` of the same `T`
> in its parameter list), or a custom `SystemParam` that re-borrows a cell its
> own system already holds. Split such work into separate systems, or order them
> with `.before()` / `.after()` so the borrows never coexist.

## The schedule tree

Systems are placed into a schedule built from four node kinds:

| node | meaning |
|---|---|
| `Phase` | a named set of systems that run together (ordered by data deps) |
| `Sequence` | child schedules run in order |
| `Loop` | a child schedule repeated (e.g. the per-timestep loop) |
| `Branch` | conditional sub-schedules |

You usually address phases through a `ScheduleSet` enum rather than building the
tree by hand:

```rust
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Step { Integrate, Output }

app.add_update_system(advance_clock, Step::Integrate);
```

## Run conditions

A system can be gated by a *run condition* — a predicate that decides whether it
runs this tick:

```rust
app.add_update_system(
    dump_frame.run_if(every_n_steps(100)),
    Step::Output,
);
```

A condition is itself a small system: it can take `SystemParam`s and read
resources to make its decision.

> **Warning: conditions accept at most 5 `SystemParam`s; systems accept 10.**
> The `impl_condition!` macro is generated for 0–5 parameters, while
> `impl_system!` covers 0–10. A `run_if` closure with six or more resource
> parameters silently fails to implement `IntoCondition` and surfaces as a
> confusing trait-bound error rather than a clear "too many parameters" message.
> If a condition needs more than five inputs, fold them into a single resource
> struct or precompute a flag in an earlier system.

## States and stages

For multi-phase runs (fill, then flow; load, then fracture) GRASS provides a
state machine:

- `StatesPlugin<S>` registers `CurrentState<S>` / `NextState<S>` and the
  transition system. Gate systems with `.run_if(in_state(S::Foo))`.
- `StageAdvancePlugin<S>` watches the current state and advances scheduler
  *stages*, with `#[derive(StageEnum)]` naming them.

This is the machinery the DIRT hopper example uses to switch from a *filling*
stage to a *flowing* stage at runtime.

The stage conditions — `in_stage(name)`, `on_enter_stage(name)`,
`first_stage_only()` — read `SchedulerManager::stage_name`. That field is
**only** populated by the stage driver in `grass_app`/`grass_io` (`RunPlugin`
and `StatesPlugin` together). A bare `Scheduler` with no stage driver always has
`stage_name == None`, so `in_stage(..)` evaluates to `false` every step and the
gated systems never run. The `[[run]]` TOML schema that feeds these stage names
lives in [I/O and Configuration](./io.md#runplugin-and-multi-stage-runs).

State transitions through `apply_state_transitions::<S>` are edge-triggered:
`on_enter_state(S::X)` fires exactly once, on the first step after the state
becomes `S::X`. Each decoration carries its own private `was_active` flag, so two
systems both gated on `on_enter_state(S::X)` each fire independently on that first
step — neither steals the edge from the other.

## How execution order is decided

Each timestep, every registered update system runs exactly once, in an order
computed by three layered rules — applied in this sequence:

1. **Sort by `(namespace, index)`.** Each system carries a phase whose `index`
   comes from its `ScheduleSet` variant's `to_index()`, and whose `namespace`
   defaults to `0` (see the footgun below). Systems are ordered first by
   namespace, then by phase index.
2. **Topologically sort within each `(namespace, index)` group** using Kahn's
   algorithm over the `.before()` / `.after()` constraints declared on those
   systems.
3. **Registration-order tie-break.** Systems left unordered by steps 1–2 (same
   `(namespace, index)`, no relative constraint) run in the order they were
   registered.

### Footgun: every `ScheduleSet` enum defaults to namespace 0

Phase indices come from `to_index()`, which a derived `ScheduleSet` numbers
`0, 1, 2, …` *per enum*. The namespace, however, defaults to `0` for **every**
enum. So if two solvers each define their own phase enum — say
`FluidPhase::Force` (index 0) and `SolidPhase::Force` (index 0) — both land at
`(namespace 0, index 0)` and their systems **interleave** instead of one solver
running fully before the other. There is no error; the ordering is just
silently wrong.

Three ways to separate them (pick one):

- **Per-enum:** `Scheduler::set_schedule_namespace::<E>(n)` assigns namespace
  `n` to every system registered under enum `E`.
- **Bulk, ordered:** the `chain_namespaces!` macro assigns `0, 1, 2, …` to the
  enums you list, left to right.
- **Explicit tree:** build a `Schedule` and install it with
  `Scheduler::set_schedule` — the tree's walk order *is* the namespace
  assignment, and it additionally supports loops and branches.

```rust
// FluidPhase systems run entirely before SolidPhase systems:
chain_namespaces!(scheduler, FluidPhase, SolidPhase);
```

## Choosing a scheduling primitive

| Primitive | Use when |
|-----------|----------|
| Plain phases (`add_update_system(sys, Phase::X)`) | One linear pass per step; ordering is just `(namespace, index)` + before/after. |
| `SystemGroup` (`.loop_while(cond, max)`) | A *block of systems inside one phase* must iterate (e.g. a fixed-point coupling sub-loop). |
| `Schedule` tree (`set_schedule`) | The whole step needs structure: ordered phases plus `loop_until` / `branch` over them. |

`SystemGroup::loop_while` repeats **while** its condition stays `true`;
`Schedule`'s `loop_until` repeats **until** its condition becomes `true` (the
inverse). In both cases the condition is evaluated **after** each iteration, so
the loop body always runs at least once.

## The schedule tree in depth

`chain_namespaces!` flattens — fine for linear schedules, lossy for schedules
with iteration. The `Schedule` / `ScheduleNode` tree is the richer primitive:
phases composed by `Sequence`, with `Loop` nodes that re-execute their body
until a condition flips. The tree lowers at `set_schedule` time to namespace
assignments plus a dispatch tree the run loop walks each iteration. Lowering is
additive: schedulers that never call `set_schedule` keep the flat
`(namespace, index)` ordering described above.

```rust,ignore
use grass_scheduler::{OnMax, Schedule};

let s = Schedule::builder()
    .then::<CouplingPre>()
    .loop_until(check_implicit_converged, 20, OnMax::Panic, |body| {
        body.then::<DemTick>()
            .then::<CfdTick>()
            .then::<ResidualUpdate>()
    })
    .then::<CouplingPost>()
    .build();

parent.set_schedule(s);
```

> **Warning: call `set_schedule` *after* all systems and resources are
> registered.** Lowering walks the tree to rewrite the namespace field on
> already-registered systems and to resolve resource indices for `Loop` / `Branch`
> conditions. Any `add_update_system` call made *after* `set_schedule` leaves that
> system at the default namespace 0 — outside the tree's ordering — and any
> resource added afterward is invisible to conditions that were already lowered.
> Relatedly, mixing whole-enum dispatch (`then::<P>()`) and per-variant dispatch
> (`then_variant(P::V)`) for the *same* enum `P` in one tree makes `set_schedule`
> panic at install time with an "ambiguous namespace assignment" message.

The four node kinds:

- **`Phase`** — run all systems registered under one `ScheduleSet` enum type.
  Namespace index is assigned during lowering by tree-walk position. Use
  `then_variant` to dispatch a single variant of the enum instead of the whole
  batch.
- **`Sequence`** — run children in order.
- **`Loop`** — re-execute the body **until** the `until` condition returns
  `true`, or `max_iters` is reached. The condition is checked *after* each
  iteration, so the body always runs at least once. On hitting max without
  convergence, the `OnMax` policy decides what happens:
  - `OnMax::AcceptUnconverged` — continue past the loop.
  - `OnMax::Panic` — abort with a diagnostic.
  - `OnMax::Rollback` — run a user-supplied rollback fragment once, then
    continue (build with `loop_with_rollback`).
- **`Branch`** — first-match-wins state-conditional dispatch (build with
  `branch`).

## Signalling the end of a run

The `start()` loop keeps calling `run()` until a system sets the scheduler's
state to `End`. The state lives on the `SchedulerManager` resource, so any system
can stop the run by taking it `ResMut` and flipping the state:

```rust
use grass_scheduler::prelude::*;

fn stop_when_landed(
    bodies: Res<Bodies>,
    mut manager: ResMut<SchedulerManager>,
) {
    if bodies.pos.iter().all(|p| p[2] <= 0.0) {
        manager.state = SchedulerState::End;
    }
}
```

`App::is_done()` reports the same thing (`SchedulerManager::state == End`); an
externally-driven loop polls it instead of relying on `start()`. With
[`grass_io`](./io.md), `RunPlugin`'s `update_cycle` system owns this signal: it
counts steps against the `[[run]]` budget and sets `End` when the last stage is
exhausted, so most solvers never write a stop system by hand.

## Reversibility with `Snapshot<T>`

`Snapshot<T>` is a resource that can hold a saved copy of another resource for
rollback inside an iterative phase (a Picard loop, an adaptive-dt retry).
`snapshot_resource::<T>()` clones the live `T` into the `Snapshot<T>` slot;
`restore_resource::<T>()` writes it back.

> **Note: restore is take-on-restore.** `restore_resource` *moves* the value out
> of the snapshot slot, leaving it `None`. A second `restore_resource` in the same
> step (with no intervening `snapshot_resource`) is a no-op, not a double-restore.
> Refill the slot with a fresh `snapshot_resource` before the next save/restore
> cycle.

## Diagnostics

- **`SIM_TRACE`** env var — when set, prints each system name to stderr as it
  executes.
- **`SIM_SUPPRESS_WARNINGS`** env var — when set, suppresses the schedule
  validation warnings normally printed at the end of `organize_systems`.
- **`Scheduler::enable_schedule_print`** — writes a Graphviz `schedule.dot`
  after organizing; render it with `dot -Tpng schedule.dot`.
- **`Scheduler::start`** prints a per-system timing table when the run loop
  ends.
