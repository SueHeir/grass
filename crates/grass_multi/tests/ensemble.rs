//! Ensembles of grass Apps: a Monte Carlo estimate against its exact value,
//! and (feature `mpi`) the same ensemble split over MPI ranks.

use grass_app::prelude::*;
use grass_multi::{Ensemble, Member, Summary};
use grass_scheduler::prelude::*;

/// A tiny model: dT/dt = −k T from T = 1 to t = 1, with backward Euler,
/// run as a grass App. Output: [T(1), k].
struct Decay {
    t: f64,
    k: f64,
    steps: u32,
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    Step,
}

const STEPS: u32 = 2000;

fn step(mut d: ResMut<Decay>, mut sm: ResMut<SchedulerManager>) {
    let h = 1.0 / STEPS as f64;
    d.t /= 1.0 + d.k * h;
    d.steps += 1;
    if d.steps == STEPS {
        sm.state = SchedulerState::End;
    }
}

fn model(m: Member) -> Vec<f64> {
    let k = 1.0 + m.uniform(0); // k ~ U[1, 2)
    let mut app = App::new();
    app.add_resource(Decay {
        t: 1.0,
        k,
        steps: 0,
    });
    app.add_update_system(step, Phase::Step);
    app.start();
    let d = app.get_resource_ref::<Decay>().unwrap();
    vec![d.t, k]
}

#[test]
fn monte_carlo_mean_matches_the_exact_expectation() {
    let outputs = Ensemble::new(400).seed(2026).threads(4).run(model);
    let s = Summary::of(outputs.iter().map(Vec::as_slice)).unwrap();
    // E[exp(−k)] for k ~ U[1, 2] = e^−1 − e^−2.
    let exact = (-1.0f64).exp() - (-2.0f64).exp();
    let (mean, se) = (s.mean()[0], s.std_error()[0]);
    assert!(
        (mean - exact).abs() < 3.0 * se + 1e-3,
        "mean {mean} ± {se} vs {exact}"
    );
    // Each member really ran its own App with its own parameter.
    for o in &outputs {
        assert!((o[0] - (-o[1]).exp()).abs() < 1e-3);
    }
    assert!(s.min()[1] >= 1.0 && s.max()[1] < 2.0);
}

#[cfg(feature = "mpi")]
#[test]
fn ranks_split_the_ensemble_and_gather_on_rank_zero() {
    const CHILD: &str = "GRASS_MULTI_ENSEMBLE_CHILD";
    let ensemble = Ensemble::new(23).seed(5).threads(2);
    if std::env::var_os(CHILD).is_some() {
        use grass_mpi::CommBackend;
        let comm = grass_mpi::MpiCommBackend::new(grass_mpi::get_mpi_world());
        let (rank, size) = (comm.rank() as usize, comm.size() as usize);
        let local = ensemble.run_rank(rank, size, model);
        if let Some(all) = grass_multi::gather_outputs(&comm, ensemble.len(), local) {
            let serial = ensemble.threads(1).run(model);
            assert_eq!(all, serial, "gathered outputs differ from a serial run");
        }
        comm.barrier();
        grass_mpi::finalize_mpi();
        return;
    }
    if std::process::Command::new("mpirun")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP: mpirun not found");
        return;
    }
    let status = std::process::Command::new("mpirun")
        .args(["-np", "3"])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ranks_split_the_ensemble_and_gather_on_rank_zero",
        ])
        .env(CHILD, "1")
        .env("OMPI_MCA_btl", "self,vader")
        .status()
        .unwrap();
    assert!(status.success(), "MPI ensemble failed: {status}");
}
