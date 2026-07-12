//! One oscillator binary; TOML alone selects local or MPI-split placement.

mod contract;

use grass_multi::{CoupledPairRunner, PairRun, RoleLaunch};

const DEFAULT_CONFIG: &str = include_str!("config.toml");

fn run_role(launch: RoleLaunch) -> (contract::SideResult, Vec<contract::SideResult>) {
    let role = launch.role().to_owned();
    let (source, transport) = launch.into_parts();
    contract::run_side_with_trace(&role, transport, &source)
}

fn main() {
    let run = CoupledPairRunner::from_cli_or(DEFAULT_CONFIG)
        .and_then(|runner| runner.run(run_role, run_role))
        .unwrap_or_else(|error| panic!("run coupled oscillator: {error}"));

    match run {
        PairRun::Local {
            first: (a, a_trace),
            second: (b, b_trace),
        } => {
            let result = contract::PairResult { a, b };
            contract::print_pair("LOCAL", result);
            contract::print_local_trace(&a_trace, &b_trace);
            assert_eq!(a.mirrored_peer.0, b.state.x);
            assert_eq!(b.mirrored_peer.0, a.state.x);
            println!("PASS local composition replayed the two-role coupling contract");
        }
        PairRun::Split {
            role,
            result: (result, trace),
        } => {
            println!(
                "MPI side={role} local={:.17e},{:.17e} mirror={:.17e}",
                result.state.x, result.state.v, result.mirrored_peer.0
            );
            contract::print_side_trace(&role, &trace);
        }
    }
}
