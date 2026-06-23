# Derive Macros

`grass_derive` provides **three** derives for the framework. The crate has no
runtime logic — every macro expands at compile time into a trait impl for a trait
defined in `grass_scheduler` or `grass_multi`.

- **`#[derive(ScheduleSet)]`** — implements `grass_scheduler::ScheduleSet`.
  Works on an **enum** (each variant gets a sequential index in declaration
  order) *or* on a **unit struct** (a single phase with `to_index() = 0`). Used
  to define solver phases (Setup → ComputeFluxes → Integrate → …).
- **`#[derive(StageEnum)]`** — implements `grass_scheduler::StageName` for an
  **enum** where every variant carries a `#[stage("name")]` attribute. Used to
  bind multi-stage `[[run]]` workflows in TOML to a Rust enum.
- **`#[derive(Namespace)]`** — implements `grass_multi::Namespace` for a **unit
  struct**, using the struct's identifier as the namespace string
  (`struct Cfd;` → `Namespace::NAME == "Cfd"`). Used to tag sub-Apps in a
  `grass_multi` coupling (see [MPI and Coupling](../model/mpi-coupling.md)).

So only `StageEnum` is enum-only; `ScheduleSet` also accepts unit structs, and
`Namespace` is unit-struct-only. Anything else — a tuple struct, a named-field
struct, a union — is a compile error.

## Required companion derives and dependencies

The generated code references trait paths in `grass_scheduler::*` and
`grass_multi::*` **literally** (not re-exported), so the corresponding crate
must be in your dependency graph: `grass_scheduler` for `ScheduleSet` /
`StageEnum`, `grass_multi` for `Namespace`. A type that resolves only
transitively can fail to compile.

`#[derive(ScheduleSet)]` does **not** add the trait's supertrait bounds for you.
`ScheduleSet: Copy + Clone + Debug + 'static`, so the target type must *also*
derive `Copy`, `Clone`, and `Debug` itself — e.g.
`#[derive(Clone, Copy, Debug, ScheduleSet)]`. Forget one and you get a
trait-bound error pointing at `grass_scheduler::ScheduleSet`, not at
`grass_derive` — a common first-use stumble. (`StageEnum` similarly needs
whatever `Clone` / `PartialEq` / `Default` your `[[run]]` driver expects.)

## `ScheduleSet`

For an enum, the derive generates:

```rust,ignore
// from:  #[derive(Clone, Copy, Debug, ScheduleSet)] enum Step { A, B, C }
impl ScheduleSet for Step {
    fn to_index(&self) -> u32 {
        match self { Step::A => 0, Step::B => 1, Step::C => 2 }  // declaration order
    }
    fn name(&self) -> &'static str {
        match self { Step::A => "A", Step::B => "B", Step::C => "C" }
    }
}
```

For a unit struct it generates `to_index() = 0` and `name() = "StructName"` — a
single-phase, type-level marker.

> **Invariant: declaration order is the schedule index.** `to_index()` is the
> variant's 0-based position in the enum body, so reordering variants silently
> reorders the schedule. Treat a `ScheduleSet` enum body like ordered
> configuration, not an arbitrary list of identifiers. This is why the
> `(namespace, index)` ordering in [The Scheduler](../model/scheduler.md) depends
> on declaration order.

> **Gotcha: a unit-struct `ScheduleSet` is always index 0.** Two such markers at
> the same scheduler namespace both land at index 0 and silently collide. Unit
> structs are only safe as single-phase markers; for several phases, use an enum.

## `StageEnum`

`StageEnum` binds an enum to the `[[run]]` TOML stage machine. Every variant must
carry exactly one `#[stage("name")]` attribute naming the matching TOML stage.

```rust,ignore
use grass_derive::StageEnum;

#[derive(Clone, PartialEq, Default, StageEnum)]
enum RunPhase {
    #[default]
    #[stage("settle")]
    Settle,
    #[stage("compress")]
    Compress,
}
```

```toml
[[run]]
stages = [
  { name = "settle",   steps = 50000  },
  { name = "compress", steps = 100000 },
]
```

The derive generates four methods:

- `stage_name(&self) -> &'static str` — this variant's `#[stage("...")]` string.
- `stage_names() -> &'static [&'static str]` — all names, in declaration order.
- `num_stages() -> usize` — variant count.
- `from_index(i: usize) -> Option<Self>` — the variant at position `i`; this is
  what `grass_io`'s `RunPlugin` calls to advance the stage machine.

**Compile-time checks:** a missing `#[stage(...)]`, a malformed one (not a string
literal), or two variants with the same stage string are each a compile error
with an actionable message.

**Runtime check:** when a `StageAdvancePlugin` registers a `StageNames` resource,
`grass_io`'s `RunPlugin` wires a setup system that cross-checks the `[[run]]`
stage count and names against `stage_names()` at startup, panicking on a
mismatch. Without that resource the check is skipped.

> **Invariant: variant position is the stage index.** `from_index` is positional,
> independent of the `#[stage("...")]` *name*. Reordering `StageEnum` variants
> changes which TOML stage index triggers which variant. And because the
> `#[stage("...")]` strings are a TOML contract, renaming one without updating
> the matching `[[run]]` entry (or vice versa) compiles cleanly but panics at
> startup — the only guard is that runtime check.

## `Namespace`

For a unit struct, the derive generates a single associated constant:

```rust,ignore
// from:  #[derive(Namespace)] struct Cfd;
impl grass_multi::Namespace for Cfd {
    const NAME: &'static str = "Cfd";   // always the struct identifier
}
```

It is used to type-key sub-Apps and cross-namespace handles in `grass_multi`
(`add_subapp_typed::<Cfd>`, `MultiRes<T, Cfd>`).

> **Limitation: the name is always the Rust identifier, case-sensitive.** There
> is no attribute to override it. When you need a namespace string that differs
> from the identifier (e.g. lowercase `"cfd"` while the type is `Cfd`), either
> implement `grass_multi::Namespace` by hand, or use the `namespace!` macro from
> `grass_multi`, which lets you name the string explicitly:
>
> ```rust,ignore
> use grass_multi::namespace;
> namespace!(pub Cfd = "cfd");   // struct Cfd; + impl Namespace { NAME = "cfd" }
> ```

## Compile-error reference

| Derive | Rejected input | Result |
|---|---|---|
| `ScheduleSet` | tuple struct, named-field struct, union | compile error |
| `ScheduleSet` | missing `Copy` / `Clone` / `Debug` | trait-bound error from `grass_scheduler` |
| `StageEnum` | anything but an enum | compile error |
| `StageEnum` | a variant missing `#[stage(...)]` | compile error (with example) |
| `StageEnum` | `#[stage(...)]` not a string literal | compile error |
| `StageEnum` | duplicate stage-name strings | compile error |
| `Namespace` | tuple/named struct, enum, union | compile error |

> The `type_ids!` macro used in `Plugin::dependencies` is **not** part of
> `grass_derive`; it lives in `grass_app` and is imported separately as
> `use grass_app::type_ids;` — see
> [App, Plugin, PluginGroup](../model/app-plugin.md#the-validation-model-two-independent-mechanisms).
