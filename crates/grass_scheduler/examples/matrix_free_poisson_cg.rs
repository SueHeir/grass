//! Matrix-free implicit Poisson solve hosted by `grass_scheduler`.
//!
//! This is a manufactured-solution check for the reusable CG/Picard scaffold in
//! `grass_scheduler::iterative`. The Poisson stencil and MMS data stay here in
//! the example; the core crate only sees a generic [`LinearOperator`] and a
//! convergence predicate.
//!
//! Run with:   `cargo run -p grass_scheduler --example matrix_free_poisson_cg`
//! Test with:  `cargo test -p grass_scheduler --example matrix_free_poisson_cg`

use grass_scheduler::prelude::*;

/// Scheduler phases for one implicit solve.
#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    /// Fill the manufactured right-hand side.
    Assemble,
    /// Solve the matrix-free linear system.
    Solve,
    /// Compare against the manufactured exact solution.
    Verify,
}

/// Interior-grid matrix-free operator for `-u_xx - u_yy` on `(0, 1)^2`.
struct Poisson2d {
    n: usize,
    h: f64,
}

impl Poisson2d {
    fn unknowns(&self) -> usize {
        self.n * self.n
    }

    fn idx(&self, i: usize, j: usize) -> usize {
        j * self.n + i
    }
}

impl LinearOperator for Poisson2d {
    fn dimension(&self) -> usize {
        self.unknowns()
    }

    fn apply(&self, x: &[f64], y: &mut [f64]) {
        let h2_inv = 1.0 / (self.h * self.h);
        for j in 0..self.n {
            for i in 0..self.n {
                let center = x[self.idx(i, j)];
                let left = if i > 0 { x[self.idx(i - 1, j)] } else { 0.0 };
                let right = if i + 1 < self.n {
                    x[self.idx(i + 1, j)]
                } else {
                    0.0
                };
                let down = if j > 0 { x[self.idx(i, j - 1)] } else { 0.0 };
                let up = if j + 1 < self.n {
                    x[self.idx(i, j + 1)]
                } else {
                    0.0
                };
                y[self.idx(i, j)] = (4.0 * center - left - right - down - up) * h2_inv;
            }
        }
    }
}

/// A single grid case and its solver buffers.
struct Case {
    operator: Poisson2d,
    rhs: Vec<f64>,
    solution: Vec<f64>,
    exact: Vec<f64>,
    report: Option<IterationReport>,
    l2_error: Option<f64>,
}

impl Case {
    fn new(n: usize) -> Self {
        let h = 1.0 / (n + 1) as f64;
        let unknowns = n * n;
        Self {
            operator: Poisson2d { n, h },
            rhs: vec![0.0; unknowns],
            solution: vec![0.0; unknowns],
            exact: vec![0.0; unknowns],
            report: None,
            l2_error: None,
        }
    }
}

/// Collection resource so the scheduler can run all refinement cases.
struct Sweep {
    cases: Vec<Case>,
    observed_order: Option<f64>,
}

fn exact_solution(x: f64, y: f64) -> f64 {
    (std::f64::consts::PI * x).sin() * (std::f64::consts::PI * y).sin()
}

fn forcing(x: f64, y: f64) -> f64 {
    2.0 * std::f64::consts::PI.powi(2) * exact_solution(x, y)
}

fn assemble(mut sweep: ResMut<Sweep>) {
    for case in &mut sweep.cases {
        for j in 0..case.operator.n {
            for i in 0..case.operator.n {
                let x = (i + 1) as f64 * case.operator.h;
                let y = (j + 1) as f64 * case.operator.h;
                let k = case.operator.idx(i, j);
                case.rhs[k] = forcing(x, y);
                case.exact[k] = exact_solution(x, y);
            }
        }
    }
}

fn solve(mut sweep: ResMut<Sweep>) {
    for case in &mut sweep.cases {
        let report = conjugate_gradient(
            &case.operator,
            &case.rhs,
            &mut case.solution,
            10_000,
            |state| state.residual_norm < 1.0e-11 || state.relative_residual() < 1.0e-12,
        )
        .expect("matrix-free CG solve");
        assert!(
            report.converged,
            "CG did not converge for n={} after {} iterations; residual={:.3e}",
            case.operator.n, report.iterations, report.residual_norm
        );
        case.report = Some(report);
    }
}

fn verify(mut sweep: ResMut<Sweep>) {
    let mut previous: Option<(f64, f64)> = None;
    let mut observed_order = None;

    for case in &mut sweep.cases {
        let squared_sum: f64 = case
            .solution
            .iter()
            .zip(&case.exact)
            .map(|(u, exact)| (u - exact).powi(2))
            .sum();
        let l2 = (squared_sum * case.operator.h * case.operator.h).sqrt();
        case.l2_error = Some(l2);

        let report = case.report.expect("solve report");
        println!(
            "[poisson] n={:3}, h={:.5}, cg_iters={:3}, residual={:.3e}, L2={:.3e}",
            case.operator.n, case.operator.h, report.iterations, report.residual_norm, l2
        );

        if let Some((h_prev, e_prev)) = previous {
            let order = (e_prev / l2).ln() / (h_prev / case.operator.h).ln();
            observed_order = Some(order);
            println!("[poisson] observed order from previous grid: {order:.3}");
        }
        previous = Some((case.operator.h, l2));
    }

    sweep.observed_order = observed_order;
}

fn build_scheduler() -> Scheduler {
    let mut scheduler = Scheduler::default();
    scheduler.add_resource(Sweep {
        cases: vec![Case::new(15), Case::new(31), Case::new(63)],
        observed_order: None,
    });
    scheduler.add_update_system(assemble, Phase::Assemble);
    scheduler.add_update_system(solve, Phase::Solve);
    scheduler.add_update_system(verify, Phase::Verify);
    scheduler.organize_systems();
    scheduler
}

fn run_sweep() -> (Vec<f64>, f64) {
    let mut scheduler = build_scheduler();
    scheduler.run();
    let sweep = scheduler
        .get_resource_ref::<Sweep>()
        .expect("sweep resource");
    let errors = sweep
        .cases
        .iter()
        .map(|case| case.l2_error.expect("verified error"))
        .collect();
    (errors, sweep.observed_order.expect("observed order"))
}

fn main() {
    let (errors, order) = run_sweep();
    assert!(
        errors.windows(2).all(|pair| pair[1] < pair[0]),
        "MMS errors did not decrease monotonically: {errors:?}"
    );
    assert!(
        order > 1.95,
        "expected second-order MMS convergence, got {order:.3}"
    );
    println!("matrix_free_poisson_cg: MMS convergence checks passed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mms_poisson_converges_at_second_order() {
        let (errors, order) = run_sweep();
        assert!(errors.windows(2).all(|pair| pair[1] < pair[0]));
        assert!(order > 1.95, "observed order {order:.3}");
    }
}
