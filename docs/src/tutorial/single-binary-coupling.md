# One Binary: Local or MPI from TOML

This lesson takes one coupled model and runs it in two placements:

```text
one process                  two MPI processes
└── role a + role b          ├── rank 0: role a
    LocalTransport           └── rank 1: role b
                                  MPI transport
```

The executable and scientific configuration stay the same. Only the
`[topology]` table and launch size determine placement.

The complete runnable source is
[`examples/oscillator_spmd`](../../../examples/oscillator_spmd/). The example
uses two harmonic oscillators because their complete trajectory can be checked
against an independent recurrence. The runner itself is not oscillator-specific.

This tutorial assumes you have already read
[Coupling Two Solvers](./coupling-two-solvers.md). That lesson explains Apps,
the parent schedule, typed exchange resources, and transports. Here we focus on
the process boundary.

## 1. Keep each role independently runnable

Each role owns one ordinary oscillator App. The oscillator library contains its
state, parameters, and integration step; it does not initialize MPI or decide
where another solver runs.

The coupling application supplies one role function:

```rust,ignore
fn run_role(launch: RoleLaunch) -> (SideResult, Vec<SideResult>) {
    let role = launch.role().to_owned();
    let (source, transport) = launch.into_parts();
    run_side_with_trace(&role, transport, &source)
}
```

`RoleLaunch` contains only process-level decisions already made by GRASS:

- `role()` — which configured solver belongs on this process;
- `peer()` — the other role's configured name;
- `config_source()` — the complete, identical TOML document;
- `into_transport()` / `into_parts()` — the selected local or MPI transport.

The role code does not inspect MPI world rank, choose a topology mode, or find
its peer's rank.

## 2. Define the narrow exchange contract

The solvers exchange only the position needed by the interface spring:

```rust,ignore
pub struct RemotePosition(pub f64);

impl Wire for RemotePosition {
    fn pack(&self) -> Vec<u8> {
        self.0.to_le_bytes().to_vec()
    }

    fn try_unpack(bytes: &[u8]) -> Result<Self, WireUnpackError> {
        // The full example checks for exactly one IEEE-754 f64.
        # todo!()
    }
}
```

Do not serialize the complete solver state merely because it is convenient.
The wire type is a scientific interface: it should contain the smallest stable
quantity the peer actually consumes.

The full checked implementation is in
[`contract.rs`](../../../examples/oscillator_spmd/contract.rs).

## 3. Declare placement in the same input file

The default example configuration begins with:

```toml
[topology]
mode = "auto"

[[topology.role]]
name = "a"
ranks = 1

[[topology.role]]
name = "b"
ranks = 1
```

The remainder of the file contains the physical inputs:

```toml
[case]
steps = 40

[a.oscillator]
x0 = 1.0
dt = 0.0025

[b.oscillator]
x0 = -0.5
dt = 0.0025
```

`mode = "auto"` resolves to local composition at world size one and split
composition when the world size equals the sum of role ranks. It fails on any
other size rather than guessing.

Use `mode = "local"` or `mode = "split"` when a deployment must be pinned.
A local configuration launched under multiple ranks and a split configuration
launched with the wrong rank count are both rejected before solver Apps are
constructed.

## 4. Hand bootstrap ownership to GRASS

The complete process-level entry is:

```rust,ignore
const DEFAULT_CONFIG: &str = include_str!("config.toml");

fn main() {
    let run = CoupledPairRunner::from_cli_or(DEFAULT_CONFIG)
        .and_then(|runner| runner.run(run_role, run_role))
        .unwrap_or_else(|error| panic!("run coupled oscillator: {error}"));

    // Match PairRun only to report or validate returned scientific results.
    # match run { _ => {} }
}
```

The two `run_role` arguments are the registered implementations for the two
roles in topology declaration order. They may be different functions when the
roles use different solver libraries.

`CoupledPairRunner` owns:

1. optional CLI file loading and TOML parsing;
2. topology and role-count validation;
3. cross-rank configuration-identity checking;
4. MPI initialization and role-local communicator splitting;
5. local threads or MPI role selection;
6. peer-rank lookup and transport construction;
7. joining local roles and finalizing MPI.

This is a process runner, not an App plugin. GRASS must know which solver App
belongs on a process before it can construct that App.

## 5. Run the unchanged binary both ways

Build once:

```bash
export BINDGEN_EXTRA_CLANG_ARGS="-I/usr/lib/gcc/x86_64-linux-gnu/16/include"
cargo build --features mpi --example oscillator_spmd
```

Run both roles locally:

```bash
target/debug/examples/oscillator_spmd
```

Run one role per MPI rank:

```bash
mpirun -np 2 target/debug/examples/oscillator_spmd
```

Neither command selects role `a` or `b`. Every process starts the same binary,
reads the same input, and lets the runner resolve its assignment.

The GCC include environment variable is a build workaround for hosts where
bindgen cannot locate Clang's standard headers. It is not part of the GRASS
runtime API and may be unnecessary on your machine.

## 6. Validate trajectories, not startup messages

Successful rank assignment proves only that bootstrap did not deadlock. The
example checks the complete 40-step scientific history:

```bash
examples/oscillator_spmd/run_spmd.sh
```

The harness compares:

- local composition against an independently implemented recurrence;
- MPI composition against that recurrence;
- MPI and local trajectories against each other;
- final-state bit fingerprints on a homogeneous host.

Expected conclusion:

```text
SPMD full trajectory: local_vs_recurrence=0.000e+00 \
split_vs_recurrence=0.000e+00 split_vs_local=0.000e+00
PASS single-binary local and role-split full trajectories agree with the independent recurrence
```

For a real coupled application, replace oscillator positions with the actual
interface history: heat flux and wall temperature, fluid force and particle
motion, or whichever conserved quantities define the coupling. Compare those
histories and conservation residuals, not merely final values.

## Current boundary

`CoupledPairRunner` deliberately matches the capability that is validated now:
two roles with one coupling rank each. The underlying topology runtime can
split multiple ranks per role, but general `N`-to-`M` interface routing still
requires a declared gather/scatter or distributed ownership map. The runner
rejects multi-rank roles instead of silently choosing an invalid mapping.

That fail-closed boundary is part of the lesson: configuration should select
among validated execution models, not imply capabilities the coupling layer
does not yet possess.
