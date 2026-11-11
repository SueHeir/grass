//! Mapped ports — coupling solvers that do not share a discretization.
//!
//! A plain [`Port<T>`](crate::Port) hands the consumer exactly what the
//! producer published, so both sides must agree on the layout of `T`. Real
//! coupled codes rarely do: a flow solver's wall triangles are not a heat
//! shield's surface nodes, particles sit inside fluid cells, a radiation
//! solver wants the flow field on its own grid.
//!
//! A [`MappedPort<Q>`] carries a field *together with the discretization it
//! lives on*. Each consumer names its own discretization, and the port maps
//! the field onto it with [`grass_map`]. The shared contract is a
//! [`Quantity`] marker type, which fixes the field kind and therefore the
//! mapping direction:
//!
//! * [`Intensive`](grass_map::Intensive) and [`Density`](grass_map::Density)
//!   quantities (temperature, heat flux) are **sampled**: constants survive.
//! * [`Extensive`](grass_map::Extensive) quantities (force, heat) are
//!   **redistributed**: totals survive.
//!
//! ```rust,ignore
//! use grass_multi::{consume_on, expose_on, Mesh, MultiAppExt, Quantity};
//! use grass_map::{Intensive, Method};
//!
//! // The contract: wall temperature, sampled.
//! struct WallTemperature;
//! impl Quantity for WallTemperature { type Kind = Intensive; }
//!
//! parent.add_mapped_port::<WallTemperature>(Method::Native);
//! parent.add_update_system(
//!     expose_on::<Shell, WallTemperature>("shell", |s| (s.surface.clone(), s.t_wall())),
//!     Phase::Couple);
//! parent.add_update_system(
//!     consume_on::<Flow, WallTemperature>("flow",
//!         |f| f.wall.clone(),                    // where the flow wants it
//!         |f, t, _report| f.set_wall_t(t)),      // already on the flow's wall
//!     Phase::Couple);
//! ```
//!
//! Neither solver knows the other's mesh. Swap the shell for a finer one, or
//! the flow wall for a particle cloud, and the coupling code is unchanged.
//!
//! ## Meshes and caching
//!
//! A [`Mesh`] is a cheap, clonable handle to a shared
//! [`Discretization`]. Every [`Mesh::new`] gets a fresh identity. A consumer
//! builds its map the first time it runs and reuses it for as long as both
//! sides keep handing out the *same* `Mesh` (the same handle or clones of
//! it). A solver that remeshes, or whose particles move, just creates a new
//! `Mesh`, and the next exchange rebuilds the map. Keep the `Mesh` in the
//! solver's state and return clones of it; do not call `Mesh::new` every
//! step on a static geometry, or every exchange pays for a rebuild.
//!
//! ## Ordering
//!
//! As for plain ports: `expose_on` after the producer's tick, `consume_on`
//! before the consumer's.

use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use grass_map::{Discretization, Field, FieldKind, Map, MapBuilder, MapReport, Method, Sampling};
use grass_scheduler::{Res, ResMut};

use crate::multi::Multi;

static NEXT_MESH_ID: AtomicU64 = AtomicU64::new(1);

/// A shared handle to a [`Discretization`], with an identity used to cache
/// maps. Clones share the identity; every [`Mesh::new`] gets a fresh one.
#[derive(Clone)]
pub struct Mesh {
    geom: Rc<dyn Discretization>,
    id: u64,
}

impl Mesh {
    /// Wrap a discretization in a new handle with a fresh identity.
    pub fn new(geom: impl Discretization + 'static) -> Self {
        Self::from_rc(Rc::new(geom))
    }

    /// Wrap an already shared discretization with a fresh identity.
    pub fn from_rc(geom: Rc<dyn Discretization>) -> Self {
        Self {
            geom,
            id: NEXT_MESH_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// The identity maps are cached under.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The discretization.
    pub fn geom(&self) -> &dyn Discretization {
        &*self.geom
    }
}

impl Deref for Mesh {
    type Target = dyn Discretization;
    fn deref(&self) -> &Self::Target {
        &*self.geom
    }
}

impl fmt::Debug for Mesh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mesh")
            .field("id", &self.id)
            .field("len", &self.geom.len())
            .field("topo_dim", &self.geom.topo_dim())
            .finish()
    }
}

/// A coupling contract for a [`MappedPort`]: a marker type naming one
/// physical quantity, with the field kind that decides how it maps.
///
/// ```
/// use grass_multi::Quantity;
/// struct WallHeat;
/// impl Quantity for WallHeat { type Kind = grass_map::Extensive; }
/// ```
pub trait Quantity: 'static {
    /// How the quantity transforms under mapping.
    type Kind: FieldKind;
}

/// The field a producer published, with the mesh it lives on.
pub struct Published<K: FieldKind> {
    /// Where the values live.
    pub mesh: Mesh,
    /// One entry (of `ncomp` components) per mesh entity.
    pub field: Field<K>,
}

/// A typed exchange slot that maps its field onto each consumer's own
/// discretization. Register with
/// [`MultiAppExt::add_mapped_port`](crate::MultiAppExt::add_mapped_port).
pub struct MappedPort<Q: Quantity> {
    published: Option<Published<Q::Kind>>,
    method: Method,
    sampling: Option<Sampling>,
    builds: Cell<u64>,
    _q: PhantomData<Q>,
}

impl<Q: Quantity> MappedPort<Q> {
    /// An empty port that maps with `method`.
    pub fn new(method: Method) -> Self {
        Self {
            published: None,
            method,
            sampling: None,
            builds: Cell::new(0),
            _q: PhantomData,
        }
    }

    /// Override the sampler's default
    /// [`Sampling`](grass_map::Discretization::default_sampling).
    pub fn with_sampling(mut self, sampling: Sampling) -> Self {
        self.sampling = Some(sampling);
        self
    }

    /// Publish `field` on `mesh`, replacing any earlier value.
    ///
    /// # Panics
    /// Panics if `field` does not have one entry per mesh entity.
    pub fn publish(&mut self, mesh: Mesh, field: Field<Q::Kind>) {
        assert_eq!(
            field.len(),
            mesh.len(),
            "MappedPort::publish: field has {} entries but the mesh has {} entities",
            field.len(),
            mesh.len()
        );
        self.published = Some(Published { mesh, field });
    }

    /// The latest published value, if any.
    pub fn get(&self) -> Option<&Published<Q::Kind>> {
        self.published.as_ref()
    }

    /// Build the map from the published mesh to `dst`.
    pub fn build_map(&self, src: &Mesh, dst: &Mesh) -> Map<Q::Kind> {
        self.builds.set(self.builds.get() + 1);
        let mut b = MapBuilder::new(src.geom(), dst.geom()).method(self.method);
        if let Some(s) = self.sampling {
            b = b.sampling(s);
        }
        b.build::<Q::Kind>()
    }

    /// How many maps consumers have built from this port (useful to check
    /// that a static coupling is not rebuilding every step).
    pub fn map_builds(&self) -> u64 {
        self.builds.get()
    }
}

/// Build a system that publishes a field from sub-App `ns` into
/// `MappedPort<Q>`. `extract` returns the producer's mesh handle and the
/// field on it.
pub fn expose_on<Src, Q>(
    ns: &'static str,
    extract: impl Fn(&Src) -> (Mesh, Field<Q::Kind>) + 'static,
) -> impl FnMut(Multi, ResMut<MappedPort<Q>>)
where
    Src: 'static,
    Q: Quantity,
{
    move |world: Multi, mut port: ResMut<MappedPort<Q>>| {
        let src = world.expect_read::<Src>(ns);
        let (mesh, field) = extract(&src);
        port.publish(mesh, field);
    }
}

/// Build a system that maps `MappedPort<Q>` onto sub-App `ns`'s own mesh and
/// hands it to `apply`.
///
/// `mesh_of` returns the consumer's mesh handle. The field passed to `apply`
/// has one entry per consumer entity. Entities the map could not reach are
/// zero and listed in the [`MapReport`] (for a sampled quantity the list is
/// of consumer entities; for an extensive one, of producer entities whose
/// amounts were dropped). The map is cached until either mesh handle
/// changes. Does nothing until the port has been published.
pub fn consume_on<Dst, Q>(
    ns: &'static str,
    mesh_of: impl Fn(&Dst) -> Mesh + 'static,
    apply: impl Fn(&mut Dst, &Field<Q::Kind>, &MapReport) + 'static,
) -> impl FnMut(Multi, Res<MappedPort<Q>>)
where
    Dst: 'static,
    Q: Quantity,
{
    let mut cache: Option<(u64, u64, Map<Q::Kind>)> = None;
    move |world: Multi, port: Res<MappedPort<Q>>| {
        let Some(published) = port.get() else {
            return;
        };
        let mut dst = world.expect_write::<Dst>(ns);
        let dst_mesh = mesh_of(&dst);
        let key = (published.mesh.id(), dst_mesh.id());
        let fresh = matches!(&cache, Some((s, d, _)) if (*s, *d) == key);
        if !fresh {
            cache = Some((key.0, key.1, port.build_map(&published.mesh, &dst_mesh)));
        }
        let map = &cache.as_ref().expect("map cached above").2;
        let mut out = Field::zeros(dst_mesh.len(), published.field.ncomp);
        map.apply(&published.field, &mut out);
        apply(&mut dst, &out, map.report());
    }
}
