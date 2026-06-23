# App, Plugin, PluginGroup

GRASS gives you three composition primitives. Together they let a complete
simulation be assembled — and reconfigured — by choosing plugins.

## App

[`App`](https://github.com/SueHeir/grass/blob/main/crates/grass_app/src/app.rs)
is the top-level container. It owns the main sub-app (a scheduler plus its
resource store) and drives the lifecycle: organize systems → setup → run →
cleanup.

```rust
let mut app = App::new();
// ... register resources, systems, plugins ...
app.start();           // the standard path: prepare + run to completion + cleanup
```

For externally-driven orchestration, the loop is also exposed piecewise:
`prepare()` / `run()` / `run_cleanup()` / `is_done()`.

## Plugin

A [`Plugin`](https://github.com/SueHeir/grass/blob/main/crates/grass_app/src/plugin.rs)
is the modular registration unit. Every feature — physics, I/O, analysis — is a
plugin whose `build(&self, app: &mut App)` wires up its resources and systems:

```rust
struct PhysicsPlugin;

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        app.add_resource(/* ... */);
        app.add_update_system(/* ... */);
    }
}
```

Optional hooks let plugins declare ordering and requirements:

| hook | purpose |
|---|---|
| `name` | human-readable identity |
| `is_unique` | reject duplicate registration |
| `dependencies` | `TypeId` ordering against other plugins |
| `provides` / `requires` | capability tags, checked at `start()` |
| `default_config` | a TOML snippet this plugin contributes |

A bare `Fn(&mut App)` closure also implements `Plugin`, so quick wiring needs no
struct. Registration validates as it goes: a duplicate unique plugin or an unmet
dependency panics with guidance.

## PluginGroup

A [`PluginGroup`](https://github.com/SueHeir/grass/blob/main/crates/grass_app/src/plugin.rs)
bundles several plugins into one `add_plugins(...)` call:

```rust
impl PluginGroup for MyPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(PhysicsPlugin)
            .add(OutputPlugin)
    }
}
```

The payoff is **swappable implementations**: a downstream group disables a plugin
type and adds its own in its place.

```rust
impl PluginGroup for MyCustomPlugins {
    fn build(self) -> PluginGroupBuilder {
        MyPlugins.build()
            .disable::<OutputPlugin>()
            .add(CustomOutputPlugin)
    }
}
```

This is exactly how SOIL and DIRT layer onto the framework: each ships a plugin
group, and a consumer can disable one plugin to substitute its own integrator,
output, or force law.

## The validation model: two independent mechanisms

`App` checks plugin wiring two different ways, at two different times:

| Mechanism | Declared by | Checked when | Order-sensitive? |
|-----------|-------------|--------------|------------------|
| **TypeId dependencies** (`Plugin::dependencies`) | `type_ids![A, B]` | **eagerly**, during `add_plugins` | **Yes** — the dependency must already be registered |
| **Capability contracts** (`Plugin::provides` / `Plugin::requires`) | `vec!["tag"]` strings | **lazily**, at `start` / `prepare` | **No** — provider may be added before *or* after |

Use **TypeId dependencies** when plugin B genuinely cannot `build()` without
plugin A's resources/systems already present (a hard ordering constraint). Use
**capability contracts** for looser "some plugin must supply `contact_forces`"
requirements, where any provider in any order satisfies the need — the
order-independence is the point.

> **Note: `type_ids!` is not in the prelude.** `Plugin::dependencies` returns a
> `Vec<TypeId>`, and the ergonomic way to build one is the `type_ids![A, B]`
> macro. It is exported at the crate root, **not** through `grass_app::prelude`,
> so `use grass_app::prelude::*;` does not bring it in. Add `use grass_app::type_ids;`
> explicitly. This trips up nearly every first-time plugin author.

```rust,ignore
use grass_app::type_ids;

impl Plugin for ForceLawPlugin {
    fn build(&self, app: &mut App) { /* ... */ }
    fn dependencies(&self) -> Vec<std::any::TypeId> {
        type_ids![NeighborListPlugin]   // must already be registered
    }
}
```

## Two lifecycle paths

- **Self-driving:** `App::start` runs the whole thing — organize → setup →
  run-loop-until-`End` → `run_cleanup`. Cleanup is automatic.
- **Externally driven:** `App::prepare` (validate + organize + setup, leaving
  the scheduler in `Run`), then call `App::run` in a loop you own (e.g. a
  `grass_multi` parent ticking sub-Apps) until `App::is_done`, then call
  `App::run_cleanup` yourself.

> **Warning: on the externally-driven path, `run_cleanup` is not automatic.**
> `start()` calls `run_cleanup()` for you; `prepare()` + a manual `run()` loop
> does **not**. If you drive the loop yourself and forget the final
> `run_cleanup()`, every registered cleanup is skipped — that means final-output
> dumps never get written and `grass_mpi::finalize_mpi` never runs. Always end an
> externally-driven loop with an explicit `app.run_cleanup();`.

```rust,ignore
app.prepare();
while !app.is_done() {
    app.run();
}
app.run_cleanup();   // REQUIRED — nothing calls it for you here
```

### Cleanup ordering

`run_cleanup` runs **resource-aware** cleanups (registered with
`add_cleanup_with_app`, which receive `&mut App`) **before** **resource-free**
cleanups (registered with `add_cleanup`). This matters when a resource-aware
cleanup writes final output that reads live resources, before a resource-free
cleanup tears the world down (e.g. `grass_mpi::finalize_mpi`).

## The `--generate-config` recipe

Each plugin can return a TOML snippet from `Plugin::default_config`; the `App`
accumulates them all into the `ConfigSnippets` resource as plugins register. If
the `GenerateConfigFlag` resource is present when `start` is called, the `App`
prints the assembled config to stdout and exits **without running the
simulation**. Wire it to a `--generate-config` CLI flag:

```rust,ignore
let mut app = App::new();
app.add_plugins(MyPlugins);
if std::env::args().any(|a| a == "--generate-config") {
    app.add_resource(GenerateConfigFlag); // start() prints snippets + exits
}
app.start();
```

## Setup-phase ordering

Systems registered with `add_update_system` run every timestep; systems
registered with `add_setup_system` run **once**, before the main loop. Just as
update systems use a `ScheduleSet` enum to order themselves, setup systems have a
ready-made one: `ScheduleSetupSet`, with three variants in this order —
`PreSetup` (0), `Setup` (1), `PostSetup` (2).

```rust
use grass_app::prelude::*;

// Read the config file first, build derived state, then validate.
app.add_setup_system(load_config,     ScheduleSetupSet::PreSetup);
app.add_setup_system(build_grid,      ScheduleSetupSet::Setup);
app.add_setup_system(check_invariants, ScheduleSetupSet::PostSetup);
```

Reach for `PreSetup` for anything that must happen before other setup (reading
TOML, allocating the resource other setup systems fill), and `PostSetup` for
validation that needs the fully-built world.

## States and stages

For runs that switch behaviour partway through — fill then flow, load then
fracture — two small plugins build a runtime state machine on top of the
scheduler's state/stage primitives:

```rust
use grass_app::prelude::*;

// The update-loop phase at which transitions and the stage-advance check run.
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Step { Integrate, Advance }

// The stages, bound to [[run]] TOML by their #[stage("...")] names.
#[derive(Clone, PartialEq, Default, StageEnum)]
enum Phase {
    #[default]
    #[stage("settle")]
    Settle,
    #[stage("flow")]
    Flow,
}

app.add_plugins(StatesPlugin::new(Phase::Settle, Step::Advance));
app.add_plugins(StageAdvancePlugin::<Phase>::new(Step::Advance));
```

`StatesPlugin<S>` registers `CurrentState<S>` / `NextState<S>` and the transition
system; gate systems with `.run_if(in_state(Phase::Flow))`. `StageAdvancePlugin<S>`
watches the current state and advances the scheduler's *stage* counter, which
`grass_io`'s `RunPlugin` ties to the `[[run]]` TOML blocks (see
[I/O and Configuration](./io.md#runplugin-and-multi-stage-runs)).

> **Warning: both plugins must use the same schedule phase.** Each takes a
> `phase: impl ScheduleSet` argument. If `StatesPlugin` and `StageAdvancePlugin`
> are given *different* phases, the transition system and the stage-advance check
> run in the wrong relative order and stage transitions misfire. Pass the same
> phase to both — conventionally the last phase of the update loop. The
> `#[derive(StageEnum)]` contract is detailed in
> [Derive Macros](../reference/derives.md#stageenum).

## Plugin authoring tips

A few patterns recur when writing plugins:

- **Guard against double registration with `has_update_system`.** When a plugin
  should add a system only if the user (or an earlier plugin) hasn't already,
  check `app.has_update_system(my_system)` before adding it. This is how
  `grass_io`'s `RunPlugin` avoids registering `advance_step` twice.
- **Allow multiple instances with `is_unique`.** Plugins are unique by default,
  so adding the same plugin type twice panics. If a plugin is *meant* to be
  registered more than once (two output writers with different config, say),
  override `fn is_unique(&self) -> bool { false }`. The duplicate check keys on
  the plugin's `name()`, not its `TypeId`.
- **`build()` runs synchronously, and the parent is pre-registered.** A plugin's
  `build()` is called during `add_plugins`, and the framework records the
  plugin's name *before* running `build()`. So a plugin whose `build()` itself
  calls `app.add_plugins(SomeDependency)` can safely have `SomeDependency`
  declare the outer plugin as a dependency — the parent already counts as
  registered.

## App internals: `SubApp` and `SubApps`

`App::main()` / `App::main_mut()` hand you a `SubApp` — the scheduler plus its
resource store and plugin bookkeeping. Most code never touches it directly; the
`App` methods delegate to it. `SubApps` is a thin wrapper that currently holds
exactly one `main: SubApp`; it exists to leave room for future multi-world setups
and should not be constructed by hand. (`grass_multi` adds its sub-Apps through
its own extension trait, not through this type — see
[MPI and Coupling](./mpi-coupling.md).) Note that `SubApp` has its own `prepare`
method, distinct from `App::prepare`: `App::prepare` first validates capability
contracts, then delegates to `SubApp::prepare`.

> See also [The Scheduler](./scheduler.md) for how phases and namespaces order
> the systems plugins register, and [I/O and Configuration](./io.md) for the
> `grass_io` plugins (`InputPlugin`, `RunPlugin`, output) that consume these
> config snippets.
