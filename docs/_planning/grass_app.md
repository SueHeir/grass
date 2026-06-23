# Documentation-needs report: `grass_app`

Generated from a read-only audit of
`crates/grass_app/src/` (all files), `crates/grass_app/README.md`, and
`crates/grass_app/Cargo.toml`, cross-referenced against `docs/src/`.

---

## 1. Purpose

`grass_app` is the top-level application and plugin framework for the GRASS
simulation stack. It provides `App` — the central resource and system container
— and the `Plugin` / `PluginGroup` registration model through which every
simulation feature (physics, I/O, analysis) wires itself in. On top of that
thin composition layer it adds: a two-mechanism validation model (eager TypeId
dependencies + lazy capability contracts), two distinct lifecycle paths
(self-driving via `start` vs. externally-driven via `prepare`/`run`/`run_cleanup`),
a generic three-phase setup ordering enum (`ScheduleSetupSet`), two ready-made
mini-plugins for runtime state machines (`StatesPlugin<S>`,
`StageAdvancePlugin<S>`), and a `--generate-config` recipe that assembles a
complete example TOML from all registered plugins. The crate is intentionally
physics-agnostic; it depends only on `grass_scheduler` and `downcast-rs`.

---

## 2. Public surface to document

### `app.rs`

| Symbol | Kind | File:line | Notes |
|--------|------|-----------|-------|
| `ConfigSnippets` | `pub struct` | `app.rs:31` | Resource; `pub snippets: Vec<String>` |
| `GenerateConfigFlag` | `pub struct` (marker) | `app.rs:41` | Resource; presence alone is the signal |
| `App` | `pub struct` | `app.rs:55` | Central container; fields are `pub(crate)` |
| `App::new` | `pub fn` | `app.rs:74` | Preferred constructor |
| `App::add_plugins` | `pub fn` | `app.rs:94` | Accepts `Plugin`, `PluginGroup`, or tuple |
| `App::main` | `pub fn` | `app.rs:222` | `&SubApp` accessor |
| `App::main_mut` | `pub fn` | `app.rs:227` | `&mut SubApp` accessor |
| `App::organize_systems` | `pub fn` | `app.rs:235` | Usually internal; exposed for external drivers |
| `App::setup` | `pub fn` | `app.rs:242` | Runs setup phase once |
| `App::run` | `pub fn` | `app.rs:250` | Runs one update tick |
| `App::add_setup_system` | `pub fn` | `app.rs:256` | Delegates to `SubApp` |
| `App::add_update_system` | `pub fn` | `app.rs:266` | Delegates to `SubApp` |
| `App::set_schedule_namespace` | `pub fn` | `app.rs:278` | Cross-solver ordering by namespace |
| `App::set_schedule` | `pub fn` | `app.rs:291` | Install hierarchical `Schedule` tree |
| `App::add_resource` | `pub fn` | `app.rs:299` | Upserts resource |
| `App::get_mut_resource` | `pub fn` | `app.rs:306` | Raw `TypeId`-keyed access |
| `App::get_resource_ref` | `pub fn` | `app.rs:312` | Typed shared borrow |
| `App::resource_cell` | `pub fn` | `app.rs:320` | Shared receiver; for `grass_multi` couplers |
| `App::prepare` | `pub fn` | `app.rs:334` | Externally-driven lifecycle entry point |
| `App::is_done` | `pub fn` | `app.rs:342` | Check `SchedulerManager` state == `End` |
| `App::run_cleanup` | `pub fn` | `app.rs:356` | Drain cleanup queues; MUST be called manually on externally-driven path |
| `App::add_cleanup` | `pub fn` | `app.rs:368` | Register `fn()` cleanup |
| `App::add_cleanup_with_app` | `pub fn` | `app.rs:378` | Register `FnOnce(&mut App)` cleanup |
| `App::start` | `pub fn` | `app.rs:391` | Self-driving lifecycle; generates config if flag present |
| `App::remove_update_system` | `pub fn` | `app.rs:416` | Remove by concrete type |
| `App::has_update_system` | `pub fn` | `app.rs:429` | Guard against double-registration |
| `App::remove_update_system_by_label` | `pub fn` | `app.rs:437` | Remove by string label |
| `App::enable_schedule_print` | `pub fn` | `app.rs:444` | Debug: print organized schedule |
| `App::set_stage_names` | `pub fn` | `app.rs:450` | Human-readable stage names |
| `App::set_warning_fn` | `pub fn` | `app.rs:456` | Domain-specific schedule warning hook |
| `AppError` | `pub(crate) enum` | `app.rs:464` | Internal; not in public API |

### `plugin.rs`

| Symbol | Kind | File:line | Notes |
|--------|------|-----------|-------|
| `type_ids!` | `#[macro_export]` macro | `plugin.rs:42` | `type_ids![A, B]` → `Vec<TypeId>` |
| `Plugin` | `pub trait` | `plugin.rs:74` | Core trait; `build` is required |
| `Plugin::build` | required method | `plugin.rs:79` | Called once during `add_plugins` |
| `Plugin::name` | optional method | `plugin.rs:84` | Defaults to `type_name::<Self>()` |
| `Plugin::is_unique` | optional method | `plugin.rs:92` | Default `true`; duplicates panic |
| `Plugin::default_config` | optional method | `plugin.rs:100` | TOML snippet or `None` |
| `Plugin::dependencies` | optional method | `plugin.rs:115` | Eager TypeId ordering |
| `Plugin::provides` | optional method | `plugin.rs:127` | Capability tag strings |
| `Plugin::requires` | optional method | `plugin.rs:140` | Capability tag strings |
| `PluginGroup` | `pub trait` | `plugin.rs:174` | `build(self) -> PluginGroupBuilder` |
| `PluginGroupBuilder` | `pub struct` | `plugin.rs:181` | Builder; `start`, `add`, `disable` |
| `PluginGroupBuilder::start` | `pub fn` | `plugin.rs:188` | Constructor, takes `G: PluginGroup` type param |
| `PluginGroupBuilder::add` | `pub fn` | `plugin.rs:198` | Add a plugin; silently skips disabled types |
| `PluginGroupBuilder::disable` | `pub fn` | `plugin.rs:206` | Mark a type to skip on future `add` calls |
| `StatesPlugin<S>` | `pub struct` + `Plugin` impl | `plugin.rs:239` | Registers `CurrentState<S>`, `NextState<S>`, transition system |
| `StatesPlugin::new` | `pub fn` | `plugin.rs:248` | `(initial: S, phase: impl ScheduleSet)` |
| `StatesPlugin::initial` | `pub field` | `plugin.rs:242` | Initial state value |
| `StageAdvancePlugin<S>` | `pub struct` + `Plugin` impl | `plugin.rs:274` | Watches `CurrentState<S>` → stage advance; adds `StageNames` resource |
| `StageAdvancePlugin::new` | `pub fn` | `plugin.rs:281` | `(phase: impl ScheduleSet)` |
| `StageNames` | `pub struct` | `plugin.rs:301` | Resource wrapping `&'static [&'static str]` |
| `Plugins<Marker>` | `pub trait` | `plugin.rs:313` | Sealed; implemented for `Plugin`, `PluginGroup`, tuples |

### `setup.rs`

| Symbol | Kind | File:line | Notes |
|--------|------|-----------|-------|
| `ScheduleSetupSet` | `pub enum` | `setup.rs:24` | `PreSetup` (0), `Setup` (1), `PostSetup` (2); implements `ScheduleSet` |

### `sub_app.rs`

| Symbol | Kind | File:line | Notes |
|--------|------|-----------|-------|
| `SubApp` | `pub struct` | `sub_app.rs:21` | Scheduler + resource store + plugin bookkeeping |
| `SubApp::new` | `pub fn` | `sub_app.rs:36` | Default constructor |
| `SubApp::start` | `pub fn` | `sub_app.rs:41` | Full lifecycle: organize → setup → run |
| `SubApp::organize_systems` | `pub fn` | `sub_app.rs:46` | Sort systems |
| `SubApp::setup` | `pub fn` | `sub_app.rs:51` | Run setup systems once |
| `SubApp::run` | `pub fn` | `sub_app.rs:58` | One update tick |
| `SubApp::prepare` | `pub fn` | `sub_app.rs:66` | External-driver entry: organize + setup + set state to `Run` |
| `SubApp::is_done` | `pub fn` | `sub_app.rs:86` | Check `SchedulerManager` |
| `SubApp::resource_cell` | `pub fn` | `sub_app.rs:96` | Shared receiver resource access |
| `SubApp::add_setup_system` | `pub fn` | `sub_app.rs:101` | — |
| `SubApp::add_update_system` | `pub fn` | `sub_app.rs:110` | — |
| `SubApp::set_schedule_namespace` | `pub fn` | `sub_app.rs:119` | — |
| `SubApp::set_schedule` | `pub fn` | `sub_app.rs:126` | Install `Schedule` tree |
| `SubApp::add_resource` | `pub fn` | `sub_app.rs:131` | — |
| `SubApp::get_mut_resource` | `pub fn` | `sub_app.rs:136` | `&mut self` raw cell |
| `SubApp::get_resource_ref` | `pub fn` | `sub_app.rs:141` | Typed shared borrow |
| `SubApp::remove_update_system` | `pub fn` | `sub_app.rs:146` | — |
| `SubApp::has_update_system` | `pub fn` | `sub_app.rs:156` | Guard against duplicates |
| `SubApp::remove_update_system_by_label` | `pub fn` | `sub_app.rs:164` | — |
| `SubApp::enable_schedule_print` | `pub fn` | `sub_app.rs:169` | — |
| `SubApp::set_stage_names` | `pub fn` | `sub_app.rs:174` | — |
| `SubApp::set_warning_fn` | `pub fn` | `sub_app.rs:179` | — |
| `SubApps` | `pub struct` | `sub_app.rs:186` | Wrapper with `pub main: SubApp` |

### Prelude exports (`lib.rs:79`)

`grass_app::prelude::*` re-exports:
`App`, `ConfigSnippets`, `GenerateConfigFlag`, `ScheduleSetupSet`, `SubApp`,
`Plugin`, `PluginGroup`, `PluginGroupBuilder`, `StageAdvancePlugin`,
`StageNames`, `StatesPlugin`.

Note: `type_ids!` is `#[macro_export]` (crate root, not prelude). Users must
import it as `use grass_app::type_ids;` or use the full path — it does NOT
appear in `prelude`.

### Blanket impl (plugin.rs:149)

Any `Fn(&mut App) + Send + Sync + 'static` implements `Plugin` automatically.
Closures can be passed directly to `add_plugins` for inline registration and
tests.

---

## 3. Config / TOML schema

`grass_app` itself reads **no TOML** and has **no config keys**. It is the
*collector* of TOML, not a consumer:

- `Plugin::default_config` (`plugin.rs:100`) lets each plugin return a `&str`
  TOML snippet. The framework accumulates these into the `ConfigSnippets`
  resource (`app.rs:31`) as plugins register.
- When `GenerateConfigFlag` is present at `start()` time, the assembled
  snippets are printed to stdout (`app.rs:403–413`), one per plugin, with a
  `# Generated configuration` header.

The format/content of each snippet is entirely up to the implementing plugin;
`grass_app` does not validate or parse TOML itself.

---

## 4. Key behaviors, invariants & gotchas

**A. Two validation mechanisms with different timing (lib.rs:12–25)**

TypeId dependency checks are **eager** (fire during `add_plugins`); capability
contract checks are **lazy** (fire at `start`/`prepare`). A plugin that
`requires` a capability but the provider is never added will compile and run
without error until `start()` is called. Conversely, a plugin that lists
`dependencies` with a missing plugin will panic immediately on `add_plugins`.

**B. Externally-driven path: `run_cleanup` is NOT automatic (lib.rs:29–36)**

When using `prepare()` + manual `run()` loop + `is_done()`:
`run_cleanup()` is **never called by the framework**. The caller must invoke
it explicitly after the loop exits. Forgetting this skips final-output writes
and `finalize_mpi`. The self-driving `start()` path calls it automatically.
`app.rs:391–400` shows the contrast.

**C. Cleanup ordering is deterministic and load-bearing (lib.rs:38–45, app.rs:356–364)**

`add_cleanup_with_app` closures (receive `&mut App`) run **before**
`add_cleanup` functions (resource-free). This is intentional: output writers
that need live resources (e.g., write final dump from `FlowField`) must
register via `add_cleanup_with_app`, and teardown functions (e.g.,
`finalize_mpi`) must register via `add_cleanup`. Reversing this order causes
use-after-free of resources.

**D. `is_unique` defaults to `true`; duplicate panics are immediate (app.rs:104–106)**

Adding the same plugin type twice panics with a clear message. To allow
multiple instances (e.g., two output plugins configured differently), the
plugin must override `is_unique` to return `false`. The name returned by
`name()` is what the duplicate check uses, not the TypeId.

**E. `set_schedule` must be called AFTER all systems are registered (app.rs:287–293)**

The scheduler walks the tree at install time to assign namespaces and prepare
loop conditions. Any `add_update_system` call after `set_schedule` places that
system outside the tree's namespace ordering. The doc comment states this
explicitly but it is easy to miss.

**F. `type_ids!` is not in the prelude (lib.rs:79–84)**

The `type_ids!` macro is exported at the crate root (`#[macro_export]`,
`plugin.rs:42`) and is commonly needed when implementing `Plugin::dependencies`.
It is **not** re-exported in `prelude`. Users must add `use grass_app::type_ids;`
separately. This is a common stumble point for new plugin authors.

**G. `PluginGroupBuilder::add` silently skips disabled plugins (plugin.rs:198–203)**

If `.disable::<P>()` is called and then `.add(P {...})` is called, the plugin
is silently dropped. There is no warning. This is intentional for the
override pattern, but consumers who accidentally disable a plugin they needed
get no diagnostic.

**H. `StagesPlugin` + `StageAdvancePlugin` must use the same schedule phase (plugin.rs:257–263, 292–298)**

Both plugins take a `phase: impl ScheduleSet` argument. If they are given
different phases, the transition system and the stage-advance check run in a
different order than expected. Convention is `PostFinalIntegration` or the
last phase of the update loop.

**I. Plugin `build()` is called synchronously during `add_plugins` (app.rs:123)**

The plugin name is recorded before `build()` runs (app.rs:121) so that nested
`add_plugins` calls inside `build()` can see the parent plugin as already
registered. This prevents false-positive dependency errors in plugin groups
that add a dependency and its dependent in sequence. Docs should explain this
"pre-register then build" pattern.

**J. `SubApp` is currently singular; `SubApps` is reserved (sub_app.rs:1–6)**

`SubApps` always holds exactly one `main: SubApp`. The struct exists to support
future multi-world scenarios. External code should not construct `SubApps`
directly. `grass_multi` adds sub-apps through its own extension trait, not
through `SubApps`.

---

## 5. Tutorial outline

A tutorial chapter should teach these steps in order:

1. **Create an `App`** — `App::new()`, what it owns (resource store, system
   lists, plugin registry).
2. **Add resources** — `app.add_resource(MyConfig { ... })`. Explain that
   resources are stored by type; adding the same type twice replaces the value.
3. **Write systems** — plain functions with `Res<T>` / `ResMut<T>` params.
   Show the dependency-injection model (no manual lookup).
4. **Name phases with `ScheduleSet`** — define a `Step` enum with
   `#[derive(Debug, Clone, Copy, ScheduleSet)]`, register systems with
   `add_update_system(sys, Step::Phase)`.
5. **Use `ScheduleSetupSet` for one-time setup** — contrast setup systems
   (run once) vs. update systems (run every tick); show `PreSetup` / `Setup` /
   `PostSetup` ordering.
6. **Bundle into a `Plugin`** — implement `Plugin::build` to group resource +
   system registration.
7. **Bundle plugins into a `PluginGroup`** — show `PluginGroupBuilder::start`,
   `.add`, and how downstream groups can `.disable` and replace a plugin.
8. **Use `Plugin::dependencies` for hard ordering** — show `type_ids!` macro,
   explain eager vs. lazy validation.
9. **Use `Plugin::provides` / `requires` for loose contracts** — show capability
   string tags, explain that order does not matter.
10. **Provide a default config snippet** — implement `Plugin::default_config`
    returning a `&str` TOML block; wire `--generate-config` CLI flag to
    `GenerateConfigFlag`.
11. **Self-driving run**: `app.start()` — the standard path, cleanup is
    automatic.
12. **Externally-driven run**: `prepare()` → loop `run()` until `is_done()` →
    `run_cleanup()` — call out the MUST on `run_cleanup`.
13. **Cleanup ordering**: distinguish `add_cleanup_with_app` (resource-aware,
    runs first) from `add_cleanup` (resource-free, runs after).
14. **Runtime state machines**: `StatesPlugin<S>` + `StageAdvancePlugin<S>` +
    `#[derive(StageEnum)]` for multi-phase simulations.
15. **Debugging**: `enable_schedule_print()` for `schedule.dot`, `has_update_system`
    guard pattern in plugin `build`.

---

## 6. Doc gaps

### `model/app-plugin.md` (the primary chapter — docs/src/model/app-plugin.md)

The existing chapter is quite good and covers the validation model, lifecycle
paths, cleanup ordering, and the `--generate-config` recipe at a high level.
The following are missing or underspecified:

1. **`ScheduleSetupSet` is not mentioned.** The chapter teaches update-system
   phases but never shows setup-phase ordering. New plugin authors reach for
   per-codebase setup enums when `ScheduleSetupSet` exists for exactly this.
   Should add a short section or sidebar.

2. **`type_ids!` macro is not in the prelude — never stated.** The chapter
   shows `Plugin::dependencies` and references `type_ids!` in the table, but
   never says how to import it. Users see it in the source and try
   `use grass_app::prelude::*` — it doesn't import it. Needs an explicit note:
   `use grass_app::type_ids;`.

3. **`StatesPlugin` / `StageAdvancePlugin` are mentioned only in passing
   (in the validation-model table footnote in the scheduler chapter).** The
   app-plugin chapter has no example of how to wire a state machine. A concrete
   2-plugin pattern (`StatesPlugin::new(Phase::Settle, ...)` + `StageAdvancePlugin::new(...)`)
   is missing. The gotcha that both must use the same schedule phase is
   undocumented anywhere.

4. **`has_update_system` guard pattern is undocumented.** Plugin authors
   frequently need to "register a system only if the user hasn't already."
   `has_update_system` exists for this but is never mentioned in docs.

5. **`Plugin::is_unique` override is never explained.** The chapter says
   duplicate unique plugins panic but never shows how to allow multiple
   instances of a plugin (return `false` from `is_unique`).

6. **`resource_cell` / `get_resource_ref` vs. `get_mut_resource` distinction
   is not explained.** The `resource_cell` method exists specifically to let
   `grass_multi` couplers hold references to multiple resources in one
   expression with only `&self`; this design is invisible to docs readers.

7. **`set_schedule` timing invariant is not in the chapter.** It is in the
   source doc-comment (`app.rs:287`) but absent from the book. Users who add
   systems after `set_schedule` get silent wrong ordering.

8. **Closure-as-plugin blanket impl is in the table but the import story is
   incomplete.** The README and chapter both mention it, but neither shows the
   full closure signature that satisfies `Fn(&mut App) + Send + Sync + 'static`.
   Tests closures often omit the bounds which only fail at compile time.

### `tutorial/write-your-own-solver.md`

9. **Step 4 ("decide when to stop") is a stub.** The text says "confirm the
   name against `grass_app` in your checkout" instead of actually showing the
   API. At the time of writing the exact stop-condition API was uncertain; now
   that `is_done` / `SchedulerManager` is clear, this step should be completed
   with a real code example.

10. **`ScheduleSetupSet` is not used in the tutorial.** The tutorial shows
    `add_update_system` but never `add_setup_system`. A realistic plugin almost
    always has at least a `PreSetup` system that reads the config file. The
    tutorial should add a setup step for resource initialization.

11. **The externally-driven lifecycle is not shown anywhere in the tutorial.**
    `grass_multi` users need it, but it appears only in the `app-plugin.md`
    reference chapter, not in any tutorial. A short "advanced: externally-driven
    loop" section in the tutorial or a separate page would help.

### `reference/crates.md`

12. **The `grass_app` row says only "top-level container and lifecycle."** At
    the level of detail of this reference page, `PluginGroup`, `StatesPlugin`,
    `StageAdvancePlugin`, `ScheduleSetupSet`, `GenerateConfigFlag`, and the
    cleanup system are invisible. The row should expand to at least a sentence
    or two covering the plugin-group override pattern and the two lifecycle paths.

### Missing entirely

13. **No page explains `SubApp` / `SubApps`.** These are public types visible
    to anyone who calls `app.main()` / `app.main_mut()`. Their purpose
    (currently singular; exists for future multi-world) and the `prepare`
    method on `SubApp` (distinct from `App::prepare`) are undocumented.

14. **No page explains the "pre-register then build" invariant (gotcha I).** A
    plugin whose `build()` calls `app.add_plugins(AnotherPlugin)` relies on the
    parent plugin being pre-registered before `build` runs. Without understanding
    this, authors write plugins that fail with confusing dependency errors when
    the dependency is added inside `build`.

---

## 7. Suggested placement

| Content | Where it belongs |
|---------|-----------------|
| `ScheduleSetupSet` with `PreSetup`/`Setup`/`PostSetup` example | Add to `model/app-plugin.md` as a new subsection "Setup-phase ordering" |
| `type_ids!` import note | Add inline to `model/app-plugin.md` where `Plugin::dependencies` is shown |
| `StatesPlugin` + `StageAdvancePlugin` wiring example | Add to `model/app-plugin.md` under "States and stages" subsection; cross-reference `model/scheduler.md` |
| `has_update_system` guard pattern | Add to `model/app-plugin.md` under "Plugin authoring tips" |
| `is_unique = false` multi-instance pattern | Add to `model/app-plugin.md` near the duplicate-plugin discussion |
| `set_schedule` timing invariant | Add to `model/scheduler.md` (already covers `set_schedule`) with a "gotcha" callout |
| `resource_cell` design rationale | Add to `model/mpi-coupling.md` or a footnote in `model/app-plugin.md` |
| Externally-driven loop worked example | Expand step 4+ of `tutorial/write-your-own-solver.md` or add a new tutorial page |
| Stop-condition worked example | Complete the stub in `tutorial/write-your-own-solver.md` step 4 |
| `SubApp` / `SubApps` explanation | Add a sidebar or footnote in `model/app-plugin.md` under "App internals" |
| "Pre-register then build" invariant | Add to `model/app-plugin.md` under "Plugin authoring tips" |
| `crates.md` expanded `grass_app` row | Expand inline; link to `model/app-plugin.md` |
