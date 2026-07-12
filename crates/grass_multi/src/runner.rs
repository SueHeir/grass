//! Declarative process runner for a locally composed or MPI-split solver pair.

use crate::{LocalTransport, MpiInterCommTransport, Transport};
use grass_mpi::{config_digest, Bootstrap, MpiRuntime, RoleTopology, TopologyConfig};
use serde::Deserialize;
use std::fmt;

/// Everything role code needs after GRASS has resolved process placement.
pub struct RoleLaunch {
    role: String,
    peer: String,
    config_source: String,
    transport: Box<dyn Transport>,
}

impl RoleLaunch {
    /// Name of the solver role executing on this process.
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Name of the role at the other end of the coupling channel.
    pub fn peer(&self) -> &str {
        &self.peer
    }

    /// Complete input document. A role builder can seed its ordinary
    /// `grass_io::Config` from these same validated bytes.
    pub fn config_source(&self) -> &str {
        &self.config_source
    }

    /// Consume the launch and recover its already-selected transport.
    pub fn into_transport(self) -> Box<dyn Transport> {
        self.transport
    }

    /// Split the launch into its input document and transport.
    pub fn into_parts(self) -> (String, Box<dyn Transport>) {
        (self.config_source, self.transport)
    }
}

/// Result placement from [`CoupledPairRunner::run`].
#[derive(Debug)]
pub enum PairRun<T> {
    /// Both roles executed in this process.
    Local {
        /// Result returned by the first declared role.
        first: T,
        /// Result returned by the second declared role.
        second: T,
    },
    /// This MPI process executed its one assigned role.
    Split {
        /// Name of the role assigned to this process.
        role: String,
        /// Result returned by that role.
        result: T,
    },
}

/// Process-level runner that turns one input document into either two local
/// solver roles or one MPI-assigned role per process.
///
/// This is deliberately above `App`: topology and MPI must be resolved before
/// GRASS knows which solver App belongs on the current process.
pub struct CoupledPairRunner {
    source: String,
    topology: TopologyConfig,
    first: String,
    second: String,
}

impl CoupledPairRunner {
    /// Load an optional path from the first CLI argument, falling back to an
    /// embedded/default document when no path is supplied.
    pub fn from_cli_or(default_source: &str) -> Result<Self, RunnerError> {
        let source = match std::env::args().nth(1) {
            Some(path) => std::fs::read_to_string(&path)
                .map_err(|source| RunnerError::ReadConfig { path, source })?,
            None => default_source.to_owned(),
        };
        Self::from_str(&source)
    }

    /// Parse a complete input document and validate that it declares exactly
    /// two roles. Role order is the topology declaration order.
    pub fn from_str(source: &str) -> Result<Self, RunnerError> {
        #[derive(Deserialize)]
        struct BootstrapDocument {
            topology: TopologyConfig,
        }
        let topology = toml::from_str::<BootstrapDocument>(source)
            .map_err(|error| RunnerError::Config(error.to_string()))?
            .topology;
        let validated = topology
            .topology()
            .map_err(|error| RunnerError::Config(error.to_string()))?;
        let [first, second] = validated.roles() else {
            return Err(RunnerError::RoleCount(validated.roles().len()));
        };
        Ok(Self {
            source: source.to_owned(),
            topology,
            first: first.name().to_owned(),
            second: second.name().to_owned(),
        })
    }

    /// Run the registered role functions using the topology-selected local or
    /// split placement. GRASS owns transport construction and MPI lifecycle.
    pub fn run<T, First, Second>(
        self,
        first: First,
        second: Second,
    ) -> Result<PairRun<T>, RunnerError>
    where
        T: Send + 'static,
        First: FnOnce(RoleLaunch) -> T + Send + 'static,
        Second: FnOnce(RoleLaunch) -> T + Send + 'static,
    {
        let digest = config_digest(self.source.as_bytes());
        match MpiRuntime::bootstrap(&self.topology, digest)
            .map_err(|error| RunnerError::Bootstrap(error.to_string()))?
        {
            Bootstrap::Local { topology } => self.run_local(topology, first, second),
            Bootstrap::Split { runtime } => self.run_split(runtime, first, second),
        }
    }

    fn run_local<T, First, Second>(
        self,
        _topology: RoleTopology,
        first: First,
        second: Second,
    ) -> Result<PairRun<T>, RunnerError>
    where
        T: Send + 'static,
        First: FnOnce(RoleLaunch) -> T + Send + 'static,
        Second: FnOnce(RoleLaunch) -> T + Send + 'static,
    {
        let (first_transport, second_transport) = LocalTransport::pair();
        let first_launch = self.launch(&self.first, &self.second, first_transport);
        let second_launch = self.launch(&self.second, &self.first, second_transport);
        let first_thread = std::thread::spawn(move || first(first_launch));
        let second_thread = std::thread::spawn(move || second(second_launch));
        let first_result = first_thread.join();
        let second_result = second_thread.join();
        grass_mpi::finalize_mpi();
        let first = first_result.map_err(|_| RunnerError::RolePanicked {
            role: self.first.clone(),
        })?;
        let second = second_result.map_err(|_| RunnerError::RolePanicked {
            role: self.second.clone(),
        })?;
        Ok(PairRun::Local { first, second })
    }

    fn run_split<T, First, Second>(
        self,
        runtime: MpiRuntime,
        first: First,
        second: Second,
    ) -> Result<PairRun<T>, RunnerError>
    where
        T: Send + 'static,
        First: FnOnce(RoleLaunch) -> T,
        Second: FnOnce(RoleLaunch) -> T,
    {
        let role = runtime.assignment().name().to_owned();
        if runtime.assignment().role_size() != 1 {
            let error = RunnerError::MultiRankRole {
                role,
                ranks: runtime.assignment().role_size(),
            };
            runtime.finalize();
            return Err(error);
        }
        let (peer, run): (&str, Box<dyn FnOnce(RoleLaunch) -> T>) = if role == self.first {
            (&self.second, Box::new(first))
        } else if role == self.second {
            (&self.first, Box::new(second))
        } else {
            let error = RunnerError::UnknownRole(role);
            runtime.finalize();
            return Err(error);
        };
        let peer_rank = runtime
            .topology()
            .role_world_range(peer)
            .expect("validated peer role")
            .start;
        let launch = self.launch(&role, peer, MpiInterCommTransport::new(peer_rank));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(launch)));
        runtime.finalize();
        match result {
            Ok(result) => Ok(PairRun::Split { role, result }),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn launch(&self, role: &str, peer: &str, transport: impl Transport) -> RoleLaunch {
        RoleLaunch {
            role: role.to_owned(),
            peer: peer.to_owned(),
            config_source: self.source.clone(),
            transport: Box::new(transport),
        }
    }
}

/// Failure before or around execution of a declarative coupled pair.
#[derive(Debug)]
pub enum RunnerError {
    /// A CLI-supplied input file could not be read.
    ReadConfig {
        /// Requested path.
        path: String,
        /// Filesystem error.
        source: std::io::Error,
    },
    /// The input document or topology was invalid.
    Config(String),
    /// A coupled pair requires exactly two declared roles.
    RoleCount(usize),
    /// MPI/topology bootstrap failed.
    Bootstrap(String),
    /// The current point-to-point runner cannot map a multi-rank role yet.
    MultiRankRole {
        /// Role name.
        role: String,
        /// Configured ranks.
        ranks: i32,
    },
    /// Bootstrap returned a role outside the validated pair.
    UnknownRole(String),
    /// A locally composed role panicked.
    RolePanicked {
        /// Failed role name.
        role: String,
    },
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadConfig { path, source } => write!(f, "read config `{path}`: {source}"),
            Self::Config(error) => write!(f, "invalid coupled-runner config: {error}"),
            Self::RoleCount(count) => write!(f, "coupled pair requires exactly two roles, got {count}"),
            Self::Bootstrap(error) => write!(f, "bootstrap coupled topology: {error}"),
            Self::MultiRankRole { role, ranks } => write!(f, "role `{role}` has {ranks} ranks; paired point-to-point coupling currently requires one"),
            Self::UnknownRole(role) => write!(f, "bootstrap selected unknown role `{role}`"),
            Self::RolePanicked { role } => write!(f, "local role `{role}` panicked"),
        }
    }
}

impl std::error::Error for RunnerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_declared_pair_in_topology_order() {
        let runner = CoupledPairRunner::from_str(
            r#"
                [topology]
                mode = "auto"
                [[topology.role]]
                name = "fluid"
                ranks = 1
                [[topology.role]]
                name = "material"
                ranks = 1
            "#,
        )
        .unwrap();
        assert_eq!(runner.first, "fluid");
        assert_eq!(runner.second, "material");
    }

    #[test]
    fn rejects_non_pair_topology_before_mpi_bootstrap() {
        let error = CoupledPairRunner::from_str(
            r#"
                [topology]
                [[topology.role]]
                name = "only"
                ranks = 1
            "#,
        )
        .err()
        .expect("one role must be rejected");
        assert!(matches!(error, RunnerError::RoleCount(1)));
    }
}
