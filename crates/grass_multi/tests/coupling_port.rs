//! Coupling-contract integration test: two *independent*, paradigm-different
//! solvers coupled through a [`Port`] in a few lines.
//!
//! - Producer `"field"` is **mesh-style**: a 1-D scalar field (`Vec<f64>`)
//!   that relaxes by conservative diffusion each step. It exposes its total
//!   (a conserved source term) as the contract value `Flux`.
//! - Consumer `"particle"` is **point-style**: a single mass integrated with
//!   semi-implicit Euler, driven by a force it reads from the contract.
//!
//! Neither solver names the other. The producer knows `HeatField` + `Flux`;
//! the consumer knows `Particle` + `Flux`. The shared `Flux` newtype is the
//! whole contract. This is the "couple any grass solver to any other" story,
//! and it is paradigm-agnostic: mesh drives particle here, but nothing in the
//! coupling layer knows or cares.

use grass_app::prelude::*;
use grass_multi::{
    consume_field, expose_field, tick_subapp, MultiAppExt, OuterIterStopPlugin, SubApps,
};
use grass_scheduler::prelude::*;

// ─── The coupling contract: a scalar source term ───────────────────────────
// The ONLY type shared between the two solvers.

#[derive(Debug, Clone, Copy)]
struct Flux(f64);

const DT: f64 = 0.1;

// ─── Producer: a mesh-style scalar field solver ─────────────────────────────

struct HeatField {
    cells: Vec<f64>,
}
impl HeatField {
    fn total(&self) -> f64 {
        self.cells.iter().sum()
    }
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum FieldPhase {
    Step,
}

// Conservative diffusion with reflecting boundaries: the sum of `cells` is
// invariant (each cell trades with its neighbours), so `total()` — the
// exposed source term — stays constant while the internal field evolves.
fn diffuse(mut f: ResMut<HeatField>) {
    const ALPHA: f64 = 0.2;
    let n = f.cells.len();
    let old = f.cells.clone();
    for i in 0..n {
        let left = if i > 0 { old[i - 1] } else { old[i] };
        let right = if i + 1 < n { old[i + 1] } else { old[i] };
        f.cells[i] = old[i] + ALPHA * (left + right - 2.0 * old[i]);
    }
}

fn build_field() -> App {
    let mut app = App::new();
    app.add_resource(HeatField {
        cells: vec![1.0, 2.0, 3.0, 4.0], // total = 10.0
    });
    app.add_update_system(diffuse, FieldPhase::Step);
    app
}

// ─── Consumer: a point-particle solver ──────────────────────────────────────

struct Particle {
    force: f64,
    vel: f64,
    pos: f64,
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum ParticlePhase {
    Integrate,
}

// Semi-implicit (symplectic) Euler, unit mass: a = force.
fn integrate(mut p: ResMut<Particle>) {
    p.vel += p.force * DT;
    let v = p.vel;
    p.pos += v * DT;
}

fn build_particle() -> App {
    let mut app = App::new();
    app.add_resource(Particle {
        force: 0.0,
        vel: 0.0,
        pos: 0.0,
    });
    app.add_update_system(integrate, ParticlePhase::Integrate);
    app
}

// ─── Parent schedule: Tick → Couple → Tick → Check ──────────────────────────

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    TickProducer,
    Couple,
    TickConsumer,
    Check,
}

#[test]
fn mesh_field_drives_particle_through_a_port() {
    const N: u32 = 5;

    let mut parent = App::new();

    // Two independent solvers + the one shared contract slot.
    parent.add_subapp("field", build_field());
    parent.add_subapp("particle", build_particle());
    parent.add_port::<Flux>();

    // The whole coupling, in three lines:
    parent.add_update_system(tick_subapp("field", 1), Phase::TickProducer);
    parent.add_update_system(
        expose_field::<HeatField, Flux>("field", |f| Flux(f.total())),
        Phase::Couple,
    );
    parent.add_update_system(
        consume_field::<Particle, Flux>("particle", |p, flux| p.force = flux.0),
        Phase::Couple,
    );
    parent.add_update_system(tick_subapp("particle", 1), Phase::TickConsumer);

    parent.add_plugins(OuterIterStopPlugin {
        n_iters: N,
        phase: Phase::Check,
    });

    parent.start();

    // ── Read back both sub-Apps' final state ────────────────────────────────
    let subs = parent.get_resource_ref::<SubApps>().unwrap();

    let field_total = {
        let cell = subs
            .find("field")
            .unwrap()
            .resource_cell(std::any::TypeId::of::<HeatField>())
            .unwrap()
            .borrow();
        cell.downcast_ref::<HeatField>().unwrap().total()
    };
    let (vel, pos, force) = {
        let cell = subs
            .find("particle")
            .unwrap()
            .resource_cell(std::any::TypeId::of::<Particle>())
            .unwrap()
            .borrow();
        let p = cell.downcast_ref::<Particle>().unwrap();
        (p.vel, p.pos, p.force)
    };

    // Producer conserves its total → the exposed source term is constant 10.0.
    assert!(
        (field_total - 10.0).abs() < 1e-9,
        "field total drifted: {field_total}"
    );

    // Consumer saw the coupled force each step.
    assert!((force - 10.0).abs() < 1e-12, "force not coupled: {force}");

    // Closed form: constant force F=10, unit mass, N semi-implicit Euler steps
    // at dt=0.1. vel_N = N*F*dt; pos_N = dt * F*dt * N(N+1)/2.
    let f = 10.0_f64;
    let expected_vel = N as f64 * f * DT;
    let expected_pos = DT * f * DT * (N as f64 * (N as f64 + 1.0) / 2.0);
    assert!(
        (vel - expected_vel).abs() < 1e-9,
        "vel {vel} != expected {expected_vel}"
    );
    assert!(
        (pos - expected_pos).abs() < 1e-9,
        "pos {pos} != expected {expected_pos}"
    );
}

// A second producer publishing a *different* contract type shows ports compose
// and that a consumer depends only on the port type it reads — not on how many
// other couplings exist. Here a constant "gravity" bias is added via its own
// port, independent of the field→particle flux coupling.
#[derive(Debug, Clone, Copy)]
struct Bias(f64);

struct Gravity(f64);

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum GravPhase {
    Noop,
}
fn grav_noop(_g: Res<Gravity>) {}

fn build_gravity() -> App {
    let mut app = App::new();
    app.add_resource(Gravity(-2.0));
    app.add_update_system(grav_noop, GravPhase::Noop);
    app
}

#[test]
fn two_ports_compose_independently() {
    const N: u32 = 3;

    let mut parent = App::new();
    parent.add_subapp("field", build_field());
    parent.add_subapp("gravity", build_gravity());
    parent.add_subapp("particle", build_particle());
    parent.add_port::<Flux>();
    parent.add_port::<Bias>();

    parent.add_update_system(tick_subapp("field", 1), Phase::TickProducer);
    parent.add_update_system(tick_subapp("gravity", 1), Phase::TickProducer);

    parent.add_update_system(
        expose_field::<HeatField, Flux>("field", |f| Flux(f.total())),
        Phase::Couple,
    );
    parent.add_update_system(
        expose_field::<Gravity, Bias>("gravity", |g| Bias(g.0)),
        Phase::Couple,
    );
    // Consumer combines two independent contracts into its own force.
    parent.add_update_system(
        consume_field::<Particle, Flux>("particle", |p, flux| p.force = flux.0),
        Phase::Couple,
    );
    parent.add_update_system(
        consume_field::<Particle, Bias>("particle", |p, bias| p.force += bias.0),
        Phase::Couple,
    );
    parent.add_update_system(tick_subapp("particle", 1), Phase::TickConsumer);

    parent.add_plugins(OuterIterStopPlugin {
        n_iters: N,
        phase: Phase::Check,
    });

    parent.start();

    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let (vel, force) = {
        let cell = subs
            .find("particle")
            .unwrap()
            .resource_cell(std::any::TypeId::of::<Particle>())
            .unwrap()
            .borrow();
        let p = cell.downcast_ref::<Particle>().unwrap();
        (p.vel, p.force)
    };

    // Effective force each step = flux(10) + bias(-2) = 8.0.
    assert!((force - 8.0).abs() < 1e-12, "combined force wrong: {force}");
    let expected_vel = N as f64 * 8.0 * DT;
    assert!(
        (vel - expected_vel).abs() < 1e-9,
        "vel {vel} != {expected_vel}"
    );
}
