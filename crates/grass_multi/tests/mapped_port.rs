//! Mapped ports: solvers on different discretizations coupled without either
//! naming the other's mesh.
//!
//! A "shell" solver lives on a triangulated surface with its own vertex
//! layout. A "flow" solver lives on a uniform grid of cells. They exchange
//! wall temperature (sampled) one way and wall heat (redistributed) the
//! other, through two `MappedPort`s.

use std::any::TypeId;

use grass_app::prelude::*;
use grass_map::{
    CellBasis, Discretization, Extensive, Field, Intensive, MapReport, Method, PointCloud,
    SurfaceDofs, TriSurface, UniformGrid,
};
use grass_multi::{
    consume_on, expose_on, tick_subapp, MappedPort, Mesh, MultiAppExt, OuterIterStopPlugin,
    Quantity, SubApps,
};
use grass_scheduler::prelude::*;

// ─── Contracts ──────────────────────────────────────────────────────────────

struct WallTemperature;
impl Quantity for WallTemperature {
    type Kind = Intensive;
}

struct WallHeat;
impl Quantity for WallHeat {
    type Kind = Extensive;
}

// ─── Shell: a triangulated plate at z = 0.05 ────────────────────────────────

struct Shell {
    surface: Mesh,
    t_wall: Vec<f64>,
    q_in: Vec<f64>,
    q_total_seen: Vec<f64>,
}

fn plate(nv: usize) -> TriSurface {
    let mut v = Vec::new();
    for j in 0..nv {
        for i in 0..nv {
            let s = 1.0 / (nv - 1) as f64;
            v.push([i as f64 * s, j as f64 * s, 0.05]);
        }
    }
    let mut t = Vec::new();
    for j in 0..nv - 1 {
        for i in 0..nv - 1 {
            let a = j * nv + i;
            t.push([a, a + 1, a + nv + 1]);
            t.push([a, a + nv + 1, a + nv]);
        }
    }
    TriSurface::new(v, t, SurfaceDofs::Vertex, 0.06)
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum ShellPhase {
    Step,
}

// Record the heat received this step; the wall temperature is a fixed linear
// profile so the mapped value can be checked exactly.
fn shell_step(mut s: ResMut<Shell>) {
    let total = s.q_in.iter().sum();
    s.q_total_seen.push(total);
}

fn build_shell(nv: usize) -> App {
    let surface = Mesh::new(plate(nv));
    let t_wall = (0..surface.len())
        .map(|i| 400.0 + 100.0 * surface.centroid(i)[0])
        .collect();
    let mut app = App::new();
    app.add_resource(Shell {
        q_in: vec![0.0; surface.len()],
        surface,
        t_wall,
        q_total_seen: Vec::new(),
    });
    app.add_update_system(shell_step, ShellPhase::Step);
    app
}

// ─── Flow: a uniform grid of cells ──────────────────────────────────────────

struct Flow {
    grid: Mesh,
    t_cell: Vec<f64>,
    unmapped: usize,
    q_cell: Vec<f64>,
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum FlowPhase {
    Step,
}

// Each cell deposits heat proportional to its distance from the wall
// temperature 450 K; the exact total depends on what the flow received.
fn flow_step(mut f: ResMut<Flow>) {
    let q: Vec<f64> = f
        .t_cell
        .iter()
        .map(|t| 0.01 * (t - 450.0).abs() + 1.0)
        .collect();
    f.q_cell = q;
}

fn build_flow(n: usize) -> App {
    let grid = Mesh::new(
        UniformGrid::new([0.0; 3], [1.0, 1.0, 0.1], [n, n, 1]).with_basis(CellBasis::Trilinear),
    );
    let mut app = App::new();
    app.add_resource(Flow {
        t_cell: vec![0.0; grid.len()],
        q_cell: vec![0.0; grid.len()],
        unmapped: 0,
        grid,
    });
    app.add_update_system(flow_step, FlowPhase::Step);
    app
}

// ─── Parent ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    TickShell,
    ShellToFlow,
    TickFlow,
    FlowToShell,
    Check,
}

fn with_sub<T: 'static, R>(parent: &App, ns: &str, f: impl FnOnce(&T) -> R) -> R {
    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let app = subs.find(ns).unwrap();
    let cell = app.resource_cell(TypeId::of::<T>()).unwrap().borrow();
    f(cell.downcast_ref::<T>().unwrap())
}

fn coupled(shell_nv: usize, flow_n: usize, iters: u32) -> App {
    let mut parent = App::new();
    parent.add_subapp("shell", build_shell(shell_nv));
    parent.add_subapp("flow", build_flow(flow_n));
    parent.add_mapped_port::<WallTemperature>(Method::Native);
    parent.add_mapped_port::<WallHeat>(Method::Native);

    parent.add_update_system(tick_subapp("shell", 1), Phase::TickShell);
    parent.add_update_system(
        expose_on::<Shell, WallTemperature>("shell", |s| {
            (s.surface.clone(), Field::scalar(s.t_wall.clone()))
        }),
        Phase::ShellToFlow,
    );
    parent.add_update_system(
        consume_on::<Flow, WallTemperature>(
            "flow",
            |f| f.grid.clone(),
            |f, t, report: &MapReport| {
                f.t_cell.clone_from(&t.values);
                f.unmapped = report.unmapped.len();
            },
        ),
        Phase::ShellToFlow,
    );
    parent.add_update_system(tick_subapp("flow", 1), Phase::TickFlow);
    parent.add_update_system(
        expose_on::<Flow, WallHeat>("flow", |f| {
            (f.grid.clone(), Field::scalar(f.q_cell.clone()))
        }),
        Phase::FlowToShell,
    );
    parent.add_update_system(
        consume_on::<Shell, WallHeat>(
            "shell",
            |s| s.surface.clone(),
            |s, q, _| s.q_in.clone_from(&q.values),
        ),
        Phase::FlowToShell,
    );
    parent.add_plugins(OuterIterStopPlugin {
        n_iters: iters,
        phase: Phase::Check,
    });
    parent
}

#[test]
fn surface_and_grid_exchange_temperature_and_heat() {
    let mut parent = coupled(7, 10, 4);
    parent.start();

    // Temperature: a linear profile on the shell's P1 vertices, averaged over
    // each flow cell's quadrature points, is exact at the cell center.
    let (t_cell, centers, unmapped, q_cell) = with_sub::<Flow, _>(&parent, "flow", |f| {
        let centers: Vec<_> = (0..f.grid.len()).map(|i| f.grid.centroid(i)).collect();
        (f.t_cell.clone(), centers, f.unmapped, f.q_cell.clone())
    });
    assert_eq!(unmapped, 0);
    for (t, c) in t_cell.iter().zip(&centers) {
        let exact = 400.0 + 100.0 * c[0];
        assert!((t - exact).abs() < 1e-9, "cell at {c:?}: {t} vs {exact}");
    }

    // Heat: every joule the flow deposited arrives on the shell.
    let q_flow: f64 = q_cell.iter().sum();
    let seen = with_sub::<Shell, _>(&parent, "shell", |s| s.q_in.iter().sum::<f64>());
    assert!(
        (seen - q_flow).abs() < 1e-9 * q_flow,
        "shell got {seen}, flow sent {q_flow}"
    );

    // Static meshes: each port built one map, once.
    let t_builds = parent
        .get_resource_ref::<MappedPort<WallTemperature>>()
        .unwrap()
        .map_builds();
    let q_builds = parent
        .get_resource_ref::<MappedPort<WallHeat>>()
        .unwrap()
        .map_builds();
    assert_eq!((t_builds, q_builds), (1, 1));
}

#[test]
fn swapping_either_mesh_needs_no_coupling_changes() {
    // Same coupling code, different resolutions on both sides.
    for (nv, n) in [(3, 4), (12, 5), (5, 17)] {
        let mut parent = coupled(nv, n, 2);
        parent.start();
        let (t_cell, centers, q_flow) = with_sub::<Flow, _>(&parent, "flow", |f| {
            let c: Vec<_> = (0..f.grid.len()).map(|i| f.grid.centroid(i)).collect();
            (f.t_cell.clone(), c, f.q_cell.iter().sum::<f64>())
        });
        for (t, c) in t_cell.iter().zip(&centers) {
            assert!((t - (400.0 + 100.0 * c[0])).abs() < 1e-9);
        }
        let seen = with_sub::<Shell, _>(&parent, "shell", |s| s.q_in.iter().sum::<f64>());
        assert!((seen - q_flow).abs() < 1e-9 * q_flow, "nv={nv} n={n}");
    }
}

// ─── Moving points: the map rebuilds only when the mesh handle changes ──────

struct Particles {
    cloud: Mesh,
    t: Vec<f64>,
    step: usize,
}

struct Gas {
    grid: Mesh,
    t: Vec<f64>,
}

struct GasTemperature;
impl Quantity for GasTemperature {
    type Kind = Intensive;
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum PPhase {
    Move,
}

// The particles move (and so get a new mesh) on steps 3 and 6 only.
fn move_particles(mut p: ResMut<Particles>) {
    p.step += 1;
    if p.step.is_multiple_of(3) {
        let shift = 0.05 * p.step as f64;
        let pts = (0..4)
            .map(|i| [0.1 + 0.2 * i as f64 + shift, 0.5, 0.5])
            .collect();
        p.cloud = Mesh::new(PointCloud::new(pts));
    }
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum GasPhase {
    Idle,
}
fn gas_idle(_g: Res<Gas>) {}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum P2 {
    TickP,
    TickGas,
    Couple,
    Check,
}

#[test]
fn map_is_rebuilt_only_when_a_mesh_changes() {
    let pts: Vec<_> = (0..4).map(|i| [0.1 + 0.2 * i as f64, 0.5, 0.5]).collect();
    let mut p_app = App::new();
    p_app.add_resource(Particles {
        cloud: Mesh::new(PointCloud::new(pts)),
        t: vec![0.0; 4],
        step: 0,
    });
    p_app.add_update_system(move_particles, PPhase::Move);

    let grid =
        UniformGrid::new([0.0; 3], [2.0, 1.0, 1.0], [8, 4, 4]).with_basis(CellBasis::Trilinear);
    let t: Vec<f64> = (0..grid.len())
        .map(|i| 300.0 + 50.0 * grid.center(i)[0])
        .collect();
    let mut gas_app = App::new();
    gas_app.add_resource(Gas {
        grid: Mesh::new(grid),
        t,
    });
    gas_app.add_update_system(gas_idle, GasPhase::Idle);

    let mut parent = App::new();
    parent.add_subapp("particles", p_app);
    parent.add_subapp("gas", gas_app);
    parent.add_mapped_port::<GasTemperature>(Method::Native);
    parent.add_update_system(tick_subapp("particles", 1), P2::TickP);
    parent.add_update_system(tick_subapp("gas", 1), P2::TickGas);
    parent.add_update_system(
        expose_on::<Gas, GasTemperature>("gas", |g| (g.grid.clone(), Field::scalar(g.t.clone()))),
        P2::Couple,
    );
    parent.add_update_system(
        consume_on::<Particles, GasTemperature>(
            "particles",
            |p| p.cloud.clone(),
            |p, t, report| {
                assert!(report.unmapped.is_empty());
                p.t.clone_from(&t.values);
            },
        ),
        P2::Couple,
    );
    parent.add_plugins(OuterIterStopPlugin {
        n_iters: 7,
        phase: P2::Check,
    });
    parent.start();

    // Initial build plus one per move (steps 3 and 6).
    let builds = parent
        .get_resource_ref::<MappedPort<GasTemperature>>()
        .unwrap()
        .map_builds();
    assert_eq!(builds, 3);

    // Trilinear interpolation of a linear profile is exact away from walls:
    // the particles see the gas temperature at their current positions.
    let (t, xs) = with_sub::<Particles, _>(&parent, "particles", |p| {
        let xs: Vec<f64> = (0..4).map(|i| p.cloud.centroid(i)[0]).collect();
        (p.t.clone(), xs)
    });
    for (ti, x) in t.iter().zip(&xs) {
        assert!((ti - (300.0 + 50.0 * x)).abs() < 1e-9, "x={x}: {ti}");
    }
}

#[test]
fn unreachable_consumer_entities_are_zero_and_reported() {
    let mut port = MappedPort::<GasTemperature>::new(Method::Native);
    let grid = Mesh::new(UniformGrid::new([0.0; 3], [1.0; 3], [2, 2, 2]));
    port.publish(grid.clone(), Field::scalar(vec![7.0; 8]));
    let probes = Mesh::new(PointCloud::new(vec![[0.5; 3], [5.0, 0.5, 0.5]]));
    let map = port.build_map(&grid, &probes);
    let mut out = Field::zeros(2, 1);
    map.apply(&port.get().unwrap().field, &mut out);
    assert_eq!(out.values, vec![7.0, 0.0]);
    assert_eq!(map.report().unmapped, vec![1]);
}

#[test]
#[should_panic(expected = "entries but the mesh has")]
fn publishing_a_field_of_the_wrong_length_panics() {
    let mut port = MappedPort::<WallHeat>::new(Method::Native);
    let grid = Mesh::new(UniformGrid::new([0.0; 3], [1.0; 3], [2, 2, 2]));
    port.publish(grid, Field::scalar(vec![1.0; 3]));
}
