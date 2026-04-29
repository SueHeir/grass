//! [`RemoteMirrorPhysics`] — a sub-App whose resources are pumped over a
//! [`Transport`] to/from a peer process each iter.
//!
//! The mirror owns a tiny [`grass_app::App`] purely for the resources
//! that cross the wire. [`Multi`](crate::Multi) /
//! [`MultiRes`](crate::MultiRes) / [`MultiResMut`](crate::MultiResMut)
//! can read or write those resources via [`Physics::resource_cell`] just
//! like a local sub-App's resources — coupling systems can't tell which
//! is which. The peer steps on its own; this side's [`Physics::step`]
//! just sends every `send_each_iter` payload then receives every
//! `recv_each_iter` payload.
//!
//! The MPMD boundary lives only at registration:
//!
//! ```rust,ignore
//! // Local: one App per physics, parent ticks them.
//! parent.add_subapp("dem", dem_app);
//!
//! // Remote: empty mirror App + transport pumps. Multi access from any
//! // parent system reads the mirror's freshly-pumped resources, identical
//! // to the local case.
//! parent.add_remote_subapp("dem", transport)
//!     .send_at_setup::<DemCriticalDt>()
//!     .send_each_iter::<SphereSet>()
//!     .recv_each_iter::<SphereForceSet>();
//! ```
//!
//! ## Send/recv ordering inside one mirror
//!
//! Every `send_*` registered on this side fires *before* every `recv_*`,
//! both inside `prepare()` (setup pumps) and inside `step()` (per-iter
//! pumps). Registration order is preserved within each list.
//!
//! Both peers send first, both peers recv second — fine as long as
//! messages fit in the wire's send buffer (true for typical MPI eager-mode
//! / TCP socket buffer / in-memory `mpsc`). Very large payloads with
//! tiny buffers could deadlock; document accordingly when you ship one.
//!
//! ## Wire format
//!
//! Every payload is a [`Transport::send`] of exactly the bytes
//! [`Wire::pack`] produced for the registered type. No framing, no type
//! tag — just the payload. The peer must unpack the same types in the
//! same order.

use crate::physics::{Physics, StepResult};
use crate::transport::Transport;
use crate::wire::Wire;
use grass_app::App;
use std::any::{Any, TypeId};
use std::cell::RefCell;

/// Type-erased "pack T from this App's resource into bytes" closure.
type PackFn = Box<dyn Fn(&App) -> Vec<u8> + Send + Sync>;
/// Type-erased "unpack bytes into this App's T resource" closure.
type UnpackFn = Box<dyn Fn(&mut App, &[u8]) + Send + Sync>;

/// A sub-App backed by a [`Transport`] to a peer process.
///
/// Wraps an empty `App` whose resources mirror those of the peer's local
/// resources. [`Physics::prepare`] runs the one-shot handshake (setup
/// pumps); [`Physics::step`] runs the per-iter pumps. [`Physics::resource_cell`]
/// delegates to the inner App so [`Multi`](crate::Multi) reads work
/// transparently.
///
/// Construct via [`crate::MultiAppExt::add_remote_subapp`] — the builder
/// chain populates the pump lists, then drop registers the physics into the
/// parent's [`SubApps`](crate::SubApps).
pub struct RemoteMirrorPhysics {
    name: String,
    /// Holds the wire-replicated resources. `inner.run()` is never called —
    /// the App is just a typed resource bag.
    inner: App,
    transport: Box<dyn Transport>,
    senders_at_setup: Vec<PackFn>,
    receivers_at_setup: Vec<UnpackFn>,
    senders_each_iter: Vec<PackFn>,
    receivers_each_iter: Vec<UnpackFn>,
}

impl RemoteMirrorPhysics {
    pub fn new(name: impl Into<String>, transport: Box<dyn Transport>) -> Self {
        Self {
            name: name.into(),
            inner: App::new(),
            transport,
            senders_at_setup: Vec::new(),
            receivers_at_setup: Vec::new(),
            senders_each_iter: Vec::new(),
            receivers_each_iter: Vec::new(),
        }
    }

    /// Register `T` as a setup-time send. The inner App gets `T::default()`
    /// pre-populated so callers writing to `Multi::write::<T>(ns)` between
    /// builder chain and first prepare see a sane value, not a missing
    /// resource.
    pub fn add_send_at_setup<T: Default + Wire + 'static>(&mut self) {
        self.ensure_resource::<T>();
        self.senders_at_setup.push(Box::new(pack_resource::<T>));
    }
    /// Register `T` as a setup-time recv.
    pub fn add_recv_at_setup<T: Default + Wire + 'static>(&mut self) {
        self.ensure_resource::<T>();
        self.receivers_at_setup
            .push(Box::new(unpack_into_resource::<T>));
    }
    /// Register `T` as a per-iter send.
    pub fn add_send_each_iter<T: Default + Wire + 'static>(&mut self) {
        self.ensure_resource::<T>();
        self.senders_each_iter.push(Box::new(pack_resource::<T>));
    }
    /// Register `T` as a per-iter recv.
    pub fn add_recv_each_iter<T: Default + Wire + 'static>(&mut self) {
        self.ensure_resource::<T>();
        self.receivers_each_iter
            .push(Box::new(unpack_into_resource::<T>));
    }

    /// Register `T` as a resource on the mirror without any wire pump.
    /// Useful when a parent system writes to `Multi::write::<T>(ns)` for
    /// a mirror namespace but the data doesn't need to cross the wire
    /// (write-only scratch on the mirror side; reads happen on the peer).
    pub fn add_local_resource<T: Default + 'static>(&mut self) {
        self.ensure_resource::<T>();
    }

    /// Drop a `T::default()` into the inner App if no `T` is registered yet.
    /// Idempotent — safe to call from every builder method.
    fn ensure_resource<T: Default + 'static>(&mut self) {
        if self.inner.get_mut_resource(TypeId::of::<T>()).is_none() {
            self.inner.add_resource(T::default());
        }
    }
}

impl Physics for RemoteMirrorPhysics {
    fn name(&self) -> &str {
        &self.name
    }

    fn prepare(&mut self) {
        // One-shot handshake. Sends first, then recvs — the peer mirrors
        // this so both sides' sends complete before either side blocks on
        // recv. Buffering assumption documented at module level.
        for pack in &self.senders_at_setup {
            let payload = pack(&self.inner);
            self.transport.send(&payload);
        }
        for unpack in &self.receivers_at_setup {
            let body = self.transport.recv();
            unpack(&mut self.inner, &body);
        }
    }

    fn step(&mut self) -> StepResult {
        for pack in &self.senders_each_iter {
            let payload = pack(&self.inner);
            self.transport.send(&payload);
        }
        for unpack in &self.receivers_each_iter {
            let body = self.transport.recv();
            unpack(&mut self.inner, &body);
        }
        StepResult::default()
    }

    fn is_done(&self) -> bool {
        // The peer signals its own done-ness via its own scheduler; the
        // mirror never reports done. Use a separate transport message or a
        // user-defined `recv_each_iter::<Bool>` if you need the peer's
        // done-state on this side.
        false
    }

    fn cleanup(&mut self) {
        // Peer cleanup is the peer's responsibility; the wire just stops.
    }

    fn resource_cell(&self, ty: TypeId) -> Option<&RefCell<Box<dyn Any>>> {
        self.inner.resource_cell(ty)
    }
}

// ─── Pack / unpack helpers (monomorphised per T) ────────────────────────────

fn pack_resource<T: Wire + 'static>(app: &App) -> Vec<u8> {
    let cell = app.resource_cell(TypeId::of::<T>()).unwrap_or_else(|| {
        panic!(
            "RemoteMirrorPhysics: no resource of type `{}` registered on the inner App",
            std::any::type_name::<T>()
        )
    });
    let g = cell.borrow();
    let value: &T = g
        .downcast_ref::<T>()
        .expect("RemoteMirrorPhysics: inner resource type mismatch (impossible)");
    value.pack()
}

fn unpack_into_resource<T: Wire + 'static>(app: &mut App, buf: &[u8]) {
    let unpacked = T::unpack(buf);
    let cell = app.get_mut_resource(TypeId::of::<T>()).unwrap_or_else(|| {
        panic!(
            "RemoteMirrorPhysics: no resource of type `{}` registered on the inner App",
            std::any::type_name::<T>()
        )
    });
    let mut g = cell.borrow_mut();
    let slot: &mut T = g
        .downcast_mut::<T>()
        .expect("RemoteMirrorPhysics: inner resource type mismatch (impossible)");
    *slot = unpacked;
}
