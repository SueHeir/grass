//! Coupled use: the Fortran ablator and the C aeroheating code flown down an
//! entry trajectory together.
//!
//! Neither code knows about the other. Each is a sub-App built from its own
//! plugin and its own slice of `entry.toml` (`[flow.*]`, `[tps.*]`). This
//! file is the coupling: it moves the heat-transfer coefficient and recovery
//! enthalpy from the flow code to the ablator, and the ablator's wall mass
//! injection back to the flow code, which uses it for the blowing correction.
//! That makes the problem two-way coupled.
//!
//! `[coupling] scheme` picks how:
//!
//! * `"explicit"`: one pass per window. The flow code sees the blowing from
//!   the end of the *previous* window (the usual loosely coupled
//!   DPLR-then-Icarus workflow).
//! * `"implicit"`: every window is iterated to a fixed point on the blowing
//!   rate. Both codes are checkpointed at the window start (their own
//!   save/restore routines) and rolled back each iteration; the update is
//!   accelerated with IQN-ILS, Aitken, or plain under-relaxation.
//!
//! In both cases the ablator subcycles `tps_substeps` steps per window.
//!
//!     cargo run --release --bin entry -- entry.toml

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use grass_app::prelude::*;
use grass_io::prelude::*;
use grass_io::ProvenancePlugin;
use grass_multi::{
    checkpoint_window, converge_window, tick_subapp, ImplicitCoupling, Multi, OuterIteration,
    WindowStep,
};
use grass_scheduler::prelude::*;
use mock_icarus::{
    Ablator, AblatorDt, AblatorPlugin, Aero, AeroDt, AeroPlugin, Environment, Surface, SurfaceBc,
    Trajectory, WallBlowing,
};

const FLOW: &str = "flow";
const TPS: &str = "tps";

use mock_icarus::CouplingConfig as Coupling;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Entry {
    Checkpoint,
    Flow,
    Couple,
    Tps,
    Converge,
    Record,
}

// ─── the data exchange ─────────────────────────────────────────────────────

/// flow → tps: heat-transfer coefficient and recovery enthalpy per station.
fn flow_to_tps(w: &Multi) {
    let env = w.expect_read::<Environment>(FLOW);
    let mut bc = w.expect_write::<SurfaceBc>(TPS);
    for (s, e) in env.0.iter().enumerate() {
        bc.ch[s] = e.ch;
        bc.h_r[s] = e.h_r;
    }
}

/// The blowing rate per station, as the ablator last computed it.
fn tps_blowing(w: &Multi) -> Vec<f64> {
    w.expect_read::<Surface>(TPS)
        .0
        .iter()
        .map(|s| s.mdot())
        .collect()
}

fn set_flow_blowing(w: &Multi, mdot: &[f64]) {
    w.expect_write::<WallBlowing>(FLOW).0.copy_from_slice(mdot);
}

// ─── output ────────────────────────────────────────────────────────────────

struct Log {
    file: std::io::BufWriter<std::fs::File>,
    recorded_windows: u64,
    peak_q: Vec<f64>,
    peak_tw: Vec<f64>,
    heat_load: Vec<f64>,
}

fn record(w: &Multi, log: &mut Log, iterations: u32, window: f64) {
    let traj = *w.expect_read::<Trajectory>(FLOW);
    let env = w.expect_read::<Environment>(FLOW);
    let surf = w.expect_read::<Surface>(TPS);
    write!(
        log.file,
        "{:.3},{:.1},{:.2},{}",
        traj.time, traj.altitude, traj.velocity, iterations
    )
    .unwrap();
    for (s, (e, o)) in env.0.iter().zip(&surf.0).enumerate() {
        write!(
            log.file,
            ",{},{},{},{},{},{},{}",
            e.q_cw,
            e.ch / e.ch0.max(1e-30),
            o.q_conv,
            o.t_wall,
            o.mdot(),
            o.recession,
            o.t_back
        )
        .unwrap();
        log.peak_q[s] = log.peak_q[s].max(e.q_cw);
        log.peak_tw[s] = log.peak_tw[s].max(o.t_wall);
        log.heat_load[s] += e.q_cw * window;
    }
    writeln!(log.file).unwrap();
    if log.recorded_windows.is_multiple_of(20) {
        let s = &surf.0[0];
        println!(
            "t {:6.1} s  alt {:6.1} km  V {:5.2} km/s  q_cw {:7.1} W/cm2  ch/ch0 {:.3}  Tw {:5.0} K  rec {:.2} mm  iters {}",
            traj.time,
            traj.altitude / 1e3,
            traj.velocity / 1e3,
            env.0[0].q_cw / 1e4,
            env.0[0].ch / env.0[0].ch0.max(1e-30),
            s.t_wall,
            s.recession * 1e3,
            iterations
        );
    }
    log.recorded_windows += 1;
}

fn stop_if_done(w: &Multi, c: &Coupling, sm: &mut SchedulerManager) {
    let traj = *w.expect_read::<Trajectory>(FLOW);
    if traj.done || traj.time >= c.final_time - 1e-9 {
        sm.state = SchedulerState::End;
    }
}

// ─── the two schemes ───────────────────────────────────────────────────────

fn explicit(parent: &mut App, c: Coupling) {
    parent.add_update_system(
        // the flow code sees the blowing from the end of the previous window
        |w: Multi| set_flow_blowing(&w, &tps_blowing(&w)),
        Entry::Flow,
    );
    parent.add_update_system(tick_subapp(FLOW, 1), Entry::Flow);
    parent.add_update_system(|w: Multi| flow_to_tps(&w), Entry::Couple);
    parent.add_update_system(tick_subapp(TPS, c.tps_substeps), Entry::Tps);
    parent.add_update_system(
        move |w: Multi, mut log: ResMut<Log>, mut sm: ResMut<SchedulerManager>| {
            record(&w, &mut log, 1, c.window);
            stop_if_done(&w, &c, &mut sm);
        },
        Entry::Record,
    );
}

fn implicit(parent: &mut App, c: Coupling, stations: usize) {
    let relax = c.relaxation();
    let n_windows = (c.final_time / c.window).round() as u64;
    parent.add_resource(ImplicitCoupling::new(
        OuterIteration::new(vec![0.0; stations], relax, c.tol, c.max_iters),
        c.window,
        n_windows,
    ));
    // Both codes save and restore their own state; GRASS only decides when.
    let saved: Rc<RefCell<(Vec<f64>, Vec<f64>)>> = Rc::default();
    let s2 = saved.clone();
    parent.add_update_system(
        checkpoint_window(
            move |w: &Multi| {
                *saved.borrow_mut() = (
                    w.expect_read::<Aero>(FLOW).save(),
                    w.expect_read::<Ablator>(TPS).save(),
                );
            },
            move |w: &Multi| {
                let s = s2.borrow();
                w.expect_write::<Aero>(FLOW).restore(&s.0);
                w.expect_write::<Ablator>(TPS).restore(&s.1);
            },
        ),
        Entry::Checkpoint,
    );
    parent.add_update_system(tick_subapp(FLOW, 1), Entry::Flow);
    parent.add_update_system(|w: Multi| flow_to_tps(&w), Entry::Couple);
    parent.add_update_system(tick_subapp(TPS, c.tps_substeps), Entry::Tps);
    parent.add_update_system(
        // The interface unknown is the blowing rate per station: read what the
        // ablator produced, hand the accelerated next iterate to the flow code
        // (for a repeat of this window, or as the first guess for the next).
        converge_window(tps_blowing, |w: &Multi, x: &[f64], _: WindowStep| {
            set_flow_blowing(w, x)
        }),
        Entry::Converge,
    );
    parent.add_update_system(
        move |w: Multi,
              ic: Res<ImplicitCoupling>,
              mut log: ResMut<Log>,
              mut sm: ResMut<SchedulerManager>| {
            if ic.window() > log.recorded_windows {
                // a window was just accepted
                let iters = *ic.iterations_per_window().last().unwrap();
                record(&w, &mut log, iters, c.window);
                stop_if_done(&w, &c, &mut sm);
            }
        },
        Entry::Record,
    );
}

// ─── main ──────────────────────────────────────────────────────────────────

fn main() {
    let mut parent = App::new();
    parent.add_plugins(InputPlugin).add_plugins(
        ProvenancePlugin::new("mock_icarus/entry", env!("CARGO_PKG_VERSION"))
            .note("ablator.f90", &mock_icarus::FORTRAN_SOURCE_HASH),
    );
    let c = Config::load_described::<Coupling>(&mut parent);

    // Two codes, each from its own plugin and its own TOML slice.
    parent.add_subapp_with_config(FLOW, |app| {
        app.add_plugins(AeroPlugin);
        app.add_resource(AeroDt(c.window)); // one flow step per window
    });
    let tps_dt = c.window / c.tps_substeps as f64;
    parent.add_subapp_with_config(TPS, |app| {
        app.add_plugins(AblatorPlugin);
        app.add_resource(AblatorDt(tps_dt)); // subcycled
    });

    // Both codes must agree on the stations.
    let stations = {
        let subs = parent.get_resource_ref::<grass_multi::SubApps>().unwrap();
        let n_flow = subs
            .find(FLOW)
            .unwrap()
            .resource_cell(std::any::TypeId::of::<Environment>())
            .unwrap()
            .borrow()
            .downcast_ref::<Environment>()
            .unwrap()
            .0
            .len();
        let n_tps = subs
            .find(TPS)
            .unwrap()
            .resource_cell(std::any::TypeId::of::<Surface>())
            .unwrap()
            .borrow()
            .downcast_ref::<Surface>()
            .unwrap()
            .0
            .len();
        assert_eq!(
            n_flow, n_tps,
            "[flow.aero] stations_deg has {n_flow} stations but [tps.ablator] stations = {n_tps}"
        );
        n_flow
    };

    let dir = parent
        .get_resource_ref::<Input>()
        .and_then(|i| i.output_dir.clone())
        .unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&dir).ok();
    let mut file =
        std::io::BufWriter::new(std::fs::File::create(format!("{dir}/history.csv")).unwrap());
    write!(file, "time,altitude,velocity,iterations").unwrap();
    for s in 0..stations {
        write!(
            file,
            ",q_cw_{s},blowing_factor_{s},q_conv_{s},t_wall_{s},mdot_{s},recession_{s},t_back_{s}"
        )
        .unwrap();
    }
    writeln!(file).unwrap();
    parent.add_resource(Log {
        file,
        recorded_windows: 0,
        peak_q: vec![0.0; stations],
        peak_tw: vec![0.0; stations],
        heat_load: vec![0.0; stations],
    });

    println!(
        "coupling: {} (window {} s, ablator {} substeps{})",
        c.scheme,
        c.window,
        c.tps_substeps,
        if c.scheme == "implicit" {
            format!(", {} to {:e}", c.acceleration, c.tol)
        } else {
            String::new()
        }
    );
    match c.scheme.as_str() {
        "explicit" => explicit(&mut parent, c.clone()),
        "implicit" => implicit(&mut parent, c.clone(), stations),
        other => panic!("[coupling] scheme = {other:?}: expected explicit or implicit"),
    }

    let t0 = std::time::Instant::now();
    parent.start();
    let wall = t0.elapsed().as_secs_f64();
    if std::env::args().any(|a| a == "--config-docs" || a == "--generate-config") {
        return;
    }

    let log = parent.get_resource_ref::<Log>().unwrap();
    let subs = parent.get_resource_ref::<grass_multi::SubApps>().unwrap();
    let surf = subs
        .find(TPS)
        .unwrap()
        .resource_cell(std::any::TypeId::of::<Surface>())
        .unwrap()
        .borrow()
        .downcast_ref::<Surface>()
        .unwrap()
        .clone();
    println!("\n station  peak q_cw   heat load   recession   peak T_wall");
    for (s, o) in surf.0.iter().enumerate() {
        println!(
            " {s:>7}  {:7.1} W/cm2  {:7.1} kJ/cm2  {:6.2} mm   {:6.0} K",
            log.peak_q[s] / 1e4,
            log.heat_load[s] / 1e7,
            o.recession * 1e3,
            log.peak_tw[s]
        );
    }
    if let Some(ic) = parent.get_resource_ref::<ImplicitCoupling>() {
        let it = ic.iterations_per_window();
        let total: u32 = it.iter().sum();
        println!(
            "\n{} windows, {} coupling iterations ({:.2} per window, max {}), {} unconverged",
            it.len(),
            total,
            total as f64 / it.len().max(1) as f64,
            it.iter().max().unwrap_or(&0),
            ic.unconverged_windows().len()
        );
    }
    println!(
        "final surface fingerprint {}",
        mock_icarus::surface_fingerprint(&surf.0)
    );
    println!("wall time {wall:.2} s, history in {dir}/history.csv");
}
