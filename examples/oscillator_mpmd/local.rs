mod contract;

fn main() {
    let trace = contract::run_local_pair_trace();
    contract::print_pair("LOCAL", trace.result);
    contract::print_trace("LOCAL", &trace.steps);
    let result = trace.result;
    assert_eq!(result.a.mirrored_peer.0, result.b.state.x);
    assert_eq!(result.b.mirrored_peer.0, result.a.state.x);
    println!("PASS LocalTransport replay matches the two-sided exchange contract");
}

#[test]
fn local_transport_replays_two_binary_contract() {
    let result = contract::run_local_pair();
    assert_eq!(result.a.mirrored_peer.0, result.b.state.x);
    assert_eq!(result.b.mirrored_peer.0, result.a.state.x);
}
