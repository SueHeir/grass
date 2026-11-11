//! Two grass Apps coupled through the real preCICE library.
//!
//! Each participant is a row of lumped bodies exchanging heat with its
//! partner across a contact conductance `h`:
//!
//! ```text
//! Ca dTa/dt = −h (Ta − Tb)      (participant "A", writes Ta, reads Tb)
//! Cb dTb/dt = +h (Ta − Tb)      (participant "B", writes Tb, reads Ta)
//! ```
//!
//! Both integrate with backward Euler. Under serial-implicit coupling with
//! IQN-ILS, the converged partitioned result equals the monolithic backward
//! Euler solution and conserves energy exactly. The participants run as two
//! processes (this test binary re-run with an ignored entry point), connected
//! by preCICE sockets.
#![cfg(feature = "precice")]

use std::path::{Path, PathBuf};
use std::process::Command;

use grass_app::prelude::*;
use grass_precice::{CouplingStep, PreciceAppExt, PrecicePlugin};
use grass_scheduler::prelude::*;

const N: usize = 4;
const WINDOW: f64 = 0.1;
const END: f64 = 1.0;

fn ca(i: usize) -> f64 {
    1.0 + 0.5 * i as f64
}
fn cb(i: usize) -> f64 {
    2.0 - 0.3 * i as f64
}
fn h(i: usize) -> f64 {
    3.0 + i as f64
}
const TA0: f64 = 400.0;
const TB0: f64 = 300.0;

// ─── One participant ────────────────────────────────────────────────────────

#[derive(Clone)]
struct Bodies {
    t: Vec<f64>,
    other: Vec<f64>,
    c: Vec<f64>,
    h: Vec<f64>,
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    Step,
}

fn backward_euler(mut b: ResMut<Bodies>, step: Res<CouplingStep>) {
    let dt = step.dt;
    let b = &mut *b;
    for i in 0..N {
        b.t[i] = (b.c[i] * b.t[i] + b.h[i] * dt * b.other[i]) / (b.c[i] + b.h[i] * dt);
    }
}

fn positions() -> Vec<f64> {
    (0..N).flat_map(|i| [i as f64, 0.0]).collect()
}

fn run_participant(role: &str, config: &str, solver_dt: f64, out: &Path) {
    let (t0, c, mesh, write, read): (f64, fn(usize) -> f64, _, _, _) = if role == "A" {
        (TA0, ca, "A-Mesh", "Ta", "Tb")
    } else {
        (TB0, cb, "B-Mesh", "Tb", "Ta")
    };
    let mut app = App::new();
    app.add_resource(Bodies {
        t: vec![t0; N],
        other: vec![0.0; N],
        c: (0..N).map(c).collect(),
        h: (0..N).map(h).collect(),
    });
    app.add_plugins(PrecicePlugin::participant(role, config, 0, 1).solver_dt(solver_dt));
    app.precice_mesh::<Bodies>(mesh, |_| positions());
    app.precice_write::<Bodies>(mesh, write, |b| b.t.clone());
    app.precice_read::<Bodies>(mesh, read, |b, v| b.other.copy_from_slice(v));
    app.precice_checkpoint::<Bodies>();
    app.add_update_system(backward_euler, Phase::Step);
    app.start();

    let b = app.get_resource_ref::<Bodies>().unwrap();
    let s = app.get_resource_ref::<CouplingStep>().unwrap();
    let mut text = format!("{} {} {} {}\n", s.time, s.windows, s.advances, s.done);
    for v in &b.t {
        text.push_str(&format!("{v:.17e}\n"));
    }
    std::fs::write(out, text).unwrap();
}

/// Entry point for the participant processes.
#[test]
#[ignore = "run by the coupled tests as a child process"]
fn participant_entry() {
    let role = std::env::var("GRASS_PRECICE_ROLE").unwrap();
    let config = std::env::var("GRASS_PRECICE_CONFIG").unwrap();
    let dt: f64 = std::env::var("GRASS_PRECICE_DT").unwrap().parse().unwrap();
    let out = PathBuf::from(std::env::var("GRASS_PRECICE_OUT").unwrap());
    run_participant(&role, &config, dt, &out);
}

// ─── Driver ─────────────────────────────────────────────────────────────────

fn config(dir: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" ?>
<precice-configuration>
  <log enabled="false" />
  <data:scalar name="Ta" />
  <data:scalar name="Tb" />
  <mesh name="A-Mesh" dimensions="2">
    <use-data name="Ta" />
    <use-data name="Tb" />
  </mesh>
  <mesh name="B-Mesh" dimensions="2">
    <use-data name="Ta" />
    <use-data name="Tb" />
  </mesh>
  <participant name="A">
    <provide-mesh name="A-Mesh" />
    <receive-mesh name="B-Mesh" from="B" />
    <write-data name="Ta" mesh="A-Mesh" />
    <read-data name="Tb" mesh="A-Mesh" />
    <mapping:nearest-neighbor direction="read" from="B-Mesh" to="A-Mesh" constraint="consistent" />
  </participant>
  <participant name="B">
    <provide-mesh name="B-Mesh" />
    <receive-mesh name="A-Mesh" from="A" />
    <write-data name="Tb" mesh="B-Mesh" />
    <read-data name="Ta" mesh="B-Mesh" />
    <mapping:nearest-neighbor direction="read" from="A-Mesh" to="B-Mesh" constraint="consistent" />
  </participant>
  <m2n:sockets acceptor="A" connector="B" exchange-directory="{dir}" />
  <coupling-scheme:serial-implicit>
    <participants first="A" second="B" />
    <max-time value="{END}" />
    <time-window-size value="{WINDOW}" />
    <max-iterations value="50" />
    <exchange data="Ta" mesh="A-Mesh" from="A" to="B" initialize="true" />
    <exchange data="Tb" mesh="B-Mesh" from="B" to="A" initialize="true" />
    <relative-convergence-measure limit="1e-12" data="Tb" mesh="B-Mesh" />
    <relative-convergence-measure limit="1e-12" data="Ta" mesh="A-Mesh" />
    <acceleration:IQN-ILS>
      <data name="Tb" mesh="B-Mesh" />
      <initial-relaxation value="0.5" />
      <max-used-iterations value="20" />
      <time-windows-reused value="2" />
      <filter type="QR2" limit="1e-3" />
    </acceleration:IQN-ILS>
  </coupling-scheme:serial-implicit>
</precice-configuration>
"#,
        dir = dir.display()
    )
}

struct Outcome {
    time: f64,
    windows: u64,
    advances: u64,
    t: Vec<f64>,
}

fn parse(path: &Path) -> Outcome {
    let text = std::fs::read_to_string(path).unwrap();
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
    assert_eq!(head[3], "true", "participant did not finish the coupling");
    Outcome {
        time: head[0].parse().unwrap(),
        windows: head[1].parse().unwrap(),
        advances: head[2].parse().unwrap(),
        t: lines.map(|l| l.parse().unwrap()).collect(),
    }
}

fn couple(name: &str, dt_a: f64, dt_b: f64) -> (Outcome, Outcome) {
    let dir = std::env::temp_dir().join(format!("grass_precice_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("precice-config.xml");
    std::fs::write(&cfg, config(&dir)).unwrap();
    let exe = std::env::current_exe().unwrap();
    let spawn = |role: &str, dt: f64| {
        Command::new(&exe)
            .args(["participant_entry", "--exact", "--ignored", "--nocapture"])
            .current_dir(&dir)
            .env("GRASS_PRECICE_ROLE", role)
            .env("GRASS_PRECICE_CONFIG", &cfg)
            .env("GRASS_PRECICE_DT", dt.to_string())
            .env("GRASS_PRECICE_OUT", dir.join(format!("{role}.out")))
            .spawn()
            .unwrap()
    };
    let mut a = spawn("A", dt_a);
    let mut b = spawn("B", dt_b);
    let (sa, sb) = (a.wait().unwrap(), b.wait().unwrap());
    assert!(
        sa.success() && sb.success(),
        "participants failed: {sa} {sb}"
    );
    let out = (parse(&dir.join("A.out")), parse(&dir.join("B.out")));
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// Monolithic backward Euler for one body pair.
fn monolithic(i: usize, dt: f64, steps: usize) -> (f64, f64) {
    let (a, b, k) = (ca(i), cb(i), h(i) * dt);
    let (mut ta, mut tb) = (TA0, TB0);
    for _ in 0..steps {
        // [a + k, −k; −k, b + k] [ta'; tb'] = [a ta; b tb]
        let det = (a + k) * (b + k) - k * k;
        let (ra, rb) = (a * ta, b * tb);
        ta = ((b + k) * ra + k * rb) / det;
        tb = (k * ra + (a + k) * rb) / det;
    }
    (ta, tb)
}

#[test]
fn implicit_coupling_matches_the_monolithic_scheme() {
    let (a, b) = couple("implicit", WINDOW, WINDOW);
    let windows = (END / WINDOW).round() as u64;
    for o in [&a, &b] {
        assert!((o.time - END).abs() < 1e-9, "time {}", o.time);
        assert_eq!(o.windows, windows);
        // Implicit: some windows were repeated.
        assert!(o.advances > windows, "advances {}", o.advances);
    }
    for i in 0..N {
        let (ta, tb) = monolithic(i, WINDOW, windows as usize);
        assert!(
            (a.t[i] - ta).abs() < 1e-8,
            "body {i}: Ta {} vs {ta}",
            a.t[i]
        );
        assert!(
            (b.t[i] - tb).abs() < 1e-8,
            "body {i}: Tb {} vs {tb}",
            b.t[i]
        );
        // Energy is conserved by the converged exchange.
        let e0 = ca(i) * TA0 + cb(i) * TB0;
        let e = ca(i) * a.t[i] + cb(i) * b.t[i];
        assert!((e - e0).abs() < 1e-8 * e0, "body {i}: energy {e} vs {e0}");
    }
}

#[test]
fn a_participant_can_subcycle_inside_a_window() {
    // B takes four steps per window; preCICE interpolates Ta in time.
    let (a, b) = couple("subcycle", WINDOW, WINDOW / 4.0);
    let windows = (END / WINDOW).round() as u64;
    assert_eq!((a.windows, b.windows), (windows, windows));
    assert!(b.advances >= 4 * windows, "B advanced {} times", b.advances);
    assert!((b.time - END).abs() < 1e-9);
    // B now integrates the exchange with a finer quadrature than A, so
    // the two sides no longer see exactly the same heat and energy is only
    // conserved to the time discretization error (here about 1%). Both
    // still relax toward a common temperature.
    for i in 0..N {
        let e0 = ca(i) * TA0 + cb(i) * TB0;
        let e = ca(i) * a.t[i] + cb(i) * b.t[i];
        assert!((e - e0).abs() < 0.02 * e0, "body {i}: energy {e} vs {e0}");
        assert!(a.t[i] > b.t[i] && a.t[i] < TA0 && b.t[i] > TB0, "body {i}");
    }
}
