//! The legacy MPI Fortran code coupled two-way to the aeroheating code down
//! an entry trajectory, each code on its own MPI ranks.
//!
//! * `flow` (the C aeroheating code, one rank) owns the trajectory and the
//!   blowing correction.
//! * `tps` (micarus, any number of ranks) runs its own domain decomposition
//!   on the role communicator GRASS hands it, with its environment set
//!   from GRASS (`bc_source = 'external'`) and its internal blowing
//!   correction switched off, so the correction is applied exactly once.
//!
//! The interface unknown is the wall blowing rate on every surface face,
//! sharded over the tps ranks as the code itself distributes them. Faces go
//! to stations through an area-weighted average (each tps rank sends its
//! partial sums), stations go to faces by linear interpolation in body angle.
//!
//! `[coupling] scheme = "explicit"` is one pass per window (lagged blowing);
//! `"implicit"` iterates each window to a fixed point, rolling both codes
//! back with their own state records.
//!
//!     mpirun -np 3 target/release/entry3d entry3d.toml    # flow 1 + tps 2

use std::any::TypeId;
use std::io::Write;
use std::time::Instant;

use grass_app::prelude::*;
use grass_io::Config;
use grass_multi::{
    AppPhysics, CoupledRun, CoupledRunner, DistributedImplicit, OuterIteration, Physics,
    Relaxation, RoleContext, WindowAction, WindowStep,
};
use icarus_legacy::{
    fingerprint, Aero, AeroConfig, FaceState, Micarus, MicarusOutputDir, MicarusOwnOutput,
    MicarusPlugin, MicarusWindow, StationMap, SurfaceEnv, SurfaceFaces, SurfaceState,
};
use serde::{Deserialize, Serialize};

/// `[coupling]`
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Coupling {
    /// "explicit" or "implicit".
    scheme: String,
    /// Coupling window [s]; the flow code takes one step per window and
    /// micarus subcycles inside it with its own adaptive step.
    window: f64,
    /// Stop after this much flight time [s].
    final_time: f64,
    /// Implicit only: "iqn", "aitken" or "picard".
    #[serde(default = "d_accel")]
    acceleration: String,
    /// Implicit only: tolerance on the face blowing-rate residual [kg/m2/s].
    #[serde(default = "d_tol")]
    tol: f64,
    #[serde(default = "d_iters")]
    max_iters: u32,
}

fn d_accel() -> String {
    "iqn".into()
}
fn d_tol() -> f64 {
    1e-5
}
fn d_iters() -> u32 {
    30
}

impl Default for Coupling {
    fn default() -> Self {
        Self {
            scheme: "implicit".into(),
            window: 0.5,
            final_time: 80.0,
            acceleration: d_accel(),
            tol: d_tol(),
            max_iters: d_iters(),
        }
    }
}

impl Coupling {
    fn iteration(&self, n: usize) -> (OuterIteration, u64) {
        let (relax, cap) = match self.scheme.as_str() {
            "explicit" => (Relaxation::Fixed(1.0), 1),
            "implicit" => (
                match self.acceleration.as_str() {
                    "iqn" => Relaxation::IqnIls {
                        omega0: 0.5,
                        reuse: 8,
                        filter: 1e-2,
                    },
                    "aitken" => Relaxation::Aitken { omega0: 0.5 },
                    "picard" => Relaxation::Fixed(1.0),
                    o => panic!("[coupling] acceleration = {o:?}"),
                },
                self.max_iters,
            ),
            o => panic!("[coupling] scheme = {o:?}: expected explicit or implicit"),
        };
        (
            OuterIteration::new(vec![0.0; n], relax, self.tol, cap),
            (self.final_time / self.window).round() as u64,
        )
    }
}

fn to_bytes(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn from_bytes(b: &[u8]) -> Vec<f64> {
    b.chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn with<R: 'static, O>(p: &AppPhysics, f: impl FnOnce(&mut R) -> O) -> O {
    let cell = p
        .resource_cell(TypeId::of::<R>())
        .unwrap_or_else(|| panic!("missing resource {}", std::any::type_name::<R>()));
    let mut b = cell.borrow_mut();
    f(b.downcast_mut::<R>().unwrap())
}

struct Setup {
    coupling: Coupling,
    aero: AeroConfig,
    out_dir: String,
}

fn setup(ctx: &RoleContext) -> Setup {
    let cfg = Config::from_str(ctx.config_source());
    let out_dir = cfg
        .table
        .get("output")
        .and_then(|o| o.get("dir"))
        .and_then(|d| d.as_str())
        .unwrap_or("out/entry3d")
        .to_string();
    std::fs::create_dir_all(&out_dir).ok();
    Setup {
        coupling: cfg.section::<Coupling>("coupling"),
        aero: cfg.for_subapp("flow", None).section::<AeroConfig>("aero"),
        out_dir,
    }
}

struct Report(Vec<String>);

// ─── flow: the C aeroheating code, one rank ─────────────────────────────────

fn flow(mut ctx: RoleContext) -> Report {
    let s = setup(&ctx);
    let c = &s.coupling;
    let tps = ctx.exchange("tps");
    let mut aero = Aero::new(&s.aero).unwrap();
    let ns = s.aero.stations_deg.len();
    let (it, n_windows) = c.iteration(0);
    let mut ic = DistributedImplicit::new(&ctx, "tps", it, c.window, n_windows);
    let mut hist = std::io::BufWriter::new(
        std::fs::File::create(format!("{}/flow_history.csv", s.out_dir)).unwrap(),
    );
    write!(hist, "time,altitude,velocity,iterations").unwrap();
    for k in 0..ns {
        write!(hist, ",q_cw_{k},blowing_factor_{k},mdot_{k}").unwrap();
    }
    writeln!(hist).unwrap();
    let mut saved = Vec::new();
    let mut mdot = vec![0.0; ns];
    let mut peak_q = 0.0f64;
    while !ic.finished() {
        match ic.begin() {
            WindowAction::Save => saved = aero.save(),
            WindowAction::Restore => aero.restore(&saved),
        }
        // 1. blowing: sum every tps rank's partial sums, then average
        let shards = tps.exchange(&[]).unwrap();
        let mut sums = vec![0.0; 2 * ns];
        for sh in &shards {
            for (a, b) in sums.iter_mut().zip(from_bytes(sh)) {
                *a += b;
            }
        }
        mdot = StationMap::finish(&sums);
        for (k, m) in mdot.iter().enumerate() {
            aero.set_blowing(k, *m);
        }
        let done = aero.step(c.window);
        // 2. environment per station: ch (blowing-corrected), h_r, p_e, q_rad
        let env: Vec<f64> = (0..ns)
            .flat_map(|k| {
                let e = aero.env(k);
                [e.ch, e.h_r, e.p_e, 0.0]
            })
            .collect();
        tps.exchange(&to_bytes(&env)).unwrap();
        let (step, _) = ic.end(None);
        if step == WindowStep::Advance {
            let t = aero.trajectory();
            write!(
                hist,
                "{:.3},{:.1},{:.2},{}",
                t[0],
                t[1],
                t[2],
                ic.iterations_per_window().last().unwrap()
            )
            .unwrap();
            for (k, m) in mdot.iter().enumerate() {
                let e = aero.env(k);
                write!(hist, ",{},{},{}", e.q_cw, e.ch / e.ch0.max(1e-30), m).unwrap();
            }
            writeln!(hist).unwrap();
            peak_q = peak_q.max(aero.env(0).q_cw);
            ic.truncate(done || t[0] >= c.final_time - 1e-9);
        }
    }
    let it = ic.iterations_per_window();
    let total: u32 = it.iter().sum();
    Report(vec![format!(
        "flow: {} windows, {total} coupling iterations ({:.2} per window, max {}), {} unconverged, peak stagnation q_cw {:.1} W/cm2",
        it.len(),
        total as f64 / it.len().max(1) as f64,
        it.iter().max().unwrap_or(&0),
        ic.unconverged_windows().len(),
        peak_q / 1e4
    )])
}

// ─── tps: micarus on its own ranks ──────────────────────────────────────────

fn tps(mut ctx: RoleContext) -> Report {
    let s = setup(&ctx);
    let c = &s.coupling;
    let flow_ex = ctx.exchange("flow");
    let rc = ctx.role_collective();
    let (rank, size) = rc.position();

    // The plugin, built from [tps.*], exactly as in a one-process App.
    let mut app = App::new();
    app.add_resource(Config::from_str(ctx.config_source()).for_subapp("tps", None));
    app.add_resource(MicarusOutputDir(s.out_dir.clone().into()));
    app.add_resource(MicarusWindow(c.window));
    app.add_plugins(MicarusPlugin);
    // Output only for accepted windows, not for every coupling iteration.
    app.add_resource(MicarusOwnOutput(false));
    let mut tps = AppPhysics::new("tps", app);
    tps.prepare();

    let faces = with(&tps, |f: &mut SurfaceFaces| f.clone());
    let map = StationMap::new(&s.aero.stations_deg, &faces);
    let ns = s.aero.stations_deg.len();
    // The accelerator on tps rank 0 works on the whole interface vector, so
    // it is sized by the global face count, not this rank's share.
    let n_global = with(&tps, |m: &mut Micarus| m.counters().faces_global) as usize;
    let (it, n_windows) = c.iteration(n_global);
    let mut ic = DistributedImplicit::new(&ctx, "tps", it, c.window, n_windows);
    let mut guess = vec![0.0; faces.len()];
    let mut saved = Vec::new();
    let mut peak_tw = vec![0.0f64; faces.len()];
    let (t0, mut stepping) = (Instant::now(), 0.0);
    while !ic.finished() {
        match ic.begin() {
            WindowAction::Save => saved = with(&tps, |m: &mut Micarus| m.save()),
            WindowAction::Restore => with(&tps, |m: &mut Micarus| m.restore(&saved)),
        }
        flow_ex
            .exchange(&to_bytes(&map.partial_sums(&guess)))
            .unwrap();
        let env = from_bytes(&flow_ex.exchange(&[]).unwrap()[0]);
        let col = |j: usize| (0..ns).map(|k| env[4 * k + j]).collect::<Vec<_>>();
        with(&tps, |e: &mut SurfaceEnv| {
            e.ch = map.to_faces(&col(0));
            e.h_r = map.to_faces(&col(1));
            e.p_e = map.to_faces(&col(2));
            e.q_rad = map.to_faces(&col(3));
        });
        let ts = Instant::now();
        tps.step(); // micarus advances to the end of the window
        stepping += ts.elapsed().as_secs_f64();
        let out: Vec<f64> = with(&tps, |s: &mut SurfaceState| {
            s.0.iter().map(FaceState::mdot).collect()
        });
        let (step, next) = ic.end(Some(&out));
        guess = next.unwrap();
        if step == WindowStep::Advance {
            with(&tps, |m: &mut Micarus| m.write_output());
            with(&tps, |s: &mut SurfaceState| {
                for (p, f) in peak_tw.iter_mut().zip(&s.0) {
                    *p = p.max(f.t_wall);
                }
            });
            ic.truncate(false);
        }
    }

    // Report from tps rank 0: gather the final surface in global face order.
    let surf = with(&tps, |s: &mut SurfaceState| s.0.clone());
    let counters = with(&tps, |m: &mut Micarus| m.counters());
    let flat: Vec<f64> = surf
        .iter()
        .zip(&peak_tw)
        .flat_map(|(f, p)| {
            [
                f.t_wall,
                f.mdot_gas,
                f.mdot_char,
                f.recession,
                f.q_cond,
                f.ch,
                f.bc,
                f.thickness,
                *p,
            ]
        })
        .collect();
    let gathered = rc.gather_f64(&flat);
    let times = rc.gather_f64(&[stepping]);
    let mut lines = Vec::new();
    if let (Some(parts), Some(times)) = (gathered, times) {
        let all: Vec<f64> = parts.concat();
        let states: Vec<FaceState> = all
            .chunks_exact(9)
            .map(|v| FaceState {
                t_wall: v[0],
                mdot_gas: v[1],
                mdot_char: v[2],
                recession: v[3],
                q_cond: v[4],
                ch: v[5],
                bc: v[6],
                thickness: v[7],
            })
            .collect();
        let peaks: Vec<f64> = all.chunks_exact(9).map(|v| v[8]).collect();
        let max_rec = states.iter().map(|f| f.recession).fold(0.0, f64::max);
        lines.push(format!(
            "tps: micarus on {size} rank(s): {} faces, {} steps, {} rejected, {} linear iterations (rank {rank} counts)",
            counters.faces_global, counters.steps, counters.rejected, counters.linear
        ));
        lines.push(format!(
            "tps: stagnation recession {:.3} mm, peak T_wall {:.0} K; max recession {:.3} mm",
            states[0].recession * 1e3,
            peaks[0],
            max_rec * 1e3
        ));
        lines.push(format!(
            "tps: micarus stepping {:.2} s (slowest rank), role wall time {:.2} s",
            times.iter().map(|t| t[0]).fold(0.0, f64::max),
            t0.elapsed().as_secs_f64()
        ));
        lines.push(format!(
            "final surface fingerprint {}",
            fingerprint(&states)
        ));
    }
    Report(lines)
}

const DEFAULT: &str = include_str!("../../entry3d.toml");

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
        for l in r.0 {
            println!("{l}");
        }
    }
    if std::env::var("OMPI_COMM_WORLD_RANK").map_or(true, |r| r == "0") {
        println!("total wall time {:.2} s", t0.elapsed().as_secs_f64());
    }
}
