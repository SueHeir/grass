//! Live single-binary role split validation for [`grass_mpi::MpiRuntime`].

#![cfg(feature = "mpi_backend")]

use grass_mpi::{CommBackend, MpiRuntime, RoleSpec, RoleTopology};
use std::process::Command;

const CHILD_ENV: &str = "GRASS_MPI_ROLE_RUNTIME_CHILD";
const NRANKS: i32 = 4;

fn run_child() {
    let topology = RoleTopology::new([
        RoleSpec::new("dem", NRANKS / 2),
        RoleSpec::new("cfd", NRANKS / 2),
    ])
    .unwrap();
    let runtime = MpiRuntime::initialize(topology).unwrap();
    let assignment = runtime.assignment();
    let backend = runtime.solver_backend();

    assert_eq!(backend.rank(), assignment.role_rank());
    assert_eq!(backend.size(), assignment.role_size());
    assert_eq!(assignment.world_range().count() as i32, backend.size());
    match assignment.world_rank() {
        0 | 1 => assert_eq!(assignment.name(), "dem"),
        2 | 3 => assert_eq!(assignment.name(), "cfd"),
        rank => panic!("unexpected raw-world rank {rank}"),
    }

    backend.barrier();
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
