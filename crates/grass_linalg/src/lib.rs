//! Matrix-free iterative solvers whose inner products are reduced across ranks.
//!
//! These solvers stop at the algebra seam. A caller supplies a
//! [`LinearOperator`] (and optionally a [`Preconditioner`]) and decides
//! convergence with a predicate. They do not know whether the operator came
//! from FEM, FVM, a particle method, or a coupled residual.
//!
//! The only parallel operation a Krylov method needs is a global sum, so each
//! solver takes a [`CommBackend`] and all-reduces every inner product and norm
//! through it. With [`grass_mpi::SingleProcessComm`] that is a no-op.
//!
//! # Layout contract
//!
//! Each rank's slices hold its own rows. Entries this rank does not own (ghost
//! or halo padding) must be zero in `rhs`, in the initial guess, and in every
//! operator and preconditioner output, so they never enter a dot product.

#![warn(missing_docs)]

pub use grass_mpi::{CommBackend, SingleProcessComm};

/// Matrix-free action of a linear operator, `y = A x`.
pub trait LinearOperator {
    /// Length of the vectors the operator maps between (this rank's slice).
    fn dimension(&self) -> usize;

    /// Apply the operator to `x`, writing the result into `y`.
    fn apply(&self, x: &[f64], y: &mut [f64]);
}

/// A preconditioner: `z ≈ M⁻¹ r` for an SPD `M` (for PCG).
pub trait Preconditioner {
    /// Apply the preconditioner to the residual `r`, writing into `z`.
    fn apply(&self, r: &[f64], z: &mut [f64]);
}

/// The identity preconditioner; turns PCG into plain CG.
#[derive(Clone, Copy, Debug, Default)]
pub struct Identity;

impl Preconditioner for Identity {
    fn apply(&self, r: &[f64], z: &mut [f64]) {
        z.copy_from_slice(r);
    }
}

/// Iteration state passed to user convergence predicates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConvergenceState {
    /// Completed iteration count. The initial residual is iteration `0`.
    pub iteration: usize,
    /// Global Euclidean norm of the current residual.
    pub residual_norm: f64,
    /// Global norm of the initial residual.
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
    /// Final global residual norm.
    pub residual_norm: f64,
    /// Initial global residual norm.
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

/// Errors reported by the solvers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IterationError {
    /// `rhs`, `x`, or a scratch vector length did not match the operator.
    DimensionMismatch {
        /// Operator dimension.
        operator: usize,
        /// Offending slice length.
        vector: usize,
    },
    /// CG encountered non-positive curvature `pᵀ A p` (operator not SPD).
    NonPositiveCurvature {
        /// Iteration where the curvature was measured.
        iteration: usize,
        /// Measured `pᵀ A p`.
        curvature: f64,
    },
    /// PCG encountered non-positive `rᵀ z` (preconditioner not SPD).
    IndefinitePreconditioner {
        /// Iteration where it was measured.
        iteration: usize,
        /// Measured `rᵀ z`.
        value: f64,
    },
}

fn ensure_len(operator: usize, vector: usize) -> Result<(), IterationError> {
    if operator == vector {
        Ok(())
    } else {
        Err(IterationError::DimensionMismatch { operator, vector })
    }
}

/// Inner product summed over all ranks.
fn dot(comm: &dyn CommBackend, a: &[f64], b: &[f64]) -> f64 {
    comm.all_reduce_sum_f64(a.iter().zip(b).map(|(x, y)| x * y).sum())
}

fn norm(comm: &dyn CommBackend, a: &[f64]) -> f64 {
    dot(comm, a, a).sqrt()
}

/// Conjugate gradient for symmetric positive definite operators.
///
/// The predicate is evaluated at the initial residual (`iteration == 0`) and
/// after each completed iteration.
pub fn conjugate_gradient<A, C>(
    comm: &dyn CommBackend,
    operator: &A,
    rhs: &[f64],
    x: &mut [f64],
    max_iterations: usize,
    converged: C,
) -> Result<IterationReport, IterationError>
where
    A: LinearOperator,
    C: FnMut(&ConvergenceState) -> bool,
{
    preconditioned_conjugate_gradient(comm, operator, &Identity, rhs, x, max_iterations, converged)
}

/// Preconditioned conjugate gradient. Both the operator and the preconditioner
/// must be symmetric positive definite.
pub fn preconditioned_conjugate_gradient<A, P, C>(
    comm: &dyn CommBackend,
    operator: &A,
    preconditioner: &P,
    rhs: &[f64],
    x: &mut [f64],
    max_iterations: usize,
    mut converged: C,
) -> Result<IterationReport, IterationError>
where
    A: LinearOperator,
    P: Preconditioner + ?Sized,
    C: FnMut(&ConvergenceState) -> bool,
{
    let n = operator.dimension();
    ensure_len(n, rhs.len())?;
    ensure_len(n, x.len())?;

    let mut residual = vec![0.0; n];
    let mut z = vec![0.0; n];
    let mut ap = vec![0.0; n];

    operator.apply(x, &mut ap);
    for i in 0..n {
        residual[i] = rhs[i] - ap[i];
    }
    let initial_residual_norm = norm(comm, &residual);
    let mut state = ConvergenceState {
        iteration: 0,
        residual_norm: initial_residual_norm,
        initial_residual_norm,
    };
    if converged(&state) {
        return Ok(IterationReport::from_state(state, true));
    }

    preconditioner.apply(&residual, &mut z);
    let mut direction = z.clone();
    let mut rz = dot(comm, &residual, &z);
    for iteration in 1..=max_iterations {
        if rz <= 0.0 {
            return Err(IterationError::IndefinitePreconditioner {
                iteration,
                value: rz,
            });
        }
        operator.apply(&direction, &mut ap);
        let curvature = dot(comm, &direction, &ap);
        if curvature <= 0.0 {
            return Err(IterationError::NonPositiveCurvature {
                iteration,
                curvature,
            });
        }

        let alpha = rz / curvature;
        for i in 0..n {
            x[i] += alpha * direction[i];
            residual[i] -= alpha * ap[i];
        }

        state = ConvergenceState {
            iteration,
            residual_norm: norm(comm, &residual),
            initial_residual_norm,
        };
        if converged(&state) {
            return Ok(IterationReport::from_state(state, true));
        }

        preconditioner.apply(&residual, &mut z);
        let rz_next = dot(comm, &residual, &z);
        let beta = rz_next / rz;
        for i in 0..n {
            direction[i] = z[i] + beta * direction[i];
        }
        rz = rz_next;
    }

    Ok(IterationReport::from_state(state, false))
}

/// Picard/Richardson fixed-point iteration for `A x = rhs`:
/// `x ← x + relaxation · (rhs − A x)`.
pub fn picard_iteration<A, C>(
    comm: &dyn CommBackend,
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
    let initial_residual_norm = norm(comm, &residual);
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
            residual_norm: norm(comm, &residual),
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
    use grass_mpi::SingleProcessComm;

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

    /// 1-D Dirichlet Laplacian `tridiag(-1, 2, -1)`: SPD, ill-conditioned.
    struct Laplace1d(usize);

    impl LinearOperator for Laplace1d {
        fn dimension(&self) -> usize {
            self.0
        }

        fn apply(&self, x: &[f64], y: &mut [f64]) {
            let n = self.0;
            for i in 0..n {
                let l = if i > 0 { x[i - 1] } else { 0.0 };
                let r = if i + 1 < n { x[i + 1] } else { 0.0 };
                y[i] = 2.0 * x[i] - l - r;
            }
        }
    }

    /// Exact inverse of `Laplace1d` as a preconditioner (Thomas algorithm).
    struct ExactLaplace1d(usize);

    impl Preconditioner for ExactLaplace1d {
        fn apply(&self, r: &[f64], z: &mut [f64]) {
            let n = self.0;
            let mut c = vec![0.0; n];
            let mut d = vec![0.0; n];
            c[0] = -0.5;
            d[0] = r[0] / 2.0;
            for i in 1..n {
                let m = 2.0 + c[i - 1];
                c[i] = -1.0 / m;
                d[i] = (r[i] + d[i - 1]) / m;
            }
            z[n - 1] = d[n - 1];
            for i in (0..n - 1).rev() {
                z[i] = d[i] - c[i] * z[i + 1];
            }
        }
    }

    /// A communicator standing in for one rank of a two-rank run whose other
    /// rank holds an exact mirror of this one: every global sum is twice the
    /// local sum.
    struct MirroredComm(SingleProcessComm);

    impl CommBackend for MirroredComm {
        fn rank(&self) -> i32 {
            0
        }
        fn size(&self) -> i32 {
            2
        }
        fn processor_decomposition(&self) -> [i32; 3] {
            [2, 1, 1]
        }
        fn processor_position(&self) -> [i32; 3] {
            [0, 0, 0]
        }
        fn set_processor_grid(&mut self, _: [i32; 3], _: [i32; 3]) {}
        fn all_reduce_sum_f64(&self, local: f64) -> f64 {
            2.0 * local
        }
        fn all_reduce_min_f64(&self, local: f64) -> f64 {
            local
        }
        fn barrier(&self) {}
        fn send_f64(&self, d: i32, b: &[f64]) {
            self.0.send_f64(d, b)
        }
        fn recv_f64(&self, s: i32) -> Vec<f64> {
            self.0.recv_f64(s)
        }
        fn recv_f64_any(&self) -> Vec<f64> {
            self.0.recv_f64_any()
        }
        fn sendrecv_f64(&self, d: i32, b: &[f64], s: i32) -> Vec<f64> {
            self.0.sendrecv_f64(d, b, s)
        }
        fn sendrecv_f64_into(&self, d: i32, b: &[f64], s: i32, r: &mut [f64]) {
            self.0.sendrecv_f64_into(d, b, s, r)
        }
        fn sendrecv_batch_f64_into(&self, ops: &mut [grass_mpi::SendRecvOp<'_>]) {
            self.0.sendrecv_batch_f64_into(ops)
        }
    }

    #[test]
    fn cg_solves_spd_diagonal_system() {
        let comm = SingleProcessComm::new();
        let op = Diagonal {
            values: vec![2.0, 4.0, 8.0],
        };
        let mut x = vec![0.0; 3];
        let report = conjugate_gradient(&comm, &op, &[2.0, 8.0, 24.0], &mut x, 10, |s| {
            s.relative_residual() < 1.0e-12
        })
        .expect("cg solve");
        assert!(report.converged);
        assert!(report.iterations <= 3);
        for (xi, want) in x.iter().zip([1.0, 2.0, 3.0]) {
            assert!((xi - want).abs() < 1.0e-12);
        }
    }

    #[test]
    fn cg_reduces_inner_products_through_the_comm() {
        let comm = MirroredComm(SingleProcessComm::new());
        let op = Diagonal {
            values: vec![2.0, 4.0, 8.0],
        };
        let rhs = [2.0, 8.0, 24.0];
        let mut x = vec![0.0; 3];
        let report = conjugate_gradient(&comm, &op, &rhs, &mut x, 10, |s| {
            s.relative_residual() < 1.0e-12
        })
        .expect("cg solve");
        assert!(report.converged);
        let local = rhs.iter().map(|v| v * v).sum::<f64>().sqrt();
        assert!((report.initial_residual_norm - 2f64.sqrt() * local).abs() < 1e-12);
        for (xi, want) in x.iter().zip([1.0, 2.0, 3.0]) {
            assert!((xi - want).abs() < 1.0e-12);
        }
    }

    #[test]
    fn exact_preconditioner_converges_in_one_iteration() {
        let comm = SingleProcessComm::new();
        let n = 50;
        let rhs: Vec<f64> = (0..n).map(|i| (i as f64 * 0.3).sin()).collect();
        let tol = |s: &ConvergenceState| s.relative_residual() < 1.0e-10;

        let mut x_cg = vec![0.0; n];
        let cg = conjugate_gradient(&comm, &Laplace1d(n), &rhs, &mut x_cg, 500, tol).unwrap();
        let mut x_pcg = vec![0.0; n];
        let pcg = preconditioned_conjugate_gradient(
            &comm,
            &Laplace1d(n),
            &ExactLaplace1d(n),
            &rhs,
            &mut x_pcg,
            500,
            tol,
        )
        .unwrap();

        assert!(cg.converged && pcg.converged);
        assert_eq!(pcg.iterations, 1);
        assert!(cg.iterations > 10);
        for (a, b) in x_cg.iter().zip(&x_pcg) {
            assert!((a - b).abs() < 1e-8);
        }
    }

    #[test]
    fn picard_uses_user_predicate() {
        let comm = SingleProcessComm::new();
        let op = Diagonal { values: vec![2.0] };
        let mut x = vec![0.0];
        let report = picard_iteration(&comm, &op, &[4.0], &mut x, 0.25, 100, |s| {
            s.residual_norm < 1.0e-10
        })
        .expect("picard solve");
        assert!(report.converged);
        assert!((x[0] - 2.0).abs() < 1.0e-10);
    }

    #[test]
    fn dimension_mismatch_is_reported() {
        let comm = SingleProcessComm::new();
        let op = Diagonal {
            values: vec![1.0, 1.0],
        };
        let mut x = vec![0.0];
        let err = conjugate_gradient(&comm, &op, &[1.0, 1.0], &mut x, 1, |_| false)
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
