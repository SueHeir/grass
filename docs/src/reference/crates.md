# Crate Map

| crate | role |
|---|---|
| [`grass_app`](https://github.com/SueHeir/grass/tree/main/crates/grass_app) | `App` / `Plugin` / `PluginGroup` — top-level container, the plugin-group override pattern, two lifecycle paths (`start` vs `prepare`/`run`/`run_cleanup`), `ScheduleSetupSet`, `StatesPlugin` / `StageAdvancePlugin`, and `--generate-config` |
| [`grass_scheduler`](https://github.com/SueHeir/grass/tree/main/crates/grass_scheduler) | typed-resource scheduler; `Schedule { Phase, Sequence, Loop, Branch }` tree; run conditions; states and stages |
| [`grass_derive`](https://github.com/SueHeir/grass/tree/main/crates/grass_derive) | `#[derive(ScheduleSet)]`, `#[derive(StageEnum)]`, `#[derive(Namespace)]` |
| [`grass_multi`](https://github.com/SueHeir/grass/tree/main/crates/grass_multi) | cross-namespace coupling — `MultiRes<T, NS>` / `MultiResMut<T, NS>`, `add_subapp` / `add_remote_subapp`, `Wire` / `Transport` / `MpiInterCommTransport` |
| [`grass_io`](https://github.com/SueHeir/grass/tree/main/crates/grass_io) | optional companion: TOML config (`Config` + `InputPlugin`), `SimClock`, `RunPlugin`, `TermOut`, `Dump` |
| [`grass_mpi`](https://github.com/SueHeir/grass/tree/main/crates/grass_mpi) | thin MPI abstraction (`CommBackend`); powers `MpiInterCommTransport` |

Lower tiers never depend on higher ones: GRASS depends on nothing
particle-specific; [SOIL](https://github.com/SueHeir/soil) builds on GRASS;
[DIRT](https://github.com/SueHeir/dirt) builds on SOIL.

> *Stub — expand each row into its own page.*
