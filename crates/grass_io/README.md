# grass_io

Optional companion crate to [`grass_app`](../grass_app/) — TOML config
loading + the three observability plugins every real simulation wants
but the framework core shouldn't be required to pull in.

Apps that don't want any of this don't depend on `grass_io` at all.
Apps that want to swap one piece (roll their own thermo, write
checkpoints in a custom format) skip that plugin and add their own —
every piece here is a plugin, not a hardwired assumption.

## What's in here

| item | TOML section | what it does |
|---|---|---|
| [`Config`](src/config.rs) + [`InputPlugin`](src/config.rs) | reads `args[1]` | `InputPlugin` parses CLI, loads the input TOML, installs a `Config` resource. Each plugin's `build()` calls `Config::load::<MyConfig>(app, "key")` to seed its own typed config. |
| [`SimClockPlugin`](src/clock.rs) + [`SimClock`](src/clock.rs) | `[clock]` | `step` / `time` accumulator. `every_n_steps(n)` is a `.run_if(...)` predicate that gates periodic work (term_out prints, dump writes). |
| [`RunPlugin`](src/run.rs) + [`RunSchedule`](src/run.rs) | `[run]` | reads `[run] steps`, ends the App at that count. Auto-installs `SimClockPlugin` and auto-registers `advance_step` if not already present. Namespaced at `RUN_NAMESPACE = 1000` so non-`Loop` schedules don't need explicit `set_schedule`. |
| [`TermOutPlugin`](src/term_out.rs) + [`TermOut`](src/term_out.rs) | `[term_out]` | LAMMPS-style aligned terminal log. User systems push named values via `TermOut::set(name, value)`; plugin prints aligned columns at the configured cadence. |
| [`DumpPlugin<F>`](src/dump.rs) + [`DumpBuffer`](src/dump.rs) | `[dump]` | per-frame file output. Generic over [`DumpFormat`](src/dump.rs); ships [`RawFrameWriter`](src/dump.rs) (writes bytes verbatim — JSON / CSV / binary all work). User fills `DumpBuffer.payload`; plugin handles path templating + file IO. |
| [`MultiIoExt::add_subapp_with_config`](src/config.rs) | (sub-App registration) | extension method on `App` that bridges `Config::for_subapp` + `add_subapp` — slices the parent's main TOML for a named sub-App, pre-seeds the slice as the sub-App's local `Config`, runs the user closure to add plugins, registers the sub-App. |

## CLI surface (via `InputPlugin`)

```
myapp <config.toml>          # run
myapp --generate-config      # print every plugin's default_config snippet
                             # (assembled into a complete starter TOML) and exit
```

`--generate-config` works because every plugin in this crate implements
`Plugin::default_config`. The same pattern applies to user plugins — emit
your snippet, get free `--generate-config` support.

## Example shape — minimum

The simple case (no observability) is three plugins plus `start`.
`RunPlugin` auto-installs `SimClockPlugin`, auto-registers
`advance_step`, and is namespaced high so you don't need
`set_schedule`:

```rust
use grass_app::prelude::*;
use grass_io::{InputPlugin, RunPlugin};

let mut app = App::new();
app.add_plugins(InputPlugin);     // reads the TOML at args[1]
app.add_plugins(MyPhysicsPlugin);
app.add_plugins(RunPlugin);
app.start();
```

## Example shape — with observability

Add `TermOutPlugin` / `DumpPlugin` for periodic terminal output and
file dumps, plus user systems that push columns / payload bytes:

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

// User systems push columns into TermOut and bytes into DumpBuffer.
fn set_columns(state: Res<MyState>, mut term: ResMut<TermOut>) {
    term.set("x", state.x);
    term.set("v", state.v);
}
fn build_dump(state: Res<MyState>, mut buf: ResMut<DumpBuffer>) {
    buf.payload = serde_json::to_vec(&*state).unwrap();
}
app.add_update_system(set_columns, TermOutSchedule::Compute);
app.add_update_system(build_dump.run_if(every_n_steps(50)), DumpSchedule::Build);

// IMPORTANT: register `advance_step` BEFORE adding `RunPlugin` if you
// want it in a specific phase (here so `TermOut` reads the just-
// completed `step`). The `RunPlugin` build() guards via
// `App::has_update_system` and skips its auto-add when it sees the
// existing registration.
app.add_update_system(advance_step, MyPhysicsPhase::Step);

app.add_plugins(RunPlugin);
app.start();
```

A worked end-to-end demo with a `main.toml` driving all five plugins
lives at [`examples/io/`](../../examples/io/).

## Conditional registration (the "fix gravity" pattern)

When a plugin should only register systems if the user opted in via
TOML — DIRT-style `[gravity]` body force — the plugin's `build()`
checks whether its config section has non-default values and
short-circuits otherwise:

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

`Config::section` and `Config::load` both return `T::default()` for
missing sections, so this pattern is one `if`-statement away.

## Multi-App config

Coupled simulations need each sub-App seeded from its own slice of the
main TOML. [`Config::for_subapp`](src/config.rs) handles two
compositional models, optionally combined:

- **Namespace prefix.** `[a.oscillator] dt = 1e-3` in main.toml shows
  up as `[oscillator] dt = 1e-3` to sub-App `a`'s plugins. The plugin
  code never knows the prefix existed.
- **File reference.** `[subapps.a] config_path = "a.toml"` in main.toml
  loads `a.toml` (relative to main.toml's directory) as sub-App `a`'s
  base Config. The same file works standalone if you point a single-App
  binary at it.
- **Combined.** `[subapps.a] config_path = "..."` for the bulk plus
  inline `[a.section] knob = ...` overrides deep-merged on top. Useful
  for per-run tweaks without editing the per-domain file.

The `add_subapp_with_config` extension method bundles the slice-and-
seed pattern into one call:

```rust
parent.add_subapp_with_config("dem", |app| {
    app.add_plugins(DemPhysicsPlugins);
});
parent.add_subapp_with_config("cfd", |app| {
    app.add_plugins(CfdPhysicsPlugins);
});
```

The closure receives a fresh sub-App with its `Config` already pre-
seeded from the relevant slice (`[dem.*]` / `[cfd.*]` or whatever the
`config_path` reference loaded). Anything the closure registers —
plugins, resources, systems — runs against that pre-seeded `Config`.

## See also

- [`grass_app`](../grass_app/) — the App/Plugin layer that hosts the
  resources and runs the schedule.
- [`grass_scheduler`](../grass_scheduler/) — the underlying scheduler;
  TermOut and Dump declare phase enums (`TermOutSchedule`,
  `DumpSchedule`) so user systems can wire up to them.
- [`grass_multi`](../grass_multi/) — sub-App registration and
  cross-namespace SystemParams (`MultiRes<T, NS>`) used to read sub-App
  state from parent-level TermOut / Dump systems.
- [`examples/io/`](../../examples/io/) — single-oscillator demo
  exercising every plugin.
