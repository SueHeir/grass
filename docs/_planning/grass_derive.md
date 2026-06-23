# Planning: `grass_derive` documentation

## Purpose

`grass_derive` is a `proc-macro = true` crate that provides three `#[derive(...)]`
macros for the GRASS framework. It has no runtime logic — every byte it emits is
expanded at compile time into trait impls for traits defined in `grass_scheduler`
and `grass_multi`. Users add it to get boilerplate-free wiring between their Rust
types and the framework's scheduler and multi-physics coupling layers.

---

## Public surface to document

### `#[derive(ScheduleSet)]`

- **Trait implemented:** `grass_scheduler::ScheduleSet`
  (lib.rs:147, 237–251, 256–262)
- **Accepted inputs:**
  - Enums: each variant gets `to_index() -> u32` = declaration position (0, 1, 2, …)
    and `name() -> &'static str` = the variant identifier as a string.
  - Unit structs (`struct Foo;`): yields `to_index() = 0`, `name() = "Foo"`.
    Useful as type-level single-phase markers.
- **Rejected inputs:** tuple structs, named-field structs (compile error), unions
  (compile error). (lib.rs:206–218)
- **No attributes accepted** — the entire contract lives in variant declaration order.
- **Generated methods:** `to_index(&self) -> u32`, `name(&self) -> &'static str`.
- **Companion derives required by the supertrait:** `Clone`, `Copy`, `Debug`.
  The macro does NOT inject these bounds. (lib.rs:30–33)

### `#[derive(StageEnum)]`

- **Trait implemented:** `grass_scheduler::StageName`
  (lib.rs:146–172, grass_scheduler/src/lib.rs:1393–1401)
- **Accepted inputs:** enums only. (lib.rs:76–85)
- **Per-variant attribute:** `#[stage("name")]` — a string literal naming the
  `[[run]]` TOML stage. Every variant must carry exactly one such attribute.
  (lib.rs:94–119)
- **Generated methods:**
  - `stage_name(&self) -> &'static str` — variant → string.
  - `stage_names() -> &'static [&'static str]` — all names in declaration order.
  - `num_stages() -> usize` — count of variants.
  - `from_index(i: usize) -> Option<Self>` — positional index → variant, used
    by `grass_io::run` to advance the stage machine. (lib.rs:162–168)
- **Compile-time invariants enforced:**
  - Missing `#[stage(...)]` on any variant → compile error with example fix
    message. (lib.rs:95–106)
  - Malformed `#[stage(...)]` (not a string literal) → compile error. (lib.rs:108–118)
  - Duplicate stage name strings → compile error. (lib.rs:126–140)
- **Runtime validation:** `grass_io::run` registers a setup system that cross-checks
  TOML stage count and names against `StageName::stage_names()` at startup,
  panicking with an actionable message on mismatch.
  (grass_io/src/run.rs:414–450)
- **Companion derives typically needed:** `Clone`, `PartialEq`, `Default` (the
  `[[run]]` driver uses them). The macro does not inject these.

### `#[derive(Namespace)]`

- **Trait implemented:** `grass_multi::Namespace`
  (lib.rs:287–313)
- **Accepted inputs:** unit structs only. (lib.rs:293–308)
- **No attributes accepted** — the namespace string is always the struct identifier.
- **Generated constant:** `const NAME: &'static str = "StructIdent";`
- **When to use by hand instead:** if you need a namespace string that differs from
  the Rust identifier, implement `grass_multi::Namespace` manually or use the
  `namespace!` macro from `grass_multi`. (lib.rs:276–281)
- **Rejected inputs:** tuple/named structs, enums, unions → compile error.

---

## Config / TOML schema

`grass_derive` itself has no TOML schema. However `#[derive(StageEnum)]` creates
the **Rust side** of a contract whose **TOML side** lives in `[[run]]` blocks:

```toml
[[run]]
stages = [
  { name = "settle", steps = 50000 },
  { name = "compress", steps = 100000 },
]
```

The `name` fields must exactly match the `#[stage("...")]` literals on the enum.
The count must match `num_stages()`. Mismatch is caught at app startup (not
compile time) by `grass_io`'s validation system.

---

## Key behaviors, invariants, and gotchas

1. **Variant order is load-bearing for `ScheduleSet`.**
   `to_index()` is the variant's 0-based position in the enum body. Reordering
   variants silently reorders the execution schedule. This is not a bug; it is
   the intended API — but it means `ScheduleSet` enum bodies should be treated
   like ordered configuration, not arbitrary Rust identifiers.
   (lib.rs:228–232)

2. **`#[stage("...")]` strings are a TOML contract.**
   Renaming a `StageEnum` variant's `#[stage(...)]` literal without updating the
   matching `[[run]]` TOML entry (or vice versa) compiles cleanly but panics at
   startup. The only guard is the runtime check in `grass_io::run`.
   (grass_io/src/run.rs:420–450)

3. **Trait paths are literal, not re-exported.**
   The generated code spells out `grass_scheduler::ScheduleSet`,
   `grass_scheduler::StageName`, `grass_multi::Namespace`. The consuming crate
   must have `grass_scheduler` (or `grass_multi`) in its own `[dependencies]`,
   not just pulled in transitively in a way the compiler might not resolve to the
   right path. (lib.rs:24–27)

4. **`ScheduleSet` does not supply supertrait bounds.**
   The `ScheduleSet` trait requires `Copy + Clone + Debug + 'static`. If a user
   writes `#[derive(ScheduleSet)]` without also deriving `Clone`, `Copy`, `Debug`,
   they get a trait-bound compile error from `grass_scheduler`, not from
   `grass_derive`. This is a common first-use friction point.
   (lib.rs:29–33, README.md)

5. **`ScheduleSet` on a unit struct → always index 0.**
   A unit struct's `to_index()` is hardcoded to `0`. This is only correct when
   the struct is used as a single-phase marker; if you have two such structs at
   the same scheduler namespace they will silently collide at index 0.
   (lib.rs:254–263)

6. **`Namespace` name = Rust identifier, no override.**
   There is no attribute to change the namespace string; the derive always uses
   `name.to_string()` on the struct ident. Case-sensitive. To get a different
   string, use `namespace!` from `grass_multi` or implement the trait by hand.
   (lib.rs:295–298)

7. **`from_index` on `StageEnum` is positional, not by name.**
   The `RunPlugin`'s stage advance calls `S::from_index(idx)`, matching the
   variant at position `idx` in the enum body. This means the position of a
   `StageEnum` variant determines which TOML stage index triggers it, independent
   of the `#[stage("...")]` name. Reordering StageEnum variants changes which
   stage fires when.
   (lib.rs:143–145, grass_scheduler/src/lib.rs:1591)

---

## Tutorial outline: adding a `ScheduleSet` and `StageEnum`

A step-by-step tutorial for the reference chapter (or tutorial/ chapter) should cover:

1. **Add dependencies** — `grass_derive`, `grass_scheduler` (and `grass_multi`
   if using `Namespace`) to `Cargo.toml`.

2. **Define a `ScheduleSet` enum** — show the required companion derives, explain
   that declaration order = run order.

   ```rust
   use grass_derive::ScheduleSet;

   #[derive(Clone, Copy, Debug, ScheduleSet)]
   enum MyPhase {
       Setup,         // index 0 — runs first
       ComputeForces, // index 1
       Integrate,     // index 2
   }
   ```

3. **Register systems** — `app.add_update_system(my_system, MyPhase::ComputeForces)`.

4. **Multi-stage runs with `StageEnum`** — show the `#[stage("...")]` attributes
   and the matching `[[run]]` TOML, emphasizing the exact-string contract.

   ```rust
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

5. **Namespace for multi-physics coupling** — show `#[derive(Namespace)]` on a
   unit struct and its use in `add_subapp`.

6. **Common mistakes** — missing `Copy`/`Clone`/`Debug` on `ScheduleSet` type;
   mismatched `#[stage("...")]` vs TOML name; reordering variants after wiring.

---

## Doc gaps

The existing `docs/src/reference/derives.md` is a solid stub. Gaps:

- No code example for `StageEnum` showing both the Rust and TOML sides side-by-side.
- No code example for `Namespace` showing `add_subapp` wiring.
- The "unit struct at index 0 collision" gotcha (item 5 above) is not mentioned anywhere.
- The positional `from_index` behavior of `StageEnum` (item 7 above) is undocumented.
- No mention of what happens if you forget `Copy`/`Clone`/`Debug` — just refers
  to the scheduler page, which doesn't explain the error you see.
- The `namespace!` macro escape hatch (for custom strings) is mentioned in the
  README but not in the mdBook reference page.
- No cross-link from the tutorial (`write-your-own-solver.md`) back to the
  reference derive page.

---

## Suggested placement

The existing `docs/src/reference/derives.md` should be expanded (not replaced)
into a full reference chapter. Suggested structure:

```
reference/derives.md
  ## ScheduleSet
     - Attributes: none
     - What is generated (with expanded pseudo-code)
     - Supertrait requirement (Clone/Copy/Debug)
     - Enum vs unit struct
     - Invariant: declaration order = index
  ## StageEnum
     - Attributes: #[stage("name")]
     - What is generated
     - The TOML contract
     - Compile errors
     - Runtime validation in grass_io
     - Invariant: position = stage index
  ## Namespace
     - What is generated
     - Limitation: name = Rust ident
     - Escape hatch: namespace! macro
  ## Compile errors reference table
     (each rejected input → what you see)
```

The tutorial step in `tutorial/write-your-own-solver.md` should gain a
cross-reference to this page after the `ScheduleSet` code block.
