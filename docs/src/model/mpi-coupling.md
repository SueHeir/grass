# MPI and Coupling

Two crates handle running across processes and coupling several solvers
together: `grass_mpi` is the thin MPI abstraction, and `grass_multi` is the
multi-physics coupling layer built on top of it.

## grass_mpi: the communication backend

`grass_mpi` provides `CommBackend` as the communication interface, `CommResource`
as a resource wrapper, and two backends:

- `SingleProcessComm` — no-op backend for serial runs.
- `MpiCommBackend` — real MPI backend, behind the `mpi_backend` feature.

### How to wire a backend

Consumers don't construct a backend by hand and pass it around — they put a
`CommResource` into the scheduler so systems can take it as `Res<CommResource>`.
There is no `CommPlugin` in this crate; the wiring lives in the consumer (e.g. a
setup system in your `App`).

**Parallel path** (with the `mpi_backend` feature):

```rust,ignore
use grass_mpi::*;

// 1. (MPMD only) split MPI_COMM_WORLD once, before any get_mpi_world().
init_app_color(0);

// 2. Grab this app's communicator (the color-split intra-comm if step 1 ran).
let world = get_mpi_world();

// 3. Build the real backend and wrap it as a resource.
let comm = CommResource(Box::new(MpiCommBackend::new(world)));
scheduler.add_resource(comm); // now available as Res<CommResource>
```

**Serial path** (no feature, or a single-process run): use the no-op backend —
every collective is the identity and point-to-point is never reached. This is
the serial-fallback contract: code written against `Res<CommResource>` runs
unchanged on one process.

```rust,ignore
use grass_mpi::{CommResource, SingleProcessComm};
let comm = CommResource(Box::new(SingleProcessComm::new()));
scheduler.add_resource(comm);
```

### Lifecycle and ordering

1. **`init_app_color(color)` once, before the first `get_mpi_world()`.** It
   color-splits `MPI_COMM_WORLD` for MPMD launches
   (`mpirun -np N1 ./a : -np N2 ./b`); calling it after a backend already
   captured the world is too late. Skip it entirely for SPMD/single-binary runs.
2. **Two communicator views:**
   - `get_mpi_world` returns the **color-split intra-comm** (this binary's own
     ranks) when `init_app_color` ran, else raw WORLD. This is what a backend
     should normally capture.
   - `get_mpi_world_raw` / `world_rank` / `world_size` always go to the **raw
     `MPI_COMM_WORLD`**, so MPMD couplers can address peers in *other* binaries
     by absolute world rank (this is what `grass_multi`'s transport uses).
3. **`finalize_mpi()` after every `CommResource` has dropped** (i.e. after the
   last `App` is finished). It calls `MPI_Finalize`; using any comm afterward is
   undefined.

> **Warning: ordering around `finalize_mpi` and `init_app_color` is unforgiving.**
> `finalize_mpi()` drops the `Universe` and calls `MPI_Finalize`; it must run
> *after every `CommResource` has dropped*, because any MPI call after finalize is
> undefined behaviour. Nothing in the type system enforces this — it falls out of
> Rust's drop order, so register `finalize_mpi` as the *last* resource-free
> cleanup (it runs after the resource-aware cleanups that still touch live
> resources). And `init_app_color(color)` is **not** idempotent despite its doc
> comment: a second call unconditionally overwrites the stored intra-comm, so a
> second call with a *different* color silently replaces the first split. Call it
> exactly once, before the first `get_mpi_world()`.

### The `unsafe impl Send/Sync` promise

`MpiCommBackend` (and the internal intra-comm storage) carry hand-written
`unsafe impl Send`/`Sync` so they can live in the scheduler's resource table and
a `static`. The soundness rests on a usage promise, not on MPI's own
thread-safety: **all MPI calls happen on a single thread** (the simulation is
single-threaded per rank). Do not share a `CommResource` across OS threads.

### What `CommBackend` offers

A system reaches the backend as `Res<CommResource>` (which `Deref`s to
`dyn CommBackend`). The interface covers process topology, collectives, and
point-to-point communication:

| Method | Purpose |
|---|---|
| `rank()` / `size()` | this rank's id and the total rank count |
| `processor_decomposition()` / `processor_position()` | the Cartesian process grid `[nx, ny, nz]` and this rank's position in it |
| `set_processor_grid(decomp, pos)` | the solver installs the grid; `grass_mpi` stores it verbatim |
| `all_reduce_sum_f64` | global sum — aggregate diagnostics |
| `all_reduce_min_f64` | global min — primary use is a global adaptive `dt` |
| `barrier()` | blocking global barrier (e.g. sync before I/O) |
| `send_f64` / `recv_f64` / `recv_f64_any` | blocking point-to-point |
| `sendrecv_f64` | deadlock-free sendrecv; probes the recv length and allocates |
| `sendrecv_f64_into` | probe-free sendrecv into a caller-sized buffer |
| `sendrecv_batch_f64_into` | many sendrecv pairs posted up front (Irecv/Isend) |

> **Note: the serial backend's point-to-point methods are not stubs.** On
> `SingleProcessComm`, `send_f64` / `recv_f64` / `recv_f64_any` / `sendrecv_f64`
> / `sendrecv_f64_into` all `unreachable!`. This is deliberate: code written
> against `Res<CommResource>` must take the local-copy path whenever
> `to_proc == rank`, and on one process *every* neighbour is local, so those
> methods are never reached. The one method the serial backend does service is
> `sendrecv_batch_f64_into`, which copies each op's send buffer into its receive
> buffer (periodic self-exchange).

For the ghost-exchange hot path, prefer `sendrecv_f64_into` (probe-free, caller
knows the recv length) over `sendrecv_f64` (convenient but allocates each call).
A batched exchange uses `SendRecvOp` — one struct per neighbour pair, with
`dest`/`source` set to `-1` to disable that half:

```rust,ignore
let ops = vec![
    SendRecvOp { dest: left,  send_buf: &out_l, source: left,  recv_buf: &mut in_l },
    SendRecvOp { dest: right, send_buf: &out_r, source: right, recv_buf: &mut in_r },
];
comm.sendrecv_batch_f64_into(&mut ops);
```

`sendrecv_batch_f64_into` posts all receives before all sends, then waits on all
— so there is no unexpected-message buffering, but every op in a batch must be
independent (disjoint `recv_buf`s, no send depending on another op's receive).

## grass_multi: multi-physics coupling

`grass_multi` provides a small set of primitives for running several independent
`grass_app::App` subsystems together inside a single parent `App`. Each
subsystem (a "sub-App") has its own scheduler and resource store; the parent
`App`'s schedule decides when each sub-App ticks and when cross-namespace
couplers run.

There is no orchestrator type, no Strategy enum, no Coupler trait — just:

- `SubApps` resource + `add_subapp` / `add_remote_subapp` for registration.
- `Multi` / `MultiRes` / `MultiResMut` SystemParams for cross-namespace reads
  and writes from ordinary parent-App systems.
- `tick_subapp` / `tick_n_times` system constructors that drive a sub-App's step
  loop from the parent's schedule.
- `Physics` trait + `AppPhysics` adapter (local sub-App) + `RemoteMirrorPhysics`
  (cross-process mirror), so MPI mirrors slot into the same `SubApps` machinery
  as local sub-Apps.
- `Wire` / `Transport` / `MpiInterCommTransport` (behind the `mpi` feature) for
  cross-process coupling.
- `OuterIterStopPlugin` for fixed-iter termination.
- `snapshot_subapp_resource` / `restore_subapp_resource` for opt-in
  reversibility (Picard / adaptive retries).

### The coupling loop: the parent schedule *is* the orchestrator

There is no hidden driver loop. The parent `App`'s own schedule decides
everything; one outer iteration (one `parent.run()`) is just the parent's
systems firing in `(namespace, index)` order. The convention maps that onto
three logical bands, expressed as parent `ScheduleSet` phases:

```text
one outer iter = parent.run() =
    Tick   →  tick_subapp(..) / tick_n_times(..) systems advance each sub-App
    Couple →  Multi / MultiRes / MultiResMut systems move data across namespaces
    Check  →  a stop system (e.g. OuterIterStopPlugin) decides whether to end
```

You wire those phases yourself with `add_update_system(sys, Phase::Tick)` etc.;
nothing forces this exact shape, but couplers must run *after* the ticks that
produce the data they read, so phase ordering is the contract.

### Borrow rules (read before writing a coupler)

Per-resource isolation comes from a `RefCell` on **each** sub-App resource,
keyed by `(type T, namespace NS)` — not from `Multi` itself. Consequences:

- **Allowed:** one system holding several cross-namespace handles at once —
  e.g. read `"cfd"`'s `T` and write `"dem"`'s `U` in the same statement
  (different cells, different borrows).
- **Panics:** taking two `&mut` handles to the *same* `(T, NS)` cell, or a `&`
  and a `&mut` to it, live at the same time (`RefCell` double-borrow).
  `expect_read` / `expect_write` also panic if the namespace or resource type
  isn't registered.
> **Warning: never mix a `Multi*` param and `ResMut<SubApps>` in one system.**
> `Multi`, `MultiRes`, and `MultiResMut` all borrow `Res<SubApps>` (shared). The
> `tick_subapp` / `tick_n_times` closures take `ResMut<SubApps>` (exclusive). A
> single system that takes both double-borrows the `SubApps` cell and panics at
> run time. Ticking (which mutates `SubApps`) and coupling (which reads `SubApps`
> to reach *into* a sub-App) must live in **separate systems and separate phases**
> — which is exactly why the convention is Tick → Couple → Check.

### Driving it: `start()` vs a manual loop

- **Self-driving:** `App::start` on the parent runs the whole thing and calls
  the parent's `run_cleanup` at the end.
- **Externally driven:** `parent.prepare()`, then `parent.run()` in a loop you
  own until a stop condition, then call `SubApps::cleanup_all` yourself before
  dropping the parent.

> **Warning: sub-App cleanups are not automatic.** The parent's `run_cleanup`
> does **not** propagate into sub-Apps — `SubApps::cleanup_all` must be called
> explicitly. On the self-driving path, register it as a cleanup-with-app from a
> plugin so `start()` fires it:
>
> ```rust,ignore
> use std::any::TypeId;
> app.add_cleanup_with_app(|parent: &mut App| {
>     if let Some(cell) = parent.get_mut_resource(TypeId::of::<SubApps>()) {
>         cell.borrow_mut().downcast_mut::<SubApps>().unwrap().cleanup_all();
>     }
> });
> ```
>
> On the externally-driven path, call `cleanup_all()` yourself after the loop and
> before dropping the parent. Forgetting it skips every sub-App's own cleanup —
> final dumps, `finalize_mpi`, and so on.

```rust,ignore
use grass_app::prelude::*;
use grass_multi::{tick_subapp, MultiAppExt, MultiRes, MultiResMut};
use grass_scheduler::prelude::*;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Phase { Tick, Couple, Check }

fn exchange(a: MultiRes<MyState, A>, mut b_other: MultiResMut<MyOther, B>) {
    b_other.x = a.x;
}

let mut parent = App::new();
parent.add_subapp("a", app_a);
parent.add_subapp("b", app_b);
parent.add_update_system(tick_subapp("a", 1), Phase::Tick);
parent.add_update_system(tick_subapp("b", 1), Phase::Tick);
parent.add_update_system(exchange, Phase::Couple);
parent.start();
```

The `Namespace` tag on each sub-App comes from `#[derive(Namespace)]` — see
[Derive Macros](../reference/derives.md).

## Tutorial: coupling two solvers in-process

This walks through coupling two independent oscillator solvers under one parent.
App **Spring** holds a spring whose extension drives a force; App **Mass** holds
a mass that the force accelerates. Each is an ordinary `grass_app::App` and
neither knows about the other — the parent schedule is the only thing that
couples them.

**1. Namespace markers.** Each sub-App is tagged by a zero-sized `Namespace`
type. `#[derive(Namespace)]` uses the struct identifier as the name string.

```rust,ignore
use grass_multi::namespace;

namespace!(pub Spring = "spring");   // == struct Spring; impl Namespace for Spring
namespace!(pub Mass   = "mass");
```

**2. Build the two sub-Apps.** Each registers its own state resource and step
systems; build them exactly as you would a standalone solver.

```rust,ignore
struct SpringState { extension: f64, force: f64 }
struct MassState   { force: f64, vel: f64, pos: f64 }

let mut spring_app = App::new();
spring_app.add_resource(SpringState { extension: 0.1, force: 0.0 });
// ... spring's own phases / systems ...

let mut mass_app = App::new();
mass_app.add_resource(MassState { force: 0.0, vel: 0.0, pos: 0.0 });
// ... mass's own phases / systems ...
```

**3. Register them on the parent.** `add_subapp_typed::<NS>` wraps the App in the
`SubApps` resource under that namespace.

```rust,ignore
let mut parent = App::new();
parent.add_subapp_typed::<Spring>(spring_app);
parent.add_subapp_typed::<Mass>(mass_app);
```

**4. Declare the parent's phases.** Declaration order *is* the ordering contract;
couplers must run after the ticks whose output they read.

```rust,ignore
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Phase { Tick, Couple, Check }
```

**5. Register tick systems.** `tick_n_times::<NS>(n)` advances a sub-App `n`
steps per outer iteration — use `n > 1` for sub-stepping (e.g. a stiff spring
taking 3 steps for every mass step).

```rust,ignore
parent.add_update_system(tick_n_times::<Spring>(1), Phase::Tick);
parent.add_update_system(tick_n_times::<Mass>(1),   Phase::Tick);
```

**6. Write the coupler.** A `Couple`-phase system reads across one namespace and
writes another. Because each `(T, NS)` cell has its own `RefCell`, one system can
hold both handles at once — as long as they are different cells.

```rust,ignore
const K: f64 = 50.0;

fn exchange(sp: MultiRes<SpringState, Spring>, mut m: MultiResMut<MassState, Mass>) {
    m.force = sp.extension * K;
}
parent.add_update_system(exchange, Phase::Couple);
```

This system cannot *also* tick a sub-App: `MultiRes` borrows `SubApps` shared and
`tick_*` borrows it exclusively (see the warning above). Keep them in separate
phases.

**7. Stop after a fixed number of outer iterations.** `OuterIterStopPlugin`
counts `parent.run()` calls and signals end at its target.

```rust,ignore
parent.add_plugins(OuterIterStopPlugin { n_iters: 1000, phase: Phase::Check });
```

**8. Run.** `parent.start()` drives the whole thing; remember
`SubApps::cleanup_all` (see the cleanup warning above) if your sub-Apps register
cleanups.

```rust,ignore
parent.start();
```

For rollback-style coupling (Picard iteration, adaptive retries),
`snapshot_subapp_resource::<T>(ns)` and `restore_subapp_resource::<T>(ns)` save
and restore a sub-App's resource around an inner loop. To test coupling wiring
without real MPI, `LocalTransport::pair()` gives a paired in-memory channel.

## Cross-process coupling: the remote variant

To put one solver in a *separate process*, replace its `add_subapp_typed` with a
remote mirror. A `RemoteMirrorPhysics` looks like a sub-App to the parent but
holds only a resource bag — it is never `.run()`d. Its `step` packs the
registered types, sends them over a `Transport`, and receives the peer's reply.

```rust,ignore
use grass_multi::{MpiInterCommTransport, MultiAppExt};

// peer_rank is the absolute MPI_COMM_WORLD rank of the other binary.
parent.add_remote_subapp("mass", MpiInterCommTransport::new(peer_rank))
    .send_each_iter::<SpringState>()   // export our state to the peer each iter
    .recv_each_iter::<MassState>()     // import the peer's state each iter
    .finish();
```

Two requirements make or break a remote coupling:

- **`Wire` for every exchanged type.** `Transport` moves raw bytes; the type is
  serialized by a hand-rolled `Wire` impl — `pack(&self) -> Vec<u8>` and
  `unpack(&[u8]) -> Self`. Built-in impls exist for the scalar primitives,
  `[f64; 3]`, `Vec<f64>`, and `String`; implement it yourself for a custom
  struct. There is no framing and no type tag on the wire, so **both peers must
  register the same types in the same order** — a mismatch silently unpacks
  garbage.

  ```rust,ignore
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

> **Warning: export before you tick the remote mirror.** `RemoteMirrorPhysics::step`
> sends whatever its resource cells hold *at tick time*. If the local side never
> copies its fresh state into the mirror before the mirror ticks, the mirror sends
> a stale value (its `T::default()` or a bounced-back value), introducing a
> one-outer-iteration feedback latency. The fix is a coupling system that writes
> the local resource into the mirror's slot, ordered *before* the
> `tick_subapp`/`tick_n_times` that drives the mirror. The canonical phase order
> is `TickLocal → Export → TickPeer → Import`. Note also that
> `RemoteMirrorPhysics::is_done` always returns `false` — a remote peer cannot
> signal completion through `Physics`; coordinate the end of the run with an
> explicit flag (e.g. `recv_each_iter::<bool>()`).

`MpiInterCommTransport::new(peer_rank)` always addresses **absolute
`MPI_COMM_WORLD` ranks** (it uses `get_mpi_world_raw()`), even after
`init_app_color` has split the world. So `peer_rank` is the WORLD rank of the
coupling counterpart, not a rank within either binary's intra-comm. Launch the
two binaries MPMD-style: `mpirun -np N1 ./spring : -np N2 ./mass`, behind the
`mpi` feature.
