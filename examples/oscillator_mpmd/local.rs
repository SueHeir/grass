mod contract;

fn main() {
    let result = contract::run_local_pair();
    contract::print_pair("LOCAL", result);
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
