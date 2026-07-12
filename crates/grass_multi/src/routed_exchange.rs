//! Geometry-agnostic routed exchange between solver roles.
//!
//! A coupling package decides each record's destination role-rank. GRASS only
//! frames, transports, validates, and deterministically delivers those opaque
//! records. Split MPI runs send non-empty frames directly between owner ranks;
//! the [`RoleExchange`] root bridge remains the local-mode implementation and
//! correctness oracle.

use crate::{RoleExchange, RoleExchangeError};
use mpi::collective::{CommunicatorCollectives, SystemOperation};
use mpi::topology::SimpleCommunicator;
use mpi::traits::{Communicator, Destination, MatchedReceiveVec, Source};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 4] = b"GRRT";
const VERSION: u8 = 1;

/// Tag period for the reused coupling context. The nonblocking barrier bounds
/// the epoch spread between any two ranks to one, so any period `>= 2` keeps two
/// concurrently live epochs from aliasing to the same tag; the headroom below
/// stays well under the MPI tag upper bound.
const EPOCH_TAG_MODULUS: u64 = 1024;

/// Monotonic identifier for one coupling exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CouplingEpoch(pub u64);

/// Stable identifier supplied by the coupling package for one exchanged item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId(pub u64);

/// One opaque record addressed to a rank in the peer solver role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedPayload {
    /// Rank within the peer role communicator, not a raw-world rank.
    pub destination: i32,
    /// Stable scientific entity or contribution identifier.
    pub entity_id: EntityId,
    /// Coupling-package-owned bytes.
    pub payload: Vec<u8>,
}

impl RoutedPayload {
    /// Construct one routed opaque record.
    pub fn new(destination: i32, entity_id: EntityId, payload: Vec<u8>) -> Self {
        Self {
            destination,
            entity_id,
            payload,
        }
    }
}

/// One record delivered to this role-rank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedPayload {
    /// Rank within the peer role that produced this record.
    pub source: i32,
    /// Stable identifier supplied by the peer coupling code.
    pub entity_id: EntityId,
    /// Coupling-package-owned bytes.
    pub payload: Vec<u8>,
}

/// Routed exchange with interchangeable root-bridge and direct sparse MPI
/// backends.
pub struct RoutedRoleExchange {
    backend: RoutedBackend,
}

enum RoutedBackend {
    RootBridge(Box<dyn RoleExchange>),
    Sparse(MpiSparseRoutedExchange),
}

struct MpiSparseRoutedExchange {
    role: SimpleCommunicator,
    coupling: SimpleCommunicator,
    peer_root: i32,
    peer_size: i32,
    transport_round: AtomicU64,
}

unsafe impl Send for MpiSparseRoutedExchange {}
unsafe impl Sync for MpiSparseRoutedExchange {}

impl RoutedRoleExchange {
    /// Wrap an existing collective role exchange.
    pub fn new(exchange: Box<dyn RoleExchange>) -> Self {
        Self {
            backend: RoutedBackend::RootBridge(exchange),
        }
    }

    /// Build a direct sparse MPI exchange from validated peer-role metadata.
    pub(crate) fn new_sparse(peer_root: i32, peer_size: i32) -> Self {
        Self {
            backend: RoutedBackend::Sparse(MpiSparseRoutedExchange {
                role: grass_mpi::get_mpi_world(),
                // Keep coupling messages in a context isolated from both the
                // role-local solver communicator and other GRASS transports.
                coupling: grass_mpi::get_mpi_world_raw().duplicate(),
                peer_root,
                peer_size,
                transport_round: AtomicU64::new(0),
            }),
        }
    }

    /// Rank and size within the local solver role.
    pub fn role_position(&self) -> (i32, i32) {
        match &self.backend {
            RoutedBackend::RootBridge(exchange) => exchange.role_position(),
            RoutedBackend::Sparse(exchange) => (exchange.role.rank(), exchange.role.size()),
        }
    }

    /// Size of the peer solver role.
    pub fn peer_size(&self) -> i32 {
        match &self.backend {
            RoutedBackend::RootBridge(exchange) => exchange.peer_size(),
            RoutedBackend::Sparse(exchange) => exchange.peer_size,
        }
    }

    /// Collectively exchange routed records and return only records addressed
    /// to this role-rank.
    ///
    /// Delivery order is deterministic: peer source role-rank order, followed
    /// by each source's original record order. Empty outgoing lists still
    /// participate in the collective.
    ///
    /// Failure is agreed across **both** roles: if any rank's round is invalid
    /// — an out-of-range route, a stale epoch, a malformed frame, or a
    /// transport fault — every rank returns an error rather than only the rank
    /// that detected it. A rank whose own round was valid learns of a peer's
    /// fault as [`RoutedExchangeError::PeerAborted`], so no rank walks into the
    /// next collective while a peer has already bailed out.
    pub fn exchange(
        &self,
        epoch: CouplingEpoch,
        outgoing: &[RoutedPayload],
    ) -> Result<Vec<ReceivedPayload>, RoutedExchangeError> {
        // Every rank must reach the failure-agreement collective, so compute
        // this rank's outcome without letting any local fault return early past
        // it. `exchange_local` still runs the data collective on all ranks
        // before surfacing any error.
        let local_outcome = self.exchange_local(epoch, outgoing);
        let any_failed = self.agree_failure(local_outcome.is_err());
        match local_outcome {
            // This rank has the actionable, specific diagnostic.
            Err(error) => Err(error),
            // This rank was valid but a peer aborted; fail in lockstep with an
            // actionable error rather than proceeding into the next collective.
            Ok(_) if any_failed => Err(RoutedExchangeError::PeerAborted),
            Ok(received) => Ok(received),
        }
    }

    /// This rank's local exchange outcome. The data collective always completes
    /// on every rank before an error is surfaced, so the caller can safely run
    /// the cross-role failure agreement afterwards.
    fn exchange_local(
        &self,
        epoch: CouplingEpoch,
        outgoing: &[RoutedPayload],
    ) -> Result<Vec<ReceivedPayload>, RoutedExchangeError> {
        // Do not return before the collective: one locally invalid route must
        // not strand peer ranks inside the root bridge. Remember the error,
        // participate with the diagnostic frame, then fail locally after the
        // collective has completed.
        let local_error = outgoing.iter().find_map(|record| {
            (record.destination < 0 || record.destination >= self.peer_size()).then_some(
                RoutedExchangeError::DestinationOutOfRange {
                    destination: record.destination,
                    peer_size: self.peer_size(),
                },
            )
        });

        let peer_frames = match &self.backend {
            RoutedBackend::RootBridge(exchange) => {
                let local_frame = encode_frame(epoch, outgoing);
                exchange.exchange(&local_frame)?
            }
            RoutedBackend::Sparse(exchange) => exchange.exchange(epoch, outgoing)?,
        };
        if let Some(error) = local_error {
            return Err(error);
        }
        deliver_frames(peer_frames, epoch, self.role_position())
    }

    /// Reduce `local_failed` across every rank of both roles, returning `true`
    /// if any rank failed. Delegated to whichever backend owns the isolated
    /// coupling context.
    fn agree_failure(&self, local_failed: bool) -> bool {
        match &self.backend {
            RoutedBackend::RootBridge(exchange) => exchange.agree_failure(local_failed),
            RoutedBackend::Sparse(exchange) => exchange.agree_failure(local_failed),
        }
    }
}

fn deliver_frames(
    peer_frames: Vec<Vec<u8>>,
    epoch: CouplingEpoch,
    (local_rank, local_size): (i32, i32),
) -> Result<Vec<ReceivedPayload>, RoutedExchangeError> {
    let mut received = Vec::new();
    for (source, frame) in peer_frames.iter().enumerate() {
        let records = decode_frame(frame, epoch)?;
        for record in records {
            if record.destination < 0 || record.destination >= local_size {
                return Err(RoutedExchangeError::DestinationOutOfRange {
                    destination: record.destination,
                    peer_size: local_size,
                });
            }
            if record.destination == local_rank {
                received.push(ReceivedPayload {
                    source: source as i32,
                    entity_id: record.entity_id,
                    payload: record.payload,
                });
            }
        }
    }
    Ok(received)
}

impl MpiSparseRoutedExchange {
    fn exchange(
        &self,
        epoch: CouplingEpoch,
        outgoing: &[RoutedPayload],
    ) -> Result<Vec<Vec<u8>>, RoutedExchangeError> {
        let mut by_destination = vec![Vec::new(); self.peer_size as usize];
        for record in outgoing {
            if let Some(records) = by_destination.get_mut(record.destination as usize) {
                records.push(record.clone());
            }
        }

        let frames: Vec<Vec<u8>> = by_destination
            .iter()
            .map(|records| encode_frame(epoch, records))
            .collect();

        // Retain one logical frame per peer source so the public delivery order
        // and source identifiers exactly match the root-bridge oracle. Sources
        // that route nothing to us keep their empty placeholder frame and are
        // never transmitted on the wire.
        let mut received: Vec<Vec<u8>> = (0..self.peer_size)
            .map(|_| encode_frame(epoch, &[]))
            .collect();

        // Nonblocking-consensus (NBX) dynamic sparse data exchange. Rather than
        // a dense world-size all-to-all announcing every rank's per-destination
        // frame length, each rank issues one synchronous nonblocking send per
        // non-empty owner-to-owner route, drains incoming frames with matched
        // probes, and enters a nonblocking barrier once its own sends are
        // locally matched. The barrier completes only after every rank has
        // entered, by which point every synchronous send has been matched and
        // received. Metadata and wire traffic are therefore proportional to the
        // number of non-empty routes, not to the world size.
        //
        // The coupling context is reused across calls, so every message is
        // stamped with an exchange-local transport round and probed by that
        // same tag. This keeps a rank that has already raced ahead from
        // having its send stolen by a peer still draining the current epoch's
        // `ANY_SOURCE` probe loop. The scientific epoch remains in the frame,
        // where a mismatched caller can be received and diagnosed instead of
        // deadlocking on a different MPI tag. The barrier ordering bounds the
        // transport-round spread between ranks to one, so the small tag period
        // cannot alias two concurrently live rounds.
        let transport_round = self.transport_round.fetch_add(1, Ordering::Relaxed);
        let tag = (transport_round % EPOCH_TAG_MODULUS) as mpi::Tag;
        let sends = by_destination
            .iter()
            .filter(|records| !records.is_empty())
            .count();
        let mut malformed = false;
        mpi::request::multiple_scope(sends, |scope, requests| {
            for (destination, records) in by_destination.iter().enumerate() {
                if !records.is_empty() {
                    requests.add(
                        self.coupling
                            .process_at_rank(self.peer_root + destination as i32)
                            .immediate_synchronous_send_with_tag(scope, &frames[destination], tag),
                    );
                }
            }

            let mut completed = Vec::with_capacity(sends);
            let mut sends_matched = sends == 0;
            let mut barrier: Option<mpi::request::Request<'static, ()>> = None;
            loop {
                // Receive every frame currently deliverable in this epoch's
                // tagged coupling context. Each peer source routes at most one
                // frame to this rank, so placing it by source index is
                // unambiguous.
                while let Some(probe) = self
                    .coupling
                    .any_process()
                    .immediate_matched_probe_with_tag(tag)
                {
                    let source = probe.1.source_rank() - self.peer_root;
                    let (frame, _) = probe.matched_receive_vec::<u8>();
                    match received.get_mut(source as usize) {
                        Some(slot) => *slot = frame,
                        None => malformed = true,
                    }
                }
                match barrier.take() {
                    None => {
                        if !sends_matched && requests.test_all(&mut completed) {
                            sends_matched = true;
                        }
                        if sends_matched {
                            barrier = Some(self.coupling.immediate_barrier());
                        }
                    }
                    Some(request) => match request.test() {
                        Ok(_) => break,
                        Err(pending) => barrier = Some(pending),
                    },
                }
            }
        });
        if malformed {
            return Err(RoutedExchangeError::MalformedFrame);
        }
        Ok(received)
    }

    fn agree_failure(&self, local_failed: bool) -> bool {
        // `coupling` is a duplicate of raw `MPI_COMM_WORLD` and therefore spans
        // every rank of both roles. A logical-OR (max over 0/1) all-reduce lets
        // one rank's fault abort every peer, on a context isolated from both
        // role-local solver communicators.
        let local = u8::from(local_failed);
        let mut any = 0_u8;
        self.coupling
            .all_reduce_into(&local, &mut any, SystemOperation::max());
        any != 0
    }
}

fn encode_frame(epoch: CouplingEpoch, records: &[RoutedPayload]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(
        4 + 1
            + 8
            + 8
            + records
                .iter()
                .map(|r| 4 + 8 + 8 + r.payload.len())
                .sum::<usize>(),
    );
    frame.extend_from_slice(MAGIC);
    frame.push(VERSION);
    frame.extend_from_slice(&epoch.0.to_le_bytes());
    frame.extend_from_slice(&(records.len() as u64).to_le_bytes());
    for record in records {
        frame.extend_from_slice(&record.destination.to_le_bytes());
        frame.extend_from_slice(&record.entity_id.0.to_le_bytes());
        frame.extend_from_slice(&(record.payload.len() as u64).to_le_bytes());
        frame.extend_from_slice(&record.payload);
    }
    frame
}

fn decode_frame(
    frame: &[u8],
    expected_epoch: CouplingEpoch,
) -> Result<Vec<RoutedPayload>, RoutedExchangeError> {
    if frame.get(..4) != Some(MAGIC) || frame.get(4).copied() != Some(VERSION) {
        return Err(RoutedExchangeError::MalformedFrame);
    }
    let mut cursor = 5;
    let actual_epoch = CouplingEpoch(read_u64(frame, &mut cursor)?);
    if actual_epoch != expected_epoch {
        return Err(RoutedExchangeError::EpochMismatch {
            expected: expected_epoch,
            actual: actual_epoch,
        });
    }
    let count = read_u64(frame, &mut cursor)? as usize;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let destination = read_i32(frame, &mut cursor)?;
        let entity_id = EntityId(read_u64(frame, &mut cursor)?);
        let len = read_u64(frame, &mut cursor)? as usize;
        let end = cursor
            .checked_add(len)
            .ok_or(RoutedExchangeError::MalformedFrame)?;
        let payload = frame
            .get(cursor..end)
            .ok_or(RoutedExchangeError::MalformedFrame)?
            .to_vec();
        cursor = end;
        records.push(RoutedPayload::new(destination, entity_id, payload));
    }
    if cursor != frame.len() {
        return Err(RoutedExchangeError::MalformedFrame);
    }
    Ok(records)
}

fn read_u64(frame: &[u8], cursor: &mut usize) -> Result<u64, RoutedExchangeError> {
    let end = cursor
        .checked_add(8)
        .ok_or(RoutedExchangeError::MalformedFrame)?;
    let bytes = frame
        .get(*cursor..end)
        .ok_or(RoutedExchangeError::MalformedFrame)?;
    *cursor = end;
    Ok(u64::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| RoutedExchangeError::MalformedFrame)?,
    ))
}

fn read_i32(frame: &[u8], cursor: &mut usize) -> Result<i32, RoutedExchangeError> {
    let end = cursor
        .checked_add(4)
        .ok_or(RoutedExchangeError::MalformedFrame)?;
    let bytes = frame
        .get(*cursor..end)
        .ok_or(RoutedExchangeError::MalformedFrame)?;
    *cursor = end;
    Ok(i32::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| RoutedExchangeError::MalformedFrame)?,
    ))
}

/// Invalid routed exchange or underlying role transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutedExchangeError {
    /// The opaque routed frame was truncated or had an unknown header.
    MalformedFrame,
    /// A peer participated in a different coupling exchange.
    EpochMismatch {
        /// Epoch requested by this rank.
        expected: CouplingEpoch,
        /// Epoch encoded by the peer.
        actual: CouplingEpoch,
    },
    /// A coupling package addressed a rank outside the destination role.
    DestinationOutOfRange {
        /// Supplied role-local destination rank.
        destination: i32,
        /// Number of ranks in the destination role.
        peer_size: i32,
    },
    /// This rank's round was valid, but a peer rank in the coupled exchange
    /// failed. This rank aborts in lockstep instead of entering the next
    /// collective alone. Inspect the failing peer's log for the root cause.
    PeerAborted,
    /// The correctness-first role exchange failed.
    Role(RoleExchangeError),
}

impl From<RoleExchangeError> for RoutedExchangeError {
    fn from(value: RoleExchangeError) -> Self {
        Self::Role(value)
    }
}

impl fmt::Display for RoutedExchangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedFrame => f.write_str("malformed routed-exchange frame"),
            Self::EpochMismatch { expected, actual } => write!(
                f,
                "coupling epoch mismatch: expected {}, received {}",
                expected.0, actual.0
            ),
            Self::DestinationOutOfRange {
                destination,
                peer_size,
            } => write!(
                f,
                "destination role-rank {destination} is outside peer role size {peer_size}"
            ),
            Self::PeerAborted => f.write_str(
                "coupled routed exchange aborted: a peer rank failed this round; \
                 see that rank's diagnostic for the root cause",
            ),
            Self::Role(error) => write!(f, "role exchange: {error}"),
        }
    }
}

impl std::error::Error for RoutedExchangeError {}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRoleExchange {
        local_rank: i32,
        local_size: i32,
        peer_frames: Vec<Vec<u8>>,
        /// Simulate a peer rank that failed its round so the both-role
        /// agreement reports failure even when this rank's round was valid.
        peer_failed: bool,
    }

    impl FakeRoleExchange {
        fn new(local_rank: i32, local_size: i32, peer_frames: Vec<Vec<u8>>) -> Self {
            Self {
                local_rank,
                local_size,
                peer_frames,
                peer_failed: false,
            }
        }
    }

    impl RoleExchange for FakeRoleExchange {
        fn role_position(&self) -> (i32, i32) {
            (self.local_rank, self.local_size)
        }

        fn peer_size(&self) -> i32 {
            self.peer_frames.len() as i32
        }

        fn exchange(&self, _local: &[u8]) -> Result<Vec<Vec<u8>>, RoleExchangeError> {
            Ok(self.peer_frames.clone())
        }

        fn agree_failure(&self, local_failed: bool) -> bool {
            local_failed || self.peer_failed
        }
    }

    #[test]
    fn filters_for_local_rank_in_deterministic_source_order() {
        let epoch = CouplingEpoch(17);
        let peer_frames = vec![
            encode_frame(
                epoch,
                &[
                    RoutedPayload::new(1, EntityId(10), vec![1]),
                    RoutedPayload::new(0, EntityId(11), vec![2]),
                ],
            ),
            encode_frame(
                epoch,
                &[
                    RoutedPayload::new(2, EntityId(20), vec![3]),
                    RoutedPayload::new(1, EntityId(21), vec![4]),
                ],
            ),
        ];
        let routed = RoutedRoleExchange::new(Box::new(FakeRoleExchange::new(1, 3, peer_frames)));
        let received = routed.exchange(epoch, &[]).unwrap();
        assert_eq!(
            received,
            vec![
                ReceivedPayload {
                    source: 0,
                    entity_id: EntityId(10),
                    payload: vec![1],
                },
                ReceivedPayload {
                    source: 1,
                    entity_id: EntityId(21),
                    payload: vec![4],
                },
            ]
        );
    }

    #[test]
    fn stale_epoch_fails_closed() {
        let frame = encode_frame(CouplingEpoch(4), &[]);
        let error = decode_frame(&frame, CouplingEpoch(5)).unwrap_err();
        assert_eq!(
            error,
            RoutedExchangeError::EpochMismatch {
                expected: CouplingEpoch(5),
                actual: CouplingEpoch(4),
            }
        );
    }

    #[test]
    fn local_destination_is_checked_against_peer_role_size() {
        let epoch = CouplingEpoch(3);
        let routed = RoutedRoleExchange::new(Box::new(FakeRoleExchange::new(
            0,
            1,
            vec![encode_frame(epoch, &[])],
        )));
        let error = routed
            .exchange(epoch, &[RoutedPayload::new(1, EntityId(9), vec![])])
            .unwrap_err();
        assert_eq!(
            error,
            RoutedExchangeError::DestinationOutOfRange {
                destination: 1,
                peer_size: 1,
            }
        );
    }

    #[test]
    fn malformed_frame_fails_closed() {
        let mut frame = encode_frame(
            CouplingEpoch(1),
            &[RoutedPayload::new(0, EntityId(7), vec![1, 2, 3])],
        );
        frame.pop();
        assert_eq!(
            decode_frame(&frame, CouplingEpoch(1)).unwrap_err(),
            RoutedExchangeError::MalformedFrame
        );
    }

    #[test]
    fn locally_valid_round_aborts_when_a_peer_fails() {
        // This rank's own round is entirely valid, but the both-role agreement
        // reports a peer failure. It must abort in lockstep rather than return
        // its (now meaningless) delivery and walk into the next collective.
        let epoch = CouplingEpoch(2);
        let mut fake = FakeRoleExchange::new(0, 1, vec![encode_frame(epoch, &[])]);
        fake.peer_failed = true;
        let routed = RoutedRoleExchange::new(Box::new(fake));
        assert_eq!(
            routed.exchange(epoch, &[]).unwrap_err(),
            RoutedExchangeError::PeerAborted
        );
    }

    #[test]
    fn locally_failed_round_reports_its_own_actionable_error() {
        // When this rank both fails locally and a peer fails, it keeps its own
        // specific diagnostic instead of the generic peer-abort error.
        let epoch = CouplingEpoch(3);
        let mut fake = FakeRoleExchange::new(0, 1, vec![encode_frame(epoch, &[])]);
        fake.peer_failed = true;
        let routed = RoutedRoleExchange::new(Box::new(fake));
        assert_eq!(
            routed
                .exchange(epoch, &[RoutedPayload::new(5, EntityId(1), vec![])])
                .unwrap_err(),
            RoutedExchangeError::DestinationOutOfRange {
                destination: 5,
                peer_size: 1,
            }
        );
    }

    #[test]
    fn sparse_peer_frames_match_root_bridge_delivery_with_source_holes() {
        let epoch = CouplingEpoch(8);
        let root_frames = vec![
            encode_frame(epoch, &[]),
            encode_frame(epoch, &[RoutedPayload::new(0, EntityId(41), vec![4, 1])]),
            encode_frame(epoch, &[]),
            encode_frame(epoch, &[RoutedPayload::new(0, EntityId(43), vec![4, 3])]),
        ];
        // This is the logical peer-frame vector reconstructed by the sparse
        // backend after transmitting only sources 1 and 3.
        let mut sparse_frames = (0..4).map(|_| encode_frame(epoch, &[])).collect::<Vec<_>>();
        sparse_frames[1] = root_frames[1].clone();
        sparse_frames[3] = root_frames[3].clone();
        assert_eq!(
            deliver_frames(sparse_frames, epoch, (0, 1)).unwrap(),
            deliver_frames(root_frames, epoch, (0, 1)).unwrap()
        );
    }
}
