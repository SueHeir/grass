//! Cross-namespace [`Snapshot<T>`] helpers — save/restore a sub-App's
//! resource via [`Multi`].
//!
//! These mirror [`grass_scheduler::snapshot_resource`] /
//! [`grass_scheduler::restore_resource`] but reach across namespaces. The
//! sub-App must own *both* the resource being snapshotted (`T`) and the
//! `Snapshot<T>` slot. Both are looked up under the same namespace.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use grass_multi::{snapshot_subapp_resource, restore_subapp_resource};
//! use grass_scheduler::Snapshot;
//!
//! // On the sub-App, register the snapshot slot alongside the real resource:
//! cfd_app.add_resource(Snapshot::<FlowField>::default());
//!
//! // On the parent, schedule save/restore around a tentative-step Loop:
//! parent.add_update_system(snapshot_subapp_resource::<FlowField>("cfd"), Phase::Save);
//! parent.add_update_system(restore_subapp_resource::<FlowField>("cfd"), Phase::RestoreOnReject);
//! ```
//!
//! For resources where `Clone` is too expensive, write a custom save/restore
//! pair as ordinary [`Multi`]-using systems — `Snapshot<T>` is just a
//! resource, you're free to fill it with a partial / double-buffered
//! representation.

use crate::multi::Multi;
use grass_scheduler::Snapshot;

/// Returns a system that takes [`Multi`], reads `T` from sub-App `ns`,
/// clones it, and stores the clone into `Snapshot<T>` on the same sub-App.
///
/// `ns` is a `'static` string so the closure doesn't have to allocate.
/// Pair with [`restore_subapp_resource`] at the rollback point.
pub fn snapshot_subapp_resource<T: Clone + 'static>(ns: &'static str) -> impl FnMut(Multi) {
    move |world: Multi| {
        let cloned = (*world.expect_read::<T>(ns)).clone();
        world.expect_write::<Snapshot<T>>(ns).saved = Some(cloned);
    }
}

/// Returns a system that takes [`Multi`], moves the saved value out of
/// sub-App `ns`'s `Snapshot<T>` (taking it), and overwrites the live `T`.
///
/// No-op when no saved value is present, so this can be placed
/// unconditionally in the schedule and only the *save* path needs gating.
pub fn restore_subapp_resource<T: 'static>(ns: &'static str) -> impl FnMut(Multi) {
    move |world: Multi| {
        let saved = world.expect_write::<Snapshot<T>>(ns).saved.take();
        if let Some(s) = saved {
            *world.expect_write::<T>(ns) = s;
        }
    }
}
