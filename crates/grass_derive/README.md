# grass_derive

Proc-macro derives for the GRASS framework.

A `proc-macro = true` crate providing three enum/unit-struct derives that
implement traits defined elsewhere in the workspace. The target trait paths
(`grass_scheduler::*`, `grass_multi::*`) are referenced literally, so those
crates must be in your dependency graph when you use the derives.

## Derives

| derive | trait | what it does |
|---|---|---|
| `#[derive(ScheduleSet)]` | [`grass_scheduler::ScheduleSet`](../grass_scheduler/) | For an enum, assigns each variant a sequential index in declaration order (0, 1, 2, …) and a `name()` from the variant identifier. Variants used as schedule phases run in that order. Also accepts a unit struct (`struct Foo;`) as a single-phase marker (`to_index() = 0`). |
| `#[derive(StageEnum)]` | [`grass_scheduler::StageName`](../grass_scheduler/) | For an enum whose variants each carry a `#[stage("name")]` attribute. Binds multi-stage `[[run]]` workflows in TOML to a Rust enum. Compile error if a variant lacks `#[stage(...)]` or two stage names collide. |
| `#[derive(Namespace)]` | [`grass_multi::Namespace`](../grass_multi/) | For a unit struct, sets `NAME` to the struct's identifier. Use [`grass_multi::namespace!`](../grass_multi/) when you want a namespace string different from the struct name. |

All three reject inputs they can't handle (e.g. `ScheduleSet` rejects tuple/named
structs and unions; `Namespace` accepts unit structs only) with a clear
compile-time error.

## Usage

```rust
use grass_derive::{ScheduleSet, StageEnum, Namespace};

#[derive(Clone, Copy, Debug, PartialEq, ScheduleSet)]
enum CfdSchedule { Setup, ComputeFluxes, Integrate, PostStep }

#[derive(Clone, PartialEq, Default, StageEnum)]
enum Phase {
    #[default]
    #[stage("settle")]   Settle,
    #[stage("compress")] Compress,
}

#[derive(Namespace)]
pub struct Cfd; // Namespace::NAME = "Cfd"
```

## See also

- [`grass_scheduler`](../grass_scheduler/) — defines `ScheduleSet` and `StageName`.
- [`grass_multi`](../grass_multi/) — defines `Namespace` and the `namespace!` macro.

## License

MIT OR Apache-2.0
