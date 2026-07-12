//! End-to-end convergence test for the Picard/Aitken outer-iteration
//! combinator on a **toy strongly-coupled 2-field problem**, wired through the
//! real grass_multi machinery (two sub-Apps + a [`Port`] + the parent schedule).
//!
//! The two solvers form a strong two-way coupling whose interface fixed-point
//! map is `G(x) = γ·α·x + (γ·β + δ) = a·x + b`. With `α=3, γ=−1` we get
//! `a = −3`: plain Picard (block Gauss–Seidel, ω=1) **oscillates and diverges**,
//! exactly the partitioned-FSI failure mode the combinator exists to fix. The
//! analytic interface fixed point is `x* = b / (1 − a)`.
//!
//!   - **Fluid** `"fluid"`: reads interface `x`, produces `p = α·x + β`.
//!   - **Solid** `"solid"`: reads `p`, produces the new interface `x̃ = γ·p + δ`.
//!
//! Neither solver names the other; they share only the `P` (pressure) port. The
//! combinator relaxes the interface residual `x̃ − x`, feeds the relaxed value
//! back into the fluid, and stops the parent loop on the convergence test.

use grass_app::prelude::*;
use grass_multi::{
    consume_field, converge_outer_iter, expose_field, tick_subapp, Multi, MultiAppExt,
    OuterIteration, Relaxation, SubApps,
};
use grass_scheduler::prelude::*;

// Coupling coefficients: a = γ·α = −3 (strong; plain Picard diverges).
const ALPHA: f64 = 3.0;
const BETA: f64 = 1.0;
const GAMMA: f64 = -1.0;
const DELTA: f64 = 2.0;

// Interface fixed point x* = b/(1−a), b = γ·β + δ, a = γ·α.
fn x_star() -> f64 {
    let a = GAMMA * ALPHA;
    let b = GAMMA * BETA + DELTA;
    b / (1.0 - a) // = 1 / 4 = 0.25
}

// ─── The coupling contract: the fluid's "pressure" ──────────────────────────
#[derive(Debug, Clone, Copy)]
struct Pressure(f64);

// ─── Fluid solver: p_out = α·x_in + β ───────────────────────────────────────
struct Fluid {
    x_in: f64,
    p_out: f64,
}
#[derive(Clone, Copy, Debug, ScheduleSet)]
enum FluidPhase {
    Step,
}
fn fluid_step(mut f: ResMut<Fluid>) {
    f.p_out = ALPHA * f.x_in + BETA;
}
fn build_fluid(x0: f64) -> App {
    let mut app = App::new();
    app.add_resource(Fluid {
        x_in: x0,
        p_out: 0.0,
    });
    app.add_update_system(fluid_step, FluidPhase::Step);
    app
}

// ─── Solid solver: x_out = γ·p_in + δ ───────────────────────────────────────
struct Solid {
    p_in: f64,
    x_out: f64,
}
#[derive(Clone, Copy, Debug, ScheduleSet)]
enum SolidPhase {
    Step,
}
fn solid_step(mut s: ResMut<Solid>) {
    s.x_out = GAMMA * s.p_in + DELTA;
}
fn build_solid() -> App {
    let mut app = App::new();
    app.add_resource(Solid {
        p_in: 0.0,
        x_out: 0.0,
    });
    app.add_update_system(solid_step, SolidPhase::Step);
    app
}

// ─── Parent schedule: TickFluid → Couple → TickSolid → Converge ─────────────
#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    TickFluid,
    Couple,
    TickSolid,
    Converge,
}

/// Build the coupled parent App with a chosen relaxation scheme, run it to
/// completion (self-driving via `start`), and read back the converged solid
/// output and the iteration state.
fn run_coupled(relax: Relaxation, max_iters: u32) -> (f64, u32, bool, f64) {
    let x0 = 0.0;
    let mut parent = App::new();

    parent.add_subapp("fluid", build_fluid(x0));
    parent.add_subapp("solid", build_solid());
    parent.add_port::<Pressure>();
    parent.add_resource(OuterIteration::new(vec![x0], relax, 1e-10, max_iters));

    // 1. fluid ticks with its current interface input.
    parent.add_update_system(tick_subapp("fluid", 1), Phase::TickFluid);
    // 2. expose the fluid's pressure and hand it to the solid.
    parent.add_update_system(
        expose_field::<Fluid, Pressure>("fluid", |f| Pressure(f.p_out)),
        Phase::Couple,
    );
    parent.add_update_system(
        consume_field::<Solid, Pressure>("solid", |s, p| s.p_in = p.0),
        Phase::Couple,
    );
    // 3. solid ticks, producing the raw new interface value x̃.
    parent.add_update_system(tick_subapp("solid", 1), Phase::TickSolid);
    // 4. relax x̃, write the next iterate back into the fluid, stop on converge.
    parent.add_update_system(
        converge_outer_iter(
            |w: &Multi| vec![w.expect_read::<Solid>("solid").x_out],
            |w: &Multi, x: &[f64]| w.expect_write::<Fluid>("fluid").x_in = x[0],
        ),
        Phase::Converge,
    );

    parent.start();

    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let x_out = {
        let solid = subs.find("solid").unwrap();
        let cell = solid
            .resource_cell(std::any::TypeId::of::<Solid>())
            .unwrap()
            .borrow();
        cell.downcast_ref::<Solid>().unwrap().x_out
    };
    let it = parent.get_resource_ref::<OuterIteration>().unwrap();
    (x_out, it.iters(), it.converged(), it.residual_norm())
}

#[test]
fn plain_picard_diverges_on_the_coupled_2field_problem() {
    // ω = 1: no relaxation → the interface residual blows up, never converges.
    let (_x_out, iters, converged, res) = run_coupled(Relaxation::Fixed(1.0), 30);
    assert!(!converged, "plain Picard must NOT converge on a=−3");
    assert_eq!(iters, 30, "should run to the iteration cap");
    assert!(res > 1e3, "residual should diverge, got {res}");
}

#[test]
fn fixed_relaxation_converges_to_analytic_fixed_point() {
    // ω = 0.25 = 1/(1−a) is optimal here → converges essentially immediately.
    let (x_out, iters, converged, _res) = run_coupled(Relaxation::Fixed(0.25), 50);
    assert!(converged, "relaxed Picard should converge");
    assert!(
        (x_out - x_star()).abs() < 1e-9,
        "solid x_out {x_out} != analytic x* {}",
        x_star()
    );
    assert!(
        iters <= 2,
        "optimal ω should converge in ≤2 iters, took {iters}"
    );
}

#[test]
fn aitken_converges_without_a_tuned_factor() {
    // Aitken starts from a poor ω0 and auto-adapts to convergence.
    let (x_out, iters, converged, _res) = run_coupled(Relaxation::Aitken { omega0: 0.8 }, 50);
    assert!(
        converged,
        "Aitken should converge on the coupled 2-field map"
    );
    assert!(
        (x_out - x_star()).abs() < 1e-9,
        "solid x_out {x_out} != analytic x* {}",
        x_star()
    );
    assert!(iters <= 6, "Aitken should converge fast, took {iters}");
}
