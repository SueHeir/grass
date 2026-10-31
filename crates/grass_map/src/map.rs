//! Building and applying mapping operators.
//!
//! Everything is assembled by one routine: take the weighted sample points
//! of one discretization (the *sampler*) and evaluate the basis of the other
//! (the *basis side*) at each of them. Row `j` of the result, normalized by
//! the weight of entity `j`'s supported samples, is entity `j`'s view of the
//! basis side:
//!
//! ```text
//! W[j, i] = Σ_{q ∈ j} w_q φ_i(x_q) / Σ_{q ∈ j, supported} w_q
//! ```
//!
//! * A **consistent** map `src → dst` samples at the target's points:
//!   `W = assemble(dst, src)`, applied forwards. If the source basis is a
//!   partition of unity, rows sum to 1 and constants are preserved.
//! * A **conservative** map `src → dst` samples at the source's points and
//!   spreads each source amount over the target basis:
//!   `C = assemble(src, dst)ᵀ`. Columns sum to 1, so totals are preserved.
//!
//! The two directions between one pair share a single matrix:
//! the conservative map `a → b` is exactly the transpose of the consistent map
//! `b → a`. [`AdjointPair`] exposes both from one assembly, which is what keeps
//! two-way exchanges (drag and its reaction, wall temperature and wall heat)
//! energy-consistent.

use crate::discretization::{Discretization, SamplePoint, Sampling};
use crate::field::{Extensive, Field, Sampled};
use crate::geom::Aabb;
use crate::index::SpatialIndex;
use crate::sparse::CsrMatrix;

/// How the basis side is evaluated at sample points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    /// The discretization's own basis ([`Discretization::evaluate`]):
    /// constant or trilinear in grid cells, projection onto the nearest
    /// triangle for surfaces, nearest point or kernel for point clouds.
    #[default]
    Native,
    /// Ignore the basis; give each sample point to the entity with the
    /// nearest centroid. Always supported, never extrapolation-free.
    NearestNeighbor,
}

/// What happened while building a map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapReport {
    /// Entities (rows of the sampler) none of whose sample points were
    /// supported. A consistent map leaves those target values untouched; a
    /// conservative map loses those source amounts.
    pub unmapped: Vec<usize>,
    /// Number of entities only some of whose sample points were supported
    /// (for example a cell straddling the edge of the other mesh).
    pub partial: usize,
    /// Largest deviation of a mapped row's basis-value sum from 1. Nonzero
    /// means the basis side is not a partition of unity there.
    pub max_unity_error: f64,
}

impl MapReport {
    /// Whether every entity was fully covered and the basis summed to one.
    pub fn is_complete(&self, tol: f64) -> bool {
        self.unmapped.is_empty() && self.partial == 0 && self.max_unity_error <= tol
    }
}

/// Assemble the normalized sampling matrix (rows: `sampler` entities,
/// columns: `basis` entities).
fn assemble(
    sampler: &dyn Discretization,
    basis: &dyn Discretization,
    method: Method,
    sampling: Option<Sampling>,
) -> (CsrMatrix, MapReport) {
    let mut samples: Vec<SamplePoint> = Vec::new();
    match sampling.unwrap_or_else(|| sampler.default_sampling()) {
        Sampling::Average => sampler.sample_points(&mut samples),
        Sampling::Centroid => samples.extend((0..sampler.len()).map(|j| SamplePoint {
            dof: j,
            point: sampler.centroid(j),
            weight: 1.0,
        })),
    }

    let nn_index = match method {
        Method::NearestNeighbor if !basis.is_empty() => {
            let boxes: Vec<Aabb> = (0..basis.len())
                .map(|i| Aabb::point(basis.centroid(i)))
                .collect();
            Some(SpatialIndex::build(&boxes))
        }
        _ => None,
    };

    let n = sampler.len();
    let mut total = vec![0.0; n];
    let mut supported = vec![0.0; n];
    let mut triplets = Vec::with_capacity(samples.len() * 4);
    let mut phi = Vec::new();
    let mut max_unity_error: f64 = 0.0;
    for s in &samples {
        total[s.dof] += s.weight;
        phi.clear();
        let ok = match method {
            Method::Native => basis.evaluate(s.point, &mut phi),
            Method::NearestNeighbor => match nn_index
                .as_ref()
                .and_then(|ix| ix.nearest(s.point, |i| basis.centroid(i)))
            {
                Some(i) => {
                    phi.push((i, 1.0));
                    true
                }
                None => false,
            },
        };
        if !ok {
            continue;
        }
        let sum: f64 = phi.iter().map(|(_, v)| v).sum();
        max_unity_error = max_unity_error.max((sum - 1.0).abs());
        supported[s.dof] += s.weight;
        for &(i, v) in &phi {
            triplets.push((s.dof, i, s.weight * v));
        }
    }
    for t in &mut triplets {
        t.2 /= supported[t.0];
    }
    let mut unmapped = Vec::new();
    let mut partial = 0;
    for j in 0..n {
        if supported[j] <= 0.0 {
            unmapped.push(j);
        } else if supported[j] < total[j] * (1.0 - 1e-12) {
            partial += 1;
        }
    }
    (
        CsrMatrix::from_triplets(n, basis.len(), triplets),
        MapReport {
            unmapped,
            partial,
            max_unity_error,
        },
    )
}

/// Configures a map from `src` to `dst`.
pub struct MapBuilder<'a> {
    src: &'a dyn Discretization,
    dst: &'a dyn Discretization,
    method: Method,
    sampling: Option<Sampling>,
}

impl<'a> MapBuilder<'a> {
    /// Start a map from `src` to `dst` with [`Method::Native`].
    pub fn new(src: &'a dyn Discretization, dst: &'a dyn Discretization) -> Self {
        Self {
            src,
            dst,
            method: Method::Native,
            sampling: None,
        }
    }

    /// Choose the evaluation method.
    pub fn method(mut self, method: Method) -> Self {
        self.method = method;
        self
    }

    /// Override the sampler's [`default_sampling`](Discretization::default_sampling).
    /// The sampler is the target for a consistent map and the source for a
    /// conservative one.
    pub fn sampling(mut self, sampling: Sampling) -> Self {
        self.sampling = Some(sampling);
        self
    }

    /// A sampling map for intensive and density fields: target entities
    /// average the source over their own sample points.
    pub fn consistent(self) -> ConsistentMap {
        let (w, report) = assemble(self.dst, self.src, self.method, self.sampling);
        ConsistentMap { w, report }
    }

    /// A redistribution map for extensive fields: each source amount is
    /// spread over the target basis at the source's own sample points.
    pub fn conservative(self) -> ConservativeMap {
        let (w, report) = assemble(self.src, self.dst, self.method, self.sampling);
        ConservativeMap {
            c: w.transpose(),
            report,
        }
    }
}

/// Maps intensive and density fields by sampling. Rows: target entities.
#[derive(Debug, Clone)]
pub struct ConsistentMap {
    w: CsrMatrix,
    report: MapReport,
}

impl ConsistentMap {
    /// `dst = W src`. Unmapped target entities keep their previous values.
    pub fn apply<K: Sampled>(&self, src: &Field<K>, dst: &mut Field<K>) {
        assert_eq!(src.ncomp, dst.ncomp, "component count mismatch");
        self.w.apply(&src.values, &mut dst.values, src.ncomp);
    }

    /// The operator (rows: target, columns: source).
    pub fn matrix(&self) -> &CsrMatrix {
        &self.w
    }

    /// Coverage diagnostics; [`MapReport::unmapped`] lists target entities.
    pub fn report(&self) -> &MapReport {
        &self.report
    }
}

/// Maps extensive fields by redistribution. Rows: target entities.
#[derive(Debug, Clone)]
pub struct ConservativeMap {
    c: CsrMatrix,
    report: MapReport,
}

impl ConservativeMap {
    /// `dst = C src`, overwriting `dst`. Amounts on unmapped source entities
    /// are dropped (see [`report`](Self::report)).
    pub fn apply(&self, src: &Field<Extensive>, dst: &mut Field<Extensive>) {
        assert_eq!(src.ncomp, dst.ncomp, "component count mismatch");
        dst.values.iter_mut().for_each(|v| *v = 0.0);
        self.c.apply(&src.values, &mut dst.values, src.ncomp);
    }

    /// The operator (rows: target, columns: source).
    pub fn matrix(&self) -> &CsrMatrix {
        &self.c
    }

    /// Coverage diagnostics; [`MapReport::unmapped`] lists *source*
    /// entities whose amounts could not be placed.
    pub fn report(&self) -> &MapReport {
        &self.report
    }
}

/// Both directions of a two-way exchange between `a` and `b`, from one
/// assembly: sample intensive data from `b` onto `a`, and deposit `a`'s
/// extensive data onto `b` with exactly the transpose.
///
/// This is the particle–mesh pattern (gather fluid velocity at particles,
/// scatter drag reaction back), generalized to any pair. Because the two
/// operators are adjoint, the power one side hands over equals the power the
/// other receives: `Σ_a f·(W u) = Σ_b (Wᵀ f)·u`.
#[derive(Debug, Clone)]
pub struct AdjointPair {
    w: CsrMatrix,
    report: MapReport,
}

impl AdjointPair {
    /// Assemble with `a` as the sampler (using its default sampling) and `b`
    /// as the basis side.
    pub fn new(a: &dyn Discretization, b: &dyn Discretization, method: Method) -> Self {
        Self::with_sampling(a, b, method, a.default_sampling())
    }

    /// Assemble with an explicit sampling for `a`.
    pub fn with_sampling(
        a: &dyn Discretization,
        b: &dyn Discretization,
        method: Method,
        sampling: Sampling,
    ) -> Self {
        let (w, report) = assemble(a, b, method, Some(sampling));
        Self { w, report }
    }

    /// Intensive or density data from `b` onto `a` (consistent).
    pub fn sample<K: Sampled>(&self, on_b: &Field<K>, on_a: &mut Field<K>) {
        assert_eq!(on_b.ncomp, on_a.ncomp, "component count mismatch");
        self.w.apply(&on_b.values, &mut on_a.values, on_b.ncomp);
    }

    /// Extensive data from `a` onto `b` (conservative), overwriting `on_b`.
    pub fn deposit(&self, on_a: &Field<Extensive>, on_b: &mut Field<Extensive>) {
        assert_eq!(on_a.ncomp, on_b.ncomp, "component count mismatch");
        self.w
            .apply_transpose(&on_a.values, &mut on_b.values, on_a.ncomp);
    }

    /// The sampling operator (rows: `a`, columns: `b`).
    pub fn matrix(&self) -> &CsrMatrix {
        &self.w
    }

    /// Coverage diagnostics; [`MapReport::unmapped`] lists `a` entities.
    pub fn report(&self) -> &MapReport {
        &self.report
    }
}
