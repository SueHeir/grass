# GRASS in 5 minutes

The whole framework in one sitting: register state, write a step, run it, then
see the three ideas that scale it up. No particles, no physics — just GRASS.

## The one idea

You never write a `main` loop. You hand the framework three things —
**resources** (your state), **systems** (functions that transform it), and the
**phases** they run in — and the **scheduler** injects the borrows and runs
everything in a deterministic order it computes from what each system touches.

## Minute 1 — a running app

```rust
use grass_app::prelude::*;
use grass_scheduler::prelude::*;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Step { Update }

struct Position(f64);

fn move_thing(mut pos: ResMut<Position>) {
    pos.0 += 1.0;
}

fn main() {
    let mut app = App::new();
    app.add_resource(Position(0.0));            // state, stored by type
    app.add_update_system(move_thing, Step::Update);
    app.start();                                // organize → setup → run → cleanup
}
```

`move_thing` takes `ResMut<Position>`; that argument type *is* the declaration
"I write `Position`." The scheduler injects the borrow and runs the system. No
dispatch code, no loop body you maintain by hand.

## Minute 2 — resources and systems

A **resource** is global state stored by type. A **system** is a plain function
whose parameters are `SystemParam`s — usually `Res<T>` (shared read) and
`ResMut<T>` (exclusive write). Add a clock and advance it:

```rust
struct Clock { t: f64, dt: f64 }

fn advance_clock(mut clock: ResMut<Clock>) {
    clock.t += clock.dt;
}

app.add_resource(Clock { t: 0.0, dt: 1e-3 });
app.add_update_system(advance_clock, Step::Update);
```

Resources live in `RefCell`s, so borrows are checked at **run time**, not
compile time. Because a run executes systems in sequence, two systems never race
— and two systems in the same phase that touch the same resource are not rejected
by `organize_systems()`. If their data order matters, make it explicit with
`.before()` / `.after()`. A *single* system that takes the same resource as both
`Res` and `ResMut` double-borrows the `RefCell` and panics with
`already mutably borrowed: BorrowError` or `already borrowed: BorrowMutError`.
Split that work into separate systems. That trade-off is the whole borrow model.

## Minute 3 — phases and order

A `ScheduleSet` enum names the phases of one timestep. **Variant order is the
execution order** — no graph wiring by hand:

```rust
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Step {
    Forces,     // update velocities
    Integrate,  // update positions, advance the clock
    Output,     // dump / print
}
```

Register each system into a phase; within a phase, data dependencies settle the
rest of the order. Under the hood these phases are nodes in a schedule tree —
`Phase` (a set of systems), `Sequence` (children in order), `Loop` (the per-step
repeat), `Branch` (conditional) — but you rarely build it by hand; the enum does.

## Minute 4 — stopping

`start()` calls `run()` until a system flips the scheduler's state to `End`. The
state lives on the `SchedulerManager` resource, so a stop condition is just a
system that takes it `ResMut`:

```rust
fn stop_after(t_end: f64) -> impl Fn(Res<Clock>, ResMut<SchedulerManager>) {
    move |clock, mut manager| {
        if clock.t >= t_end {
            manager.state = SchedulerState::End;
        }
    }
}

app.add_update_system(stop_after(1.0), Step::Output);
```

If you drive the loop yourself instead of `start()`, poll `app.is_done()` and
call `app.run_cleanup()` after the loop.

## Minute 5 — the three things that scale it up

Everything above is one solver. GRASS earns its keep when you grow it:

- **Plugins.** Bundle those resources and systems into a `Plugin`'s
  `build(&mut app)`. A `PluginGroup` bundles plugins, and a consumer can
  `disable::<YourOutput>()` and add their own — swappable implementations without
  editing the original. See
  [App, Plugin, PluginGroup](./model/app-plugin.md).
- **Coupling.** Run several `App`s as sub-Apps under one parent and exchange
  state from the parent each step with `MultiRes<T, NS>` / `MultiResMut<T, NS>` — in one process
  (`add_subapp`) or across separate MPI binaries (`add_remote_subapp` +
  `MpiInterCommTransport`). See [MPI and Coupling](./model/mpi-coupling.md).
- **I/O for free.** Add the `grass_io` plugins for a TOML-configured clock,
  terminal output, and file dumps instead of hand-rolling them. See
  [I/O and Configuration](./model/io.md).

## Where next

- [Write Your Own Solver](./tutorial/write-your-own-solver.md) — the same shape
  as this page, built out into a complete falling-bodies integrator with a plugin.
- [The Scheduler](./model/scheduler.md) — resources, systems, the schedule tree,
  run conditions, and the borrow rules in full.
- To make bodies **interact** and run **in parallel across MPI ranks**, that's
  the particle substrate — [SOIL](https://sueheir.github.io/soil) picks up where
  this leaves off.
