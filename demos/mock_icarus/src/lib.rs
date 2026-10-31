//! GRASS wrappers for two "legacy" codes that know nothing about GRASS:
//!
//! * `fortran/ablator.f90`: a 1D pyrolyzing-ablator material-response code
//!   (a stand-in for something like Icarus), exposed through `bind(C)`.
//! * `c/aero.c`: an engineering aeroheating code (trajectory + Sutton-Graves
//!   + blowing correction), exposed as plain C.
//!
//! Each code gets three layers, and only the last one is GRASS:
//!
//! 1. `ffi`: the raw `extern "C"` declarations, one line per Fortran/C entry
//!    point.
//! 2. A safe owner type ([`Ablator`], [`Aero`]): creates the instance, frees
//!    it on drop, turns status codes into `Result`s.
//! 3. A plugin ([`AblatorPlugin`], [`AeroPlugin`]): reads a TOML section,
//!    publishes the inputs and outputs as typed resources, and registers one
//!    step system. That is all a coupling layer, a checkpoint plugin or an
//!    output plugin ever sees.

use grass_app::{App, ConfigDescription, Plugin};
use grass_derive::ConfigDescription as DeriveConfigDescription;
use grass_io::{CheckpointAppExt, Config, DescribedConfig};
use grass_scheduler::prelude::*;
use grass_scheduler::{Res, ResMut};
use serde::{Deserialize, Serialize};

/// FNV-1a hash of the Fortran source this binary was built from, for
/// provenance records.
pub static FORTRAN_SOURCE_HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "fnv1a64:{:016x}",
        grass_io::fnv1a64(include_bytes!("../fortran/ablator.f90"))
    )
});

mod ffi {
    use std::os::raw::{c_char, c_int};
    extern "C" {
        // fortran/ablator.f90 (module mock_icarus, bind(C) entry points)
        pub fn icarus_create(nstations: c_int, ncells: c_int, thickness: f64, t0: f64) -> c_int;
        pub fn icarus_destroy(h: c_int);
        pub fn icarus_set_material(h: c_int, name: *const c_char, value: f64) -> c_int;
        pub fn icarus_set_bc(h: c_int, s: c_int, ch: f64, h_r: f64);
        pub fn icarus_step(h: c_int, dt: f64) -> c_int;
        pub fn icarus_get(h: c_int, s: c_int, out: *mut f64);
        pub fn icarus_get_profile(h: c_int, s: c_int, t: *mut f64, beta: *mut f64);
        pub fn icarus_time(h: c_int) -> f64;
        pub fn icarus_state_size(h: c_int) -> c_int;
        pub fn icarus_save_state(h: c_int, buf: *mut f64);
        pub fn icarus_restore_state(h: c_int, buf: *const f64);

        // c/aero.c
        pub fn aero_create(
            n: c_int,
            theta_deg: *const f64,
            nose_radius: f64,
            ballistic_coef: f64,
            v0: f64,
            gamma0_deg: f64,
            h0: f64,
        ) -> c_int;
        pub fn aero_destroy(h: c_int);
        pub fn aero_set_blowing(h: c_int, s: c_int, mdot: f64);
        pub fn aero_step(h: c_int, dt: f64) -> c_int;
        pub fn aero_get(h: c_int, s: c_int, out: *mut f64);
        pub fn aero_trajectory(h: c_int, out: *mut f64);
        pub fn aero_state_size(h: c_int) -> c_int;
        pub fn aero_save_state(h: c_int, buf: *mut f64);
        pub fn aero_restore_state(h: c_int, buf: *const f64);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Layer 2: safe owners
// ═══════════════════════════════════════════════════════════════════════════

/// Surface state of one ablator station after a step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SurfaceOut {
    /// Wall temperature [K].
    pub t_wall: f64,
    /// Back-face (bondline) temperature [K].
    pub t_back: f64,
    /// Accumulated recession [m].
    pub recession: f64,
    /// Pyrolysis-gas mass flux leaving the surface [kg/m2/s].
    pub mdot_gas: f64,
    /// Char mass-loss rate [kg/m2/s].
    pub mdot_char: f64,
    /// Convective heat flux into the wall [W/m2].
    pub q_conv: f64,
    /// Re-radiated heat flux [W/m2].
    pub q_rad: f64,
    /// Remaining thickness [m].
    pub thickness: f64,
}

impl SurfaceOut {
    /// Total wall mass injection, the quantity the aeroheating code needs.
    pub fn mdot(&self) -> f64 {
        self.mdot_gas + self.mdot_char
    }
}

/// Error codes returned by `icarus_step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AblatorError {
    BadHandle,
    NonFinite,
    BurnThrough,
    Unknown(i32),
}

impl std::fmt::Display for AblatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadHandle => write!(f, "invalid ablator handle"),
            Self::NonFinite => write!(f, "temperature became non-finite"),
            Self::BurnThrough => write!(f, "the ablator burned through"),
            Self::Unknown(c) => write!(f, "icarus_step returned {c}"),
        }
    }
}

/// One instance of the Fortran ablator. Owns the Fortran-side state and
/// frees it on drop.
///
/// Not `Send`: the Fortran code keeps its instances in module (global) state
/// with no locking, like most legacy codes. GRASS runs a sub-App's systems
/// on one thread, so that is all it needs.
pub struct Ablator {
    h: i32,
    stations: usize,
    cells: usize,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Ablator {
    pub fn new(stations: usize, cells: usize, thickness: f64, t0: f64) -> Result<Self, String> {
        let h = unsafe { ffi::icarus_create(stations as i32, cells as i32, thickness, t0) };
        if h < 1 {
            return Err(format!(
                "icarus_create({stations}, {cells}, {thickness}, {t0}) failed"
            ));
        }
        Ok(Self {
            h,
            stations,
            cells,
            _not_send: std::marker::PhantomData,
        })
    }

    pub fn set_material(&mut self, name: &str, value: f64) -> Result<(), String> {
        let c = std::ffi::CString::new(name).unwrap();
        match unsafe { ffi::icarus_set_material(self.h, c.as_ptr(), value) } {
            0 => Ok(()),
            _ => Err(format!("unknown ablator material property `{name}`")),
        }
    }

    pub fn stations(&self) -> usize {
        self.stations
    }

    pub fn cells(&self) -> usize {
        self.cells
    }

    pub fn set_bc(&mut self, station: usize, ch: f64, h_r: f64) {
        unsafe { ffi::icarus_set_bc(self.h, station as i32 + 1, ch, h_r) }
    }

    pub fn step(&mut self, dt: f64) -> Result<(), AblatorError> {
        match unsafe { ffi::icarus_step(self.h, dt) } {
            0 => Ok(()),
            1 => Err(AblatorError::BadHandle),
            2 => Err(AblatorError::NonFinite),
            3 => Err(AblatorError::BurnThrough),
            c => Err(AblatorError::Unknown(c)),
        }
    }

    pub fn surface(&self, station: usize) -> SurfaceOut {
        let mut o = [0.0; 8];
        unsafe { ffi::icarus_get(self.h, station as i32 + 1, o.as_mut_ptr()) };
        SurfaceOut {
            t_wall: o[0],
            t_back: o[1],
            recession: o[2],
            mdot_gas: o[3],
            mdot_char: o[4],
            q_conv: o[5],
            q_rad: o[6],
            thickness: o[7],
        }
    }

    /// Temperature and decomposition-extent profiles, surface first.
    pub fn profile(&self, station: usize) -> (Vec<f64>, Vec<f64>) {
        let (mut t, mut b) = (vec![0.0; self.cells], vec![0.0; self.cells]);
        unsafe {
            ffi::icarus_get_profile(self.h, station as i32 + 1, t.as_mut_ptr(), b.as_mut_ptr())
        };
        (t, b)
    }

    pub fn time(&self) -> f64 {
        unsafe { ffi::icarus_time(self.h) }
    }

    /// The Fortran code's complete state as a flat buffer.
    pub fn save(&self) -> Vec<f64> {
        let n = unsafe { ffi::icarus_state_size(self.h) } as usize;
        let mut buf = vec![0.0; n];
        unsafe { ffi::icarus_save_state(self.h, buf.as_mut_ptr()) };
        buf
    }

    pub fn restore(&mut self, buf: &[f64]) {
        let n = unsafe { ffi::icarus_state_size(self.h) } as usize;
        assert_eq!(buf.len(), n, "ablator restart buffer has the wrong size");
        unsafe { ffi::icarus_restore_state(self.h, buf.as_ptr()) }
    }
}

impl Drop for Ablator {
    fn drop(&mut self) {
        unsafe { ffi::icarus_destroy(self.h) }
    }
}

/// Aerothermal environment at one station.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct StationEnv {
    /// Blowing-corrected heat-transfer coefficient rho_e u_e C_H [kg/m2/s].
    pub ch: f64,
    /// Recovery enthalpy [J/kg].
    pub h_r: f64,
    /// Heat-transfer coefficient without blowing [kg/m2/s].
    pub ch0: f64,
    /// Cold-wall heat flux without blowing [W/m2].
    pub q_cw: f64,
}

/// Vehicle state along the trajectory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Trajectory {
    pub time: f64,
    pub altitude: f64,
    pub velocity: f64,
    pub flight_path_deg: f64,
    pub density: f64,
    /// Set once the heating pulse is over (below 5 km or 300 m/s).
    pub done: bool,
}

/// One instance of the C aeroheating code.
pub struct Aero {
    h: i32,
    stations: usize,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Aero {
    pub fn new(cfg: &AeroConfig) -> Result<Self, String> {
        let h = unsafe {
            ffi::aero_create(
                cfg.stations_deg.len() as i32,
                cfg.stations_deg.as_ptr(),
                cfg.nose_radius,
                cfg.ballistic_coefficient,
                cfg.entry_velocity,
                cfg.entry_angle_deg,
                cfg.entry_altitude,
            )
        };
        if h < 1 {
            return Err("aero_create failed".into());
        }
        Ok(Self {
            h,
            stations: cfg.stations_deg.len(),
            _not_send: std::marker::PhantomData,
        })
    }

    pub fn stations(&self) -> usize {
        self.stations
    }

    pub fn set_blowing(&mut self, station: usize, mdot: f64) {
        unsafe { ffi::aero_set_blowing(self.h, station as i32, mdot) }
    }

    /// Returns `true` when the heating pulse is over.
    pub fn step(&mut self, dt: f64) -> bool {
        unsafe { ffi::aero_step(self.h, dt) != 0 }
    }

    pub fn env(&self, station: usize) -> StationEnv {
        let mut o = [0.0; 4];
        unsafe { ffi::aero_get(self.h, station as i32, o.as_mut_ptr()) };
        StationEnv {
            ch: o[0],
            h_r: o[1],
            ch0: o[2],
            q_cw: o[3],
        }
    }

    pub fn trajectory(&self) -> Trajectory {
        let mut o = [0.0; 5];
        unsafe { ffi::aero_trajectory(self.h, o.as_mut_ptr()) };
        Trajectory {
            time: o[0],
            altitude: o[1],
            velocity: o[2],
            flight_path_deg: o[3],
            density: o[4],
            done: false,
        }
    }

    pub fn save(&self) -> Vec<f64> {
        let n = unsafe { ffi::aero_state_size(self.h) } as usize;
        let mut buf = vec![0.0; n];
        unsafe { ffi::aero_save_state(self.h, buf.as_mut_ptr()) };
        buf
    }

    pub fn restore(&mut self, buf: &[f64]) {
        unsafe { ffi::aero_restore_state(self.h, buf.as_ptr()) }
    }
}

impl Drop for Aero {
    fn drop(&mut self) {
        unsafe { ffi::aero_destroy(self.h) }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Layer 3: GRASS plugins
// ═══════════════════════════════════════════════════════════════════════════

fn d_one() -> usize {
    1
}
fn d_cells() -> usize {
    60
}
fn d_t0() -> f64 {
    300.0
}

/// `[ablator]` section.
#[derive(Debug, Clone, Deserialize, Serialize, DeriveConfigDescription)]
#[serde(deny_unknown_fields)]
#[config_description(
    section = "ablator",
    narrative = "Fortran 1D pyrolyzing-ablator material response (mock Icarus). Material properties default to a PICA-like ablator; any of them may be overridden here."
)]
pub struct AblatorConfig {
    /// Number of independent 1D columns (heatshield stations).
    #[serde(default = "d_one")]
    pub stations: usize,
    /// Finite-volume cells per column.
    #[serde(default = "d_cells")]
    pub cells: usize,
    /// Initial ablator thickness [m].
    pub thickness: f64,
    /// Initial (uniform) temperature [K].
    #[serde(default = "d_t0")]
    pub initial_temperature: f64,
    /// Material-response time step [s]. A coupling parent may change it.
    pub dt: f64,
    /// Virgin density [kg/m3].
    pub rho_virgin: Option<f64>,
    /// Char density [kg/m3].
    pub rho_char: Option<f64>,
    /// Virgin conductivity [W/m/K].
    pub k_virgin: Option<f64>,
    /// Char conductivity [W/m/K].
    pub k_char: Option<f64>,
    /// Surface emissivity.
    pub emissivity: Option<f64>,
    /// Diffusion-limited char oxidation B'c.
    pub bc_max: Option<f64>,
}

impl Default for AblatorConfig {
    fn default() -> Self {
        Self {
            stations: 1,
            cells: 60,
            thickness: 0.05,
            initial_temperature: 300.0,
            dt: 0.05,
            rho_virgin: None,
            rho_char: None,
            k_virgin: None,
            k_char: None,
            emissivity: None,
            bc_max: None,
        }
    }
}

/// Time step the ablator takes on its next tick [s].
#[derive(Debug, Clone, Copy)]
pub struct AblatorDt(pub f64);

/// Input: surface boundary condition per station.
#[derive(Debug, Clone, Default)]
pub struct SurfaceBc {
    /// Heat-transfer coefficient rho_e u_e C_H [kg/m2/s].
    pub ch: Vec<f64>,
    /// Recovery enthalpy [J/kg].
    pub h_r: Vec<f64>,
}

/// Output: surface state per station after the last step.
#[derive(Debug, Clone, Default)]
pub struct Surface(pub Vec<SurfaceOut>);

/// The Fortran state mirrored into a serializable resource after every
/// step, so `grass_io`'s CheckpointPlugin can save it like any other
/// resource. A copy that came back from a restart file is pushed into the
/// Fortran code before the next step.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AblatorRestart {
    pub state: Vec<f64>,
    #[serde(skip)]
    in_sync: bool,
}

/// Schedule namespace of the ablator step. Systems in the default namespace
/// (0) run before it, so a boundary-condition system there feeds this step;
/// `grass_io`'s output plugins (100+) run after it.
pub const ABLATOR_NAMESPACE: u32 = 20;

#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum AblatorSchedule {
    Step,
}

/// Wraps the Fortran ablator as a GRASS plugin.
pub struct AblatorPlugin;

impl Plugin for AblatorPlugin {
    fn build(&self, app: &mut App) {
        let cfg = Config::load_described::<AblatorConfig>(app);
        let mut abl = Ablator::new(
            cfg.stations,
            cfg.cells,
            cfg.thickness,
            cfg.initial_temperature,
        )
        .unwrap_or_else(|e| panic!("[ablator]: {e}"));
        for (name, v) in [
            ("rho_virgin", cfg.rho_virgin),
            ("rho_char", cfg.rho_char),
            ("k_virgin", cfg.k_virgin),
            ("k_char", cfg.k_char),
            ("emissivity", cfg.emissivity),
            ("bc_max", cfg.bc_max),
        ] {
            if let Some(v) = v {
                abl.set_material(name, v).unwrap();
            }
        }
        let n = cfg.stations;
        app.add_resource(Surface((0..n).map(|s| abl.surface(s)).collect()));
        app.add_resource(SurfaceBc {
            ch: vec![0.0; n],
            h_r: vec![0.0; n],
        });
        app.add_resource(AblatorDt(cfg.dt));
        app.add_resource(AblatorRestart::default());
        app.add_resource(abl);
        app.set_schedule_namespace::<AblatorSchedule>(ABLATOR_NAMESPACE);
        app.add_update_system(step_ablator, AblatorSchedule::Step);
        if app.get_resource_ref::<grass_io::Checkpoints>().is_some() {
            app.checkpoint::<AblatorRestart>("ablator");
        }
    }

    fn config_description(&self) -> Option<ConfigDescription> {
        Some(AblatorConfig::description())
    }

    fn schedule_labels(&self) -> Vec<&'static str> {
        vec!["AblatorSchedule::Step"]
    }

    fn exchange_contracts(&self) -> Vec<&'static str> {
        vec![
            "in: SurfaceBc (ch, h_r per station)",
            "out: Surface (T_wall, mdot, recession, ... per station)",
        ]
    }
}

fn step_ablator(
    mut abl: ResMut<Ablator>,
    bc: Res<SurfaceBc>,
    dt: Res<AblatorDt>,
    mut surface: ResMut<Surface>,
    mut restart: ResMut<AblatorRestart>,
) {
    if !restart.in_sync && !restart.state.is_empty() {
        abl.restore(&restart.state);
    }
    for s in 0..abl.stations() {
        abl.set_bc(s, bc.ch[s], bc.h_r[s]);
    }
    if let Err(e) = abl.step(dt.0) {
        panic!("mock_icarus: {e} at t = {:.3} s", abl.time());
    }
    for (s, out) in surface.0.iter_mut().enumerate() {
        *out = abl.surface(s);
    }
    restart.state = abl.save();
    restart.in_sync = true;
}

// ─── aero ──────────────────────────────────────────────────────────────────

fn d_stations() -> Vec<f64> {
    vec![0.0]
}

/// `[aero]` section.
#[derive(Debug, Clone, Deserialize, Serialize, DeriveConfigDescription)]
#[serde(deny_unknown_fields)]
#[config_description(
    section = "aero",
    narrative = "C engineering aeroheating code: ballistic entry trajectory, Sutton-Graves stagnation heating, a laminar distribution over the nose, and a blowing correction driven by the wall mass injection."
)]
pub struct AeroConfig {
    /// Body angle of each station from the stagnation point [deg].
    #[serde(default = "d_stations")]
    pub stations_deg: Vec<f64>,
    /// Nose radius [m].
    pub nose_radius: f64,
    /// Ballistic coefficient m / (Cd A) [kg/m2].
    pub ballistic_coefficient: f64,
    /// Entry-interface velocity [m/s].
    pub entry_velocity: f64,
    /// Entry flight-path angle [deg], negative downwards.
    pub entry_angle_deg: f64,
    /// Entry-interface altitude [m].
    pub entry_altitude: f64,
    /// Trajectory time step [s]. A coupling parent may change it.
    pub dt: f64,
}

impl Default for AeroConfig {
    fn default() -> Self {
        Self {
            stations_deg: vec![0.0],
            nose_radius: 0.22,
            ballistic_coefficient: 60.0,
            entry_velocity: 11_000.0,
            entry_angle_deg: -8.0,
            entry_altitude: 125_000.0,
            dt: 0.1,
        }
    }
}

/// Time step the aero code takes on its next tick [s].
#[derive(Debug, Clone, Copy)]
pub struct AeroDt(pub f64);

/// Input: wall mass injection per station [kg/m2/s].
#[derive(Debug, Clone, Default)]
pub struct WallBlowing(pub Vec<f64>);

/// Output: environment per station.
#[derive(Debug, Clone, Default)]
pub struct Environment(pub Vec<StationEnv>);

/// Schedule namespace of the aero step (before the ablator's).
pub const AERO_NAMESPACE: u32 = 10;

#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum AeroSchedule {
    Step,
}

/// Wraps the C aeroheating code as a GRASS plugin.
pub struct AeroPlugin;

impl Plugin for AeroPlugin {
    fn build(&self, app: &mut App) {
        let cfg = Config::load_described::<AeroConfig>(app);
        let aero = Aero::new(&cfg).unwrap_or_else(|e| panic!("[aero]: {e}"));
        let n = aero.stations();
        app.add_resource(Environment((0..n).map(|s| aero.env(s)).collect()));
        app.add_resource(aero.trajectory());
        app.add_resource(WallBlowing(vec![0.0; n]));
        app.add_resource(AeroDt(cfg.dt));
        app.add_resource(aero);
        app.set_schedule_namespace::<AeroSchedule>(AERO_NAMESPACE);
        app.add_update_system(step_aero, AeroSchedule::Step);
    }

    fn config_description(&self) -> Option<ConfigDescription> {
        Some(AeroConfig::description())
    }

    fn exchange_contracts(&self) -> Vec<&'static str> {
        vec![
            "in: WallBlowing (mdot per station)",
            "out: Environment (ch, h_r per station), Trajectory",
        ]
    }
}

fn step_aero(
    mut aero: ResMut<Aero>,
    blowing: Res<WallBlowing>,
    dt: Res<AeroDt>,
    mut env: ResMut<Environment>,
    mut traj: ResMut<Trajectory>,
) {
    for (s, m) in blowing.0.iter().enumerate() {
        aero.set_blowing(s, *m);
    }
    let done = aero.step(dt.0);
    for (s, e) in env.0.iter_mut().enumerate() {
        *e = aero.env(s);
    }
    *traj = Trajectory {
        done,
        ..aero.trajectory()
    };
}

// ─── coupling configuration (shared by the in-process and MPI drivers) ──────

/// `[coupling]`
#[derive(Debug, Clone, Deserialize, Serialize, DeriveConfigDescription)]
#[serde(deny_unknown_fields)]
#[config_description(
    section = "coupling",
    narrative = "How the flow code and the ablator are coupled along the trajectory."
)]
pub struct CouplingConfig {
    /// "explicit" (one pass per window, lagged blowing) or "implicit" (iterate each window).
    pub scheme: String,
    /// Coupling window [s]. The flow code takes one step per window.
    pub window: f64,
    /// Ablator steps per window.
    pub tps_substeps: usize,
    /// Stop after this much flight time [s] (or when the heating pulse ends).
    pub final_time: f64,
    /// Implicit only: "iqn", "aitken" or "picard".
    #[serde(default = "d_accel")]
    pub acceleration: String,
    /// Implicit only: tolerance on the blowing-rate residual [kg/m2/s].
    #[serde(default = "d_tol")]
    pub tol: f64,
    /// Implicit only: iteration cap per window.
    #[serde(default = "d_iters")]
    pub max_iters: u32,
}

fn d_accel() -> String {
    "iqn".into()
}
fn d_tol() -> f64 {
    1e-6
}
fn d_iters() -> u32 {
    30
}

impl Default for CouplingConfig {
    fn default() -> Self {
        Self {
            scheme: "implicit".into(),
            window: 0.5,
            tps_substeps: 5,
            final_time: 60.0,
            acceleration: d_accel(),
            tol: d_tol(),
            max_iters: d_iters(),
        }
    }
}

impl CouplingConfig {
    /// The interface acceleration named by `acceleration`.
    pub fn relaxation(&self) -> grass_multi::Relaxation {
        use grass_multi::Relaxation;
        match self.acceleration.as_str() {
            // A looser QR filter than the library default: with 1e-8 and
            // reuse of old windows, IQN stalls in the early, pyrolysis-dominated
            // windows of this problem.
            "iqn" => Relaxation::IqnIls {
                omega0: 0.5,
                reuse: 8,
                filter: 1e-2,
            },
            "aitken" => Relaxation::Aitken { omega0: 0.5 },
            "picard" => Relaxation::Fixed(1.0),
            other => panic!("[coupling] acceleration = {other:?}: expected iqn, aitken or picard"),
        }
    }
}

/// A hash of the exact bits of every station's final surface state, to
/// check that two runs (in-process, threads, MPI) produced identical results.
pub fn surface_fingerprint(surface: &[SurfaceOut]) -> String {
    let mut bytes = Vec::new();
    for s in surface {
        for v in [
            s.t_wall,
            s.t_back,
            s.recession,
            s.mdot_gas,
            s.mdot_char,
            s.q_conv,
            s.q_rad,
            s.thickness,
        ] {
            bytes.extend_from_slice(&v.to_bits().to_le_bytes());
        }
    }
    format!("{:016x}", grass_io::fnv1a64(&bytes))
}
