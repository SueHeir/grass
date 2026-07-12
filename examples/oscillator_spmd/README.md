# oscillator_spmd — one binary, topology-driven composition

A single executable composes the *same* two coupled harmonic oscillators two
ways, and **only the declarative `[topology]` TOML decides which**:

| `mode`            | Launch                         | Composition                                   |
| ----------------- | ------------------------------ | --------------------------------------------- |
| `local`           | `oscillator_spmd local.toml`   | both roles in one process over `LocalTransport` |
| `split`           | `mpirun -np 2 … split.toml`    | one role per rank; `MPI_COMM_WORLD` split       |
| `auto` *(default)*| either of the above            | world size 1 → local, world size 2 → split      |

The role function and physics tables are byte-identical across both runs.
`grass_multi::CoupledPairRunner` owns configuration loading, topology
resolution, local threading, MPI role selection, peer-rank lookup, transport
construction, and MPI finalization. The example only supplies the role code and
checks its results.

The entry point is intentionally small:

```rust,ignore
let run = CoupledPairRunner::from_cli_or(DEFAULT_CONFIG)?
    .run(run_role_a, run_role_b)?;
```

This runner is process-level rather than an `App` plugin because GRASS must
select which solver App belongs on the process before constructing that App.

## Run it

```bash
# Serial local composition (auto resolves to local at world size 1):
cargo run --features mpi --example oscillator_spmd

# MPI role split (auto resolves to split at world size 2):
mpirun -np 2 target/debug/examples/oscillator_spmd

# Full check: both paths + fail-closed guardrails + trajectory parity:
examples/oscillator_spmd/run_spmd.sh
```

`run_spmd.sh` replays a completely independent Python recurrence and asserts the
*whole* 40-step trajectory — not just the final state — agrees for the local
run, the MPI run, and the two against each other. On a homogeneous host the
final states are also bit-identical (`fingerprint_match=True`).

## How the split addresses its peer

The `[topology]` roles are laid out contiguously in declaration order, so role
`a` owns raw-world rank 0 and role `b` owns rank 1. The runner resolves both the
local role and peer rank and supplies the selected transport to the role code.
No application-level bootstrap or out-of-band rank map is needed.

Before the split, every rank cross-checks that it parsed an identical
configuration (`all_uniform_u64` over a `config_digest`); a disagreement is
rejected rather than silently coupling mismatched physics.

## Fail-closed validation

The declarative mode is enforced, not advisory:

- `split.toml` run serially → rejected (world size 1 ≠ configured 2).
- `local.toml` under `mpirun -np 2` → rejected (local requires a single rank).
- `mode = "auto"` at any world size other than 1 or the role total → rejected.

## Limitations (current)

- **One coupling rank per role.** Each role is one rank, and coupling is a
  single point-to-point pair (role `a` rank 0 ↔ role `b` rank 0). Multi-rank
  roles split correctly into role-local communicators, but the cross-role
  coupling transport here still assumes a single peer rank; a multi-rank
  coupling map is future work.
- **Two roles.** The physics and the recurrence checker are a two-body pair.
  The topology layer itself is N-role; the example is deliberately the minimal
  coupled pair.
- **Homogeneous-path bit parity.** `fingerprint_match` is expected on a single
  homogeneous host; heterogeneous floating-point paths may differ in the last
  bits while still meeting the 5e-14 trajectory tolerance.

## Build note (this host)

Building the `mpi` feature runs `bindgen` over OpenMPI's headers. On a host
without clang's builtin headers on the default search path, point bindgen at a
GCC include dir, e.g.:

```bash
export BINDGEN_EXTRA_CLANG_ARGS="-I/usr/lib/gcc/x86_64-linux-gnu/16/include"
```

OpenMPI's TCP BTL may print `Unable to find reachable pairing` warnings on hosts
with only loopback reachable; they are harmless. Silence them with
`export OMPI_MCA_btl=self,vader`.
