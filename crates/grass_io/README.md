# grass_io

TOML config loading plus simulation observability (clock, run loop, terminal log, file dumps) for the [`grass`](../../) simulation framework.

Optional companion to [`grass_app`](../grass_app/). It provides the things every real simulation wants but the framework core shouldn't be forced to pull in. Apps that want none of it don't depend on `grass_io`; apps that want to swap one piece (roll their own thermo, write checkpoints in a custom format) skip that plugin and add their own. Every piece here is a plugin, not a hardwired assumption.

## What it does

`InputPlugin` parses the CLI, reads the TOML file at `args[1]`, and installs a `Config` resource holding the parsed table. Each plugin's `build()` then calls `Config::load::<MyConfig>(app, "section")` for optional config or `Config::load_required::<MyConfig>(app, "section")` for required config, deserializing its own slice and registering it as a resource. Because every plugin here implements `Plugin::default_config`, `--generate-config` assembles a complete starter TOML from all registered plugins.

The remaining plugins gate periodic work on a shared step/time clock.

## Key types and plugins

| item | TOML | what it does |
|---|---|---|
| `Config` + `InputPlugin` | `args[1]` | `InputPlugin` parses CLI, loads the input TOML, installs a `Config` (and `Input`) resource. `Config::load`/`section` read optional sections and return `T::default()` for missing ones; `Config::load_required`/`required_section` report missing required sections as config errors; `parse_array` deserializes `[[section]]` arrays. |
| `SimClock` + `SimClockPlugin` | `[clock]` | `step` / `time` accumulator. Install the resource (optionally seeded with `start_step`/`start_time`); add `advance_step` in whichever phase should tick. `every_n_steps(n)` is a `.run_if(...)` predicate gating periodic work. |
| `RunPlugin` + `RunConfig` + `RunSchedule` | `[run]` / `[[run]]` | drives one or more run stages (single table or array of tables), each with its own `steps`, optional `name`/`dt`/`skip`/`save_at_end`, and a flattened `overrides` catch-all merged into `StageOverrides`. `StageOverrides::section` defaults only when a section is absent; malformed present sections fail with a parse diagnostic. Auto-installs `SimClockPlugin` and `advance_step`; ends the App after the final stage. Namespaced at `RUN_NAMESPACE = 1000`. |
| `TermOutPlugin` + `TermOut` + `TermOutSchedule` | `[term_out]` | LAMMPS-style aligned terminal log. User systems push named values via `TermOut::set` in `TermOutSchedule::Compute`; `step`/`time` are auto-populated. Prints every `every` steps (`0` disables). |
| `DumpPlugin<F>` + `DumpBuffer` + `DumpSchedule` | `[dump]` | periodic per-frame file output. Generic over `DumpFormat`; ships `RawFrameWriter` (writes bytes verbatim). User fills `DumpBuffer.payload` in `DumpSchedule::Build`; plugin templates the path (`{step}`/`{step:0N}`/`{time}`) and writes every `interval` steps (`0` disables). |
| `MultiIoExt::add_subapp_with_config` | sub-App registration | extension method on `App` that slices the parent TOML for a named sub-App, pre-seeds the slice as that sub-App's local `Config`, runs the user closure to add plugins, and registers the sub-App via `grass_multi`. |

## CLI surface (via `InputPlugin`)

```
myapp <config.toml>          # run
myapp --generate-config      # print every plugin's default_config snippet,
                             # assembled into a complete starter TOML, then exit
```

## Usage — minimum

The simple case (no observability) is three plugins plus `start`. `RunPlugin` auto-installs `SimClockPlugin`, auto-registers `advance_step`, and is namespaced high so you don't need `set_schedule`:

```rust
use grass_app::prelude::*;
use grass_io::{InputPlugin, RunPlugin};

let mut app = App::new();
app.add_plugins(InputPlugin);     // reads the TOML at args[1]
app.add_plugins(MyPhysicsPlugin);
app.add_plugins(RunPlugin);
app.start();
```

## Usage — with observability

Add `TermOutPlugin` / `DumpPlugin` for periodic terminal output and file dumps, plus user systems that push columns / payload bytes:

```rust
use grass_app::prelude::*;
use grass_io::{
    advance_step, every_n_steps, DumpBuffer, DumpPlugin, DumpSchedule,
    InputPlugin, RunPlugin, TermOut, TermOutPlugin, TermOutSchedule,
};

let mut app = App::new();
app.add_plugins(InputPlugin);
app.add_plugins(MyPhysicsPlugin);
app.add_plugins(TermOutPlugin);
app.add_plugins(DumpPlugin::default());

fn set_columns(state: Res<MyState>, mut term: ResMut<TermOut>) {
    term.set("x", state.x);
    term.set("v", state.v);
}
fn build_dump(state: Res<MyState>, mut buf: ResMut<DumpBuffer>) {
    buf.payload = serde_json::to_vec(&*state).unwrap();
}
app.add_update_system(set_columns, TermOutSchedule::Compute);
app.add_update_system(build_dump.run_if(every_n_steps(50)), DumpSchedule::Build);

// Register advance_step BEFORE RunPlugin if you want it in a specific phase
// (e.g. so TermOut reads the just-completed step). RunPlugin's build() guards
// via App::has_update_system and skips its auto-add when it sees the registration.
app.add_update_system(advance_step, MyPhysicsPhase::Step);

app.add_plugins(RunPlugin);
app.start();
```

## Conditional registration (the opt-in pattern)

A plugin that should register systems only when the user opted in via TOML checks whether its config section is non-default and short-circuits otherwise:

```rust
impl Plugin for GravityPlugin {
    fn build(&self, app: &mut App) {
        let cfg = Config::load::<GravityConfig>(app, "gravity");
        if cfg == GravityConfig::default() {
            return; // nothing was set, opt out
        }
        app.add_update_system(apply_gravity, MyPhase::Force);
    }
}
```

`Config::section` and `Config::load` both return `T::default()` for missing sections, so this is one `if` away.

## Required config

Use the required-read helpers when the plugin cannot run correctly without its TOML section. A missing or misspelled section is reported as a config error naming the section and pointing the user back to `--generate-config`:

```rust
impl Plugin for MaterialPlugin {
    fn build(&self, app: &mut App) {
        let cfg = Config::load_required::<MaterialConfig>(app, "material");
        app.add_update_system(apply_material_model, MyPhase::Force);
    }
}
```

Use `Config::required_section::<T>("material")` when you already have a `Config` reference, or `Config::try_required_section::<T>("material")` in tests and wrappers that need to return a `Result`. Keep using `Config::load`/`section` for optional plugins whose absence means "use defaults" or "do not register".

## Multi-App config

Coupled simulations need each sub-App seeded from its own slice of the main TOML. `Config::for_subapp` handles two compositional models, optionally combined:

- **Namespace prefix.** `[a.oscillator] dt = 1e-3` in main.toml shows up as `[oscillator] dt = 1e-3` to sub-App `a`'s plugins.
- **File reference.** `[subapps.a] config_path = "a.toml"` loads `a.toml` (relative to main.toml's directory) as sub-App `a`'s base config. The same file works standalone.
- **Combined.** A `config_path` base with inline `[a.section]` overrides deep-merged on top.

`add_subapp_with_config` bundles the slice-and-seed pattern into one call:

```rust
parent.add_subapp_with_config("dem", |app| {
    app.add_plugins(DemPhysicsPlugins);
});
parent.add_subapp_with_config("cfd", |app| {
    app.add_plugins(CfdPhysicsPlugins);
});
```

The closure receives a fresh sub-App with its `Config` (and `Input`) already pre-seeded from the relevant slice. Anything the closure registers runs against that pre-seeded config.

## See also

- [`grass_app`](../grass_app/) — the App/Plugin layer that hosts the resources and runs the schedule.
- [`grass_scheduler`](../grass_scheduler/) — the underlying scheduler; `RunSchedule`, `TermOutSchedule`, and `DumpSchedule` are phase enums user systems wire up to.
- [`grass_multi`](../grass_multi/) — sub-App registration and cross-namespace SystemParams used to read sub-App state from parent-level term_out / dump systems.

## License

MIT OR Apache-2.0
