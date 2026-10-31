//! # grass_precice — a grass App as a preCICE participant
//!
//! [preCICE](https://precice.org) couples independent solvers (OpenFOAM,
//! CalculiX, SU2, FEniCS, your own code) through a shared XML configuration:
//! each solver is a *participant* that defines meshes, writes and reads data
//! on them, and lets preCICE do the communication, mapping, time-window
//! control and acceleration.
//!
//! This crate makes a grass [`App`] a participant with a few declarations:
//!
//! ```rust,ignore
//! use grass_precice::{CouplingStep, PreciceAppExt, PrecicePlugin};
//!
//! let mut app = App::new();
//! app.add_resource(Shell::new());
//! app.add_plugins(PrecicePlugin::participant("Solid", "precice-config.xml", 0, 1).solver_dt(1e-3));
//! app.precice_mesh::<Shell>("Solid-Mesh", |s| s.surface_positions());
//! app.precice_write::<Shell>("Solid-Mesh", "Temperature", |s| s.wall_temperature());
//! app.precice_read::<Shell>("Solid-Mesh", "Heat-Flux", |s, q| s.set_wall_flux(q));
//! app.precice_checkpoint::<Shell>();          // for implicit coupling
//! app.add_update_system(advance_shell, ShellPhase::Step);  // reads Res<CouplingStep>.dt
//! app.start();
//! ```
//!
//! The plugin drives the whole participant loop:
//!
//! * **Setup**: define every mesh, write initial data if the configuration
//!   asks for it, `initialize`, then save checkpoints and read data for the
//!   first step.
//! * **Each step**, after the solver's own phases (namespace
//!   [`PRECICE_NAMESPACE`]): write data, `advance(dt)`, restore checkpoints if
//!   preCICE asks to repeat the window, save them at the start of a new one,
//!   pick the next `dt`, and read data for it. When the coupling ends the
//!   participant is finalized and the App stops.
//!
//! The solver reads its step size from the [`CouplingStep`] resource. It is
//! the smaller of the solver's preferred step and what remains of the
//! current coupling window, so subcycling inside a window is automatic.
//!
//! A participant can be a whole multi-physics App. Solvers coupled inside a
//! grass parent App (through `grass_multi` ports, mapped ports and implicit
//! windows) can appear to preCICE as a single participant, so an internal
//! material and radiation model can couple to an external flow code while
//! still exchanging data with each other directly.
//!
//! ## Backends
//!
//! The plugin talks to preCICE through the [`CouplingInterface`] trait. With
//! the `precice` feature it is implemented for the official bindings'
//! `precice::Participant` (preCICE v3.2, found with `pkg-config`); create one
//! with [`participant`]. The trait also lets tests drive the plugin with a
//! scripted stand-in.
//!
//! ## Output and other end-of-step plugins
//!
//! Plugins in later namespaces (such as `grass_io`'s terminal output and
//! dumps) run after this plugin, so on a repeated window they see the
//! restored state. Gate them with [`step_accepted`] to only report accepted
//! steps.

#![warn(missing_docs)]

use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use grass_app::prelude::*;
use grass_scheduler::prelude::*;

/// Schedule namespace of the per-step and setup systems (after the default
/// namespace `0` that solver phases use, before `grass_io`'s output at 100).
pub const PRECICE_NAMESPACE: u32 = 50;

/// An error reported by preCICE (or a stand-in backend).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreciceError(pub String);

impl fmt::Display for PreciceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "preCICE: {}", self.0)
    }
}

impl std::error::Error for PreciceError {}

/// Result type of [`CouplingInterface`] calls.
pub type Result<T> = std::result::Result<T, PreciceError>;

/// The subset of the preCICE v3 participant API the plugin uses.
///
/// Implemented for `precice::Participant` with the `precice` feature.
/// Vertex ids are preCICE's `i32` vertex ids.
pub trait CouplingInterface {
    /// Spatial dimension of `mesh`.
    fn mesh_dimensions(&self, mesh: &str) -> Result<usize>;
    /// Components per vertex of `data` on `mesh`.
    fn data_dimensions(&self, mesh: &str, data: &str) -> Result<usize>;
    /// Define vertices from interleaved coordinates; returns their ids.
    fn set_mesh_vertices(&mut self, mesh: &str, positions: &[f64]) -> Result<Vec<i32>>;
    /// Define triangles from vertex-id triples.
    fn set_mesh_triangles(&mut self, mesh: &str, ids: &[i32]) -> Result<()>;
    /// Whether initial data must be written before `initialize`.
    fn requires_initial_data(&mut self) -> Result<bool>;
    /// Establish communication and exchange initial data.
    fn initialize(&mut self) -> Result<()>;
    /// Advance by `dt`: exchange data, map, accelerate.
    fn advance(&mut self, dt: f64) -> Result<()>;
    /// Close communication.
    fn finalize(&mut self) -> Result<()>;
    /// Whether the coupled simulation is still running.
    fn is_coupling_ongoing(&self) -> Result<bool>;
    /// Whether the last advance completed a coupling window.
    fn is_time_window_complete(&self) -> Result<bool>;
    /// Largest step that stays inside the current coupling window.
    fn max_time_step_size(&self) -> Result<f64>;
    /// Whether the solver must save its state now (start of an implicit window).
    fn requires_writing_checkpoint(&mut self) -> Result<bool>;
    /// Whether the solver must restore its saved state (window repeated).
    fn requires_reading_checkpoint(&mut self) -> Result<bool>;
    /// Write values (interleaved by data dimension) on vertices `ids`.
    fn write_data(&mut self, mesh: &str, data: &str, ids: &[i32], values: &[f64]) -> Result<()>;
    /// Read values at `dt` into the step on vertices `ids`.
    fn read_data(
        &self,
        mesh: &str,
        data: &str,
        ids: &[i32],
        dt: f64,
        values: &mut [f64],
    ) -> Result<()>;
}

#[cfg(feature = "precice")]
mod backend {
    use super::{CouplingInterface, PreciceError, Result};

    fn err(e: precice::Error) -> PreciceError {
        PreciceError(e.what().to_string())
    }

    impl CouplingInterface for precice::Participant {
        fn mesh_dimensions(&self, mesh: &str) -> Result<usize> {
            self.get_mesh_dimensions(mesh)
                .map(|d| d as usize)
                .map_err(err)
        }
        fn data_dimensions(&self, mesh: &str, data: &str) -> Result<usize> {
            self.get_data_dimensions(mesh, data)
                .map(|d| d as usize)
                .map_err(err)
        }
        fn set_mesh_vertices(&mut self, mesh: &str, positions: &[f64]) -> Result<Vec<i32>> {
            let dim = self.mesh_dimensions(mesh)?;
            let mut ids = vec![0; positions.len() / dim.max(1)];
            precice::Participant::set_mesh_vertices(self, mesh, positions, &mut ids)
                .map_err(err)?;
            Ok(ids)
        }
        fn set_mesh_triangles(&mut self, mesh: &str, ids: &[i32]) -> Result<()> {
            precice::Participant::set_mesh_triangles(self, mesh, ids).map_err(err)
        }
        fn requires_initial_data(&mut self) -> Result<bool> {
            precice::Participant::requires_initial_data(self).map_err(err)
        }
        fn initialize(&mut self) -> Result<()> {
            precice::Participant::initialize(self).map_err(err)
        }
        fn advance(&mut self, dt: f64) -> Result<()> {
            precice::Participant::advance(self, dt).map_err(err)
        }
        fn finalize(&mut self) -> Result<()> {
            precice::Participant::finalize(self).map_err(err)
        }
        fn is_coupling_ongoing(&self) -> Result<bool> {
            precice::Participant::is_coupling_ongoing(self).map_err(err)
        }
        fn is_time_window_complete(&self) -> Result<bool> {
            precice::Participant::is_time_window_complete(self).map_err(err)
        }
        fn max_time_step_size(&self) -> Result<f64> {
            self.get_max_time_step_size().map_err(err)
        }
        fn requires_writing_checkpoint(&mut self) -> Result<bool> {
            precice::Participant::requires_writing_checkpoint(self).map_err(err)
        }
        fn requires_reading_checkpoint(&mut self) -> Result<bool> {
            precice::Participant::requires_reading_checkpoint(self).map_err(err)
        }
        fn write_data(
            &mut self,
            mesh: &str,
            data: &str,
            ids: &[i32],
            values: &[f64],
        ) -> Result<()> {
            precice::Participant::write_data(self, mesh, data, ids, values).map_err(err)
        }
        fn read_data(
            &self,
            mesh: &str,
            data: &str,
            ids: &[i32],
            dt: f64,
            values: &mut [f64],
        ) -> Result<()> {
            precice::Participant::read_data(self, mesh, data, ids, dt, values).map_err(err)
        }
    }

    /// Create a preCICE participant `name` from the XML configuration at
    /// `config`, as rank `rank` of `size`.
    pub fn participant(
        name: &str,
        config: &str,
        rank: i32,
        size: i32,
    ) -> Result<Box<dyn CouplingInterface>> {
        let p = precice::Participant::new(name, config, rank, size).map_err(err)?;
        Ok(Box::new(p))
    }
}

#[cfg(feature = "precice")]
pub use backend::participant;

// ─── Resources ──────────────────────────────────────────────────────────────

/// Step control for the solver: read `dt` from here.
#[derive(Debug, Clone, PartialEq)]
pub struct CouplingStep {
    /// Step size for the next solver step: the solver's preferred step,
    /// shortened to end exactly at the coupling window boundary.
    pub dt: f64,
    /// Simulation time at the start of the next step.
    pub time: f64,
    /// Repetitions of the current window so far (0 on its first attempt).
    pub iteration: u32,
    /// Accepted coupling windows so far.
    pub windows: u64,
    /// Whether the last advance was accepted (not followed by a restore).
    pub accepted: bool,
    /// Whether the coupled simulation has ended.
    pub done: bool,
    /// Total `advance` calls, including repeated windows.
    pub advances: u64,
    solver_dt: f64,
    saved_time: f64,
}

/// Run condition: true when the step that just finished was accepted. Use it
/// to gate output that should not report repeated implicit iterations.
pub fn step_accepted(step: Res<CouplingStep>) -> bool {
    step.accepted
}

struct MeshInfo {
    ids: Vec<i32>,
}

/// The participant and its bookkeeping. Added by [`PrecicePlugin`].
pub struct Precice {
    iface: Box<dyn CouplingInterface>,
    meshes: HashMap<String, MeshInfo>,
    initial_data: Option<bool>,
    save: bool,
    restore: bool,
    done: bool,
}

impl Precice {
    /// The underlying participant, for calls the plugin does not wrap.
    pub fn interface(&mut self) -> &mut dyn CouplingInterface {
        &mut *self.iface
    }

    /// Vertex ids of `mesh`, in the order its positions were given.
    pub fn vertex_ids(&self, mesh: &str) -> Option<&[i32]> {
        self.meshes.get(mesh).map(|m| m.ids.as_slice())
    }

    fn mesh(&self, mesh: &str) -> &MeshInfo {
        self.meshes.get(mesh).unwrap_or_else(|| {
            panic!("grass_precice: mesh `{mesh}` was not defined with precice_mesh")
        })
    }

    fn initial_data_required(&mut self) -> bool {
        if self.initial_data.is_none() {
            self.initial_data = Some(ok(self.iface.requires_initial_data()));
        }
        self.initial_data == Some(true)
    }

    /// After `initialize` or `advance`: decide what the next step does.
    fn sync(&mut self, step: &mut CouplingStep) {
        self.done = !ok(self.iface.is_coupling_ongoing());
        step.done = self.done;
        if self.done {
            self.save = false;
            ok(self.iface.finalize());
            return;
        }
        self.save = ok(self.iface.requires_writing_checkpoint());
        if self.save {
            step.saved_time = step.time;
        }
        let max = ok(self.iface.max_time_step_size());
        step.dt = if step.solver_dt > 0.0 {
            step.solver_dt.min(max)
        } else {
            max
        };
    }
}

/// A saved copy of `T` for implicit coupling, registered by
/// [`PreciceAppExt::precice_checkpoint`].
pub struct PreciceCheckpoint<T>(pub Option<T>);

fn ok<T>(r: Result<T>) -> T {
    r.unwrap_or_else(|e| panic!("{e}"))
}

// ─── Schedules ──────────────────────────────────────────────────────────────

/// Setup phases (namespace [`PRECICE_NAMESPACE`]).
#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum PreciceSetup {
    /// Define meshes.
    Mesh,
    /// Write initial data, if required.
    InitialData,
    /// `initialize`.
    Initialize,
    /// Save checkpoints for the first window.
    Save,
    /// Read data for the first step.
    Read,
}

/// Per-step phases (namespace [`PRECICE_NAMESPACE`]).
#[derive(Debug, Clone, Copy, ScheduleSet)]
pub enum PreciceStep {
    /// Write data produced by this step.
    Write,
    /// `advance`.
    Advance,
    /// Restore checkpoints if the window is repeated.
    Restore,
    /// Save checkpoints at the start of a new window.
    Save,
    /// Read data for the next step.
    Read,
}

// ─── Plugin ─────────────────────────────────────────────────────────────────

/// Makes the App a preCICE participant. Register meshes, data and
/// checkpoints with [`PreciceAppExt`].
pub struct PrecicePlugin {
    factory: Box<dyn Fn() -> Result<Box<dyn CouplingInterface>> + Send + Sync>,
    solver_dt: f64,
}

impl PrecicePlugin {
    /// Drive the participant `factory` creates when the plugin is built.
    /// (A factory rather than a value because plugins must be `Send` and a
    /// preCICE participant is not.)
    pub fn new(
        factory: impl Fn() -> Result<Box<dyn CouplingInterface>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            factory: Box::new(factory),
            solver_dt: 0.0,
        }
    }

    /// Participant `name` of the preCICE configuration at `config`, as
    /// rank `rank` of `size`.
    #[cfg(feature = "precice")]
    pub fn participant(name: &str, config: &str, rank: i32, size: i32) -> Self {
        let (name, config) = (name.to_string(), config.to_string());
        Self::new(move || participant(&name, &config, rank, size))
    }

    /// The solver's preferred step size. Steps are shortened to end on
    /// coupling window boundaries. Without it every step is a whole window.
    pub fn solver_dt(mut self, dt: f64) -> Self {
        self.solver_dt = dt;
        self
    }
}

impl Plugin for PrecicePlugin {
    fn build(&self, app: &mut App) {
        let iface = ok((self.factory)());
        app.add_resource(Precice {
            iface,
            meshes: HashMap::new(),
            initial_data: None,
            save: false,
            restore: false,
            done: false,
        });
        app.add_resource(CouplingStep {
            dt: self.solver_dt,
            time: 0.0,
            iteration: 0,
            windows: 0,
            accepted: true,
            done: false,
            advances: 0,
            solver_dt: self.solver_dt,
            saved_time: 0.0,
        });
        app.set_schedule_namespace::<PreciceSetup>(PRECICE_NAMESPACE);
        app.set_schedule_namespace::<PreciceStep>(PRECICE_NAMESPACE);
        app.add_setup_system(initialize, PreciceSetup::Initialize);
        app.add_update_system(advance, PreciceStep::Advance);
    }
}

fn initialize(mut p: ResMut<Precice>, mut step: ResMut<CouplingStep>) {
    // Ask before initialize even when no data needs it, as preCICE expects.
    p.initial_data_required();
    ok(p.iface.initialize());
    p.sync(&mut step);
}

fn advance(
    mut p: ResMut<Precice>,
    mut step: ResMut<CouplingStep>,
    mut sm: ResMut<SchedulerManager>,
) {
    if p.done {
        sm.state = SchedulerState::End;
        return;
    }
    let dt = step.dt;
    ok(p.iface.advance(dt));
    step.advances += 1;
    p.restore = ok(p.iface.requires_reading_checkpoint());
    step.accepted = !p.restore;
    if p.restore {
        step.time = step.saved_time;
        step.iteration += 1;
    } else {
        step.time += dt;
        if ok(p.iface.is_time_window_complete()) {
            step.windows += 1;
            step.iteration = 0;
        }
    }
    p.sync(&mut step);
    if p.done {
        sm.state = SchedulerState::End;
    }
}

/// Registration methods on [`App`]. Call them after adding
/// [`PrecicePlugin`]; `T` is any resource of the App.
pub trait PreciceAppExt {
    /// Define `mesh` from `T`. `positions` returns interleaved vertex
    /// coordinates (as many per vertex as the mesh's dimension in the
    /// configuration). Data written and read on the mesh use the same
    /// vertex order.
    fn precice_mesh<T: 'static>(
        &mut self,
        mesh: &'static str,
        positions: impl Fn(&T) -> Vec<f64> + 'static,
    ) -> &mut Self;

    /// Like [`precice_mesh`](Self::precice_mesh), plus triangles as vertex
    /// index triples (indices into the positions), for mappings that need
    /// connectivity.
    fn precice_mesh_with_triangles<T: 'static>(
        &mut self,
        mesh: &'static str,
        positions: impl Fn(&T) -> Vec<f64> + 'static,
        triangles: impl Fn(&T) -> Vec<[usize; 3]> + 'static,
    ) -> &mut Self;

    /// Write `data` on `mesh` after every step (and as initial data when the
    /// configuration asks for it). `values` returns one entry per vertex
    /// (times the data dimension, interleaved).
    fn precice_write<T: 'static>(
        &mut self,
        mesh: &'static str,
        data: &'static str,
        values: impl Fn(&T) -> Vec<f64> + 'static,
    ) -> &mut Self;

    /// Read `data` on `mesh` before every step, at the end of the step.
    fn precice_read<T: 'static>(
        &mut self,
        mesh: &'static str,
        data: &'static str,
        apply: impl Fn(&mut T, &[f64]) + 'static,
    ) -> &mut Self;

    /// Save `T` when preCICE starts an implicit window and restore it when
    /// the window is repeated.
    fn precice_checkpoint<T: Clone + 'static>(&mut self) -> &mut Self;
}

type TrianglesFn<T> = dyn Fn(&T) -> Vec<[usize; 3]>;

fn define_mesh<T>(
    p: &mut Precice,
    t: &T,
    mesh: &str,
    positions: &dyn Fn(&T) -> Vec<f64>,
    triangles: Option<&TrianglesFn<T>>,
) {
    let dim = ok(p.iface.mesh_dimensions(mesh));
    let pos = positions(t);
    assert_eq!(
        pos.len() % dim,
        0,
        "grass_precice: mesh `{mesh}` has dimension {dim} but {} coordinates were given",
        pos.len()
    );
    let ids = ok(p.iface.set_mesh_vertices(mesh, &pos));
    if let Some(tri) = triangles {
        let flat: Vec<i32> = tri(t).iter().flatten().map(|&i| ids[i]).collect();
        ok(p.iface.set_mesh_triangles(mesh, &flat));
    }
    p.meshes.insert(mesh.to_string(), MeshInfo { ids });
}

impl PreciceAppExt for App {
    fn precice_mesh<T: 'static>(
        &mut self,
        mesh: &'static str,
        positions: impl Fn(&T) -> Vec<f64> + 'static,
    ) -> &mut Self {
        self.add_setup_system(
            move |t: Res<T>, mut p: ResMut<Precice>| {
                define_mesh(&mut p, &*t, mesh, &positions, None)
            },
            PreciceSetup::Mesh,
        )
    }

    fn precice_mesh_with_triangles<T: 'static>(
        &mut self,
        mesh: &'static str,
        positions: impl Fn(&T) -> Vec<f64> + 'static,
        triangles: impl Fn(&T) -> Vec<[usize; 3]> + 'static,
    ) -> &mut Self {
        self.add_setup_system(
            move |t: Res<T>, mut p: ResMut<Precice>| {
                define_mesh(&mut p, &*t, mesh, &positions, Some(&triangles))
            },
            PreciceSetup::Mesh,
        )
    }

    fn precice_write<T: 'static>(
        &mut self,
        mesh: &'static str,
        data: &'static str,
        values: impl Fn(&T) -> Vec<f64> + 'static,
    ) -> &mut Self {
        let values = Rc::new(values);
        let write = move |p: &mut Precice, t: &T| {
            let v = values(t);
            let ids = p.mesh(mesh).ids.clone();
            let dd = ok(p.iface.data_dimensions(mesh, data));
            assert_eq!(
                v.len(),
                ids.len() * dd,
                "grass_precice: `{data}` on `{mesh}` needs {} values, got {}",
                ids.len() * dd,
                v.len()
            );
            ok(p.iface.write_data(mesh, data, &ids, &v));
        };
        let write2 = write.clone();
        self.add_setup_system(
            move |t: Res<T>, mut p: ResMut<Precice>| {
                if p.initial_data_required() {
                    write(&mut p, &t);
                }
            },
            PreciceSetup::InitialData,
        );
        self.add_update_system(
            move |t: Res<T>, mut p: ResMut<Precice>| {
                if !p.done {
                    write2(&mut p, &t);
                }
            },
            PreciceStep::Write,
        )
    }

    fn precice_read<T: 'static>(
        &mut self,
        mesh: &'static str,
        data: &'static str,
        apply: impl Fn(&mut T, &[f64]) + 'static,
    ) -> &mut Self {
        let apply = Rc::new(apply);
        let read = move |p: &Precice, step: &CouplingStep, t: &mut T| {
            if p.done {
                return;
            }
            let m = p.mesh(mesh);
            let dd = ok(p.iface.data_dimensions(mesh, data));
            let mut buf = vec![0.0; m.ids.len() * dd];
            ok(p.iface.read_data(mesh, data, &m.ids, step.dt, &mut buf));
            apply(t, &buf);
        };
        let read2 = read.clone();
        self.add_setup_system(
            move |mut t: ResMut<T>, p: Res<Precice>, step: Res<CouplingStep>| {
                read(&p, &step, &mut t)
            },
            PreciceSetup::Read,
        );
        self.add_update_system(
            move |mut t: ResMut<T>, p: Res<Precice>, step: Res<CouplingStep>| {
                read2(&p, &step, &mut t)
            },
            PreciceStep::Read,
        )
    }

    fn precice_checkpoint<T: Clone + 'static>(&mut self) -> &mut Self {
        self.add_resource(PreciceCheckpoint::<T>(None));
        let save = |t: Res<T>, mut c: ResMut<PreciceCheckpoint<T>>, p: Res<Precice>| {
            if p.save {
                c.0 = Some(t.clone());
            }
        };
        self.add_setup_system(save, PreciceSetup::Save);
        self.add_update_system(
            |mut t: ResMut<T>, c: Res<PreciceCheckpoint<T>>, p: Res<Precice>| {
                if p.restore {
                    *t = c
                        .0
                        .clone()
                        .expect("grass_precice: preCICE asked to restore before any checkpoint");
                }
            },
            PreciceStep::Restore,
        );
        self.add_update_system(save, PreciceStep::Save)
    }
}
