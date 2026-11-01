//! The coupled entry of `entry.rs`, with each code as its own MPI role.
//!
//! `flow` (the C aeroheating code) and `tps` (the Fortran ablator) run as
//! separate roles placed by `[topology]`. The ablator's stations are split
//! across the `tps` ranks, and each rank runs its own Fortran instance for its
//! block. Implicit coupling uses `DistributedImplicit`: every window both
//! roles checkpoint, iterate on the blowing rate, and roll back together;
//! the IQN update runs on `tps` rank 0 over the gathered interface vector.
//!
//! The same input runs three ways, and all three give bit-identical results
//! to the in-process `entry` binary:
//!
//!     cargo run --release --features mpi --bin entry_mpi -- entry_mpi.toml          # threads
//!     mpirun -np 3 target/release/entry_mpi entry_mpi.toml                            # flow 1 + tps 2
//!     (edit [[topology.role]] ranks to change the split)

use std::any::TypeId;
use std::time::Instant;

use grass_app::prelude::*;
use grass_io::Config;
use grass_multi::{
    AppPhysics, CoupledRun, CoupledRunner, DistributedImplicit, OuterIteration, Physics,
    RoleContext, WindowAction, WindowStep,
};
use mock_icarus::{
    surface_fingerprint, Ablator, AblatorDt, AblatorPlugin, Aero, AeroDt, AeroPlugin,
    CouplingConfig, Environment, Surface, SurfaceBc, SurfaceOut, Trajectory, WallBlowing,
};

fn with<R: 'static, O>(p: &AppPhysics, f: impl FnOnce(&mut R) -> O) -> O {
    let cell = p
        .resource_cell(TypeId::of::<R>())
        .unwrap_or_else(|| panic!("missing resource {}", std::any::type_name::<R>()));
    let mut b = cell.borrow_mut();
    f(b.downcast_mut::<R>().unwrap())
}

fn to_bytes(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn from_bytes(b: &[u8]) -> Vec<f64> {
    b.chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// This rank's block of `n` stations.
fn block(n: usize, rank: i32, size: i32) -> std::ops::Range<usize> {
    let (r, s) = (rank as usize, size as usize);
    r * n / s..(r + 1) * n / s
}

fn coupling(ctx: &RoleContext) -> CouplingConfig {
    Config::from_str(ctx.config_source()).section::<CouplingConfig>("coupling")
}

fn iteration(c: &CouplingConfig, stations: usize) -> (OuterIteration, u64) {
    // explicit = one pass per window: the next window's guess is this
    // window's output, i.e. lagged blowing.
    let (relax, cap) = match c.scheme.as_str() {
        "implicit" => (c.relaxation(), c.max_iters),
        "explicit" => (grass_multi::Relaxation::Fixed(1.0), 1),
        other => panic!("[coupling] scheme = {other:?}"),
    };
    (
        OuterIteration::new(vec![0.0; stations], relax, c.tol, cap),
        (c.final_time / c.window).round() as u64,
    )
}

fn sub_app(ctx: &RoleContext, name: &str, stations: Option<usize>) -> App {
    let mut cfg = Config::from_str(ctx.config_source()).for_subapp(name, None);
    if let Some(n) = stations {
        cfg.table
            .get_mut("ablator")
            .and_then(|t| t.as_table_mut())
            .expect("[tps.ablator]")
            .insert("stations".into(), toml::Value::Integer(n as i64));
    }
    let mut app = App::new();
    app.add_resource(cfg);
    app
}

struct Report {
    role: String,
    lines: Vec<String>,
}

/// The aeroheating code: all stations, one rank.
fn flow(mut ctx: RoleContext) -> Report {
    let c = coupling(&ctx);
    let tps = ctx.exchange("tps");
    let mut app = sub_app(&ctx, "flow", None);
    app.add_plugins(AeroPlugin);
    app.add_resource(AeroDt(c.window));
    let mut flow = AppPhysics::new("flow", app);
    flow.prepare();
    let n = with(&flow, |e: &mut Environment| e.0.len());
    let (it, n_windows) = iteration(&c, n);
    let mut ic = DistributedImplicit::new(&ctx, "tps", it, c.window, n_windows);
    let mut saved = Vec::new();
    let (mut peak_q, mut t_end) = (vec![0.0f64; n], 0.0);
    let t0 = Instant::now();
    while !ic.finished() {
        match ic.begin() {
            WindowAction::Save => saved = with(&flow, |a: &mut Aero| a.save()),
            WindowAction::Restore => with(&flow, |a: &mut Aero| a.restore(&saved)),
        }
        // 1. the blowing guess, as the tps ranks' shards in station order
        let blowing: Vec<f64> = tps
            .exchange(&[])
            .unwrap()
            .iter()
            .flat_map(|s| from_bytes(s))
            .collect();
        with(&flow, |w: &mut WallBlowing| w.0.copy_from_slice(&blowing));
        flow.step();
        // 2. ch and h_r for every station; each tps rank takes its block
        let env = with(&flow, |e: &mut Environment| {
            e.0.iter().flat_map(|s| [s.ch, s.h_r]).collect::<Vec<_>>()
        });
        tps.exchange(&to_bytes(&env)).unwrap();
        let (step, _) = ic.end(None);
        if step == WindowStep::Advance {
            let traj = with(&flow, |t: &mut Trajectory| *t);
            with(&flow, |e: &mut Environment| {
                for (p, s) in peak_q.iter_mut().zip(&e.0) {
                    *p = p.max(s.q_cw);
                }
            });
            t_end = traj.time;
            ic.truncate(traj.done || traj.time >= c.final_time - 1e-9);
        }
    }
    let it = ic.iterations_per_window();
    let total: u32 = it.iter().sum();
    let coupling = if c.scheme == "explicit" {
        "explicit, one pass per window".to_string()
    } else {
        format!(
            "{total} coupling iterations ({:.2} per window), {} unconverged",
            total as f64 / it.len().max(1) as f64,
            ic.unconverged_windows().len()
        )
    };
    Report {
        role: "flow".into(),
        lines: vec![
            format!(
                "flow: {} windows to t = {t_end:.1} s, {coupling}, peak stagnation q_cw {:.1} W/cm2",
                it.len(),
                peak_q[0] / 1e4
            ),
            format!("flow: wall time {:.3} s", t0.elapsed().as_secs_f64()),
        ],
    }
}

/// The Fortran ablator: this rank's block of stations.
fn tps(mut ctx: RoleContext) -> Report {
    let c = coupling(&ctx);
    let flow_ex = ctx.exchange("flow");
    let rc = ctx.role_collective();
    let (rank, size) = rc.position();
    let total = Config::from_str(ctx.config_source())
        .for_subapp("tps", None)
        .table["ablator"]["stations"]
        .as_integer()
        .expect("[tps.ablator] stations") as usize;
    let mine = block(total, rank, size);
    let mut app = sub_app(&ctx, "tps", Some(mine.len()));
    app.add_plugins(AblatorPlugin);
    app.add_resource(AblatorDt(c.window / c.tps_substeps as f64));
    let mut tps = AppPhysics::new("tps", app);
    tps.prepare();
    let (it, n_windows) = iteration(&c, total);
    let mut ic = DistributedImplicit::new(&ctx, "tps", it, c.window, n_windows);
    let mut guess = vec![0.0; mine.len()];
    let mut saved = Vec::new();
    let mut peak_tw = vec![0.0f64; mine.len()];
    let t0 = Instant::now();
    let mut stepping = 0.0;
    while !ic.finished() {
        match ic.begin() {
            WindowAction::Save => saved = with(&tps, |a: &mut Ablator| a.save()),
            WindowAction::Restore => with(&tps, |a: &mut Ablator| a.restore(&saved)),
        }
        flow_ex.exchange(&to_bytes(&guess)).unwrap();
        let env = from_bytes(&flow_ex.exchange(&[]).unwrap()[0]);
        with(&tps, |bc: &mut SurfaceBc| {
            for (k, s) in mine.clone().enumerate() {
                bc.ch[k] = env[2 * s];
                bc.h_r[k] = env[2 * s + 1];
            }
        });
        let ts = Instant::now();
        for _ in 0..c.tps_substeps {
            tps.step();
        }
        stepping += ts.elapsed().as_secs_f64();
        let out: Vec<f64> = with(&tps, |s: &mut Surface| {
            s.0.iter().map(|o| o.mdot()).collect()
        });
        let (step, next) = ic.end(Some(&out));
        guess = next.unwrap();
        if step == WindowStep::Advance {
            with(&tps, |s: &mut Surface| {
                for (p, o) in peak_tw.iter_mut().zip(&s.0) {
                    *p = p.max(o.t_wall);
                }
            });
            ic.truncate(false);
        }
    }
    // Gather the final surface state to tps rank 0 for the report.
    let surf = with(&tps, |s: &mut Surface| s.0.clone());
    let flat: Vec<f64> = surf
        .iter()
        .zip(&peak_tw)
        .flat_map(|(o, p)| {
            [
                o.t_wall,
                o.t_back,
                o.recession,
                o.mdot_gas,
                o.mdot_char,
                o.q_conv,
                o.q_rad,
                o.thickness,
                *p,
            ]
        })
        .collect();
    let timing = [stepping, t0.elapsed().as_secs_f64()];
    let gathered = rc.gather_f64(&flat);
    let times = rc.gather_f64(&timing);
    let mut lines = Vec::new();
    if let (Some(parts), Some(times)) = (gathered, times) {
        let all: Vec<f64> = parts.concat();
        let surface: Vec<SurfaceOut> = all
            .chunks_exact(9)
            .map(|v| SurfaceOut {
                t_wall: v[0],
                t_back: v[1],
                recession: v[2],
                mdot_gas: v[3],
                mdot_char: v[4],
                q_conv: v[5],
                q_rad: v[6],
                thickness: v[7],
            })
            .collect();
        let peaks: Vec<f64> = all.chunks_exact(9).map(|v| v[8]).collect();
        lines.push(format!("tps: {total} stations on {size} rank(s)"));
        if total <= 16 {
            lines.push(" station  recession   peak T_wall".into());
            for (s, (o, p)) in surface.iter().zip(&peaks).enumerate() {
                lines.push(format!(
                    " {s:>7}  {:6.2} mm   {:6.0} K",
                    o.recession * 1e3,
                    p
                ));
            }
        }
        let max_step = times.iter().map(|t| t[0]).fold(0.0, f64::max);
        lines.push(format!(
            "tps: ablator stepping {max_step:.3} s (slowest rank), wall time {:.3} s",
            times[0][1]
        ));
        lines.push(format!(
            "final surface fingerprint {}",
            surface_fingerprint(&surface)
        ));
    }
    Report {
        role: "tps".into(),
        lines,
    }
}

const DEFAULT: &str = include_str!("../../entry_mpi.toml");

fn main() {
    let t0 = Instant::now();
    let run = CoupledRunner::from_cli_or(DEFAULT)
        .unwrap_or_else(|e| panic!("{e}"))
        .role("flow", flow)
        .role("tps", tps)
        .run()
        .unwrap_or_else(|e| panic!("{e}"));
    let reports = match run {
        CoupledRun::Local(all) => {
            println!("placement: local (one thread per role)");
            all.into_iter().map(|(_, r)| r).collect()
        }
        CoupledRun::Split { result, .. } => vec![result],
    };
    for r in reports {
        for l in r.lines {
            println!("{l}");
        }
        let _ = r.role;
    }
    if std::env::var("OMPI_COMM_WORLD_RANK").map_or(true, |r| r == "0") {
        println!("total wall time {:.3} s", t0.elapsed().as_secs_f64());
    }
}
