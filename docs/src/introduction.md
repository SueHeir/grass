# Introduction

Most scientific frameworks help build a solver. **GRASS helps build scientific libraries that can coexist.** A solver becomes an `App` built from typed state, scheduled functions, and plugins; an application or coupling owner decides how independently useful libraries meet.

That emphasis starts with an ordinary maintenance problem. A solver commonly accumulates its own state model, timestep driver, I/O, parallel assumptions, and extension mechanism. The resulting executable can be scientifically useful, but its capabilities are entangled with its private structure. A later coupling must either modify both programs or maintain an adapter that knows both private worlds.

GRASS is a library boundary before it is an implementation style. It does not make arbitrary solvers compatible automatically, infer a scientifically valid exchange, or choose coupling cadence, interpolation, convergence, or stop policy. Authors make those choices explicit in a contract.

## Evidence first: two tested oscillators

The smallest runnable demonstration is [`oscillator_demo`](../../examples/oscillator_demo/README.md). It composes two unchanged oscillator libraries through direct typed access and through a stable position port; the validation requires their final fingerprints to agree. The same sweep compares an uncoupled oscillator with `x = cos(t), v = -sin(t)` at `t = 1` and requires maximum component error below `1e-4`.

```bash
source ~/projects/.build-env
cargo run --example oscillator_demo
$BENCH_PYTHON examples/oscillator_demo/sweep.py
```

The [coupled-oscillator walkthrough](./tutorial/coupled-oscillator-walkthrough.md) explains the example from its two library boundaries through its explicit parent schedule.

## The composition contract

```text
solver/library A                         application or coupling owner                 solver/library B
private resources ─────────────────►     conversion, schedule, termination      ─────────────────► private resources
systems and plugins                      direct MultiRes or optional Port<T>                     systems and plugins
```

At the framework level, a library defines typed resources and systems and packages them as plugins; the App holds the resource instances and declares dependencies/capabilities. Systems make their read/write access visible as `Res<T>`/`ResMut<T>`; execution is currently single-threaded and deterministic. A coupling owner owns the parent schedule and stop policy. A pair-specific parent coupling can access both sides directly with `MultiRes`; an optional `Port<T>` is useful when several implementations genuinely share one interface-owned exchange contract. Child systems use ordinary `Res`/`ResMut` and never receive cross-App `MultiRes` access.

Read the precise requirements and their executable checks in the [library composition contract](./reference/library-composition-contract.md). They define how libraries designed for this boundary can compose; they are not a universal adapter for arbitrary software.

## One evidence-backed stack

The present case study is **GRASS → SOIL → DIRT**:

```text
DIRT  DEM laws and method-specific state/evidence
  │
SOIL  particle substrate: AtomData, communication, neighbour lists
  │
GRASS App, plugins, typed scheduler, I/O, MPI, coupling
```

GRASS knows neither particles nor physics. [SOIL](https://github.com/SueHeir/soil) owns reusable particle infrastructure, while [DIRT](https://github.com/SueHeir/dirt) owns DEM-specific state, laws, and its cited validation. DIRT's evidence is not evidence that every particle method, CFD solver, or cross-substrate coupling is validated. The [case study](./stack/grass-soil-dirt-case-study.md) traces the contracts, dependency direction, and what remains future work.

## Start from your role

- **Library author:** [library composition contract](./reference/library-composition-contract.md), then [write your own solver](./tutorial/write-your-own-solver.md).
- **Application author:** [GRASS in 5 minutes](./quickstart.md), then [App, Plugin, PluginGroup](./model/app-plugin.md).
- **Coupling author:** [coupling two solvers](./tutorial/coupling-two-solvers.md), followed by [MPI and coupling](./model/mpi-coupling.md).

The rest of this book describes the `App`, scheduler, I/O, MPI, and coupling tools that implement these boundaries.
