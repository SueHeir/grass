mod contract;

use grass_multi::MpiInterCommTransport;

fn main() {
    // This tiny one-rank binary has no intra-app MPI backend to initialize.
    assert_eq!(
        grass_mpi::world_size(),
        2,
        "oscillator MPMD expects exactly two WORLD ranks: `-np 1 a : -np 1 b`"
    );
    assert_eq!(
        grass_mpi::world_rank(),
        0,
        "oscillator_mpmd_a must occupy absolute WORLD rank 0; check MPMD launch order"
    );
    let result = contract::run_side(contract::A, MpiInterCommTransport::new(1));
    println!(
        "MPI side=a local={:.17e},{:.17e} mirror={:.17e}",
        result.state.x, result.state.v, result.mirrored_peer.0
    );
    grass_mpi::finalize_mpi();
}
