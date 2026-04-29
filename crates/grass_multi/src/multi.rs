//! [`Multi`] — first-class [`SystemParam`] for cross-namespace resource
//! access from a parent App's systems.
//!
//! `Multi` lets ordinary systems registered on a parent App read and
//! write resources from any registered sub-App by namespace string:
//!
//! ```rust,ignore
//! fn sync_dt(world: Multi) {
//!     let cfd_dt = world.read::<SolverState>("cfd").dt;
//!     if cfd_dt > 0.0 {
//!         let crit = world.read::<DemCriticalDt>("dem").0;
//!         world.write::<Atom>("dem").dt = cfd_dt.min(crit);
//!     }
//! }
//!
//! parent.add_subapp("dem", dem_app);
//! parent.add_subapp("cfd", cfd_app);
//! parent.add_update_system(sync_dt, MyPhase::Coupling);
//! parent.add_update_system(tick_subapp("dem", 1), MyPhase::Tick);
//! parent.add_update_system(tick_subapp("cfd", 3), MyPhase::Tick);
//! ```
//!
//! For namespaces fixed at compile time, prefer the typed
//! [`MultiRes<T, NS>`](crate::MultiRes) /
//! [`MultiResMut<T, NS>`](crate::MultiResMut) variants — same machinery,
//! typo-safe.
//!
//! ## Design notes
//!
//! - `SubApps` is an owning resource on the parent App: it holds
//!   `Vec<Box<dyn Physics>>` and a name → index map.
//! - `Multi<'w>` is the SystemParam — a thin newtype around `Res<'w, SubApps>`.
//!   Its `read::<T>(ns)` / `write::<T>(ns)` methods take `&self` so a single
//!   system can hold multiple borrows simultaneously (e.g. read `"cfd"` and
//!   write `"dem"` in the same expression). Borrow isolation comes from the
//!   `RefCell` on each resource, not from `Multi` itself.
//! - `MultiRef<T>` / `MultiMut<T>` are typed handles produced by those
//!   methods. They wrap `Ref<'_, T>` / `RefMut<'_, T>` and `Deref` to `&T` /
//!   `&mut T` so the system body reads naturally.
//! - `tick_subapp(name, n)` is a system constructor: returns a closure that
//!   takes `ResMut<SubApps>` and advances the named sub-App `n` times. Use
//!   it when the parent App's scheduler should drive sub-App ticks (the
//!   default Tier-0 model).

use crate::physics::{AppPhysics, Physics};
use crate::remote::RemoteMirrorPhysics;
use crate::transport::Transport;
use grass_app::App;
use grass_scheduler::{Res, ResMut, SystemParam};
use std::any::{Any, TypeId};
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

// ─── SubApps ────────────────────────────────────────────────────────────────

/// Owning collection of named sub-Apps registered on a parent App.
///
/// Lives as a normal `Resource` on the parent (added via
/// [`MultiAppExt::add_subapp`]). [`Multi`] reads it via `Res<SubApps>` to
/// resolve cross-namespace lookups; [`tick_subapp`] writes it via
/// `ResMut<SubApps>` to drive a sub-App's step loop.
///
/// Designed to outlive any single `Multi` borrow — the `Vec<Box<dyn Physics>>`
/// is stable for the lifetime of the parent App.
pub struct SubApps {
    physics: Vec<Box<dyn Physics>>,
    name_to_idx: HashMap<String, usize>,
    /// Per-physics flag — `true` once `prepare()` has been called for that
    /// sub-App. We track it here (not on `Physics`) so the tick loop is
    /// idempotent regardless of whether a `Physics` impl makes its own
    /// `prepare` idempotent.
    prepared: Vec<bool>,
}

impl Default for SubApps {
    fn default() -> Self {
        Self::new()
    }
}

impl SubApps {
    pub fn new() -> Self {
        Self {
            physics: Vec::new(),
            name_to_idx: HashMap::new(),
            prepared: Vec::new(),
        }
    }

    /// Register a new physics under its `name()`. Panics on duplicate names.
    pub fn register(&mut self, p: Box<dyn Physics>) {
        let name = p.name().to_string();
        if self.name_to_idx.contains_key(&name) {
            panic!("SubApps: namespace `{name}` already registered");
        }
        let idx = self.physics.len();
        self.name_to_idx.insert(name, idx);
        self.physics.push(p);
        self.prepared.push(false);
    }

    /// Look up a physics by name. Returns `None` if no such namespace.
    pub fn find(&self, ns: &str) -> Option<&dyn Physics> {
        self.name_to_idx.get(ns).map(|&i| &*self.physics[i])
    }

    fn idx_of(&self, ns: &str) -> Option<usize> {
        self.name_to_idx.get(ns).copied()
    }

    /// Names of all registered sub-Apps. Useful for diagnostics.
    pub fn participants(&self) -> impl Iterator<Item = &str> {
        self.physics.iter().map(|p| p.name())
    }

    /// Advance the named sub-App by one step, calling `prepare()` first if
    /// this is the sub-App's first tick. Panics if the namespace is unknown.
    pub fn tick(&mut self, ns: &str) {
        let idx = self
            .idx_of(ns)
            .unwrap_or_else(|| panic!("SubApps::tick: unknown namespace `{ns}`"));
        if !self.prepared[idx] {
            self.physics[idx].prepare();
            self.prepared[idx] = true;
        }
        self.physics[idx].step();
    }

    /// Returns `true` if any registered sub-App has signalled `is_done()`.
    /// Usable as a parent-App stop condition.
    pub fn any_done(&self) -> bool {
        self.physics.iter().any(|p| p.is_done())
    }

    /// Run every sub-App's `cleanup()` exactly once. Call before drop when
    /// the parent App owns the orchestration loop.
    pub fn cleanup_all(&mut self) {
        for p in self.physics.iter_mut() {
            p.cleanup();
        }
    }
}

// ─── Multi system param ─────────────────────────────────────────────────────

/// Cross-namespace resource accessor as a [`SystemParam`].
///
/// A system that takes `world: Multi` can read or write resources from any
/// registered sub-App by namespace. The borrow lifetime is tied to the
/// `Res<SubApps>` it holds; per-resource borrows go through each sub-App's
/// `RefCell`, so multiple simultaneous reads on different namespaces (and
/// reads-on-A + writes-on-B in the same statement) all work.
pub struct Multi<'w> {
    inner: Res<'w, SubApps>,
}

impl<'w> Multi<'w> {
    /// Borrow a resource of type `T` from the named sub-App. Returns `None`
    /// if the namespace is unknown OR the sub-App has no resource of type `T`.
    pub fn read<T: 'static>(&self, ns: &str) -> Option<MultiRef<'_, T>> {
        let physics = self.inner.find(ns)?;
        let cell = physics.resource_cell(TypeId::of::<T>())?;
        Some(MultiRef {
            inner: Ref::map(cell.borrow(), |b| {
                b.downcast_ref::<T>()
                    .expect("Multi::read: resource type mismatch — registered under a different concrete type")
            }),
        })
    }

    /// Mutably borrow a resource of type `T` from the named sub-App.
    pub fn write<T: 'static>(&self, ns: &str) -> Option<MultiMut<'_, T>> {
        let physics = self.inner.find(ns)?;
        let cell = physics.resource_cell(TypeId::of::<T>())?;
        Some(MultiMut {
            inner: RefMut::map(cell.borrow_mut(), |b| {
                b.downcast_mut::<T>()
                    .expect("Multi::write: resource type mismatch")
            }),
        })
    }

    /// Same as [`read`](Self::read) but panics with a clear message when the
    /// namespace or resource is missing. Use when both are required by
    /// contract — most coupling code is in this category.
    pub fn expect_read<T: 'static>(&self, ns: &str) -> MultiRef<'_, T> {
        self.read::<T>(ns).unwrap_or_else(|| {
            panic!(
                "Multi::expect_read: namespace `{}` has no resource of type `{}`",
                ns,
                std::any::type_name::<T>()
            )
        })
    }

    /// Same as [`write`](Self::write) but panics with a clear message on
    /// missing namespace / resource.
    pub fn expect_write<T: 'static>(&self, ns: &str) -> MultiMut<'_, T> {
        self.write::<T>(ns).unwrap_or_else(|| {
            panic!(
                "Multi::expect_write: namespace `{}` has no resource of type `{}`",
                ns,
                std::any::type_name::<T>()
            )
        })
    }

    /// Names of all registered sub-Apps. Useful in coupler bodies that need
    /// to enumerate participants.
    pub fn participants(&self) -> impl Iterator<Item = &str> {
        self.inner.participants()
    }
}

impl<'w> SystemParam for Multi<'w> {
    type Item<'new> = Multi<'new>;
    fn retrieve<'r>(
        resources: &'r [RefCell<Box<dyn Any>>],
        index: usize,
        locals: *mut HashMap<TypeId, Box<dyn Any>>,
    ) -> Self::Item<'r> {
        let inner = <Res<'r, SubApps> as SystemParam>::retrieve(resources, index, locals);
        Multi { inner }
    }
    fn resource_type_id() -> Option<(TypeId, &'static str)> {
        Some((TypeId::of::<SubApps>(), "grass_multi::SubApps"))
    }
}

// ─── Typed handles ──────────────────────────────────────────────────────────

/// Read-only handle to a resource of type `T` in a named sub-App. Produced
/// by [`Multi::read`]. Derefs to `&T`.
pub struct MultiRef<'w, T: 'static> {
    inner: Ref<'w, T>,
}

impl<T: 'static> Deref for MultiRef<'_, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        &self.inner
    }
}

/// Mutable handle to a resource of type `T` in a named sub-App. Produced by
/// [`Multi::write`]. Derefs to `&mut T`.
pub struct MultiMut<'w, T: 'static> {
    inner: RefMut<'w, T>,
}

impl<T: 'static> Deref for MultiMut<'_, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T: 'static> DerefMut for MultiMut<'_, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

// ─── Namespace marker trait ─────────────────────────────────────────────────

/// Compile-time marker for a sub-App namespace.
///
/// Implementing types act as zero-sized handles for a runtime namespace
/// string. Use them to opt into typed registration / typed ticking instead
/// of passing namespace strings around (which lose type safety and only
/// fail at runtime on typo).
///
/// ```rust,ignore
/// pub struct DemNs;
/// impl Namespace for DemNs { const NAME: &'static str = "dem"; }
///
/// pub struct CfdNs;
/// impl Namespace for CfdNs { const NAME: &'static str = "cfd"; }
///
/// parent.add_subapp_typed::<DemNs>(dem_app);
/// parent.add_subapp_typed::<CfdNs>(cfd_app);
/// parent.add_update_system(tick_n_times::<CfdNs>(3), Phase::Tick);
/// parent.add_update_system(tick_n_times::<DemNs>(1), Phase::Tick);
/// ```
///
/// String-based [`MultiAppExt::add_subapp`] / [`tick_subapp`] remain
/// available for runtime-named cases (e.g. namespaces loaded from config).
pub trait Namespace: 'static {
    /// The runtime namespace string this marker represents.
    const NAME: &'static str;
}

/// Convenience macro for declaring a [`Namespace`] marker.
///
/// ```rust,ignore
/// namespace!(pub DemNs = "dem");
/// namespace!(pub CfdNs = "cfd");
/// ```
///
/// Expands to:
///
/// ```rust,ignore
/// pub struct DemNs;
/// impl Namespace for DemNs { const NAME: &'static str = "dem"; }
/// ```
#[macro_export]
macro_rules! namespace {
    ($vis:vis $name:ident = $ns:literal) => {
        $vis struct $name;
        impl $crate::Namespace for $name {
            const NAME: &'static str = $ns;
        }
    };
}

// ─── tick_subapp ────────────────────────────────────────────────────────────

/// System constructor: advance the named sub-App `n` times each time the
/// parent scheduler runs this system.
///
/// Use to place sub-App ticks explicitly in the parent's schedule. Pair
/// `n = 1` for single-rate coupling; use different `n` values across two
/// `tick_subapp` calls for fixed-ratio substepping (e.g. `n = 3` for one
/// physics paired with `n = 1` for another).
///
/// Returns a closure with signature `FnMut(ResMut<SubApps>)` — register it
/// like any other system:
///
/// ```rust,ignore
/// parent.add_update_system(tick_subapp("cfd", 3), MyPhase::Tick);
/// parent.add_update_system(tick_subapp("dem", 1), MyPhase::Tick);
/// ```
pub fn tick_subapp(name: &str, n: usize) -> impl FnMut(ResMut<SubApps>) {
    let name = name.to_string();
    move |mut subs: ResMut<SubApps>| {
        for _ in 0..n {
            subs.tick(&name);
        }
    }
}

/// Typed counterpart of [`tick_subapp`]. The sub-App is identified by a
/// [`Namespace`] marker type instead of a string. Equivalent to
/// `tick_subapp(NS::NAME, n)` but typo-safe at compile time.
pub fn tick_n_times<NS: Namespace>(n: usize) -> impl FnMut(ResMut<SubApps>) {
    move |mut subs: ResMut<SubApps>| {
        for _ in 0..n {
            subs.tick(NS::NAME);
        }
    }
}

// ─── App extension ──────────────────────────────────────────────────────────

/// Extension trait on [`App`] adding sub-App registration.
///
/// `parent.add_subapp("dem", dem_app)` either creates the [`SubApps`]
/// resource on the parent (first call) or registers into the existing one
/// (subsequent calls). The sub-App is wrapped in [`AppPhysics`] under the
/// hood, so its lifecycle (`prepare` → `step` × N → `cleanup`) plays nicely
/// with [`SubApps::tick`] and [`SubApps::cleanup_all`].
pub trait MultiAppExt {
    fn add_subapp(&mut self, name: &str, app: App) -> &mut Self;

    /// Typed counterpart of [`add_subapp`](Self::add_subapp). Registers
    /// `app` under `NS::NAME`. Use when the namespace is known at compile
    /// time and you'd rather catch typos than chase a runtime panic.
    fn add_subapp_typed<NS: Namespace>(&mut self, app: App) -> &mut Self {
        self.add_subapp(NS::NAME, app)
    }

    /// Register a remote (cross-process) sub-App backed by a [`Transport`].
    /// Returns a [`RemoteSubAppBuilder`] for declaring which resource types
    /// cross the wire, in which direction, and at what cadence:
    ///
    /// ```rust,ignore
    /// parent.add_remote_subapp("dem", transport)
    ///     .send_at_setup::<DemCriticalDt>()      // once at HandshakePhase
    ///     .send_each_iter::<SphereSet>()         // every parent iter
    ///     .recv_each_iter::<SphereForceSet>();
    /// ```
    ///
    /// The builder registers a [`RemoteMirrorPhysics`] with the parent's
    /// [`SubApps`] when it drops at the end of the chain — declaration is
    /// done in one statement.
    fn add_remote_subapp<Tr: Transport + 'static>(
        &mut self,
        name: &str,
        transport: Tr,
    ) -> RemoteSubAppBuilder<'_>;
}

impl MultiAppExt for App {
    fn add_subapp(&mut self, name: &str, app: App) -> &mut Self {
        let physics: Box<dyn Physics> = Box::new(AppPhysics::new(name.to_string(), app));
        register_physics(self, physics);
        self
    }

    fn add_remote_subapp<Tr: Transport + 'static>(
        &mut self,
        name: &str,
        transport: Tr,
    ) -> RemoteSubAppBuilder<'_> {
        let physics = RemoteMirrorPhysics::new(name.to_string(), Box::new(transport));
        RemoteSubAppBuilder {
            app: self,
            physics: Some(physics),
        }
    }
}

/// Common upsert path: register `physics` into the parent's [`SubApps`]
/// resource, creating the resource on first call.
fn register_physics(app: &mut App, physics: Box<dyn Physics>) {
    if app.get_mut_resource(TypeId::of::<SubApps>()).is_some() {
        let cell = app
            .get_mut_resource(TypeId::of::<SubApps>())
            .expect("SubApps existence checked just above");
        let mut g = cell.borrow_mut();
        let subs = g
            .downcast_mut::<SubApps>()
            .expect("SubApps: resource type mismatch");
        subs.register(physics);
    } else {
        let mut subs = SubApps::new();
        subs.register(physics);
        app.add_resource(subs);
    }
}

// ─── RemoteSubAppBuilder ────────────────────────────────────────────────────

/// Fluent builder returned by [`MultiAppExt::add_remote_subapp`]. Each chain
/// method extends the in-flight [`RemoteMirrorPhysics`] with one more
/// type-direction-cadence triple (e.g. `send_each_iter::<SphereSet>()`).
/// The accumulated physics is registered into the parent's [`SubApps`]
/// when the builder drops — typically at the end of the statement.
///
/// The `'a` lifetime ties the builder to its parent App, so multiple
/// concurrent builders for the same App can't coexist (each holds `&mut
/// App`); use one builder per `add_remote_subapp` call, completed in a
/// single statement.
pub struct RemoteSubAppBuilder<'a> {
    app: &'a mut App,
    /// `Some` while building, `None` after explicit `finish()` or after
    /// the `Drop` impl has handed the physics off to `SubApps`.
    physics: Option<RemoteMirrorPhysics>,
}

impl<'a> RemoteSubAppBuilder<'a> {
    fn with_physics<F: FnOnce(&mut RemoteMirrorPhysics)>(&mut self, f: F) {
        if let Some(p) = self.physics.as_mut() {
            f(p);
        } else {
            panic!(
                "RemoteSubAppBuilder: already finished — chain methods after .finish() are bugs"
            );
        }
    }

    /// Register `T` to be sent once at setup time (during `Physics::prepare`).
    /// Pair with the peer's matching `recv_at_setup::<T>` in the same order.
    pub fn send_at_setup<T: Default + crate::wire::Wire + 'static>(mut self) -> Self {
        self.with_physics(|p| p.add_send_at_setup::<T>());
        self
    }

    /// Register `T` to be received once at setup time. Mirror of
    /// [`send_at_setup`](Self::send_at_setup) — the peer pushes the value
    /// during its own prepare; this side reads it and overwrites the inner
    /// App's `T` resource.
    pub fn recv_at_setup<T: Default + crate::wire::Wire + 'static>(mut self) -> Self {
        self.with_physics(|p| p.add_recv_at_setup::<T>());
        self
    }

    /// Register `T` to be sent every iter (during `Physics::step`).
    pub fn send_each_iter<T: Default + crate::wire::Wire + 'static>(mut self) -> Self {
        self.with_physics(|p| p.add_send_each_iter::<T>());
        self
    }

    /// Register `T` to be received every iter, overwriting the inner App's
    /// `T` resource so [`Multi`]-using systems on this side see the freshly
    /// pumped value.
    pub fn recv_each_iter<T: Default + crate::wire::Wire + 'static>(mut self) -> Self {
        self.with_physics(|p| p.add_recv_each_iter::<T>());
        self
    }

    /// Register `T` as a resource on the mirror **without** any wire
    /// pump. Use when a system on the parent App writes to the mirror's
    /// `T` slot as a side effect (e.g., a coupling system that touches
    /// both sides symmetrically) but the data never needs to cross the
    /// wire — the receive side will write its own.
    pub fn with_resource<T: Default + 'static>(mut self) -> Self {
        self.with_physics(|p| p.add_local_resource::<T>());
        self
    }

    /// Explicitly finish the builder. Equivalent to letting it drop —
    /// useful in tests or when you'd rather not rely on drop ordering.
    pub fn finish(mut self) {
        if let Some(physics) = self.physics.take() {
            register_physics(self.app, Box::new(physics));
        }
    }
}

impl Drop for RemoteSubAppBuilder<'_> {
    fn drop(&mut self) {
        if let Some(physics) = self.physics.take() {
            register_physics(self.app, Box::new(physics));
        }
    }
}
