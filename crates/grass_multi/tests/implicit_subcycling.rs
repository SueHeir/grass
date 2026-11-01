//! Implicit coupling over time windows, with subcycling and time
//! interpolation, against a monolithic reference.
//!
//! Two scalar solvers form the linear system
//!
//! ```text
//! a' = −λa·a + kab·b        (solver "a", 3 substeps per window)
//! b' = −λb·b + kba·a        (solver "b", 2 substeps per window)
//! ```
//!
//! each integrating its own equation with the trapezoidal rule and reading
//! the other's value from a [`Waveform`] at its own substep times. The
//! interface iterate is b's waveform over the window. With `kab·kba = −48`
//! and a coarse window, plain Picard iteration diverges; IQN-ILS and Aitken
//! converge every window, and the converged scheme is second order in time
//! with linear interpolation and first order without it.

use std::any::TypeId;

use grass_app::prelude::*;
use grass_multi::{
    checkpoint_window, consume_field, converge_window, expose_field, tick_subapp, ImplicitCoupling,
    Interpolation, Multi, MultiAppExt, OuterIteration, Relaxation, SubApps, Waveform, WindowStep,
};
use grass_scheduler::prelude::*;

const LA: f64 = 1.0;
const LB: f64 = 2.0;
const KAB: f64 = 6.0;
const KBA: f64 = -8.0;
const A0: f64 = 1.0;
const B0: f64 = 0.0;
const SUB_A: usize = 3;
const SUB_B: usize = 2;

/// One scalar solver: `u' = −λ u + k v`, with `v` read from a waveform.
struct Solver {
    u: f64,
    t: f64,
    h: f64,
    lambda: f64,
    k: f64,
    interp: Interpolation,
    input: Waveform,
    output: Waveform,
    saved: Option<(f64, f64)>,
}

impl Solver {
    fn new(u0: f64, h: f64, lambda: f64, k: f64, interp: Interpolation) -> Self {
        let mut output = Waveform::new(1);
        output.start(0.0, &[u0]);
        Self {
            u: u0,
            t: 0.0,
            h,
            lambda,
            k,
            interp,
            input: Waveform::new(1),
            output,
            saved: None,
        }
    }

    fn save(&mut self) {
        self.saved = Some((self.u, self.t));
        self.output.start(self.t, &[self.u]);
    }

    fn restore(&mut self) {
        let (u, t) = self.saved.expect("checkpoint");
        self.u = u;
        self.t = t;
        self.output.start(t, &[u]);
    }
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum SolverPhase {
    Step,
}

fn trapezoid(mut s: ResMut<Solver>) {
    let (h, l, k) = (s.h, s.lambda, s.k);
    let v0 = s.input.sample(s.t, s.interp)[0];
    let v1 = s.input.sample(s.t + h, s.interp)[0];
    s.u = (s.u * (1.0 - 0.5 * l * h) + 0.5 * h * k * (v0 + v1)) / (1.0 + 0.5 * l * h);
    s.t += h;
    let (t, u) = (s.t, s.u);
    s.output.push(t, &[u]);
}

fn solver_app(s: Solver) -> App {
    let mut app = App::new();
    app.add_resource(s);
    app.add_update_system(trapezoid, SolverPhase::Step);
    app
}

// b's waveform, handed from b to a (through the iteration) and a's to b
// (through this port).
struct AWave(Waveform);

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    Checkpoint,
    TickA,
    Couple,
    TickB,
    Converge,
}

struct Run {
    a: f64,
    b: f64,
    iters: Vec<u32>,
    unconverged: usize,
}

fn run(dt: f64, n_windows: u64, relax: Relaxation, interp: Interpolation, cap: u32) -> Run {
    let mut a = Solver::new(A0, dt / SUB_A as f64, LA, KAB, interp);
    // a's first guess for b over window 0: constant at b's initial value.
    a.input.start(0.0, &[B0]);
    for k in 1..=SUB_B {
        a.input.push(k as f64 * dt / SUB_B as f64, &[B0]);
    }
    let b = Solver::new(B0, dt / SUB_B as f64, LB, KBA, interp);

    let mut parent = App::new();
    parent.add_subapp("a", solver_app(a));
    parent.add_subapp("b", solver_app(b));
    parent.add_port::<AWave>();
    parent.add_resource(ImplicitCoupling::new(
        OuterIteration::new(vec![B0; SUB_B], relax, 1e-12, cap),
        dt,
        n_windows,
    ));

    parent.add_update_system(
        checkpoint_window(
            |w: &Multi| {
                w.expect_write::<Solver>("a").save();
                w.expect_write::<Solver>("b").save();
            },
            |w: &Multi| {
                w.expect_write::<Solver>("a").restore();
                w.expect_write::<Solver>("b").restore();
            },
        ),
        Phase::Checkpoint,
    );
    parent.add_update_system(tick_subapp("a", SUB_A), Phase::TickA);
    parent.add_update_system(
        expose_field::<Solver, AWave>("a", |s| AWave(s.output.clone())),
        Phase::Couple,
    );
    parent.add_update_system(
        consume_field::<Solver, AWave>("b", |s, w| s.input = w.0.clone()),
        Phase::Couple,
    );
    parent.add_update_system(tick_subapp("b", SUB_B), Phase::TickB);
    parent.add_update_system(
        converge_window(
            |w: &Multi| w.expect_read::<Solver>("b").output.interior().to_vec(),
            |w: &Multi, x: &[f64], step: WindowStep| {
                let mut a = w.expect_write::<Solver>("a");
                match step {
                    WindowStep::Repeat => a.input.set_interior(x),
                    WindowStep::Advance => {
                        // The new window starts from b's accepted end state;
                        // the remaining samples are the next guess.
                        let b = w.expect_read::<Solver>("b");
                        let (t0, b0) = (b.t, b.u);
                        a.input.start(t0, &[b0]);
                        for (k, v) in x.iter().enumerate() {
                            let t = t0 + (k + 1) as f64 * b.h;
                            a.input.push(t, &[*v]);
                        }
                    }
                }
            },
        ),
        Phase::Converge,
    );
    parent.start();

    let read = |ns: &str| {
        let subs = parent.get_resource_ref::<SubApps>().unwrap();
        let app = subs.find(ns).unwrap();
        let cell = app.resource_cell(TypeId::of::<Solver>()).unwrap().borrow();
        let s = cell.downcast_ref::<Solver>().unwrap();
        (s.u, s.t)
    };
    let (ua, ta) = read("a");
    let (ub, tb) = read("b");
    let end = dt * n_windows as f64;
    assert!(
        (ta - end).abs() < 1e-9 && (tb - end).abs() < 1e-9,
        "{ta} {tb}"
    );
    let ic = parent.get_resource_ref::<ImplicitCoupling>().unwrap();
    Run {
        a: ua,
        b: ub,
        iters: ic.iterations_per_window().to_vec(),
        unconverged: ic.unconverged_windows().len(),
    }
}

/// The monolithic system integrated with small RK4 steps.
fn reference(t_end: f64) -> (f64, f64) {
    let f = |a: f64, b: f64| (-LA * a + KAB * b, -LB * b + KBA * a);
    let n = 200_000;
    let h = t_end / n as f64;
    let (mut a, mut b) = (A0, B0);
    for _ in 0..n {
        let k1 = f(a, b);
        let k2 = f(a + 0.5 * h * k1.0, b + 0.5 * h * k1.1);
        let k3 = f(a + 0.5 * h * k2.0, b + 0.5 * h * k2.1);
        let k4 = f(a + h * k3.0, b + h * k3.1);
        a += h / 6.0 * (k1.0 + 2.0 * k2.0 + 2.0 * k3.0 + k4.0);
        b += h / 6.0 * (k1.1 + 2.0 * k2.1 + 2.0 * k3.1 + k4.1);
    }
    (a, b)
}

fn error_at_one(dt: f64, interp: Interpolation) -> f64 {
    let n = (1.0 / dt).round() as u64;
    let r = run(dt, n, Relaxation::iqn_ils(), interp, 50);
    assert_eq!(r.unconverged, 0, "dt={dt}");
    let (a, b) = reference(1.0);
    (r.a - a).abs().max((r.b - b).abs())
}

#[test]
fn linear_interpolation_is_second_order_in_time() {
    let e: Vec<f64> = [0.1, 0.05, 0.025]
        .iter()
        .map(|&dt| error_at_one(dt, Interpolation::Linear))
        .collect();
    for w in e.windows(2) {
        let ratio = w[0] / w[1];
        assert!(ratio > 3.5, "errors {e:?}: ratio {ratio}");
    }
}

#[test]
fn holding_the_next_sample_is_only_first_order() {
    let e: Vec<f64> = [0.05, 0.025, 0.0125]
        .iter()
        .map(|&dt| error_at_one(dt, Interpolation::Constant))
        .collect();
    for w in e.windows(2) {
        let ratio = w[0] / w[1];
        assert!(ratio > 1.6 && ratio < 2.6, "errors {e:?}: ratio {ratio}");
    }
    // And it is much less accurate than interpolating.
    assert!(e[2] > 10.0 * error_at_one(0.0125, Interpolation::Linear));
}

#[test]
fn strong_coupling_needs_acceleration() {
    let (dt, n) = (0.5, 4);
    let picard = run(dt, n, Relaxation::Fixed(1.0), Interpolation::Linear, 30);
    assert_eq!(picard.unconverged, 4, "Picard should fail every window");

    let aitken = run(
        dt,
        n,
        Relaxation::Aitken { omega0: 0.1 },
        Interpolation::Linear,
        50,
    );
    let iqn = run(
        dt,
        n,
        Relaxation::IqnIls {
            omega0: 0.1,
            reuse: 4,
            filter: 1e-8,
        },
        Interpolation::Linear,
        50,
    );
    assert_eq!(aitken.unconverged, 0);
    assert_eq!(iqn.unconverged, 0);
    // Both converge to the same discrete solution.
    assert!((aitken.a - iqn.a).abs() < 1e-9 && (aitken.b - iqn.b).abs() < 1e-9);
    // The interface is two values per window: IQN-ILS needs at most n + 2
    // observations in the first window and fewer once it reuses old ones.
    assert!(iqn.iters[0] <= 4, "IQN-ILS {:?}", iqn.iters);
    assert!(
        iqn.iters[1..].iter().all(|&k| k <= 3),
        "IQN-ILS {:?}",
        iqn.iters
    );
    let total = |r: &Run| r.iters.iter().sum::<u32>();
    assert!(
        total(&iqn) < total(&aitken),
        "IQN-ILS {:?} vs Aitken {:?}",
        iqn.iters,
        aitken.iters
    );
}
