//! Composable schedule fragments for hierarchical run-loop control.
//!
//! `chain_namespaces!` flattens — fine for linear schedules, lossy for
//! schedules with iteration. This module adds [`Schedule`] / [`ScheduleNode`]
//! as a tree primitive: phases composed by `Sequence`, with `Loop` nodes
//! that re-execute their body until a condition flips.
//!
//! The tree lowers at [`Scheduler::set_schedule`](crate::Scheduler::set_schedule)
//! time to namespace assignments + a dispatch tree the run loop walks each
//! iter. Lowering is additive: schedulers that never call `set_schedule`
//! keep today's flat `(namespace, index)` ordering behaviour.
//!
//! ## Builder
//!
//! ```rust,ignore
//! use grass_scheduler::{OnMax, Schedule};
//!
//! let s = Schedule::builder()
//!     .then::<CouplingPre>()
//!     .loop_until(check_implicit_converged, 20, OnMax::Panic, |body| {
//!         body.then::<DemTick>()
//!             .then::<CfdTick>()
//!             .then::<ResidualUpdate>()
//!     })
//!     .then::<CouplingPost>()
//!     .build();
//!
//! parent.set_schedule(s);
//! ```
//!
//! ## What's in 1.0
//!
//! - `ScheduleNode::Phase` — run all systems registered under one
//!   `ScheduleSet` enum type. Namespace index is assigned during lowering by
//!   tree-walk position.
//! - `ScheduleNode::Sequence` — run children in order.
//! - `ScheduleNode::Loop` — re-execute body until the `until` condition
//!   returns `true`, or `max_iters` is reached. On hitting max:
//!   - `OnMax::AcceptUnconverged` — continue past the loop
//!   - `OnMax::Panic` — abort with diagnostic
//!
//!   `OnMax::RejectAndShrinkDt` is deferred to Phase 2 (needs `Snapshot<T>`
//!   wired up first).
//!
//! ## What's not yet
//!
//! - `Branch` (state-conditional fragments) — Phase 1.5.
//! - `RejectAndShrinkDt` rollback semantics — Phase 2.

use crate::{Condition, IntoCondition, ScheduleSet};
use std::any::TypeId;

/// What to do when a [`ScheduleNode::Loop`] hits its `max_iters` without the
/// `until` condition flipping to `true`.
///
/// Not `Copy`/`Clone` because `Rollback` carries an owned [`ScheduleNode`]
/// containing `Box<dyn Condition>`. Match on `&OnMax` (or `&mut OnMax`) when
/// you need to inspect it after pattern binding.
pub enum OnMax {
    /// Move past the loop and continue the rest of the schedule. Use when
    /// "best effort" iteration is acceptable (e.g. fixed-point that usually
    /// converges; the rare miss is logged but doesn't abort).
    AcceptUnconverged,
    /// Abort with a diagnostic. Use during development / strict simulations
    /// where non-convergence is a bug, not a budget miss.
    Panic,
    /// Run a user-supplied rollback fragment exactly once, then continue
    /// past the loop. Pair with `Snapshot<T>` save/restore systems to
    /// undo a tentative step; pair with a "halve dt" system to react to
    /// the convergence failure. Use the [`ScheduleBuilder::loop_with_rollback`]
    /// builder method to construct a Loop with this behaviour.
    ///
    /// "Retry with a smaller dt" is naturally expressed as a Loop-of-Loops:
    /// outer `loop_until` checks for outer-convergence, inner
    /// `loop_with_rollback` halves dt + restores on max. The inner's
    /// rollback runs once per outer try; after enough outer tries, the
    /// outer's max iters fires.
    Rollback(Box<ScheduleNode>),
}

impl std::fmt::Debug for OnMax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AcceptUnconverged => f.write_str("AcceptUnconverged"),
            Self::Panic => f.write_str("Panic"),
            Self::Rollback(n) => f.debug_tuple("Rollback").field(n).finish(),
        }
    }
}

/// Tree node in a [`Schedule`]. Constructed via [`ScheduleBuilder`]; usually
/// not built directly.
pub enum ScheduleNode {
    /// Run systems registered under a `ScheduleSet` type. If `variant` is
    /// `None`, dispatches every system whose `schedule_type_id` matches —
    /// the whole-enum batch. If `Some(i)`, dispatches **only** systems
    /// registered under the variant whose `to_index() == i`.
    ///
    /// `namespace` is assigned during lowering (tree-walk order).
    Phase {
        /// Human-readable type name for diagnostics + dot output.
        type_name: &'static str,
        /// Identifies which `ScheduleSet` enum's systems this node dispatches.
        type_id: TypeId,
        /// `None` = whole-enum dispatch (every variant); `Some(i)` =
        /// dispatch only the variant with `to_index() == i`.
        variant: Option<u32>,
        /// Namespace index assigned at lowering. `0` until lowered.
        namespace: u32,
    },
    /// Run children in order.
    Sequence(Vec<ScheduleNode>),
    /// Re-execute `body` until `until` returns `true` or `max_iters` is hit.
    Loop {
        body: Box<ScheduleNode>,
        until: Box<dyn Condition + 'static>,
        max_iters: usize,
        on_max: OnMax,
    },
    /// First-match-wins state-conditional dispatch. Each arm pairs a
    /// `Condition` with a sub-tree; the schedule evaluates conditions in
    /// declaration order and runs the first matching arm's body. If no arm
    /// matches, the branch is a no-op.
    Branch {
        arms: Vec<(Box<dyn Condition + 'static>, ScheduleNode)>,
    },
}

impl std::fmt::Debug for ScheduleNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Phase {
                type_name,
                variant,
                namespace,
                ..
            } => f
                .debug_struct("Phase")
                .field("type_name", type_name)
                .field("variant", variant)
                .field("namespace", namespace)
                .finish(),
            Self::Sequence(c) => f.debug_tuple("Sequence").field(c).finish(),
            Self::Loop {
                body,
                max_iters,
                on_max,
                ..
            } => f
                .debug_struct("Loop")
                .field("body", body)
                .field("max_iters", max_iters)
                .field("on_max", on_max)
                .finish_non_exhaustive(),
            Self::Branch { arms } => {
                let bodies: Vec<&ScheduleNode> = arms.iter().map(|(_, n)| n).collect();
                f.debug_struct("Branch")
                    .field("arm_count", &arms.len())
                    .field("bodies", &bodies)
                    .finish()
            }
        }
    }
}

/// Top-level holder. Set on a `Scheduler` via
/// [`Scheduler::set_schedule`](crate::Scheduler::set_schedule); walked each
/// iter by `run_with_schedule`.
pub struct Schedule {
    pub(crate) root: ScheduleNode,
}

impl Schedule {
    /// Start a new builder.
    pub fn builder() -> ScheduleBuilder {
        ScheduleBuilder::new()
    }

    /// Direct constructor for tests / advanced use.
    pub fn from_node(root: ScheduleNode) -> Self {
        Self { root }
    }

    /// Borrow the root node — useful for diagnostics / dot output.
    pub fn root(&self) -> &ScheduleNode {
        &self.root
    }
}

impl std::fmt::Debug for Schedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Schedule")
            .field("root", &self.root)
            .finish()
    }
}

// ─── Builder ────────────────────────────────────────────────────────────────

/// Fluent builder for [`Schedule`].
///
/// Each `.then::<P>()` appends a `ScheduleNode::Phase` for phase enum `P`.
/// `.loop_until(...)` appends a `ScheduleNode::Loop` whose body is built by
/// the provided closure (which receives a fresh inner builder). `.build()`
/// finalises into a `Schedule`; if exactly one node was appended it becomes
/// the root directly, otherwise the nodes are wrapped in `Sequence`.
pub struct ScheduleBuilder {
    nodes: Vec<ScheduleNode>,
}

impl Default for ScheduleBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduleBuilder {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Append a phase: every system registered under `P` runs at this point,
    /// in `to_index` order. Whole-enum dispatch.
    pub fn then<P: ScheduleSet + 'static>(mut self) -> Self {
        self.nodes.push(ScheduleNode::Phase {
            type_name: std::any::type_name::<P>(),
            type_id: TypeId::of::<P>(),
            variant: None,
            namespace: 0, // assigned at lowering
        });
        self
    }

    /// Append a phase that dispatches **only** the systems registered
    /// under one specific variant of `P`. Use to place separate variants
    /// of the same `ScheduleSet` enum at different positions in the tree
    /// (e.g. `Stage::Save` before a `Loop`, `Stage::Check` after).
    ///
    /// Mixing whole-enum (`then::<P>()`) and per-variant (`then_variant`)
    /// dispatch for the **same `P`** in one tree is a build-time error —
    /// the lowering can't assign a unique namespace to systems that
    /// would match both forms.
    pub fn then_variant<P: ScheduleSet + 'static>(mut self, value: P) -> Self {
        self.nodes.push(ScheduleNode::Phase {
            type_name: std::any::type_name::<P>(),
            type_id: TypeId::of::<P>(),
            variant: Some(value.to_index()),
            namespace: 0,
        });
        self
    }

    /// Append a `Loop` whose body is built by `body_fn`. The closure receives
    /// a fresh inner builder; whatever it returns becomes the loop body.
    ///
    /// `until` is any function/closure that returns `bool` and uses the
    /// scheduler's [`SystemParam`](crate::SystemParam) machinery — the same
    /// thing `.run_if(...)` accepts.
    pub fn loop_until<I, C: IntoCondition<I>>(
        mut self,
        until: C,
        max_iters: usize,
        on_max: OnMax,
        body_fn: impl FnOnce(ScheduleBuilder) -> ScheduleBuilder,
    ) -> Self
    where
        C::Condition: Condition + 'static,
    {
        let body_builder = body_fn(ScheduleBuilder::new());
        let body = body_builder.into_node();
        self.nodes.push(ScheduleNode::Loop {
            body: Box::new(body),
            until: Box::new(until.into_condition()),
            max_iters,
            on_max,
        });
        self
    }

    /// Append a `Loop` with a [`OnMax::Rollback`] sub-tree. The `body_fn`
    /// builds the loop body (run up to `max_iters` times); `rollback_fn`
    /// builds the rollback fragment that runs exactly once when the loop
    /// hits `max_iters` without the `until` condition flipping. After the
    /// rollback runs, the schedule continues past the loop normally.
    ///
    /// Pair the rollback fragment with `Snapshot<T>` restore systems and a
    /// "halve dt" / "set rejected flag" system to undo a tentative step
    /// gracefully. For "retry with smaller dt" semantics, wrap this loop in
    /// an outer `loop_until` that gates on a "made progress" condition.
    pub fn loop_with_rollback<I, C: IntoCondition<I>>(
        mut self,
        until: C,
        max_iters: usize,
        body_fn: impl FnOnce(ScheduleBuilder) -> ScheduleBuilder,
        rollback_fn: impl FnOnce(ScheduleBuilder) -> ScheduleBuilder,
    ) -> Self
    where
        C::Condition: Condition + 'static,
    {
        let body = body_fn(ScheduleBuilder::new()).into_node();
        let rollback = rollback_fn(ScheduleBuilder::new()).into_node();
        self.nodes.push(ScheduleNode::Loop {
            body: Box::new(body),
            until: Box::new(until.into_condition()),
            max_iters,
            on_max: OnMax::Rollback(Box::new(rollback)),
        });
        self
    }

    /// Append a `Branch` — first-match-wins state-conditional dispatch.
    /// The closure receives a fresh `BranchBuilder`; each `.arm(cond, body)`
    /// call adds one (condition, body) pair. At run time the schedule
    /// evaluates conditions in declaration order and runs the first that
    /// returns `true`; if none match, the branch is a no-op.
    pub fn branch(mut self, builder_fn: impl FnOnce(BranchBuilder) -> BranchBuilder) -> Self {
        let bb = builder_fn(BranchBuilder::new());
        self.nodes.push(ScheduleNode::Branch { arms: bb.arms });
        self
    }

    /// Convert the builder's accumulated nodes into a single node:
    /// - 0 nodes → empty `Sequence(vec![])`
    /// - 1 node  → that node directly
    /// - N nodes → `Sequence(nodes)`
    fn into_node(self) -> ScheduleNode {
        match self.nodes.len() {
            0 => ScheduleNode::Sequence(Vec::new()),
            1 => self.nodes.into_iter().next().unwrap(),
            _ => ScheduleNode::Sequence(self.nodes),
        }
    }

    /// Finalise into a [`Schedule`].
    pub fn build(self) -> Schedule {
        Schedule {
            root: self.into_node(),
        }
    }
}

// ─── BranchBuilder ──────────────────────────────────────────────────────────

/// Fluent builder for [`ScheduleNode::Branch`]'s arm list. Returned by
/// [`ScheduleBuilder::branch`].
///
/// Each `.arm(cond, body_fn)` call appends one `(condition, body)` pair.
/// At run time, conditions are evaluated in declaration order — first match
/// wins, so place specific arms before catch-alls (a `|| true` closure works
/// as a default arm).
pub struct BranchBuilder {
    arms: Vec<(Box<dyn Condition + 'static>, ScheduleNode)>,
}

impl Default for BranchBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl BranchBuilder {
    pub fn new() -> Self {
        Self { arms: Vec::new() }
    }

    /// Append an arm. `cond` is any function/closure that returns `bool`
    /// (same shape `.run_if` accepts). `body_fn` builds the arm's body via
    /// a fresh inner schedule builder.
    pub fn arm<I, C: IntoCondition<I>>(
        mut self,
        cond: C,
        body_fn: impl FnOnce(ScheduleBuilder) -> ScheduleBuilder,
    ) -> Self
    where
        C::Condition: Condition + 'static,
    {
        let body = body_fn(ScheduleBuilder::new()).into_node();
        self.arms.push((Box::new(cond.into_condition()), body));
        self
    }
}

// ─── Internal helpers used by Scheduler ─────────────────────────────────────

/// Walk the tree assigning sequential namespace indices to every Phase node
/// in tree-walk order. Returns the next available namespace value, so callers
/// can chain.
pub(crate) fn assign_namespaces(node: &mut ScheduleNode, counter: &mut u32) {
    match node {
        ScheduleNode::Phase { namespace, .. } => {
            *namespace = *counter;
            *counter += 1;
        }
        ScheduleNode::Sequence(children) => {
            for c in children {
                assign_namespaces(c, counter);
            }
        }
        ScheduleNode::Loop { body, on_max, .. } => {
            assign_namespaces(body, counter);
            if let OnMax::Rollback(rb) = on_max {
                assign_namespaces(rb, counter);
            }
        }
        ScheduleNode::Branch { arms } => {
            for (_, body) in arms {
                assign_namespaces(body, counter);
            }
        }
    }
}

/// One assignment from the schedule tree: the namespace + (optional) variant
/// filter that `set_schedule` will impose on every matching system.
pub(crate) struct PhaseAssignment {
    pub type_id: TypeId,
    /// `None` = whole-enum dispatch; `Some(i)` = only variant whose
    /// `to_index() == i`.
    pub variant: Option<u32>,
    pub namespace: u32,
}

/// Walk the tree collecting one [`PhaseAssignment`] per Phase node.
/// Used by `Scheduler::set_schedule` to retroactively rewrite the
/// namespace fields on already-registered systems.
pub(crate) fn collect_phase_assignments(node: &ScheduleNode, out: &mut Vec<PhaseAssignment>) {
    match node {
        ScheduleNode::Phase {
            type_id,
            variant,
            namespace,
            ..
        } => {
            out.push(PhaseAssignment {
                type_id: *type_id,
                variant: *variant,
                namespace: *namespace,
            });
        }
        ScheduleNode::Sequence(children) => {
            for c in children {
                collect_phase_assignments(c, out);
            }
        }
        ScheduleNode::Loop { body, on_max, .. } => {
            collect_phase_assignments(body, out);
            if let OnMax::Rollback(rb) = on_max {
                collect_phase_assignments(rb, out);
            }
        }
        ScheduleNode::Branch { arms } => {
            for (_, body) in arms {
                collect_phase_assignments(body, out);
            }
        }
    }
}

/// Recursively call `prepare` on every condition in the tree, accumulating
/// any missing-resource error strings.
pub(crate) fn prepare_conditions(
    node: &mut ScheduleNode,
    index: &std::collections::HashMap<TypeId, usize>,
) -> Vec<String> {
    let mut errors = Vec::new();
    match node {
        ScheduleNode::Phase { .. } => {}
        ScheduleNode::Sequence(children) => {
            for c in children {
                errors.extend(prepare_conditions(c, index));
            }
        }
        ScheduleNode::Loop {
            body,
            until,
            on_max,
            ..
        } => {
            errors.extend(prepare_conditions(body, index));
            errors.extend(until.prepare(index));
            if let OnMax::Rollback(rb) = on_max {
                errors.extend(prepare_conditions(rb, index));
            }
        }
        ScheduleNode::Branch { arms } => {
            for (cond, body) in arms {
                errors.extend(cond.prepare(index));
                errors.extend(prepare_conditions(body, index));
            }
        }
    }
    errors
}
