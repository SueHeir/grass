# Planning: `grass_io` documentation

*Crate:* `grass_io`  
*Planned chapter:* `docs/src/model/io.md` (already exists as a stub)  
*Status as of 2026-06-22:* stub chapter covers purpose + namespace table; no API reference, no TOML schema, no tutorial, no gotchas.

---

## Purpose

`grass_io` is the optional observability companion to `grass_app`. It supplies the five things every real simulation needs but that the zero-dependency core cannot assume: TOML config loading (`Config` + `InputPlugin`), a shared step/time counter (`SimClockPlugin`), periodic terminal logging (`TermOutPlugin`), periodic file output (`DumpPlugin`), and multi-stage run control (`RunPlugin`). Every piece is an opt-in plugin; apps that roll their own simply omit the corresponding plugin.

---

## Public surface to document

### Resources (installed by plugins)
| Resource | Plugin | File:line |
|---|---|---|
| `Config { table: toml::Table }` | `InputPlugin` | `config.rs:48` |
| `Input { filename, output_dir }` | `InputPlugin` | `config.rs:214` |
| `SimClock { step: u64, time: f64 }` | `SimClockPlugin` | `clock.rs:43` |
| `ClockConfig { start_step, start_time }` | `SimClockPlugin` | `clock.rs:54` |
| `RunConfig { stages: Vec<StageConfig> }` | `RunPlugin` | `run.rs:99` |
| `RunState { total_cycle, cycle_count, cycle_remaining }` | `RunPlugin` | `run.rs:163` |
| `StageOverrides { table: toml::Table }` | `RunPlugin` | `run.rs:188` |
| `FirstStageOnlyConfigs(Vec<(String, String)>)` | user-registered, consumed by `RunPlugin` | `run.rs:215` |
| `TermOut { every, columns, width, values, … }` | `TermOutPlugin` | `term_out.rs:53` |
| `TermOutConfig { every, columns, width }` | `TermOutPlugin` | `term_out.rs:98` |
| `DumpBuffer { payload: Vec<u8> }` | `DumpPlugin` | `dump.rs:89` |
| `DumpConfig { interval, path_template }` | `DumpPlugin` | `dump.rs:102` |

### Plugins
| Plugin | Phase enum | Namespace const | File |
|---|---|---|---|
| `InputPlugin` | — | — | `config.rs:233` |
| `SimClockPlugin` | — (advance_step user-placed) | — | `clock.rs:68` |
| `TermOutPlugin` | `TermOutSchedule::{Compute, Print}` | `TERM_OUT_NAMESPACE = 100` | `term_out.rs:138` |
| `DumpPlugin<F: DumpFormat>` | `DumpSchedule::{Build, Write}` | `DUMP_NAMESPACE = 200` | `dump.rs:136` |
| `RunPlugin` | `RunSchedule::Cycle` | `RUN_NAMESPACE = 1000` | `run.rs:232` |

### Key free functions
| Function | Purpose | File:line |
|---|---|---|
| `Config::load::<T>(app, key)` | deserialize `[key]` + register `Res<T>` | `config.rs:102` |
| `Config::section::<T>(key)` | deserialize `[key]`, return `T::default()` if absent | `config.rs:71` |
| `Config::parse_array::<T>(key)` | deserialize `[[key]]` array | `config.rs:176` |
| `Config::for_subapp(name, base_dir)` | slice parent TOML for a named sub-App | `config.rs:140` |
| `deep_merge(base, overrides)` | recursive TOML table merge | `config.rs:358` |
| `load_toml(path)` | read + parse `.toml` file, exit on error | `config.rs:373` |
| `advance_step` | system: `clock.step += 1` | `clock.rs:95` |
| `every_n_steps(n)` | `.run_if(...)` predicate: `step % n == 0` | `clock.rs:102` |
| `set_stage_name` | setup system: copy stage name + build `StageOverrides` | `run.rs:305` |
| `run_read_input` | setup system: init cycle counters + print run banner | `run.rs:342` |
| `update_cycle` | update system: tick counters, advance/end stages | `run.rs:377` |
| `validate_stages` | setup system: assert TOML stages match `StageNames` | `run.rs:418` |

### Traits
| Trait | Purpose | File:line |
|---|---|---|
| `DumpFormat` | pluggable frame writer; `write_frame(path, step, time, payload)` | `dump.rs:53` |
| `MultiIoExt` on `App` | `add_subapp_with_config(name, closure)` | `config.rs:311` |

### Concrete `DumpFormat` impl
- `RawFrameWriter` — writes bytes verbatim (`dump.rs:65`); creates parent dirs.

### Prelude / re-exports (`lib.rs:47–56`)
All public items are re-exported at the crate root. Notable namespace constants: `TERM_OUT_NAMESPACE`, `DUMP_NAMESPACE`, `RUN_NAMESPACE`.

---

## Config/TOML schema

### `[clock]` — read by `SimClockPlugin` (`clock.rs:54`)
```toml
[clock]
start_step = 0    # u64, default 0 — starting step count (restart support)
start_time = 0.0  # f64, default 0.0 — starting simulated time
```
`#[serde(deny_unknown_fields)]`. Both default to zero; only needed for restarts.

### `[run]` / `[[run]]` — read by `RunPlugin` (`run.rs:55–94`)
Single stage (`[run]`) or array of stages (`[[run]]`). Absent entirely → one default stage (1000 steps).
```toml
[run]
steps      = 1000   # u32, default 1000 — steps to run in this stage
name       = "..."  # Option<String>, default None — human label (required by validate_stages)
dt         = 0.0    # f64, default 0.0 — per-stage dt; 0 means "don't touch"
skip       = false  # bool, default false — advance past stage immediately
save_at_end = false # bool, default false — codebase hint for checkpoint writes
# Any other keys land in `overrides` (serde flatten) and appear in StageOverrides.
```
For multi-stage: replace `[run]` with `[[run]]` blocks; `RunConfig::from_config` handles both (`run.rs:116`).

### `[output]` — read by `InputPlugin` directly (`config.rs:262`)
```toml
[output]
dir = "path/to/output"  # String, optional — output directory for relative dump paths
```
Falls back to the input file's parent directory if absent. No dedicated config struct; read inline during `InputPlugin::build`.

### `[term_out]` — read by `TermOutPlugin` (`term_out.rs:98`)
```toml
[term_out]
every   = 100                    # u64, default 100 — print every N steps; 0 disables
columns = ["step", "time"]       # Vec<String>, default ["step","time"]
width   = 14                     # usize, default 14 — per-column field width
```
`#[serde(deny_unknown_fields)]`. `step` and `time` are auto-populated from `SimClock`; any other column must be pushed by a user system via `TermOut::set`.

### `[dump]` — read by `DumpPlugin` (`dump.rs:102`)
```toml
[dump]
interval      = 0                   # u64, default 0 — write every N steps; 0 disables
path_template = "frame_{step:06}.bin"  # String — per-frame path template
```
`#[serde(deny_unknown_fields)]`. Template placeholders: `{step}`, `{step:0N}` (zero-padded to N digits), `{time}`. Relative paths resolve against `Input.output_dir`. When `interval = 0` the `write_dump` system is not registered at all (`dump.rs:167–179`).

### `[subapps.<name>]` — read by `Config::for_subapp` (`config.rs:159`)
```toml
[subapps.dem]
config_path = "dem.toml"   # String — path to per-sub-App TOML (relative to main.toml)
```
Optional. When present, the file is the base; any inline `[<name>.*]` keys are deep-merged on top.

### Per-stage overrides (catch-all inside `[[run]]`)
Any key inside a `[[run]]` block that is not a `StageConfig` field (name/steps/dt/skip/save_at_end) is captured by `#[serde(flatten)] overrides: toml::Table` (`run.rs:79`). These are deep-merged with the global config into `StageOverrides` by `set_stage_name` (`run.rs:334–337`). Downstream plugins read stage-aware config via `StageOverrides::section::<T>(key)` (`run.rs:193`).

---

## Key behaviors, invariants & gotchas

### Namespace ordering is load-bearing (`lib.rs:28–39`)
All `grass_io` schedules pin to fixed namespaces (100 / 200 / 1000) so they always sort *after* user solver phases (default namespace 0). Adding a user system to `TermOutSchedule::Compute` is correct; adding it to `RunSchedule::Cycle` is unusual and may race with `update_cycle`.

### `advance_step` is user-placed for `SimClockPlugin`, auto-placed by `RunPlugin`
`SimClockPlugin` installs `SimClock` but does **not** register `advance_step` — the caller picks the phase (`clock.rs:67`). `RunPlugin` does register `advance_step` in `RunSchedule::Cycle`, but only if `app.has_update_system(advance_step)` returns false (`run.rs:261`). If you need the step to tick at a specific phase (e.g. so `TermOut` reads the just-completed step), register `advance_step` manually before adding `RunPlugin`.

### `InputPlugin` is idempotent / test-safe (`config.rs:236–239`)
If a `Config` resource is already present when `InputPlugin::build` runs, it returns immediately without touching CLI args. Tests can seed `Config::from_str(...)` then call `app.add_plugins(InputPlugin)` safely.

### Missing config section → `T::default()`, not an error (`config.rs:71–76`)
`Config::section` and `Config::load` return `T::default()` when the section is absent. This is intentional for optional/opt-in plugins but is a silent non-error when a required section is simply misspelled. The only hard error path is a present-but-unparseable section (prints actionable message + exits, `config.rs:77–90`).

### `TermOut` values persist between steps — stale data risk (`term_out.rs:67–77`)
`TermOut::set` overwrites but nothing clears the map. A column set only on some steps carries its last value on intervening printed lines. If a column should show "no data this step", the user must explicitly overwrite it (e.g. to `f64::NAN`).

### `DumpPlugin::build` can only be called once per instance (`dump.rs:172–174`)
The `DumpFormat` is stashed in a `Mutex<Option<F>>` and `take()`'d during `build`. A second `build` call panics. This is a `Plugin: Send + Sync` workaround and is documented inline.

### `every_n_steps(0)` is the disabled gate (`clock.rs:103`)
Returns `false` for any step count because of the `n > 0` guard. `TermOutPlugin` and `DumpPlugin` also check `cfg.every > 0` / `cfg.interval > 0` before registering the write system at all.

### `set_stage_name` strips `[run]` from `StageOverrides` (`run.rs:335`)
`merged.remove("run")` prevents the run array itself from leaking into per-stage config reads. Important: per-stage overrides cannot re-add a `run` section.

### `validate_stages` only fires if `StageNames` is registered (`run.rs:266–269`)
Validation of stage name/count alignment is opt-in: `RunPlugin` checks `app.get_resource_ref::<StageNames>().is_some()` and only wires `validate_stages` if a `StageAdvancePlugin` (or equivalent) has registered `StageNames`. Missing this means a TOML/code stage count mismatch silently runs whatever stages exist.

### `FirstStageOnlyConfigs` produces warnings, not errors (`run.rs:319–330`)
Overriding a first-stage-only section in a later `[[run]]` block emits a `WARNING` to stderr but does not abort. The override is ignored at the framework level; the codebase system that reads the section must treat stage 0 data as authoritative.

### `Config::for_subapp` deep-merges inline keys on top of file base (`config.rs:140–157`)
When both `config_path` and inline `[<name>.*]` keys are present, the file is loaded first and the inline keys win on conflict. This is the override pattern for per-run sweeps without editing per-domain files.

### `output_dir` propagation to sub-Apps (`config.rs:327–344`)
`add_subapp_with_config` reads `[output] dir` from the sub-App's sliced config and installs it as `Input.output_dir` on the sub-App. Without this, `DumpPlugin` on a sub-App would fall back to cwd.

---

## Tutorial outline

A step-by-step section for `model/io.md` should cover:

1. **Minimum app** — `InputPlugin` + `RunPlugin` + `app.start()`. Show the TOML `[run]` block. Show `--generate-config` output.
2. **Adding a clock column to terminal output** — add `TermOutPlugin`, register a column-setter system in `TermOutSchedule::Compute`, show `[term_out]` TOML.
3. **Periodic file dumps** — `DumpPlugin::default()` + fill `DumpBuffer.payload` in `DumpSchedule::Build`. Show `[dump]` with `{step:06}` template, explain `output_dir` resolution.
4. **Multi-stage runs** — switch `[run]` to `[[run]]` with named stages, show `StageOverrides::section` for a per-stage config read, mention `save_at_end` and `skip`.
5. **Opt-in plugin pattern** — conditional registration via `Config::load` + early return when config is default.
6. **Testing** — `Config::from_str(...)` + `app.add_plugins(InputPlugin)` idiom to bypass CLI.
7. **Custom `DumpFormat`** — implement `DumpFormat`, pass to `DumpPlugin::new(MyFormat)`.
8. **Multi-App config** — `add_subapp_with_config`, namespace-prefix vs `config_path` file reference, deep-merge override pattern.

---

## Doc gaps

| Gap | Severity | Notes |
|---|---|---|
| `StageOverrides::section` not documented in book or README | medium | Downstream consumers need this for stage-aware config reads |
| `FirstStageOnlyConfigs` absent from README and book | low | Niche but important for codebases with multi-stage setup-only sections |
| `validate_stages` / `StageNames` dependency not explained in book | medium | Users who add `StageAdvancePlugin` won't know they get name validation for free |
| `DumpPlugin<F>` generics and custom `DumpFormat` not in book | medium | `model/io.md` only mentions the trait name, no usage example |
| `every_n_steps(0)` disabled-gate behavior | low | Worth a single sentence alongside the `every`/`interval` descriptions |
| `TermOut` stale-value behavior | medium | The "values persist, set to NAN to blank" gotcha is easy to miss |
| `output_dir` fallback chain not documented (TOML > input file parent > cwd) | low | `config.rs:262–275` and `dump.rs:212–215` each have a piece of it |
| `deep_merge` recursion semantics (last writer wins on scalar conflict) | low | Relevant for the inline-override pattern |
| `Config::for_subapp` file-then-inline merge order | low | Documented in rustdoc but not in the book |

---

## Suggested placement

The chapter **`docs/src/model/io.md`** already exists and has the correct title, purpose blurb, and namespace table. It is the right home. Recommended sections to add:

```
## Config and InputPlugin
## SimClock and advance_step
## TermOutPlugin
## DumpPlugin
## RunPlugin and multi-stage runs
## Multi-App config (MultiIoExt)
## Testing without a file (Config::from_str)
## Reference: TOML schema (complete flat listing)
```

The tutorial content (step-by-step assembly) belongs in `docs/src/tutorial/write-your-own-solver.md`, which currently covers a bare solver; a second tutorial or an extended section could assemble the same solver with full `grass_io` observability.

---

*All file:line citations are from `/Users/suehr/Documents/GitHub/grass/crates/grass_io/src/`.*
