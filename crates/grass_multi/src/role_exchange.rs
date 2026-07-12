//! Correctness-first exchange of variable-sized interface shards between MPI roles.

use crate::{LocalTransport, Transport, TransportError, TransportOperation};
use mpi::topology::SimpleCommunicator;
use mpi::traits::{Communicator, Destination, Source};
use std::fmt;
use std::sync::Mutex;

/// Collective cross-role exchange. Every rank in a role contributes one shard
/// and receives every peer-role shard in deterministic role-rank order.
pub trait RoleExchange: Send + Sync + 'static {
    /// Rank and size within the local solver role.
    fn role_position(&self) -> (i32, i32);
    /// Number of shards expected from the peer role.
    fn peer_size(&self) -> i32;
    /// Collectively exchange one local interface shard for all peer shards.
    fn exchange(&self, local: &[u8]) -> Result<Vec<Vec<u8>>, RoleExchangeError>;
}

pub(crate) struct LocalRoleExchange(LocalTransport);

impl LocalRoleExchange {
    pub(crate) fn pair() -> (Box<dyn RoleExchange>, Box<dyn RoleExchange>) {
        let (first, second) = LocalTransport::pair();
        (Box::new(Self(first)), Box::new(Self(second)))
    }
}

impl RoleExchange for LocalRoleExchange {
    fn role_position(&self) -> (i32, i32) {
        (0, 1)
    }
    fn peer_size(&self) -> i32 {
        1
    }
    fn exchange(&self, local: &[u8]) -> Result<Vec<Vec<u8>>, RoleExchangeError> {
        self.0.send(local);
        Ok(vec![self.0.recv()])
    }
}

/// Root-bridge implementation: gather locally, exchange between role roots,
/// then broadcast the complete peer shard set within each role.
pub struct MpiRoleExchange {
    role: SimpleCommunicator,
    coupling: SimpleCommunicator,
    peer_root: i32,
    peer_size: i32,
}

unsafe impl Send for MpiRoleExchange {}
unsafe impl Sync for MpiRoleExchange {}

impl MpiRoleExchange {
    /// Build from the already-split role communicator and peer role metadata.
    pub fn new(peer_root: i32, peer_size: i32) -> Self {
        Self {
            role: grass_mpi::get_mpi_world(),
            // Every rank constructs the runner at the same bootstrap point, so
            // this collective duplicate creates an isolated message context.
            // Solver traffic on either role communicator cannot match coupling
            // traffic even when libraries use MPI's default tag.
            coupling: grass_mpi::get_mpi_world_raw().duplicate(),
            peer_root,
            peer_size,
        }
    }
}

impl RoleExchange for MpiRoleExchange {
    fn role_position(&self) -> (i32, i32) {
        (self.role.rank(), self.role.size())
    }

    fn peer_size(&self) -> i32 {
        self.peer_size
    }

    fn exchange(&self, local: &[u8]) -> Result<Vec<Vec<u8>>, RoleExchangeError> {
        let rank = self.role.rank();
        let size = self.role.size();
        if rank != 0 {
            self.role.process_at_rank(0).send(local);
            let (peer_frame, _) = self.role.process_at_rank(0).receive_vec::<u8>();
            return decode_shards(&peer_frame, self.peer_size);
        }

        let mut local_shards = Vec::with_capacity(size as usize);
        local_shards.push(local.to_vec());
        for source in 1..size {
            let (shard, _) = self.role.process_at_rank(source).receive_vec::<u8>();
            local_shards.push(shard);
        }
        let local_frame = encode_shards(&local_shards);
        let peer_frame = mpi::request::scope(|scope| {
            let request = self
                .coupling
                .process_at_rank(self.peer_root)
                .immediate_send(scope, &local_frame);
            let (frame, _) = self
                .coupling
                .process_at_rank(self.peer_root)
                .receive_vec::<u8>();
            request.wait();
            frame
        });
        let peer_shards = decode_shards(&peer_frame, self.peer_size)?;
        for destination in 1..size {
            self.role.process_at_rank(destination).send(&peer_frame);
        }
        Ok(peer_shards)
    }
}

fn encode_shards(shards: &[Vec<u8>]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(8 + shards.iter().map(|s| 8 + s.len()).sum::<usize>());
    frame.extend_from_slice(&(shards.len() as u64).to_le_bytes());
    for shard in shards {
        frame.extend_from_slice(&(shard.len() as u64).to_le_bytes());
        frame.extend_from_slice(shard);
    }
    frame
}

fn decode_shards(frame: &[u8], expected: i32) -> Result<Vec<Vec<u8>>, RoleExchangeError> {
    let mut cursor = 0;
    let count = read_u64(frame, &mut cursor)? as usize;
    if count != expected as usize {
        return Err(RoleExchangeError::ShardCount {
            expected,
            actual: count,
        });
    }
    let mut shards = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_u64(frame, &mut cursor)? as usize;
        let end = cursor
            .checked_add(len)
            .ok_or(RoleExchangeError::MalformedFrame)?;
        let bytes = frame
            .get(cursor..end)
            .ok_or(RoleExchangeError::MalformedFrame)?;
        shards.push(bytes.to_vec());
        cursor = end;
    }
    if cursor != frame.len() {
        return Err(RoleExchangeError::MalformedFrame);
    }
    Ok(shards)
}

fn read_u64(frame: &[u8], cursor: &mut usize) -> Result<u64, RoleExchangeError> {
    let end = cursor
        .checked_add(8)
        .ok_or(RoleExchangeError::MalformedFrame)?;
    let bytes: [u8; 8] = frame
        .get(*cursor..end)
        .ok_or(RoleExchangeError::MalformedFrame)?
        .try_into()
        .map_err(|_| RoleExchangeError::MalformedFrame)?;
    *cursor = end;
    Ok(u64::from_le_bytes(bytes))
}

/// Adapts a role exchange to the legacy one-peer `Transport` contract. It is
/// intentionally valid only when the peer role has one rank.
pub struct SinglePeerTransport {
    exchange: Box<dyn RoleExchange>,
    pending: Mutex<Option<Vec<u8>>>,
}

impl SinglePeerTransport {
    /// Wrap a collective role exchange for a one-rank peer.
    pub fn new(exchange: Box<dyn RoleExchange>) -> Self {
        Self {
            exchange,
            pending: Mutex::new(None),
        }
    }
}

impl Transport for SinglePeerTransport {
    fn transport_name(&self) -> &str {
        "SinglePeerTransport"
    }

    fn try_send(&self, payload: &[u8]) -> Result<(), TransportError> {
        *self.pending.lock().unwrap() = Some(payload.to_vec());
        Ok(())
    }

    fn try_recv(&self) -> Result<Vec<u8>, TransportError> {
        let local = self.pending.lock().unwrap().take().ok_or_else(|| {
            TransportError::new(
                self.transport_name(),
                TransportOperation::Recv,
                "recv called before send",
            )
        })?;
        let mut peers = self.exchange.exchange(&local).map_err(|error| {
            TransportError::new(
                self.transport_name(),
                TransportOperation::Recv,
                error.to_string(),
            )
        })?;
        if peers.len() != 1 {
            return Err(TransportError::new(
                self.transport_name(),
                TransportOperation::Recv,
                format!("legacy Transport requires one peer shard, received {}; use RoleExchange for multi-rank coupling", peers.len()),
            ));
        }
        Ok(peers.remove(0))
    }
}

/// Invalid framed role exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleExchangeError {
    /// Frame did not contain complete length-prefixed shards.
    MalformedFrame,
    /// Peer root sent a shard count inconsistent with topology.
    ShardCount {
        /// Peer-role size declared by the validated topology.
        expected: i32,
        /// Shards actually present in the received frame.
        actual: usize,
    },
}

impl fmt::Display for RoleExchangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedFrame => f.write_str("malformed role-shard frame"),
            Self::ShardCount { expected, actual } => {
                write!(f, "expected {expected} peer shards, received {actual}")
            }
        }
    }
}

impl std::error::Error for RoleExchangeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_shards_round_trip_in_rank_order() {
        let shards = vec![vec![], vec![1], vec![2, 3, 4], vec![5; 1024]];
        assert_eq!(decode_shards(&encode_shards(&shards), 4).unwrap(), shards);
    }

    #[test]
    fn shard_count_mismatch_fails_closed() {
        let error = decode_shards(&encode_shards(&[vec![1]]), 2).unwrap_err();
        assert_eq!(
            error,
            RoleExchangeError::ShardCount {
                expected: 2,
                actual: 1
            }
        );
    }
}
