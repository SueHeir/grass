//! Shared physics for the worked examples in this workspace.
//!
//! The scenario: two damped harmonic oscillators coupled by an interface
//! spring of stiffness `κ`. Each oscillator is its own grass `App`
//! (sub-App); they exchange position via a coupling layer the example
//! drives.
//!
//! Per oscillator (one App owns one set of these resources):
//!
//! ```text
//! ẋ = v
//! v̇ = (-k_self·x  -  γ·v  -  κ·(x - x_other_seen)) / mass
//! ```
//!
//! `OtherX` is what THIS sub-App currently believes the OTHER sub-App's
//! position to be. The job of the coupling layer (a parent-side
//! [`exchange_positions`] system, a `Schedule::Loop` body, …) is to keep
//! the two sides' `OtherX` consistent with the actual `OscState.x` of
//! the peer.
//!
//! We use **semi-implicit Euler** for the per-substep integration so the
//! integrator itself is well-behaved; what makes explicit/CSS coupling
//! visibly lag at high `κ` is the **stale `OtherX`** read across the
//! coupling boundary, not the integrator. That separation is what makes
//! the implicit example a meaningful contrast with the explicit one.
//!
//! ## Configuring
//!
//! [`OscillatorPlugin`] reads its parameters from the
//! [`Config`] resource's `[oscillator]` section:
//!
//! ```toml
//! [oscillator]
//! x0 = 1.0
//! v0 = 0.0
//! other_x0 = 0.0
//! k_self = 1.0
//! gamma = 0.05
//! k_couple = 0.0
//! mass = 1.0
//! dt = 5.0e-3
//! ```
//!
//! Coupled examples register two oscillators on a parent `App`, slicing
//! the main TOML with [`Config::for_subapp`](grass_io::Config::for_subapp)
//! so each sub-App's local `Config` contains a single `[oscillator]`
//! section drawn from main's `[a.oscillator]` / `[b.oscillator]`.

use grass_app::prelude::*;
use grass_io::Config;
use grass_multi::{namespace, MultiRes, MultiResMut, Wire};
use grass_scheduler::prelude::*;
use serde::Deserialize;

// Re-exported so example mains can pull `OuterIterStopPlugin` from the
// same import line as the rest of the demo — the canonical home is
// `grass_multi`.
pub use grass_multi::OuterIterStopPlugin;

// ─── Compile-time namespace markers for the canonical two-oscillator scenario

namespace!(pub A = "a");
namespace!(pub B = "b");

// ─── The coupling system, reused across every coupled example ─────────────

/// Read each sub-App's `OscState.x`; write it into the OTHER sub-App's
/// `OtherX`. The signature reads as the API contract — A's state in,
/// B's state in, A's view of B out, B's view of A out.
///
/// Used unchanged across `examples/coupling/{explicit, implicit, adaptive,
/// explicit_mpi/{a,b}}` — only the *schedule* around it differs.
pub fn exchange_positions(
    a_state: MultiRes<OscState, A>,
    b_state: MultiRes<OscState, B>,
    mut a_other: MultiResMut<OtherX, A>,
    mut b_other: MultiResMut<OtherX, B>,
) {
    a_other.0 = b_state.x;
    b_other.0 = a_state.x;
}

// ─── Resources ──────────────────────────────────────────────────────────────

/// Position + velocity of one oscillator. Lives on the sub-App.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OscState {
    pub x: f64,
    pub v: f64,
}

/// What this sub-App believes the OTHER sub-App's position is right now.
/// The coupling layer keeps it fresh; the integrator reads it as a
/// boundary condition.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OtherX(pub f64);

// ─── Wire impls (used by the MPI two-binary example) ──────────────────────

impl Wire for OscState {
    fn pack(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        out.extend_from_slice(&self.x.to_le_bytes());
        out.extend_from_slice(&self.v.to_le_bytes());
        out
    }
    fn unpack(buf: &[u8]) -> Self {
        let mut a = [0u8; 8];
        a.copy_from_slice(&buf[..8]);
        let x = f64::from_le_bytes(a);
        a.copy_from_slice(&buf[8..16]);
        let v = f64::from_le_bytes(a);
        OscState { x, v }
    }
}

impl Wire for OtherX {
    fn pack(&self) -> Vec<u8> {
        self.0.to_le_bytes().to_vec()
    }
    fn unpack(buf: &[u8]) -> Self {
        let mut a = [0u8; 8];
        a.copy_from_slice(&buf[..8]);
        OtherX(f64::from_le_bytes(a))
    }
}

/// Per-oscillator material parameters. Constant across the run.
#[derive(Debug, Clone, Copy, Default)]
pub struct OscParams {
    /// Self-restoring spring stiffness.
    pub k_self: f64,
    /// Damping coefficient.
    pub gamma: f64,
    /// Coupling-spring stiffness — the wedge between explicit and implicit.
    pub k_couple: f64,
    /// Inertial mass.
    pub mass: f64,
    /// Default substep size (seconds). Adaptive examples mutate
    /// [`StepSize::dt`] on the sub-App at runtime.
    pub dt: f64,
}

/// Per-App mutable dt. Adaptive examples adjust this; fixed-dt examples
/// leave it at [`OscParams::dt`].
#[derive(Debug, Clone, Copy, Default)]
pub struct StepSize {
    pub dt: f64,
}

// ─── Sub-App phase ──────────────────────────────────────────────────────────

/// Single phase enum for the oscillator's step. The parent App's
/// `tick_subapp` doesn't care about this — it just calls `step()` on the
/// sub-App. But it has to live somewhere reachable so the system can be
/// registered.
#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum OscSchedule {
    /// Velocity update, then position update (semi-implicit Euler).
    Step,
}

// ─── Integration system (one step of semi-implicit Euler) ──────────────────

pub fn osc_step(
    mut state: ResMut<OscState>,
    other: Res<OtherX>,
    params: Res<OscParams>,
    step: Res<StepSize>,
) {
    let dt = step.dt;
    let force =
        -params.k_self * state.x - params.gamma * state.v - params.k_couple * (state.x - other.0);
    let a = force / params.mass;
    // semi-implicit Euler: update v first (using current x), then x using new v.
    state.v += a * dt;
    let new_v = state.v;
    state.x += new_v * dt;
}

// ─── TOML config + Plugin ──────────────────────────────────────────────────

/// `[oscillator]` section. Every field has a default so missing sections
/// produce an inert oscillator (zero state, zero stiffness, zero dt).
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OscillatorConfig {
    /// Initial position.
    pub x0: f64,
    /// Initial velocity.
    pub v0: f64,
    /// Initial value for [`OtherX`] — what this oscillator initially
    /// believes the peer's position to be. Irrelevant when `k_couple = 0`.
    pub other_x0: f64,
    /// Self-restoring spring stiffness.
    pub k_self: f64,
    /// Damping coefficient.
    pub gamma: f64,
    /// Coupling spring stiffness. 0 disables coupling.
    pub k_couple: f64,
    /// Inertial mass.
    pub mass: f64,
    /// Substep size.
    pub dt: f64,
}

/// One oscillator's worth of resources + the integrator system.
///
/// Reads `[oscillator]` from the App's [`Config`] resource at `build`
/// time and seeds [`OscState`] / [`OtherX`] / [`OscParams`] / [`StepSize`].
/// Pre-seed `Config` (via [`InputPlugin`](grass_io::InputPlugin) on the
/// main App, or `app.add_resource(Config::for_subapp(...))` on a sub-App)
/// before adding this plugin.
pub struct OscillatorPlugin;

impl Plugin for OscillatorPlugin {
    fn build(&self, app: &mut App) {
        let cfg = Config::load::<OscillatorConfig>(app, "oscillator");
        app.add_resource(OscState {
            x: cfg.x0,
            v: cfg.v0,
        });
        app.add_resource(OtherX(cfg.other_x0));
        app.add_resource(OscParams {
            k_self: cfg.k_self,
            gamma: cfg.gamma,
            k_couple: cfg.k_couple,
            mass: cfg.mass,
            dt: cfg.dt,
        });
        app.add_resource(StepSize { dt: cfg.dt });
        app.add_update_system(osc_step, OscSchedule::Step);
    }

    fn default_config(&self) -> Option<&str> {
        Some(
            r#"# [oscillator] — one damped harmonic oscillator (with optional coupling).
[oscillator]
# Initial state.
x0 = 1.0
v0 = 0.0
# Coupling boundary: this oscillator's view of the OTHER oscillator's
# position. Irrelevant when k_couple = 0. Coupling layers (e.g.
# `exchange_positions`) keep this in sync with the peer's actual x.
other_x0 = 0.0
# Self-restoring spring stiffness.
k_self = 1.0
# Damping coefficient.
gamma = 0.05
# Coupling spring stiffness (peer's x is read via OtherX). 0 disables.
k_couple = 0.0
# Mass.
mass = 1.0
# Substep size (seconds).
dt = 5.0e-3
"#,
        )
    }
}

// ─── Final-state helpers (used by every coupling example's main) ──────────

/// Final `(x_a, v_a, x_b, v_b)` after the run completes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FinalState {
    pub x_a: f64,
    pub v_a: f64,
    pub x_b: f64,
    pub v_b: f64,
}

impl FinalState {
    pub fn print_pretty(&self, label: &str) {
        println!(
            "{label}:  x_a = {:>+12.9}   v_a = {:>+12.9}   x_b = {:>+12.9}   v_b = {:>+12.9}",
            self.x_a, self.v_a, self.x_b, self.v_b
        );
    }

    /// Print as `RESULT_BITS x_a=0xHEX v_a=0xHEX x_b=0xHEX v_b=0xHEX` so
    /// trajectories can be regression-checked bit-exactly across edits.
    pub fn print_exact(&self, label: &str) {
        println!(
            "RESULT_BITS {label} x_a={:#018x} v_a={:#018x} x_b={:#018x} v_b={:#018x}",
            self.x_a.to_bits(),
            self.v_a.to_bits(),
            self.x_b.to_bits(),
            self.v_b.to_bits(),
        );
    }

    /// Convenience: print both human-readable + bit-exact lines.
    pub fn print(&self, label: &str) {
        self.print_pretty(label);
        self.print_exact(label);
    }
}

/// Pull `FinalState` out of a parent App that has two sub-Apps named
/// `"a"` and `"b"`, each holding an [`OscState`] resource.
pub fn extract_final_state(parent: &App) -> FinalState {
    use grass_multi::SubApps;
    let subs = parent
        .get_resource_ref::<SubApps>()
        .expect("SubApps resource present after start()");
    let read = |ns: &str| {
        let cell = subs
            .find(ns)
            .unwrap_or_else(|| panic!("sub-App `{ns}` not registered"))
            .resource_cell(std::any::TypeId::of::<OscState>())
            .expect("OscState present on the sub-App")
            .borrow();
        *cell.downcast_ref::<OscState>().expect("OscState type")
    };
    let a = read("a");
    let b = read("b");
    FinalState {
        x_a: a.x,
        v_a: a.v,
        x_b: b.x,
        v_b: b.v,
    }
}
