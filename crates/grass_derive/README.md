# grass_derive

Proc-macro crate for the GRASS framework. Three derives:

| derive | trait | what it does |
|---|---|---|
| `#[derive(ScheduleSet)]` | [`grass_scheduler::ScheduleSet`](../grass_scheduler/src/lib.rs) | for enums, assigns each variant a sequential index by declaration order. Variants used as schedule phases run in that order. Also works on unit structs (single-phase). |
| `#[derive(StageEnum)]` | `grass_scheduler::StageName` | for enums whose variants carry `#[stage("name")]` attributes. Binds multi-stage `[[run]]` workflows in TOML to a Rust enum. |
| `#[derive(Namespace)]` | [`grass_multi::Namespace`](../grass_multi/src/multi.rs) | for unit structs; sets `NAME` to the struct's name. Use [`grass_multi::namespace!`](../grass_multi/src/multi.rs) when you want a different namespace string than the struct name. |

## Examples

```rust
use grass_derive::ScheduleSet;

#[derive(Clone, Copy, Debug, PartialEq, ScheduleSet)]
enum CfdSchedule { Setup, ComputeFluxes, Integrate, PostStep }
```

```rust
use grass_derive::StageEnum;

#[derive(Clone, PartialEq, Default, StageEnum)]
enum Phase {
    #[default]
    #[stage("settle")]   Settle,
    #[stage("compress")] Compress,
}
```

```rust
use grass_derive::Namespace;

#[derive(Namespace)]
pub struct A;  // NAME = "A"

// or with a custom string:
grass_multi::namespace!(pub B = "b");
```

## See also

- [`grass_scheduler`](../grass_scheduler/) — defines the `ScheduleSet` trait.
- [`grass_multi`](../grass_multi/) — defines the `Namespace` trait and the `namespace!` macro.
