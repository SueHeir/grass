//! Geometry-agnostic routed exchange between solver roles.
//!
//! A coupling package decides each record's destination role-rank. GRASS only
//! frames, transports, validates, and deterministically delivers those opaque
//! records. Split MPI runs send non-empty frames directly between owner ranks;
//! the [`RoleExchange`] root bridge remains the local-mode implementation and
//! correctness oracle.

use crate::{RoleExchange, RoleExchangeError};
use mpi::collective::CommunicatorCollectives;
use mpi::topology::SimpleCommunicator;
use mpi::traits::{Communicator, Destination, Source};
use std::fmt;

const MAGIC: &[u8; 4] = b"GRRT";
const VERSION: u8 = 1;

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
    pub fn exchange(
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
        let world_size = self.coupling.size() as usize;
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
        let mut send_lengths = vec![0_u64; world_size];
        for (destination, records) in by_destination.iter().enumerate() {
            if !records.is_empty() {
                send_lengths[self.peer_root as usize + destination] =
                    frames[destination].len() as u64;
            }
        }
        let mut receive_lengths = vec![0_u64; world_size];
        self.coupling
            .all_to_all_into(&send_lengths, &mut receive_lengths);

        let sends = by_destination
            .iter()
            .filter(|records| !records.is_empty())
            .count();
        // Retain one logical frame per peer source so the public delivery
        // order and source identifiers exactly match the root-bridge oracle,
        // while transmitting no empty payload frames on the wire.
        let mut received: Vec<Result<Vec<u8>, RoutedExchangeError>> = (0..self.peer_size)
            .map(|_| Ok(encode_frame(epoch, &[])))
            .collect();
        mpi::request::multiple_scope(sends, |scope, requests| {
            for (destination, records) in by_destination.iter().enumerate() {
                if !records.is_empty() {
                    requests.add(
                        self.coupling
                            .process_at_rank(self.peer_root + destination as i32)
                            .immediate_send(scope, &frames[destination]),
                    );
                }
            }
            for source in 0..self.peer_size {
                let world_source = self.peer_root + source;
                let expected = receive_lengths[world_source as usize];
                if expected != 0 {
                    let (frame, _) = self
                        .coupling
                        .process_at_rank(world_source)
                        .receive_vec::<u8>();
                    if frame.len() as u64 != expected {
                        received[source as usize] = Err(RoutedExchangeError::MalformedFrame);
                    } else {
                        received[source as usize] = Ok(frame);
                    }
                }
            }
            let mut completed = Vec::with_capacity(sends);
            requests.wait_all(&mut completed);
        });
        received.into_iter().collect()
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
        let routed = RoutedRoleExchange::new(Box::new(FakeRoleExchange {
            local_rank: 1,
            local_size: 3,
            peer_frames,
        }));
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
        let routed = RoutedRoleExchange::new(Box::new(FakeRoleExchange {
            local_rank: 0,
            local_size: 1,
            peer_frames: vec![encode_frame(epoch, &[])],
        }));
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
