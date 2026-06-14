# grass_app

Plugin-based application framework for explicit, time-stepping particle and grid solvers,
built on top of [`grass_scheduler`](../grass_scheduler/).

[`App`](src/app.rs) is the top-level container; [`Plugin`](src/plugin.rs) is the modular
registration unit. Every feature — physics, I/O, analysis — is a plugin that registers
resources and systems with the app during its build phase. The app then drives the
lifecycle: organize systems → setup → run → cleanup.

## Core surface

| primitive | what it does |
|---|---|
| [`App`](src/app.rs) | top-level container; owns the main [`SubApp`](src/sub_app.rs) and runs the lifecycle. `App::new()` then `App::start()` is the standard path; `prepare()` / `run()` / `run_cleanup()` / `is_done()` expose the loop for externally-driven orchestration |
| [`Plugin`](src/plugin.rs) | trait whose `build(&self, app: &mut App)` wires up a feature. Optional hooks: `name`, `is_unique`, `dependencies` (TypeId ordering), `provides` / `requires` (capability tags), `default_config` (TOML snippet). A bare `Fn(&mut App)` closure also implements `Plugin` |
| [`PluginGroup`](src/plugin.rs) / [`PluginGroupBuilder`](src/plugin.rs) | bundle several plugins into one `add_plugins(...)` call; `.disable::<P>()` skips a plugin type so a downstream group can swap an implementation |
| [`SubApp`](src/sub_app.rs) / [`SubApps`](src/sub_app.rs) | a self-contained `Scheduler` plus its resource store and plugin bookkeeping. Every `App` currently has exactly one (the `main` sub-app); `App` delegates to it |
| [`ScheduleSetupSet`](src/setup.rs) | generic 3-phase ordering for one-time setup systems: `PreSetup` → `Setup` → `PostSetup`. Reach for this in reusable plugins instead of a per-codebase setup enum |
| [`StatesPlugin<S>`](src/plugin.rs) | registers `CurrentState<S>` / `NextState<S>` and the end-of-step transition system at a chosen phase, for runtime state machines |
| [`StageAdvancePlugin<S>`](src/plugin.rs) / [`StageNames`](src/plugin.rs) | watches `CurrentState<S>` and requests scheduler stage advance; stores stage names for validation / DOT export. Pairs with `StatesPlugin` and `#[derive(StageEnum)]` |
| [`ConfigSnippets`](src/app.rs) / [`GenerateConfigFlag`](src/app.rs) | plugins emit TOML fragments via `default_config`; adding `GenerateConfigFlag` makes `start()` print the assembled config and exit |

Plugin registration validates as it goes: a duplicate unique plugin or an unmet TypeId
dependency panics with guidance, and `start()` checks that every required capability tag
has a provider.

## Example shape

```rust
use grass_app::prelude::*;
use grass_scheduler::prelude::*;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Step { Update }

struct Position(f64);

fn move_thing(mut pos: ResMut<Position>) {
    pos.0 += 1.0;
}

let mut app = App::new();
app.add_resource(Position(0.0));
app.add_update_system(move_thing, Step::Update);
app.start();
```

## Plugin groups

```rust
impl PluginGroup for MyPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(PhysicsPlugin)
            .add(OutputPlugin)
    }
}
```

Swap an implementation downstream by disabling a plugin type before re-adding:

```rust
impl PluginGroup for MyCustomPlugins {
    fn build(self) -> PluginGroupBuilder {
        MyPlugins.build()
            .disable::<OutputPlugin>()
            .add(CustomOutputPlugin)
    }
}
```

## See also

- [`grass_scheduler`](../grass_scheduler/) — the scheduler engine that `App` wraps.
- [`grass_multi`](../grass_multi/) — extension trait `MultiAppExt` adds `add_subapp` /
  `add_remote_subapp` and `tick_subapp` for cross-namespace coupling.

## License

MIT OR Apache-2.0
