//! Matrix-free iterative-solver scaffolding.
//!
//! The types in this module deliberately stop at the algebra seam: callers
//! provide a [`LinearOperator`] and decide convergence with a predicate. GRASS
//! does not know whether the operator came from FEM, FVM, spectral collocation,
//! a particle method, or a coupled multi-physics residual.

/// Matrix-free action of a linear operator.
///
/// Implementors compute `y = A x` without exposing, storing, or assuming a
/// concrete matrix format.
pub trait LinearOperator {
    /// Number of scalar unknowns in the operator domain/range.
    fn dimension(&self) -> usize;

    /// Apply the operator to `x`, writing the result into `y`.
    ///
    /// # Panics
    ///
    /// Implementors may panic if either slice is not [`dimension`](Self::dimension)
    /// long.
    fn apply(&self, x: &[f64], y: &mut [f64]);

    /// Combine a locally computed partial sum (an inner product over this
    /// process's entries) into the global value the solver should see.
    ///
    /// The default is the identity, which is right for a serial operator. A
    /// distributed operator, whose `x`/`y` slices hold only this rank's owned
    /// unknowns (plus zeroed padding), returns the all-reduced sum here so the
    /// Krylov iteration's step lengths and residual norms are global and every
    /// rank takes the same branch.
    fn global_sum(&self, local: f64) -> f64 {
        local
    }
}

/// Iteration state passed to user convergence predicates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConvergenceState {
    /// Completed iteration count. The initial residual is iteration `0`.
    pub iteration: usize,
    /// Euclidean norm of the current residual.
    pub residual_norm: f64,
    /// Initial residual norm, useful for relative stopping criteria.
    pub initial_residual_norm: f64,
}

impl ConvergenceState {
    /// Residual norm relative to the initial residual.
    pub fn relative_residual(&self) -> f64 {
        if self.initial_residual_norm == 0.0 {
            0.0
        } else {
            self.residual_norm / self.initial_residual_norm
        }
    }
}

/// Summary returned by an iterative solve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IterationReport {
    /// Number of completed iterations.
    pub iterations: usize,
    /// Final residual norm.
    pub residual_norm: f64,
    /// Initial residual norm.
    pub initial_residual_norm: f64,
    /// Whether the caller's predicate accepted the final state.
    pub converged: bool,
}

impl IterationReport {
    fn from_state(state: ConvergenceState, converged: bool) -> Self {
        Self {
            iterations: state.iteration,
            residual_norm: state.residual_norm,
            initial_residual_norm: state.initial_residual_norm,
            converged,
        }
    }
}

/// Errors reported by the iterative scaffolding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IterationError {
    /// `rhs`, `x`, or a scratch vector length did not match the operator.
    DimensionMismatch {
        /// Operator dimension.
        operator: usize,
        /// Offending slice length.
        vector: usize,
    },
    /// Conjugate gradient encountered non-positive curvature `p^T A p`.
    NonPositiveCurvature {
        /// Iteration where the curvature was measured.
        iteration: usize,
        /// Measured `p^T A p`.
        curvature: f64,
    },
}

fn ensure_len(operator: usize, vector: usize) -> Result<(), IterationError> {
    if operator == vector {
        Ok(())
    } else {
        Err(IterationError::DimensionMismatch { operator, vector })
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(ai, bi)| ai * bi).sum()
}

/// Inner product reduced across processes by the operator.
fn global_dot<A: LinearOperator>(operator: &A, a: &[f64], b: &[f64]) -> f64 {
    operator.global_sum(dot(a, b))
}

/// Euclidean norm reduced across processes by the operator.
fn global_norm<A: LinearOperator>(operator: &A, a: &[f64]) -> f64 {
    global_dot(operator, a, a).sqrt()
}

/// Matrix-free conjugate gradient for symmetric positive definite operators.
///
/// The caller owns the convergence contract. The predicate is evaluated at the
/// initial residual (`iteration == 0`) and after each completed iteration.
pub fn conjugate_gradient<A, C>(
    operator: &A,
    rhs: &[f64],
    x: &mut [f64],
    max_iterations: usize,
    mut converged: C,
) -> Result<IterationReport, IterationError>
where
    A: LinearOperator,
    C: FnMut(&ConvergenceState) -> bool,
{
    let n = operator.dimension();
    ensure_len(n, rhs.len())?;
    ensure_len(n, x.len())?;

    let mut ax = vec![0.0; n];
    let mut residual = vec![0.0; n];
    let mut direction = vec![0.0; n];
    let mut ap = vec![0.0; n];

    operator.apply(x, &mut ax);
    for i in 0..n {
        residual[i] = rhs[i] - ax[i];
        direction[i] = residual[i];
    }

    let initial_residual_norm = global_norm(operator, &residual);
    let mut state = ConvergenceState {
        iteration: 0,
        residual_norm: initial_residual_norm,
        initial_residual_norm,
    };
    if converged(&state) {
        return Ok(IterationReport::from_state(state, true));
    }

    let mut rr = global_dot(operator, &residual, &residual);
    for iteration in 1..=max_iterations {
        operator.apply(&direction, &mut ap);
        let curvature = global_dot(operator, &direction, &ap);
        if curvature <= 0.0 {
            return Err(IterationError::NonPositiveCurvature {
                iteration,
                curvature,
            });
        }

        let alpha = rr / curvature;
        for i in 0..n {
            x[i] += alpha * direction[i];
            residual[i] -= alpha * ap[i];
        }

        let rr_next = global_dot(operator, &residual, &residual);
        state = ConvergenceState {
            iteration,
            residual_norm: rr_next.sqrt(),
            initial_residual_norm,
        };
        if converged(&state) {
            return Ok(IterationReport::from_state(state, true));
        }

        let beta = rr_next / rr;
        for i in 0..n {
            direction[i] = residual[i] + beta * direction[i];
        }
        rr = rr_next;
    }

    Ok(IterationReport::from_state(state, false))
}

/// Matrix-free Picard/Richardson fixed-point iteration for `A x = rhs`.
///
/// Each iteration performs `x <- x + relaxation * (rhs - A x)`. More elaborate
/// problem-specific preconditioning or nonlinear residual construction should
/// live above this seam; this helper supplies the reusable convergence loop.
pub fn picard_iteration<A, C>(
    operator: &A,
    rhs: &[f64],
    x: &mut [f64],
    relaxation: f64,
    max_iterations: usize,
    mut converged: C,
) -> Result<IterationReport, IterationError>
where
    A: LinearOperator,
    C: FnMut(&ConvergenceState) -> bool,
{
    let n = operator.dimension();
    ensure_len(n, rhs.len())?;
    ensure_len(n, x.len())?;

    let mut ax = vec![0.0; n];
    let mut residual = vec![0.0; n];

    operator.apply(x, &mut ax);
    for i in 0..n {
        residual[i] = rhs[i] - ax[i];
    }
    let initial_residual_norm = global_norm(operator, &residual);
    let mut state = ConvergenceState {
        iteration: 0,
        residual_norm: initial_residual_norm,
        initial_residual_norm,
    };
    if converged(&state) {
        return Ok(IterationReport::from_state(state, true));
    }

    for iteration in 1..=max_iterations {
        for i in 0..n {
            x[i] += relaxation * residual[i];
        }

        operator.apply(x, &mut ax);
        for i in 0..n {
            residual[i] = rhs[i] - ax[i];
        }
        state = ConvergenceState {
            iteration,
            residual_norm: global_norm(operator, &residual),
            initial_residual_norm,
        };
        if converged(&state) {
            return Ok(IterationReport::from_state(state, true));
        }
    }

    Ok(IterationReport::from_state(state, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Diagonal {
        values: Vec<f64>,
    }

    impl LinearOperator for Diagonal {
        fn dimension(&self) -> usize {
            self.values.len()
        }

        fn apply(&self, x: &[f64], y: &mut [f64]) {
            for ((yi, ai), xi) in y.iter_mut().zip(&self.values).zip(x) {
                *yi = ai * xi;
            }
        }
    }

    #[test]
    fn cg_solves_spd_diagonal_system() {
        let operator = Diagonal {
            values: vec![2.0, 4.0, 8.0],
        };
        let rhs = vec![2.0, 8.0, 24.0];
        let mut x = vec![0.0; 3];
        let report = conjugate_gradient(&operator, &rhs, &mut x, 10, |s| {
            s.relative_residual() < 1.0e-12
        })
        .expect("cg solve");

        assert!(report.converged);
        assert!(report.iterations <= 3);
        assert!((x[0] - 1.0).abs() < 1.0e-12);
        assert!((x[1] - 2.0).abs() < 1.0e-12);
        assert!((x[2] - 3.0).abs() < 1.0e-12);
    }

    /// One "rank" of a two-rank diagonal system whose other half is an exact
    /// mirror: the global inner product is twice the local one. CG must use the
    /// reduced sums (the residual norm it reports is the global one) and still
    /// land on the same solution.
    struct MirroredDiagonal {
        inner: Diagonal,
        reductions: std::cell::Cell<usize>,
    }

    impl LinearOperator for MirroredDiagonal {
        fn dimension(&self) -> usize {
            self.inner.dimension()
        }

        fn apply(&self, x: &[f64], y: &mut [f64]) {
            self.inner.apply(x, y);
        }

        fn global_sum(&self, local: f64) -> f64 {
            self.reductions.set(self.reductions.get() + 1);
            2.0 * local
        }
    }

    #[test]
    fn cg_reduces_inner_products_through_the_operator() {
        let operator = MirroredDiagonal {
            inner: Diagonal {
                values: vec![2.0, 4.0, 8.0],
            },
            reductions: std::cell::Cell::new(0),
        };
        let rhs = vec![2.0, 8.0, 24.0];
        let mut x = vec![0.0; 3];
        let report = conjugate_gradient(&operator, &rhs, &mut x, 10, |s| {
            s.relative_residual() < 1.0e-12
        })
        .expect("cg solve");

        assert!(report.converged);
        assert!(operator.reductions.get() > 0);
        let local_norm = rhs.iter().map(|v| v * v).sum::<f64>().sqrt();
        assert!((report.initial_residual_norm - 2.0f64.sqrt() * local_norm).abs() < 1.0e-12);
        for (xi, want) in x.iter().zip([1.0, 2.0, 3.0]) {
            assert!((xi - want).abs() < 1.0e-12);
        }
    }

    #[test]
    fn picard_uses_user_predicate() {
        let operator = Diagonal { values: vec![2.0] };
        let rhs = vec![4.0];
        let mut x = vec![0.0];
        let report = picard_iteration(&operator, &rhs, &mut x, 0.25, 100, |s| {
            s.residual_norm < 1.0e-10
        })
        .expect("picard solve");

        assert!(report.converged);
        assert!((x[0] - 2.0).abs() < 1.0e-10);
    }

    #[test]
    fn dimension_mismatch_is_reported() {
        let operator = Diagonal {
            values: vec![1.0, 1.0],
        };
        let mut x = vec![0.0];
        let err = conjugate_gradient(&operator, &[1.0, 1.0], &mut x, 1, |_| false)
            .expect_err("dimension mismatch");
        assert_eq!(
            err,
            IterationError::DimensionMismatch {
                operator: 2,
                vector: 1
            }
        );
    }
}
