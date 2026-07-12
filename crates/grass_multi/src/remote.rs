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
use crate::transport::{Transport, TransportError};
use crate::wire::{Wire, WireUnpackError};
use grass_app::App;
use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

/// Type-erased "pack T from this App's resource into bytes" closure.
type PackFn = Box<dyn Fn(&App) -> Vec<u8> + Send + Sync>;
/// Type-erased "unpack bytes into this App's T resource" closure.
type UnpackFn = Box<
    dyn Fn(&mut App, &[u8], &str, RemotePumpPhase, usize) -> Result<(), RemoteUnpackError>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, Copy, Default)]
struct MirrorCoherence {
    receives: bool,
    fresh: bool,
    dirty: bool,
}

/// Observable local coherence state for one resource on a remote mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteResourceCoherence {
    /// Whether this resource is populated by a receive pump.
    pub receives: bool,
    /// Whether the most recent required receive completed successfully.
    pub fresh: bool,
    /// Whether local code has mutably borrowed it since the last send.
    pub dirty: bool,
}

/// Whether a failed remote decode happened during setup or an iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePumpPhase {
    /// The one-shot setup pump driven by [`RemoteMirrorPhysics::prepare`].
    Setup,
    /// The per-iteration pump driven by [`RemoteMirrorPhysics::step`].
    EachIter,
}

impl fmt::Display for RemotePumpPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup => f.write_str("setup"),
            Self::EachIter => f.write_str("each-iter"),
        }
    }
}

/// Contextual error returned when a remote payload cannot be decoded into the
/// resource registered for that receive slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUnpackError {
    mirror_name: String,
    phase: RemotePumpPhase,
    recv_index: usize,
    resource_type: &'static str,
    payload_len: usize,
    source: WireUnpackError,
}

impl RemoteUnpackError {
    fn new<T: 'static>(
        mirror_name: &str,
        phase: RemotePumpPhase,
        recv_index: usize,
        payload_len: usize,
        source: WireUnpackError,
    ) -> Self {
        Self {
            mirror_name: mirror_name.to_string(),
            phase,
            recv_index,
            resource_type: std::any::type_name::<T>(),
            payload_len,
            source,
        }
    }

    /// Name of the remote mirror sub-App that received the payload.
    pub fn mirror_name(&self) -> &str {
        &self.mirror_name
    }

    /// Pump phase where the decode failed.
    pub fn phase(&self) -> RemotePumpPhase {
        self.phase
    }

    /// Zero-based index within the phase's registered receive list.
    pub fn recv_index(&self) -> usize {
        self.recv_index
    }

    /// Rust resource type expected by this receive slot.
    pub fn resource_type(&self) -> &'static str {
        self.resource_type
    }

    /// Number of bytes received from the transport.
    pub fn payload_len(&self) -> usize {
        self.payload_len
    }

    /// Lower-level wire decode error.
    pub fn source(&self) -> &WireUnpackError {
        &self.source
    }
}

impl fmt::Display for RemoteUnpackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RemoteMirrorPhysics `{}` failed to unpack {} recv slot #{} as `{}` from {} bytes: {}",
            self.mirror_name,
            self.phase,
            self.recv_index,
            self.resource_type,
            self.payload_len,
            self.source.detail()
        )
    }
}

impl std::error::Error for RemoteUnpackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Direction of a remote transport pump slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePumpDirection {
    /// Payload was being sent from this mirror to the peer.
    Send,
    /// Payload was being received from the peer into this mirror.
    Recv,
}

impl fmt::Display for RemotePumpDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Send => f.write_str("send"),
            Self::Recv => f.write_str("recv"),
        }
    }
}

/// Contextual error returned when a remote transport send or recv fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTransportError {
    mirror_name: String,
    phase: RemotePumpPhase,
    direction: RemotePumpDirection,
    slot_index: usize,
    payload_len: Option<usize>,
    source: TransportError,
}

impl RemoteTransportError {
    fn send(
        mirror_name: &str,
        phase: RemotePumpPhase,
        slot_index: usize,
        payload_len: usize,
        source: TransportError,
    ) -> Self {
        Self {
            mirror_name: mirror_name.to_string(),
            phase,
            direction: RemotePumpDirection::Send,
            slot_index,
            payload_len: Some(payload_len),
            source,
        }
    }

    fn recv(
        mirror_name: &str,
        phase: RemotePumpPhase,
        slot_index: usize,
        source: TransportError,
    ) -> Self {
        Self {
            mirror_name: mirror_name.to_string(),
            phase,
            direction: RemotePumpDirection::Recv,
            slot_index,
            payload_len: None,
            source,
        }
    }

    /// Name of the remote mirror sub-App whose transport failed.
    pub fn mirror_name(&self) -> &str {
        &self.mirror_name
    }

    /// Pump phase where transport failed.
    pub fn phase(&self) -> RemotePumpPhase {
        self.phase
    }

    /// Whether the failure happened while sending or receiving.
    pub fn direction(&self) -> RemotePumpDirection {
        self.direction
    }

    /// Zero-based index within the phase's registered send or receive list.
    pub fn slot_index(&self) -> usize {
        self.slot_index
    }

    /// Number of bytes in the outgoing payload, for send failures.
    pub fn payload_len(&self) -> Option<usize> {
        self.payload_len
    }

    /// Lower-level transport error.
    pub fn source(&self) -> &TransportError {
        &self.source
    }
}

impl fmt::Display for RemoteTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.payload_len {
            Some(payload_len) => write!(
                f,
                "RemoteMirrorPhysics `{}` failed to {} {} {} slot #{} ({} bytes): {}",
                self.mirror_name,
                self.direction,
                self.phase,
                self.direction,
                self.slot_index,
                payload_len,
                self.source
            ),
            None => write!(
                f,
                "RemoteMirrorPhysics `{}` failed to {} {} {} slot #{}: {}",
                self.mirror_name,
                self.direction,
                self.phase,
                self.direction,
                self.slot_index,
                self.source
            ),
        }
    }
}

impl std::error::Error for RemoteTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Contextual error returned by fallible remote pump entry points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemotePumpError {
    /// A transport send or recv failed.
    Transport(RemoteTransportError),
    /// A received payload could not be decoded into the registered resource.
    Unpack(RemoteUnpackError),
}

impl RemotePumpError {
    /// Returns the transport diagnostic when this is a transport failure.
    pub fn transport(&self) -> Option<&RemoteTransportError> {
        match self {
            Self::Transport(err) => Some(err),
            Self::Unpack(_) => None,
        }
    }

    /// Returns the unpack diagnostic when this is a decode failure.
    pub fn unpack(&self) -> Option<&RemoteUnpackError> {
        match self {
            Self::Transport(_) => None,
            Self::Unpack(err) => Some(err),
        }
    }
}

impl fmt::Display for RemotePumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(err) => fmt::Display::fmt(err, f),
            Self::Unpack(err) => fmt::Display::fmt(err, f),
        }
    }
}

impl std::error::Error for RemotePumpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(err) => Some(err),
            Self::Unpack(err) => Some(err),
        }
    }
}

impl From<RemoteUnpackError> for RemotePumpError {
    fn from(value: RemoteUnpackError) -> Self {
        Self::Unpack(value)
    }
}

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
    sender_types_at_setup: Vec<TypeId>,
    receivers_at_setup: Vec<UnpackFn>,
    receiver_types_at_setup: Vec<TypeId>,
    senders_each_iter: Vec<PackFn>,
    sender_types_each_iter: Vec<TypeId>,
    receivers_each_iter: Vec<UnpackFn>,
    receiver_types_each_iter: Vec<TypeId>,
    coherence: RefCell<HashMap<TypeId, MirrorCoherence>>,
}

impl RemoteMirrorPhysics {
    /// Creates a mirror physics under `name`, backed by `transport` for
    /// cross-process resource replication. The pump lists start empty;
    /// populate them via the [`MultiAppExt::add_remote_subapp`](crate::MultiAppExt::add_remote_subapp)
    /// builder chain before registration.
    pub fn new(name: impl Into<String>, transport: Box<dyn Transport>) -> Self {
        Self {
            name: name.into(),
            inner: App::new(),
            transport,
            senders_at_setup: Vec::new(),
            sender_types_at_setup: Vec::new(),
            receivers_at_setup: Vec::new(),
            receiver_types_at_setup: Vec::new(),
            senders_each_iter: Vec::new(),
            sender_types_each_iter: Vec::new(),
            receivers_each_iter: Vec::new(),
            receiver_types_each_iter: Vec::new(),
            coherence: RefCell::new(HashMap::new()),
        }
    }

    /// Register `T` as a setup-time send. The inner App gets `T::default()`
    /// pre-populated so callers writing to `Multi::write::<T>(ns)` between
    /// builder chain and first prepare see a sane value, not a missing
    /// resource.
    pub fn add_send_at_setup<T: Default + Wire + 'static>(&mut self) {
        self.ensure_resource::<T>();
        self.coherence
            .get_mut()
            .entry(TypeId::of::<T>())
            .or_default();
        self.senders_at_setup.push(Box::new(pack_resource::<T>));
        self.sender_types_at_setup.push(TypeId::of::<T>());
    }
    /// Register `T` as a setup-time recv.
    pub fn add_recv_at_setup<T: Default + Wire + 'static>(&mut self) {
        self.ensure_resource::<T>();
        self.register_receiver::<T>();
        self.receivers_at_setup
            .push(Box::new(unpack_into_resource::<T>));
        self.receiver_types_at_setup.push(TypeId::of::<T>());
    }
    /// Register `T` as a per-iter send.
    pub fn add_send_each_iter<T: Default + Wire + 'static>(&mut self) {
        self.ensure_resource::<T>();
        self.coherence
            .get_mut()
            .entry(TypeId::of::<T>())
            .or_default();
        self.senders_each_iter.push(Box::new(pack_resource::<T>));
        self.sender_types_each_iter.push(TypeId::of::<T>());
    }
    /// Register `T` as a per-iter recv.
    pub fn add_recv_each_iter<T: Default + Wire + 'static>(&mut self) {
        self.ensure_resource::<T>();
        self.register_receiver::<T>();
        self.receivers_each_iter
            .push(Box::new(unpack_into_resource::<T>));
        self.receiver_types_each_iter.push(TypeId::of::<T>());
    }

    /// Register `T` as a resource on the mirror without any wire pump.
    /// Useful when a parent system writes to `Multi::write::<T>(ns)` for
    /// a mirror namespace but the data doesn't need to cross the wire
    /// (write-only scratch on the mirror side; reads happen on the peer).
    pub fn add_local_resource<T: Default + 'static>(&mut self) {
        self.ensure_resource::<T>();
    }

    fn register_receiver<T: 'static>(&mut self) {
        let state = self
            .coherence
            .get_mut()
            .entry(TypeId::of::<T>())
            .or_default();
        state.receives = true;
        state.fresh = false;
    }

    /// Return the mirror-local coherence metadata for `T`, if `T` participates
    /// in a wire pump. This inspection is local and never touches transport.
    pub fn resource_coherence<T: 'static>(&self) -> Option<RemoteResourceCoherence> {
        self.coherence
            .borrow()
            .get(&TypeId::of::<T>())
            .map(|s| RemoteResourceCoherence {
                receives: s.receives,
                fresh: s.fresh,
                dirty: s.dirty,
            })
    }

    /// Drop a `T::default()` into the inner App if no `T` is registered yet.
    /// Idempotent — safe to call from every builder method.
    fn ensure_resource<T: Default + 'static>(&mut self) {
        if self.inner.get_mut_resource(TypeId::of::<T>()).is_none() {
            self.inner.add_resource(T::default());
        }
    }

    /// Fallible form of [`Physics::prepare`], returning contextual transport
    /// and wire diagnostics instead of panicking on malformed received
    /// payloads or disconnected peers.
    pub fn try_prepare(&mut self) -> Result<(), RemotePumpError> {
        // One-shot handshake. Sends first, then recvs — the peer mirrors
        // this so both sides' sends complete before either side blocks on
        // recv. Buffering assumption documented at module level.
        for (slot_index, pack) in self.senders_at_setup.iter().enumerate() {
            let payload = pack(&self.inner);
            self.transport.try_send(&payload).map_err(|source| {
                RemotePumpError::Transport(RemoteTransportError::send(
                    &self.name,
                    RemotePumpPhase::Setup,
                    slot_index,
                    payload.len(),
                    source,
                ))
            })?;
        }
        for ty in &self.sender_types_at_setup {
            self.coherence.get_mut().get_mut(ty).unwrap().dirty = false;
        }
        for ty in &self.receiver_types_at_setup {
            self.coherence.get_mut().get_mut(ty).unwrap().fresh = false;
        }
        for (recv_index, unpack) in self.receivers_at_setup.iter().enumerate() {
            let body = self.transport.try_recv().map_err(|source| {
                RemotePumpError::Transport(RemoteTransportError::recv(
                    &self.name,
                    RemotePumpPhase::Setup,
                    recv_index,
                    source,
                ))
            })?;
            unpack(
                &mut self.inner,
                &body,
                &self.name,
                RemotePumpPhase::Setup,
                recv_index,
            )
            .map_err(RemotePumpError::Unpack)?;
        }
        for ty in &self.receiver_types_at_setup {
            self.coherence.get_mut().get_mut(ty).unwrap().fresh = true;
        }
        Ok(())
    }

    /// Fallible form of [`Physics::step`], returning contextual transport and
    /// wire diagnostics instead of panicking on malformed received payloads or
    /// disconnected peers.
    pub fn try_step(&mut self) -> Result<StepResult, RemotePumpError> {
        // Once a new pump begins, the prior iteration's received values are
        // stale. Invalidate before any fallible send so every early return
        // fails closed on subsequent reads.
        for ty in &self.receiver_types_each_iter {
            self.coherence.get_mut().get_mut(ty).unwrap().fresh = false;
        }
        for (slot_index, pack) in self.senders_each_iter.iter().enumerate() {
            let payload = pack(&self.inner);
            self.transport.try_send(&payload).map_err(|source| {
                RemotePumpError::Transport(RemoteTransportError::send(
                    &self.name,
                    RemotePumpPhase::EachIter,
                    slot_index,
                    payload.len(),
                    source,
                ))
            })?;
        }
        for ty in &self.sender_types_each_iter {
            self.coherence.get_mut().get_mut(ty).unwrap().dirty = false;
        }
        for (recv_index, unpack) in self.receivers_each_iter.iter().enumerate() {
            let body = self.transport.try_recv().map_err(|source| {
                RemotePumpError::Transport(RemoteTransportError::recv(
                    &self.name,
                    RemotePumpPhase::EachIter,
                    recv_index,
                    source,
                ))
            })?;
            unpack(
                &mut self.inner,
                &body,
                &self.name,
                RemotePumpPhase::EachIter,
                recv_index,
            )
            .map_err(RemotePumpError::Unpack)?;
        }
        for ty in &self.receiver_types_each_iter {
            self.coherence.get_mut().get_mut(ty).unwrap().fresh = true;
        }
        Ok(StepResult::default())
    }
}

impl Physics for RemoteMirrorPhysics {
    fn name(&self) -> &str {
        &self.name
    }

    fn prepare(&mut self) {
        self.try_prepare().unwrap_or_else(|err| panic!("{err}"));
    }

    fn step(&mut self) -> StepResult {
        self.try_step().unwrap_or_else(|err| panic!("{err}"))
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

    fn validate_resource_read(&self, ty: TypeId, type_name: &'static str) {
        if let Some(state) = self.coherence.borrow().get(&ty) {
            if state.receives && !state.fresh {
                panic!(
                    "RemoteMirrorPhysics `{}`: stale read of `{type_name}`; its receive pump has not completed successfully",
                    self.name
                );
            }
        }
    }

    fn mark_resource_written(&self, ty: TypeId) {
        if let Some(state) = self.coherence.borrow_mut().get_mut(&ty) {
            state.dirty = true;
        }
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

fn unpack_into_resource<T: Wire + 'static>(
    app: &mut App,
    buf: &[u8],
    mirror_name: &str,
    phase: RemotePumpPhase,
    recv_index: usize,
) -> Result<(), RemoteUnpackError> {
    let unpacked = T::try_unpack(buf).map_err(|source| {
        RemoteUnpackError::new::<T>(mirror_name, phase, recv_index, buf.len(), source)
    })?;
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
    Ok(())
}
