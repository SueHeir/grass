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
//!   - [`Relaxation::IqnIls`] is interface quasi-Newton (IQN-ILS): it fits an
//!     inverse Jacobian of the interface map from every secant pair seen so
//!     far, optionally reusing pairs from earlier time windows, and filters
//!     nearly dependent pairs with a QR filter. It needs far fewer iterations
//!     than Aitken when several interface modes are strongly coupled.
//!
//! For time-dependent runs, [`OuterIteration::begin_window`] starts the next
//! window's fixed point, and [`ImplicitCoupling`](crate::ImplicitCoupling)
//! wraps the whole checkpoint, iterate and accept cycle.
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
//! - J. Degroote, K.-J. Bathe & J. Vierendeels, *Performance of a new
//!   partitioned procedure versus a monolithic procedure in fluid–structure
//!   interaction*, Comput. Struct. 87 (2009) 793–801 — IQN-ILS.
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
    /// Interface quasi-Newton with an inverse Jacobian from a least-squares
    /// fit (IQN-ILS; Degroote, Bathe & Vierendeels 2009). Every iteration adds
    /// one secant pair `(Δr, Δx̃)` and the next iterate is
    /// `xₖ₊₁ = x̃ₖ + W c` with `c = argmin ‖V c + rₖ‖₂`, where the columns of `V`
    /// and `W` are the residual and output differences. For a linear map of
    /// dimension `n` it converges in at most `n + 1` iterations; in practice it
    /// needs far fewer than Aitken when the interface has many coupled modes.
    ///
    /// The first step of a run (before any secant pair exists) is relaxed with
    /// `omega0`.
    IqnIls {
        /// Relaxation factor for the first step, before any secant pair.
        omega0: f64,
        /// Number of *previous* time windows whose secant pairs are kept
        /// after [`OuterIteration::begin_window`]. `0` starts each window from
        /// scratch; `5` to `20` is typical for time-dependent FSI.
        reuse: usize,
        /// QR filter: a column is dropped when orthogonalizing it against the
        /// newer ones leaves less than `filter` times its own length (it is
        /// nearly linearly dependent). `1e-8` to `1e-2`; larger is more robust.
        /// Independently of `filter`, a column is also dropped when what
        /// remains is below `√eps·‖x̃‖`, the level of rounding noise in
        /// differences of nearly converged iterates.
        filter: f64,
    },
}

impl Relaxation {
    /// IQN-ILS with `omega0 = 0.1`, no reuse and a `1e-6` filter.
    pub fn iqn_ils() -> Self {
        Self::IqnIls {
            omega0: 0.1,
            reuse: 0,
            filter: 1e-6,
        }
    }

    fn omega0(&self) -> f64 {
        match *self {
            Self::Fixed(w) => w,
            Self::Aitken { omega0 } | Self::IqnIls { omega0, .. } => omega0,
        }
    }
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
    /// Previous raw output `x̃ₖ₋₁`, kept for the IQN-ILS secant pairs.
    prev_out: Option<Vec<f64>>,
    /// Secant pairs `(Δr, Δx̃)` of the current window, newest first.
    pairs: Vec<(Vec<f64>, Vec<f64>)>,
    /// Secant pairs of earlier windows, newest window first.
    history: std::collections::VecDeque<Vec<(Vec<f64>, Vec<f64>)>>,
    /// Columns the QR filter dropped on the most recent step.
    filtered: usize,
}

impl OuterIteration {
    /// New iteration seeded with the initial interface guess `x0`.
    ///
    /// `tol` is compared against the L2 norm of the interface residual;
    /// `max_iters` bounds the loop so a divergent map still terminates.
    pub fn new(x0: Vec<f64>, relax: Relaxation, tol: f64, max_iters: u32) -> Self {
        let omega = relax.omega0();
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
            prev_out: None,
            pairs: Vec::new(),
            history: std::collections::VecDeque::new(),
            filtered: 0,
        }
    }

    /// Start a new coupling time window from the current iterate.
    ///
    /// Clears the iteration count and convergence flag and forgets the
    /// residual history that Aitken and IQN-ILS build within a window. IQN-ILS
    /// keeps the secant pairs of the last `reuse` windows, which is what makes
    /// it fast on time-dependent problems: from the second window on, the
    /// first iterate is already a quasi-Newton step.
    pub fn begin_window(&mut self) {
        if let Relaxation::IqnIls { reuse, .. } = self.relax {
            let pairs = std::mem::take(&mut self.pairs);
            if reuse > 0 && !pairs.is_empty() {
                self.history.push_front(pairs);
            }
            self.history.truncate(reuse);
        }
        self.pairs.clear();
        self.prev_res = None;
        self.prev_out = None;
        self.omega = self.relax.omega0();
        self.iters = 0;
        self.converged = false;
        self.res_norm = f64::INFINITY;
    }

    /// Replace the current iterate, for example with an extrapolated guess at
    /// the start of a window.
    ///
    /// # Panics
    /// If `x.len()` differs from the interface dimension.
    pub fn set_input(&mut self, x: &[f64]) {
        assert_eq!(x.len(), self.x.len(), "interface dimension mismatch");
        self.x.copy_from_slice(x);
    }

    /// Secant pairs IQN-ILS used on the most recent step, after filtering
    /// (zero for the other schemes).
    pub fn qn_columns(&self) -> usize {
        self.pairs.len() + self.history.iter().map(Vec::len).sum::<usize>() - self.filtered
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

        if let Relaxation::IqnIls { filter, .. } = self.relax {
            self.observe_iqn(x_tilde, res, filter);
            return &self.x;
        }

        // Pick ωₖ.
        match self.relax {
            Relaxation::IqnIls { .. } => unreachable!("handled above"),
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

    fn observe_iqn(&mut self, x_tilde: &[f64], res: Vec<f64>, filter: f64) {
        if let (Some(pr), Some(po)) = (&self.prev_res, &self.prev_out) {
            let dr: Vec<f64> = res.iter().zip(pr).map(|(a, b)| a - b).collect();
            let dx: Vec<f64> = x_tilde.iter().zip(po).map(|(a, b)| a - b).collect();
            self.pairs.insert(0, (dr, dx));
        }
        let cols: Vec<&(Vec<f64>, Vec<f64>)> = self
            .pairs
            .iter()
            .chain(self.history.iter().flatten())
            .collect();
        self.filtered = 0;
        if cols.is_empty() {
            self.omega = self.relax.omega0();
            for (x, r) in self.x.iter_mut().zip(&res) {
                *x += self.omega * r;
            }
        } else {
            // Modified Gram–Schmidt QR of V (newest column first), dropping
            // columns that are nearly dependent on the ones already kept.
            let n = res.len();
            // A column whose orthogonal part is this short is rounding error,
            // however long the column is relative to itself: differences of
            // nearly converged iterates carry noise of order eps·‖x̃‖, and a
            // relative filter alone lets them in once the iteration has
            // converged (they then poison the windows that reuse them).
            let floor = f64::EPSILON.sqrt() * l2(x_tilde);
            let mut q: Vec<Vec<f64>> = Vec::new();
            let mut r: Vec<Vec<f64>> = Vec::new(); // r[k][j], j <= k
            let mut kept: Vec<usize> = Vec::new();
            for (ci, (v, _)) in cols.iter().enumerate() {
                let len = l2(v);
                let mut u = v.clone();
                let mut rk = Vec::with_capacity(q.len() + 1);
                for qj in &q {
                    let rjk = dot(qj, &u);
                    for (ui, qi) in u.iter_mut().zip(qj) {
                        *ui -= rjk * qi;
                    }
                    rk.push(rjk);
                }
                let norm = l2(&u);
                if len == 0.0 || norm < filter * len || norm < floor || q.len() == n {
                    self.filtered += 1;
                    continue;
                }
                u.iter_mut().for_each(|ui| *ui /= norm);
                rk.push(norm);
                q.push(u);
                r.push(rk);
                kept.push(ci);
            }
            // Solve R c = −Qᵀ r (R upper triangular, stored by column).
            let m = q.len();
            let mut c: Vec<f64> = q.iter().map(|qj| -dot(qj, &res)).collect();
            for i in (0..m).rev() {
                c[i] /= r[i][i];
                let ci = c[i];
                for (j, cj) in c.iter_mut().enumerate().take(i) {
                    *cj -= r[i][j] * ci;
                }
            }
            // xₖ₊₁ = x̃ₖ + W c
            self.x.copy_from_slice(x_tilde);
            for (ci, &col) in c.iter().zip(&kept) {
                for (x, w) in self.x.iter_mut().zip(&cols[col].1) {
                    *x += ci * w;
                }
            }
        }
        self.prev_res = Some(res);
        self.prev_out = Some(x_tilde.to_vec());
        self.iters += 1;
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

    // ── IQN-ILS ────────────────────────────────────────────────────────────
    // A dense symmetric interface map G(x) = M x + c, M = H D H with H a
    // Householder reflection, so the spectrum D is known exactly.
    fn spread_map(n: usize, lo: f64, hi: f64) -> (Vec<Vec<f64>>, Vec<f64>) {
        let d: Vec<f64> = (0..n)
            .map(|k| lo + (hi - lo) * k as f64 / (n - 1) as f64)
            .collect();
        map_with_spectrum(&d)
    }

    fn map_with_spectrum(d: &[f64]) -> (Vec<Vec<f64>>, Vec<f64>) {
        let n = d.len();
        let v: Vec<f64> = (0..n).map(|i| 1.0 + (i as f64 * 0.7).sin()).collect();
        let vv = dot(&v, &v);
        let h = |i: usize, j: usize| (if i == j { 1.0 } else { 0.0 }) - 2.0 * v[i] * v[j] / vv;
        let m = (0..n)
            .map(|i| {
                (0..n)
                    .map(|j| (0..n).map(|k| h(i, k) * d[k] * h(k, j)).sum())
                    .collect()
            })
            .collect();
        let c = (0..n).map(|i| (i as f64 * 0.3).cos()).collect();
        (m, c)
    }

    fn apply_map(m: &[Vec<f64>], c: &[f64], x: &[f64]) -> Vec<f64> {
        m.iter().zip(c).map(|(row, ci)| dot(row, x) + ci).collect()
    }

    fn run_map(m: &[Vec<f64>], c: &[f64], relax: Relaxation, max: u32) -> OuterIteration {
        let mut it = OuterIteration::new(vec![0.0; c.len()], relax, 1e-10, max);
        while !it.done() {
            let out = apply_map(m, c, it.input());
            it.observe(&out);
        }
        it
    }

    #[test]
    fn iqn_ils_solves_a_linear_map_in_at_most_n_plus_one_steps() {
        // Eigenvalues from −3 to 0.5: strongly coupled, plain Picard diverges.
        for n in [2, 5, 12] {
            let (m, c) = spread_map(n, -3.0, 0.5);
            let it = run_map(&m, &c, Relaxation::iqn_ils(), 200);
            assert!(it.converged(), "n={n}: residual {}", it.residual_norm());
            // n + 1 quasi-Newton steps reach the fixed point; one more
            // observation measures the zero residual.
            assert!(
                it.iters() <= n as u32 + 2,
                "n={n}: {} iterations",
                it.iters()
            );
            let out = apply_map(&m, &c, it.input());
            let err = l2(&out
                .iter()
                .zip(it.input())
                .map(|(a, b)| a - b)
                .collect::<Vec<_>>());
            assert!(err < 1e-8, "n={n}: fixed-point error {err}");
        }
    }

    #[test]
    fn iqn_ils_beats_aitken_when_a_few_modes_are_unstable() {
        // The usual partitioned-FSI picture: a handful of strongly unstable
        // interface modes (added mass) among many mild ones. IQN-ILS learns
        // the few bad modes; one Aitken factor must damp the worst mode, which
        // leaves the mild ones crawling.
        let mut d: Vec<f64> = (0..37).map(|k| -0.2 + 0.5 * k as f64 / 36.0).collect();
        d.extend([-2.0, -5.0, -8.0]);
        let (m, c) = map_with_spectrum(&d);
        let iqn = run_map(&m, &c, Relaxation::iqn_ils(), 1000);
        let aitken = run_map(&m, &c, Relaxation::Aitken { omega0: 0.1 }, 1000);
        assert!(iqn.converged());
        assert!(
            !aitken.converged() || aitken.iters() >= 2 * iqn.iters(),
            "IQN-ILS {} vs Aitken {} iterations",
            iqn.iters(),
            aitken.iters()
        );
    }

    #[test]
    fn reusing_earlier_windows_speeds_up_later_ones() {
        // Same interface Jacobian every window, different right-hand side,
        // as in a time-dependent problem with fixed physics.
        let (m, c0) = spread_map(16, -2.0, 0.5);
        let run = |reuse: usize| -> Vec<u32> {
            let relax = Relaxation::IqnIls {
                omega0: 0.1,
                reuse,
                filter: 1e-8,
            };
            let mut it = OuterIteration::new(vec![0.0; 16], relax, 1e-9, 200);
            let mut counts = Vec::new();
            for w in 0..6 {
                let c: Vec<f64> = c0.iter().map(|v| v * (1.0 + 0.2 * w as f64)).collect();
                while !it.done() {
                    let out = apply_map(&m, &c, it.input());
                    it.observe(&out);
                }
                assert!(it.converged(), "reuse={reuse} window {w}");
                assert!(it.input().iter().all(|x| x.is_finite()));
                counts.push(it.iters());
                it.begin_window();
            }
            counts
        };
        let fresh = run(0);
        let reused = run(8);
        // Later windows with reuse need only a couple of iterations; without
        // it every window pays the full cost again.
        assert!(fresh[1..].iter().all(|&k| k >= 10), "fresh: {fresh:?}");
        assert!(reused[2..].iter().all(|&k| k <= 3), "reused: {reused:?}");
    }

    #[test]
    fn filter_drops_dependent_columns_without_breaking() {
        // A scalar map makes every secant pair parallel: all but one column
        // must be filtered, and the iteration must still converge.
        let relax = Relaxation::IqnIls {
            omega0: 0.5,
            reuse: 4,
            filter: 1e-6,
        };
        let mut it = OuterIteration::new(vec![0.0], relax, 1e-12, 50);
        for _ in 0..3 {
            while !it.done() {
                let out = g(it.input()[0]);
                it.observe(&[out]);
            }
            assert!(it.converged());
            assert!((it.input()[0] - x_star()).abs() < 1e-10);
            assert!(it.qn_columns() <= 1);
            it.begin_window();
        }
    }

    /// The interface map of a coupling-matrix case (three roles in a chain,
    /// Gauss-Seidel, 8 interface values) on which IQN-ILS used to blow up:
    /// a weak rank-3 map, so each window converges in a few iterations and
    /// leaves secant pairs whose differences are at the rounding level.
    /// With reuse and a permissive relative filter those pairs were kept,
    /// the least-squares coefficients grew to 1e15, and a later window
    /// diverged to 1e22 and never converged.
    #[test]
    fn rounding_level_pairs_from_converged_windows_are_not_reused() {
        let n = 8;
        let mut m = vec![vec![0.0; n]; n];
        m[0][4] = 0.06383347307998544;
        m[2][2] = -0.01066086506435665;
        m[4][4] = -0.0044347577374231895;
        let x0 = [
            0.03923091636190357,
            -0.7724715783505647,
            0.4775912173541521,
            0.9433078248873221,
            0.5339205345999984,
            0.4220202338402552,
            0.6938835191440254,
            -0.05522578830967739,
        ];
        let b: [[f64; 8]; 8] = [
            [
                0.02401795366641371,
                -0.4879376612309469,
                0.3733687191527278,
                0.6164065779441,
                0.3467189445043419,
                0.30286635693480884,
                0.5456748157217128,
                -0.022022368373207955,
            ],
            [
                0.02819427017625702,
                -0.3092101931392593,
                0.2834358427052934,
                0.4031956147541717,
                0.22263108529059886,
                0.2158299884481517,
                0.4291224626292367,
                -0.006258832512234557,
            ],
            [
                0.025923135771523536,
                -0.19656806432797985,
                0.21241315085580617,
                0.26407069615827605,
                0.141838294223032,
                0.15293508997391067,
                0.3374648831638453,
                0.0006538147724864326,
            ],
            [
                0.02138924923994134,
                -0.1253375327438082,
                0.15776867020593463,
                0.1732289542654098,
                0.08953257934144834,
                0.10787340407119331,
                0.26538472647418293,
                0.0032215726319125434,
            ],
            [
                0.016578423863966744,
                -0.08014475311607097,
                0.11645419156245553,
                0.11386140995151539,
                0.055879454629911014,
                0.07581063443256782,
                0.20870039094284756,
                0.003768582540589548,
            ],
            [
                0.0123237782398086,
                -0.05137945267507437,
                0.08559204906666781,
                0.07501867640499771,
                0.034378642905079644,
                0.05312433641578078,
                0.1641234360332888,
                0.0034639773049072132,
            ],
            [
                0.00888245418117355,
                -0.033013942372688856,
                0.06273069087434123,
                0.04956804406970293,
                0.020753673797309358,
                0.03714519532773165,
                0.12906780928239653,
                0.0028735546656712056,
            ],
            [
                0.006245491921845249,
                -0.02125449665863463,
                0.04589461448981145,
                0.032862340292144375,
                0.012204103915685648,
                0.025931436827722584,
                0.10149982108331118,
                0.0022539529656977683,
            ],
        ];
        let relax = Relaxation::IqnIls {
            omega0: 0.5,
            reuse: 4,
            filter: 1e-10,
        };
        let mut it = OuterIteration::new(x0.to_vec(), relax, 1e-12, 60);
        for (w, bw) in b.iter().enumerate() {
            while !it.done() {
                let out = apply_map(&m, bw, it.input());
                it.observe(&out);
            }
            assert!(
                it.converged() && it.iters() <= 8,
                "window {w}: {} iterations, residual {:e}",
                it.iters(),
                it.residual_norm()
            );
            it.begin_window();
        }
    }
}
