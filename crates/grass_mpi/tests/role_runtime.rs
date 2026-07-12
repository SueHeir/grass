//! Live single-binary role split validation for [`grass_mpi::MpiRuntime`].

#![cfg(feature = "mpi_backend")]

use grass_mpi::{world_rank, CommBackend, MpiRuntime, RoleSpec, RoleTopology};
use std::process::Command;

const CHILD_ENV: &str = "GRASS_MPI_ROLE_RUNTIME_CHILD";
const DEM_RANKS: i32 = 2;
const CFD_RANKS: i32 = 3;
const NRANKS: i32 = DEM_RANKS + CFD_RANKS;

fn run_child() {
    let topology = RoleTopology::new([
        RoleSpec::new("dem", DEM_RANKS),
        RoleSpec::new("cfd", CFD_RANKS),
    ])
    .unwrap();
    let runtime = MpiRuntime::initialize(topology).unwrap();
    let assignment = runtime.assignment();
    let backend = runtime.solver_backend();
    // A second accessor must duplicate the role-local communicator, rather
    // than accidentally falling back to raw MPI_COMM_WORLD.
    let duplicate_backend = runtime.solver_backend();
    let _comm_f = runtime.solver_comm_fortran_handle();

    assert_eq!(backend.rank(), assignment.role_rank());
    assert_eq!(backend.size(), assignment.role_size());
    assert_eq!(duplicate_backend.rank(), assignment.role_rank());
    assert_eq!(duplicate_backend.size(), assignment.role_size());
    assert_eq!(assignment.world_range().count() as i32, backend.size());
    match assignment.world_rank() {
        0 | 1 => assert_eq!(assignment.name(), "dem"),
        2 | 3 | 4 => assert_eq!(assignment.name(), "cfd"),
        rank => panic!("unexpected raw-world rank {rank}"),
    }

    // Repeated role-local synchronization must neither mix the unequal role
    // domains nor poison later operations on independently duplicated comms.
    // Summing raw world ranks makes cross-role leakage immediately visible.
    let expected_world_rank_sum = match assignment.name() {
        "dem" => 0.0 + 1.0,
        "cfd" => 2.0 + 3.0 + 4.0,
        role => panic!("unexpected role {role}"),
    };
    for round in 0..16 {
        let observed = if round % 2 == 0 {
            backend.barrier();
            backend.all_reduce_sum_f64(world_rank() as f64)
        } else {
            duplicate_backend.barrier();
            duplicate_backend.all_reduce_sum_f64(world_rank() as f64)
        };
        assert_eq!(
            observed,
            expected_world_rank_sum,
            "round {round} crossed the {} communicator domain",
            assignment.name()
        );
    }

    drop(duplicate_backend);
    drop(backend);
    runtime.finalize();
}

#[test]
fn one_binary_splits_into_role_local_communicators() {
    if std::env::var_os(CHILD_ENV).is_some() {
        run_child();
        return;
    }
    if Command::new("mpirun").arg("--version").output().is_err() {
        eprintln!("SKIP role runtime test: `mpirun` not found");
        return;
    }

    let executable = std::env::current_exe().expect("locate role-runtime test binary");
    let status = Command::new("mpirun")
        .args(["--oversubscribe", "-np", &NRANKS.to_string()])
        .arg(executable)
        .args([
            "--exact",
            "one_binary_splits_into_role_local_communicators",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .status()
        .expect("spawn role-runtime test under mpirun");
    assert!(status.success(), "role-runtime mpirun failed: {status}");
}
