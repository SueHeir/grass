//! Live unequal-role validation for the correctness-first root bridge.

#![cfg(feature = "mpi")]

use grass_multi::{CoupledPairRunner, RoleLaunch};
use grass_mpi::{CommBackend, MpiCommBackend};
use std::process::Command;

const CHILD_ENV: &str = "GRASS_MULTI_ROLE_EXCHANGE_CHILD";
const NRANKS: i32 = 5;
const CONFIG: &str = r#"
    [topology]
    mode = "split"
    [[topology.role]]
    name = "dem"
    ranks = 3
    [[topology.role]]
    name = "cfd"
    ranks = 2
"#;

fn run_role(launch: RoleLaunch) {
    let role = launch.role().to_owned();
    let exchange = launch.into_role_exchange();
    // This is the solver communicator, already split by role. The ring
    // traffic below stands in for DEM forward/reverse and CFD halo traffic.
    // It deliberately runs around every cross-role exchange to prove the two
    // MPI message domains cannot match one another.
    let solver = MpiCommBackend::new(grass_mpi::get_mpi_world());
    let (rank, size) = exchange.role_position();
    let (marker, expected_peer_marker, expected_peer_size) = match role.as_str() {
        "dem" => (1_u8, 2_u8, 2),
        "cfd" => (2_u8, 1_u8, 3),
        other => panic!("unexpected role {other}"),
    };
    assert_eq!(size, if role == "dem" { 3 } else { 2 });
    assert_eq!(exchange.peer_size(), expected_peer_size);

    for iteration in 0..4 {
        let role_sum = solver.all_reduce_sum_f64((rank + 1) as f64);
        assert_eq!(role_sum, (size * (size + 1) / 2) as f64);

        let next = (rank + 1) % size;
        let previous = (rank + size - 1) % size;
        let forward = solver.sendrecv_f64(next, &[marker as f64, rank as f64], previous);
        assert_eq!(forward, vec![marker as f64, previous as f64]);
        let reverse = solver.sendrecv_f64(previous, &[iteration as f64, rank as f64], next);
        assert_eq!(reverse, vec![iteration as f64, next as f64]);

        // Deliberately exceed common MPI eager limits and vary every shard length.
        let mut local = vec![marker; 128 * 1024 + rank as usize * 97];
        local[0] = marker;
        local[1] = rank as u8;
        local[2] = iteration;
        let peer = exchange.exchange(&local).expect("exchange role shards");
        assert_eq!(peer.len(), expected_peer_size as usize);
        for (peer_rank, shard) in peer.iter().enumerate() {
            assert_eq!(shard.len(), 128 * 1024 + peer_rank * 97);
            assert_eq!(shard[0], expected_peer_marker);
            assert_eq!(shard[1], peer_rank as u8);
            assert_eq!(shard[2], iteration);
        }
    }
}

#[test]
fn unequal_roles_exchange_all_large_shards_in_rank_order() {
    if std::env::var_os(CHILD_ENV).is_some() {
        CoupledPairRunner::from_source(CONFIG)
            .and_then(|runner| runner.run(run_role, run_role))
            .expect("run unequal role exchange");
        return;
    }
    if Command::new("mpirun").arg("--version").output().is_err() {
        eprintln!("SKIP multi-rank role exchange: `mpirun` not found");
        return;
    }
    let executable = std::env::current_exe().expect("locate test binary");
    let status = Command::new("mpirun")
        .args(["--oversubscribe", "-np", &NRANKS.to_string()])
        .arg(executable)
        .args([
            "--exact",
            "unequal_roles_exchange_all_large_shards_in_rank_order",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .env("OMPI_MCA_btl", "self,vader")
        .status()
        .expect("spawn unequal role test under mpirun");
    assert!(
        status.success(),
        "multi-rank role exchange failed: {status}"
    );
}
