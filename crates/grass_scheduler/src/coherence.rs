//! Scheduler-mediated host↔device coherence (coherence_plan.md Phase 2).
//!
//! A *mirror* is a logical resource that lives in two places — a host copy in the
//! scheduler's resource table and a device copy (e.g. GPU buffers) — kept coherent
//! lazily. The scheduler reads each system's declared access
//! ([`System::accesses`](crate::System::accesses)) and, before running a system
//! that touches a `DeviceDirty` mirror, calls the mirror's [`MirrorBridge`] to pull
//! the device copy back to the host. Every such pull is counted and attributed to
//! the offending system with a one-line warning ("residency lost this tick").
//!
//! ## Roles
//! - **Host consumers** (any ordinary system reading/writing the trigger resource)
//!   are auto-managed: a read of a `DeviceDirty` mirror forces a `download`; a write
//!   marks the mirror `HostDirty` afterwards.
//! - **The device producer** (e.g. the resident GPU stepper) is *self-managed*: it
//!   takes `ResMut<CoherenceRegistry>` and drives the state itself
//!   (`take_host_dirty` → upload+reprime, `mark_device_dirty` after stepping). Any
//!   system that touches the registry is skipped by the auto hooks, so its own
//!   trigger access does not falsely flip the state.
//!
//! This module is wgpu-free: the actual device transfer lives in a [`MirrorBridge`]
//! implementor in the GPU crate, which borrows the resource cells it needs by index.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::AccessKind;

/// Coherence state of a single mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorState {
    /// Host and device copies agree.
    Coherent,
    /// A host system wrote the host copy; the device is stale.
    HostDirty,
    /// The device advanced the canonical copy; the host mirror is stale.
    DeviceDirty,
}

/// Moves a mirror's data device→host. Implemented in the GPU crate; the bridge
/// stores the resource indices it needs and borrows those cells from `resources`
/// itself (so this crate stays wgpu-free). Must NOT touch the `CoherenceRegistry`
/// cell — the scheduler holds it borrowed while calling `download`.
pub trait MirrorBridge: 'static {
    /// Sync the device copy back into the host resource(s). Called by the scheduler
    /// when a host consumer reads a `DeviceDirty` mirror.
    fn download(&self, resources: &[RefCell<Box<dyn Any>>]);
}

struct MirrorEntry {
    /// The host resource whose access triggers coherence (e.g. `Atom`).
    trigger_type: TypeId,
    /// `trigger_type`'s slot in the resource table, resolved at organize time.
    trigger_index: usize,
    state: MirrorState,
    /// Count of device→host pulls forced by host consumers.
    syncs: u64,
    bridge: Box<dyn MirrorBridge>,
    /// System names already warned about (rate-limit: warn once per system).
    warned: HashSet<String>,
}

/// Registry of mirrors, stored as a scheduler resource. Registered by the GPU
/// plugin; consulted by the scheduler's run loop around every system.
pub struct CoherenceRegistry {
    mirrors: Vec<MirrorEntry>,
    /// When true, suppress the per-sync warning (still counts). Mirrors
    /// `SIM_SUPPRESS_WARNINGS`.
    pub suppress_warnings: bool,
}

impl Default for CoherenceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CoherenceRegistry {
    /// New empty registry. Honors `SIM_SUPPRESS_WARNINGS` for the sync warning.
    pub fn new() -> Self {
        CoherenceRegistry {
            mirrors: Vec::new(),
            suppress_warnings: std::env::var("SIM_SUPPRESS_WARNINGS").is_ok(),
        }
    }

    /// Register a mirror triggered by access to `trigger_type`. `trigger_index` is
    /// resolved later by [`resolve_indices`](Self::resolve_indices).
    pub fn register(&mut self, trigger_type: TypeId, bridge: Box<dyn MirrorBridge>) {
        self.mirrors.push(MirrorEntry {
            trigger_type,
            trigger_index: usize::MAX,
            state: MirrorState::Coherent,
            syncs: 0,
            bridge,
            warned: HashSet::new(),
        });
    }

    /// Resolve each mirror's trigger resource index from the scheduler's type→index
    /// map. Called from `organize_systems` once the resource table is final.
    pub fn resolve_indices(&mut self, index: &HashMap<TypeId, usize>) {
        for m in &mut self.mirrors {
            m.trigger_index = index.get(&m.trigger_type).copied().unwrap_or(usize::MAX);
        }
    }

    /// Mark the device copy authoritative (the device just advanced the trajectory).
    /// Called by the device producer after stepping.
    pub fn mark_device_dirty(&mut self, trigger_type: TypeId) {
        if let Some(m) = self.mirrors.iter_mut().find(|m| m.trigger_type == trigger_type) {
            m.state = MirrorState::DeviceDirty;
        }
    }

    /// If the mirror is `HostDirty`, clear it to `Coherent` and return `true` (the
    /// caller must upload host→device). Otherwise return `false`. Called by the
    /// device producer before stepping.
    pub fn take_host_dirty(&mut self, trigger_type: TypeId) -> bool {
        if let Some(m) = self.mirrors.iter_mut().find(|m| m.trigger_type == trigger_type) {
            if m.state == MirrorState::HostDirty {
                m.state = MirrorState::Coherent;
                return true;
            }
        }
        false
    }

    /// Current state of a mirror (for tests / diagnostics).
    pub fn state(&self, trigger_type: TypeId) -> Option<MirrorState> {
        self.mirrors
            .iter()
            .find(|m| m.trigger_type == trigger_type)
            .map(|m| m.state)
    }

    /// Number of device→host pulls forced by host consumers (for tests / metrics).
    pub fn syncs(&self, trigger_type: TypeId) -> u64 {
        self.mirrors
            .iter()
            .find(|m| m.trigger_type == trigger_type)
            .map(|m| m.syncs)
            .unwrap_or(0)
    }

    /// True if any registered mirror's trigger is among `accesses`.
    fn touches_any_trigger(&self, accesses: &[(usize, AccessKind)]) -> bool {
        accesses
            .iter()
            .any(|(idx, _)| self.mirrors.iter().any(|m| m.trigger_index == *idx))
    }
}

/// Returns true if this access set touches the coherence registry itself — such a
/// system is self-managed and skipped by the auto hooks.
#[inline]
fn is_self_managed(coh_index: usize, accesses: &[(usize, AccessKind)]) -> bool {
    accesses.iter().any(|(idx, _)| *idx == coh_index)
}

/// Pre-run hook: for each trigger this system reads/writes, if the mirror is
/// `DeviceDirty`, pull the device copy back to the host before the system runs.
/// Counts the sync and warns (once per system). No-op for self-managed systems.
pub(crate) fn ensure_coherent(
    resources: &[RefCell<Box<dyn Any>>],
    coh_index: usize,
    accesses: &[(usize, AccessKind)],
    sys_name: &str,
) {
    if accesses.is_empty() || is_self_managed(coh_index, accesses) {
        return;
    }
    let mut guard = resources[coh_index].borrow_mut();
    let reg = guard
        .downcast_mut::<CoherenceRegistry>()
        .expect("coherence: resource at coh_index is not a CoherenceRegistry");
    if !reg.touches_any_trigger(accesses) {
        return;
    }
    let suppress = reg.suppress_warnings;
    for (idx, _kind) in accesses {
        for m in reg.mirrors.iter_mut() {
            if m.trigger_index == *idx && m.state == MirrorState::DeviceDirty {
                // Bridge borrows OTHER resource cells (host copy + device handle);
                // it must not touch resources[coh_index], which we hold here.
                m.bridge.download(resources);
                m.state = MirrorState::Coherent;
                m.syncs += 1;
                if !suppress && m.warned.insert(sys_name.to_string()) {
                    eprintln!(
                        "[coherence] system `{}` forced a device→host sync — residency lost this tick (mirror sync #{})",
                        sys_name, m.syncs
                    );
                }
            }
        }
    }
}

/// Post-run hook: mark `HostDirty` every mirror this system wrote (via a `Write`
/// access to the trigger). No-op for self-managed systems.
pub(crate) fn mark_writes(
    resources: &[RefCell<Box<dyn Any>>],
    coh_index: usize,
    accesses: &[(usize, AccessKind)],
) {
    if accesses.is_empty() || is_self_managed(coh_index, accesses) {
        return;
    }
    let mut guard = resources[coh_index].borrow_mut();
    let reg = guard
        .downcast_mut::<CoherenceRegistry>()
        .expect("coherence: resource at coh_index is not a CoherenceRegistry");
    for (idx, kind) in accesses {
        if *kind == AccessKind::Write {
            for m in reg.mirrors.iter_mut() {
                if m.trigger_index == *idx {
                    m.state = MirrorState::HostDirty;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeAtom(i32);
    struct FakeGpu(i32);

    struct FakeBridge {
        atom_idx: usize,
        gpu_idx: usize,
    }
    impl MirrorBridge for FakeBridge {
        fn download(&self, res: &[RefCell<Box<dyn Any>>]) {
            let gpu_val = res[self.gpu_idx].borrow().downcast_ref::<FakeGpu>().unwrap().0;
            res[self.atom_idx].borrow_mut().downcast_mut::<FakeAtom>().unwrap().0 = gpu_val;
        }
    }

    // resources: [0]=FakeAtom (trigger), [1]=CoherenceRegistry, [2]=FakeGpu
    fn setup() -> (Vec<RefCell<Box<dyn Any>>>, usize, usize) {
        let (atom_idx, coh_idx, gpu_idx) = (0usize, 1usize, 2usize);
        let mut reg = CoherenceRegistry::new();
        reg.suppress_warnings = true;
        reg.register(TypeId::of::<FakeAtom>(), Box::new(FakeBridge { atom_idx, gpu_idx }));
        let mut index = HashMap::new();
        index.insert(TypeId::of::<FakeAtom>(), atom_idx);
        reg.resolve_indices(&index);
        let resources: Vec<RefCell<Box<dyn Any>>> = vec![
            RefCell::new(Box::new(FakeAtom(0))),
            RefCell::new(Box::new(reg)),
            RefCell::new(Box::new(FakeGpu(99))),
        ];
        (resources, coh_idx, atom_idx)
    }

    fn reg_ref(resources: &[RefCell<Box<dyn Any>>], coh: usize) -> MirrorState {
        resources[coh]
            .borrow()
            .downcast_ref::<CoherenceRegistry>()
            .unwrap()
            .state(TypeId::of::<FakeAtom>())
            .unwrap()
    }

    #[test]
    fn device_dirty_read_pulls_and_counts() {
        let (res, coh, atom) = setup();
        res[coh].borrow_mut().downcast_mut::<CoherenceRegistry>().unwrap()
            .mark_device_dirty(TypeId::of::<FakeAtom>());
        // A host reader of the trigger forces a download.
        ensure_coherent(&res, coh, &[(atom, AccessKind::Read)], "reader_sys");
        assert_eq!(res[atom].borrow().downcast_ref::<FakeAtom>().unwrap().0, 99);
        assert_eq!(reg_ref(&res, coh), MirrorState::Coherent);
        let syncs = res[coh].borrow().downcast_ref::<CoherenceRegistry>().unwrap()
            .syncs(TypeId::of::<FakeAtom>());
        assert_eq!(syncs, 1);
    }

    #[test]
    fn coherent_read_is_noop() {
        let (res, coh, atom) = setup(); // starts Coherent
        ensure_coherent(&res, coh, &[(atom, AccessKind::Read)], "reader_sys");
        assert_eq!(res[atom].borrow().downcast_ref::<FakeAtom>().unwrap().0, 0); // untouched
        assert_eq!(reg_ref(&res, coh), MirrorState::Coherent);
    }

    #[test]
    fn write_marks_host_dirty_and_take_clears() {
        let (res, coh, atom) = setup();
        mark_writes(&res, coh, &[(atom, AccessKind::Write)]);
        assert_eq!(reg_ref(&res, coh), MirrorState::HostDirty);
        // The device producer consumes the HostDirty (→ would upload+reprime).
        let took = res[coh].borrow_mut().downcast_mut::<CoherenceRegistry>().unwrap()
            .take_host_dirty(TypeId::of::<FakeAtom>());
        assert!(took);
        assert_eq!(reg_ref(&res, coh), MirrorState::Coherent);
        // Second take is false (already cleared).
        let took2 = res[coh].borrow_mut().downcast_mut::<CoherenceRegistry>().unwrap()
            .take_host_dirty(TypeId::of::<FakeAtom>());
        assert!(!took2);
    }

    #[test]
    fn self_managed_system_is_skipped() {
        let (res, coh, atom) = setup();
        res[coh].borrow_mut().downcast_mut::<CoherenceRegistry>().unwrap()
            .mark_device_dirty(TypeId::of::<FakeAtom>());
        // A self-managed system touches the registry (coh index) AND the trigger.
        ensure_coherent(&res, coh, &[(atom, AccessKind::Write), (coh, AccessKind::Write)], "resident_step");
        // No download happened; state untouched.
        assert_eq!(res[atom].borrow().downcast_ref::<FakeAtom>().unwrap().0, 0);
        assert_eq!(reg_ref(&res, coh), MirrorState::DeviceDirty);
        // And mark_writes is also skipped (no false HostDirty).
        mark_writes(&res, coh, &[(atom, AccessKind::Write), (coh, AccessKind::Write)]);
        assert_eq!(reg_ref(&res, coh), MirrorState::DeviceDirty);
    }
}
