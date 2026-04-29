# grass_app

Plugin-based application framework on top of [`grass_scheduler`](../grass_scheduler/).
Provides [`App`](src/app.rs) as the top-level container and [`Plugin`](src/plugin.rs)
as the modular registration unit.

## Core surface

| primitive | what it does |
|---|---|
| [`App`](src/app.rs) | top-level container — owns a `Scheduler`, a resource map, and a list of registered plugins; `App::new()` / `App::start()` is the standard lifecycle |
| [`Plugin`](src/plugin.rs) | trait with one method `build(&self, app: &mut App)`; bundles resources + systems + sub-schedules into a reusable unit |
| [`PluginGroup`](src/plugin.rs) / [`PluginGroupBuilder`](src/plugin.rs) | combine several plugins into one `add_plugins(...)` call; supports `.disable::<P>()` for replacement |
| [`SubApp`](src/sub_app.rs) | child App owned by a parent; the unit [`grass_multi`](../grass_multi/) tracks via its `SubApps` resource for cross-namespace coupling |
| [`StatesPlugin<S>`](src/plugin.rs) | wires `CurrentState<S>` / `NextState<S>` resources from `grass_scheduler` for runtime state machines |
| [`StageAdvancePlugin<S>`](src/plugin.rs) / [`StageNames`](src/plugin.rs) | multi-stage workflow runner — drive a state machine by named stage, coordinate with TOML `[[run]]` blocks |
| [`ConfigSnippets`](src/app.rs) / [`GenerateConfigFlag`](src/app.rs) | plugins emit TOML fragments; `--generate-config` dumps them to a default config file |
| `App::has_update_system` / `set_schedule_namespace` | plugin-author tools — query whether a system is already registered (for "auto-register only if user hasn't" guards), or persistently namespace a `ScheduleSet` enum so registrations made before *or* after pick up the assignment |

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

Extend or replace plugins downstream:

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

- [`grass_scheduler`](../grass_scheduler/) — the scheduler engine that
  `App` wraps.
- [`grass_multi`](../grass_multi/) — extension trait `MultiAppExt` adds
  `add_subapp` / `add_remote_subapp` for cross-namespace coupling.
- [`examples/coupling/single_oscillator/main.rs`](../../examples/coupling/single_oscillator/main.rs)
  — minimal `App` + `Plugin` use.
