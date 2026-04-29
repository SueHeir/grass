//! [`Snapshot<T>`] — per-resource opt-in reversibility.
//!
//! A `Snapshot<T>` is a tiny resource that holds at most one saved copy of
//! a `T`. Plugins opt their resources into reversibility by registering
//! `Snapshot<T>` alongside the resource itself, then placing
//! [`snapshot_resource`] / [`restore_resource`] systems where the schedule
//! needs save/restore points (typically a `Schedule::Loop` accepting a
//! tentative step that may be rolled back, or a predictor-corrector
//! pattern).
//!
//! ## Quick use
//!
//! ```rust,ignore
//! use grass_scheduler::{snapshot_resource, restore_resource, Snapshot};
//!
//! parent.add_resource(Snapshot::<FlowField>::default());
//! parent.add_update_system(snapshot_resource::<FlowField>(), Phase::SaveBeforeStep);
//! parent.add_update_system(restore_resource::<FlowField>(), Phase::RestoreOnReject);
//! ```
//!
//! `Snapshot::<T>::default()` requires no bound on `T` — the saved slot
//! starts as `None`. Only the *save* system needs `T: Clone`.
//!
//! ## When `Clone` is too expensive
//!
//! For resources where a full `Clone` is prohibitive (e.g. an `Atom` with
//! neighbor lists), implement a custom save/restore pair as ordinary
//! systems — `Snapshot<T>` is just a plain resource, you're free to fill
//! it with a partial / double-buffered representation. The default
//! [`snapshot_resource`] / [`restore_resource`] helpers exist for the
//! common `T: Clone` case.

use crate::{Res, ResMut};

/// Single-slot save buffer for resource `T`. Default is `None`.
pub struct Snapshot<T> {
    /// `Some(saved_value)` after a save; `None` initially or after a restore.
    pub saved: Option<T>,
}

impl<T> Default for Snapshot<T> {
    fn default() -> Self {
        Self { saved: None }
    }
}

impl<T> Snapshot<T> {
    /// Construct an empty snapshot. Equivalent to `Snapshot::default()` but
    /// inferable in places where the type can't be elided.
    pub const fn empty() -> Self {
        Self { saved: None }
    }

    /// `true` if a saved value is currently held.
    pub fn has_saved(&self) -> bool {
        self.saved.is_some()
    }

    /// Drop any saved value. Useful between schedule iterations to enforce
    /// "snapshot must be re-taken each iter".
    pub fn clear(&mut self) {
        self.saved = None;
    }
}

/// Returns a system `FnMut(Res<T>, ResMut<Snapshot<T>>)` that clones the
/// current value of `T` into the snapshot slot, overwriting any prior save.
pub fn snapshot_resource<T: Clone + 'static>() -> impl FnMut(Res<T>, ResMut<Snapshot<T>>) {
    |src: Res<T>, mut snap: ResMut<Snapshot<T>>| {
        snap.saved = Some((*src).clone());
    }
}

/// Returns a system `FnMut(ResMut<T>, ResMut<Snapshot<T>>)` that — when a
/// saved value is present — restores `T` from the snapshot and clears the
/// slot (`take` semantics). If no saved value is present, the system is a
/// no-op, so callers can place restore systems unconditionally and only
/// the *save* path needs to gate by state.
pub fn restore_resource<T: 'static>() -> impl FnMut(ResMut<T>, ResMut<Snapshot<T>>) {
    |mut dst: ResMut<T>, mut snap: ResMut<Snapshot<T>>| {
        if let Some(saved) = snap.saved.take() {
            *dst = saved;
        }
    }
}
