use grass_multi::{LocalTransport, Physics, RemoteMirrorPhysics, Transport, Wire};
use std::any::TypeId;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Counter(u64);

impl Wire for Counter {
    fn pack(&self) -> Vec<u8> {
        self.0.to_le_bytes().to_vec()
    }
    fn unpack(buf: &[u8]) -> Self {
        Self(u64::from_le_bytes(buf.try_into().unwrap()))
    }
}

#[test]
fn receive_resource_is_stale_until_explicit_pump_completes() {
    let (peer, mirror_transport) = LocalTransport::pair();
    let mut mirror = RemoteMirrorPhysics::new("peer", Box::new(mirror_transport));
    mirror.add_recv_each_iter::<Counter>();

    let state = mirror.resource_coherence::<Counter>().unwrap();
    assert!(state.receives);
    assert!(!state.fresh);

    peer.send(&Counter(42).pack());
    mirror.try_step().unwrap();
    assert!(mirror.resource_coherence::<Counter>().unwrap().fresh);
    Physics::validate_resource_read(&mirror, TypeId::of::<Counter>(), "Counter");
    let value = mirror
        .resource_cell(TypeId::of::<Counter>())
        .unwrap()
        .borrow();
    assert_eq!(value.downcast_ref::<Counter>(), Some(&Counter(42)));
}

#[test]
fn local_write_marks_send_slot_dirty_and_successful_send_clears_it() {
    let (peer, mirror_transport) = LocalTransport::pair();
    let mut mirror = RemoteMirrorPhysics::new("peer", Box::new(mirror_transport));
    mirror.add_send_each_iter::<Counter>();

    Physics::mark_resource_written(&mirror, TypeId::of::<Counter>());
    assert!(mirror.resource_coherence::<Counter>().unwrap().dirty);
    mirror.try_step().unwrap();
    assert!(!mirror.resource_coherence::<Counter>().unwrap().dirty);
    assert_eq!(Counter::unpack(&peer.recv()), Counter(0));
}
