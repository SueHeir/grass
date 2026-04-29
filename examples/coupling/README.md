# Coupling examples

Five worked examples that build up coupled-oscillator simulation in the
GRASS stack one concept at a time. Each example is a single `main`
file; together they tell one story:

> Same physics, same coupling function. Only the **schedule** changes.

The coupling function is [`oscillator_demo::exchange_positions`](../../crates/oscillator_demo/src/lib.rs#L50-L58):

```rust
pub fn exchange_positions(
    a_state: MultiRes<OscState, A>,
    b_state: MultiRes<OscState, B>,
    mut a_other: MultiResMut<OtherX, A>,
    mut b_other: MultiResMut<OtherX, B>,
) {
    a_other.0 = b_state.x;
    b_other.0 = a_state.x;
}
```

That function is used **verbatim** by the explicit, implicit, adaptive,
and MPI examples below. Each example builds a different `Schedule` —
single-pass, fixed-point loop, nested retry loops, or cross-process
pump — around it.

## The scenario

Two damped harmonic oscillators connected by an interface spring of
stiffness `κ`. Each oscillator owns its position `x`, velocity `v`, and
self-spring `k_self`; each integrates with semi-implicit Euler at its
own dt. The coupling boundary is `OtherX` — what THIS oscillator
believes the OTHER's position to be right now. The job of the coupling
layer is to keep `OtherX` consistent with the peer's actual
`OscState.x`.

The shared physics — `OscillatorPlugin`, integrator, resources, final-
state extractor — lives in
[`crates/oscillator_demo/src/lib.rs`](../../crates/oscillator_demo/src/lib.rs).
Each example contains a small `main.rs` (the wiring: which sub-Apps to
register, what schedule to run, where to slot the coupler) plus a
`main.toml` (every numeric parameter — material, initial state, total
steps).

Every example accepts `--generate-config` to dump a starter TOML built
from each registered plugin's defaults.

## 1. [`single_oscillator/`](single_oscillator/main.rs) — the baseline

One oscillator, plain `grass_app::App`, no coupling. Three plugins do
the work:
[`OscillatorPlugin`](../../crates/oscillator_demo/src/lib.rs) (reads
`[oscillator]` from TOML, registers the integrator) and
[`RunPlugin`](../../crates/grass_io/src/run.rs) (reads `[run] steps`,
auto-installs `SimClockPlugin`, auto-registers `advance_step`, and
stops the App when the step count hits the configured target).

```rust
let mut app = App::new();
app.add_plugins(InputPlugin);     // reads main.toml at args[1]
app.add_plugins(OscillatorPlugin);
app.add_plugins(RunPlugin);
app.start();
```

Every coupled example below pairs two of these single-oscillator Apps
under one parent App and adds a coupling layer that keeps `OtherX` in
sync between the sides.

## 2. [`explicit/`](explicit/main.rs) — explicit / CSS coupling

Two oscillators, the simplest possible coupling: tick A, tick B, swap
positions, repeat. Standard "conventional sequential staggered" —
explicit, first-order in coupling time.

The schedule is a flat enum:

```rust
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum OuterStep { TickA, TickB, Exchange }
```

The parent App registers two sub-Apps via `add_subapp_with_config` (a
`grass_io` helper that pulls each sub-App's `[a.*]` / `[b.*]` slice
from the parent's `Config`, seeds it as the sub-App's local `Config`,
and runs the closure to add plugins). Then it slots one system per
phase. Termination comes from `RunPlugin` reading `[run] steps`:

```rust
parent.add_subapp_with_config(A::NAME, |app| { app.add_plugins(OscillatorPlugin); });
parent.add_subapp_with_config(B::NAME, |app| { app.add_plugins(OscillatorPlugin); });
parent.add_plugins(RunPlugin);

parent.add_update_system(tick_subapp(A::NAME, 1), OuterStep::TickA);
parent.add_update_system(tick_subapp(B::NAME, 1), OuterStep::TickB);
parent.add_update_system(exchange_positions, OuterStep::Exchange);
```

This is the whole pattern: **sub-Apps are registered like resources;
ticks and couplers are registered like systems; the schedule is an
enum**. Cross-namespace reads/writes inside `exchange_positions` go
through `MultiRes<T, NS>` / `MultiResMut<T, NS>` — typed exactly like
`Res<T>` / `ResMut<T>`, but with a namespace marker telling the
SystemParam machinery which sub-App's resource store to look in.

Each side reads `OtherX` (its peer's previous-iter position) when it
ticks, so the coupling lags by one outer iter. At low `κ` that's
invisible; at high `κ` it makes the interface oscillate. The next
example fixes that.

## 3. [`implicit/`](implicit/main.rs) — implicit Picard coupling

The fix for high-`κ` lag is to **iterate** the coupling exchange until
each side's view of the other agrees with reality:

> Save state → loop { restore, tick A, tick B, compute residual,
> exchange } until residual < tol → check done.

The schedule wraps the body of one outer iter in `Schedule::Loop`:

```rust
let schedule = Schedule::builder()
    .then::<Stage>()
    .loop_until(
        picard_converged,
        implicit.max_inner_iters as usize,
        OnMax::AcceptUnconverged,
        |body| body.then::<BodyStep>(),
    )
    .then::<RunSchedule>()
    .build();
```

Each Picard iter restarts from the saved pre-loop state, so only the
boundary guess (`OtherX`) carries over between attempts. The residual
is L¹ disagreement between `OscState.x` on each side and what the OTHER
side currently believes that position to be — drops to zero when the
coupling is consistent. `tol` and `max_inner_iters` come from
`[implicit]` in `main.toml`.

The coupling function used in the loop body is **the same**
`exchange_positions` from before, slotted into `BodyStep::UpdateOtherX`:

```rust
parent.add_update_system(exchange_positions, BodyStep::UpdateOtherX);
```

## 4. [`adaptive/`](adaptive/main.rs) — adaptive dt + implicit Picard

What if Picard doesn't converge in the inner budget? The textbook
answer is to halve dt and try again. That gives a **nested** schedule:

> Save state → loop {
>   loop { restore, apply dt, tick A, tick B, residual, exchange } until converged,
>   if not converged: halve dt
> } until converged → grow dt → check done.

A nested `loop_until` expresses this directly:

```rust
let schedule = Schedule::builder()
    .then_variant(Stage::Save)
    .loop_until(
        picard_converged,
        adaptive.max_outer_retries as usize,
        OnMax::AcceptUnconverged,
        |outer_body| {
            outer_body
                .loop_until(
                    picard_converged,
                    adaptive.max_inner_iters as usize,
                    OnMax::AcceptUnconverged,
                    |inner_body| inner_body.then::<BodyStep>(),
                )
                .then_variant(Stage::Halve)
        },
    )
    .then_variant(Stage::Grow)
    .then::<RunSchedule>()
    .build();
```

`Stage::Halve` is the only system that runs *between* the inner
loop ending and the outer loop deciding to retry. It's gated with
`.run_if(dt_should_shrink)` so it only fires when Picard didn't
converge:

```rust
parent.add_update_system(halve_dt.run_if(dt_should_shrink), Stage::Halve);
```

Cross-namespace mutation of sub-App state from a parent system shows up
as
[`apply_dt` at adaptive/main.rs:92-99](adaptive/main.rs#L92-L99) — it
writes the parent's adaptive dt into each sub-App's `StepSize` resource:

```rust
fn apply_dt(
    mut a: MultiResMut<StepSize, A>,
    mut b: MultiResMut<StepSize, B>,
    parent_dt: Res<ParentDt>,
) {
    a.dt = parent_dt.0;
    b.dt = parent_dt.0;
}
```

> **Note on this scenario:** the canonical seeds in
> `examples/coupling/adaptive/main.toml` let Picard converge easily,
> so the dt-halve branch never fires and `adaptive` produces bit-
> identical results to `implicit`. The schedule is still exercised —
> the inner loop runs, the outer loop runs once and exits on
> convergence. To see the halve branch fire, push `κ` higher or
> tighten `tol`.

## 5. [`explicit_mpi/`](explicit_mpi/) — explicit coupling across two binaries

Same explicit/CSS algorithm, same `exchange_positions` function, but
the two oscillators live in separate processes talking over MPI.

Both binaries load the same `examples/coupling/explicit_mpi/main.toml`.
This binary owns oscillator A as a real local sub-App; the peer binary
owns oscillator B. Each side keeps a **remote mirror** of the peer's
`OscState`, registered with `add_remote_subapp`:

```rust
parent.add_subapp_with_config(A::NAME, |app| { app.add_plugins(OscillatorPlugin); });

let transport = MpiInterCommTransport::new(/* peer = */ 1);
parent
    .add_remote_subapp(B::NAME, transport)
    .send_each_iter::<OscState>()      // A.x → B
    .recv_each_iter::<OscState>()      // B.x → mirror
    .with_resource::<OtherX>();        // scratch slot for exchange_positions
```

The mirror is a `Physics` impl just like a local sub-App, so
`tick_subapp(B::NAME, 1)` works on it: when the parent schedule fires
the mirror's tick, it sends what's in its `OscState` slot and recvs
what the peer wrote into its `OscState` slot.

The schedule needs an extra phase compared to the single-binary
version:

```rust
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum OuterStep { TickA, ExportLocalA, TickMirrorB, Exchange }
```

`ExportLocalA` is the copy-into-the-wire-slot step:

```rust
fn export_local_a(a: MultiRes<OscState, A>, mut b_mirror: MultiResMut<OscState, B>) {
    *b_mirror = *a;
}
```

It runs *between* the local tick and the mirror tick so the wire ships
A's freshly-advanced state instead of the previous-iter recv echoing
back. The peer binary
[`b.rs`](explicit_mpi/b.rs) is the symmetric mirror image:
local B + remote A, with `export_local_b` swapping the namespaces.

The `Exchange` phase runs the same `exchange_positions` function used
by every other example. `MultiRes` / `MultiResMut` see local sub-Apps
and remote mirrors as the same kind of namespace, so the coupling
function is location-transparent — it doesn't know whether it's reading
a real sub-App or a wire-recv buffer.

## Running the examples

Each example takes a TOML config path on the command line:

```sh
cargo run --release --example single_oscillator -- examples/coupling/single_oscillator/main.toml
cargo run --release --example explicit          -- examples/coupling/explicit/main.toml
cargo run --release --example implicit          -- examples/coupling/implicit/main.toml
cargo run --release --example adaptive          -- examples/coupling/adaptive/main.toml
```

Or generate a starter config from each example's plugin defaults:

```sh
cargo run --example explicit -- --generate-config
```

MPI two-binary example (requires the `mpi` feature and a working MPI
launcher):

```sh
cargo build --features mpi --example explicit_mpi_a --example explicit_mpi_b
mpirun -np 1 ./target/debug/examples/explicit_mpi_a examples/coupling/explicit_mpi/main.toml \
     : -np 1 ./target/debug/examples/explicit_mpi_b examples/coupling/explicit_mpi/main.toml
```

Each coupled example prints a final state in two forms:

```
explicit:  x_a = -0.341684381   v_a = +3.446731088   x_b = +0.656594391   v_b = -4.155194777
RESULT_BITS explicit x_a=0xbfd5de282ad7bf20 v_a=0x400b92e7bfac4d3f x_b=0x3fe502d23d82336e v_b=0xc0109eeb612a3643
```

The `RESULT_BITS` line is the bit-exact `f64::to_bits` of each
component — useful when you want to check that a refactor didn't
perturb the trajectory.

## What's reused vs what's per-example

| component | source | used by |
|---|---|---|
| Physics + integrator | [`OscillatorPlugin`](../../crates/oscillator_demo/src/lib.rs) | every example |
| Coupling function | [`exchange_positions`](../../crates/oscillator_demo/src/lib.rs) | every coupled example |
| Final-state extractor | [`extract_final_state`](../../crates/oscillator_demo/src/lib.rs) | every coupled example |
| TOML loader | [`InputPlugin`](../../crates/grass_io/src/config.rs) | every example |
| Step/time + termination | [`RunPlugin`](../../crates/grass_io/src/run.rs) (auto-installs `SimClockPlugin`, auto-registers `advance_step`) | every example |
| Per-sub-App config seeding | [`MultiIoExt::add_subapp_with_config`](../../crates/grass_io/src/config.rs) (uses `Config::for_subapp` internally) | every coupled example |
| Schedule + phase enums + main.toml | inline in each example | per-example |

The shared infrastructure handles physics + observability + termination;
each example's `main.rs` is just the schedule, and `main.toml` carries
every numeric parameter.

## See also

[`examples/io/`](../io/) — single-oscillator demo wired to the
[`grass_io`](../../crates/grass_io/) plugin trio (config / clock /
term_out / dump) and driven from one `main.toml`. Different focus from
this folder: this tour teaches the *coupling layer*; that one teaches
the *I/O and observability layer*.
