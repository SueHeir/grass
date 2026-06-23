# Planning: `grass_mpi` documentation

## Purpose

`grass_mpi` is the thin MPI abstraction layer in GRASS. It decouples
all domain and solver crates from direct `rsmpi` usage by supplying:

- a `CommBackend` trait (the only interface callers touch),
- a `CommResource` newtype (`Box<dyn CommBackend>`) that lives in the
  scheduler's resource table as `Res<CommResource>`,
- two concrete backends: `SingleProcessComm` (serial, no-op) and
  `MpiCommBackend` (real MPI, feature-gated), and
- a handful of free functions for MPI lifecycle and MPMD bootstrap.

Its primary downstream consumer is `grass_multi::MpiInterCommTransport`.

---

## Public surface to document

### Trait

| Item | File:line | Notes |
|---|---|---|
| `CommBackend` | `src/lib.rs:113` | Sealed by `Send + Sync + 'static` bound. All collective and P2P ops live here. |

Key methods on `CommBackend`:

| Method | Line | Signature |
|---|---|---|
| `rank` | 115 | `fn rank(&self) -> i32` |
| `size` | 117 | `fn size(&self) -> i32` |
| `processor_decomposition` | 119 | `fn processor_decomposition(&self) -> [i32; 3]` |
| `processor_position` | 121 | `fn processor_position(&self) -> [i32; 3]` |
| `set_processor_grid` | 123 | `fn set_processor_grid(&mut self, decomp: [i32; 3], position: [i32; 3])` |
| `all_reduce_sum_f64` | 125 | allreduce SUM over `f64` scalars |
| `all_reduce_min_f64` | 127 | allreduce MIN — primary use: global adaptive dt |
| `barrier` | 129 | blocking global barrier |
| `send_f64` | 133 | blocking point-to-point send; `unreachable!` on serial |
| `recv_f64` | 135 | blocking recv, allocates; `unreachable!` on serial |
| `recv_f64_any` | 137 | blocking recv from any rank; `unreachable!` on serial |
| `sendrecv_f64` | 140 | deadlock-free sendrecv, probes length, allocates |
| `sendrecv_f64_into` | 146 | probe-free sendrecv into caller buffer (caller must know recv length) |
| `sendrecv_batch_f64_into` | 156 | batched non-blocking sendrecv (Isend/Irecv posted up-front) |

### Structs

| Item | File:line | Notes |
|---|---|---|
| `CommResource` | `src/lib.rs:162` | `Box<dyn CommBackend>`; `Deref`/`DerefMut` to `dyn CommBackend` |
| `SingleProcessComm` | `src/lib.rs:180` | Always available. rank=0, size=1. |
| `MpiCommBackend` | `src/lib.rs:384` | Behind `mpi_backend` feature. |
| `SendRecvOp<'a>` | `src/lib.rs:91` | One element in a batched sendrecv: `dest`, `send_buf`, `source`, `recv_buf`; `-1` disables that half. |

### Free functions (all `#[cfg(feature = "mpi_backend")]` unless noted)

| Function | Line | Notes |
|---|---|---|
| `get_mpi_world()` | `src/lib.rs:286` | Returns intra-comm (color-split) if `init_app_color` ran, else raw WORLD. |
| `get_mpi_world_raw()` | `src/lib.rs:347` | Always raw `MPI_COMM_WORLD`. |
| `init_app_color(color: i32)` | `src/lib.rs:315` | MPMD bootstrap: splits WORLD by color. Call once, before `get_mpi_world`. |
| `finalize_mpi()` | `src/lib.rs:338` (mpi), `src/lib.rs:381` (no-op) | Drops Universe → `MPI_Finalize`. No-op stub always compiled in. |
| `world_rank()` | `src/lib.rs:359` | Raw WORLD rank. |
| `world_size()` | `src/lib.rs:371` | Raw WORLD size. |

### Feature flags

| Feature | Default | Effect |
|---|---|---|
| `mpi_backend` | **on** | Pulls in `mpi = "0.8.0"` + `libffi-sys = "2.3.0"` (system libffi); enables `MpiCommBackend`, all `get_mpi_world*`, `init_app_color`, `world_rank/size`, and the real `finalize_mpi`. Without it, only `SingleProcessComm` + the no-op `finalize_mpi` stub compile. |

`Cargo.toml:13`: `default = ["mpi_backend"]` — downstream crates that want
serial-only must opt out with `default-features = false`.

---

## Config / TOML schema

There is no TOML schema inside `grass_mpi` itself. The processor grid
(`processor_decomposition`, `processor_position`) is set at runtime by the
downstream solver via `CommBackend::set_processor_grid` (`src/lib.rs:123`).
How/when that is called and what grid values mean is entirely up to the caller;
`grass_mpi` stores and returns the values verbatim.

---

## Key behaviors, invariants, and gotchas

### 1. Init/finalize ordering (`src/lib.rs:41–58`)

- `init_app_color(color)` must be called **before** the first `get_mpi_world()`.
  Calling it after a `CommResource` has captured the world is silently too late
  — the backend already holds raw WORLD.
- `finalize_mpi()` must be called **after every `CommResource` has dropped**.
  It sets the `Mutex<Option<Universe>>` to `None`, which drops the `Universe`
  and triggers `MPI_Finalize`. Any MPI call after this is undefined behavior.
- The no-op `finalize_mpi` stub (`src/lib.rs:381`) compiles in even without
  the feature, so callers can always call it unconditionally.

### 2. Serial-fallback contract (`src/lib.rs:100–112`)

`SingleProcessComm` is **not** a stub — the unreachable panics in `send_f64`,
`recv_f64`, `recv_f64_any`, `sendrecv_f64`, and `sendrecv_f64_into` are
deliberate. Callers that use `Res<CommResource>` must take the
`to_proc == rank` local-copy path for every neighbor communication and never
reach those methods. The one exception is `sendrecv_batch_f64_into`
(`src/lib.rs:244`): the serial backend services it by copying each op's
`send_buf` into its `recv_buf` (periodic self-exchange).

### 3. Two communicator views (MPMD vs. SPMD)

- `get_mpi_world` → intra-comm (this binary's own ranks). Use for backends.
- `get_mpi_world_raw` / `world_rank` / `world_size` → raw `MPI_COMM_WORLD`.
  Use for MPMD couplers that must address peers in the other binary by
  absolute world rank (see `grass_multi::MpiInterCommTransport`, README:34).

### 4. `unsafe impl Send/Sync` promise (`src/lib.rs:60–67`, `src/lib.rs:393–395`)

`MpiCommBackend` and `IntraComm` carry hand-written `unsafe impl Send/Sync`
so they can live in a `Mutex<Option<..>>` static and the scheduler resource
table. **All MPI calls must happen on a single thread per rank.** Sharing a
`CommResource` across OS threads is unsound even though it compiles.

### 5. Batched sendrecv overlap (`src/lib.rs:487–520`)

`sendrecv_batch_f64_into` posts all `Irecv`s before all `Isends`, then waits
on all with `wait_all`. Receives first → no unexpected-message buffering.
All ops in a batch must be mutually independent (disjoint `recv_buf`s; no
send may depend on another op's receive completing). The caller is responsible
for that invariant.

### 6. `sendrecv_f64` vs `sendrecv_f64_into` allocation tradeoff (`src/lib.rs:464–485`)

- `sendrecv_f64`: probes recv length via `MPI_Probe`, allocates a `Vec`.
  Convenient for setup/teardown; expensive in the hot path.
- `sendrecv_f64_into`: probe-free, caller provides correctly-sized `recv_buf`.
  Used by per-step ghost forward/reverse comm where `SwapData` already records
  the recv count. Prefer this in the simulation loop.

---

## Tutorial outline

A short tutorial section inside the "MPI and Coupling" chapter should cover:

1. **Serial run (default)**: add `SingleProcessComm` as a `CommResource` and
   show that collectives work unchanged. No `#[cfg]` in caller code required.
2. **Parallel run (SPMD)**: cargo feature flag, `get_mpi_world()`,
   `MpiCommBackend::new()`, `scheduler.add_resource(comm)`, `finalize_mpi()`.
3. **MPMD run**: when and why to call `init_app_color`; two-binary
   `mpirun -np N1 ./a : -np N2 ./b`; using `world_rank` vs `rank` to address
   the remote binary.
4. **Using collectives**: `all_reduce_sum_f64` (aggregate diagnostics),
   `all_reduce_min_f64` (global dt), `barrier` (sync before I/O).
5. **Ghost exchange pattern**: distinguish `sendrecv_f64_into` (single pair,
   known size) from `sendrecv_batch_f64_into` (multiple pairs in-flight at
   once) with the `SendRecvOp` type.
6. **Safety note**: single-threaded MPI per rank; don't share `CommResource`
   across OS threads.

---

## Doc gaps

- **No doc-test coverage for `MpiCommBackend`**: all trait-impl tests
  (`src/lib.rs:526–551`) exercise only `SingleProcessComm`. The MPI path has
  zero automated test coverage inside this crate.
- **`processor_decomposition` / `processor_position` semantics are undocumented
  at the trait level**: the trait (`src/lib.rs:119–123`) says "Cartesian
  process-grid dimensions `[nx, ny, nz]`" but nothing explains how indices
  map to spatial directions, or who is responsible for calling
  `set_processor_grid`. The downstream solver that calls it holds that
  knowledge; the crate docs should at minimum point there.
- **`SendRecvOp::dest = -1` / `source = -1` semantics** are documented only in
  the struct-level doc (`src/lib.rs:84`) and the batch-fn doc (`src/lib.rs:149`).
  A shared "disabled half" note at the `SendRecvOp` struct itself would reduce
  repeated reading.
- **`init_app_color` idempotence caveat** (`src/lib.rs:312`: "Idempotent if
  called twice with the same color"): the implementation does *not* check
  whether the stored intra-comm used the same color — it unconditionally
  overwrites `MPI_INTRA` (`src/lib.rs:331`). The doc comment overstates the
  safety; a second call with a different color silently replaces the first.
- **No `CommPlugin`**: the module doc (`src/lib.rs:13`) notes there is no
  plugin, but there is no explanation of *why* (the wiring is intentionally
  left to the consumer so different App setups can choose backends). A short
  rationale note would prevent "why is there no plugin?" questions.
- **`finalize_mpi` drop-ordering guarantee**: the doc says "after every
  `CommResource` has dropped" but nothing enforces this at the type level.
  A note that Rust's drop order for `App` fields drives this (or that the
  caller must ensure it) would prevent subtle use-after-finalize bugs.

---

## Suggested placement

All `grass_mpi` content belongs in the existing
**`docs/src/model/mpi-coupling.md`** chapter (already listed in `SUMMARY.md`
under "MPI and Coupling"). The existing page already contains the lifecycle,
wiring code, and `unsafe` note drawn verbatim from `src/lib.rs:1–67`. The
additions needed are:

- Expand the `CommBackend` method table (currently absent from the page).
- Add a `SendRecvOp` / batch-sendrecv subsection.
- Add the tutorial sequence above as a step-by-step "Wiring walkthrough"
  subsection.
- Document the two communicator views (`get_mpi_world` vs `get_mpi_world_raw`)
  more explicitly — currently only mentioned in the lifecycle numbered list.
- Note the doc gaps above as known omissions until resolved.

The `reference/crates.md` entry (`docs/src/reference/crates.md`) already has
a one-liner for `grass_mpi`; no change needed there.
