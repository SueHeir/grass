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
    /// Invalid role topology for this MPI launch.
    Topology(RoleTopologyError),
    /// MPI communicator bootstrap lifecycle violation.
    Bootstrap(crate::InitAppColorError),
}

#[cfg(feature = "mpi_backend")]
impl fmt::Display for MpiRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Topology(error) => error.fmt(f),
            Self::Bootstrap(error) => error.fmt(f),
        }
    }
}

#[cfg(feature = "mpi_backend")]
impl std::error::Error for MpiRuntimeError {}

#[cfg(feature = "mpi_backend")]
impl From<RoleTopologyError> for MpiRuntimeError {
    fn from(value: RoleTopologyError) -> Self {
        Self::Topology(value)
    }
}

#[cfg(feature = "mpi_backend")]
impl From<crate::InitAppColorError> for MpiRuntimeError {
    fn from(value: crate::InitAppColorError) -> Self {
        Self::Bootstrap(value)
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
}
