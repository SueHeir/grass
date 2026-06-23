# Documentation Planning: `grass_scheduler`

> Status: planning only — READ-ONLY pass over source + existing docs.
> Generated: 2026-06-22

---

## Purpose

`grass_scheduler` is the dependency-injection scheduler that sits below `grass_app`
in the GRASS stack. It has no knowledge of particles, physics, or I/O. Its job is
to own typed resources, register systems (plain functions with `SystemParam`
parameters), sort and execute them in user-defined phase order each timestep, and
provide all of the control-flow primitives (loops, branches, states, stages,
snapshots) that more complex multi-physics schedules need.

Key file: `crates/grass_scheduler/src/lib.rs` (3 866 lines) plus
`src/schedule.rs` (512 lines) and `src/snapshot.rs` (86 lines).

---

## Public Surface to Document

### Structs

| Item | Defined at |
|---|---|
| `Scheduler` | `lib.rs:1700` |
| `SchedulerManager` | `lib.rs:3222` |
| `StoredPhase` | `lib.rs:1148` |
| `SystemDescriptor<S>` | `lib.rs:739` |
| `SystemGroupInfo` | `lib.rs:536` |
| `SystemGroup` | `lib.rs:877` |
| `Res<'a, T>` | `lib.rs:353` |
| `ResMut<'a, T>` | `lib.rs:374` |
| `Local<'a, T>` | `lib.rs:409` |
| `CurrentState<S>` | `lib.rs:1230` |
| `NextState<S>` | `lib.rs:1235` |
| `Schedule` | `schedule.rs:175` |
| `ScheduleBuilder` | `schedule.rs:213` |
| `BranchBuilder` | `schedule.rs:369` |
| `Snapshot<T>` | `snapshot.rs:36` |
| `FunctionSystem<Input, F>` | `lib.rs:552` (internal; still relevant for understanding) |
| `StoredSystemEntry` | `lib.rs:1107` (pub; used by plugins) |

### Enums

| Item | Defined at |
|---|---|
| `SchedulerState` | `lib.rs:3212` |
| `ScheduleNode` | `schedule.rs:99` |
| `OnMax` | `schedule.rs:65` |

### Traits

| Item | Defined at |
|---|---|
| `SystemParam` | `lib.rs:275` |
| `System` | `lib.rs:507` |
| `Condition` | `lib.rs:651` |
| `ScheduleSet` | `lib.rs:1135` |
| `StageName` | `lib.rs:1393` |
| `SystemExt` | `lib.rs:800` (fluent builder on `IntoSystem`) |
| `IntoSystem<Input>` | `lib.rs:562` |
| `IntoCondition<Input>` | `lib.rs:677` |
| `IntoScheduledSystem<M>` | `lib.rs:1041` |
| `IntoSystemLabel<M>` | `lib.rs:604` |

### Key free functions

| Item | Defined at |
|---|---|
| `in_state(target)` | `lib.rs:1294` |
| `on_enter_state(target)` | `lib.rs:1367` |
| `apply_state_transitions::<S>` | `lib.rs:1381` |
| `in_stage(name)` | `lib.rs:1450` |
| `on_enter_stage(name)` | `lib.rs:1516` |
| `first_stage_only()` | `lib.rs:1564` |
| `check_stage_advance::<S>` | `lib.rs:1579` |
| `snapshot_resource::<T>()` | `snapshot.rs:68` |
| `restore_resource::<T>()` | `snapshot.rs:79` |

### Macros

| Item | Defined at |
|---|---|
| `chain_namespaces!(scheduler, E1, E2, …)` | `lib.rs:1213` |

### `prelude` exports (`lib.rs:3250`)

`apply_state_transitions`, `check_stage_advance`, `first_stage_only`,
`in_stage`, `in_state`, `on_enter_stage`, `on_enter_state`,
`ConditionalSystem`, `CurrentState`, `IntoScheduledSystem`, `IntoSystemLabel`,
`Local`, `NextState`, `Res`, `ResMut`, `ScheduleSet`, `Scheduler`,
`SchedulerManager`, `SchedulerState`, `StageName`, `StoredPhase`,
`SystemDescriptor`, `SystemExt`, `SystemGroup`, `SystemGroupInfo`.
Also re-exports `grass_derive::ScheduleSet` derive macro.

---

## Config / TOML Schema

`grass_scheduler` itself has no TOML schema. The TOML-driven side of the stage
machinery (`[[run]]` sections, stage `name` strings) lives in `grass_io`
(`RunPlugin`, `InputPlugin`). The bridge into the scheduler is via
`Scheduler::set_stage_names` (`lib.rs:2337`) and the `SchedulerManager` resource
(`stage_name: Option<String>`, `advance_requested: bool`, `lib.rs:3222`).
Doc note: a cross-reference to `grass_io`'s `[[run]]` schema is needed wherever
`in_stage` / `on_enter_stage` / `check_stage_advance` are explained.

---

## Key Behaviors, Invariants, and Gotchas

### 1. Execution order is three-layer: (namespace, index) → topo-sort → registration order
`lib.rs:22-59`. The sort key is `(phase.namespace, phase.index)`; within a group
Kahn's algorithm (`topo_sort_group`, `lib.rs:1620`) sorts by `.before()` /
`.after()`; ties fall back to registration order. This is fully deterministic —
there is no parallelism (the README's "run independent ones concurrently" note in
`docs/src/model/scheduler.md:23` is **incorrect** — the implementation is single-
threaded).

### 2. Namespace-0 footgun
Every `ScheduleSet` enum defaults to namespace `0` (`StoredPhase::from_typed`,
`lib.rs:1171`). Two different enums with overlapping `to_index()` values silently
interleave. No error is raised. Fix: `set_schedule_namespace`, `chain_namespaces!`,
or `set_schedule`. Documented in source (`lib.rs:40-58`) and in
`docs/src/model/scheduler.md:94-117` but absent from the tutorial.

### 3. `organize_systems()` must be called before `run()` or `setup()`
Resource indices are resolved lazily in `prepare()` inside `organize_systems`
(`lib.rs:1771`). Calling `run()` without organizing will use stale or zero indices
(usize::MAX sentinel), silently skipping resource borrows. `start()` calls
`organize_systems` automatically, but code that drives `setup()` + `run()`
manually must call it first.

### 4. RefCell borrow rules are enforced at runtime, not compile time
Resources live in `Vec<RefCell<Box<dyn Any>>>` (`lib.rs:1712`). A system taking
`Res<T>` calls `borrow()` and one taking `ResMut<T>` calls `borrow_mut()`
(`lib.rs:311`, `lib.rs:333`). If two systems in the **same timestep** both try to
hold a `ResMut<T>` — or one holds `Res<T>` while another holds `ResMut<T>` — the
second borrow panics at runtime with a `BorrowMutError`. The scheduler has no
static conflict detection; the user must not place conflicting borrows in the same
phase group (or must order them with `.before()` / `.after()` so they never
overlap in time).

### 5. `set_schedule` must be called after systems and resources are registered
Lowering (`lib.rs:2045`) calls `collect_phase_assignments` to rewrite namespace
fields on already-registered systems, and calls `prepare_conditions` to resolve
resource indices for `Loop`/`Branch` conditions (`lib.rs:2104`). Registering
systems or resources *after* `set_schedule` means those systems keep namespace 0
and new-resource indices are not seen by existing `Loop` conditions.

### 6. Mixing whole-enum and per-variant dispatch for the same type panics
`ScheduleBuilder::then::<P>()` and `then_variant(P::V)` for the same `P` in one
tree causes `set_schedule` to panic with an "ambiguous namespace assignment"
message (`lib.rs:2065`). The error fires at install time, not at build time.
(`schedule.rs:231-268`).

### 7. `loop_while` vs `loop_until` are logical inverses
`SystemGroup::loop_while(cond, max)` repeats **while** `cond` is `true`
(`lib.rs:993-1015`). `Schedule::loop_until(cond, …)` repeats **until** `cond` is
`true` (`schedule.rs:277`). In both cases the condition is evaluated *after* the
first iteration, so the body always runs at least once. This is a common source of
off-by-one confusion when translating convergence checks.

### 8. `on_enter_state` uses local `was_active` state
`OnEnterStateCondition` (`lib.rs:1310`) holds `was_active: bool` that persists
across timesteps. It fires exactly on the first call after a transition — even if
the transition was triggered by stage-exhaustion rather than `next_state.set()`.
Because the flag is private to the condition instance, two `.run_if(on_enter_state(S::X))`
decorations on different systems are independent; neither can "steal" the
first-fire from the other.

### 9. `Snapshot<T>` is consume-on-restore (take semantics)
`restore_resource` calls `snap.saved.take()` (`snapshot.rs:81`), so after
restoration the slot is `None`. Calling `restore_resource` a second time in the
same timestep (without an intervening `snapshot_resource`) is a no-op, not a
double-restore. The slot must be refilled explicitly before the next save/restore
cycle.

### 10. `requires_label` is validation-only, not ordering
`.requires_label("x")` (`lib.rs:775`) panics at `organize_systems` time if `"x"`
is not present in the same `ScheduleSet` group, but it does **not** add a
before/after edge. Systems are still sorted independently of this constraint. Use
`.after("x")` if ordering is the goal.

### 11. `in_stage` / `on_enter_stage` are dead without `grass_app`'s stage driver
`in_stage` reads `SchedulerManager::stage_name` (`lib.rs:1413`). That field is
only populated by `update_cycle` in `grass_app` (`RunPlugin` / `StatesPlugin`). A
bare `Scheduler` always has `stage_name = None`, so `in_stage(..)` conditions
always return `false` (`lib.rs:1443-1456`). This is documented in the source but
not in the mdBook.

### 12. Conditions accept at most 5 SystemParam parameters (systems accept 10)
`impl_condition!` is generated for 0–5 parameters (`lib.rs:684-696`);
`impl_system!` for 0–10 (`lib.rs:569-580`). A condition closure with 6 or more
resource parameters silently fails to implement `IntoCondition` with a confusing
trait-bound error.

### 13. DOT output quality depends on stage names being set before `write_dot`
`write_dot` selects either flat or per-stage layout based on whether
`self.stage_names` is non-empty (`lib.rs:2562-2573`). Calling
`enable_schedule_print()` before `set_stage_names` always produces the flat
single-loop layout even in multi-stage runs.

---

## Tutorial Outline

A new tutorial page (or expanded section in the existing "Write Your Own Solver"
tutorial) should cover:

1. **Minimal hello-world** — one resource, one system, no phases.
2. **Defining phases with `ScheduleSet`** — `#[derive(ScheduleSet)]` on an enum;
   registration-order vs. index-order.
3. **`before` / `after` ordering** — string labels vs. function handles; when
   each is appropriate.
4. **`run_if` conditions** — simple closures; reading resources; the 5-param cap.
5. **Cross-solver ordering and the namespace footgun** — `chain_namespaces!` vs.
   `set_schedule_namespace` vs. the tree.
6. **`SystemGroup` for intra-phase loops** — `.loop_while(cond, max)`.
7. **`Schedule` tree for whole-step structure** — builder API; `loop_until`,
   `OnMax`; `branch`; `then_variant`.
8. **Reversibility with `Snapshot<T>`** — save/restore in a rollback loop.
9. **States** — `CurrentState` / `NextState`; `in_state` / `on_enter_state`;
   wiring `apply_state_transitions`.
10. **Stages** — how `grass_app`'s `RunPlugin` populates `SchedulerManager`;
    `in_stage`; `check_stage_advance`; `first_stage_only`.
11. **Diagnostics** — `SIM_TRACE`, `enable_schedule_print`, timing table.
12. **Implementing `SystemParam`** — custom DI parameters for plugin authors.

---

## Doc Gaps (what is missing or wrong today)

| Gap | Current state | Priority |
|---|---|---|
| Concurrency claim | `docs/src/model/scheduler.md:23` says the scheduler runs independent systems "concurrently." The implementation is strictly single-threaded (`run_flat` iterates sequentially, `lib.rs:1920`). The sentence should be removed or corrected. | HIGH — correctness |
| Runtime borrow panic | No mention anywhere in the book that two `ResMut<T>` for the same `T` in the same timestep panics at runtime. | HIGH — safety |
| 5-param cap on conditions | Not documented; appears only in the source macros. | HIGH — usability |
| `in_stage` dead without `grass_app` | Source has a note (`lib.rs:1443`); book has nothing. | MEDIUM |
| `set_schedule` ordering constraint (call after registration) | Not in the book. | MEDIUM |
| `requires_label` is validation-only (not ordering) | Missing from both book and README. | MEDIUM |
| `on_enter_state` edge-trigger semantics | Missing; important for multi-stage simulations. | MEDIUM |
| `Snapshot<T>` take semantics | Not documented beyond the rustdoc. | MEDIUM |
| `then_variant` panic on mixed dispatch | Not in the book. | MEDIUM |
| `Local<T>` isolation guarantees | Only in rustdoc. No book mention. | LOW |
| `has_update_system` / `remove_update_system` | Completely undocumented in the book. | LOW |
| `get_resource_ref` / `resource_cell` | Public, used by `grass_multi`; no book mention. | LOW |
| DOT output and stage-name ordering requirement | Not in the book. | LOW |

---

## Suggested Placement in the mdBook

```
# The Model
- [App, Plugin, PluginGroup](model/app-plugin.md)
- [The Scheduler](model/scheduler.md)           ← expand in-place (existing page)
  - Resources and SystemParams                  ← new subsection or child page
  - Execution Order (3 layers + namespace)       ← already partly here; correct concurrency claim
  - Run Conditions                               ← new subsection
  - SystemGroup and intra-phase loops            ← new subsection
  - Schedule trees (Loop, Branch, OnMax)         ← already partly here; add gotchas
  - States and Stages                            ← already partly here; add in_stage caveat
  - Snapshot and reversibility                   ← new subsection
  - Diagnostics                                  ← already here; complete
- [I/O and Configuration](model/io.md)
- [MPI and Coupling](model/mpi-coupling.md)

# Reference
- [Crate Map](reference/crates.md)
- [Derive Macros](reference/derives.md)         ← add ScheduleSet declaration-order invariant (already done)
```

The most efficient approach is to expand `model/scheduler.md` with the missing
subsections rather than splitting into child pages, because all concepts are
tightly coupled. If page length becomes unwieldy, split off
`model/scheduler-advanced.md` for the Schedule tree + Snapshot + States material.
