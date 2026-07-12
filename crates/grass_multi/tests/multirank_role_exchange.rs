//! Live unequal-role validation for the correctness-first root bridge.

#![cfg(feature = "mpi")]

use grass_mpi::{CommBackend, MpiCommBackend};
use grass_multi::{
    CoupledPairRunner, CouplingEpoch, EntityId, RoleLaunch, RoutedExchangeError, RoutedPayload,
};
use std::process::Command;

const CHILD_ENV: &str = "GRASS_MULTI_ROLE_EXCHANGE_CHILD";
const ROUTED_CHILD_ENV: &str = "GRASS_MULTI_ROUTED_EXCHANGE_CHILD";
const ROUTE_FAULT_CHILD_ENV: &str = "GRASS_MULTI_ROUTED_ROUTE_FAULT_CHILD";
const EPOCH_FAULT_CHILD_ENV: &str = "GRASS_MULTI_ROUTED_EPOCH_FAULT_CHILD";
const NRANKS: i32 = 5;
/// The coupled round on which exactly one rank injects a fault. Earlier rounds
/// must all succeed so the test proves the abort is *coordinated*, not merely
/// that a bad round fails on the rank that produced it.
const FAULT_STEP: u64 = 2;
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

fn run_routed_role(launch: RoleLaunch) {
    let role = launch.role().to_owned();
    let exchange = launch.into_routed_exchange();
    let (rank, size) = exchange.role_position();
    assert_eq!(size, if role == "dem" { 3 } else { 2 });

    for step in 0..3_u64 {
        let payload = vec![rank as u8; 128 * 1024 + rank as usize * 31];
        let outgoing = match (role.as_str(), rank) {
            ("dem", 0) => vec![RoutedPayload::new(0, EntityId(100), payload)],
            ("dem", 1) => vec![
                RoutedPayload::new(0, EntityId(110), payload.clone()),
                RoutedPayload::new(1, EntityId(111), payload),
            ],
            ("dem", 2) => vec![RoutedPayload::new(1, EntityId(121), payload)],
            ("cfd", 0) => vec![
                RoutedPayload::new(0, EntityId(200), payload.clone()),
                RoutedPayload::new(1, EntityId(201), payload),
            ],
            ("cfd", 1) => vec![
                RoutedPayload::new(1, EntityId(211), payload.clone()),
                RoutedPayload::new(2, EntityId(212), payload),
            ],
            _ => panic!("unexpected role/rank {role}/{rank}"),
        };
        let received = exchange
            .exchange(CouplingEpoch(step), &outgoing)
            .expect("exchange routed role records");
        let keys: Vec<(i32, u64)> = received
            .iter()
            .map(|record| (record.source, record.entity_id.0))
            .collect();
        let expected = match (role.as_str(), rank) {
            ("dem", 0) => vec![(0, 200)],
            ("dem", 1) => vec![(0, 201), (1, 211)],
            ("dem", 2) => vec![(1, 212)],
            ("cfd", 0) => vec![(0, 100), (1, 110)],
            ("cfd", 1) => vec![(1, 111), (2, 121)],
            _ => unreachable!(),
        };
        assert_eq!(keys, expected);
        for record in received {
            assert_eq!(record.payload[0], record.source as u8);
            assert_eq!(
                record.payload.len(),
                128 * 1024 + record.source as usize * 31
            );
        }
    }
    assert!(exchange
        .exchange(CouplingEpoch(99), &[])
        .expect("participate in empty sparse exchange")
        .is_empty());
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

#[test]
fn unequal_roles_route_large_records_to_selected_owners() {
    if std::env::var_os(ROUTED_CHILD_ENV).is_some() {
        CoupledPairRunner::from_source(CONFIG)
            .and_then(|runner| runner.run(run_routed_role, run_routed_role))
            .expect("run unequal routed exchange");
        return;
    }
    if Command::new("mpirun").arg("--version").output().is_err() {
        eprintln!("SKIP routed role exchange: `mpirun` not found");
        return;
    }
    let executable = std::env::current_exe().expect("locate test binary");
    let status = Command::new("mpirun")
        .args(["--oversubscribe", "-np", &NRANKS.to_string()])
        .arg(executable)
        .args([
            "--exact",
            "unequal_roles_route_large_records_to_selected_owners",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROUTED_CHILD_ENV, "1")
        .env("OMPI_MCA_btl", "self,vader")
        .status()
        .expect("spawn routed role test under mpirun");
    assert!(status.success(), "routed role exchange failed: {status}");
}

/// One rank in `dem` injects a locally-invalid route on [`FAULT_STEP`]. The
/// coordinated cross-role agreement must abort *every* rank on that same round,
/// with an actionable diagnostic each: the faulting rank names the bad route,
/// every peer names a peer abort. Without agreement the four valid ranks would
/// loop into the next collective and strand on the vanished faulting rank.
fn run_routed_route_fault_role(launch: RoleLaunch) {
    let role = launch.role().to_owned();
    let exchange = launch.into_routed_exchange();
    let (rank, _size) = exchange.role_position();
    let peer_size = exchange.peer_size();
    let is_faulter = role == "dem" && rank == 1;

    // A realistic driver loop: keep exchanging until a round fails, then stop.
    // If the abort were *not* coordinated, the ranks that saw a valid round
    // would iterate past `FAULT_STEP` and block forever in the next collective.
    let mut outcome: Option<(u64, RoutedExchangeError)> = None;
    for step in 0..16_u64 {
        let outgoing = if step == FAULT_STEP && is_faulter {
            // Address a rank outside the peer role — a validation fault local to
            // exactly one rank.
            vec![RoutedPayload::new(peer_size + 3, EntityId(9), vec![0; 64])]
        } else {
            vec![RoutedPayload::new(
                0,
                EntityId(1000 + rank as u64),
                vec![rank as u8; 64],
            )]
        };
        match exchange.exchange(CouplingEpoch(step), &outgoing) {
            Ok(_) => assert!(
                step < FAULT_STEP,
                "{role} rank {rank} succeeded at step {step}; expected coordinated abort at {FAULT_STEP}"
            ),
            Err(error) => {
                outcome = Some((step, error));
                break;
            }
        }
    }

    let (reached, error) = outcome
        .unwrap_or_else(|| panic!("{role} rank {rank} never observed the coordinated failure"));
    assert_eq!(
        reached, FAULT_STEP,
        "{role} rank {rank} aborted at step {reached}; expected coordinated abort at {FAULT_STEP}"
    );
    // Actionable, specific diagnostic on every rank.
    assert!(
        !error.to_string().is_empty(),
        "{role} rank {rank} produced an empty diagnostic"
    );
    if is_faulter {
        assert_eq!(
            error,
            RoutedExchangeError::DestinationOutOfRange {
                destination: peer_size + 3,
                peer_size,
            },
            "faulting rank must keep its own specific diagnostic"
        );
    } else {
        assert_eq!(
            error,
            RoutedExchangeError::PeerAborted,
            "{role} rank {rank} must learn of the peer fault as a lockstep abort"
        );
    }
}

/// One rank in `cfd` injects a stale coupling epoch on [`FAULT_STEP`], sending a
/// record framed a round behind. Here the fault is detected on *different* ranks
/// than the injector (whoever decodes the stale frame), exercising remote fault
/// detection: agreement must still abort every rank on the same round with an
/// actionable error.
fn run_routed_epoch_fault_role(launch: RoleLaunch) {
    let role = launch.role().to_owned();
    let exchange = launch.into_routed_exchange();
    let (rank, _size) = exchange.role_position();
    let is_faulter = role == "cfd" && rank == 0;

    let mut outcome: Option<(u64, RoutedExchangeError)> = None;
    for step in 0..16_u64 {
        // Every rank sends one record to peer rank 0, so the injector's stale
        // frame is actually decoded by a peer.
        let outgoing = vec![RoutedPayload::new(
            0,
            EntityId(2000 + rank as u64),
            vec![rank as u8; 64],
        )];
        // The faulter claims a round it already left behind.
        let claimed = if step == FAULT_STEP && is_faulter {
            CouplingEpoch(step.wrapping_sub(1))
        } else {
            CouplingEpoch(step)
        };
        match exchange.exchange(claimed, &outgoing) {
            Ok(_) => assert!(
                step < FAULT_STEP,
                "{role} rank {rank} succeeded at step {step}; expected coordinated abort at {FAULT_STEP}"
            ),
            Err(error) => {
                outcome = Some((step, error));
                break;
            }
        }
    }

    let (reached, error) = outcome
        .unwrap_or_else(|| panic!("{role} rank {rank} never observed the coordinated failure"));
    assert_eq!(
        reached, FAULT_STEP,
        "{role} rank {rank} aborted at step {reached}; expected coordinated abort at {FAULT_STEP}"
    );
    // Every rank aborts with an actionable error: either it decoded the stale
    // frame itself (epoch mismatch) or it learned of the peer fault (peer abort).
    assert!(
        matches!(
            error,
            RoutedExchangeError::EpochMismatch { .. } | RoutedExchangeError::PeerAborted
        ),
        "{role} rank {rank} produced an unexpected diagnostic: {error}"
    );
}

/// Spawn `child_env`'s role pair under `mpirun`, guarding the run with `timeout`
/// so a regression that reintroduces the strand fails fast instead of hanging
/// the whole suite.
fn run_fault_child_under_mpirun(test_name: &str, child_env: &str) {
    if Command::new("mpirun").arg("--version").output().is_err() {
        eprintln!("SKIP {test_name}: `mpirun` not found");
        return;
    }
    let executable = std::env::current_exe().expect("locate test binary");
    // A surviving strand blocks forever in the next collective; `timeout`
    // converts that hang into a legible failure (exit code 124).
    let has_timeout = Command::new("timeout").arg("--version").output().is_ok();
    let mut command = if has_timeout {
        let mut c = Command::new("timeout");
        c.args(["90", "mpirun"]);
        c
    } else {
        Command::new("mpirun")
    };
    let status = command
        .args(["--oversubscribe", "-np", &NRANKS.to_string()])
        .arg(executable)
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(child_env, "1")
        .env("OMPI_MCA_btl", "self,vader")
        .status()
        .expect("spawn fault-injection test under mpirun");
    if has_timeout {
        assert_ne!(
            status.code(),
            Some(124),
            "{test_name} deadlocked: a rank strand survived the coordinated abort (timed out)"
        );
    }
    assert!(status.success(), "{test_name} failed: {status}");
}

#[test]
fn one_invalid_route_aborts_every_rank_without_deadlock() {
    if std::env::var_os(ROUTE_FAULT_CHILD_ENV).is_some() {
        CoupledPairRunner::from_source(CONFIG)
            .and_then(|runner| runner.run(run_routed_route_fault_role, run_routed_route_fault_role))
            .expect("run routed route-fault exchange");
        return;
    }
    run_fault_child_under_mpirun(
        "one_invalid_route_aborts_every_rank_without_deadlock",
        ROUTE_FAULT_CHILD_ENV,
    );
}

#[test]
fn one_stale_epoch_aborts_every_rank_without_deadlock() {
    if std::env::var_os(EPOCH_FAULT_CHILD_ENV).is_some() {
        CoupledPairRunner::from_source(CONFIG)
            .and_then(|runner| runner.run(run_routed_epoch_fault_role, run_routed_epoch_fault_role))
            .expect("run routed epoch-fault exchange");
        return;
    }
    run_fault_child_under_mpirun(
        "one_stale_epoch_aborts_every_rank_without_deadlock",
        EPOCH_FAULT_CHILD_ENV,
    );
}
