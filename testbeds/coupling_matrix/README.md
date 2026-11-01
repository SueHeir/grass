# Coupling test matrix

A test bed for the GRASS coupling layer. It lives on the `coupling-tests`
branch, not in the library.

The question it answers is whether the MPI and coupling code works for *any*
coupling, not just the two-code demo. To find out, it couples N roles over an
arbitrary graph. Each case is run several ways, and every run has to agree
with a serial reference.

## The problem

The model is a linear ODE system `u' = A u`. Its components are split over
the roles (see `src/model.rs`). Each component has three parts:

* A stiff diagonal decay, treated implicitly.
* Couplings to other components of the same role, taken at the window start.
  To evaluate these, a role spread over several ranks has to share its
  state over its own communicator every window.
* Couplings to components of other roles, only along the edges of the
  coupling graph. These are what the coupling layer moves.

Each role takes its own number of backward-Euler substeps per window, so
subcycling is covered.

The problem is linear, so the serial reference is exact. Explicit schemes
are replayed operation for operation. For implicit schemes, each window's
coupled fixed point is solved directly.

## What varies

| axis | values |
|---|---|
| graph | chain, star, ring, complete |
| roles | 2 to 5, with 1 to 5 components each |
| scheme | explicit Jacobi, explicit Gauss-Seidel, implicit Jacobi, implicit Gauss-Seidel |
| interface owners | implicit Jacobi: every role owns the interface. Implicit GS: the roles that feed a lower role own it (`DistributedImplicit::with_owners`) |
| acceleration | IQN-ILS (reuse 4, filter 1e-10, which is deliberately permissive), Aitken, plain Picard |
| exchange | root-bridge shard exchange (`RoleContext::exchange`), sparse rank-to-rank routing with entity ids (`RoleContext::routed_exchange`) |
| placement | roles as threads (`CoupledRunner`, local mode); roles as sub-Apps of one parent App (`ImplicitCoupling`, `checkpoint_window`, `converge_window`); MPI with several rank layouts per case, up to 8 ranks, including roles with more ranks than components |
| failure | one role panics mid-run, under threads and under MPI |

## What is checked

* **Explicit schemes:** bit-identical to the serial reference, in every placement.
* **Implicit schemes:**
  * Within 1e-9 of the exact per-window fixed point.
  * No window hits the iteration cap.
  * Values and iteration counts are bit-identical across threads, in-process
    and every MPI layout.
* **Failure cases:** every placement exits nonzero with the injected panic,
  and none hangs.

## Running it

    cargo build --release
    python3 run_matrix.py          # 109 cases, about 2.5 minutes on 4 cores
    python3 run_matrix.py --quick  # 70 cases
    python3 run_matrix.py --only 57

You can also run a single case by hand:

    target/release/coupling_matrix case.toml                  # threads
    mpirun -np 5 target/release/coupling_matrix case.toml     # [topology] layout
    target/release/coupling_matrix --inprocess case.toml      # sub-Apps
    target/release/coupling_matrix --reference case.toml      # serial reference
    target/release/coupling_matrix --serial case.toml         # OuterIteration on the serial sweep
    target/release/coupling_matrix --map case.toml            # print each window's interface map

## What it found

The first run had 4 failures out of 70. In all four, IQN-ILS diverged to
1e161 in a late window.

`--serial` showed that MPI was not involved. `--map` and an IQN debug trace
pinned down the cause. When a window converges, its last secant pairs are
differences at the rounding level. The QR filter compared a column only
with its own length, so these noise columns passed. With reuse they were
carried into later windows, where they blew up the least-squares solve.

The fix is in `grass_multi::relax`: columns below `sqrt(eps)*|x~|` are now
dropped. A regression test replays the failing case's map. After the fix,
all cases pass.
