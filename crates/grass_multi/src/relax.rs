//! Reusable **Picard / Aitken-accelerated outer-iteration combinator** for
//! strongly-coupled multi-physics (FSI-style) runs.
//!
//! A strongly-coupled problem is solved by a fixed-point outer loop over the
//! interface: feed the current interface iterate `xₖ` into the solvers, let
//! them produce a *raw* new interface value `x̃ₖ = G(xₖ)`, then decide the next
//! iterate. Plain Picard / block Gauss–Seidel takes `xₖ₊₁ = x̃ₖ` and, for a
//! strong two-way coupling (an interface fixed-point map whose spectral radius
//! is ≥ 1), **oscillates and diverges** — the classic added-mass instability of
//! partitioned FSI. Under-relaxation fixes it:
//!
//! ```text
//! rₖ    = x̃ₖ − xₖ                 (interface residual)
//! xₖ₊₁  = xₖ + ωₖ · rₖ            (relaxed update; ωₖ = 1 ⇒ plain Picard)
//! ```
//!
//! [`Relaxation`] chooses `ωₖ`:
//!   - [`Relaxation::Fixed`] holds `ω` constant (static under-relaxation).
//!   - [`Relaxation::Aitken`] adapts `ω` every iteration from the last two
//!     residuals (Aitken Δ² / Irons–Tuck dynamic relaxation), typically
//!     converging in far fewer outer iterations than a hand-tuned constant.
//!
//! The numerics live in [`OuterIteration`], a **pure, scheduler-free** core:
//! you hand it `x̃ₖ` via [`observe`](OuterIteration::observe) and it returns the
//! next iterate and tracks convergence. This makes the acceleration/relaxation
//! logic unit-testable against theory, independent of any coupling wiring.
//!
//! For the grass scheduler, [`converge_outer_iter`] wraps that core as a system
//! constructor in the spirit of [`expose_field`](crate::expose_field) /
//! [`consume_field`](crate::consume_field): give it a closure that reads the raw
//! interface output `x̃` from a sub-App and one that writes the relaxed iterate
//! back, and it drives one outer step and requests `SchedulerState::End` once
//! the residual test passes (or `max_iters` is hit). So an FSI-style run wires a
//! *converging* outer loop instead of re-implementing relaxation + a stopping
//! test by hand.
//!
//! ## References
//! - U. Küttler & W. A. Wall, *Fixed-point fluid–structure interaction solvers
//!   with dynamic relaxation*, Comput. Mech. 43 (2008) 61–72 — the Aitken Δ²
//!   relaxation formula used here.
//! - B. M. Irons & R. C. Tuck, *A version of the Aitken accelerator for
//!   computer iteration*, Int. J. Numer. Methods Eng. 1 (1969) 275–277.
//!
//! ## Usage (pure core)
//! ```rust
//! use grass_multi::{OuterIteration, Relaxation};
//!
//! // Interface fixed-point map G(x) = -3·x + 1  (strongly coupled: |slope| > 1,
//! // so plain Picard diverges). True fixed point x* = 1/(1-(-3)) = 0.25.
//! let g = |x: f64| -3.0 * x + 1.0;
//!
//! let mut it = OuterIteration::new(vec![0.0], Relaxation::Aitken { omega0: 0.5 }, 1e-12, 100);
//! while !it.done() {
//!     let x_tilde = g(it.input()[0]);
//!     it.observe(&[x_tilde]);
//! }
//! assert!(it.converged());
//! assert!((it.input()[0] - 0.25).abs() < 1e-9);
//! ```

use crate::multi::Multi;
use grass_scheduler::prelude::{ResMut, SchedulerManager, SchedulerState};

/// Under-relaxation scheme for the outer (coupling) fixed-point iteration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Relaxation {
    /// Constant under-relaxation factor `ω`. `ω = 1` is plain (unrelaxed)
    /// Picard / block Gauss–Seidel; `0 < ω < 1` damps the update to stabilize a
    /// strong coupling. Convergence for a map with interface amplification
    /// factor `a` needs `|1 + ω(a − 1)| < 1`.
    Fixed(f64),
    /// Aitken Δ² dynamic relaxation (Irons–Tuck 1969; Küttler & Wall 2008): `ω`
    /// is recomputed each iteration from the last two residuals,
    /// `ωₖ = −ωₖ₋₁ · (rₖ₋₁ · Δr) / (Δr · Δr)` with `Δr = rₖ − rₖ₋₁`. `omega0`
    /// is the factor used for the very first step, before a second residual
    /// exists.
    Aitken {
        /// Relaxation factor for the first outer iteration.
        omega0: f64,
    },
}

/// Pure, scheduler-free core of the accelerated outer iteration.
///
/// Holds the current interface iterate `x`, applies the chosen [`Relaxation`]
/// each time a raw fixed-point output is [`observe`](Self::observe)d, and tracks
/// the residual norm and convergence. Operates on an interface vector in `ℝⁿ`
/// (`Vec<f64>`), the paradigm-agnostic representation of interface DOFs — it
/// assumes nothing about meshes or particles.
#[derive(Debug, Clone)]
pub struct OuterIteration {
    /// Current interface iterate `xₖ` — the value to feed the solvers next.
    x: Vec<f64>,
    relax: Relaxation,
    /// Convergence tolerance on the L2 norm of the interface residual `‖rₖ‖₂`.
    tol: f64,
    /// Hard cap on outer iterations (safety net against a non-converging map).
    max_iters: u32,
    /// Outer iterations performed so far.
    iters: u32,
    /// Relaxation factor applied on the most recent step (and, for Aitken, the
    /// `ωₖ₋₁` carried into the next).
    omega: f64,
    /// Previous residual `rₖ₋₁`, kept for the Aitken update.
    prev_res: Option<Vec<f64>>,
    /// L2 norm of the most recent residual.
    res_norm: f64,
    /// Whether the most recent residual passed the tolerance.
    converged: bool,
}

impl OuterIteration {
    /// New iteration seeded with the initial interface guess `x0`.
    ///
    /// `tol` is compared against the L2 norm of the interface residual;
    /// `max_iters` bounds the loop so a divergent map still terminates.
    pub fn new(x0: Vec<f64>, relax: Relaxation, tol: f64, max_iters: u32) -> Self {
        let omega = match relax {
            Relaxation::Fixed(w) => w,
            Relaxation::Aitken { omega0 } => omega0,
        };
        Self {
            x: x0,
            relax,
            tol,
            max_iters,
            iters: 0,
            omega,
            prev_res: None,
            res_norm: f64::INFINITY,
            converged: false,
        }
    }

    /// The current interface iterate `xₖ` to feed into the coupled solvers.
    pub fn input(&self) -> &[f64] {
        &self.x
    }

    /// Feed the raw fixed-point output `x̃ₖ = G(xₖ)` (what the solvers produced
    /// from the current [`input`](Self::input)). Computes the residual, updates
    /// convergence state, applies the relaxation, advances the iterate, and
    /// returns the next [`input`](Self::input) `xₖ₊₁`.
    ///
    /// # Panics
    /// If `x_tilde.len()` differs from the interface dimension.
    pub fn observe(&mut self, x_tilde: &[f64]) -> &[f64] {
        assert_eq!(
            x_tilde.len(),
            self.x.len(),
            "observed output dimension {} != interface dimension {}",
            x_tilde.len(),
            self.x.len()
        );

        // Interface residual rₖ = x̃ₖ − xₖ and its L2 norm.
        let res: Vec<f64> = x_tilde.iter().zip(&self.x).map(|(t, x)| t - x).collect();
        self.res_norm = l2(&res);
        self.converged = self.res_norm <= self.tol;

        // Pick ωₖ.
        match self.relax {
            Relaxation::Fixed(w) => self.omega = w,
            Relaxation::Aitken { .. } => {
                if let Some(prev) = &self.prev_res {
                    // Δr = rₖ − rₖ₋₁
                    let dr: Vec<f64> = res.iter().zip(prev).map(|(r, p)| r - p).collect();
                    let denom = dot(&dr, &dr);
                    if denom > f64::MIN_POSITIVE {
                        // ωₖ = −ωₖ₋₁ · (rₖ₋₁ · Δr) / (Δr · Δr)
                        let next = -self.omega * dot(prev, &dr) / denom;
                        if next.is_finite() {
                            self.omega = next;
                        }
                    }
                    // denom ≈ 0 ⇒ residual unchanged; keep the previous ω.
                }
                // No previous residual ⇒ keep omega0 (already in self.omega).
            }
        }

        // Relaxed update xₖ₊₁ = xₖ + ωₖ · rₖ.
        for (x, r) in self.x.iter_mut().zip(&res) {
            *x += self.omega * r;
        }

        self.prev_res = Some(res);
        self.iters += 1;
        &self.x
    }

    /// `true` once the last observed residual met the tolerance.
    pub fn converged(&self) -> bool {
        self.converged
    }

    /// Loop-termination predicate: converged, or the iteration cap is reached.
    pub fn done(&self) -> bool {
        self.converged || self.iters >= self.max_iters
    }

    /// Number of outer iterations performed.
    pub fn iters(&self) -> u32 {
        self.iters
    }

    /// L2 norm of the most recent interface residual (`+∞` before the first
    /// [`observe`](Self::observe)).
    pub fn residual_norm(&self) -> f64 {
        self.res_norm
    }

    /// Relaxation factor applied on the most recent step. For
    /// [`Relaxation::Aitken`] this is the dynamically chosen `ωₖ`.
    pub fn omega(&self) -> f64 {
        self.omega
    }

    /// Interface dimension `n`.
    pub fn dim(&self) -> usize {
        self.x.len()
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn l2(v: &[f64]) -> f64 {
    dot(v, v).sqrt()
}

/// System constructor: drive one **accelerated outer-iteration step** and stop
/// the parent loop on convergence.
///
/// Reads the raw interface output `x̃` from the coupled solvers via
/// `read_output`, feeds it to the [`OuterIteration`] resource (relaxation +
/// convergence test), writes the next relaxed iterate back via `write_input`,
/// and sets `SchedulerState::End` once [`OuterIteration::done`] holds (residual
/// within tolerance, or `max_iters` reached).
///
/// Register it in a parent phase that runs **after** the solvers have ticked and
/// the interface output is available — the canonical shape is
/// `TickA → Couple → TickB → Converge`. The parent must hold an
/// [`OuterIteration`] resource (`parent.add_resource(OuterIteration::new(..))`)
/// whose `x0` matches the interface value initially fed to the first solver.
///
/// Like [`expose_field`](crate::expose_field), both closures reach into sub-Apps
/// through [`Multi`]; keep this system out of the same phase as any
/// `tick_subapp` (which takes `ResMut<SubApps>`) — see the crate borrow rules.
///
/// ```rust,ignore
/// parent.add_resource(OuterIteration::new(vec![x0], Relaxation::Aitken { omega0: 0.5 }, 1e-10, 50));
/// parent.add_update_system(
///     converge_outer_iter(
///         // x̃: read the raw new interface value the "solid" solver produced
///         |w| vec![w.expect_read::<Solid>("solid").x_out],
///         // write the relaxed iterate back as the "fluid" solver's input
///         |w, x| w.expect_write::<Fluid>("fluid").x_in = x[0],
///     ),
///     Phase::Converge,
/// );
/// ```
pub fn converge_outer_iter<R, W>(
    read_output: R,
    write_input: W,
) -> impl FnMut(Multi, ResMut<OuterIteration>, ResMut<SchedulerManager>)
where
    R: Fn(&Multi) -> Vec<f64> + 'static,
    W: Fn(&Multi, &[f64]) + 'static,
{
    move |world: Multi, mut it: ResMut<OuterIteration>, mut sm: ResMut<SchedulerManager>| {
        let x_tilde = read_output(&world);
        let next = it.observe(&x_tilde).to_vec();
        write_input(&world, &next);
        if it.done() {
            sm.state = SchedulerState::End;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Scalar strongly-coupled map G(x) = a·x + b, |a| > 1 ────────────────────
    // Fixed point x* = b / (1 − a). With a = −3 the map oscillates and plain
    // Picard diverges, so this is a genuine strong-coupling stress test.
    const A: f64 = -3.0;
    const B: f64 = 1.0;
    fn g(x: f64) -> f64 {
        A * x + B
    }
    fn x_star() -> f64 {
        B / (1.0 - A) // = 0.25
    }

    fn run_scalar(relax: Relaxation, x0: f64, max_iters: u32) -> OuterIteration {
        let mut it = OuterIteration::new(vec![x0], relax, 1e-12, max_iters);
        while !it.done() {
            let out = g(it.input()[0]);
            it.observe(&[out]);
        }
        it
    }

    #[test]
    fn plain_picard_diverges_on_strong_coupling() {
        // ω = 1 (no relaxation): the residual must blow up, never converging.
        let it = run_scalar(Relaxation::Fixed(1.0), 0.0, 40);
        assert!(!it.converged(), "plain Picard should not converge on a=−3");
        assert_eq!(it.iters(), 40, "should run to the iteration cap");
        assert!(
            it.residual_norm() > 1e3,
            "residual should diverge, got {}",
            it.residual_norm()
        );
    }

    #[test]
    fn fixed_relaxation_converges_to_fixed_point() {
        // ω = 1/(1−a) = 0.25 is the optimal constant factor here (it makes the
        // iteration matrix exactly zero → one-step convergence to x*).
        let it = run_scalar(Relaxation::Fixed(0.25), 0.0, 50);
        assert!(it.converged(), "relaxed Picard should converge");
        assert!(
            (it.input()[0] - x_star()).abs() < 1e-10,
            "converged to {} != x* {}",
            it.input()[0],
            x_star()
        );
        // ω = 0.25 zeroes the residual in a single relaxed step; detection needs
        // one more iteration to *measure* the zero residual.
        assert!(it.iters() <= 2, "optimal ω should converge immediately");
    }

    #[test]
    fn aitken_discovers_the_optimal_factor_and_converges() {
        let it = run_scalar(Relaxation::Aitken { omega0: 0.5 }, 0.0, 50);
        assert!(it.converged(), "Aitken should converge on a=−3");
        assert!(
            (it.input()[0] - x_star()).abs() < 1e-10,
            "Aitken converged to {} != x* {}",
            it.input()[0],
            x_star()
        );
        // Aitken auto-tunes toward the optimal ω = 0.25 without it being given.
        assert!(
            (it.omega() - 0.25).abs() < 1e-9,
            "Aitken ω should approach optimal 0.25, got {}",
            it.omega()
        );
        assert!(
            it.iters() <= 4,
            "Aitken should converge fast, took {}",
            it.iters()
        );
    }

    // ── Vector strongly-coupled map G(x) = M·x + c (coupled 2-field) ────────────
    // M = [[-2,-1],[-1,-2]] has eigenvalues −1 and −3 (a strong two-way
    // coupling; plain Picard diverges). Fixed point x* = (I − M)⁻¹ c with
    // I − M = [[3,1],[1,3]] (det 8), so (I − M)⁻¹ = (1/8)[[3,−1],[−1,3]].
    fn g_vec(x: &[f64]) -> Vec<f64> {
        vec![
            -2.0 * x[0] - x[1] + 1.0, // c = (1, 2)
            -x[0] - 2.0 * x[1] + 2.0,
        ]
    }
    fn x_star_vec() -> [f64; 2] {
        // (1/8)[[3,−1],[−1,3]] · (1,2)
        [(3.0 - 2.0) / 8.0, (-1.0 + 6.0) / 8.0]
    }

    fn run_vec(relax: Relaxation, max_iters: u32) -> OuterIteration {
        let mut it = OuterIteration::new(vec![0.0, 0.0], relax, 1e-12, max_iters);
        while !it.done() {
            let out = g_vec(it.input());
            it.observe(&out);
        }
        it
    }

    #[test]
    fn vector_plain_picard_diverges() {
        let it = run_vec(Relaxation::Fixed(1.0), 40);
        assert!(!it.converged());
        assert!(it.residual_norm() > 1e3, "should diverge");
    }

    #[test]
    fn vector_aitken_converges_to_analytic_fixed_point() {
        let it = run_vec(Relaxation::Aitken { omega0: 0.5 }, 100);
        assert!(
            it.converged(),
            "Aitken should converge on the 2×2 coupled map"
        );
        let xs = x_star_vec();
        assert!(
            (it.input()[0] - xs[0]).abs() < 1e-9 && (it.input()[1] - xs[1]).abs() < 1e-9,
            "Aitken converged to {:?} != x* {:?}",
            it.input(),
            xs
        );
    }

    #[test]
    fn aitken_beats_fixed_relaxation_in_iteration_count() {
        // A safe constant factor for eigenvalues {−1,−3} needs ω < 0.5; take a
        // conservative ω = 0.3. Aitken should reach the same tolerance in no
        // more iterations (typically far fewer).
        let fixed = run_vec(Relaxation::Fixed(0.3), 500);
        let aitken = run_vec(Relaxation::Aitken { omega0: 0.3 }, 500);
        assert!(fixed.converged() && aitken.converged());
        assert!(
            aitken.iters() <= fixed.iters(),
            "Aitken ({}) should not need more iters than fixed ({})",
            aitken.iters(),
            fixed.iters()
        );
    }
}
