//! Live validation and a reproducible scaling/volume benchmark for the direct
//! sparse routed exchange (NBX nonblocking-consensus metadata discovery).
//!
//! The correctness test drives unequal role sizes with a deliberately sparse
//! routing plan: some ranks route nothing (source holes), others route several
//! records to a small fan of owners. An in-test oracle recomputes the expected
//! per-owner delivery straight from the routing spec, independent of the
//! exchange internals, and asserts exact parity including deterministic source
//! ordering and empty-source semantics.
//!
//! The benchmark reports wall time and the dense all-to-all metadata volume the
//! previous personalized-exchange (PEX) length discovery would have moved,
//! versus the zero dedicated metadata bytes NBX moves. Topology and step count
//! are environment-driven so the benchmark is reproducible at any scale:
//!
//! ```text
//! GRASS_SPARSE_BENCH_DEM=20 GRASS_SPARSE_BENCH_CFD=12 GRASS_SPARSE_BENCH_STEPS=500 \
//!   cargo test -p grass_multi --features mpi --test sparse_routing_scaling \
//!   sparse_routing_scaling_volume -- --nocapture --ignored
//! ```

#![cfg(feature = "mpi")]

use grass_mpi::{CommBackend, MpiCommBackend};
use grass_multi::{CoupledPairRunner, CouplingEpoch, EntityId, RoleLaunch, RoutedPayload};
use std::process::Command;
use std::time::Instant;

const CORRECTNESS_CHILD: &str = "GRASS_SPARSE_ROUTING_CORRECTNESS_CHILD";
const BENCH_CHILD: &str = "GRASS_SPARSE_ROUTING_BENCH_CHILD";

const DEM_TAG: u64 = 0;
const CFD_TAG: u64 = 1;

fn role_tag(role: &str) -> u64 {
    match role {
        "dem" => DEM_TAG,
        "cfd" => CFD_TAG,
        other => panic!("unexpected role {other}"),
    }
}

/// Deterministic, sparse routing plan shared by senders and the receive oracle.
///
/// Returns `(destination_owner, entity_id, payload_len)` for each record this
/// `rank` routes into the peer role at `step`. Ranks selected by the silence
/// rule route nothing, producing source holes the delivery layer must skip
/// while preserving peer-source ordering for the ranks that do route.
fn routes(tag: u64, rank: i32, peer_size: i32, step: u64) -> Vec<(i32, u64, usize)> {
    if peer_size == 0 {
        return Vec::new();
    }
    // Rotating silence: a different subset of ranks routes nothing each step.
    if (rank as u64 + step) % 4 == 3 {
        return Vec::new();
    }
    let fan = std::cmp::min(2, peer_size);
    (0..fan)
        .map(|i| {
            let dest = ((rank as i64 * 7 + step as i64 * 3 + i as i64).rem_euclid(peer_size as i64))
                as i32;
            let entity_id = (tag << 40) | ((rank as u64) << 24) | (step << 8) | (i as u64);
            let payload_len = 4096 + rank as usize * 37 + i as usize * 11;
            (dest, entity_id, payload_len)
        })
        .collect()
}

fn build_outgoing(tag: u64, rank: i32, peer_size: i32, step: u64) -> Vec<RoutedPayload> {
    routes(tag, rank, peer_size, step)
        .into_iter()
        .map(|(dest, entity_id, len)| {
            let mut payload = vec![rank as u8; len];
            if len > 1 {
                payload[1] = tag as u8;
            }
            RoutedPayload::new(dest, EntityId(entity_id), payload)
        })
        .collect()
}

/// Oracle: what this owner must receive, derived purely from the peer role's
/// routing spec, in peer-source-rank order then per-source record order.
fn expected_delivery(
    peer_tag: u64,
    my_rank: i32,
    my_size: i32,
    peer_size: i32,
    step: u64,
) -> Vec<(i32, u64, usize)> {
    let mut expected = Vec::new();
    for source in 0..peer_size {
        for (dest, entity_id, len) in routes(peer_tag, source, my_size, step) {
            if dest == my_rank {
                expected.push((source, entity_id, len));
            }
        }
    }
    expected
}

fn bench_topology() -> (i32, i32, u64) {
    let dem = std::env::var("GRASS_SPARSE_BENCH_DEM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);
    let cfd = std::env::var("GRASS_SPARSE_BENCH_CFD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let steps = std::env::var("GRASS_SPARSE_BENCH_STEPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    (dem, cfd, steps)
}

fn topology_config(dem: i32, cfd: i32) -> String {
    format!(
        r#"
        [topology]
        mode = "split"
        [[topology.role]]
        name = "dem"
        ranks = {dem}
        [[topology.role]]
        name = "cfd"
        ranks = {cfd}
        "#
    )
}

const CORRECTNESS_DEM: i32 = 6;
const CORRECTNESS_CFD: i32 = 4;
const CORRECTNESS_STEPS: u64 = 6;

fn run_correctness(launch: RoleLaunch) {
    let role = launch.role().to_owned();
    let tag = role_tag(&role);
    let peer_tag = if tag == DEM_TAG { CFD_TAG } else { DEM_TAG };
    let exchange = launch.into_routed_exchange();
    let (rank, size) = exchange.role_position();
    let peer_size = exchange.peer_size();
    assert_eq!(
        size,
        if role == "dem" {
            CORRECTNESS_DEM
        } else {
            CORRECTNESS_CFD
        }
    );
    assert_eq!(
        peer_size,
        if role == "dem" {
            CORRECTNESS_CFD
        } else {
            CORRECTNESS_DEM
        }
    );

    for step in 0..CORRECTNESS_STEPS {
        let outgoing = build_outgoing(tag, rank, peer_size, step);
        let received = exchange
            .exchange(CouplingEpoch(step), &outgoing)
            .expect("sparse routed exchange");

        let expected = expected_delivery(peer_tag, rank, size, peer_size, step);
        let actual: Vec<(i32, u64, usize)> = received
            .iter()
            .map(|r| (r.source, r.entity_id.0, r.payload.len()))
            .collect();
        assert_eq!(
            actual, expected,
            "role {role} rank {rank} step {step}: delivery must match the routing oracle"
        );
        for record in &received {
            assert_eq!(
                record.payload[0], record.source as u8,
                "payload must originate from its reported source"
            );
            if record.payload.len() > 1 {
                assert_eq!(
                    record.payload[1], peer_tag as u8,
                    "payload must carry peer role tag"
                );
            }
        }
    }

    // Empty-source semantics: every rank still participates in the collective
    // and receives nothing when no owner routes to it.
    assert!(exchange
        .exchange(CouplingEpoch(9999), &[])
        .expect("participate in empty sparse exchange")
        .is_empty());
}

fn run_bench(launch: RoleLaunch) {
    let role = launch.role().to_owned();
    let tag = role_tag(&role);
    let (_, _, steps) = bench_topology();
    let world = MpiCommBackend::new(grass_mpi::get_mpi_world_raw());
    let world_size = grass_mpi::world_size();
    let world_rank = grass_mpi::world_rank();

    let exchange = launch.into_routed_exchange();
    let (rank, _size) = exchange.role_position();
    let peer_size = exchange.peer_size();

    // Warm up one exchange so communicator setup is out of the timed region.
    exchange
        .exchange(CouplingEpoch(0), &build_outgoing(tag, rank, peer_size, 0))
        .expect("warmup exchange");
    world.barrier();

    let mut local_sends = 0_u64;
    let start = Instant::now();
    for step in 1..=steps {
        let outgoing = build_outgoing(tag, rank, peer_size, step);
        local_sends += outgoing.len() as u64;
        let _ = exchange
            .exchange(CouplingEpoch(step), &outgoing)
            .expect("timed sparse routed exchange");
    }
    let elapsed = start.elapsed();

    let total_seconds = world.all_reduce_sum_f64(elapsed.as_secs_f64());
    let total_sends = world.all_reduce_sum_f64(local_sends as f64);
    if world_rank == 0 {
        let p = world_size as f64;
        // PEX length discovery: every rank contributes a P-length u64 send
        // buffer to MPI_Alltoall each step -> P*(P-1)*8 bytes cross the wire.
        let pex_metadata_bytes_per_step = p * (p - 1.0) * 8.0;
        let pex_metadata_total = pex_metadata_bytes_per_step * steps as f64;
        let avg_seconds = total_seconds / p;
        eprintln!("=== sparse routing scaling/volume ===");
        eprintln!("world ranks           : {world_size}");
        eprintln!("steps                 : {steps}");
        eprintln!("non-empty routes total: {}", total_sends as u64);
        eprintln!(
            "avg wall / rank       : {:.3} ms ({:.4} ms/step)",
            avg_seconds * 1e3,
            avg_seconds * 1e3 / steps as f64
        );
        eprintln!(
            "PEX metadata avoided  : {} bytes ({:.0} bytes/step, dense all-to-all)",
            pex_metadata_total as u64, pex_metadata_bytes_per_step
        );
        eprintln!("NBX metadata bytes    : 0 (rendezvous by matched probe)");
        eprintln!("======================================");
    }
    world.barrier();
}

fn require_mpirun() -> bool {
    if Command::new("mpirun").arg("--version").output().is_err() {
        eprintln!("SKIP sparse routing test: `mpirun` not found");
        return false;
    }
    true
}

fn spawn_under_mpirun(np: i32, filter: &str, child_env: &str, ignored: bool) {
    let executable = std::env::current_exe().expect("locate test binary");
    let mut args = vec![
        "--exact".to_string(),
        filter.to_string(),
        "--nocapture".to_string(),
        "--test-threads=1".to_string(),
    ];
    if ignored {
        args.push("--ignored".to_string());
    }
    let status = Command::new("mpirun")
        .args(["--oversubscribe", "-np", &np.to_string()])
        .arg(executable)
        .args(&args)
        .env(child_env, "1")
        .env("OMPI_MCA_btl", "self,vader")
        .status()
        .expect("spawn sparse routing test under mpirun");
    assert!(status.success(), "sparse routing child failed: {status}");
}

#[test]
fn sparse_unequal_roles_deliver_only_addressed_owners() {
    if std::env::var_os(CORRECTNESS_CHILD).is_some() {
        let config = topology_config(CORRECTNESS_DEM, CORRECTNESS_CFD);
        CoupledPairRunner::from_source(&config)
            .and_then(|runner| runner.run(run_correctness, run_correctness))
            .expect("run sparse routing correctness");
        return;
    }
    if !require_mpirun() {
        return;
    }
    spawn_under_mpirun(
        CORRECTNESS_DEM + CORRECTNESS_CFD,
        "sparse_unequal_roles_deliver_only_addressed_owners",
        CORRECTNESS_CHILD,
        false,
    );
}

#[test]
#[ignore = "scaling/volume benchmark; run explicitly with --nocapture"]
fn sparse_routing_scaling_volume() {
    if std::env::var_os(BENCH_CHILD).is_some() {
        let (dem, cfd, _) = bench_topology();
        let config = topology_config(dem, cfd);
        CoupledPairRunner::from_source(&config)
            .and_then(|runner| runner.run(run_bench, run_bench))
            .expect("run sparse routing benchmark");
        return;
    }
    if !require_mpirun() {
        return;
    }
    let (dem, cfd, _) = bench_topology();
    spawn_under_mpirun(
        dem + cfd,
        "sparse_routing_scaling_volume",
        BENCH_CHILD,
        true,
    );
}
