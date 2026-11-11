//! A tour of `grass_map` on a heat-shield-shaped problem: a fluid grid above
//! a non-matching triangulated shield surface, plus debris particles in the
//! flow. Every check is asserted; the run prints a PASS table.

use grass_map::*;

/// Triangulated rectangle `[0,1] × [0,1]` at height `z`.
fn plate(nx: usize, ny: usize, z: f64, dofs: SurfaceDofs) -> TriSurface {
    let mut v = Vec::new();
    for j in 0..=ny {
        for i in 0..=nx {
            v.push([i as f64 / nx as f64, j as f64 / ny as f64, z]);
        }
    }
    let id = |i: usize, j: usize| i + (nx + 1) * j;
    let mut t = Vec::new();
    for j in 0..ny {
        for i in 0..nx {
            t.push([id(i, j), id(i + 1, j), id(i + 1, j + 1)]);
            t.push([id(i, j), id(i + 1, j + 1), id(i, j + 1)]);
        }
    }
    TriSurface::new(v, t, dofs, 1e-3)
}

fn check(name: &str, value: f64, limit: f64) {
    let ok = value <= limit;
    println!(
        "{:<52} {:>11.3e}  (limit {:.0e})  {}",
        name,
        value,
        limit,
        if ok { "PASS" } else { "FAIL" }
    );
    assert!(ok, "{name} failed: {value} > {limit}");
}

fn main() {
    // Fluid: a 24 × 24 × 12 grid over the unit box; the shield wall is z = 0.
    let fluid = UniformGrid::new([0.0; 3], [1.0; 3], [24, 24, 12]).with_basis(CellBasis::Trilinear);
    // Fluid-side wall faces (as a CFD code would expose them) and a coarser,
    // differently triangulated shield surface lifted 1e-4 off the wall.
    let fluid_wall = plate(24, 24, 0.0, SurfaceDofs::Face);
    let shield = plate(7, 9, 1e-4, SurfaceDofs::Vertex);

    // 1. Temperature: fluid cells -> shield vertices (consistent trace).
    let temp = |p: Point| 300.0 + 1500.0 * p[0] + 400.0 * p[1] - 200.0 * p[2];
    let t_fluid =
        Field::<Intensive>::scalar((0..fluid.len()).map(|i| temp(fluid.centroid(i))).collect());
    let trace = MapBuilder::new(&fluid, &shield).consistent();
    let mut t_shield = Field::<Intensive>::zeros(shield.len(), 1);
    trace.apply(&t_fluid, &mut t_shield);
    // Trilinear reproduces linear fields between cell centers; within half a
    // cell of the side walls it is clamped, so compare interior vertices.
    let h = 1.0 / 24.0;
    let interior_err = (0..shield.len())
        .filter(|&i| {
            let c = shield.centroid(i);
            c[0] > h / 2.0 && c[0] < 1.0 - h / 2.0 && c[1] > h / 2.0 && c[1] < 1.0 - h / 2.0
        })
        .map(|i| {
            // The trace reads the first cell layer: the trilinear basis is
            // clamped below the first cell center, z = 1/24.
            let c = shield.centroid(i);
            (t_shield.values[i] - temp([c[0], c[1], 1.0 / 24.0])).abs()
        })
        .fold(0.0, f64::max);
    check(
        "wall temperature trace, interior max error [K]",
        interior_err,
        1e-9,
    );
    check(
        "wall temperature trace, unmapped vertices",
        trace.report().unmapped.len() as f64,
        0.0,
    );

    // 2. Heat: fluid wall faces -> shield vertices (conservative).
    let q_density = Field::<Density>::scalar(
        (0..fluid_wall.len())
            .map(|i| 1.0e6 * (1.0 + fluid_wall.centroid(i)[0]))
            .collect(),
    );
    let q_faces = q_density.to_extensive(&fluid_wall.measures()); // watts per face
    let heat = MapBuilder::new(&fluid_wall, &shield).conservative();
    let mut q_shield = Field::<Extensive>::zeros(shield.len(), 1);
    heat.apply(&q_faces, &mut q_shield);
    let rel = (q_shield.totals()[0] - q_faces.totals()[0]).abs() / q_faces.totals()[0];
    check("wall heat into shield, relative total error", rel, 1e-12);

    // 3. Debris particles in the flow: sample velocity, deposit drag reaction
    //    with exactly the transpose, so the exchanged power balances.
    let debris = PointCloud::new(
        (0..200)
            .map(|k| {
                let f = k as f64;
                [
                    (f * 0.618_034).fract(),
                    (f * 0.414_214).fract(),
                    0.05 + 0.3 * (f * 0.732_051).fract(),
                ]
            })
            .collect(),
    );
    let pair = AdjointPair::new(&debris, &fluid, Method::Native);
    let ux = Field::<Intensive>::scalar(
        (0..fluid.len())
            .map(|i| {
                let c = fluid.centroid(i);
                2000.0 * c[2] * (1.0 - c[2]) * (1.0 + 0.2 * (6.0 * c[0]).sin())
            })
            .collect(),
    );
    let mut ux_at_debris = Field::<Intensive>::zeros(debris.len(), 1);
    pair.sample(&ux, &mut ux_at_debris);
    let drag = Field::<Extensive>::scalar(ux_at_debris.values.iter().map(|u| 1e-6 * u).collect());
    let mut reaction = Field::<Extensive>::zeros(fluid.len(), 1);
    pair.deposit(&drag, &mut reaction);
    let p_debris: f64 = drag
        .values
        .iter()
        .zip(&ux_at_debris.values)
        .map(|(f, u)| f * u)
        .sum();
    let p_fluid: f64 = reaction
        .values
        .iter()
        .zip(&ux.values)
        .map(|(f, u)| f * u)
        .sum();
    check(
        "drag power, debris vs fluid, relative difference",
        (p_debris - p_fluid).abs() / p_debris,
        1e-12,
    );
    check(
        "drag force total, relative difference",
        (drag.totals()[0] - reaction.totals()[0]).abs() / drag.totals()[0],
        1e-12,
    );

    println!(
        "operators: trace {} nnz, heat {} nnz, debris {} nnz",
        trace.matrix().nnz(),
        heat.matrix().nnz(),
        pair.matrix().nnz()
    );
}
