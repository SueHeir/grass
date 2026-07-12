//! Four parent-side coupling schedules for the unchanged oscillator library.
//!
//! The executable deliberately makes no claim that a converged Picard solve
//! removes the time-discretisation error of the library's semi-implicit Euler
//! step.  It measures every policy against the analytic coupled normal mode.

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
    adaptive_initial_dt: f64,
    minimum_dt: f64,
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
    for name in [A, B] {
        parent.add_subapp_with_config(name, |app| {
            app.add_plugins(OscillatorPlugin);
        });
    }
    (parent, case)
}
fn get<T: Copy + 'static>(subs: &SubApps, name: &str) -> T {
    let participant = subs.find(name).unwrap();
    let value = *participant
        .resource_cell(TypeId::of::<T>())
        .unwrap()
        .borrow()
        .downcast_ref::<T>()
        .unwrap();
    value
}
fn put<T: Copy + 'static>(subs: &SubApps, name: &str, value: T) {
    let participant = subs.find(name).unwrap();
    *participant
        .resource_cell(TypeId::of::<T>())
        .unwrap()
        .borrow_mut()
        .downcast_mut::<T>()
        .unwrap() = value;
}
fn pair(subs: &SubApps) -> Pair {
    Pair {
        a: get(subs, A),
        b: get(subs, B),
    }
}
fn restore(subs: &SubApps, p: Pair) {
    put(subs, A, p.a);
    put(subs, B, p.b);
}
fn set_dt(subs: &SubApps, h: f64) {
    for name in [A, B] {
        let mut p: OscillatorParameters = get(subs, name);
        p.dt = h;
        put(subs, name, p);
    }
}
fn energy(p: Pair) -> f64 {
    // Parameters are fixed in the declarative case below: m=1, k=1, kc=2000.
    0.5 * (p.a.v.powi(2) + p.b.v.powi(2))
        + 0.5 * (p.a.x.powi(2) + p.b.x.powi(2))
        + 1000.0 * (p.a.x - p.b.x).powi(2)
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

fn publish(subs: &SubApps, target: &str, source: OscillatorState) {
    let port = PositionPort::<()>::from_state(&source);
    put(subs, target, PeerPosition(port.position.0));
}

// Conventional serial CSS: B consumes A's newly exported value, while A has
// consumed B's value from the beginning of the macro window.  The asymmetry is
// intentional: it is the exchange/schedule policy being compared.
fn css_step(subs: &mut SubApps, old: Pair) {
    publish(subs, A, old.b);
    subs.tick(A);
    publish(subs, B, pair(subs).a);
    subs.tick(B);
}

// One Jacobi fixed-point sweep.  Every sweep starts from the saved state, so
// only the interface guess changes; no provisional solver state accumulates.
fn picard_sweep(subs: &mut SubApps, start: Pair, guess: Pair) -> Pair {
    restore(subs, start);
    publish(subs, A, guess.b);
    publish(subs, B, guess.a);
    subs.tick(A);
    subs.tick(B);
    pair(subs)
}

fn solve_picard(
    subs: &mut SubApps,
    start: Pair,
    c: Case,
    relaxed: bool,
) -> Result<(Pair, usize, f64), f64> {
    let mut guess = start;
    let mut last_residual = f64::INFINITY;
    for iteration in 1..=c.picard_max_iterations {
        let candidate = picard_sweep(subs, start, guess);
        last_residual = residual(candidate, guess);
        if last_residual < c.picard_tolerance {
            return Ok((candidate, iteration, last_residual));
        }
        if relaxed {
            // Fixed under-relaxation (0 < omega < 1), not the case-specific
            // cancellation value.  It damps the alternating Picard mode.
            guess.a.x += c.relaxation * (candidate.a.x - guess.a.x);
            guess.b.x += c.relaxation * (candidate.b.x - guess.b.x);
        } else {
            guess = candidate;
        }
    }
    restore(subs, start);
    Err(last_residual)
}

fn report(
    name: &'static str,
    state: Pair,
    max_residual: f64,
    max_iterations: usize,
    rejected: usize,
    min_accepted_dt: f64,
    e0: f64,
) -> Report {
    Report {
        name,
        state,
        max_residual,
        max_iterations,
        rejected,
        min_accepted_dt,
        energy_ratio: energy(state) / e0,
    }
}

// Machine-readable scientific trace consumed by the example sweep.  Keeping
// it here makes the reported histories come from the same schedule execution
// as the summary, rather than from a reconstructed implementation.
fn trace(name: &str, step: usize, dt: f64, solve: (usize, f64, bool), p: Pair, e0: f64) {
    let (iterations, residual, accepted) = solve;
    println!(
        "COUPLING_TRACE policy={name} step={step} dt={dt:.17e} iterations={iterations} residual={residual:.17e} accepted={accepted} energy_ratio={:.17e} state={:.17e},{:.17e},{:.17e},{:.17e}",
        energy(p) / e0, p.a.x, p.a.v, p.b.x, p.b.v
    );
}

fn explicit() -> Report {
    let (mut app, c) = build();
    let outer = app.get_mut_resource(TypeId::of::<SubApps>()).unwrap();
    let mut outer = outer.borrow_mut();
    let subs = outer.downcast_mut::<SubApps>().unwrap();
    set_dt(subs, c.nominal_dt);
    let e0 = energy(pair(subs));
    let mut max_r: f64 = 0.0;
    for step in 1..=(c.final_time / c.nominal_dt).round() as usize {
        let old = pair(subs);
        css_step(subs, old);
        let r = residual(pair(subs), old);
        max_r = max_r.max(r);
        trace(
            "explicit_CSS",
            step,
            c.nominal_dt,
            (1, r, true),
            pair(subs),
            e0,
        );
    }
    report("explicit CSS", pair(subs), max_r, 1, 0, c.nominal_dt, e0)
}
fn implicit(relaxed: bool) -> Report {
    let (mut app, c) = build();
    let outer = app.get_mut_resource(TypeId::of::<SubApps>()).unwrap();
    let mut outer = outer.borrow_mut();
    let subs = outer.downcast_mut::<SubApps>().unwrap();
    set_dt(subs, c.nominal_dt);
    let e0 = energy(pair(subs));
    let mut max_r: f64 = 0.0;
    let mut max_i = 0;
    for step in 1..=(c.final_time / c.nominal_dt).round() as usize {
        let start = pair(subs);
        let (accepted, i, r) =
            solve_picard(subs, start, c, relaxed).expect("nominal Picard step must converge");
        restore(subs, accepted);
        max_r = max_r.max(r);
        max_i = max_i.max(i);
        trace(
            if relaxed {
                "relaxed_Picard"
            } else {
                "implicit_Picard"
            },
            step,
            c.nominal_dt,
            (i, r, true),
            pair(subs),
            e0,
        );
    }
    report(
        if relaxed {
            "relaxed Picard"
        } else {
            "implicit Picard"
        },
        pair(subs),
        max_r,
        max_i,
        0,
        c.nominal_dt,
        e0,
    )
}
fn adaptive() -> Report {
    let (mut app, c) = build();
    let outer = app.get_mut_resource(TypeId::of::<SubApps>()).unwrap();
    let mut outer = outer.borrow_mut();
    let subs = outer.downcast_mut::<SubApps>().unwrap();
    let e0 = energy(pair(subs));
    let mut t: f64 = 0.0;
    let mut h = c.adaptive_initial_dt;
    let mut rejects = 0;
    let mut min_h = f64::INFINITY;
    let mut max_r: f64 = 0.0;
    let mut max_i = 0;
    let mut step = 0;
    while t < c.final_time - 1e-12 {
        h = h.min(c.final_time - t);
        set_dt(subs, h);
        let start = pair(subs);
        match solve_picard(subs, start, c, false) {
            Ok((accepted, i, r)) => {
                restore(subs, accepted);
                t += h;
                step += 1;
                min_h = min_h.min(h);
                max_r = max_r.max(r);
                max_i = max_i.max(i);
                trace("adaptive_retry", step, h, (i, r, true), pair(subs), e0);
                h = c.nominal_dt;
            }
            Err(r) => {
                max_r = max_r.max(r);
                rejects += 1;
                // A rejected attempt has no trajectory state, but its
                // residual, iteration cap, and proposed dt are evidence for
                // the adaptive decision.
                trace(
                    "adaptive_retry",
                    step + 1,
                    h,
                    (c.picard_max_iterations, r, false),
                    start,
                    e0,
                );
                h *= 0.5;
                assert!(
                    h >= c.minimum_dt - 1e-12,
                    "adaptive retry exhausted minimum dt"
                );
            }
        }
    }
    report(
        "adaptive retry",
        pair(subs),
        max_r,
        max_i,
        rejects,
        min_h,
        e0,
    )
}

// Independent physical reference: the antisymmetric initial state has the
// exact normal mode omega=sqrt((k+2kc)/m)=sqrt(4001).
fn exact(t: f64) -> Pair {
    let omega = 4001_f64.sqrt();
    let x = (omega * t).cos();
    let v = -omega * (omega * t).sin();
    Pair {
        a: OscillatorState { x, v },
        b: OscillatorState { x: -x, v: -v },
    }
}
fn relative_error(p: Pair, reference: Pair) -> f64 {
    let numerator = (p.a.x - reference.a.x).powi(2)
        + (p.a.v - reference.a.v).powi(2)
        + (p.b.x - reference.b.x).powi(2)
        + (p.b.v - reference.b.v).powi(2);
    let denominator = reference.a.x.powi(2)
        + reference.a.v.powi(2)
        + reference.b.x.powi(2)
        + reference.b.v.powi(2);
    (numerator / denominator).sqrt()
}
fn main() {
    let (_, c) = build();
    let reference = exact(c.final_time);
    let reports = [explicit(), implicit(false), implicit(true), adaptive()];
    for r in &reports {
        println!("{} residual={:.3e} iterations={} rejected={} accepted_dt={:.5} energy_ratio={:.6} relative_error={:.9} state={:.17e},{:.17e},{:.17e},{:.17e} fingerprint={:016x?}", r.name, r.max_residual, r.max_iterations, r.rejected, r.min_accepted_dt, r.energy_ratio, relative_error(r.state, reference), r.state.a.x, r.state.a.v, r.state.b.x, r.state.b.v, fingerprint(r.state));
    }
    let css = &reports[0];
    let picard = &reports[1];
    let relaxed = &reports[2];
    let adaptive = &reports[3];
    assert!(
        picard.max_iterations >= 2,
        "case must expose Picard iteration"
    );
    assert!(
        relaxed.max_iterations < picard.max_iterations,
        "fixed relaxation must reduce iteration count"
    );
    assert!(
        adaptive.rejected > 0,
        "adaptive residual rejection branch did not fire"
    );
    assert!(
        relative_error(picard.state, reference) < 0.25,
        "Picard misses the external 25% phase-space accuracy budget"
    );
    assert!(
        relative_error(relaxed.state, reference) < 0.25,
        "relaxed Picard misses the external 25% phase-space accuracy budget"
    );
    assert!(
        relative_error(adaptive.state, reference) < 0.25,
        "adaptive Picard misses the external 25% phase-space accuracy budget"
    );
    assert!(
        relative_error(css.state, reference) > 0.5,
        "CSS lag is not exposed by this documented strong-coupling case"
    );
    println!("ALL CHECKS PASSED");
}
