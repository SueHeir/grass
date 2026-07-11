//! Same oscillator component and `PositionPort` exchange value, four parent
//! orchestration policies.  Nothing in `oscillator_demo` is modified here.

use std::any::TypeId;

use grass_app::prelude::*;
use grass_io::{Config, MultiIoExt};
use grass_multi::SubApps;
use oscillator_demo::{
    OscillatorParameters, OscillatorPlugin, OscillatorState, PeerPosition, PositionPort,
};
use serde::Deserialize;

const A: &str = "a";
const B: &str = "b";

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct Case {
    final_time: f64,
    nominal_dt: f64,
    minimum_dt: f64,
    reference_dt: f64,
    picard_tolerance: f64,
    picard_max_iterations: usize,
    relaxation: f64,
}
#[derive(Clone, Copy, Debug)]
struct Pair {
    a: OscillatorState,
    b: OscillatorState,
}
#[derive(Clone, Debug)]
struct Report {
    name: &'static str,
    state: Pair,
    max_residual: f64,
    max_iterations: usize,
    rejected: usize,
    min_accepted_dt: f64,
    energy_ratio: f64,
}

fn build() -> (App, Case) {
    let mut parent = App::new();
    parent.add_resource(Config::from_str(include_str!("config.toml")));
    let case = Config::load::<Case>(&mut parent, "case");
    parent.add_subapp_with_config(A, |app| {
        app.add_plugins(OscillatorPlugin);
    });
    parent.add_subapp_with_config(B, |app| {
        app.add_plugins(OscillatorPlugin);
    });
    (parent, case)
}
fn cell<'a, T: 'static>(
    subs: &'a SubApps,
    name: &str,
) -> &'a std::cell::RefCell<Box<dyn std::any::Any>> {
    subs.find(name)
        .unwrap()
        .resource_cell(TypeId::of::<T>())
        .unwrap()
}
fn pair(subs: &SubApps) -> Pair {
    let a = *cell::<OscillatorState>(subs, A)
        .borrow()
        .downcast_ref::<OscillatorState>()
        .unwrap();
    let b = *cell::<OscillatorState>(subs, B)
        .borrow()
        .downcast_ref::<OscillatorState>()
        .unwrap();
    Pair { a, b }
}
fn put<T: Copy + 'static>(subs: &SubApps, name: &str, value: T) {
    *cell::<T>(subs, name)
        .borrow_mut()
        .downcast_mut::<T>()
        .unwrap() = value;
}
fn restore(subs: &SubApps, p: Pair) {
    put(subs, A, p.a);
    put(subs, B, p.b);
}
fn set_dt(subs: &SubApps, dt: f64) {
    let mut a = *cell::<OscillatorParameters>(subs, A)
        .borrow()
        .downcast_ref::<OscillatorParameters>()
        .unwrap();
    a.dt = dt;
    put(subs, A, a);
    let mut b = *cell::<OscillatorParameters>(subs, B)
        .borrow()
        .downcast_ref::<OscillatorParameters>()
        .unwrap();
    b.dt = dt;
    put(subs, B, b);
}
fn energy(p: Pair, k: f64, kc: f64) -> f64 {
    0.5 * (p.a.v * p.a.v + p.b.v * p.b.v)
        + 0.5 * k * (p.a.x * p.a.x + p.b.x * p.b.x)
        + 0.5 * kc * (p.a.x - p.b.x).powi(2)
}

// The port payload is the only cross-solver value.  The parent owns its
// directional binding; neither oscillator sees the other's state type.
fn exchange_then_tick(subs: &mut SubApps, guess: Pair) {
    let to_a = PositionPort::<()>::from_state(&guess.b);
    let to_b = PositionPort::<()>::from_state(&guess.a);
    put(subs, A, PeerPosition(to_a.position.0));
    put(subs, B, PeerPosition(to_b.position.0));
    subs.tick(A);
    subs.tick(B);
}
fn residual(p: Pair, guess: Pair) -> f64 {
    (p.a.x - guess.a.x).abs().max((p.b.x - guess.b.x).abs())
}
fn fingerprint(p: Pair) -> [u64; 4] {
    [
        p.a.x.to_bits(),
        p.a.v.to_bits(),
        p.b.x.to_bits(),
        p.b.v.to_bits(),
    ]
}

fn explicit() -> Report {
    let (mut app, c) = build();
    let e0;
    let mut r: f64 = 0.;
    {
        let mut outer = app
            .get_mut_resource(TypeId::of::<SubApps>())
            .unwrap()
            .borrow_mut();
        let subs = outer.downcast_mut::<SubApps>().unwrap();
        set_dt(subs, c.nominal_dt);
        e0 = energy(pair(subs), 1., 600.);
        for _ in 0..(c.final_time / c.nominal_dt).round() as usize {
            let old = pair(subs);
            exchange_then_tick(subs, old);
            r = r.max(residual(pair(subs), old));
        }
        let p = pair(subs);
        return Report {
            name: "explicit/CSS",
            state: p,
            max_residual: r,
            max_iterations: 1,
            rejected: 0,
            min_accepted_dt: c.nominal_dt,
            energy_ratio: energy(p, 1., 600.) / e0,
        };
    }
}
fn implicit(relaxed: bool) -> Report {
    let (mut app, c) = build();
    let e0;
    let mut max_r: f64 = 0.;
    let mut max_i = 0;
    {
        let mut outer = app
            .get_mut_resource(TypeId::of::<SubApps>())
            .unwrap()
            .borrow_mut();
        let subs = outer.downcast_mut::<SubApps>().unwrap();
        set_dt(subs, c.nominal_dt);
        e0 = energy(pair(subs), 1., 600.);
        for _ in 0..(c.final_time / c.nominal_dt).round() as usize {
            let start = pair(subs);
            let mut guess = start;
            let mut last = start;
            for i in 1..=c.picard_max_iterations {
                restore(subs, start);
                exchange_then_tick(subs, guess);
                last = pair(subs);
                let raw = residual(last, guess);
                max_r = max_r.max(raw);
                max_i = max_i.max(i);
                if raw < c.picard_tolerance {
                    break;
                }
                if relaxed {
                    guess.a.x += c.relaxation * (last.a.x - guess.a.x);
                    guess.b.x += c.relaxation * (last.b.x - guess.b.x);
                } else {
                    guess = last;
                }
            }
            restore(subs, last);
        }
        let p = pair(subs);
        return Report {
            name: if relaxed {
                "relaxed implicit"
            } else {
                "implicit Picard"
            },
            state: p,
            max_residual: max_r,
            max_iterations: max_i,
            rejected: 0,
            min_accepted_dt: c.nominal_dt,
            energy_ratio: energy(p, 1., 600.) / e0,
        };
    }
}
fn adaptive() -> Report {
    let (mut app, c) = build();
    let e0;
    let mut t: f64 = 0.;
    let mut h = c.nominal_dt;
    let mut rejects = 0;
    let mut min_h = h;
    let mut max_r: f64 = 0.;
    let mut max_i = 0;
    {
        let mut outer = app
            .get_mut_resource(TypeId::of::<SubApps>())
            .unwrap()
            .borrow_mut();
        let subs = outer.downcast_mut::<SubApps>().unwrap();
        e0 = energy(pair(subs), 1., 600.);
        while t < c.final_time - 1e-12 {
            h = h.min(c.final_time - t);
            let start = pair(subs);
            let mut guess = start;
            let mut candidate = start;
            let mut ok = false;
            set_dt(subs, h);
            for i in 1..=c.picard_max_iterations {
                restore(subs, start);
                exchange_then_tick(subs, guess);
                candidate = pair(subs);
                let raw = residual(candidate, guess);
                max_r = max_r.max(raw);
                max_i = max_i.max(i);
                if raw < c.picard_tolerance {
                    ok = true;
                    break;
                }
                guess = candidate;
            }
            if ok {
                restore(subs, candidate);
                t += h;
                min_h = min_h.min(h);
                h = (2. * h).min(c.nominal_dt);
            } else {
                restore(subs, start);
                h *= 0.5;
                rejects += 1;
                assert!(
                    h >= c.minimum_dt - 1e-12,
                    "adaptive retry reached configured minimum dt"
                );
            }
        }
        let p = pair(subs);
        return Report {
            name: "adaptive retry",
            state: p,
            max_residual: max_r,
            max_iterations: max_i,
            rejected: rejects,
            min_accepted_dt: min_h,
            energy_ratio: energy(p, 1., 600.) / e0,
        };
    }
}
fn reference(h: f64, final_time: f64) -> Pair {
    let mut p = Pair {
        a: OscillatorState { x: 1., v: 0. },
        b: OscillatorState { x: -1., v: 0. },
    };
    for _ in 0..(final_time / h).round() as usize {
        // Simultaneous solve of the fixed point used by the rollback policies:
        // x_a' - h² k_c x_b' = rhs_a, and its symmetric companion.
        let old = p;
        let q = h * h * 600.;
        let ra = old.a.x + h * old.a.v - h * h * 601. * old.a.x;
        let rb = old.b.x + h * old.b.v - h * h * 601. * old.b.x;
        p.a.x = (ra + q * rb) / (1. - q * q);
        p.b.x = (rb + q * ra) / (1. - q * q);
        p.a.v = (p.a.x - old.a.x) / h;
        p.b.v = (p.b.x - old.b.x) / h;
    }
    p
}
fn err(a: Pair, b: Pair) -> f64 {
    (a.a.x - b.a.x)
        .abs()
        .max((a.a.v - b.a.v).abs())
        .max((a.b.x - b.b.x).abs())
        .max((a.b.v - b.b.v).abs())
}
fn main() {
    let (_, c) = build();
    let reference_state = reference(c.reference_dt, c.final_time);
    let reports = [explicit(), implicit(false), implicit(true), adaptive()];
    let refined = reference(c.reference_dt / 2., c.final_time);
    let ref_error = err(reference_state, refined);
    println!("reference refinement error={ref_error:.3e}");
    assert!(ref_error < 2e-2, "reference step is not converged");
    for r in &reports {
        println!("{} residual={:.3e} iterations={} rejected={} accepted_dt={:.5} energy_ratio={:.6} fingerprint={:016x?} error_to_reference={:.3e}",r.name,r.max_residual,r.max_iterations,r.rejected,r.min_accepted_dt,r.energy_ratio,fingerprint(r.state),err(r.state,reference_state));
    }
    let pic = &reports[1];
    let relax = &reports[2];
    let adapt = &reports[3];
    assert!(
        pic.max_iterations > 5,
        "strong case must expose Picard iteration"
    );
    assert!(
        relax.max_iterations < pic.max_iterations,
        "relaxation should reduce iteration count"
    );
    assert!(adapt.rejected > 0, "adaptive retry branch did not fire");
    // These policies converge to the simultaneous fixed-point solve at their
    // accepted step, while the refined run quantifies ordinary time error.
    assert!(err(relax.state, reference(c.nominal_dt, c.final_time)) < 1e-8);
    assert!(err(adapt.state, reference(adapt.min_accepted_dt, c.final_time)) < 1e-8);
    println!("ALL CHECKS PASSED");
}
