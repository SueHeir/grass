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
        1,
        "oscillator_mpmd_b must occupy absolute WORLD rank 1; check MPMD launch order"
    );
    let result = contract::run_side(contract::B, MpiInterCommTransport::new(0));
    println!(
        "MPI side=b local={:.17e},{:.17e} mirror={:.17e}",
        result.state.x, result.state.v, result.mirrored_peer.0
    );
    grass_mpi::finalize_mpi();
}
