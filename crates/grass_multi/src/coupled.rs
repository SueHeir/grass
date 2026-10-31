//! Any number of coupled solver roles, in one process or split over MPI, with
//! implicit (iterated) coupling across them.
//!
//! [`CoupledPairRunner`](crate::CoupledPairRunner) places exactly two roles.
//! [`CoupledRunner`] places any number. The `[topology]` table lists the
//! roles and their rank counts. The same input runs every role on its own
//! thread in one process (`mode = "local"`), or splits `MPI_COMM_WORLD` so
//! each role owns its ranks and its own solver communicator
//! (`mode = "split"`, `mpirun -np <sum of ranks>`).
//!
//! ```toml
//! [topology]
//! mode = "auto"           # local at world size 1, split at the declared size
//! [[topology.role]]
//! name = "flow"
//! ranks = 4
//! [[topology.role]]
//! name = "tps"
//! ranks = 2
//! [[topology.role]]
//! name = "structure"
//! ranks = 1
//! ```
//!
//! ```rust,ignore
//! CoupledRunner::from_cli_or(DEFAULT)?
//!     .role("flow", run_flow)
//!     .role("tps", run_tps)
//!     .role("structure", run_structure)
//!     .run()?;
//! ```
//!
//! Each role function receives a [`RoleContext`]:
//!
//! * [`RoleContext::exchange`] / [`routed_exchange`](RoleContext::routed_exchange)
//!   to any peer role. Every pair of roles gets its own MPI communicator
//!   spanning only those two roles, so a pair's collectives (including the
//!   coordinated failure check) never involve a third role, and traffic on
//!   one pair can never match traffic on another.
//! * [`RoleContext::group`]: a [`CouplingGroup`] spanning every rank of
//!   every role, for decisions the whole coupled run must agree on.
//! * [`RoleContext::role_collective`]: gather/scatter inside this role.
//!
//! [`DistributedImplicit`] uses the last two to run windowed implicit
//! coupling (checkpoint, iterate, accelerate with IQN-ILS or Aitken, accept)
//! across roles that live on different ranks.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Barrier, Mutex};

use grass_mpi::{config_digest, Bootstrap, MpiRuntime, RoleTopology, TopologyConfig};
use mpi::collective::{CommunicatorCollectives, Root, SystemOperation};
use mpi::topology::{Color, Communicator, SimpleCommunicator};
use mpi::traits::{Destination, Source};
use serde::Deserialize;

use crate::implicit::WindowStep;
use crate::relax::OuterIteration;
use crate::role_exchange::LocalRoleExchange;
use crate::{
    MpiRoleExchange, RoleExchange, RoutedRoleExchange, RunnerError, SinglePeerTransport, Transport,
};

// ─── groups ─────────────────────────────────────────────────────────────────

/// Collectives over every rank of every role. All calls are collective: every
/// rank of every role must make the same calls in the same order.
pub trait CouplingGroup: Send + Sync {
    /// Number of roles.
    fn roles(&self) -> usize;
    /// Index (declaration order) of this process's role.
    fn role_index(&self) -> usize;
    /// Rank 0 of role `root_role` supplies `data`; every rank receives it.
    fn broadcast(&self, root_role: usize, data: &[u8]) -> Vec<u8>;
    /// `true` on every rank if any rank passed `true`.
    fn any(&self, flag: bool) -> bool;
}

/// Gather and scatter inside one role (between its ranks).
pub trait RoleCollective: Send + Sync {
    /// This rank's position in its role and the role's size.
    fn position(&self) -> (i32, i32);
    /// Gather every rank's `local` to role rank 0 (in rank order). Returns
    /// `Some` on rank 0 and `None` elsewhere.
    fn gather_f64(&self, local: &[f64]) -> Option<Vec<Vec<f64>>>;
    /// Rank 0 passes one part per rank; every rank receives its own part.
    fn scatter_f64(&self, parts: Option<Vec<Vec<f64>>>) -> Vec<f64>;
}

struct LocalGroupShared {
    barrier: Barrier,
    slot: Mutex<Vec<u8>>,
    flags: Mutex<Vec<bool>>,
}

/// Threads in one process, one per role.
struct LocalGroup {
    shared: Arc<LocalGroupShared>,
    roles: usize,
    me: usize,
}

impl LocalGroup {
    fn create(roles: usize) -> Vec<Self> {
        let shared = Arc::new(LocalGroupShared {
            barrier: Barrier::new(roles),
            slot: Mutex::new(Vec::new()),
            flags: Mutex::new(vec![false; roles]),
        });
        (0..roles)
            .map(|me| Self {
                shared: shared.clone(),
                roles,
                me,
            })
            .collect()
    }
}

impl CouplingGroup for LocalGroup {
    fn roles(&self) -> usize {
        self.roles
    }
    fn role_index(&self) -> usize {
        self.me
    }
    fn broadcast(&self, root_role: usize, data: &[u8]) -> Vec<u8> {
        self.shared.barrier.wait();
        if self.me == root_role {
            *self.shared.slot.lock().unwrap() = data.to_vec();
        }
        self.shared.barrier.wait();
        let out = self.shared.slot.lock().unwrap().clone();
        self.shared.barrier.wait();
        out
    }
    fn any(&self, flag: bool) -> bool {
        self.shared.flags.lock().unwrap()[self.me] = flag;
        self.shared.barrier.wait();
        let any = self.shared.flags.lock().unwrap().iter().any(|f| *f);
        self.shared.barrier.wait();
        any
    }
}

/// A role of one rank (every role in local mode).
struct SoloRole;

impl RoleCollective for SoloRole {
    fn position(&self) -> (i32, i32) {
        (0, 1)
    }
    fn gather_f64(&self, local: &[f64]) -> Option<Vec<Vec<f64>>> {
        Some(vec![local.to_vec()])
    }
    fn scatter_f64(&self, parts: Option<Vec<Vec<f64>>>) -> Vec<f64> {
        parts
            .and_then(|mut p| p.pop())
            .expect("scatter on a one-rank role needs one part")
    }
}

struct MpiGroup {
    comm: SimpleCommunicator,
    /// World rank of rank 0 of each role.
    roots: Vec<i32>,
    me: usize,
}

unsafe impl Send for MpiGroup {}
unsafe impl Sync for MpiGroup {}

impl CouplingGroup for MpiGroup {
    fn roles(&self) -> usize {
        self.roots.len()
    }
    fn role_index(&self) -> usize {
        self.me
    }
    fn broadcast(&self, root_role: usize, data: &[u8]) -> Vec<u8> {
        let root = self.comm.process_at_rank(self.roots[root_role]);
        let is_root = self.comm.rank() == self.roots[root_role];
        let mut len = if is_root { data.len() as u64 } else { 0 };
        root.broadcast_into(&mut len);
        let mut buf = if is_root {
            data.to_vec()
        } else {
            vec![0u8; len as usize]
        };
        if len > 0 {
            root.broadcast_into(&mut buf[..]);
        }
        buf
    }
    fn any(&self, flag: bool) -> bool {
        let local = u8::from(flag);
        let mut any = 0u8;
        self.comm
            .all_reduce_into(&local, &mut any, SystemOperation::max());
        any != 0
    }
}

struct MpiRole {
    comm: SimpleCommunicator,
}

unsafe impl Send for MpiRole {}
unsafe impl Sync for MpiRole {}

impl RoleCollective for MpiRole {
    fn position(&self) -> (i32, i32) {
        (self.comm.rank(), self.comm.size())
    }
    fn gather_f64(&self, local: &[f64]) -> Option<Vec<Vec<f64>>> {
        let (rank, size) = self.position();
        if rank != 0 {
            self.comm.process_at_rank(0).send(local);
            return None;
        }
        let mut parts = vec![local.to_vec()];
        for r in 1..size {
            let (part, _) = self.comm.process_at_rank(r).receive_vec::<f64>();
            parts.push(part);
        }
        Some(parts)
    }
    fn scatter_f64(&self, parts: Option<Vec<Vec<f64>>>) -> Vec<f64> {
        let (rank, size) = self.position();
        if rank != 0 {
            return self.comm.process_at_rank(0).receive_vec::<f64>().0;
        }
        let parts = parts.expect("role rank 0 must supply the scatter parts");
        assert_eq!(parts.len(), size as usize, "one scatter part per rank");
        for (r, part) in parts.iter().enumerate().skip(1) {
            self.comm.process_at_rank(r as i32).send(&part[..]);
        }
        parts.into_iter().next().unwrap()
    }
}

// ─── role context ───────────────────────────────────────────────────────────

enum Channel {
    Local(Box<dyn RoleExchange>),
    Mpi {
        /// Root-bridge exchange over the pair communicator.
        bridge: Box<dyn RoleExchange>,
        /// A second duplicate of the pair communicator for sparse routing.
        sparse: SimpleCommunicator,
        peer_root: i32,
        peer_size: i32,
    },
}

unsafe impl Send for Channel {}

/// Everything a role function needs: its name, the input document, a channel
/// to every other role, and the groups for run-wide agreement.
pub struct RoleContext {
    role: String,
    roles: Vec<String>,
    config_source: String,
    channels: BTreeMap<String, Channel>,
    group: Arc<dyn CouplingGroup>,
    role_collective: Arc<dyn RoleCollective>,
}

impl RoleContext {
    /// This process's role.
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Every role, in declaration order.
    pub fn roles(&self) -> &[String] {
        &self.roles
    }

    /// Index of a role in declaration order.
    pub fn role_index(&self, name: &str) -> Option<usize> {
        self.roles.iter().position(|r| r == name)
    }

    /// The complete input document, identical on every rank.
    pub fn config_source(&self) -> &str {
        &self.config_source
    }

    /// Roles this one can exchange with (all others), in declaration order.
    pub fn peers(&self) -> Vec<&str> {
        self.roles
            .iter()
            .filter(|r| **r != self.role)
            .map(String::as_str)
            .collect()
    }

    fn take(&mut self, peer: &str) -> Channel {
        self.channels.remove(peer).unwrap_or_else(|| {
            panic!(
                "role `{}`: no channel to `{peer}` (unknown role, itself, or already taken)",
                self.role
            )
        })
    }

    /// Take the shard exchange with `peer`. Every rank contributes one shard
    /// per call and receives every peer-role shard in peer rank order. Both
    /// roles must call it the same number of times.
    pub fn exchange(&mut self, peer: &str) -> Box<dyn RoleExchange> {
        match self.take(peer) {
            Channel::Local(e) => e,
            Channel::Mpi { bridge, .. } => bridge,
        }
    }

    /// Take a routed exchange with `peer` (direct sparse rank-to-rank delivery
    /// under MPI).
    pub fn routed_exchange(&mut self, peer: &str) -> RoutedRoleExchange {
        match self.take(peer) {
            Channel::Local(e) => RoutedRoleExchange::new(e),
            Channel::Mpi {
                bridge,
                sparse,
                peer_root,
                peer_size,
            } => {
                drop(bridge);
                RoutedRoleExchange::new_sparse_on(
                    grass_mpi::get_mpi_world(),
                    sparse,
                    peer_root,
                    peer_size,
                )
            }
        }
    }

    /// Take a one-peer byte transport to `peer` (valid when `peer` has one
    /// rank), e.g. for [`MultiAppExt::add_remote_subapp`](crate::MultiAppExt::add_remote_subapp).
    pub fn transport(&mut self, peer: &str) -> Box<dyn Transport> {
        Box::new(SinglePeerTransport::new(self.exchange(peer)))
    }

    /// Collectives over every rank of every role.
    pub fn group(&self) -> Arc<dyn CouplingGroup> {
        self.group.clone()
    }

    /// Gather/scatter among this role's ranks.
    pub fn role_collective(&self) -> Arc<dyn RoleCollective> {
        self.role_collective.clone()
    }
}

// ─── runner ─────────────────────────────────────────────────────────────────

type RoleFn<T> = Box<dyn FnOnce(RoleContext) -> T + Send>;

/// Where each role ran.
#[derive(Debug)]
pub enum CoupledRun<T> {
    /// Every role ran on its own thread in this process; results in
    /// declaration order.
    Local(Vec<(String, T)>),
    /// This MPI process ran one role.
    Split {
        /// The role this process ran.
        role: String,
        /// Its result.
        result: T,
    },
}

/// Process-level runner for any number of coupled roles. See the
/// [module docs](self).
pub struct CoupledRunner<T> {
    source: String,
    topology: TopologyConfig,
    declared: Vec<String>,
    roles: BTreeMap<String, RoleFn<T>>,
}

impl<T: Send + 'static> CoupledRunner<T> {
    /// Read the input from the first CLI argument, or use `default_source`.
    pub fn from_cli_or(default_source: &str) -> Result<Self, RunnerError> {
        let source = match std::env::args().nth(1) {
            Some(path) => std::fs::read_to_string(&path)
                .map_err(|source| RunnerError::ReadConfig { path, source })?,
            None => default_source.to_owned(),
        };
        Self::from_source(&source)
    }

    /// Parse the `[topology]` table of a complete input document.
    pub fn from_source(source: &str) -> Result<Self, RunnerError> {
        #[derive(Deserialize)]
        struct Doc {
            topology: TopologyConfig,
        }
        let topology = toml::from_str::<Doc>(source)
            .map_err(|e| RunnerError::Config(e.to_string()))?
            .topology;
        let validated = topology
            .topology()
            .map_err(|e| RunnerError::Config(e.to_string()))?;
        if validated.roles().len() < 2 {
            return Err(RunnerError::RoleCount(validated.roles().len()));
        }
        Ok(Self {
            source: source.to_owned(),
            declared: validated
                .roles()
                .iter()
                .map(|r| r.name().to_owned())
                .collect(),
            topology,
            roles: BTreeMap::new(),
        })
    }

    /// Register the function that runs role `name`.
    pub fn role(mut self, name: &str, run: impl FnOnce(RoleContext) -> T + Send + 'static) -> Self {
        self.roles.insert(name.to_owned(), Box::new(run));
        self
    }

    /// Place the roles and run them. Fails before MPI starts if a declared
    /// role has no function or a function names an undeclared role.
    pub fn run(mut self) -> Result<CoupledRun<T>, RunnerError> {
        for name in &self.declared {
            if !self.roles.contains_key(name) {
                return Err(RunnerError::Config(format!(
                    "role `{name}` is declared in [topology] but no function was registered for it"
                )));
            }
        }
        if let Some(extra) = self.roles.keys().find(|k| !self.declared.contains(k)) {
            return Err(RunnerError::UnknownRole(extra.clone()));
        }
        let digest = config_digest(self.source.as_bytes());
        match MpiRuntime::bootstrap(&self.topology, digest)
            .map_err(|e| RunnerError::Bootstrap(e.to_string()))?
        {
            Bootstrap::Local { .. } => Ok(self.run_local()),
            Bootstrap::Split { runtime } => self.run_split(runtime),
        }
    }

    fn run_local(&mut self) -> CoupledRun<T> {
        let n = self.declared.len();
        let mut channels: Vec<BTreeMap<String, Channel>> =
            (0..n).map(|_| BTreeMap::new()).collect();
        for i in 0..n {
            for j in i + 1..n {
                let (a, b) = LocalRoleExchange::pair();
                channels[i].insert(self.declared[j].clone(), Channel::Local(a));
                channels[j].insert(self.declared[i].clone(), Channel::Local(b));
            }
        }
        let groups = LocalGroup::create(n);
        let mut handles = Vec::new();
        for ((name, chans), group) in self.declared.iter().zip(channels).zip(groups) {
            let ctx = RoleContext {
                role: name.clone(),
                roles: self.declared.clone(),
                config_source: self.source.clone(),
                channels: chans,
                group: Arc::new(group),
                role_collective: Arc::new(SoloRole),
            };
            let f = self.roles.remove(name).unwrap();
            handles.push((
                name.clone(),
                std::thread::Builder::new()
                    .name(name.clone())
                    .spawn(move || f(ctx))
                    .expect("spawn role thread"),
            ));
        }
        let mut out = Vec::new();
        let mut panicked = None;
        for (name, h) in handles {
            match h.join() {
                Ok(r) => out.push((name, r)),
                Err(p) => panicked = panicked.or(Some(p)),
            }
        }
        grass_mpi::finalize_mpi();
        if let Some(p) = panicked {
            std::panic::resume_unwind(p);
        }
        CoupledRun::Local(out)
    }

    fn run_split(&mut self, runtime: MpiRuntime) -> Result<CoupledRun<T>, RunnerError> {
        let topo: RoleTopology = runtime.topology().clone();
        let me = runtime.assignment().name().to_owned();
        let me_idx = self.declared.iter().position(|r| *r == me).unwrap();
        let world = grass_mpi::get_mpi_world_raw();
        let world_rank = world.rank();
        let ranges: Vec<std::ops::Range<i32>> = self
            .declared
            .iter()
            .map(|r| topo.role_world_range(r).expect("validated role"))
            .collect();

        // One communicator per pair of roles, spanning exactly those two.
        // Every world rank takes part in every split, in the same order.
        let mut channels = BTreeMap::new();
        let n = self.declared.len();
        for i in 0..n {
            for j in i + 1..n {
                let in_pair = ranges[i].contains(&world_rank) || ranges[j].contains(&world_rank);
                let color = if in_pair {
                    Color::with_value(0)
                } else {
                    Color::undefined()
                };
                let Some(pair) = world.split_by_color_with_key(color, world_rank) else {
                    continue;
                };
                let sparse = pair.duplicate();
                // In the pair communicator, role i occupies the first ranks.
                let (peer, peer_root, peer_size) = if i == me_idx {
                    (j, ranges[i].len() as i32, ranges[j].len() as i32)
                } else {
                    (i, 0, ranges[i].len() as i32)
                };
                let bridge = MpiRoleExchange::on_comms(
                    grass_mpi::get_mpi_world(),
                    pair,
                    peer_root,
                    peer_size,
                );
                channels.insert(
                    self.declared[peer].clone(),
                    Channel::Mpi {
                        bridge: Box::new(bridge),
                        sparse,
                        peer_root,
                        peer_size,
                    },
                );
            }
        }
        let group = MpiGroup {
            comm: world.duplicate(),
            roots: ranges.iter().map(|r| r.start).collect(),
            me: me_idx,
        };
        let ctx = RoleContext {
            role: me.clone(),
            roles: self.declared.clone(),
            config_source: self.source.clone(),
            channels,
            group: Arc::new(group),
            role_collective: Arc::new(MpiRole {
                comm: grass_mpi::get_mpi_world(),
            }),
        };
        let f = self.roles.remove(&me).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(ctx)));
        drop(world);
        runtime.finalize();
        match result {
            Ok(result) => Ok(CoupledRun::Split { role: me, result }),
            Err(p) => std::panic::resume_unwind(p),
        }
    }
}

// ─── distributed implicit coupling ──────────────────────────────────────────

/// What a role does at the start of an implicit-coupling iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    /// A new window starts: save this role's state.
    Save,
    /// The window is repeated: restore the state saved at its start.
    Restore,
}

/// Windowed implicit coupling between roles on different ranks (or threads).
///
/// The same state machine as [`ImplicitCoupling`](crate::ImplicitCoupling),
/// made collective. Every rank of every role drives one of these in lockstep:
///
/// ```rust,ignore
/// let mut ic = DistributedImplicit::new(&ctx, "tps", iteration, dt, n_windows);
/// while !ic.finished() {
///     match ic.begin() {
///         WindowAction::Save => saved = my_state.save(),
///         WindowAction::Restore => my_state.restore(&saved),
///     }
///     // ... advance this role over the window, exchanging with peers ...
///     let (step, next) = ic.end(my_output_shard);   // owner role passes Some
///     // owner role: `next` is this rank's part of the next interface iterate
/// }
/// ```
///
/// One role (the *owner*) produces the interface output `x̃` the iteration
/// converges, distributed over its ranks as shards. `end` gathers the shards
/// to the owner's rank 0, applies the acceleration there, scatters the next
/// iterate back, and broadcasts the decision (repeat or advance) to every
/// rank of every role, so all roles roll back or move on together. The
/// interface vector is assembled on one rank: fine for surface data up to
/// millions of values.
pub struct DistributedImplicit {
    group: Arc<dyn CouplingGroup>,
    role: Arc<dyn RoleCollective>,
    owner: usize,
    is_owner: bool,
    iter: Option<OuterIteration>,
    dt: f64,
    window: u64,
    n_windows: u64,
    needs_checkpoint: bool,
    iters: u32,
    iters_per_window: Vec<u32>,
    unconverged: Vec<u64>,
}

impl DistributedImplicit {
    /// `owner` is the role whose output is the interface unknown. `iter` is
    /// only used on the owner's rank 0 (pass the same value everywhere).
    pub fn new(
        ctx: &RoleContext,
        owner: &str,
        iter: OuterIteration,
        dt: f64,
        n_windows: u64,
    ) -> Self {
        let owner_idx = ctx
            .role_index(owner)
            .unwrap_or_else(|| panic!("DistributedImplicit: unknown owner role `{owner}`"));
        let is_owner = ctx.role() == owner;
        let role = ctx.role_collective();
        let iter = (is_owner && role.position().0 == 0).then_some(iter);
        Self {
            group: ctx.group(),
            role,
            owner: owner_idx,
            is_owner,
            iter,
            dt,
            window: 0,
            n_windows,
            needs_checkpoint: true,
            iters: 0,
            iters_per_window: Vec::new(),
            unconverged: Vec::new(),
        }
    }

    /// Start an iteration. Returns [`WindowAction::Save`] at the start of a
    /// new window (checkpoint your state) and [`WindowAction::Restore`] when
    /// the window is being repeated (roll back to that checkpoint).
    pub fn begin(&mut self) -> WindowAction {
        if self.needs_checkpoint {
            self.needs_checkpoint = false;
            WindowAction::Save
        } else {
            WindowAction::Restore
        }
    }

    /// Finish an iteration. Owner ranks pass `Some(local shard of x̃)`, every
    /// other rank `None`. Returns the decision, and on owner ranks this rank's
    /// shard of the next iterate (to use on the repeat, or as the first guess
    /// of the next window).
    pub fn end(&mut self, x_tilde: Option<&[f64]>) -> (WindowStep, Option<Vec<f64>>) {
        let mut decision = [0u8; 9];
        let mut next_local = None;
        if self.is_owner {
            let local = x_tilde.expect("owner role must pass its interface output");
            let gathered = self.role.gather_f64(local);
            let parts = gathered.map(|shards| {
                let lens: Vec<usize> = shards.iter().map(Vec::len).collect();
                let flat: Vec<f64> = shards.concat();
                let it = self.iter.as_mut().unwrap();
                let next = it.observe(&flat).to_vec();
                let done = it.done();
                decision[0] = u8::from(done);
                decision[1..5].copy_from_slice(&it.iters().to_le_bytes());
                decision[5] = u8::from(it.converged());
                if done {
                    it.begin_window();
                }
                let mut out = Vec::with_capacity(lens.len());
                let mut at = 0;
                for l in lens {
                    out.push(next[at..at + l].to_vec());
                    at += l;
                }
                out
            });
            next_local = Some(self.role.scatter_f64(parts));
        } else {
            assert!(
                x_tilde.is_none(),
                "only the owner role passes an interface output"
            );
        }
        let d = self.group.broadcast(self.owner, &decision);
        let done = d[0] != 0;
        self.iters = u32::from_le_bytes(d[1..5].try_into().unwrap());
        let step = if done {
            self.iters_per_window.push(self.iters);
            if d[5] == 0 {
                self.unconverged.push(self.window);
            }
            self.window += 1;
            self.needs_checkpoint = true;
            WindowStep::Advance
        } else {
            WindowStep::Repeat
        };
        (step, next_local)
    }

    /// Whether every window has been accepted.
    pub fn finished(&self) -> bool {
        self.window >= self.n_windows
    }

    /// Index of the current window.
    pub fn window(&self) -> u64 {
        self.window
    }

    /// Window length.
    pub fn dt(&self) -> f64 {
        self.dt
    }

    /// Stop after the current window (e.g. the physics ended early). Collective:
    /// every rank must call it with the same value at the same point.
    pub fn truncate(&mut self, stop: bool) {
        if self.group.any(stop) {
            self.n_windows = self.window;
        }
    }

    /// Iterations each accepted window took (identical on every rank).
    pub fn iterations_per_window(&self) -> &[u32] {
        &self.iters_per_window
    }

    /// Windows accepted at the iteration cap without converging.
    pub fn unconverged_windows(&self) -> &[u64] {
        &self.unconverged
    }
}

impl fmt::Debug for DistributedImplicit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DistributedImplicit")
            .field("owner", &self.owner)
            .field("window", &self.window)
            .field("n_windows", &self.n_windows)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREE: &str = r#"
        [topology]
        [[topology.role]]
        name = "flow"
        ranks = 2
        [[topology.role]]
        name = "tps"
        ranks = 1
        [[topology.role]]
        name = "structure"
        ranks = 1
    "#;

    #[test]
    fn accepts_any_number_of_roles_in_declaration_order() {
        let r = CoupledRunner::<()>::from_source(THREE).unwrap();
        assert_eq!(r.declared, ["flow", "tps", "structure"]);
    }

    #[test]
    fn rejects_a_single_role() {
        let e = CoupledRunner::<()>::from_source(
            "[topology]\n[[topology.role]]\nname = \"only\"\nranks = 1\n",
        )
        .err()
        .unwrap();
        assert!(matches!(e, RunnerError::RoleCount(1)));
    }

    // Both checks run before MPI is touched.
    #[test]
    fn missing_or_unknown_role_functions_fail_before_bootstrap() {
        let e = CoupledRunner::<()>::from_source(THREE)
            .unwrap()
            .role("flow", |_| ())
            .role("tps", |_| ())
            .run()
            .err()
            .unwrap();
        assert!(e.to_string().contains("`structure`"), "{e}");
        let e = CoupledRunner::<()>::from_source(THREE)
            .unwrap()
            .role("flow", |_| ())
            .role("tps", |_| ())
            .role("structure", |_| ())
            .role("radiation", |_| ())
            .run()
            .err()
            .unwrap();
        assert!(matches!(e, RunnerError::UnknownRole(r) if r == "radiation"));
    }

    #[test]
    fn local_group_broadcast_and_any() {
        let groups = LocalGroup::create(3);
        let handles: Vec<_> = groups
            .into_iter()
            .map(|g| {
                std::thread::spawn(move || {
                    let me = g.role_index();
                    let b = g.broadcast(1, &[me as u8; 3]);
                    let any = g.any(me == 2);
                    let none = g.any(false);
                    (b, any, none)
                })
            })
            .collect();
        for h in handles {
            let (b, any, none) = h.join().unwrap();
            assert_eq!(b, vec![1u8; 3]);
            assert!(any && !none);
        }
    }
}
