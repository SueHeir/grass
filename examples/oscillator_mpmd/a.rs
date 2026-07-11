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
    let (result, trace) = contract::run_side_with_trace(contract::A, MpiInterCommTransport::new(1));
    println!(
        "MPI side=a local={:.17e},{:.17e} mirror={:.17e}",
        result.state.x, result.state.v, result.mirrored_peer.0
    );
    for (step, state) in trace.iter().enumerate() {
        println!(
            "MPI_TRACE side=a step={} local={:.17e},{:.17e}",
            step + 1,
            state.state.x,
            state.state.v,
        );
    }
    grass_mpi::finalize_mpi();
}
