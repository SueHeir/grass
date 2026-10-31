//! The one trait every discretization implements.

use crate::geom::{Aabb, Point};

/// Topological dimension of a discretization's entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopoDim {
    /// Points or particles.
    Point,
    /// Curves, rays, fibers.
    Line,
    /// Surfaces: boundary faces, shells.
    Surface,
    /// Volumes: cells, elements.
    Volume,
}

/// One weighted sample point owned by an entity (degree of freedom).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SamplePoint {
    /// The entity the weight belongs to.
    pub dof: usize,
    /// Where the sample sits.
    pub point: Point,
    /// Its weight. For a quadrature rule this is the quadrature weight times
    /// the entity's test function, so the weights of one entity sum to its
    /// measure (length, area or volume).
    pub weight: f64,
}

/// How a sampler turns its entities into sample points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampling {
    /// Use [`Discretization::sample_points`]: every entity averages the other
    /// side over its own quadrature points (a lumped L2 projection). Natural
    /// for cell and face data, and for spreading loads.
    Average,
    /// One point per entity, at its [`centroid`](Discretization::centroid),
    /// with unit weight: plain interpolation. Natural for nodal data (mesh
    /// vertices, particles), and exact for fields the other side represents
    /// exactly.
    Centroid,
}

/// A discretization answers two questions about itself, and that is enough
/// to map data to or from any other discretization:
///
/// 1. [`sample_points`](Self::sample_points): *where and how do I integrate?*
///    Weighted points per entity, e.g. Gauss points of a cell, a particle
///    weighted by its volume, points along a ray.
/// 2. [`evaluate`](Self::evaluate): *what is my value at this point?* The
///    entities whose basis functions are nonzero at `p`, with their values,
///    e.g. the enclosing cell (piecewise constant), the eight cells around
///    `p` (trilinear), the vertices of the nearest triangle (barycentric).
///
/// For mapping to conserve totals and preserve constants, the basis values
/// returned by `evaluate` should form a partition of unity (sum to 1)
/// wherever the discretization claims support.
pub trait Discretization {
    /// Number of entities (degrees of freedom) data lives on.
    fn len(&self) -> usize;

    /// Whether there are no entities.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Topological dimension of the entities.
    fn topo_dim(&self) -> TopoDim;

    /// A representative point per entity (used by nearest-neighbor mapping).
    fn centroid(&self, i: usize) -> Point;

    /// Bounding box of everything this discretization covers.
    fn bounds(&self) -> Aabb;

    /// Append the weighted sample points of every entity to `out`.
    fn sample_points(&self, out: &mut Vec<SamplePoint>);

    /// Append `(entity, basis value)` pairs for the entities supported at
    /// `p`. Returns `false` (and appends nothing) if `p` is outside this
    /// discretization's support.
    fn evaluate(&self, p: Point, out: &mut Vec<(usize, f64)>) -> bool;

    /// The sampling used when this discretization is the sampler and the
    /// map builder does not override it.
    fn default_sampling(&self) -> Sampling {
        Sampling::Average
    }

    /// Measure (length, area or volume; 1 per point for a bare point cloud)
    /// of each entity: the sum of its sample weights.
    fn measures(&self) -> Vec<f64> {
        let mut m = vec![0.0; self.len()];
        let mut pts = Vec::new();
        self.sample_points(&mut pts);
        for s in pts {
            m[s.dof] += s.weight;
        }
        m
    }
}
