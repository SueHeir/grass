//! Run one case of the coupling matrix.
//!
//!     coupling_matrix case.toml               # roles as threads (or under mpirun)
//!     mpirun -np N coupling_matrix case.toml  # roles on ranks, per [topology]
//!     coupling_matrix --reference case.toml   # serial reference
//!
//! Every process prints `VALUE <component> <bits>` for the components it
//! owns and the reporting process prints `ITERS <per-window counts>`; the
//! driver puts the pieces together.

mod distributed;
mod inprocess;
mod model;

use std::sync::Arc;

use grass_multi::{CoupledRun, CoupledRunner};
use model::{Case, System};

fn load(path: &str) -> (String, Case) {
    #[derive(serde::Deserialize)]
    struct Doc {
        case: Case,
    }
    let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let doc: Doc = toml::from_str(&src).unwrap_or_else(|e| panic!("{path}: {e}"));
    (src, doc.case)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--reference") {
        let (_, case) = load(&args[1]);
        let sys = System::build(&case);
        for (i, v) in sys.reference(&case).iter().enumerate() {
            println!("VALUE {i} {:016x}", v.to_bits());
        }
        return;
    }
    if args.first().map(String::as_str) == Some("--serial") {
        let (_, case) = load(&args[1]);
        serial(&case, args.get(2).map(String::as_str) == Some("--dump"));
        return;
    }
    if args.first().map(String::as_str) == Some("--map") {
        let (_, case) = load(&args[1]);
        dump_map(&case);
        return;
    }
    if args.first().map(String::as_str) == Some("--inprocess") {
        let (_, case) = load(&args[1]);
        let sys = Arc::new(System::build(&case));
        print_output(inprocess::run(case, sys));
        return;
    }
    let (src, case) = load(&args[0]);
    let sys = Arc::new(System::build(&case));
    let mut runner = CoupledRunner::from_source(&src).unwrap_or_else(|e| panic!("{e}"));
    for r in 0..case.roles() {
        let (case, sys) = (case.clone(), sys.clone());
        runner = runner.role(&Case::role_name(r), move |ctx| {
            distributed::run_role(ctx, case, sys)
        });
    }
    let outputs = match runner.run().unwrap_or_else(|e| panic!("{e}")) {
        CoupledRun::Local(all) => all.into_iter().map(|(_, o)| o).collect(),
        CoupledRun::Split { result, .. } => vec![result],
    };
    for o in outputs {
        print_output(o);
    }
}

fn print_output(o: distributed::Output) {
    for (i, v) in &o.values {
        println!("VALUE {i} {:016x}", v.to_bits());
    }
    if o.report {
        let it: Vec<String> = o.iterations.iter().map(u32::to_string).collect();
        println!("ITERS {}", it.join(","));
        println!("UNCONVERGED {}", o.unconverged);
    }
}

/// The implicit schemes with the iteration in one process: the coupled
/// solvers replaced by the serial sweep, everything else as distributed.
fn serial(case: &Case, dump: bool) {
    use grass_multi::OuterIteration;
    let sys = System::build(case);
    let gs = case.gauss_seidel();
    let owners: Vec<usize> = if gs {
        sys.gs_owners()
    } else {
        (0..sys.roles()).collect()
    };
    let idx: Vec<usize> = owners.iter().flat_map(|&q| sys.role_range(q)).collect();
    let mut it = OuterIteration::new(
        idx.iter().map(|&i| sys.u0[i]).collect(),
        case.relaxation(),
        1e-12,
        60,
    );
    let mut u = sys.u0.clone();
    let mut counts = Vec::new();
    for w in 0..case.windows {
        let start = u.clone();
        loop {
            let mut guess = start.clone();
            for (k, &i) in idx.iter().enumerate() {
                guess[i] = it.input()[k];
            }
            u = sys.sweep(case, &start, &guess, gs);
            let out: Vec<f64> = idx.iter().map(|&i| u[i]).collect();
            if dump {
                println!("DUMP {w} in {:?} out {:?}", it.input(), out);
            }
            it.observe(&out);
            if it.done() {
                break;
            }
        }
        counts.push(it.iters().to_string());
        it.begin_window();
    }
    println!("ITERS {}", counts.join(","));
    for (i, v) in u.iter().enumerate() {
        println!("VALUE {i} {:016x}", v.to_bits());
    }
}

/// Print the affine interface map `F(v) = M v + b_w` of each window (owner
/// components only), for turning a failing case into a unit test.
fn dump_map(case: &Case) {
    let sys = System::build(case);
    let gs = case.gauss_seidel();
    let owners: Vec<usize> = if gs {
        sys.gs_owners()
    } else {
        (0..sys.roles()).collect()
    };
    let idx: Vec<usize> = owners.iter().flat_map(|&q| sys.role_range(q)).collect();
    let m = idx.len();
    let mut u = sys.u0.clone();
    for w in 0..case.windows {
        let start = u.clone();
        let f = |v: &[f64]| {
            let mut guess = start.clone();
            for (k, &i) in idx.iter().enumerate() {
                guess[i] = v[k];
            }
            let all = sys.sweep(case, &start, &guess, gs);
            (idx.iter().map(|&i| all[i]).collect::<Vec<f64>>(), all)
        };
        let b = f(&vec![0.0; m]).0;
        let mut a = vec![vec![0.0; m]; m];
        for j in 0..m {
            let mut e = vec![0.0; m];
            e[j] = 1.0;
            let col = f(&e).0;
            for i in 0..m {
                a[i][j] = col[i] - b[i];
            }
        }
        if w == 0 {
            println!("M {a:?}");
            println!(
                "X0 {:?}",
                idx.iter().map(|&i| sys.u0[i]).collect::<Vec<_>>()
            );
        }
        println!("B {b:?}");
        let lhs: Vec<Vec<f64>> = (0..m)
            .map(|i| (0..m).map(|j| f64::from(i == j) - a[i][j]).collect())
            .collect();
        let v = model::solve(lhs, b.clone());
        u = f(&v).1;
    }
}
