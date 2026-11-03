//! GRASS adapter for `micarus`, a legacy-style MPI Fortran material-response
//! code (see `fortran/`), plus the mock C aeroheating code it couples to.
//!
//! The adapter has the same three layers as `examples/mock_icarus`, but the
//! code underneath behaves like a real legacy code, so each layer has more
//! to do:
//!
//! 1. **C interface** (`adapter/micarus_capi.f90`): `bind(C)` wrappers over
//!    the routines the stand-alone program already calls.
//! 2. **Safe owner** ([`Micarus`]): one instance per process, ever (the code
//!    keeps its state in module variables and never frees its mesh). It is
//!    handed the GRASS communicator as a Fortran handle, rewrites the input
//!    deck's relative paths, and turns status codes into `Result`s.
//! 3. **Plugin** ([`MicarusPlugin`]): reads `[icarus]`, publishes the surface
//!    as typed resources, and registers one system that advances the code to
//!    the end of the current window with its own adaptive time step.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use grass_app::{App, ConfigDescription, Plugin};
use grass_derive::ConfigDescription as DeriveConfigDescription;
use grass_io::{Config, DescribedConfig};
use grass_scheduler::prelude::*;
use grass_scheduler::{Res, ResMut};
use serde::{Deserialize, Serialize};

mod ffi {
    use std::os::raw::{c_char, c_int};
    extern "C" {
        // adapter/micarus_capi.f90
        pub fn micarus_init(comm_f: c_int, input: *const c_char, overrides: *const c_char)
            -> c_int;
        pub fn micarus_set_error_hook(f: extern "C" fn(c_int, *const c_char));
        pub fn micarus_advance_to(t: f64) -> c_int;
        pub fn micarus_time() -> f64;
        pub fn micarus_control(out: *mut f64);
        pub fn micarus_counters(out: *mut c_int);
        pub fn micarus_n_surface() -> c_int;
        pub fn micarus_surface_geometry(
            gid: *mut c_int,
            theta: *mut f64,
            phi: *mut f64,
            area: *mut f64,
        );
        pub fn micarus_set_environment(
            ch: *const f64,
            h_r: *const f64,
            p_e: *const f64,
            q_rad: *const f64,
        ) -> c_int;
        pub fn micarus_get_surface(out: *mut f64);
        pub fn micarus_state_size() -> c_int;
        pub fn micarus_save_state(buf: *mut f64);
        pub fn micarus_restore_state(buf: *const f64);
        pub fn micarus_write_output();
        pub fn micarus_write_restart();
        pub fn micarus_finalize();

        // ../mock_icarus/c/aero.c
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

/// FNV-1a hash of every Fortran source this binary was built from.
pub static FORTRAN_SOURCE_HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let mut all = Vec::new();
    for src in [
        include_str!("../fortran/mi_globals.f90"),
        include_str!("../fortran/mi_parallel.f90"),
        include_str!("../fortran/mi_input.f90"),
        include_str!("../fortran/mi_mesh.f90"),
        include_str!("../fortran/mi_solver.f90"),
        include_str!("../fortran/mi_physics.f90"),
        include_str!("../fortran/mi_output.f90"),
        include_str!("../fortran/mi_driver.f90"),
        include_str!("../adapter/micarus_capi.f90"),
    ] {
        all.extend_from_slice(src.as_bytes());
    }
    format!("fnv1a64:{:016x}", grass_io::fnv1a64(&all))
});

// ═══════════════════════════════════════════════════════════════════════════
// Layer 2: the safe owner
// ═══════════════════════════════════════════════════════════════════════════

/// Surface faces this rank owns.
#[derive(Debug, Clone, Default)]
pub struct SurfaceFaces {
    /// Global face id (1-based), stable across rank counts.
    pub gid: Vec<i32>,
    /// Body angle from the stagnation point [rad].
    pub theta: Vec<f64>,
    /// Azimuth [rad].
    pub phi: Vec<f64>,
    /// Current face area [m2].
    pub area: Vec<f64>,
}

impl SurfaceFaces {
    pub fn len(&self) -> usize {
        self.gid.len()
    }
    pub fn is_empty(&self) -> bool {
        self.gid.is_empty()
    }
}

/// Response of one surface face.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct FaceState {
    /// Wall temperature [K].
    pub t_wall: f64,
    /// Pyrolysis-gas mass flux through the surface [kg/m2/s].
    pub mdot_gas: f64,
    /// Char mass-loss rate [kg/m2/s].
    pub mdot_char: f64,
    /// Accumulated recession [m].
    pub recession: f64,
    /// Net heat flux conducted into the material [W/m2].
    pub q_cond: f64,
    /// Heat-transfer coefficient the code used [kg/m2/s].
    pub ch: f64,
    /// B'c from the table.
    pub bc: f64,
    /// Remaining thickness [m].
    pub thickness: f64,
}

impl FaceState {
    /// Total wall mass injection, what the flow code needs.
    pub fn mdot(&self) -> f64 {
        self.mdot_gas + self.mdot_char
    }
}

/// Recoverable failures `micarus_advance_to` reports. Everything else in
/// the code is fatal and aborts the MPI job (after the error hook runs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicarusError {
    StepUnderflow,
    BurnThrough,
    Unknown(i32),
}

impl std::fmt::Display for MicarusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StepUnderflow => write!(f, "time step fell below dt_min"),
            Self::BurnThrough => write!(f, "the heatshield burned through"),
            Self::Unknown(c) => write!(f, "micarus_advance_to returned {c}"),
        }
    }
}

/// Run-control values from the input deck.
#[derive(Debug, Clone, Copy)]
pub struct Control {
    pub t_start: f64,
    pub t_end: f64,
    pub output_interval: f64,
}

/// Counters the code keeps.
#[derive(Debug, Clone, Copy)]
pub struct Counters {
    pub steps: i32,
    pub rejected: i32,
    pub picard: i32,
    pub linear: i32,
    pub ranks: i32,
    pub faces_global: i32,
}

static LIVE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_fatal(rank: c_int, msg: *const c_char) {
    let msg = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    eprintln!("[grass] micarus is aborting the job (rank {rank}): {msg}");
}

/// The process's single micarus instance.
///
/// Not `Send`, and at most one per process for the life of the process:
/// the code keeps everything in module variables and allocates its mesh
/// once. Two micarus roles therefore need two MPI processes.
pub struct Micarus {
    faces: SurfaceFaces,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Micarus {
    /// Start the code on the communicator `comm_f` (a Fortran handle, e.g.
    /// `grass_mpi::get_mpi_world_fortran_handle()`), reading `deck` and then
    /// the namelist `overrides` file, if any.
    pub fn init(comm_f: i32, deck: &Path, overrides: Option<&Path>) -> Result<Self, String> {
        if LIVE.swap(true, Ordering::SeqCst) {
            return Err("micarus can only be started once per process".into());
        }
        let deck_c = CString::new(deck.to_string_lossy().as_bytes()).unwrap();
        let ov_c = CString::new(
            overrides
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
        .unwrap();
        unsafe {
            ffi::micarus_set_error_hook(on_fatal);
            ffi::micarus_init(comm_f, deck_c.as_ptr(), ov_c.as_ptr());
        }
        let n = unsafe { ffi::micarus_n_surface() } as usize;
        let mut faces = SurfaceFaces {
            gid: vec![0; n],
            theta: vec![0.0; n],
            phi: vec![0.0; n],
            area: vec![0.0; n],
        };
        unsafe {
            ffi::micarus_surface_geometry(
                faces.gid.as_mut_ptr(),
                faces.theta.as_mut_ptr(),
                faces.phi.as_mut_ptr(),
                faces.area.as_mut_ptr(),
            )
        };
        Ok(Self {
            faces,
            _not_send: std::marker::PhantomData,
        })
    }

    pub fn faces(&self) -> &SurfaceFaces {
        &self.faces
    }

    pub fn control(&self) -> Control {
        let mut o = [0.0; 4];
        unsafe { ffi::micarus_control(o.as_mut_ptr()) };
        Control {
            t_start: o[0],
            t_end: o[1],
            output_interval: o[2],
        }
    }

    pub fn counters(&self) -> Counters {
        let mut o = [0; 6];
        unsafe { ffi::micarus_counters(o.as_mut_ptr()) };
        Counters {
            steps: o[0],
            rejected: o[1],
            picard: o[2],
            linear: o[3],
            ranks: o[4],
            faces_global: o[5],
        }
    }

    pub fn time(&self) -> f64 {
        unsafe { ffi::micarus_time() }
    }

    /// Advance to exactly `t`; the code chooses (and may cut) its own steps.
    pub fn advance_to(&mut self, t: f64) -> Result<(), MicarusError> {
        match unsafe { ffi::micarus_advance_to(t) } {
            0 => Ok(()),
            2 => Err(MicarusError::StepUnderflow),
            3 => Err(MicarusError::BurnThrough),
            c => Err(MicarusError::Unknown(c)),
        }
    }

    /// Set the surface environment (the deck must say `bc_source = 'external'`).
    pub fn set_environment(&mut self, env: &SurfaceEnv) -> Result<(), String> {
        let n = self.faces.len();
        assert!(
            env.ch.len() == n && env.h_r.len() == n && env.p_e.len() == n && env.q_rad.len() == n
        );
        match unsafe {
            ffi::micarus_set_environment(
                env.ch.as_ptr(),
                env.h_r.as_ptr(),
                env.p_e.as_ptr(),
                env.q_rad.as_ptr(),
            )
        } {
            0 => Ok(()),
            _ => Err(
                "micarus reads its environment from a table: set &boundary bc_source = 'external'"
                    .into(),
            ),
        }
    }

    pub fn surface(&self) -> Vec<FaceState> {
        let n = self.faces.len();
        let mut o = vec![0.0; 8 * n];
        unsafe { ffi::micarus_get_surface(o.as_mut_ptr()) };
        o.chunks_exact(8)
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
            .collect()
    }

    /// The code's restart record for this rank, in memory.
    pub fn save(&self) -> Vec<f64> {
        let n = unsafe { ffi::micarus_state_size() } as usize;
        let mut buf = vec![0.0; n];
        unsafe { ffi::micarus_save_state(buf.as_mut_ptr()) };
        buf
    }

    /// Collective over the code's communicator (it refreshes halos).
    pub fn restore(&mut self, buf: &[f64]) {
        let n = unsafe { ffi::micarus_state_size() } as usize;
        assert_eq!(buf.len(), n, "micarus state buffer has the wrong size");
        unsafe { ffi::micarus_restore_state(buf.as_ptr()) }
    }

    /// The code's own history, thermocouple and Tecplot surface output.
    /// Collective.
    pub fn write_output(&mut self) {
        unsafe { ffi::micarus_write_output() }
    }

    /// The code's own per-rank restart files. Collective.
    pub fn write_restart(&mut self) {
        unsafe { ffi::micarus_write_restart() }
    }
}

impl Drop for Micarus {
    fn drop(&mut self) {
        // Frees the communicator; the mesh stays allocated, so LIVE is never
        // cleared: the code cannot be started again in this process.
        unsafe { ffi::micarus_finalize() }
    }
}

// ─── the input deck ──────────────────────────────────────────────────────────

fn namelist_value(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => format!("{f:e}").replace('e', "d"),
        toml::Value::Boolean(b) => if *b { ".true." } else { ".false." }.into(),
        toml::Value::Array(a) => a.iter().map(namelist_value).collect::<Vec<_>>().join(", "),
        other => panic!("[icarus.set]: cannot write {other} as a namelist value"),
    }
}

/// The value of `key = '...'` in a namelist deck, if present.
fn deck_string(deck: &str, key: &str) -> Option<String> {
    deck.lines()
        .map(|l| l.split('!').next().unwrap_or(""))
        .flat_map(|l| l.split(','))
        .find_map(|item| {
            let (k, v) = item.split_once('=')?;
            if !k.trim().eq_ignore_ascii_case(key) {
                return None;
            }
            Some(v.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
        })
}

/// Write the override namelist that turns a stand-alone deck into a hosted
/// run: the user's `[icarus.set]` values, plus every file path made
/// absolute (the code resolves them against the process's working
/// directory, which a host does not control) and `output_dir` pointed at
/// the GRASS output directory.
pub fn write_overrides(
    deck: &Path,
    set: &toml::Table,
    output_dir: &Path,
    to: &Path,
) -> std::io::Result<()> {
    let text = std::fs::read_to_string(deck)?;
    let base = deck.parent().unwrap_or(Path::new("."));
    let mut groups: std::collections::BTreeMap<String, Vec<(String, String)>> = Default::default();
    for (group, vars) in set {
        let vars = vars.as_table().unwrap_or_else(|| {
            panic!("[icarus.set.{group}] must be a table of namelist variables")
        });
        for (k, v) in vars {
            groups
                .entry(group.clone())
                .or_default()
                .push((k.clone(), namelist_value(v)));
        }
    }
    let control = groups.entry("control".into()).or_default();
    for key in ["env_file", "bprime_file"] {
        if control.iter().any(|(k, _)| k == key) {
            continue;
        }
        if let Some(p) = deck_string(&text, key) {
            let p = if Path::new(&p).is_absolute() {
                PathBuf::from(p)
            } else {
                base.join(p)
            };
            control.push((
                key.into(),
                format!("'{}'", std::path::absolute(&p)?.display()),
            ));
        }
    }
    control.retain(|(k, _)| k != "output_dir");
    control.push((
        "output_dir".into(),
        format!("'{}'", std::path::absolute(output_dir)?.display()),
    ));
    let mut out = String::from("! written by the GRASS adapter: read after the input deck\n");
    for (g, vars) in groups {
        out.push_str(&format!("&{g}\n"));
        for (k, v) in vars {
            out.push_str(&format!("  {k} = {v}\n"));
        }
        out.push_str("/\n");
    }
    std::fs::write(to, out)
}

// ═══════════════════════════════════════════════════════════════════════════
// Layer 3: the plugin
// ═══════════════════════════════════════════════════════════════════════════

fn d_true() -> bool {
    true
}

/// `[icarus]`
#[derive(Debug, Clone, Deserialize, Serialize, DeriveConfigDescription)]
#[serde(deny_unknown_fields)]
#[config_description(
    section = "icarus",
    narrative = "micarus, a legacy MPI Fortran material-response code, run from its own namelist deck. Any namelist variable can be overridden under [icarus.set.<group>]."
)]
pub struct IcarusConfig {
    /// The code's own input deck (Fortran namelists).
    pub deck: String,
    /// Namelist overrides: `[icarus.set.boundary] bc_source = "external"`.
    #[serde(default)]
    pub set: toml::Table,
    /// Let the code write its own history/thermocouple/surface files after every window.
    #[serde(default = "d_true")]
    pub own_output: bool,
}

impl Default for IcarusConfig {
    fn default() -> Self {
        Self {
            deck: "micarus.nml".into(),
            set: Default::default(),
            own_output: true,
        }
    }
}

/// Input: the surface environment for the next window, per local face.
#[derive(Debug, Clone, Default)]
pub struct SurfaceEnv {
    /// Heat-transfer coefficient [kg/m2/s] (blowing-corrected unless the
    /// deck asks the code to correct it).
    pub ch: Vec<f64>,
    /// Recovery enthalpy [J/kg].
    pub h_r: Vec<f64>,
    /// Surface pressure [Pa].
    pub p_e: Vec<f64>,
    /// Incident radiative heat flux [W/m2].
    pub q_rad: Vec<f64>,
}

/// Output: the response of every local face after the last window.
#[derive(Debug, Clone, Default)]
pub struct SurfaceState(pub Vec<FaceState>);

/// Length of one GRASS tick in simulated time. The code subcycles inside it
/// with its own adaptive step. Defaults to the deck's `output_interval`.
#[derive(Debug, Clone, Copy)]
pub struct MicarusWindow(pub f64);

/// Where the code writes its files (set before adding the plugin to
/// override `[output] dir`).
#[derive(Debug, Clone)]
pub struct MicarusOutputDir(pub PathBuf);

/// Whether the step system lets the code write its own output.
#[derive(Debug, Clone, Copy)]
pub struct MicarusOwnOutput(pub bool);

/// Whether the environment comes from GRASS (`SurfaceEnv`) or the code's table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvSource {
    Table,
    External,
}

pub const MICARUS_NAMESPACE: u32 = 20;

#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum MicarusSchedule {
    Advance,
}

/// End of the window that starts at `time`, derived from the code's own
/// clock so a restore needs no GRASS-side bookkeeping. The same arithmetic
/// as the stand-alone program's main loop, so the step sequence is identical.
pub fn window_end(time: f64, c: &Control, window: f64) -> f64 {
    let k = ((time - c.t_start) / window + 1e-9).floor();
    (c.t_start + (k + 1.0) * window).min(c.t_end)
}

/// Wraps micarus as a GRASS plugin.
pub struct MicarusPlugin;

impl Plugin for MicarusPlugin {
    fn build(&self, app: &mut App) {
        let cfg = Config::load_described::<IcarusConfig>(app);
        let out_dir = app
            .get_resource_ref::<MicarusOutputDir>()
            .map(|d| d.0.clone())
            .or_else(|| {
                app.get_resource_ref::<grass_io::Input>()
                    .and_then(|i| i.output_dir.clone())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| "out".into());
        std::fs::create_dir_all(&out_dir).ok();
        let comm = grass_mpi::get_mpi_world_fortran_handle();
        let deck = PathBuf::from(&cfg.deck);
        // Every rank writes the same overrides; each reads its own copy.
        let rank = grass_mpi::world_rank();
        let ov = out_dir.join(format!(".grass_overrides.{rank}.nml"));
        write_overrides(&deck, &cfg.set, &out_dir, &ov)
            .unwrap_or_else(|e| panic!("[icarus] deck {}: {e}", deck.display()));
        let mi = Micarus::init(comm, &deck, Some(&ov)).unwrap_or_else(|e| panic!("[icarus]: {e}"));
        let n = mi.faces().len();
        let control = mi.control();
        let external = cfg
            .set
            .get("boundary")
            .and_then(|b| b.get("bc_source"))
            .and_then(|v| v.as_str())
            .map(|s| s == "external")
            .unwrap_or_else(|| {
                std::fs::read_to_string(&deck)
                    .ok()
                    .and_then(|t| deck_string(&t, "bc_source"))
                    .is_some_and(|s| s == "external")
            });
        app.add_resource(if external {
            EnvSource::External
        } else {
            EnvSource::Table
        });
        app.add_resource(mi.faces().clone());
        app.add_resource(SurfaceState(mi.surface()));
        app.add_resource(SurfaceEnv {
            ch: vec![0.0; n],
            h_r: vec![0.0; n],
            p_e: vec![0.0; n],
            q_rad: vec![0.0; n],
        });
        if app.get_resource_ref::<MicarusWindow>().is_none() {
            app.add_resource(MicarusWindow(control.output_interval));
        }
        app.add_resource(MicarusOwnOutput(cfg.own_output));
        app.add_resource(control);
        app.add_resource(mi);
        app.set_schedule_namespace::<MicarusSchedule>(MICARUS_NAMESPACE);
        app.add_update_system(advance_micarus, MicarusSchedule::Advance);
    }

    fn config_description(&self) -> Option<ConfigDescription> {
        Some(IcarusConfig::description())
    }

    fn schedule_labels(&self) -> Vec<&'static str> {
        vec!["MicarusSchedule::Advance"]
    }

    fn exchange_contracts(&self) -> Vec<&'static str> {
        vec![
            "in: SurfaceEnv (ch, h_r, p_e, q_rad per local face; external mode)",
            "out: SurfaceState (T_wall, mdot, recession, ... per local face)",
            "geometry: SurfaceFaces (global id, theta, phi, area per local face)",
        ]
    }
}

fn advance_micarus(
    mut mi: ResMut<Micarus>,
    source: Res<EnvSource>,
    env: Res<SurfaceEnv>,
    window: Res<MicarusWindow>,
    control: Res<Control>,
    own: Res<MicarusOwnOutput>,
    mut out: ResMut<SurfaceState>,
) {
    if *source == EnvSource::External {
        mi.set_environment(&env).unwrap();
    }
    let t = window_end(mi.time(), &control, window.0);
    if let Err(e) = mi.advance_to(t) {
        panic!("micarus: {e} at t = {:.4} s", mi.time());
    }
    out.0 = mi.surface();
    if own.0 {
        mi.write_output();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The aeroheating code (unchanged from examples/mock_icarus)
// ═══════════════════════════════════════════════════════════════════════════

/// `[aero]` section.
#[derive(Debug, Clone, Deserialize, Serialize, DeriveConfigDescription)]
#[serde(deny_unknown_fields)]
#[config_description(
    section = "aero",
    narrative = "C engineering aeroheating code: ballistic entry trajectory, Sutton-Graves stagnation heating, a laminar distribution over the nose, and a blowing correction driven by the wall mass injection."
)]
pub struct AeroConfig {
    /// Body angle of each station from the stagnation point [deg].
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
}

impl Default for AeroConfig {
    fn default() -> Self {
        Self {
            stations_deg: vec![0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0],
            nose_radius: 0.22,
            ballistic_coefficient: 60.0,
            entry_velocity: 11_500.0,
            entry_angle_deg: -8.0,
            entry_altitude: 125_000.0,
        }
    }
}

/// Aerothermal environment at one station.
#[derive(Debug, Clone, Copy, Default)]
pub struct StationEnv {
    /// Blowing-corrected heat-transfer coefficient [kg/m2/s].
    pub ch: f64,
    /// Recovery enthalpy [J/kg].
    pub h_r: f64,
    /// Heat-transfer coefficient without blowing [kg/m2/s].
    pub ch0: f64,
    /// Cold-wall heat flux without blowing [W/m2].
    pub q_cw: f64,
    /// Surface pressure [Pa].
    pub p_e: f64,
}

/// One instance of the C aeroheating code.
pub struct Aero {
    h: i32,
    theta_deg: Vec<f64>,
    done: bool,
    _not_send: std::marker::PhantomData<*const ()>,
}

/// Newtonian surface pressure plus free-stream static pressure; the same
/// formula `tools/make_env.c` writes into the one-way table.
pub fn env_pressure(rho: f64, v: f64, theta_deg: f64) -> f64 {
    let c = theta_deg.to_radians().cos();
    rho * v * v * c * c + rho * 287.0 * 250.0 + 10.0
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
            theta_deg: cfg.stations_deg.clone(),
            done: false,
            _not_send: std::marker::PhantomData,
        })
    }

    pub fn stations_deg(&self) -> &[f64] {
        &self.theta_deg
    }

    pub fn set_blowing(&mut self, station: usize, mdot: f64) {
        unsafe { ffi::aero_set_blowing(self.h, station as i32, mdot) }
    }

    /// Advance the trajectory; returns `true` once the heating pulse is over.
    pub fn step(&mut self, dt: f64) -> bool {
        self.done |= unsafe { ffi::aero_step(self.h, dt) } != 0;
        self.done
    }

    /// [time, altitude, velocity, flight-path angle, density]
    pub fn trajectory(&self) -> [f64; 5] {
        let mut o = [0.0; 5];
        unsafe { ffi::aero_trajectory(self.h, o.as_mut_ptr()) };
        o
    }

    pub fn env(&self, station: usize) -> StationEnv {
        let mut o = [0.0; 4];
        unsafe { ffi::aero_get(self.h, station as i32, o.as_mut_ptr()) };
        let t = self.trajectory();
        StationEnv {
            ch: o[0],
            h_r: o[1],
            ch0: o[2],
            q_cw: o[3],
            p_e: env_pressure(t[4], t[2], self.theta_deg[station]),
        }
    }

    pub fn save(&self) -> Vec<f64> {
        let n = unsafe { ffi::aero_state_size(self.h) } as usize;
        let mut buf = vec![0.0; n + 1];
        unsafe { ffi::aero_save_state(self.h, buf.as_mut_ptr()) };
        buf[n] = f64::from(u8::from(self.done));
        buf
    }

    pub fn restore(&mut self, buf: &[f64]) {
        unsafe { ffi::aero_restore_state(self.h, buf.as_ptr()) };
        self.done = buf[buf.len() - 1] != 0.0;
    }
}

impl Drop for Aero {
    fn drop(&mut self) {
        unsafe { ffi::aero_destroy(self.h) }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Station <-> face transfer
// ═══════════════════════════════════════════════════════════════════════════

/// Linear ("hat") interpolation from the flow code's stations, which sit at
/// body angles, to the material code's faces. `w[f] = (k, 1 - a, a)` means
/// face f takes `(1 - a)` of station k and `a` of station k + 1. Blowing goes
/// back through the transpose, weighted by face area, so the flow code sees
/// an area-weighted average of the faces each station covers.
#[derive(Debug, Clone)]
pub struct StationMap {
    weights: Vec<(usize, f64, f64)>,
    area: Vec<f64>,
    stations: usize,
}

impl StationMap {
    pub fn new(stations_deg: &[f64], faces: &SurfaceFaces) -> Self {
        let th: Vec<f64> = stations_deg.iter().map(|d| d.to_radians()).collect();
        let n = th.len();
        let weights = faces
            .theta
            .iter()
            .map(|&t| {
                if n == 1 || t <= th[0] {
                    return (0, 1.0, 0.0);
                }
                if t >= th[n - 1] {
                    return (n - 2, 0.0, 1.0);
                }
                let k = th.windows(2).position(|w| t <= w[1]).unwrap();
                let a = (t - th[k]) / (th[k + 1] - th[k]);
                (k, 1.0 - a, a)
            })
            .collect();
        Self {
            weights,
            area: faces.area.clone(),
            stations: n,
        }
    }

    /// Station values -> face values.
    pub fn to_faces(&self, station: &[f64]) -> Vec<f64> {
        self.weights
            .iter()
            .map(|&(k, w0, w1)| w0 * station[k] + if w1 > 0.0 { w1 * station[k + 1] } else { 0.0 })
            .collect()
    }

    /// This rank's partial sums for the face -> station average:
    /// `[sum(w A x) per station, sum(w A) per station]`. Sum them over ranks
    /// and divide.
    pub fn partial_sums(&self, face: &[f64]) -> Vec<f64> {
        let n = self.stations;
        let mut s = vec![0.0; 2 * n];
        for ((&(k, w0, w1), a), x) in self.weights.iter().zip(&self.area).zip(face) {
            s[k] += w0 * a * x;
            s[n + k] += w0 * a;
            if w1 > 0.0 {
                s[k + 1] += w1 * a * x;
                s[n + k + 1] += w1 * a;
            }
        }
        s
    }

    /// Finish the average from the summed partial sums.
    pub fn finish(sums: &[f64]) -> Vec<f64> {
        let n = sums.len() / 2;
        (0..n)
            .map(|k| {
                if sums[n + k] > 0.0 {
                    sums[k] / sums[n + k]
                } else {
                    0.0
                }
            })
            .collect()
    }
}

/// Hash of the exact bits of a set of face states, to check two runs agree.
pub fn fingerprint(faces: &[FaceState]) -> String {
    let mut bytes = Vec::new();
    for f in faces {
        for v in [
            f.t_wall,
            f.mdot_gas,
            f.mdot_char,
            f.recession,
            f.q_cond,
            f.ch,
            f.bc,
            f.thickness,
        ] {
            bytes.extend_from_slice(&v.to_bits().to_le_bytes());
        }
    }
    format!("{:016x}", grass_io::fnv1a64(&bytes))
}
