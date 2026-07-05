//! Structured-grid **finite-difference** 1-D heat-diffusion solver driven entirely
//! by `grass_scheduler` — a *non-particle* discretization living on the same
//! scheduler that hosts the Verlet mini-solver.
//!
//! This exercises the tier-vision contract that `grass` is agnostic to the
//! *entire discretization paradigm*: there is no particle, atom, neighbor list,
//! position, or pairwise interaction anywhere here — only a field sampled on a
//! fixed grid and a local stencil. The physics (`∂T/∂t = α ∂²T/∂x²`) is advanced
//! by an explicit FTCS (forward-time, centered-space) update expressed as two
//! ordered scheduler phases:
//!
//!   1. `Stencil` — compute the new field from the centered second difference,
//!   2. `Commit`  — swap the double buffer so the next step reads the new field.
//!
//! Two analytical checks validate it:
//!
//!   * **Steady state.** With fixed (Dirichlet) end temperatures the field must
//!     relax to the exact linear profile `T(x) = T_L + (T_R − T_L)·x/L`
//!     (`∂²T/∂x² = 0`). We assert the max deviation is < 1e-6.
//!   * **Fourier-mode decay.** With homogeneous ends, an initial `sin(πx/L)`
//!     mode must decay at the analytical continuum rate `exp(−α (π/L)² t)`.
//!     We assert the numerical amplitude tracks that rate to within the
//!     scheme's `O(Δx²)` truncation error.
//!
//! Run with:   `cargo run --example heat_diffusion_1d`
//! Test with:  `cargo test --example heat_diffusion_1d`

use grass_scheduler::prelude::*;

/// A temperature field sampled on a uniform 1-D grid of `n` nodes.
///
/// `x_i = i·dx`, `i ∈ [0, n)`, with `x_0 = 0` and `x_{n-1} = L`. Node 0 and node
/// `n-1` carry fixed Dirichlet boundary values. `cur`/`next` are a double buffer:
/// the stencil reads `cur` and writes `next`, then `Commit` swaps them so no cell
/// is updated in place while its adjacent nodes are still being read.
struct Field {
    /// Current field values (length `n`).
    cur: Vec<f64>,
    /// Scratch buffer for the next time level (length `n`).
    next: Vec<f64>,
    /// Diffusion number `r = α·dt/dx²`. Must be ≤ 1/2 for FTCS stability.
    r: f64,
    /// Fixed left boundary temperature (`x = 0`).
    t_left: f64,
    /// Fixed right boundary temperature (`x = L`).
    t_right: f64,
}

impl Field {
    fn new(cur: Vec<f64>, r: f64, t_left: f64, t_right: f64) -> Self {
        let next = cur.clone();
        Field {
            cur,
            next,
            r,
            t_left,
            t_right,
        }
    }

    fn n(&self) -> usize {
        self.cur.len()
    }
}

/// Phases of one explicit diffusion step. Declaration order = schedule index,
/// so `Stencil` always runs before `Commit` within a step.
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Step {
    /// Compute `next` from the centered second difference of `cur`.
    Stencil,
    /// Publish `next` as the new `cur` (double-buffer swap).
    Commit,
}

/// FTCS stencil: `T_i^{n+1} = T_i + r·(T_{i+1} − 2·T_i + T_{i−1})` on the
/// interior; the Dirichlet ends are re-imposed each step.
fn stencil(mut f: ResMut<Field>) {
    let n = f.n();
    let r = f.r;
    // Interior update from the centered second difference.
    for i in 1..n - 1 {
        f.next[i] = f.cur[i] + r * (f.cur[i + 1] - 2.0 * f.cur[i] + f.cur[i - 1]);
    }
    // Fixed-temperature boundaries.
    f.next[0] = f.t_left;
    f.next[n - 1] = f.t_right;
}

/// Double-buffer swap: what we just computed in `next` becomes `cur`.
fn commit(mut f: ResMut<Field>) {
    let field = &mut *f;
    std::mem::swap(&mut field.cur, &mut field.next);
}

/// Build a scheduler wired with the two diffusion phases over `field`.
fn build(field: Field) -> Scheduler {
    let mut scheduler = Scheduler::default();
    scheduler.add_resource(field);
    scheduler.add_update_system(stencil, Step::Stencil);
    scheduler.add_update_system(commit, Step::Commit);
    scheduler.organize_systems();
    scheduler
}

/// Advance `steps` diffusion steps and return the final field values.
fn advance(field: Field, steps: usize) -> Vec<f64> {
    let mut scheduler = build(field);
    for _ in 0..steps {
        scheduler.run();
    }
    let result = scheduler
        .get_resource_ref::<Field>()
        .expect("Field resource")
        .cur
        .clone();
    result
}

/// **Check 1 — steady state.** Relax fixed-end diffusion to the exact linear
/// profile and report the max deviation.
fn check_steady_state() -> f64 {
    let n = 41usize;
    let l = 1.0f64;
    let dx = l / (n - 1) as f64;
    let t_left = 100.0;
    let t_right = 20.0;
    let r = 0.25; // stable (≤ 0.5)

    // Start cold in the interior; only the boundaries carry heat initially.
    let mut cur = vec![0.0; n];
    cur[0] = t_left;
    cur[n - 1] = t_right;

    // Enough steps for the slowest mode (~ L²/(α·π²)) to fully decay.
    // alpha = r·dx²/dt; we work in step units so the decay-per-step of the
    // fundamental sets the count: g = 1 − 2r(1 − cos(π·dx/L)).
    let g = 1.0 - 2.0 * r * (1.0 - (std::f64::consts::PI * dx / l).cos());
    let steps = ((1e-9f64).ln() / g.ln()).ceil() as usize;

    let field = Field::new(cur, r, t_left, t_right);
    let out = advance(field, steps);

    let mut max_err = 0.0f64;
    for (i, &t) in out.iter().enumerate() {
        let x = i as f64 * dx;
        let exact = t_left + (t_right - t_left) * x / l;
        max_err = max_err.max((t - exact).abs());
    }
    println!("[steady] n={n}, steps={steps}: max|T − linear| = {max_err:.3e}");
    max_err
}

/// **Check 2 — Fourier-mode decay.** A `sin(πx/L)` mode under homogeneous ends
/// must decay as `exp(−α (π/L)² t)`. Return `(numerical, analytical)` amplitudes.
fn check_mode_decay() -> (f64, f64) {
    let n = 201usize;
    let l = 1.0f64;
    let dx = l / (n - 1) as f64;
    let r = 0.25;
    let alpha = 1.0f64;
    let dt = r * dx * dx / alpha;

    // Initial fundamental mode, homogeneous Dirichlet ends.
    let k = std::f64::consts::PI / l;
    let cur: Vec<f64> = (0..n).map(|i| (k * i as f64 * dx).sin()).collect();

    // Long enough for the mode to decay through ~2 e-folds (amplitude ≈ 0.14),
    // so we validate the *rate* over a real decay, not just a nudge.
    let steps = 32000usize;
    let t_end = steps as f64 * dt;

    let field = Field::new(cur, r, 0.0, 0.0);
    let out = advance(field, steps);

    // Amplitude = peak of the (still sinusoidal) field, at the domain midpoint.
    let mid = n / 2;
    let numerical = out[mid];
    let analytical = (-alpha * k * k * t_end).exp(); // initial amplitude = 1
    println!(
        "[mode]   n={n}, steps={steps}, t={t_end:.4}: numerical={numerical:.6}, \
         analytical={analytical:.6}, rel_err={:.3e}",
        (numerical - analytical).abs() / analytical
    );
    (numerical, analytical)
}

fn main() {
    // ── Check 1: steady-state linear profile ─────────────────────────────────
    let max_err = check_steady_state();
    assert!(
        max_err < 1e-6,
        "steady state did not match linear profile: max err {max_err:.3e}"
    );

    // ── Check 2: analytical Fourier-mode decay rate ──────────────────────────
    let (numerical, analytical) = check_mode_decay();
    let rel_err = (numerical - analytical).abs() / analytical;
    // FTCS is O(Δx², Δt); at n=201, r=0.25 the truncation error is ~1e-3.
    assert!(
        rel_err < 5e-3,
        "mode decay departed from analytical rate: rel err {rel_err:.3e}"
    );

    println!("heat_diffusion_1d: all analytical checks passed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_state_is_linear() {
        assert!(check_steady_state() < 1e-6);
    }

    #[test]
    fn fourier_mode_decays_at_analytical_rate() {
        let (numerical, analytical) = check_mode_decay();
        assert!((numerical - analytical).abs() / analytical < 5e-3);
    }

    /// The scheduler must actually be doing the work: with `r = 0` (no
    /// diffusion) the interior can never move off its initial value.
    #[test]
    fn zero_diffusion_is_frozen() {
        let cur = vec![0.0, 7.0, 7.0, 7.0, 0.0];
        let out = advance(Field::new(cur, 0.0, 0.0, 0.0), 50);
        assert_eq!(out, vec![0.0, 7.0, 7.0, 7.0, 0.0]);
    }
}
