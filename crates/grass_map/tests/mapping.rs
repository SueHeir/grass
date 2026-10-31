//! Property tests for grass_map: constants preserved, totals conserved,
//! adjointness, polynomial reproduction, convergence, and coverage reports.

use grass_map::*;

const TOL: f64 = 1e-12;

/// Triangulated rectangle `[0,lx] × [0,ly]` at height `z`, `nx × ny` quads
/// each split into two triangles.
fn plane(
    nx: usize,
    ny: usize,
    lx: f64,
    ly: f64,
    z: f64,
    dofs: SurfaceDofs,
    tol: f64,
) -> TriSurface {
    let mut v = Vec::new();
    for j in 0..=ny {
        for i in 0..=nx {
            v.push([lx * i as f64 / nx as f64, ly * j as f64 / ny as f64, z]);
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
    TriSurface::new(v, t, dofs, tol)
}

fn field_from<K: FieldKind>(d: &dyn Discretization, f: impl Fn(Point) -> f64) -> Field<K> {
    Field::scalar((0..d.len()).map(|i| f(d.centroid(i))).collect())
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn max_abs_diff(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f64::max)
}

#[test]
fn consistent_maps_preserve_constants_for_every_pair() {
    let coarse = UniformGrid::new([0.0; 3], [1.0; 3], [3, 4, 2]);
    let fine_tri = UniformGrid::new([0.0; 3], [1.0; 3], [7, 5, 6]).with_basis(CellBasis::Trilinear);
    let particles = PointCloud::new(
        (0..60)
            .map(|k| {
                let f = k as f64;
                [
                    (f * 0.618).fract(),
                    (f * 0.414).fract(),
                    (f * 0.732).fract(),
                ]
            })
            .collect(),
    )
    .with_basis(PointBasis::Kernel { radius: 0.5 });
    let top = plane(5, 3, 1.0, 1.0, 1.0, SurfaceDofs::Vertex, 1e-9);

    let pairs: Vec<(&dyn Discretization, &dyn Discretization, &str)> = vec![
        (&coarse, &fine_tri, "grid -> grid"),
        (&fine_tri, &coarse, "trilinear grid -> grid"),
        (&coarse, &particles, "grid -> particles"),
        (&particles, &coarse, "particles (kernel) -> grid"),
        (&coarse, &top, "volume -> boundary surface (trace)"),
        (&top, &particles, "surface -> points (beyond tolerance)"),
    ];
    for (src, dst, name) in pairs {
        let map = MapBuilder::new(src, dst).consistent();
        let s = Field::<Intensive>::scalar(vec![42.0; src.len()]);
        let mut d = Field::<Intensive>::zeros(dst.len(), 1);
        map.apply(&s, &mut d);
        let r = map.report();
        for j in 0..dst.len() {
            if r.unmapped.contains(&j) {
                assert_eq!(d.values[j], 0.0, "{name}: unmapped entity {j} was written");
            } else {
                assert!(
                    (d.values[j] - 42.0).abs() < 1e-9,
                    "{name}: entity {j} = {}",
                    d.values[j]
                );
            }
        }
        assert!(
            r.max_unity_error < 1e-12,
            "{name}: basis not a partition of unity"
        );
    }
}

#[test]
fn surface_to_off_surface_points_is_reported_unmapped() {
    let top = plane(5, 3, 1.0, 1.0, 1.0, SurfaceDofs::Vertex, 1e-6);
    let pts = PointCloud::new(vec![
        [0.5, 0.5, 1.0 + 1e-7],
        [0.5, 0.5, 0.5],
        [2.0, 0.5, 1.0],
    ]);
    let map = MapBuilder::new(&top, &pts).consistent();
    assert_eq!(map.report().unmapped, vec![1, 2]);
}

#[test]
fn conservative_maps_preserve_totals() {
    let coarse = UniformGrid::new([0.0; 3], [1.0; 3], [3, 4, 2]);
    let fine = UniformGrid::new([0.0; 3], [1.0; 3], [7, 5, 6]).with_basis(CellBasis::Trilinear);
    let particles = PointCloud::new(
        (0..50)
            .map(|k| {
                let f = k as f64;
                [
                    (f * 0.618).fract(),
                    (f * 0.414).fract(),
                    (f * 0.732).fract(),
                ]
            })
            .collect(),
    )
    .with_measures(vec![1e-3; 50]);
    let faces_fine = plane(8, 6, 1.0, 1.0, 0.0, SurfaceDofs::Face, 1e-9);
    let verts_coarse = plane(3, 2, 1.0, 1.0, 0.0, SurfaceDofs::Vertex, 1e-9);

    let pairs: Vec<(&dyn Discretization, &dyn Discretization, &str)> = vec![
        (&coarse, &fine, "grid -> trilinear grid"),
        (&fine, &coarse, "grid -> grid"),
        (
            &particles,
            &fine,
            "particles -> trilinear grid (cloud-in-cell deposit)",
        ),
        (&faces_fine, &verts_coarse, "fine faces -> coarse vertices"),
        (&verts_coarse, &faces_fine, "coarse vertices -> fine faces"),
    ];
    for (src, dst, name) in pairs {
        let map = MapBuilder::new(src, dst).conservative();
        assert!(
            map.report().is_complete(1e-12),
            "{name}: {:?}",
            map.report()
        );
        let s: Field<Extensive> = Field::new(
            (0..src.len() * 3)
                .map(|i| (i as f64 * 0.37).sin())
                .collect(),
            3,
        );
        let mut d = Field::<Extensive>::zeros(dst.len(), 3);
        map.apply(&s, &mut d);
        for (a, b) in s.totals().iter().zip(d.totals()) {
            assert!((a - b).abs() < 1e-10, "{name}: total {a} became {b}");
        }
    }
}

#[test]
fn adjoint_pair_exchanges_equal_power() {
    let grid =
        UniformGrid::new([0.0; 3], [2.0, 1.0, 1.0], [6, 3, 3]).with_basis(CellBasis::Trilinear);
    let particles = PointCloud::new(
        (0..40)
            .map(|k| {
                let f = k as f64;
                [
                    2.0 * (f * 0.618).fract(),
                    (f * 0.414).fract(),
                    (f * 0.732).fract(),
                ]
            })
            .collect(),
    );
    let pair = AdjointPair::new(&particles, &grid, Method::Native);
    assert!(pair.report().is_complete(1e-12));

    // Fluid velocity on the grid, drag force on the particles.
    let u: Field<Intensive> = field_from(&grid, |p| (p[0] * 1.3).sin() + p[1] * p[2]);
    let f: Field<Extensive> = field_from(&particles, |p| p[0] - 2.0 * p[1] + 0.5);
    let mut u_at_p = Field::<Intensive>::zeros(particles.len(), 1);
    let mut f_on_grid = Field::<Extensive>::zeros(grid.len(), 1);
    pair.sample(&u, &mut u_at_p);
    pair.deposit(&f, &mut f_on_grid);

    let power_particles = dot(&f.values, &u_at_p.values);
    let power_grid = dot(&f_on_grid.values, &u.values);
    assert!((power_particles - power_grid).abs() < 1e-12 * power_particles.abs().max(1.0));
    assert!((f.totals()[0] - f_on_grid.totals()[0]).abs() < 1e-12);

    // The standalone conservative map particles -> grid is the same operator.
    let cons = MapBuilder::new(&particles, &grid).conservative();
    let mut f2 = Field::<Extensive>::zeros(grid.len(), 1);
    cons.apply(&f, &mut f2);
    assert!(max_abs_diff(&f2.values, &f_on_grid.values) < TOL);
}

#[test]
fn cloud_in_cell_weights_match_hand_computation() {
    // 2 x 2 x 1 grid on the unit square; centers at 0.25 and 0.75.
    let grid =
        UniformGrid::new([0.0; 3], [1.0, 1.0, 1.0], [2, 2, 1]).with_basis(CellBasis::Trilinear);
    let p = PointCloud::new(vec![[0.375, 0.625, 0.5]]);
    let pair = AdjointPair::new(&p, &grid, Method::Native);
    let mut on_grid = Field::<Extensive>::zeros(4, 1);
    pair.deposit(&Field::scalar(vec![1.0]), &mut on_grid);
    // x: 0.375 is 1/4 of the way from 0.25 to 0.75; y: 0.625 is 3/4.
    let expected = [0.75 * 0.25, 0.25 * 0.25, 0.75 * 0.75, 0.25 * 0.75];
    assert!(
        max_abs_diff(&on_grid.values, &expected) < TOL,
        "{:?}",
        on_grid.values
    );
}

#[test]
fn trilinear_grid_reproduces_linear_fields_between_centers() {
    let grid = UniformGrid::new([0.0; 3], [1.0; 3], [5, 4, 3]).with_basis(CellBasis::Trilinear);
    let lin = |p: Point| 1.0 + 2.0 * p[0] - 3.0 * p[1] + 0.5 * p[2];
    let src: Field<Intensive> = field_from(&grid, lin);
    // Points strictly inside the hull of cell centers.
    let pts = PointCloud::new(vec![
        [0.31, 0.44, 0.5],
        [0.15, 0.2, 0.25],
        [0.88, 0.8, 0.79],
    ]);
    let map = MapBuilder::new(&grid, &pts).consistent();
    let mut out = Field::<Intensive>::zeros(3, 1);
    map.apply(&src, &mut out);
    for (i, p) in pts.points().iter().enumerate() {
        assert!((out.values[i] - lin(*p)).abs() < 1e-12);
    }
}

#[test]
fn p1_surfaces_reproduce_linear_fields_across_nonmatching_meshes() {
    // Two different triangulations of the same plate, the second lifted by
    // 1e-5 (inside the projection tolerance).
    let a = plane(4, 3, 1.0, 1.0, 0.0, SurfaceDofs::Vertex, 1e-4);
    let b = plane(7, 5, 1.0, 1.0, 1e-5, SurfaceDofs::Vertex, 1e-4);
    let lin = |p: Point| 3.0 * p[0] - p[1] + 2.0;
    let src: Field<Intensive> = field_from(&a, lin);
    let map = MapBuilder::new(&a, &b).consistent();
    assert!(map.report().is_complete(1e-12));
    let mut out = Field::<Intensive>::zeros(b.len(), 1);
    map.apply(&src, &mut out);
    let exact: Vec<f64> = (0..b.len()).map(|i| lin(b.centroid(i))).collect();
    // Vertex data interpolate by default, and P1 reproduces linear fields.
    assert!(
        max_abs_diff(&out.values, &exact) < 1e-12,
        "{}",
        max_abs_diff(&out.values, &exact)
    );

    // Averaging instead is a lumped L2 projection: smoother, first-order
    // accurate, still constant-preserving.
    let avg = MapBuilder::new(&a, &b)
        .sampling(Sampling::Average)
        .consistent();
    let mut out_avg = Field::<Intensive>::zeros(b.len(), 1);
    avg.apply(&src, &mut out_avg);
    let e = max_abs_diff(&out_avg.values, &exact);
    assert!(e > 1e-6 && e < 0.2, "{e}");
}

/// L2 error of mapping a smooth field from an `n`-cell grid to a fixed
/// fine grid.
fn grid_to_grid_error(n: usize, basis: CellBasis, interior_only: bool) -> f64 {
    let f = |p: Point| (std::f64::consts::PI * p[0]).sin() * (std::f64::consts::PI * p[1]).cos();
    let src = UniformGrid::new([0.0; 3], [1.0; 3], [n, n, 1]).with_basis(basis);
    let dst = UniformGrid::new([0.0; 3], [1.0; 3], [97, 89, 1]).with_quadrature_order(1);
    let map = MapBuilder::new(&src, &dst).consistent();
    let s: Field<Intensive> = field_from(&src, f);
    let mut d = Field::<Intensive>::zeros(dst.len(), 1);
    map.apply(&s, &mut d);
    // Within half a source cell of the wall the trilinear basis is clamped
    // (constant extrapolation), which is first order there.
    let margin = if interior_only { 0.5 / 8.0 } else { 0.0 };
    let keep: Vec<usize> = (0..dst.len())
        .filter(|&i| {
            let c = dst.centroid(i);
            (0..2).all(|d| c[d] > margin && c[d] < 1.0 - margin)
        })
        .collect();
    let e2: f64 = keep
        .iter()
        .map(|&i| (d.values[i] - f(dst.centroid(i))).powi(2))
        .sum();
    (e2 / keep.len() as f64).sqrt()
}

#[test]
fn grid_mapping_converges_at_the_expected_order() {
    // Ratios per halving of h: about 2 is first order, about 4 second order.
    for (basis, interior, min_ratio) in [
        (CellBasis::Constant, false, 1.8),
        (CellBasis::Trilinear, false, 2.5),
        (CellBasis::Trilinear, true, 3.5),
    ] {
        let e: Vec<f64> = [8, 16, 32]
            .iter()
            .map(|&n| grid_to_grid_error(n, basis, interior))
            .collect();
        for k in 0..2 {
            let ratio = e[k] / e[k + 1];
            assert!(
                ratio > min_ratio,
                "{basis:?} interior={interior}: errors {e:?}, ratio {ratio}"
            );
        }
    }
}

#[test]
fn partial_coverage_is_reported() {
    let small = UniformGrid::new([0.0; 3], [1.0; 3], [2, 2, 2]);
    // Larger box: cells beyond x = 1 are outside, the cell straddling it is partial.
    let big = UniformGrid::new([0.0; 3], [1.6, 1.0, 1.0], [4, 2, 2]);
    let map = MapBuilder::new(&small, &big).consistent();
    let r = map.report();
    // Cells with x in [1.2, 1.6] are entirely outside: ix = 3.
    let outside: Vec<usize> = (0..big.len()).filter(|i| i % 4 == 3).collect();
    assert_eq!(r.unmapped, outside);
    // Cells with x in [0.8, 1.2] straddle the edge: ix = 2.
    assert_eq!(r.partial, 4);
    assert!(!r.is_complete(1e-12));
}

#[test]
fn nearest_neighbor_method_assigns_whole_values() {
    let sensors = PointCloud::new(vec![[0.1, 0.1, 0.5], [0.9, 0.9, 0.5]]);
    let grid = UniformGrid::new([0.0; 3], [1.0; 3], [4, 4, 1]);
    let map = MapBuilder::new(&sensors, &grid)
        .method(Method::NearestNeighbor)
        .consistent();
    let mut out = Field::<Intensive>::zeros(16, 1);
    map.apply(&Field::scalar(vec![1.0, 2.0]), &mut out);
    assert_eq!(out.values[grid.index(0, 0, 0)], 1.0);
    assert_eq!(out.values[grid.index(3, 3, 0)], 2.0);
    assert!(map.report().is_complete(0.0));
}

#[test]
fn density_and_extensive_round_trip() {
    let surf = plane(3, 2, 2.0, 1.0, 0.0, SurfaceDofs::Face, 1e-9);
    let m = surf.measures();
    assert!((m.iter().sum::<f64>() - 2.0).abs() < 1e-12);
    let q = Field::<Density>::scalar((0..surf.len()).map(|i| i as f64).collect());
    let back = q.to_extensive(&m).to_density(&m);
    assert!(max_abs_diff(&q.values, &back.values) < 1e-12);
}

#[test]
fn volume_trace_onto_boundary_surface_reads_the_adjacent_cells() {
    let grid = UniformGrid::new([0.0; 3], [1.0; 3], [4, 4, 4]);
    let values = Field::<Intensive>::scalar((0..64).map(|i| (i / 16) as f64).collect()); // = iz
    let top = plane(4, 4, 1.0, 1.0, 1.0, SurfaceDofs::Face, 1e-9);
    let map = MapBuilder::new(&grid, &top).consistent();
    let mut out = Field::<Intensive>::zeros(top.len(), 1);
    map.apply(&values, &mut out);
    assert!(
        out.values.iter().all(|v| (*v - 3.0).abs() < TOL),
        "{:?}",
        out.values
    );
}

/// Uniform flux on a coarse surface, deposited conservatively onto a grid
/// whose cells are comparable to the triangles: each cell should receive
/// flux × (its overlap with the surface). The default 3-point rule is far
/// off; subdividing converges to the exact overlap.
fn surface_deposit_error(subdivision: usize) -> f64 {
    // 3 × 3 quads (18 triangles) over [0,1]², grid of 5 × 5 cells.
    let surf = plane(3, 3, 1.0, 1.0, 0.5, SurfaceDofs::Face, 1e-9).with_subdivision(subdivision);
    let grid = UniformGrid::new([0.0; 3], [1.0; 3], [5, 5, 1]);
    let flux = Field::<Density>::scalar(vec![2.0; surf.len()]);
    let q = flux.to_extensive(&surf.measures());
    let map = MapBuilder::new(&surf, &grid).conservative();
    let mut out = Field::<Extensive>::zeros(grid.len(), 1);
    map.apply(&q, &mut out);
    assert!(
        (out.totals()[0] - 2.0).abs() < TOL,
        "total {}",
        out.totals()[0]
    );
    let exact = 2.0 / 25.0;
    out.values
        .iter()
        .map(|v| (v - exact).abs() / exact)
        .fold(0.0, f64::max)
}

#[test]
fn subdivided_surface_quadrature_converges_to_exact_overlap() {
    let errs: Vec<f64> = [1, 2, 4, 8, 16]
        .iter()
        .map(|&n| surface_deposit_error(n))
        .collect();
    eprintln!("max relative cell error by subdivision 1,2,4,8,16: {errs:?}");
    assert!(
        errs[0] > 0.2,
        "default rule unexpectedly accurate: {}",
        errs[0]
    );
    assert!(errs[4] < 0.1, "subdivision 16 still off: {}", errs[4]);
    assert!(errs[4] < errs[0] / 8.0);
}
