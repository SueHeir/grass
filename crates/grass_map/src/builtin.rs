//! Built-in discretizations: point clouds (including particles), uniform
//! cell-centered grids, and triangle surface meshes.
//!
//! These are general geometric containers, not physics: a solver library
//! either uses them directly or implements [`Discretization`] for its own
//! types (FIELD meshes, SOIL particle stores).

use crate::discretization::{Discretization, SamplePoint, Sampling, TopoDim};
use crate::geom::{add, closest_on_triangle, dist2, scale, triangle_area, Aabb, Point};
use crate::index::SpatialIndex;

// ---------------------------------------------------------------------------
// Point cloud
// ---------------------------------------------------------------------------

/// How a point cloud reports its value at an arbitrary point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointBasis {
    /// The nearest point carries the whole value.
    Nearest,
    /// Normalized (Shepard) Wendland C2 kernel weights over all points
    /// within `radius`, the usual SPH-style particle interpolation.
    Kernel {
        /// Kernel support radius.
        radius: f64,
    },
}

/// A set of points: sensors, particles, mesh vertices, a black-box code's
/// output locations.
#[derive(Debug, Clone)]
pub struct PointCloud {
    points: Vec<Point>,
    measures: Option<Vec<f64>>,
    basis: PointBasis,
    max_distance: Option<f64>,
    index: SpatialIndex,
}

impl PointCloud {
    /// A point cloud using nearest-point evaluation and unit weights.
    pub fn new(points: Vec<Point>) -> Self {
        let boxes: Vec<Aabb> = points.iter().map(|p| Aabb::point(*p)).collect();
        Self {
            index: SpatialIndex::build(&boxes),
            points,
            measures: None,
            basis: PointBasis::Nearest,
            max_distance: None,
        }
    }

    /// Give each point a measure (a particle's volume, a vertex's area).
    /// This becomes its integration weight.
    pub fn with_measures(mut self, measures: Vec<f64>) -> Self {
        assert_eq!(measures.len(), self.points.len(), "one measure per point");
        self.measures = Some(measures);
        self
    }

    /// Choose how values are evaluated between points.
    pub fn with_basis(mut self, basis: PointBasis) -> Self {
        self.basis = basis;
        self
    }

    /// For [`PointBasis::Nearest`]: treat points farther than `d` from every
    /// point as outside the support (unmapped) instead of extrapolating.
    pub fn with_max_distance(mut self, d: f64) -> Self {
        self.max_distance = Some(d);
        self
    }

    /// The points.
    pub fn points(&self) -> &[Point] {
        &self.points
    }
}

/// Wendland C2 kernel shape on `q = r / h ∈ [0, 1]` (unnormalized; Shepard
/// normalization makes the constant irrelevant).
fn wendland_c2(q: f64) -> f64 {
    if q >= 1.0 {
        0.0
    } else {
        let t = 1.0 - q;
        t * t * t * t * (4.0 * q + 1.0)
    }
}

impl Discretization for PointCloud {
    fn len(&self) -> usize {
        self.points.len()
    }

    fn topo_dim(&self) -> TopoDim {
        TopoDim::Point
    }

    fn centroid(&self, i: usize) -> Point {
        self.points[i]
    }

    fn bounds(&self) -> Aabb {
        Aabb::of_points(&self.points)
    }

    fn sample_points(&self, out: &mut Vec<SamplePoint>) {
        for (i, p) in self.points.iter().enumerate() {
            let weight = self.measures.as_ref().map_or(1.0, |m| m[i]);
            out.push(SamplePoint {
                dof: i,
                point: *p,
                weight,
            });
        }
    }

    fn evaluate(&self, p: Point, out: &mut Vec<(usize, f64)>) -> bool {
        match self.basis {
            PointBasis::Nearest => {
                let Some(i) = self.index.nearest(p, |i| self.points[i]) else {
                    return false;
                };
                if let Some(d) = self.max_distance {
                    if dist2(p, self.points[i]) > d * d {
                        return false;
                    }
                }
                out.push((i, 1.0));
                true
            }
            PointBasis::Kernel { radius } => {
                let mut cand = Vec::new();
                self.index
                    .candidates(&Aabb::point(p).inflate(radius), &mut cand);
                let start = out.len();
                let mut sum = 0.0;
                for i in cand {
                    let w = wendland_c2(dist2(p, self.points[i]).sqrt() / radius);
                    if w > 0.0 {
                        out.push((i, w));
                        sum += w;
                    }
                }
                if sum <= 0.0 {
                    out.truncate(start);
                    return false;
                }
                for e in &mut out[start..] {
                    e.1 /= sum;
                }
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Uniform grid
// ---------------------------------------------------------------------------

/// How a uniform grid reports its value at an arbitrary point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellBasis {
    /// The enclosing cell's value (finite-volume style).
    Constant,
    /// Trilinear between cell centers (cloud-in-cell), clamped at the walls.
    Trilinear,
}

/// A box split into `n[0] × n[1] × n[2]` equal cells, data at cell centers.
/// Cell `(ix, iy, iz)` has index `ix + n[0] * (iy + n[1] * iz)`.
#[derive(Debug, Clone)]
pub struct UniformGrid {
    lo: Point,
    hi: Point,
    n: [usize; 3],
    h: [f64; 3],
    basis: CellBasis,
    quad_order: usize,
    tolerance: f64,
}

impl UniformGrid {
    /// A grid over `[lo, hi]` with `n` cells per axis, constant basis and
    /// 2-point Gauss quadrature per axis.
    ///
    /// # Panics
    /// Panics if any `n` is zero or the box is empty.
    pub fn new(lo: Point, hi: Point, n: [usize; 3]) -> Self {
        assert!(n.iter().all(|&k| k > 0), "need at least one cell per axis");
        let h = [
            (hi[0] - lo[0]) / n[0] as f64,
            (hi[1] - lo[1]) / n[1] as f64,
            (hi[2] - lo[2]) / n[2] as f64,
        ];
        assert!(
            h.iter().all(|&x| x > 0.0),
            "grid box must have positive extent"
        );
        Self {
            lo,
            hi,
            n,
            h,
            basis: CellBasis::Constant,
            quad_order: 2,
            tolerance: 1e-9,
        }
    }

    /// Choose how values are evaluated between cell centers.
    pub fn with_basis(mut self, basis: CellBasis) -> Self {
        self.basis = basis;
        self
    }

    /// Gauss–Legendre points per axis per cell (1 to 3).
    pub fn with_quadrature_order(mut self, order: usize) -> Self {
        assert!(
            (1..=3).contains(&order),
            "quadrature order must be 1, 2 or 3"
        );
        self.quad_order = order;
        self
    }

    /// Points within `tol × cell size` outside the box still count as inside
    /// (so boundary faces of a matching mesh are covered).
    pub fn with_tolerance(mut self, tol: f64) -> Self {
        self.tolerance = tol;
        self
    }

    /// Cells per axis.
    pub fn cells(&self) -> [usize; 3] {
        self.n
    }

    /// Flat index of cell `(ix, iy, iz)`.
    pub fn index(&self, ix: usize, iy: usize, iz: usize) -> usize {
        ix + self.n[0] * (iy + self.n[1] * iz)
    }

    /// Center of cell `i`.
    pub fn center(&self, i: usize) -> Point {
        let ix = i % self.n[0];
        let iy = (i / self.n[0]) % self.n[1];
        let iz = i / (self.n[0] * self.n[1]);
        [
            self.lo[0] + (ix as f64 + 0.5) * self.h[0],
            self.lo[1] + (iy as f64 + 0.5) * self.h[1],
            self.lo[2] + (iz as f64 + 0.5) * self.h[2],
        ]
    }

    fn inside(&self, p: Point) -> bool {
        (0..3).all(|d| {
            let t = self.tolerance * self.h[d];
            p[d] >= self.lo[d] - t && p[d] <= self.hi[d] + t
        })
    }
}

fn gauss_legendre(order: usize) -> &'static [(f64, f64)] {
    // Points on [0, 1] with weights summing to 1.
    const G1: [(f64, f64); 1] = [(0.5, 1.0)];
    const G2: [(f64, f64); 2] = [
        (0.211_324_865_405_187_1, 0.5),
        (0.788_675_134_594_812_9, 0.5),
    ];
    const G3: [(f64, f64); 3] = [
        (0.112_701_665_379_258_3, 5.0 / 18.0),
        (0.5, 8.0 / 18.0),
        (0.887_298_334_620_741_7, 5.0 / 18.0),
    ];
    match order {
        1 => &G1,
        2 => &G2,
        _ => &G3,
    }
}

impl Discretization for UniformGrid {
    fn len(&self) -> usize {
        self.n[0] * self.n[1] * self.n[2]
    }

    fn topo_dim(&self) -> TopoDim {
        TopoDim::Volume
    }

    fn centroid(&self, i: usize) -> Point {
        self.center(i)
    }

    fn bounds(&self) -> Aabb {
        Aabb {
            lo: self.lo,
            hi: self.hi,
        }
    }

    fn sample_points(&self, out: &mut Vec<SamplePoint>) {
        let g = gauss_legendre(self.quad_order);
        let vol = self.h[0] * self.h[1] * self.h[2];
        for iz in 0..self.n[2] {
            for iy in 0..self.n[1] {
                for ix in 0..self.n[0] {
                    let dof = self.index(ix, iy, iz);
                    for &(sz, wz) in g {
                        for &(sy, wy) in g {
                            for &(sx, wx) in g {
                                out.push(SamplePoint {
                                    dof,
                                    point: [
                                        self.lo[0] + (ix as f64 + sx) * self.h[0],
                                        self.lo[1] + (iy as f64 + sy) * self.h[1],
                                        self.lo[2] + (iz as f64 + sz) * self.h[2],
                                    ],
                                    weight: vol * wx * wy * wz,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    fn evaluate(&self, p: Point, out: &mut Vec<(usize, f64)>) -> bool {
        if !self.inside(p) {
            return false;
        }
        match self.basis {
            CellBasis::Constant => {
                let c: Vec<usize> = (0..3)
                    .map(|d| {
                        let k = ((p[d] - self.lo[d]) / self.h[d]).floor();
                        (k.max(0.0) as usize).min(self.n[d] - 1)
                    })
                    .collect();
                out.push((self.index(c[0], c[1], c[2]), 1.0));
            }
            CellBasis::Trilinear => {
                // Per axis: lower neighbor index and the weight of the upper one.
                let mut lo_idx = [0usize; 3];
                let mut frac = [0.0f64; 3];
                for d in 0..3 {
                    if self.n[d] == 1 {
                        continue;
                    }
                    let s = (p[d] - self.lo[d]) / self.h[d] - 0.5;
                    let i0 = (s.floor().max(0.0) as usize).min(self.n[d] - 2);
                    lo_idx[d] = i0;
                    frac[d] = (s - i0 as f64).clamp(0.0, 1.0);
                }
                for dz in 0..2 {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let off = [dx, dy, dz];
                            let mut w = 1.0;
                            let mut c = [0usize; 3];
                            let mut skip = false;
                            for d in 0..3 {
                                if self.n[d] == 1 {
                                    if off[d] == 1 {
                                        skip = true;
                                    }
                                    continue;
                                }
                                c[d] = lo_idx[d] + off[d];
                                w *= if off[d] == 1 { frac[d] } else { 1.0 - frac[d] };
                            }
                            if !skip && w > 0.0 {
                                out.push((self.index(c[0], c[1], c[2]), w));
                            }
                        }
                    }
                }
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Triangle surface
// ---------------------------------------------------------------------------

/// Where data lives on a triangle surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceDofs {
    /// One value per vertex, linear across each triangle (finite-element style).
    Vertex,
    /// One value per triangle (finite-volume boundary-face style).
    Face,
}

/// A triangle mesh in 3-D: a wall, a heat-shield surface, a fluid boundary.
/// Points are evaluated by projecting onto the nearest triangle, within a
/// distance tolerance, so non-matching surfaces map onto each other.
#[derive(Debug, Clone)]
pub struct TriSurface {
    vertices: Vec<Point>,
    triangles: Vec<[usize; 3]>,
    dofs: SurfaceDofs,
    tolerance: f64,
    index: SpatialIndex,
}

/// Interior 3-point rule (exact for quadratics): barycentric points, each
/// weighted one third of the triangle area.
const TRI_RULE: [[f64; 3]; 3] = [
    [2.0 / 3.0, 1.0 / 6.0, 1.0 / 6.0],
    [1.0 / 6.0, 2.0 / 3.0, 1.0 / 6.0],
    [1.0 / 6.0, 1.0 / 6.0, 2.0 / 3.0],
];

impl TriSurface {
    /// A surface with the given projection tolerance (the largest normal
    /// distance at which a point still counts as on the surface).
    ///
    /// # Panics
    /// Panics if a triangle references a missing vertex.
    pub fn new(
        vertices: Vec<Point>,
        triangles: Vec<[usize; 3]>,
        dofs: SurfaceDofs,
        tolerance: f64,
    ) -> Self {
        assert!(
            triangles.iter().flatten().all(|&v| v < vertices.len()),
            "triangle references a missing vertex"
        );
        let boxes: Vec<Aabb> = triangles
            .iter()
            .map(|t| {
                Aabb::of_points(&[vertices[t[0]], vertices[t[1]], vertices[t[2]]])
                    .inflate(tolerance)
            })
            .collect();
        Self {
            index: SpatialIndex::build(&boxes),
            vertices,
            triangles,
            dofs,
            tolerance,
        }
    }

    /// The vertices.
    pub fn vertices(&self) -> &[Point] {
        &self.vertices
    }

    /// The triangles.
    pub fn triangles(&self) -> &[[usize; 3]] {
        &self.triangles
    }

    fn corners(&self, t: usize) -> [Point; 3] {
        let [a, b, c] = self.triangles[t];
        [self.vertices[a], self.vertices[b], self.vertices[c]]
    }
}

impl Discretization for TriSurface {
    fn len(&self) -> usize {
        match self.dofs {
            SurfaceDofs::Vertex => self.vertices.len(),
            SurfaceDofs::Face => self.triangles.len(),
        }
    }

    fn topo_dim(&self) -> TopoDim {
        TopoDim::Surface
    }

    /// Vertex data are nodal values, so they interpolate; face data average.
    fn default_sampling(&self) -> Sampling {
        match self.dofs {
            SurfaceDofs::Vertex => Sampling::Centroid,
            SurfaceDofs::Face => Sampling::Average,
        }
    }

    fn centroid(&self, i: usize) -> Point {
        match self.dofs {
            SurfaceDofs::Vertex => self.vertices[i],
            SurfaceDofs::Face => {
                let [a, b, c] = self.corners(i);
                scale(add(add(a, b), c), 1.0 / 3.0)
            }
        }
    }

    fn bounds(&self) -> Aabb {
        Aabb::of_points(&self.vertices)
    }

    fn sample_points(&self, out: &mut Vec<SamplePoint>) {
        for (t, tri) in self.triangles.iter().enumerate() {
            let [a, b, c] = self.corners(t);
            let w = triangle_area(a, b, c) / 3.0;
            for bary in TRI_RULE {
                let point = add(add(scale(a, bary[0]), scale(b, bary[1])), scale(c, bary[2]));
                match self.dofs {
                    SurfaceDofs::Face => out.push(SamplePoint {
                        dof: t,
                        point,
                        weight: w,
                    }),
                    SurfaceDofs::Vertex => {
                        for k in 0..3 {
                            out.push(SamplePoint {
                                dof: tri[k],
                                point,
                                weight: w * bary[k],
                            });
                        }
                    }
                }
            }
        }
    }

    fn evaluate(&self, p: Point, out: &mut Vec<(usize, f64)>) -> bool {
        let mut cand = Vec::new();
        self.index.candidates(&Aabb::point(p), &mut cand);
        let mut best: Option<(f64, usize, [f64; 3])> = None;
        for t in cand {
            let [a, b, c] = self.corners(t);
            let (bary, d2) = closest_on_triangle(p, a, b, c);
            if best.is_none_or(|(bd, _, _)| d2 < bd) {
                best = Some((d2, t, bary));
            }
        }
        match best {
            Some((d2, t, bary)) if d2 <= self.tolerance * self.tolerance => {
                match self.dofs {
                    SurfaceDofs::Face => out.push((t, 1.0)),
                    SurfaceDofs::Vertex => {
                        for (&v, &w) in self.triangles[t].iter().zip(&bary) {
                            if w > 0.0 {
                                out.push((v, w));
                            }
                        }
                    }
                }
                true
            }
            _ => false,
        }
    }
}
