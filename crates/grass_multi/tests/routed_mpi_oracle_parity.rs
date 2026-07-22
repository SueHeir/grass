//! Real-MPI parity campaign for direct sparse routing against the root-bridge
//! delivery oracle.  The oracle deliberately constructs the records a root
//! bridge would deliver: peer source-rank order, then source-local order.

#![cfg(feature = "mpi")]

use grass_mpi::{CommBackend, MpiCommBackend};
use grass_multi::{CoupledPairRunner, CouplingEpoch, EntityId, RoleLaunch, RoutedPayload};
use std::process::Command;

const CHILD_ENV: &str = "GRASS_ROUTED_MPI_ORACLE_PARITY_CHILD";
const DEM_RANKS: i32 = 4;
const CFD_RANKS: i32 = 3;
const STEPS: u64 = 5;

const CONFIG: &str = r#"
    [topology]
    mode = "split"
    [[topology.role]]
    name = "left"
    ranks = 4
    [[topology.role]]
    name = "right"
    ranks = 3
"#;

fn tag(role: &str) -> u8 {
    match role {
        "left" => 0x4c,
        "right" => 0x52,
        other => panic!("unexpected role {other}"),
    }
}

/// The campaign's declarative routing matrix.  It rotates silent senders and
/// deliberately leaves owners empty on some epochs while retaining multi-record
/// sources, so ordering and conservation are observable rather than incidental.
fn plan(source_tag: u8, source: i32, peer_size: i32, epoch: u64) -> Vec<(i32, u64, Vec<u8>)> {
    if (source as u64 + epoch) % 3 == 2 {
        return Vec::new();
    }
    let count = if (source as u64 + epoch) % 2 == 0 {
        2
    } else {
        1
    };
    (0..count)
        .map(|ordinal| {
            // This intentionally never chooses the final owner on even epochs,
            // giving that destination a genuine empty-delivery round.
            let active_owners = if epoch % 2 == 0 {
                peer_size - 1
            } else {
                peer_size
            };
            let destination =
                ((source as u64 * 5 + epoch * 2 + ordinal) % active_owners as u64) as i32;
            let id = ((source_tag as u64) << 56)
                | ((epoch & 0xffff) << 24)
                | ((source as u64) << 8)
                | ordinal as u64;
            let len = 23 + source as usize * 7 + ordinal as usize * 3 + epoch as usize;
            let payload = (0..len)
                .map(|byte| {
                    source_tag
                        .wrapping_add(source as u8)
                        .wrapping_add(byte as u8)
                })
                .collect();
            (destination, id, payload)
        })
        .collect()
}

fn outgoing(source_tag: u8, source: i32, peer_size: i32, epoch: u64) -> Vec<RoutedPayload> {
    plan(source_tag, source, peer_size, epoch)
        .into_iter()
        .map(|(destination, id, payload)| RoutedPayload::new(destination, EntityId(id), payload))
        .collect()
}

/// Independent root-bridge oracle: roots concatenate one frame per peer
/// source, and delivery filters those frames without reordering their bytes.
fn root_bridge_oracle(
    peer_tag: u8,
    owner: i32,
    my_size: i32,
    peer_size: i32,
    epoch: u64,
) -> Vec<(i32, EntityId, Vec<u8>)> {
    let mut delivered = Vec::new();
    for source in 0..peer_size {
        for (destination, id, payload) in plan(peer_tag, source, my_size, epoch) {
            if destination == owner {
                delivered.push((source, EntityId(id), payload));
            }
        }
    }
    delivered
}

fn run_campaign(launch: RoleLaunch) {
    let role = launch.role().to_owned();
    let source_tag = tag(&role);
    let peer_tag = if role == "left" {
        tag("right")
    } else {
        tag("left")
    };
    let exchange = launch.into_routed_exchange();
    let (rank, size) = exchange.role_position();
    let peer_size = exchange.peer_size();
    assert_eq!(size, if role == "left" { DEM_RANKS } else { CFD_RANKS });
    assert_eq!(
        peer_size,
        if role == "left" { CFD_RANKS } else { DEM_RANKS }
    );
    let world = MpiCommBackend::new(grass_mpi::get_mpi_world_raw());

    for epoch in 0..STEPS {
        let sent = outgoing(source_tag, rank, peer_size, epoch);
        let expected = root_bridge_oracle(peer_tag, rank, size, peer_size, epoch);
        let received = exchange
            .exchange(CouplingEpoch(epoch), &sent)
            .expect("direct sparse exchange");
        let actual: Vec<_> = received
            .into_iter()
            .map(|r| (r.source, r.entity_id, r.payload))
            .collect();
        assert_eq!(actual, expected, "{role} rank {rank}, epoch {epoch}: direct sparse output differs from root-bridge oracle");

        let local_sent = sent.len() as f64;
        let local_received = actual.len() as f64;
        let global_sent = world.all_reduce_sum_f64(local_sent);
        let global_received = world.all_reduce_sum_f64(local_received);
        assert_eq!(
            global_sent, global_received,
            "epoch {epoch}: routed payload count is not conserved"
        );
    }
    // Every rank participates in the final empty round, including owners that
    // had sparse traffic in prior epochs.
    assert!(exchange
        .exchange(CouplingEpoch(99), &[])
        .expect("empty round")
        .is_empty());
}

#[test]
fn direct_sparse_mpi_is_byte_identical_to_root_bridge_oracle() {
    if std::env::var_os(CHILD_ENV).is_some() {
        CoupledPairRunner::from_source(CONFIG)
            .and_then(|runner| runner.run(run_campaign, run_campaign))
            .expect("run routed MPI oracle-parity campaign");
        return;
    }
    if Command::new("mpirun").arg("--version").output().is_err() {
        eprintln!("SKIP routed MPI oracle parity: `mpirun` not found");
        return;
    }
    let executable = std::env::current_exe().expect("locate test binary");
    let status = Command::new("mpirun")
        .args(["--oversubscribe", "-np", "7"])
        .arg(executable)
        .args([
            "--exact",
            "direct_sparse_mpi_is_byte_identical_to_root_bridge_oracle",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .env("OMPI_MCA_btl", "self,vader")
        .status()
        .expect("spawn routed MPI oracle-parity campaign under mpirun");
    assert!(
        status.success(),
        "routed MPI oracle-parity campaign failed: {status}"
    );
}
