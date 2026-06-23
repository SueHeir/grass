# Planning: `grass_multi` documentation

## Purpose

`grass_multi` is the cross-namespace coupling layer of the GRASS framework. It
lets multiple independent `grass_app::App` subsystems ("sub-Apps"), each with
its own scheduler and resource store, run under a single parent `App` whose
schedule is the orchestrator. The parent schedule decides when each sub-App
ticks and when coupling systems exchange data across namespaces — in-process
via typed SystemParams, or across MPI process boundaries via a pluggable
`Transport` and hand-rolled `Wire` serialization.

There is deliberately no orchestrator type, strategy enum, or coupler trait.
The parent schedule is the coupling contract.

---

## Public surface to document

### SystemParams (the primary user-facing API)

| Item | File:line | Notes |
|---|---|---|
| `Multi<'w>` | `multi.rs:154` | String-keyed; `.read::<T>(ns)` / `.write::<T>(ns)` return `Option<MultiRef>` / `Option<MultiMut>`; `.expect_read` / `.expect_write` panic with a message. Takes `Res<SubApps>` internally. |
| `MultiRef<'w, T>` | `multi.rs:235` | Read handle; `Deref → &T`. |
| `MultiMut<'w, T>` | `multi.rs:249` | Mut handle; `Deref/DerefMut → &mut T`. |
| `MultiRes<T, NS>` | `typed_multi.rs:61` | Compile-time-keyed read borrow; `SystemParam`; holds an `unsafe`-extended `Ref` pair (outer `SubApps` + inner resource cell). |
| `MultiResMut<T, NS>` | `typed_multi.rs:135` | Compile-time-keyed mut borrow; same two-ref structure as `MultiRes`. |

### Traits

| Item | File:line | Notes |
|---|---|---|
| `Namespace` | `multi.rs:292` | `const NAME: &'static str`; implement on a unit struct or use `#[derive(Namespace)]` from `grass_derive`. The derive uses the struct identifier as the name. |
| `Physics` | `physics.rs:56` | Lifecycle: `name / prepare / step / is_done / cleanup / resource_cell`. Optional reserved hooks: `time / max_stable_dt / set_dt` (none consumed yet). |
| `Wire` | `wire.rs:35` | `pack(&self) -> Vec<u8>` / `unpack(buf: &[u8]) -> Self`. Hand-rolled per type; no serde dep. Built-in impls: `f32/f64/i32/i64/u32/u64/bool/[f64;3]/Vec<f64>/String`. |
| `Transport` | `transport.rs:35` | `send(&self, &[u8])` / `recv(&self) -> Vec<u8>`. Blocking; errors via panic. |

### Registration functions (on `MultiAppExt`)

| Item | File:line | Notes |
|---|---|---|
| `add_subapp(name, app)` | `multi.rs:367` | Wraps `app` in `AppPhysics`; upserts `SubApps` resource on parent. |
| `add_subapp_typed::<NS>(app)` | `multi.rs:372` | Delegates to `add_subapp(NS::NAME, app)`. |
| `add_remote_subapp(name, transport)` | `multi.rs:408` | Returns `RemoteSubAppBuilder`; builder registers `RemoteMirrorPhysics` on drop. |

### Builder (fluent, registers on drop)

`RemoteSubAppBuilder` (`multi.rs:466`):
- `.send_at_setup::<T>()` — send once during `Physics::prepare`
- `.recv_at_setup::<T>()` — recv once during `Physics::prepare`
- `.send_each_iter::<T>()` — send every `tick_subapp` call
- `.recv_each_iter::<T>()` — recv every `tick_subapp` call
- `.with_resource::<T>()` — register a mirror-side resource without any pump
- `.finish()` — explicit registration (vs relying on `Drop`)

All pump methods require `T: Default + Wire + 'static`.

### System constructors

| Item | File:line | Notes |
|---|---|---|
| `tick_subapp(name, n)` | `multi.rs:337` | String-keyed; returns `impl FnMut(ResMut<SubApps>)`. |
| `tick_n_times::<NS>(n)` | `multi.rs:349` | Type-keyed equivalent. |
| `check_done_outer_iter` | `outer_iter.rs:28` | Increments `OuterIter`, signals `SchedulerState::End` at `NIters`. |
| `snapshot_subapp_resource::<T>(ns)` | `snapshot.rs:36` | Returns `impl FnMut(Multi)`; clones sub-App's `T` into `Snapshot<T>` on same sub-App. |
| `restore_subapp_resource::<T>(ns)` | `snapshot.rs:48` | Restores from `Snapshot<T>`, no-op if empty. |

### Resources

| Item | File:line | Notes |
|---|---|---|
| `SubApps` | `multi.rs:66` | Owning `Vec<Box<dyn Physics>>` + name→idx map. `.participants()` iterator; `.any_done()`; `.cleanup_all()`. |
| `OuterIter` | `outer_iter.rs:20` | `pub u32` counter; readable from user systems. |
| `NIters` | `outer_iter.rs:25` | `pub u32` target. |

### Concrete Physics impls

| Item | File:line | Notes |
|---|---|---|
| `AppPhysics` | `physics.rs:109` | Wraps a local `App`; `step` calls `app.run()`; `resource_cell` delegates to `app`. |
| `RemoteMirrorPhysics` | `remote.rs:69` | Inner `App` is a resource bag only (never `.run()`d); `step` sends all packed types then recvs all unpacked types. `is_done` always returns `false`. |

### Transports

| Item | File:line | Notes |
|---|---|---|
| `LocalTransport` | `transport.rs:49` | Paired in-memory `mpsc` channels; `pair()` → `(server, client)`. For tests. |
| `MpiInterCommTransport` (feature `mpi`) | `transport.rs:98` | MPMD over `MPI_COMM_WORLD`; `new(peer_rank: i32)`; uses `grass_mpi::get_mpi_world_raw()` so it always addresses absolute WORLD ranks even after `init_app_color`. Hand-written `unsafe Send/Sync`. |

### Prelude / re-exports

Everything is re-exported from `lib.rs`. The `namespace!` macro (`multi.rs:311`)
is also `#[macro_export]`-ed:
```rust
namespace!(pub DemNs = "dem");  // expands to struct + impl Namespace
```

### Derive macro

`#[derive(Namespace)]` is in `grass_derive`; it uses the struct identifier as
`NAME`. Documented at `reference/derives.md` but the behavioral note belongs in
the coupling chapter too.

---

## Config / TOML schema

None. `grass_multi` has no TOML schema. Namespace names are either compile-time
constants (`Namespace::NAME`) or string literals passed to `add_subapp` /
`tick_subapp`. Nothing reads from `grass_io::Config`. No TOML section to
document.

---

## Key behaviors, invariants, and gotchas

### 1. Borrow isolation is per `(T, NS)` RefCell, not per `Multi`

`Multi` holds `Res<SubApps>` (shared borrow). Per-resource isolation comes from
the `RefCell` on each sub-App's resource slot. Consequence: one system can hold
multiple cross-namespace handles simultaneously as long as no two are the same
`(T, NS)` pair (`multi.rs:154` comment block; also `lib.rs:47–63`).

Double-borrowing the same `(T, NS)` cell — e.g. two `MultiResMut<T, NS>` params
in one system, or any mix of `Multi::read` and `::write` on the same cell —
panics at runtime with a `RefCell` borrow error.

### 2. The big hazard: `Multi` + `ResMut<SubApps>` in the same system

`Multi` borrows `Res<SubApps>` (shared). `tick_subapp` returns a closure that
takes `ResMut<SubApps>` (exclusive). If one system takes both, it
double-borrows the `SubApps` cell and panics at runtime.

**Rule:** ticking and coupling must be in separate systems and separate phases.
`lib.rs:58–62`; also the doc comment on `add_remote_subapp` at `multi.rs:379`.

### 3. Phase ordering is the coupling contract

Couplers must run *after* the ticks that produce the data they read. The
three-band convention (Tick → Couple → Check) is not enforced by the framework;
it is enforced by declaration order in the user's `ScheduleSet`. Reordering
phases silently reorders execution. `lib.rs:32–42`.

### 4. `prepare` is lazy and idempotent

`SubApps::tick` calls `prepare()` on first tick, tracked by the `prepared` vec
(`multi.rs:119–128`). `AppPhysics::prepare` is also guarded (`physics.rs:145`).
However, the `prepared` flag lives on `SubApps`, not on `Physics`, so even a
custom `Physics` whose own `prepare` is not idempotent is only called once.

### 5. Sub-App cleanups are not automatic

The parent App's `run_cleanup` does **not** propagate to sub-Apps.
`SubApps::cleanup_all` must be called explicitly — either by registering it
as a cleanup-with-app system, or manually before dropping the parent in
externally-driven loops. `lib.rs:68–74`; `multi.rs:138`.

### 6. Remote mirror: export-before-tick ordering or stale latency

`RemoteMirrorPhysics::step` sends whatever the mirror's resource cells hold at
tick time, then recvs. If the local side never writes into the mirror before
ticking, the mirror sends its own `T::default()` (or a previously bounced-back
value) — a one-outer-iter feedback latency.

The fix: add a coupling system that copies the local App's resource into the
mirror's resource slot, ordered *before* the `tick_subapp` that drives the
remote mirror. Canonical phase order: `TickLocal → Export → TickPeer → Import`.
`multi.rs:397–406` (doc comment on `add_remote_subapp`).

### 7. Remote send/recv ordering: all sends before all recvs

Within a single `RemoteMirrorPhysics::prepare` or `::step`, all registered
senders fire first, then all receivers. Both peers must follow the same order.
This relies on the wire's send buffer being large enough to hold all payloads
while both sides are sending; very large payloads on tiny buffers can deadlock.
`remote.rs:31–37`.

### 8. `RemoteMirrorPhysics::is_done` always returns `false`

The mirror cannot detect peer completion through `Physics::is_done`. Signal
completion separately — e.g. a `recv_each_iter::<bool>()` flag, or a
coordinating transport message. `remote.rs:168–173`.

### 9. `StepResult::completed_full_step` is reserved / unused

The field exists on `Physics::step`'s return type but nothing in `grass_multi`
reads it. `physics.rs:26–31`.

### 10. `Physics::time / max_stable_dt / set_dt` are reserved / unused

The `Physics` trait has three optional adaptive-dt hooks; no code in
`grass_multi` calls them. Default impls return `None` / no-op. `physics.rs:87–104`.

### 11. `MpiInterCommTransport` uses absolute WORLD ranks

After `init_app_color` splits `MPI_COMM_WORLD`, each binary's `get_mpi_world()`
returns its color-split intra-comm. `MpiInterCommTransport::new(peer_rank)` uses
`grass_mpi::get_mpi_world_raw()` explicitly to always address absolute WORLD
ranks — so the peer rank passed must be the absolute WORLD rank of the coupling
counterpart, not a rank within either binary's intra-comm. `transport.rs:119`.

### 12. Wire format: no framing, no type tag

Each `Transport::send` carries exactly the bytes `Wire::pack` produced for one
type. No length prefix at the transport layer, no type tag. The peer must call
`unpack` on the same types in the same order. Mismatched registration order
silently unpacks garbage. `remote.rs:39–45`.

### 13. Duplicate namespace registration panics

`SubApps::register` panics on duplicate names (`multi.rs:92–96`). Both
`add_subapp` and `add_remote_subapp` go through `register_physics` → `register`.

---

## Tutorial outline: coupling two solvers

A coupling tutorial should live in the mdBook Tutorial chapter (parallel to
"Write Your Own Solver") or as a new page under the MPI and Coupling model
chapter.

**Scenario:** two independent oscillator solvers (App A = "spring", App B =
"mass") exchange state each outer iteration, running in-process.

**Steps:**

1. **Define namespace markers.** `#[derive(Namespace)] pub struct Spring;` and
   `pub struct Mass;` (or use `namespace!` macro). Explain `NAME` derivation.

2. **Build two sub-Apps** with their own plugins, resources, and schedules.
   Neither knows about the other.

3. **Create the parent App.** Call `parent.add_subapp_typed::<Spring>(spring_app)`
   and `parent.add_subapp_typed::<Mass>(mass_app)`. Explain `SubApps` resource.

4. **Declare parent phases.** `#[derive(Debug, Clone, Copy, ScheduleSet)] enum Phase { Tick, Couple, Check }`.
   Explain why declaration order is the ordering contract.

5. **Register tick systems.**
   ```rust
   parent.add_update_system(tick_n_times::<Spring>(1), Phase::Tick);
   parent.add_update_system(tick_n_times::<Mass>(1), Phase::Tick);
   ```
   Show substepping variant: `tick_n_times::<Spring>(3)` for 3:1 ratio.

6. **Write a coupling system** using `MultiRes` / `MultiResMut`.
   ```rust
   fn exchange(sp: MultiRes<SpringState, Spring>, mut m: MultiResMut<MassState, Mass>) {
       m.force = sp.extension * K;
   }
   parent.add_update_system(exchange, Phase::Couple);
   ```
   Note: why this system cannot also tick (SubApps borrow conflict).

7. **Add stop condition.** `parent.add_plugins(OuterIterStopPlugin { n_iters: 1000, phase: Phase::Check })`.

8. **Run.** `parent.start()` or manual `prepare() + run()×N + cleanup_all()`.

9. **Remote variant (cross-process).** Replace one `add_subapp_typed` with
   `add_remote_subapp("spring", MpiInterCommTransport::new(0))` + builder chain.
   Show export-before-tick system. Show `Wire` impl for `SpringState`. Note MPI
   feature flag and MPMD launch command.

---

## Doc gaps

1. **No coupling tutorial exists.** The existing "Write Your Own Solver" tutorial
   ends with "see `grass_multi`" but there is no tutorial page that actually does
   it. This is the largest gap. (`tutorial/write-your-own-solver.md:127`).

2. **`mpi-coupling.md` covers `grass_multi` well at the model level** but has no
   code example for the remote / cross-process path. The export-before-tick
   hazard is described in `multi.rs` doc comments but not in the mdBook.

3. **`Wire` trait and built-in impls are not documented in the mdBook at all.**
   The only mention is in the crate map one-liner (`reference/crates.md:8`).
   Users who need to implement `Wire` for a custom type have nowhere to look
   except source.

4. **`OuterIterStopPlugin` and snapshot helpers are not mentioned in the mdBook.**
   The model page ends after the basic example. Users who need fixed-iter
   termination or Picard rollback have no docs.

5. **`StepResult`, `Physics::time/max_stable_dt/set_dt` are reserved but
   undocumented as such.** A doc reader might think these are callable contracts.
   A brief "reserved, not yet consumed" note should appear wherever they're
   mentioned.

6. **The `namespace!` macro is undocumented in the mdBook.** `#[derive(Namespace)]`
   is covered in `reference/derives.md`; the macro equivalent is not.

7. **Cleanup lifecycle gap.** The fact that `SubApps::cleanup_all` is not called
   automatically by `App::start` is a foot-gun; `mpi-coupling.md:139` mentions
   it but doesn't explain *how* to register it as a cleanup-with-app system.

8. **`LocalTransport::pair()` is the only test harness.** Not mentioned in docs.
   Worth a callout in the tutorial so CI authors know they don't need real MPI to
   test coupling wiring.

---

## Suggested placement

The existing `docs/src/model/mpi-coupling.md` already contains the model-level
description of `grass_multi` (at `mpi-coupling.md:74–169`). Recommended
structure going forward:

- **Keep** `mpi-coupling.md` as the concept page: the coupling loop, the three
  phases, the borrow rules, the drive model. Expand the remote section with the
  export-before-tick hazard and `Wire` requirements.

- **Add** `docs/src/tutorial/coupling-two-solvers.md` for the step-by-step
  tutorial (in-process first, then remote/MPI variant).

- **Expand** `docs/src/reference/crates.md` from a stub table into per-crate
  pages; `grass_multi` should link to both the model page and the tutorial.

The `_planning/` file for this crate: `docs/_planning/grass_multi.md` (this file).
