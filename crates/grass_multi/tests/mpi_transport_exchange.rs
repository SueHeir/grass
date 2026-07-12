#![cfg(feature = "mpi")]

use grass_multi::{MpiInterCommTransport, Transport};
use std::process::Command;

const CHILD_ENV: &str = "GRASS_MULTI_MPI_TRANSPORT_CHILD";

#[test]
fn symmetric_large_payload_exchange_completes() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let rank = grass_mpi::world_rank();
        assert_eq!(grass_mpi::world_size(), 2);
        let peer = 1 - rank;
        let transport = MpiInterCommTransport::new(peer);
        let payload = vec![rank as u8; 2 * 1024 * 1024 + rank as usize];
        transport.send(&payload);
        let received = transport.recv();
        assert_eq!(received, vec![peer as u8; 2 * 1024 * 1024 + peer as usize]);
        drop(transport);
        grass_mpi::finalize_mpi();
        return;
    }

    if Command::new("mpirun").arg("--version").output().is_err() {
        eprintln!("SKIP MPI transport exchange: `mpirun` not found");
        return;
    }
    let exe = std::env::current_exe().expect("current test executable");
    let status = Command::new("mpirun")
        .args(["-np", "2"])
        .arg(exe)
        .args(["--exact", "symmetric_large_payload_exchange_completes"])
        .env(CHILD_ENV, "1")
        .env("OMPI_MCA_btl", "self,vader")
        .status()
        .expect("spawn MPI transport exchange");
    assert!(status.success(), "MPI transport exchange failed: {status}");
}
