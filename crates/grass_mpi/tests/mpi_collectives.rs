//! Collective-operation tests for the real [`MpiCommBackend`].
//!
//! These run against a live MPI communicator obtained from [`get_mpi_world`].
//! Under a plain `cargo test` (no launcher) MPI initializes as a singleton, so
//! the communicator has size 1; under `mpirun -np N` the same assertions hold
//! for any N because every expected value is written as a function of the rank
//! count rather than hard-coded for one rank.
//!
//! **Single-threaded MPI contract.** `MpiCommBackend` promises that every MPI
//! call happens on one thread (see the crate docs' `unsafe impl Send/Sync`
//! note). `cargo test` runs test functions on multiple threads concurrently, so
//! *all* MPI use is funnelled through this one `#[test]` function — collectives
//! are exercised sequentially inside it, and no other test in this file touches
//! MPI. `finalize_mpi()` is called once at the very end, after the last
//! collective and while still on this single test thread.
#![cfg(feature = "mpi_backend")]

use grass_mpi::{finalize_mpi, get_mpi_world, CommBackend, MpiCommBackend};

/// Exercise every collective on the real backend against a live communicator.
#[test]
fn mpi_backend_collectives() {
    let world = get_mpi_world();
    let backend = MpiCommBackend::new(world);

    let rank = backend.rank();
    let size = backend.size();
    assert!(size >= 1, "communicator size must be positive, got {size}");
    assert!(
        rank >= 0 && rank < size,
        "rank {rank} out of range for size {size}"
    );

    let n = size as f64;

    // all_reduce_sum: each rank contributes (rank + 1). The global sum is the
    // triangular number 1 + 2 + ... + size = size*(size+1)/2. On a singleton
    // this is 1.0; under mpirun -np N it scales with N.
    let sum = backend.all_reduce_sum_f64((rank + 1) as f64);
    let expected_sum = n * (n + 1.0) / 2.0;
    assert_eq!(
        sum, expected_sum,
        "all_reduce_sum_f64: expected {expected_sum}, got {sum}"
    );

    // all_reduce_sum of a per-rank constant 1.0 recovers the rank count exactly.
    let count = backend.all_reduce_sum_f64(1.0);
    assert_eq!(count, n, "all_reduce_sum_f64 of ones should equal size");

    // all_reduce_min: with each rank contributing (rank + 1), the global min is
    // rank 0's value, 1.0, regardless of how many ranks participate.
    let min = backend.all_reduce_min_f64((rank + 1) as f64);
    assert_eq!(min, 1.0, "all_reduce_min_f64: expected 1.0, got {min}");

    // all_reduce_min sees a value contributed only by the highest rank: make
    // every rank report a large sentinel except the last, which reports -3.0.
    let sentinel = if rank == size - 1 { -3.0 } else { 1.0e9 };
    let min2 = backend.all_reduce_min_f64(sentinel);
    assert_eq!(
        min2, -3.0,
        "all_reduce_min_f64 should surface the last rank's value"
    );

    // barrier must return (it is a no-op on a singleton, a real sync otherwise).
    backend.barrier();

    // A reduction after the barrier confirms the communicator is still usable.
    let post_barrier = backend.all_reduce_sum_f64(2.0);
    assert_eq!(post_barrier, 2.0 * n, "reduction after barrier");

    // Tear down the MPI universe on this same (single) thread. Safe here
    // because this is the only test that touches MPI.
    finalize_mpi();
}
