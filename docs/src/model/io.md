# I/O and Configuration

`grass_io` is an **optional companion** to `grass_app`. It provides the things
every real simulation wants but the framework core shouldn't be required to pull
in:

- **`Config` + `InputPlugin`** — read a TOML file at startup and seed plugin
  parameters from it. Mirrors DIRT's `Config::load::<T>(app, "section")`
  convention, so plugins port between the two with no reshape.
- **`SimClockPlugin`** — a `step` / `time` resource (`SimClock`) that everything
  periodic gates against (see `every_n_steps`).
- **`TermOutPlugin`** — periodic terminal output, LAMMPS-style.
- **`DumpPlugin`** — periodic file output, LAMMPS-style.
- **`RunPlugin`** — drives multi-stage `[[run]]` workflows and the run-end check.

All of these **ship today** — they are plugins you opt into, not stubs. Apps
that don't want any of this don't depend on `grass_io` at all. Apps that want to
swap one piece (e.g. roll their own terminal output) skip that plugin and add
their own — every piece here is a plugin, not a hardwired assumption.

## Schedule ordering and namespaces

`grass_io`'s plugins each pin their schedule enum to a fixed namespace so their
systems sort *after* a solver's per-step work, in a deliberate sequence. (Recall
from [The Scheduler](./scheduler.md) that systems sort by `(namespace, index)`.)

| Namespace | Owner | Runs |
|-----------|-------|------|
| `0` (default) | **your** solver phases | the actual physics each step |
| `100` (`TERM_OUT_NAMESPACE`) | `TermOutPlugin` | gather + print the terminal log line |
| `200` (`DUMP_NAMESPACE`) | `DumpPlugin` | write periodic dump files |
| `1000` (`RUN_NAMESPACE`) | `RunPlugin` | advance the `[[run]]` stage / signal end-of-run |

The gaps are intentional: observability (term_out, dump) sees the step's *final*
state because it runs after the solver, and the run-end / stage-advance check
runs last so it acts on a fully-updated step.

This namespace discipline is the reason the namespace-0 footgun in
[The Scheduler](./scheduler.md) matters: a solver that leaves its phases at the
default namespace 0 still sorts cleanly *before* these I/O bands, but two
solvers both at namespace 0 would interleave.

## Config and InputPlugin

`InputPlugin` is the entry point. On `build` it reads CLI args, loads the TOML
file they name, and installs two resources: `Config` (the whole parsed
`toml::Table`) and `Input` (the input filename plus a resolved `output_dir`).
Every other `grass_io` plugin, and your own plugins, then pull their section out
of `Config`.

```rust
use grass_app::prelude::*;
use grass_io::*;

let mut app = App::new();
app.add_plugins(InputPlugin);   // parses argv, loads the .toml, installs Config
```

A plugin reads its own typed section in `build` and registers the deserialized
struct as a resource:

```rust
#[derive(serde::Deserialize, Default)]
struct MyConfig { gravity: f64 }

impl Plugin for MyPlugin {
    fn build(&self, app: &mut App) {
        let cfg: MyConfig = app
            .get_resource_ref::<Config>()
            .unwrap()
            .section::<MyConfig>("my_section");
        app.add_resource(cfg);
    }
}
```

The two helpers differ in error behaviour:

- `Config::section::<T>("key")` returns `T::default()` if `[key]` is **absent**.
- `Config::load::<T>(app, "key")` does the same and also registers the result as
  a resource in one call.

> **Warning: a missing section is silent, a malformed one is fatal.** Because an
> absent section falls back to `T::default()`, a *misspelled* section name reads
> as "use the defaults" with no error — the simulation runs with values you never
> set. The only hard error is a section that is present but does not parse, which
> prints an actionable message and exits. Double-check section names against your
> `--generate-config` output.

`InputPlugin` is idempotent: if a `Config` resource already exists when it
builds, it returns immediately without touching CLI args. That is what makes
tests possible without a file — seed the config from a string first:

```rust
let mut app = App::new();
app.add_resource(Config::from_str(r#"
    [run]
    steps = 10
"#));
app.add_plugins(InputPlugin);   // sees Config already present, does nothing to argv
```

### `--generate-config`

`grass_app` collects a TOML snippet from every plugin's `default_config` into the
`ConfigSnippets` resource. Wiring the `GenerateConfigFlag` resource to a
`--generate-config` CLI flag makes `start()` print the assembled example config
and exit without running (see
[App, Plugin, PluginGroup](./app-plugin.md#the---generate-config-recipe)). This
is the canonical way to discover every TOML key a built app understands.

## SimClock and the step counter

`SimClockPlugin` installs `SimClock { step: u64, time: f64 }` — the shared
counter everything periodic gates against — plus a `ClockConfig` read from
`[clock]` for restart support. It deliberately does **not** register the
`advance_step` system that ticks the counter; the caller chooses the phase:

```rust
app.add_plugins(SimClockPlugin);
app.add_update_system(advance_step, MyStep::EndOfStep);   // you pick the phase
```

`RunPlugin` *does* register `advance_step` for you (in its `RunSchedule::Cycle`
band) — but only if you haven't already registered it. If `TermOut` or `Dump`
must observe the just-completed step number, register `advance_step` yourself at
the phase you want before adding `RunPlugin`. The `every_n_steps(n)` predicate
gates a system to fire when `step % n == 0`; `every_n_steps(0)` is always
`false`, the disabled gate.

## TermOutPlugin

`TermOutPlugin` prints a periodic, LAMMPS-style terminal line. It reads
`[term_out]`, auto-populates the `step` and `time` columns from `SimClock`, and
exposes a `TermOut` resource whose `set("name", value)` your systems call to fill
the other columns. Register the column-setter in `TermOutSchedule::Compute`; the
plugin prints in `TermOutSchedule::Print`.

```rust
app.add_plugins(TermOutPlugin);
app.add_update_system(
    |mut t: ResMut<TermOut>, ke: Res<KineticEnergy>| t.set("ke", ke.0),
    TermOutSchedule::Compute,
);
```

> **Warning: `TermOut` values are sticky.** `TermOut::set` overwrites a column
> but nothing clears the map between prints. A column you set only on some steps
> carries its last value onto every intervening printed line. To show "no data
> this step", overwrite it explicitly — e.g. `t.set("ke", f64::NAN)`.

## DumpPlugin

`DumpPlugin<F>` writes periodic binary frames to disk. The generic `F` is the
frame format, any type implementing `DumpFormat`; the built-in `RawFrameWriter`
writes the buffer bytes verbatim (and creates parent directories). Your system
fills `DumpBuffer.payload` in `DumpSchedule::Build`; the plugin writes the file
in `DumpSchedule::Write`.

```rust
app.add_plugins(DumpPlugin::default());   // == DumpPlugin::new(RawFrameWriter)
app.add_update_system(
    |bodies: Res<Bodies>, mut buf: ResMut<DumpBuffer>| {
        buf.payload = serialize(&*bodies);
    },
    DumpSchedule::Build,
);
```

The `[dump] path_template` supports `{step}`, `{step:0N}` (zero-padded), and
`{time}` placeholders; relative paths resolve against `Input.output_dir`. To use
a custom on-disk format, implement `DumpFormat::write_frame` and pass it:
`DumpPlugin::new(MyCsvWriter)`.

> **Note: `interval = 0` disables dumping entirely.** When `[dump] interval` is
> `0`, the write system is *not even registered* — there is no per-step
> short-circuit, the work simply never wires in. The same is true of
> `[term_out] every = 0`. A `DumpPlugin` instance can also only be `build`'d
> once: the format is moved out of the plugin during `build`, so re-adding the
> same instance panics.

## RunPlugin and multi-stage runs

`RunPlugin` drives the run from `[run]` / `[[run]]` config and owns the run-end
signal. A single `[run]` table is one stage; an array of `[[run]]` tables is a
multi-stage run executed in order. With no `[run]` section at all, you get one
default stage of 1000 steps.

```toml
[[run]]
name  = "settle"
steps = 50000

[[run]]
name  = "flow"
steps = 100000
dt    = 5.0e-6     # per-stage dt override; 0 (default) means "leave dt alone"
```

Each stage runs for `steps` cycles; when the last stage is exhausted,
`update_cycle` sets `SchedulerManager::state = End` and the run stops. Any key
inside a `[[run]]` block that is not a known `StageConfig` field
(`name` / `steps` / `dt` / `skip` / `save_at_end`) is captured as a per-stage
**override** and deep-merged on top of the global config for the duration of that
stage. Downstream plugins read stage-aware config through
`StageOverrides::section::<T>("key")` rather than `Config::section`.

When a `StageEnum` and its `StageAdvancePlugin` are present, `RunPlugin` also
wires a `validate_stages` setup system that asserts the `[[run]]` stage count and
names match the enum's `#[stage("...")]` declarations, panicking at startup on a
mismatch. Without a registered `StageNames` resource this validation is skipped —
the run silently executes whatever stages the TOML declares. See
[Derive Macros](../reference/derives.md#stageenum) for the enum side of that
contract.

## Reference: TOML schema

A complete listing of every section `grass_io` reads, with types and defaults.

### `[clock]` — `SimClockPlugin`

```toml
[clock]
start_step = 0      # u64,  default 0   — starting step count (restart support)
start_time = 0.0    # f64,  default 0.0 — starting simulated time
```

`deny_unknown_fields`. Both default to zero; only needed for restarts.

### `[run]` / `[[run]]` — `RunPlugin`

```toml
[run]                  # or repeated [[run]] blocks for multiple stages
steps       = 1000     # u32,            default 1000  — cycles to run this stage
name        = "..."    # Option<String>, default None  — label (required by validate_stages)
dt          = 0.0      # f64,            default 0.0   — per-stage dt; 0 = don't touch
skip        = false    # bool,           default false — advance past this stage immediately
save_at_end = false    # bool,           default false — checkpoint hint for the codebase
# any other key here lands in this stage's StageOverrides (serde flatten)
```

### `[output]` — `InputPlugin`

```toml
[output]
dir = "path/to/output"   # String, optional — base for relative dump paths
```

Falls back to the input file's parent directory if absent, then to the working
directory.

### `[term_out]` — `TermOutPlugin`

```toml
[term_out]
every   = 100               # u64,         default 100 — print every N steps; 0 disables
columns = ["step", "time"]  # Vec<String>, default ["step","time"]
width   = 14                # usize,       default 14  — per-column field width
```

`deny_unknown_fields`. `step` and `time` are filled automatically from
`SimClock`; any other column must be pushed by a user system via `TermOut::set`.

### `[dump]` — `DumpPlugin`

```toml
[dump]
interval      = 0                      # u64,    default 0 — write every N steps; 0 disables
path_template = "frame_{step:06}.bin"  # String          — per-frame path
```

`deny_unknown_fields`. Placeholders: `{step}`, `{step:0N}`, `{time}`. Relative
paths resolve against `Input.output_dir`.

### `[subapps.<name>]` — `Config::for_subapp` (multi-App)

```toml
[subapps.dem]
config_path = "dem.toml"   # String — per-sub-App TOML, relative to the main file
```

Optional. When present, the referenced file is the base and any inline
`[<name>.*]` keys are deep-merged on top (inline keys win on conflict). The
`MultiIoExt::add_subapp_with_config(name, build)` extension slices the parent TOML
for a named sub-App and propagates its `[output] dir`; see
[MPI and Coupling](./mpi-coupling.md).
