//! Standalone use: the Fortran ablator in a simulated arc-jet test.
//!
//! Everything about the run comes from TOML (see `arcjet.toml`): the
//! material, the facility condition, a heating stage and a cooldown stage,
//! terminal output and checkpoints. The only code here is the facility
//! model, a dozen lines that turn the arc-jet condition into the ablator's
//! boundary condition each step.
//!
//!     cargo run --release --bin arcjet -- arcjet.toml
//!     cargo run --release --bin arcjet -- --config-docs

use std::io::Write;

use grass_app::prelude::*;
use grass_io::prelude::*;
use grass_io::{CheckpointPlugin, ProvenancePlugin, StageOverrides, TermOut, TermOutSchedule};
use grass_scheduler::prelude::*;
use mock_icarus::{AblatorPlugin, Surface, SurfaceBc};
use serde::{Deserialize, Serialize};

/// `[arcjet]`: the facility condition. Stages may override it, e.g. the
/// cooldown stage sets `arcjet = { q_cw = 0.0 }`.
#[derive(Debug, Clone, Deserialize, Serialize, grass_derive::ConfigDescription)]
#[serde(deny_unknown_fields)]
#[config_description(
    section = "arcjet",
    narrative = "Arc-jet facility condition at the coupon surface."
)]
struct ArcJet {
    /// Cold-wall heat flux [W/cm2].
    q_cw: f64,
    /// Recovery enthalpy [MJ/kg].
    h_r: f64,
    /// Apply the laminar blowing correction using the ablator's last mass flux.
    #[serde(default)]
    blowing: bool,
}

impl Default for ArcJet {
    fn default() -> Self {
        Self {
            q_cw: 0.0,
            h_r: 20.0,
            blowing: true,
        }
    }
}

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Facility {
    Condition,
}

/// Runs at the start of every stage: pick up that stage's condition.
fn read_stage(stage: Res<StageOverrides>, mut aj: ResMut<ArcJet>) {
    *aj = stage.section::<ArcJet>("arcjet");
}

/// Every step, before the ablator: the facility's heat-transfer coefficient
/// with the blowing correction from the previous step's mass flux.
fn condition(aj: Res<ArcJet>, surface: Res<Surface>, mut bc: ResMut<SurfaceBc>) {
    let h_r = aj.h_r * 1e6;
    let ch0 = aj.q_cw * 1e4 / (h_r - 1004.5 * 300.0);
    for s in 0..bc.ch.len() {
        let phi = if aj.blowing && ch0 > 0.0 {
            surface.0[s].mdot() / ch0
        } else {
            0.0
        };
        let omega = if phi > 1e-8 { phi / phi.exp_m1() } else { 1.0 };
        bc.ch[s] = ch0 * omega;
        bc.h_r[s] = h_r;
    }
}

struct History(std::io::BufWriter<std::fs::File>);

fn record(
    surface: Res<Surface>,
    mut clock: ResMut<SimClock>,
    abl: Res<mock_icarus::Ablator>,
    mut out: ResMut<TermOut>,
    mut hist: ResMut<History>,
) {
    clock.time = abl.time();
    let s = surface.0[0];
    out.set("time", clock.time);
    out.set("T_wall", s.t_wall);
    out.set("T_back", s.t_back);
    out.set("recession_mm", s.recession * 1e3);
    out.set("mdot_gas", s.mdot_gas);
    out.set("q_conv_Wcm2", s.q_conv * 1e-4);
    writeln!(
        hist.0,
        "{},{},{},{},{},{},{},{}",
        clock.time, s.t_wall, s.t_back, s.recession, s.mdot_gas, s.mdot_char, s.q_conv, s.q_rad
    )
    .unwrap();
}

fn main() {
    let mut app = App::new();
    app.add_plugins(InputPlugin)
        .add_plugins(RunPlugin)
        .add_plugins(TermOutPlugin)
        .add_plugins(
            ProvenancePlugin::new("mock_icarus/arcjet", env!("CARGO_PKG_VERSION"))
                .note("ablator.f90", &mock_icarus::FORTRAN_SOURCE_HASH),
        )
        .add_plugins(CheckpointPlugin)
        .add_plugins(AblatorPlugin); // the Fortran code

    let _ = Config::load_described::<ArcJet>(&mut app);
    app.add_resource(ArcJet::default());
    app.add_setup_system(read_stage, ScheduleSetupSet::Setup);
    app.add_update_system(condition, Facility::Condition);
    app.add_update_system(record, TermOutSchedule::Compute);

    let dir = app
        .get_resource_ref::<Input>()
        .and_then(|i| i.output_dir.clone())
        .unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&dir).ok();
    let mut f = std::fs::File::create(format!("{dir}/history.csv")).unwrap();
    writeln!(
        f,
        "time,t_wall,t_back,recession,mdot_gas,mdot_char,q_conv,q_rad"
    )
    .unwrap();
    app.add_resource(History(std::io::BufWriter::new(f)));

    app.start();

    let abl = app.get_resource_ref::<mock_icarus::Ablator>().unwrap();
    let (t, beta) = abl.profile(0);
    let s = abl.surface(0);
    let mut f = std::fs::File::create(format!("{dir}/profile.csv")).unwrap();
    writeln!(f, "depth_mm,temperature,beta").unwrap();
    let n = t.len() as f64;
    for (i, (t, b)) in t.iter().zip(&beta).enumerate() {
        let z = (i as f64 + 0.5) * s.thickness / n;
        writeln!(f, "{},{},{}", z * 1e3, t, b).unwrap();
    }
    println!(
        "final: t = {:.1} s, recession {:.2} mm, char depth (beta > 0.98) {:.2} mm, back face {:.0} K",
        abl.time(),
        s.recession * 1e3,
        beta.iter().filter(|b| **b > 0.98).count() as f64 * s.thickness / beta.len() as f64 * 1e3,
        s.t_back
    );
}
