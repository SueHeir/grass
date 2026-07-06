# Coupling Two Solvers

This tutorial couples two independent solvers under one parent App — first
in-process, then across MPI processes. It uses only `grass_multi`: the
simulation-agnostic coupling layer. The two "solvers" here are deliberately
trivial toys (a spring and a mass) so the coupling machinery stays in the
foreground; nothing about them is a framework concept.

By the end you will have wired:

- two `grass_app::App` sub-Apps, each unaware of the other,
- a parent schedule that ticks both and moves data between them,
- and the same coupling running across a process boundary over a `Transport`.

If you have not written a standalone solver yet, read
[Write Your Own Solver](./write-your-own-solver.md) first — each sub-App below
is just such a solver.

If you only want the rules a solver must satisfy to be couplable — with nothing
about this particular spring/mass toy — skip to the
[Couplability contract: the checklist](#couplability-contract-the-checklist) at
the end. Everything between here and there is the walkthrough that earns each
line of it.

## The scenario

App **Spring** holds a spring whose extension produces a force. App **Mass**
holds a mass that the force accelerates. Neither App references the other; the
parent's schedule is the only thing that couples them:

```text
spring.extension ──(coupler)──▶ mass.force ──▶ mass integrates ──▶ mass.pos
```

Each is an ordinary `App` with its own resources and systems. The coupling is
"one-way" here (spring drives mass) to keep the example short; a two-way
coupling is just a second coupler in the other direction.

## 1. Namespace markers

Every sub-App is tagged by a zero-sized `Namespace` type whose `NAME` string
keys it inside the parent. Use the `namespace!` macro (which expands to a unit
struct plus the `Namespace` impl), or `#[derive(Namespace)]`, which takes the
struct identifier as the name.

```rust,ignore
use grass_multi::namespace;

namespace!(pub Spring = "spring");
namespace!(pub Mass   = "mass");
```

Equivalent derive form (see [Derive Macros](../reference/derives.md)):

```rust,ignore
use grass_multi::Namespace;

#[derive(Namespace)] pub struct Spring;   // NAME == "Spring"
#[derive(Namespace)] pub struct Mass;
```

## 2. Build the two sub-Apps

Each sub-App is built exactly like a standalone solver: register its state
resource and its step systems. Below, each holds one state resource; a real
solver would add its own phases and systems (omitted here for brevity).

```rust,ignore
use grass_app::prelude::*;

struct SpringState { extension: f64, force: f64 }
struct MassState   { force: f64, vel: f64, pos: f64 }

let mut spring_app = App::new();
spring_app.add_resource(SpringState { extension: 0.1, force: 0.0 });
// ... spring's own phases / systems: recompute `force` from `extension` ...

let mut mass_app = App::new();
mass_app.add_resource(MassState { force: 0.0, vel: 0.0, pos: 0.0 });
// ... mass's own phases / systems: integrate `pos`/`vel` from `force` ...
```

## 3. Register them on the parent

`add_subapp_typed::<NS>` wraps an App in the parent's `SubApps` resource under
that namespace. (`add_subapp("spring", app)` is the string-keyed equivalent;
the typed form catches namespace typos at compile time.)

```rust,ignore
use grass_multi::MultiAppExt;

let mut parent = App::new();
parent.add_subapp_typed::<Spring>(spring_app);
parent.add_subapp_typed::<Mass>(mass_app);
```

The first `add_subapp*` call creates the `SubApps` resource; later calls
register into it. Registering the same name twice panics.

## 4. Declare the parent's phases

The parent's schedule *is* the orchestrator: there is no hidden driver loop.
One outer iteration is one `parent.run()`, and its systems fire in phase order.
The convention is three bands — **Tick → Couple → Check** — and declaration
order is the ordering contract:

```rust,ignore
use grass_scheduler::prelude::*;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Phase { Tick, Couple, Check }
```

Couplers must run *after* the ticks that produce the data they read; that is
the whole reason `Couple` follows `Tick`.

## 5. Register tick systems

`tick_n_times::<NS>(n)` advances a sub-App `n` steps per outer iteration.
Use `n > 1` for sub-stepping — e.g. a stiff spring taking 3 inner steps for
every mass step:

```rust,ignore
use grass_multi::tick_n_times;

parent.add_update_system(tick_n_times::<Spring>(1), Phase::Tick);
parent.add_update_system(tick_n_times::<Mass>(1),   Phase::Tick);
// Sub-stepping example: tick_n_times::<Spring>(3) for a 3:1 spring:mass ratio.
```

`tick_subapp("spring", 1)` is the string-keyed equivalent of
`tick_n_times::<Spring>(1)`.

## 6. Write the coupler

A `Couple`-phase system reads one namespace and writes another with
`MultiRes<T, NS>` (shared) and `MultiResMut<U, NS>` (exclusive). Both deref to
the underlying resource, so you use them like plain references:

```rust,ignore
use grass_multi::{MultiRes, MultiResMut};

const K: f64 = 50.0;

fn exchange(sp: MultiRes<SpringState, Spring>, mut m: MultiResMut<MassState, Mass>) {
    m.force = sp.extension * K;
}
parent.add_update_system(exchange, Phase::Couple);
```

Isolation is per `(type, namespace)` cell — each sub-App resource has its own
`RefCell` — so one system may hold several cross-namespace handles at once, as
long as no two touch the *same* `(T, NS)` cell.

> **Never mix a `Multi*` param and ticking in one system.** `MultiRes` /
> `MultiResMut` borrow `SubApps` **shared**; the `tick_*` closures borrow it
> **exclusively**. A single system that both couples and ticks double-borrows
> the `SubApps` cell and panics at run time. That is exactly why ticking and
> coupling live in separate phases (Tick vs Couple). Keep them apart.

## 6b. The coupling contract: exchange ports

The coupler above works, but notice what it costs: the `Mass` side of the
system signature names `SpringState` — the *producer's own* resource type. The
two solvers are now coupled at the source level. Swap the spring for a
different driver and every consumer that read `SpringState` must change.

The **exchange port** removes that. A `Port<T>` is a typed slot on the parent
that a producer *exposes* a value into and a consumer *consumes*. The only
thing the two solvers share is the contract type `T` — never each other's
internal structs. `T` is whatever the physics needs: a scalar source term, a
boundary value, a sampled field `Vec<f64>`, particle data — anything. That is
what makes coupling *paradigm-agnostic*: it works whether the two solvers are
particle, mesh, or neither.

Define the contract type, register a port, and wire producer → port → consumer:

```rust,ignore
use grass_multi::{consume_field, expose_field, MultiAppExt};

// The whole contract: a scalar force. Only this type is shared.
struct Force(f64);

parent.add_port::<Force>();

// Producer exposes a value it derives from its OWN state (`SpringState`):
parent.add_update_system(
    expose_field::<SpringState, Force>("spring", |s| Force(s.extension * K)),
    Phase::Couple,
);
// Consumer applies the value into its OWN state (`MassState`):
parent.add_update_system(
    consume_field::<MassState, Force>("mass", |m, f| m.force = f.0),
    Phase::Couple,
);
```

`expose_field` reads the producer's resource and publishes `T`;
`consume_field` reads `T` and writes the consumer's resource, and is a
**no-op until the port has been published**, so it is safe to schedule
unconditionally. Order them the usual way — expose after the producer's tick,
consume before the consumer's tick:

```text
TickProducer → expose_field (→ Port) → consume_field (→ consumer) → TickConsumer → Check
```

Because the consumer now depends only on `Port<Force>`, any producer that
publishes a `Force` can drive it, and any consumer that reads `Force` can be
added — ports compose, and each solver compiles without the others. Prefer a
port when the exchange is a stable interface; a bare `Multi` coupler is fine
for a one-off, tightly-bound pair.

A full runnable example that couples a mesh-style field solver to a
point-particle solver through a port — and checks the coupled physics against a
closed form — lives in `grass_multi/tests/coupling_port.rs`.

## 7. Stop after a fixed number of iterations

`OuterIterStopPlugin` counts outer iterations and signals the scheduler to end
at its target, in whichever phase you give it:

```rust,ignore
use grass_multi::OuterIterStopPlugin;

parent.add_plugins(OuterIterStopPlugin { n_iters: 1000, phase: Phase::Check });
```

The load-bearing termination signal for a local sub-App is
`Physics::is_done()`, which `AppPhysics` reads from the wrapped `App`'s
scheduler state. In practice, a local solver ends by running a check system that
sets `SchedulerManager::state = End`, or by using `OuterIterStopPlugin` on the
parent loop. `StepResult::completed_full_step` is reserved for a future
full-step/substep distinction; `grass_multi` does not currently read it, and the
built-in local and remote physics adapters return `true` for every step.

## 8. Run it

`parent.start()` drives the whole thing:

```rust,ignore
parent.start();
```

> **Sub-App cleanups are not automatic.** The parent's `run_cleanup` does not
> propagate into sub-Apps — you must call `SubApps::cleanup_all` yourself. On
> the self-driving path, register it as a cleanup-with-app so `start()` fires
> it; on an externally-driven loop (`parent.prepare()`, then `parent.run()` in
> your own loop), call `cleanup_all()` after the loop and before dropping the
> parent. Forgetting it skips every sub-App's own cleanup (final dumps, MPI
> finalize, and so on).

For rollback-style coupling (Picard iteration, adaptive retries),
`snapshot_subapp_resource::<T>(ns)` and `restore_subapp_resource::<T>(ns)` save
and restore a sub-App's resource around an inner loop.

## 9. Cross-process: the remote variant

To put one solver in a *separate process*, replace its `add_subapp_typed` with
a **remote mirror**. A remote sub-App looks like a sub-App to the parent but
holds only a resource bag — it is never `.run()`d. Ticking it instead packs the
registered types, sends them over a `Transport`, and receives the peer's reply.

```rust,ignore
use grass_multi::{MpiInterCommTransport, MultiAppExt};

// peer_rank is the absolute MPI_COMM_WORLD rank of the other binary.
parent.add_remote_subapp("mass", MpiInterCommTransport::new(peer_rank))
    .send_each_iter::<SpringState>()   // export our state to the peer each iter
    .recv_each_iter::<MassState>()     // import the peer's state each iter
    .finish();
```

Two requirements make or break a remote coupling.

**A `Wire` impl for every exchanged type.** `Transport` moves raw bytes; each
type is serialized by a hand-rolled `Wire` — `pack(&self) -> Vec<u8>` and
`unpack(&[u8]) -> Self`. Built-in impls exist for the scalar primitives,
`bool`, `[f64; 3]`, `Vec<f64>`, and `String`; write your own for a custom
struct. There is no framing and no type tag on the wire, so **both peers must
register the same types in the same order** — a mismatch silently unpacks
garbage.

```rust,ignore
use grass_multi::Wire;

impl Wire for SpringState {
    fn pack(&self) -> Vec<u8> {
        let mut b = self.extension.pack();
        b.extend(self.force.pack());
        b
    }
    fn unpack(buf: &[u8]) -> Self {
        SpringState {
            extension: f64::unpack(&buf[0..8]),
            force:     f64::unpack(&buf[8..16]),
        }
    }
}
```

**Export before you tick the mirror.** `send_each_iter::<T>` ships whatever the
mirror's `T` cell holds *at tick time*. If the local side never copies its
fresh state into the mirror before the mirror ticks, the mirror sends a stale
value — a one-outer-iteration feedback latency. The fix is a coupling system
that writes the local resource into the mirror's slot, ordered *before* the
tick that drives the mirror. The canonical phase order becomes:

```text
TickLocal → Export(local → mirror) → TickPeer → Import
```

> **A remote mirror cannot signal completion.** Its `is_done` always returns
> `false` — a peer's end-of-run cannot propagate through the mirror. Coordinate
> termination with an explicit flag instead, e.g. `recv_each_iter::<bool>()`.

`MpiInterCommTransport::new(peer_rank)` always addresses **absolute
`MPI_COMM_WORLD` ranks** (it uses `get_mpi_world_raw()`), even after
`init_app_color` has split the world — so `peer_rank` is the WORLD rank of the
coupling counterpart, not a rank within either binary's intra-comm. Launch the
two binaries MPMD-style, behind the `mpi` feature:

```sh
mpirun -np 1 ./spring : -np 1 ./mass
```

## Testing coupling without MPI

You do not need a real MPI launch to test the wiring. `LocalTransport::pair()`
returns a paired in-memory channel `(server, client)` that satisfies the same
`Transport` interface, so a single-process test can drive both ends of a remote
coupling. Use it in CI to exercise the pack/unpack and export-before-tick logic
before running the real MPMD job.

## Couplability contract: the checklist

Everything above is one worked example. This section is the general contract it
demonstrates: **what any solver must expose, and how it must behave, to be
coupled to any other solver on grass** — independent of physics or paradigm. If
a solver satisfies these, `grass_multi` can couple it to any other solver that
also satisfies them, in-process or across MPI. Each item names the primitive
that enforces it and the failure you get if you skip it.

### A. Resources a solver must expose

A sub-App is coupled *through its resources*: a coupler reaches into a
producer's resource to read, and a consumer's resource to write. So the solver
must make those resources reachable and stable.

- [ ] **Register every exchanged resource with `add_resource::<T>`.** A coupler
  finds a resource by `TypeId` via `Physics::resource_cell`; an unregistered
  type resolves to `None`, so the `MultiRes` / `MultiResMut` SystemParam (and
  `expose_field` / `consume_field`, which go through `expect_read` /
  `expect_write`) panics at run time with `sub-App … has no resource of type …`.
  Everything a peer reads or writes must be a registered resource, not a local
  variable.
- [ ] **Keep the exchanged data in a resource you own, never in the peer's
  type.** The producer exposes a value *derived from its own state*; the
  consumer applies a value *into its own state*. Neither should hold or name the
  other's resource type. A bare `MultiRes<ProducerState, …>` in the consumer
  works but source-couples the two — see §6/§6b.
- [ ] **For a decoupled interface, share only a contract type `T` via a
  `Port<T>`.** `add_port::<T>()` registers the parent-side slot;
  `expose_field::<Own, T>` publishes, `consume_field::<Own, T>` reads. `T` is
  the *entire* shared surface — a scalar, a boundary value, `Vec<f64>`, particle
  data, anything `'static`. This is what makes the coupling paradigm-agnostic:
  the two solvers need not be the same kind of discretization, or any
  discretization at all.
- [ ] **`consume_field` is a no-op until its port has been published**, so a
  consumer is safe to schedule before the first expose (e.g. iteration 0). Don't
  add your own "is it ready yet" guard.
- [ ] **For cross-process coupling, implement `Wire` for every exchanged type**
  (`pack`/`unpack`), and register the *same types in the same order* on both
  peers via `send_each_iter::<T>()` / `recv_each_iter::<T>()`. The wire carries
  no type tag or framing; a mismatch silently unpacks garbage. Built-in `Wire`
  impls: the scalar primitives, `bool`, `[f64; 3]`, `Vec<f64>`, `String`.

### B. Tick semantics

The parent schedule is the only driver — there is no hidden loop inside
`grass_multi`. A couplable solver must be steppable on demand from the parent.

- [ ] **The solver is an ordinary `App` advanced by `tick_subapp(name, n)` /
  `tick_n_times::<NS>(n)` from the parent's `Tick` phase.** One
  `parent.run()` = one outer iteration; each `tick_*` registration calls the
  sub-App's `step` `n` times. There is no self-driving inner loop — if you never
  register a tick for a namespace, that solver never advances.
- [ ] **Sub-stepping is `n > 1`.** A stiff solver that needs 3 inner steps per
  outer step is `tick_n_times::<NS>(3)`; the rate ratio between two solvers is
  just their two `n`s. This is the only knob for multi-rate coupling today.
- [ ] **Couplers run *after* the ticks that produce their inputs.** The
  canonical band order is **Tick → Couple → Check** (or, with ports,
  `TickProducer → expose → consume → TickConsumer → Check`). Ordering *is* the
  contract: declare phases in that order and register each system into the right
  phase. Read-before-produce gives you a one-iteration staleness lag, not an
  error.
- [ ] **Signal completion through the scheduler so `is_done` becomes `true`**
  (e.g. a check system setting `SchedulerManager::state = End`, or
  `OuterIterStopPlugin`). A **remote mirror's `is_done` is always `false`** — a
  peer's end-of-run cannot propagate back — so terminate a cross-process
  coupling with an explicit flag (e.g. `recv_each_iter::<bool>()`), never by
  waiting on the mirror.
- [ ] **Treat `StepResult::completed_full_step` as reserved.** It is public on
  `Physics::step` for a future scheduler that distinguishes full steps from
  internal substeps, but `grass_multi` does not consume it today. Do not use it
  as a termination signal; for local sub-Apps use scheduler done-state, and for
  remote peers exchange an explicit done flag.
- [ ] **Cleanup is not automatic across the sub-App boundary.** The parent's
  `run_cleanup` does not descend into sub-Apps; call `SubApps::cleanup_all`
  yourself (register it as a cleanup-with-app on `start()`, or after your own
  driven loop). Skipping it drops every sub-App's final dumps / MPI finalize.
- [ ] **Remote only: export local state into the mirror *before* you tick it.**
  `send_each_iter::<T>` ships whatever the mirror's `T` cell holds at tick time;
  without a copy-in system ordered before the mirror tick you send a stale
  value — `TickLocal → Export(local→mirror) → TickPeer → Import`.
- [ ] **Treat `Physics::time`, `Physics::max_stable_dt`, and `Physics::set_dt`
  as reserved.** They are interface hooks for future adaptive-dt orchestration.
  The current `grass_multi` driver never calls them; multi-rate behavior is the
  explicit `n` passed to `tick_subapp` / `tick_n_times`.

### C. Borrow rules

Cross-namespace access goes through a `RefCell` on *each* resource, so the
borrow discipline is per-cell, and violating it panics at run time (not compile
time).

- [ ] **Isolation is per `(type, namespace)` cell.** One system may hold several
  cross-namespace handles at once — read `"cfd"` and write `"dem"` in one
  expression — as long as no two handles touch the *same* `(T, NS)` cell.
- [ ] **`MultiRes<T, NS>` borrows a cell shared; `MultiResMut<T, NS>` borrows it
  exclusively.** Two `MultiResMut` on the same cell, or a `MultiRes` and a
  `MultiResMut` on the same cell, in one system → `already borrowed` panic. Split
  them across systems or phases.
- [ ] **Never mix a `Multi*` param and a tick in the same system.** `MultiRes` /
  `MultiResMut` / `Multi` borrow the `SubApps` resource **shared**; the `tick_*`
  closures borrow it **exclusively**. A single system that both couples and
  ticks double-borrows the `SubApps` cell and panics. This is *why* Tick and
  Couple are separate phases — keep ticking and coupling in different systems.
- [ ] **A port hop sidesteps cross-solver borrows entirely.** `expose_field`
  touches only the producer cell + the port; `consume_field` touches only the
  port + the consumer cell. Neither ever borrows the other solver's cell, so
  producer and consumer can never contend for the same `(T, NS)`.

The runnable end-to-end example that exercises this whole contract — a
mesh-style field solver driving a point-particle solver through a port, checked
against a closed form — is `grass_multi/tests/coupling_port.rs` (`cargo test -p
grass_multi --test coupling_port`).

## Where to go from here

- The concept-level companion to this tutorial — the coupling loop, the borrow
  rules, and the drive model — is [MPI and Coupling](../model/mpi-coupling.md).
- For the primitives named above, see the `grass_multi` items in the
  [Crate Map](../reference/crates.md).
