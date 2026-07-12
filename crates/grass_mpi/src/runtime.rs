//! Process-level MPI role topology and bootstrap.
//!
//! A single executable can assign disjoint rank groups to solver roles, split
//! raw `MPI_COMM_WORLD` once, and then hand each solver only its role-local
//! communicator. Cross-role coupling continues to use raw-world ranks through
//! the dedicated coupling layer.

use std::collections::HashSet;
use std::fmt;

/// One solver role and the number of MPI ranks assigned to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleSpec {
    name: String,
    ranks: i32,
}

impl RoleSpec {
    /// Construct a role specification.
    pub fn new(name: impl Into<String>, ranks: i32) -> Self {
        Self {
            name: name.into(),
            ranks,
        }
    }

    /// Stable role name used by configuration and diagnostics.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Number of raw-world ranks assigned to this role.
    pub fn ranks(&self) -> i32 {
        self.ranks
    }
}

/// Ordered, contiguous assignment of raw-world ranks to solver roles.
///
/// Roles receive colors in declaration order. Their rank ranges are also
/// contiguous in declaration order, making the mapping deterministic and easy
/// to audit from configuration alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleTopology {
    roles: Vec<RoleSpec>,
}

impl RoleTopology {
    /// Construct and validate a role topology independent of a particular MPI
    /// world size. World-size equality is checked by [`assign`](Self::assign).
    pub fn new(roles: impl IntoIterator<Item = RoleSpec>) -> Result<Self, RoleTopologyError> {
        let roles: Vec<RoleSpec> = roles.into_iter().collect();
        if roles.is_empty() {
            return Err(RoleTopologyError::NoRoles);
        }
        let mut names = HashSet::new();
        for role in &roles {
            if role.name.trim().is_empty() {
                return Err(RoleTopologyError::EmptyName);
            }
            if role.ranks <= 0 {
                return Err(RoleTopologyError::NonPositiveRankCount {
                    role: role.name.clone(),
                    ranks: role.ranks,
                });
            }
            if !names.insert(role.name.clone()) {
                return Err(RoleTopologyError::DuplicateName(role.name.clone()));
            }
        }
        Ok(Self { roles })
    }

    /// Ordered role specifications.
    pub fn roles(&self) -> &[RoleSpec] {
        &self.roles
    }

    /// Total ranks requested by all roles.
    pub fn required_world_size(&self) -> i32 {
        self.roles.iter().map(RoleSpec::ranks).sum()
    }

    /// Half-open raw-world rank range occupied by the named role, if present.
    ///
    /// Cross-role couplers use this to resolve a peer role's coupling rank
    /// without re-deriving the contiguous declaration-order layout by hand.
    pub fn role_world_range(&self, name: &str) -> Option<std::ops::Range<i32>> {
        let mut start = 0;
        for role in &self.roles {
            let end = start + role.ranks;
            if role.name == name {
                return Some(start..end);
            }
            start = end;
        }
        None
    }

    /// Resolve one raw-world rank into its role, role-local rank, and color.
    pub fn assign(
        &self,
        world_rank: i32,
        world_size: i32,
    ) -> Result<RoleAssignment, RoleTopologyError> {
        let required = self.required_world_size();
        if required != world_size {
            return Err(RoleTopologyError::WorldSizeMismatch {
                configured: required,
                actual: world_size,
            });
        }
        if !(0..world_size).contains(&world_rank) {
            return Err(RoleTopologyError::WorldRankOutOfRange {
                rank: world_rank,
                size: world_size,
            });
        }

        let mut start = 0;
        for (color, role) in self.roles.iter().enumerate() {
            let end = start + role.ranks;
            if (start..end).contains(&world_rank) {
                return Ok(RoleAssignment {
                    name: role.name.clone(),
                    color: color as i32,
                    world_rank,
                    world_size,
                    role_rank: world_rank - start,
                    role_size: role.ranks,
                    world_start: start,
                    world_end: end,
                });
            }
            start = end;
        }
        unreachable!("validated rank must belong to exactly one role")
    }
}

/// This process's resolved solver role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleAssignment {
    name: String,
    color: i32,
    world_rank: i32,
    world_size: i32,
    role_rank: i32,
    role_size: i32,
    world_start: i32,
    world_end: i32,
}

impl RoleAssignment {
    /// Stable role name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// MPI split color assigned in topology declaration order.
    pub fn color(&self) -> i32 {
        self.color
    }

    /// Rank in raw `MPI_COMM_WORLD`.
    pub fn world_rank(&self) -> i32 {
        self.world_rank
    }

    /// Size of raw `MPI_COMM_WORLD`.
    pub fn world_size(&self) -> i32 {
        self.world_size
    }

    /// Rank within this role's solver communicator.
    pub fn role_rank(&self) -> i32 {
        self.role_rank
    }

    /// Size of this role's solver communicator.
    pub fn role_size(&self) -> i32 {
        self.role_size
    }

    /// Half-open raw-world rank range occupied by this role.
    pub fn world_range(&self) -> std::ops::Range<i32> {
        self.world_start..self.world_end
    }
}

/// Invalid role topology or assignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleTopologyError {
    /// No roles were configured.
    NoRoles,
    /// A role name was empty.
    EmptyName,
    /// The same role name appeared more than once.
    DuplicateName(String),
    /// A role requested zero or fewer ranks.
    NonPositiveRankCount {
        /// Role with the invalid count.
        role: String,
        /// Invalid rank count.
        ranks: i32,
    },
    /// Configured rank counts do not equal the launched world size.
    WorldSizeMismatch {
        /// Sum of configured role counts.
        configured: i32,
        /// Launched raw-world size.
        actual: i32,
    },
    /// A requested raw-world rank was invalid.
    WorldRankOutOfRange {
        /// Invalid rank.
        rank: i32,
        /// Raw-world size.
        size: i32,
    },
}

impl fmt::Display for RoleTopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRoles => f.write_str("MPI role topology must contain at least one role"),
            Self::EmptyName => f.write_str("MPI role names must not be empty"),
            Self::DuplicateName(name) => write!(f, "duplicate MPI role name `{name}`"),
            Self::NonPositiveRankCount { role, ranks } => {
                write!(
                    f,
                    "MPI role `{role}` must have at least one rank, got {ranks}"
                )
            }
            Self::WorldSizeMismatch { configured, actual } => write!(
                f,
                "MPI role topology requests {configured} ranks but MPI_COMM_WORLD has {actual}"
            ),
            Self::WorldRankOutOfRange { rank, size } => {
                write!(f, "raw MPI world rank {rank} is outside 0..{size}")
            }
        }
    }
}

impl std::error::Error for RoleTopologyError {}

/// How a single binary should compose its solver roles.
///
/// This is the one declarative knob that distinguishes an in-process
/// composition from an MPI role split. It is read from configuration
/// *before* any App is built, so the composition code downstream never
/// branches on a launch flag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopologyMode {
    /// Pick [`Local`](Self::Local) at world size 1 and [`Split`](Self::Split)
    /// when the launched world exactly matches the configured role ranks.
    /// Any other world size is rejected rather than guessed.
    #[default]
    Auto,
    /// Compose every role inside one process over an in-memory transport.
    /// Rejected when launched with more than one rank, so a stale `local`
    /// config under `mpirun -np N>1` fails closed instead of silently
    /// running N duplicate serial copies.
    Local,
    /// Split raw `MPI_COMM_WORLD` into one role-local communicator per role.
    /// Requires the launched world size to equal the configured rank total.
    Split,
}

/// One declaratively configured role and its rank count.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct RoleConfig {
    /// Stable role name; must match the physics/config tables downstream.
    pub name: String,
    /// Raw-world ranks assigned to this role.
    pub ranks: i32,
}

/// Declarative bootstrap topology, deserialized from a `[topology]` TOML
/// table before any App is constructed.
///
/// ```toml
/// [topology]
/// mode = "auto"          # auto | local | split
/// [[topology.role]]
/// name = "a"
/// ranks = 1
/// [[topology.role]]
/// name = "b"
/// ranks = 1
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Deserialize)]
pub struct TopologyConfig {
    /// Composition mode. Defaults to [`TopologyMode::Auto`].
    #[serde(default)]
    pub mode: TopologyMode,
    /// Named role rank allocations, in declaration (color) order.
    #[serde(default, rename = "role")]
    pub roles: Vec<RoleConfig>,
}

impl TopologyConfig {
    /// Build and validate the [`RoleTopology`] this config declares. Applies
    /// the same name/rank-count rules as [`RoleTopology::new`].
    pub fn topology(&self) -> Result<RoleTopology, RoleTopologyError> {
        RoleTopology::new(
            self.roles
                .iter()
                .map(|role| RoleSpec::new(role.name.clone(), role.ranks)),
        )
    }

    /// Resolve this declaration against the launched raw-world size into a
    /// concrete [`TopologyPlan`]. Fails closed on every ambiguous or
    /// mismatched combination rather than degrading to a default.
    pub fn resolve(&self, world_size: i32) -> Result<TopologyPlan, BootstrapError> {
        let topology = self.topology()?;
        let required = topology.required_world_size();
        match self.mode {
            TopologyMode::Local => {
                if world_size != 1 {
                    return Err(BootstrapError::LocalRequiresSingleRank { world_size });
                }
                Ok(TopologyPlan::Local(topology))
            }
            TopologyMode::Split => {
                if world_size != required {
                    return Err(BootstrapError::WorldSizeMismatch {
                        configured: required,
                        actual: world_size,
                    });
                }
                Ok(TopologyPlan::Split(topology))
            }
            TopologyMode::Auto => {
                if world_size == 1 {
                    Ok(TopologyPlan::Local(topology))
                } else if world_size == required {
                    Ok(TopologyPlan::Split(topology))
                } else {
                    Err(BootstrapError::AutoAmbiguous {
                        required,
                        actual: world_size,
                    })
                }
            }
        }
    }
}

/// The resolved composition a binary should perform for the launched world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopologyPlan {
    /// Compose every role in this one process over an in-memory transport.
    Local(RoleTopology),
    /// Split raw `MPI_COMM_WORLD`; this process owns exactly one role.
    Split(RoleTopology),
}

/// A stable, process-local digest of configuration bytes.
///
/// Used to cross-check that every rank of a single-binary launch parsed an
/// identical configuration. `std`'s `DefaultHasher` uses fixed keys, so the
/// same bytes hash to the same value on every rank of the same executable.
pub fn config_digest(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Declarative bootstrap resolution failure. Every variant is a fail-closed
/// rejection: the binary refuses to compose rather than guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootstrapError {
    /// The declared roles are themselves invalid.
    Topology(RoleTopologyError),
    /// `mode = "local"` but the launch has more than one rank.
    LocalRequiresSingleRank {
        /// Launched raw-world size.
        world_size: i32,
    },
    /// `mode = "split"` but the launch size does not match the role total.
    WorldSizeMismatch {
        /// Sum of configured role ranks.
        configured: i32,
        /// Launched raw-world size.
        actual: i32,
    },
    /// `mode = "auto"` and the launch matched neither local (1) nor the
    /// configured split total.
    AutoAmbiguous {
        /// Sum of configured role ranks.
        required: i32,
        /// Launched raw-world size.
        actual: i32,
    },
    /// Ranks disagreed on the parsed configuration digest.
    ConfigMismatch,
}

impl From<RoleTopologyError> for BootstrapError {
    fn from(value: RoleTopologyError) -> Self {
        Self::Topology(value)
    }
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Topology(error) => error.fmt(f),
            Self::LocalRequiresSingleRank { world_size } => write!(
                f,
                "topology mode `local` requires a single rank, but the launch has {world_size}"
            ),
            Self::WorldSizeMismatch { configured, actual } => write!(
                f,
                "topology mode `split` requests {configured} ranks but MPI_COMM_WORLD has {actual}"
            ),
            Self::AutoAmbiguous { required, actual } => write!(
                f,
                "topology mode `auto` cannot resolve world size {actual}: expected 1 (local) or \
                 {required} (split)"
            ),
            Self::ConfigMismatch => f.write_str(
                "ranks parsed different bootstrap configurations; every rank must read an \
                 identical topology",
            ),
        }
    }
}

impl std::error::Error for BootstrapError {}

/// Process-owned role bootstrap for a single-binary split MPI run.
///
/// Construct this before any solver plugin calls `get_mpi_world()`. The split
/// makes that accessor return this process's role-local solver communicator.
/// Call [`finalize`](Self::finalize) only after every App and communication
/// resource has been dropped.
#[cfg(feature = "mpi_backend")]
pub struct MpiRuntime {
    topology: RoleTopology,
    assignment: RoleAssignment,
}

/// The composition a single-binary launch resolved to from its declarative
/// [`TopologyConfig`]. The caller matches on this once, at startup, and the
/// downstream App composition never re-checks how it was launched.
#[cfg(feature = "mpi_backend")]
pub enum Bootstrap {
    /// This process should compose every role locally over an in-memory
    /// transport. Carries the validated topology for role enumeration.
    Local {
        /// The validated role layout.
        topology: RoleTopology,
    },
    /// Raw `MPI_COMM_WORLD` was split and this process owns exactly one role.
    Split {
        /// The initialized role runtime for this process.
        runtime: MpiRuntime,
    },
}

#[cfg(feature = "mpi_backend")]
impl MpiRuntime {
    /// Initialize MPI as needed, resolve this process's role, and split raw
    /// `MPI_COMM_WORLD` into the role-local solver communicator.
    pub fn initialize(topology: RoleTopology) -> Result<Self, MpiRuntimeError> {
        let world_rank = crate::world_rank();
        let world_size = crate::world_size();
        let assignment = topology.assign(world_rank, world_size)?;
        crate::try_init_app_color(assignment.color())?;
        Ok(Self {
            topology,
            assignment,
        })
    }

    /// Resolve a declarative [`TopologyConfig`] against the launched raw-world
    /// size and return the composition this process should perform.
    ///
    /// `config_digest` is a digest of the exact configuration bytes this rank
    /// parsed (see [`config_digest`]). For a split launch every rank's digest
    /// is compared with a collective over raw `MPI_COMM_WORLD`; a disagreement
    /// is rejected before any communicator is split. All ranks must call this,
    /// as the identity check is a collective.
    pub fn bootstrap(
        config: &TopologyConfig,
        config_digest: u64,
    ) -> Result<Bootstrap, MpiRuntimeError> {
        let world_size = crate::world_size();
        match config.resolve(world_size)? {
            TopologyPlan::Local(topology) => Ok(Bootstrap::Local { topology }),
            TopologyPlan::Split(topology) => {
                // Cross-rank config identity, before the world is split.
                if !crate::all_uniform_u64(config_digest) {
                    return Err(MpiRuntimeError::Bootstrap(BootstrapError::ConfigMismatch));
                }
                let world_rank = crate::world_rank();
                let assignment = topology.assign(world_rank, world_size)?;
                crate::try_init_app_color(assignment.color())?;
                Ok(Bootstrap::Split {
                    runtime: Self {
                        topology,
                        assignment,
                    },
                })
            }
        }
    }

    /// Complete configured topology.
    pub fn topology(&self) -> &RoleTopology {
        &self.topology
    }

    /// This process's role assignment.
    pub fn assignment(&self) -> &RoleAssignment {
        &self.assignment
    }

    /// Construct a communication backend over this role's solver communicator.
    pub fn solver_backend(&self) -> crate::MpiCommBackend {
        crate::MpiCommBackend::new(crate::get_mpi_world())
    }

    /// Return this role's solver communicator as an MPI Fortran handle.
    ///
    /// A Fortran handle is the portable integer representation intended for
    /// crossing an FFI boundary. Native libraries should recover the C
    /// communicator with `MPI_Comm_f2c` rather than assuming `MPI_Comm` is an
    /// integer or reusing `MPI_COMM_WORLD`.
    pub fn solver_comm_fortran_handle(&self) -> std::os::raw::c_int {
        crate::get_mpi_world_fortran_handle()
    }

    /// Finalize MPI after all Apps and communicator-backed resources have been
    /// dropped. Consumes the runtime to prevent accidental reuse.
    pub fn finalize(self) {
        crate::finalize_mpi();
    }
}

/// Failure to validate or initialize the process-level MPI role runtime.
#[cfg(feature = "mpi_backend")]
#[derive(Debug)]
pub enum MpiRuntimeError {
    /// Declarative bootstrap resolution or role validation failure.
    Bootstrap(BootstrapError),
    /// MPI communicator split lifecycle violation.
    Init(crate::InitAppColorError),
}

#[cfg(feature = "mpi_backend")]
impl fmt::Display for MpiRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bootstrap(error) => error.fmt(f),
            Self::Init(error) => error.fmt(f),
        }
    }
}

#[cfg(feature = "mpi_backend")]
impl std::error::Error for MpiRuntimeError {}

#[cfg(feature = "mpi_backend")]
impl From<BootstrapError> for MpiRuntimeError {
    fn from(value: BootstrapError) -> Self {
        Self::Bootstrap(value)
    }
}

#[cfg(feature = "mpi_backend")]
impl From<RoleTopologyError> for MpiRuntimeError {
    fn from(value: RoleTopologyError) -> Self {
        Self::Bootstrap(BootstrapError::Topology(value))
    }
}

#[cfg(feature = "mpi_backend")]
impl From<crate::InitAppColorError> for MpiRuntimeError {
    fn from(value: crate::InitAppColorError) -> Self {
        Self::Init(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topology() -> RoleTopology {
        RoleTopology::new([RoleSpec::new("dem", 2), RoleSpec::new("cfd", 3)]).unwrap()
    }

    #[test]
    fn contiguous_role_assignment_is_deterministic() {
        let topology = topology();
        assert_eq!(topology.assign(0, 5).unwrap().name(), "dem");
        assert_eq!(topology.assign(1, 5).unwrap().role_rank(), 1);
        let cfd = topology.assign(2, 5).unwrap();
        assert_eq!(cfd.name(), "cfd");
        assert_eq!(cfd.color(), 1);
        assert_eq!(cfd.role_rank(), 0);
        assert_eq!(cfd.role_size(), 3);
        assert_eq!(cfd.world_range(), 2..5);
    }

    #[test]
    fn invalid_topologies_fail_before_mpi_split() {
        assert_eq!(
            RoleTopology::new([]).unwrap_err(),
            RoleTopologyError::NoRoles
        );
        assert_eq!(
            RoleTopology::new([RoleSpec::new("dem", 1), RoleSpec::new("dem", 1)]).unwrap_err(),
            RoleTopologyError::DuplicateName("dem".into())
        );
        assert!(matches!(
            topology().assign(0, 4),
            Err(RoleTopologyError::WorldSizeMismatch { .. })
        ));
    }

    #[test]
    fn role_world_range_resolves_peer_ranks() {
        let topology = topology();
        assert_eq!(topology.role_world_range("dem"), Some(0..2));
        assert_eq!(topology.role_world_range("cfd"), Some(2..5));
        assert_eq!(topology.role_world_range("missing"), None);
    }

    fn pair_config(mode: TopologyMode) -> TopologyConfig {
        TopologyConfig {
            mode,
            roles: vec![
                RoleConfig {
                    name: "a".into(),
                    ranks: 1,
                },
                RoleConfig {
                    name: "b".into(),
                    ranks: 1,
                },
            ],
        }
    }

    #[test]
    fn topology_config_parses_from_toml() {
        let parsed: TopologyConfig = toml::from_str(
            r#"
            mode = "split"
            [[role]]
            name = "a"
            ranks = 1
            [[role]]
            name = "b"
            ranks = 1
            "#,
        )
        .unwrap();
        assert_eq!(parsed, pair_config(TopologyMode::Split));
        assert_eq!(parsed.topology().unwrap().required_world_size(), 2);
    }

    #[test]
    fn auto_mode_selects_by_world_size() {
        let config = pair_config(TopologyMode::Auto);
        assert!(matches!(config.resolve(1), Ok(TopologyPlan::Local(_))));
        assert!(matches!(config.resolve(2), Ok(TopologyPlan::Split(_))));
        assert_eq!(
            config.resolve(3),
            Err(BootstrapError::AutoAmbiguous {
                required: 2,
                actual: 3,
            })
        );
    }

    #[test]
    fn explicit_modes_fail_closed_on_mismatch() {
        assert_eq!(
            pair_config(TopologyMode::Local).resolve(2),
            Err(BootstrapError::LocalRequiresSingleRank { world_size: 2 })
        );
        assert_eq!(
            pair_config(TopologyMode::Split).resolve(1),
            Err(BootstrapError::WorldSizeMismatch {
                configured: 2,
                actual: 1,
            })
        );
        assert!(matches!(
            pair_config(TopologyMode::Split).resolve(2),
            Ok(TopologyPlan::Split(_))
        ));
    }

    #[test]
    fn config_digest_is_stable_and_content_sensitive() {
        assert_eq!(config_digest(b"topology"), config_digest(b"topology"));
        assert_ne!(config_digest(b"topology"), config_digest(b"topolog"));
    }
}
