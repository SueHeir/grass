//! End-to-end velocity-Verlet mini-solver driven entirely by `grass_scheduler`.
//!
//! Demonstrates the full small-scale lifecycle:
//!   - one resource (`Body`, a 1-D particle in a spring potential),
//!   - three ordered phases (`Kick → Drift → Kick` half-steps + a stop check),
//!   - `organize_systems()` to compute execution order,
//!   - a finite `run()` loop,
//!   - a final-state assertion.
//!
//! Run with: `cargo run -p grass_scheduler --example verlet_minisolver`

use grass_scheduler::prelude::*;

/// 1-D particle: position `x`, velocity `v`, acted on by spring force `-k*x`.
struct Body {
    x: f64,
    v: f64,
    k: f64,
    dt: f64,
    /// Acceleration carried between the two half-kicks of one step.
    a: f64,
}

/// Phases of one velocity-Verlet step. Declaration order = schedule index.
#[derive(Debug, Clone, Copy)]
enum Step {
    KickA,  // v += a*dt/2 using the *old* acceleration
    Drift,  // x += v*dt, then recompute acceleration for the new x
    KickB,  // v += a*dt/2 using the *new* acceleration
}

impl ScheduleSet for Step {
    fn to_index(&self) -> u32 {
        match self {
            Step::KickA => 0,
            Step::Drift => 1,
            Step::KickB => 2,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Step::KickA => "KickA",
            Step::Drift => "Drift",
            Step::KickB => "KickB",
        }
    }
}

fn kick_a(mut b: ResMut<Body>) {
    let half = 0.5 * b.dt;
    b.v += b.a * half;
}

fn drift(mut b: ResMut<Body>) {
    let dt = b.dt;
    b.x += b.v * dt;
    // Spring acceleration at the new position (unit mass).
    b.a = -b.k * b.x;
}

fn kick_b(mut b: ResMut<Body>) {
    let half = 0.5 * b.dt;
    b.v += b.a * half;
}

fn main() {
    let mut scheduler = Scheduler::default();

    let k = 1.0;
    let x0 = 1.0;
    scheduler.add_resource(Body {
        x: x0,
        v: 0.0,
        k,
        dt: 0.01,
        a: -k * x0, // initial acceleration
    });

    scheduler.add_update_system(kick_a, Step::KickA);
    scheduler.add_update_system(drift, Step::Drift);
    scheduler.add_update_system(kick_b, Step::KickB);

    // Compute execution order once, then drive a finite loop ourselves.
    scheduler.organize_systems();

    // One full period of the unit-mass spring is 2*pi; step until t ~= 2*pi.
    let steps = (2.0 * std::f64::consts::PI / 0.01).round() as usize;
    for _ in 0..steps {
        scheduler.run();
    }

    let body = scheduler.get_resource_ref::<Body>().expect("Body resource");
    println!("after {steps} steps: x = {:.4}, v = {:.4}", body.x, body.v);

    // After one full period the oscillator should return near its start.
    assert!((body.x - x0).abs() < 1e-2, "x did not return to start: {}", body.x);
    assert!(body.v.abs() < 1e-2, "v did not return to zero: {}", body.v);

    // Energy (0.5 v^2 + 0.5 k x^2) should be conserved by symplectic Verlet.
    let energy = 0.5 * body.v * body.v + 0.5 * k * body.x * body.x;
    let energy0 = 0.5 * k * x0 * x0;
    assert!((energy - energy0).abs() < 1e-3, "energy drift: {energy} vs {energy0}");

    println!("verlet_minisolver: final state asserts passed");
}
