//! Several Apps in one process share one MPI session.
//!
//! Coupled sub-Apps and Apps run one after another both need MPI to outlive
//! any single App. GRASS finalizes MPI at process exit, so an App finishing
//! must not take MPI down under the next one.
#![cfg(feature = "mpi_backend")]

use grass_app::App;
use grass_mpi::{mpi_is_finalized, CommResource, MpiPlugin};

/// Sum of 1.0 over all ranks, and the rank count; equal when MPI works.
fn rank_sum() -> (f64, f64) {
    let mut app = App::new();
    app.add_plugins(MpiPlugin);
    let (total, size) = {
        let comm = app
            .get_resource_ref::<CommResource>()
            .expect("MpiPlugin installs a communicator");
        (comm.all_reduce_sum_f64(1.0), comm.size() as f64)
    };
    app.run_cleanup();
    (total, size)
}

#[test]
fn apps_in_sequence_and_side_by_side_share_mpi() {
    // One after another: each App cleans up before the next is built.
    for _ in 0..3 {
        let (total, size) = rank_sum();
        assert_eq!(total, size);
        assert!(!mpi_is_finalized(), "an App's cleanup finalized MPI");
    }

    // Side by side, the way coupled sub-Apps live: the first one to clean up
    // must leave MPI running for the other.
    let mut a = App::new();
    a.add_plugins(MpiPlugin);
    let mut b = App::new();
    b.add_plugins(MpiPlugin);
    a.run_cleanup();
    drop(a);
    assert!(!mpi_is_finalized());
    {
        let comm = b.get_resource_ref::<CommResource>().unwrap();
        assert_eq!(comm.all_reduce_sum_f64(2.0), 2.0 * comm.size() as f64);
    }
    b.run_cleanup();
}
