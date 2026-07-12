//! Single-binary SPMD coupled oscillator.
//!
//! One executable composes the *same* two coupled oscillator roles two ways,
//! and **only the declarative `[topology]` table decides which**:
//!
//!   * `mode = "local"` (or `auto` at world size 1): both roles run in this one
//!     process over an in-memory `LocalTransport`.
//!   * `mode = "split"` (or `auto` when the world matches the role total):
//!     `mpirun -np 2 <this binary>` splits `MPI_COMM_WORLD` into one role-local
//!     communicator per role; this rank owns exactly one role and couples to its
//!     peer over `MpiInterCommTransport`.
//!
//! The composition in `contract.rs` is identical on both paths. Run it as
//! `cargo run --features mpi --example oscillator_spmd` for the local path and
//! `mpirun -np 2 <binary>` for the split path — the physics matches to the last
//! trajectory step. See `run_spmd.sh`.

mod contract;

use grass_mpi::{config_digest, Bootstrap, MpiRuntime, TopologyConfig};
use grass_multi::MpiInterCommTransport;

/// Default declarative bootstrap. `mode = "auto"` lets the launched world size
/// pick local vs split with no source or config change at all.
const DEFAULT_CONFIG: &str = include_str!("config.toml");

fn main() {
    // An optional path argument swaps the whole declarative document, proving
    // that only the TOML — not the Rust — chooses local vs role-split.
    let config_str = match std::env::args().nth(1) {
        Some(path) => std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read topology config `{path}`: {error}")),
        None => DEFAULT_CONFIG.to_string(),
    };

    let topology: TopologyConfig = grass_io::Config::from_str(&config_str).required_section("topology");
    let digest = config_digest(config_str.as_bytes());

    match MpiRuntime::bootstrap(&topology, digest)
        .unwrap_or_else(|error| panic!("bootstrap topology: {error}"))
    {
        Bootstrap::Local { topology } => run_local(&config_str, &topology),
        Bootstrap::Split { runtime } => run_split(&config_str, runtime),
    }
}

/// `TopologyMode::Local`: compose every role in this process.
fn run_local(config_str: &str, topology: &grass_mpi::RoleTopology) {
    assert_eq!(
        topology.roles().len(),
        2,
        "the coupled-oscillator example composes exactly two roles"
    );
    let trace = contract::run_local_pair_trace(config_str);
    contract::print_pair("LOCAL", trace.result);
    contract::print_local_trace(&trace.steps);

    let result = trace.result;
    assert_eq!(result.a.mirrored_peer.0, result.b.state.x);
    assert_eq!(result.b.mirrored_peer.0, result.a.state.x);
    println!("PASS local composition replayed the two-role coupling contract");
    grass_mpi::finalize_mpi();
}

/// `TopologyMode::Split`: this rank owns one role and couples to its peer over
/// raw-world MPI. The peer's coupling rank is resolved from the same topology,
/// so the launch never needs an out-of-band rank map.
fn run_split(config_str: &str, runtime: MpiRuntime) {
    let assignment = runtime.assignment().clone();
    let topology = runtime.topology().clone();
    let me = assignment.name();
    let peer = match me {
        contract::A => contract::B,
        contract::B => contract::A,
        other => panic!("unexpected role `{other}`: this example defines roles `a` and `b`"),
    };
    // Single coupling rank per role: address the peer role's first world rank.
    let peer_rank = topology
        .role_world_range(peer)
        .unwrap_or_else(|| panic!("peer role `{peer}` missing from topology"))
        .start;
    assert_eq!(
        assignment.role_size(),
        1,
        "this example assigns exactly one rank per role; multi-rank roles are future work"
    );

    let (result, trace) =
        contract::run_side_with_trace(me, MpiInterCommTransport::new(peer_rank), config_str);
    println!(
        "MPI side={me} world_rank={} color={} local={:.17e},{:.17e} mirror={:.17e}",
        assignment.world_rank(),
        assignment.color(),
        result.state.x,
        result.state.v,
        result.mirrored_peer.0
    );
    contract::print_side_trace(me, &trace);
    runtime.finalize();
}
