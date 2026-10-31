//! Building and running GRASS Apps from Python.
//!
//! * Resources are Python objects stored by name in the App (a dict).
//! * Systems are Python callables; their parameter names say which
//!   resources they receive. The special name `sim` receives a [`Sim`]
//!   handle (step, time, `stop()`, and `tick()` for parents); `res` receives
//!   all of the App's resources (for plugins that pick names at run time).
//! * Phases are named strings, ordered by declaration.
//! * Plugins are Python objects with `build(app)`, or the Rust plugins in
//!   `grass.plugins` (Run, Checkpoint, Provenance).
//! * Another App can be added as a child (`add_app`) and ticked from the
//!   parent's schedule (`tick`) or from a parent system (`sim.tick`). The
//!   child's resources are visible to parent systems under the child's name.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use grass_app::prelude::App as RustApp;
use grass_io::{
    CheckpointAppExt, CheckpointPlugin, Config, Input, ProvenancePlugin, RunPlugin, SimClock,
};
use grass_multi::{tick_subapp, MultiAppExt, SubApps};
use grass_scheduler::prelude::*;
use grass_scheduler::{SchedulerState, System, SystemDescriptor};
use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyTuple};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ─── Phases ─────────────────────────────────────────────────────────────────

/// A phase named from Python. Ordered by declaration within the App.
#[derive(Clone, Copy, Debug)]
struct PyPhase {
    index: u32,
    name: &'static str,
}

impl ScheduleSet for PyPhase {
    fn to_index(&self) -> u32 {
        self.index
    }
    fn name(&self) -> &'static str {
        self.name
    }
}

/// Runs after all Python phases, before grass_io's run control (1000).
#[derive(Clone, Copy, Debug, ScheduleSet)]
enum ChildDone {
    Check,
}
const CHILD_DONE_NAMESPACE: u32 = 990;

/// Restores checkpointed Python resources after grass_io loads a restart (1100).
#[derive(Clone, Copy, Debug, ScheduleSet)]
enum PyRestore {
    Apply,
}
const PY_RESTORE_NAMESPACE: u32 = 1150;

// ─── Resources ──────────────────────────────────────────────────────────────

type ErrorSlot = Rc<RefCell<Option<PyErr>>>;
type Keys = Rc<RefCell<Vec<Py<PyAny>>>>;

/// How a resource key reads in messages: a class by its name, a string as itself.
fn key_label(k: &Bound<'_, PyAny>) -> String {
    if let Ok(s) = k.extract::<String>() {
        return format!("'{s}'");
    }
    k.getattr("__name__")
        .and_then(|n| n.extract::<String>())
        .unwrap_or_else(|_| k.to_string())
}

/// The Python resources of one App.
struct PyWorld {
    dict: Py<PyDict>,
    error: ErrorSlot,
    checkpointed: Keys,
}

/// Checkpointed Python resources, pickled when grass_io writes a checkpoint.
struct PySnapshot {
    dict: Option<Py<PyDict>>,
    names: Keys,
    restored: Option<Vec<u8>>,
}

impl Serialize for PySnapshot {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let bytes = Python::with_gil(|py| -> PyResult<Vec<u8>> {
            let src = self
                .dict
                .as_ref()
                .ok_or_else(|| PyRuntimeError::new_err("snapshot not attached"))?
                .bind(py);
            let out = PyDict::new(py);
            for k in self.names.borrow().iter() {
                let k = k.bind(py);
                let v = src.get_item(k)?.ok_or_else(|| {
                    PyKeyError::new_err(format!("checkpointed resource {}", key_label(k)))
                })?;
                out.set_item(k, v)?;
            }
            let b = py.import("pickle")?.call_method1("dumps", (out,))?;
            Ok(b.downcast::<PyBytes>()?.as_bytes().to_vec())
        })
        .map_err(|e| serde::ser::Error::custom(e.to_string()))?;
        bytes.serialize(s)
    }
}

impl<'de> Deserialize<'de> for PySnapshot {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self {
            dict: None,
            names: Rc::default(),
            restored: Some(Vec::<u8>::deserialize(d)?),
        })
    }
}

fn py_restore(mut snap: ResMut<PySnapshot>, world: Res<PyWorld>) {
    Python::with_gil(|py| {
        if let Some(bytes) = snap.restored.take() {
            let restored = py
                .import("pickle")
                .and_then(|p| p.call_method1("loads", (PyBytes::new(py, &bytes),)))
                .and_then(|d| world.dict.bind(py).call_method1("update", (d,)))
                .map(|_| ());
            if let Err(e) = restored {
                panic!("grass: restoring Python resources: {e}");
            }
        }
        snap.dict = Some(world.dict.clone_ref(py));
        snap.names = world.checkpointed.clone();
    });
}

// ─── Sim handle ─────────────────────────────────────────────────────────────

/// Passed to a system that has a parameter named `sim`.
#[pyclass(unsendable, module = "grass")]
pub struct Sim {
    #[pyo3(get)]
    step: u64,
    #[pyo3(get, set)]
    time: f64,
    stopped: bool,
    subapps: *mut SubApps,
}

#[pymethods]
impl Sim {
    /// End the run after this step.
    fn stop(&mut self) {
        self.stopped = true;
    }

    /// Advance child App `name` by up to `n` steps now (parent systems
    /// only). Stops early if the child ends; returns the steps taken.
    #[pyo3(signature = (child, n = 1))]
    fn tick(&mut self, child: &Bound<'_, PyAny>, n: usize) -> PyResult<usize> {
        let name = child_name(child)?;
        let name = name.as_str();
        if self.subapps.is_null() {
            return Err(PyRuntimeError::new_err(
                "sim.tick: this App has no child Apps (or the call is outside its system)",
            ));
        }
        // SAFETY: the pointer is set from a live RefMut<SubApps> held for the
        // whole duration of the system call and cleared before it is dropped.
        let subs = unsafe { &mut *self.subapps };
        if !subs.participants().any(|p| p == name) {
            return Err(PyKeyError::new_err(format!("no child App '{name}'")));
        }
        for k in 0..n {
            if subs.find(name).is_some_and(|p| p.is_done()) {
                return Ok(k);
            }
            subs.tick(name);
        }
        Ok(n)
    }
}

/// A child App's resources, seen from a parent system. Subclass it to
/// make a typed marker for a child: `class Hot(grass.AppView): pass`.
#[pyclass(unsendable, subclass, module = "grass")]
pub struct AppView {
    name: String,
    dict: Py<PyDict>,
}

#[pymethods]
impl AppView {
    fn __getattr__(&self, py: Python<'_>, key: &str) -> PyResult<PyObject> {
        self.dict
            .bind(py)
            .get_item(key)?
            .map(|v| v.unbind())
            .ok_or_else(|| {
                pyo3::exceptions::PyAttributeError::new_err(format!(
                    "App '{}' has no resource '{key}'",
                    self.name
                ))
            })
    }
    fn __getitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        self.dict.bind(py).get_item(key)?.map(|v| v.unbind()).ok_or_else(|| {
            PyKeyError::new_err(format!("App '{}' has no resource {}", self.name, key_label(key)))
        })
    }
    fn __setitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<()> {
        check_typed(key, value)?;
        self.dict.bind(py).set_item(key, value)
    }
    fn __repr__(&self, py: Python<'_>) -> String {
        let keys: Vec<String> = self
            .dict
            .bind(py)
            .keys()
            .iter()
            .map(|k| key_label(&k))
            .collect();
        format!("<App '{}' resources={keys:?}>", self.name)
    }
}

// ─── Python systems ─────────────────────────────────────────────────────────

enum Arg {
    /// A resource, keyed by its class (annotated parameter) or by the
    /// parameter's name (unannotated).
    Resource(Py<PyAny>, String),
    Child(String, Py<PyDict>),
    Sim,
    /// `res`: this App's own resources, for systems that pick them at run time.
    Own,
}

struct Call {
    func: Py<PyAny>,
    args: Vec<Arg>,
}

impl Call {
    fn new(py: Python<'_>, func: Py<PyAny>, children: &[Child]) -> PyResult<Self> {
        let f = func.bind(py);
        let sig = py.import("inspect")?.call_method1("signature", (f,))?;
        let params = sig.getattr("parameters")?;
        // Resolve string annotations too; unannotated callables give {}.
        let hints = py
            .import("typing")?
            .call_method1("get_type_hints", (f,))
            .and_then(|h| Ok(h.downcast_into::<PyDict>()?))
            .unwrap_or_else(|_| PyDict::new(py));
        let sim_type = py.get_type::<Sim>();
        let mut args = Vec::new();
        for name in params.call_method0("keys")?.try_iter()? {
            let name: String = name?.extract()?;
            let arg = match hints.get_item(&name)? {
                Some(hint) => {
                    if hint.is(&sim_type) {
                        Arg::Sim
                    } else if let Some(c) = children.iter().find(|c| c.key.bind(py).is(&hint)) {
                        Arg::Child(c.name.clone(), c.dict.clone_ref(py))
                    } else if hint.is_instance_of::<pyo3::types::PyType>() {
                        let label = key_label(&hint);
                        Arg::Resource(hint.unbind(), label)
                    } else {
                        return Err(pyo3::exceptions::PyTypeError::new_err(format!(
                            "parameter '{name}': annotate with a resource class (got {hint})"
                        )));
                    }
                }
                None if name == "sim" => Arg::Sim,
                None if name == "res" => Arg::Own,
                None => match children.iter().find(|c| c.name == name) {
                    Some(c) => Arg::Child(name, c.dict.clone_ref(py)),
                    None => {
                        let label = format!("'{name}'");
                        Arg::Resource(pyo3::types::PyString::new(py, &name).into_any().unbind(), label)
                    }
                },
            };
            args.push(arg);
        }
        Ok(Self { func, args })
    }

    fn needs(&self, py: Python<'_>) -> Vec<(Py<PyAny>, String)> {
        self.args
            .iter()
            .filter_map(|a| match a {
                Arg::Resource(k, l) => Some((k.clone_ref(py), l.clone())),
                _ => None,
            })
            .collect()
    }

    fn call<'py>(
        &self,
        py: Python<'py>,
        world: &Bound<'py, PyDict>,
        sim: &Bound<'py, Sim>,
        system: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut vals = Vec::with_capacity(self.args.len());
        for a in &self.args {
            vals.push(match a {
                Arg::Sim => sim.clone().into_any(),
                Arg::Own => Bound::new(
                    py,
                    AppView {
                        name: "self".into(),
                        dict: world.clone().unbind(),
                    },
                )?
                .into_any(),
                Arg::Child(name, d) => Bound::new(
                    py,
                    AppView {
                        name: name.clone(),
                        dict: d.clone_ref(py),
                    },
                )?
                .into_any(),
                Arg::Resource(k, label) => world.get_item(k)?.ok_or_else(|| {
                    let have: Vec<String> = world.keys().iter().map(|k| key_label(&k)).collect();
                    PyKeyError::new_err(format!(
                        "system '{system}' needs resource {label}, which this App does not have \
                         (resources: {have:?})"
                    ))
                })?,
            });
        }
        self.func.bind(py).call1(PyTuple::new(py, vals)?)
    }

    fn wants_sim(&self) -> bool {
        self.args.iter().any(|a| matches!(a, Arg::Sim))
    }
}

struct PySystem {
    name: String,
    body: Call,
    condition: Option<Call>,
    world: usize,
    manager: usize,
    clock: Option<usize>,
    subapps: Option<usize>,
}

impl System for PySystem {
    fn prepare(
        &mut self,
        index: &HashMap<std::any::TypeId, usize>,
        _resources: &[RefCell<Box<dyn std::any::Any>>],
    ) -> Vec<String> {
        use std::any::TypeId;
        let mut missing = Vec::new();
        match index.get(&TypeId::of::<PyWorld>()) {
            Some(&i) => self.world = i,
            None => missing.push("PyWorld".to_string()),
        }
        match index.get(&TypeId::of::<SchedulerManager>()) {
            Some(&i) => self.manager = i,
            None => missing.push("SchedulerManager".to_string()),
        }
        self.clock = index.get(&TypeId::of::<SimClock>()).copied();
        self.subapps = index.get(&TypeId::of::<SubApps>()).copied();
        missing
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn run(&mut self, resources: &[RefCell<Box<dyn std::any::Any>>]) {
        let (dict, error) = {
            let w = resources[self.world].borrow();
            let w = w.downcast_ref::<PyWorld>().expect("PyWorld");
            if w.error.borrow().is_some() {
                return;
            }
            (Python::with_gil(|py| w.dict.clone_ref(py)), w.error.clone())
        };
        let (step, time) = match self.clock {
            Some(i) => {
                let c = resources[i].borrow();
                let c = c.downcast_ref::<SimClock>().expect("SimClock");
                (c.step, c.time)
            }
            None => (0, 0.0),
        };
        // Children are ticked from Python only through `sim.tick`, so hold
        // the SubApps borrow just while a `sim`-taking system runs.
        let wants_sim = self.body.wants_sim()
            || self.condition.as_ref().is_some_and(|c| c.wants_sim());
        let mut subs = match (wants_sim, self.subapps) {
            (true, Some(i)) => Some(resources[i].borrow_mut()),
            _ => None,
        };
        let subs_ptr: *mut SubApps = subs
            .as_mut()
            .and_then(|s| s.downcast_mut::<SubApps>())
            .map_or(std::ptr::null_mut(), |s| s as *mut SubApps);

        let outcome = Python::with_gil(|py| -> PyResult<(bool, f64)> {
            let world = dict.bind(py);
            let sim = Bound::new(
                py,
                Sim {
                    step,
                    time,
                    stopped: false,
                    subapps: subs_ptr,
                },
            )?;
            let result = (|| -> PyResult<()> {
                if let Some(cond) = &self.condition {
                    if !cond.call(py, world, &sim, &self.name)?.is_truthy()? {
                        return Ok(());
                    }
                }
                self.body.call(py, world, &sim, &self.name)?;
                Ok(())
            })();
            let mut s = sim.borrow_mut();
            s.subapps = std::ptr::null_mut();
            result.map(|_| (s.stopped, s.time))
        });
        drop(subs);

        let stop = match outcome {
            Ok((stopped, new_time)) => {
                if let Some(i) = self.clock {
                    let mut c = resources[i].borrow_mut();
                    c.downcast_mut::<SimClock>().expect("SimClock").time = new_time;
                }
                stopped
            }
            Err(e) => {
                Python::with_gil(|py| {
                    let note = format!("in GRASS system '{}'", self.name);
                    let _ = e.value(py).call_method1("add_note", (note,));
                });
                *error.borrow_mut() = Some(e);
                true
            }
        };
        if stop {
            let mut m = resources[self.manager].borrow_mut();
            m.downcast_mut::<SchedulerManager>()
                .expect("SchedulerManager")
                .state = SchedulerState::End;
        }
    }
}

// ─── Rust plugins ───────────────────────────────────────────────────────────

#[derive(Clone)]
enum RustPluginKind {
    Run,
    Checkpoint,
    Provenance {
        name: String,
        version: String,
        notes: Vec<(String, String)>,
    },
}

/// A plugin implemented in Rust (see `grass.plugins`).
#[pyclass(module = "grass", name = "RustPlugin")]
#[derive(Clone)]
pub struct RustPluginPy {
    kind: RustPluginKind,
}

#[pymethods]
impl RustPluginPy {
    fn __repr__(&self) -> String {
        format!("<grass.plugins.{}>", self.label())
    }
}

impl RustPluginPy {
    fn label(&self) -> &'static str {
        match self.kind {
            RustPluginKind::Run => "Run",
            RustPluginKind::Checkpoint => "Checkpoint",
            RustPluginKind::Provenance { .. } => "Provenance",
        }
    }
}

/// `[run]` steps / stages and end-of-run control.
#[pyfunction(name = "Run")]
fn run_plugin() -> RustPluginPy {
    RustPluginPy {
        kind: RustPluginKind::Run,
    }
}

/// Periodic checkpoints and restart (`[checkpoint]` every / dir / keep / restart).
#[pyfunction(name = "Checkpoint")]
fn checkpoint_plugin() -> RustPluginPy {
    RustPluginPy {
        kind: RustPluginKind::Checkpoint,
    }
}

/// Writes provenance.toml (code version, host, input hash, notes) at start.
#[pyfunction(name = "Provenance")]
#[pyo3(signature = (name, version, notes = None))]
fn provenance_plugin(
    name: String,
    version: String,
    notes: Option<HashMap<String, String>>,
) -> RustPluginPy {
    let mut notes: Vec<_> = notes.unwrap_or_default().into_iter().collect();
    notes.sort();
    RustPluginPy {
        kind: RustPluginKind::Provenance {
            name,
            version,
            notes,
        },
    }
}

// ─── App ────────────────────────────────────────────────────────────────────

/// A child App as its parent sees it.
struct Child {
    /// What parent systems use to ask for it: a class or a string.
    key: Py<PyAny>,
    /// The sub-App namespace in grass_multi.
    name: String,
    dict: Py<PyDict>,
}

/// The namespace for a child key: the string itself, or the class name.
fn child_name(key: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(s) = key.extract::<String>() {
        return Ok(s);
    }
    key.getattr("__name__")?.extract()
}

/// `app[SomeClass] = value` must hold a `SomeClass`.
fn check_typed(key: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<()> {
    if let Ok(t) = key.downcast::<pyo3::types::PyType>() {
        if !value.is_instance(t)? {
            return Err(pyo3::exceptions::PyTypeError::new_err(format!(
                "resource keyed by {} must be a {} (got {})",
                key_label(key),
                key_label(key),
                key_label(&value.get_type().into_any())
            )));
        }
    }
    Ok(())
}

/// A GRASS App: resources, systems in ordered phases, plugins, child Apps.
#[pyclass(unsendable, module = "grass")]
pub struct App {
    inner: Option<RustApp>,
    /// Name of the parent this App was moved into, if any.
    moved_into: Option<String>,
    world: Py<PyDict>,
    error: ErrorSlot,
    checkpointed: Keys,
    phases: Vec<&'static str>,
    children: Vec<Child>,
    /// Resources each update system needs: (system label, key, key label).
    needs: Vec<(String, Py<PyAny>, String)>,
    child_errors: Vec<ErrorSlot>,
    plugins: HashSet<String>,
    prepared: bool,
    finished: bool,
    steps_run: u64,
}

impl App {
    fn app(&mut self) -> PyResult<&mut RustApp> {
        if self.prepared {
            return Err(PyRuntimeError::new_err(
                "the App has started running; its schedule can no longer change",
            ));
        }
        self.app_any()
    }

    fn app_any(&mut self) -> PyResult<&mut RustApp> {
        match (&mut self.inner, &self.moved_into) {
            (Some(a), _) => Ok(a),
            (None, Some(p)) => Err(PyRuntimeError::new_err(format!(
                "this App was added to parent App as '{p}'; run the parent instead"
            ))),
            (None, None) => Err(PyRuntimeError::new_err("App is not available")),
        }
    }

    fn phase(&mut self, name: &str) -> PyPhase {
        let index = match self.phases.iter().position(|p| *p == name) {
            Some(i) => i,
            None => {
                self.phases.push(Box::leak(name.to_string().into_boxed_str()));
                self.phases.len() - 1
            }
        };
        PyPhase {
            index: index as u32,
            name: self.phases[index],
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn make_system(
        &mut self,
        py: Python<'_>,
        func: Py<PyAny>,
        label: Option<String>,
        before: Vec<String>,
        after: Vec<String>,
        run_if: Option<Py<PyAny>>,
    ) -> PyResult<SystemDescriptor<PySystem>> {
        let name: String = func
            .bind(py)
            .getattr("__name__")
            .and_then(|n| n.extract())
            .unwrap_or_else(|_| "python_system".into());
        let body = Call::new(py, func, &self.children)?;
        let condition = run_if
            .map(|c| Call::new(py, c, &self.children))
            .transpose()?;
        let label = label.unwrap_or_else(|| name.clone());
        let cond_needs = condition.as_ref().map(|c| c.needs(py)).unwrap_or_default();
        for (k, l) in body.needs(py).into_iter().chain(cond_needs) {
            self.needs.push((label.clone(), k, l));
        }
        Ok(SystemDescriptor {
            system: PySystem {
                name: label.clone(),
                body,
                condition,
                world: usize::MAX,
                manager: usize::MAX,
                clock: None,
                subapps: None,
            },
            label: Some(label),
            befores: before,
            afters: after,
            requires: vec![],
        })
    }

    fn take_error(&self) -> Option<PyErr> {
        if let Some(e) = self.error.borrow_mut().take() {
            return Some(e);
        }
        self.child_errors.iter().find_map(|e| e.borrow_mut().take())
    }
}

fn names(v: Option<&Bound<'_, PyAny>>) -> PyResult<Vec<String>> {
    match v {
        None => Ok(vec![]),
        Some(v) if v.is_none() => Ok(vec![]),
        Some(v) => {
            if let Ok(s) = v.extract::<String>() {
                return Ok(vec![s]);
            }
            let mut out = Vec::new();
            for item in v.try_iter()? {
                let item = item?;
                out.push(match item.extract::<String>() {
                    Ok(s) => s,
                    Err(_) => item.getattr("__name__")?.extract()?,
                });
            }
            Ok(out)
        }
    }
}

fn one_name(v: Option<&Bound<'_, PyAny>>) -> PyResult<Vec<String>> {
    match v {
        Some(v) if !v.is_none() && v.extract::<String>().is_err() && v.hasattr("__name__")? => {
            Ok(vec![v.getattr("__name__")?.extract()?])
        }
        other => names(other),
    }
}

#[pymethods]
impl App {
    /// `config` is TOML text (the same input file a Rust GRASS code reads);
    /// `output_dir` is where plugins write files.
    #[new]
    #[pyo3(signature = (config = None, output_dir = None))]
    fn new(py: Python<'_>, config: Option<&str>, output_dir: Option<String>) -> PyResult<Self> {
        let mut app = RustApp::new();
        if let Some(c) = config {
            let cfg = Config::try_from_str(c)
                .map_err(|e| PyValueError::new_err(format!("config: {e}")))?;
            app.add_resource(cfg);
        }
        app.add_resource(Input {
            filename: String::new(),
            output_dir,
        });
        let world = PyDict::new(py).unbind();
        let error: ErrorSlot = Rc::default();
        let checkpointed: Keys = Rc::default();
        app.add_resource(PyWorld {
            dict: world.clone_ref(py),
            error: error.clone(),
            checkpointed: checkpointed.clone(),
        });
        Ok(Self {
            inner: Some(app),
            moved_into: None,
            world,
            error,
            checkpointed,
            phases: Vec::new(),
            children: Vec::new(),
            needs: Vec::new(),
            child_errors: Vec::new(),
            plugins: HashSet::new(),
            prepared: false,
            finished: false,
            steps_run: 0,
        })
    }

    // ── resources ──

    /// Add a resource keyed by its class: `app.insert(Temperature(...))`.
    /// Systems receive it through a parameter annotated `: Temperature`.
    fn insert(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<()> {
        self.world.bind(py).set_item(value.get_type(), value)
    }

    /// `app[Temperature]` for a typed resource, or `app["name"]` for one
    /// added by name (unannotated parameters look resources up by name).
    fn __setitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<()> {
        if let Ok(name) = key.extract::<String>() {
            if name == "sim" || name == "res" || self.children.iter().any(|c| c.name == name) {
                return Err(PyValueError::new_err(format!("'{name}' is a reserved name")));
            }
        }
        check_typed(key, value)?;
        self.world.bind(py).set_item(key, value)
    }

    fn __getitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        self.world
            .bind(py)
            .get_item(key)?
            .map(|v| v.unbind())
            .ok_or_else(|| PyKeyError::new_err(format!("no resource {}", key_label(key))))
    }

    fn __contains__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<bool> {
        self.world.bind(py).contains(key)
    }

    /// This App's resources (classes by name, named resources quoted).
    fn resources(&self, py: Python<'_>) -> Vec<String> {
        self.world
            .bind(py)
            .keys()
            .iter()
            .map(|k| key_label(&k))
            .collect()
    }

    // ── schedule ──

    /// Declare phases in execution order (phases used without being
    /// declared are appended in order of first use).
    #[pyo3(signature = (*names))]
    fn add_phases(&mut self, names: Vec<String>) -> PyResult<()> {
        self.app()?;
        for n in names {
            self.phase(&n);
        }
        Ok(())
    }

    /// The declared phases, in order.
    fn phases(&self) -> Vec<String> {
        self.phases.iter().map(|s| s.to_string()).collect()
    }

    /// Run `func` every step in `phase`. Its parameter names pick the
    /// resources it receives. `before` / `after` take labels or functions;
    /// `run_if` is a callable with the same parameter rules.
    #[pyo3(signature = (func, phase = "update", *, label = None, before = None, after = None, run_if = None))]
    #[allow(clippy::too_many_arguments)]
    fn add_system(
        &mut self,
        py: Python<'_>,
        func: Py<PyAny>,
        phase: &str,
        label: Option<String>,
        before: Option<&Bound<'_, PyAny>>,
        after: Option<&Bound<'_, PyAny>>,
        run_if: Option<Py<PyAny>>,
    ) -> PyResult<()> {
        self.app()?;
        let sys = self.make_system(py, func, label, one_name(before)?, one_name(after)?, run_if)?;
        let phase = self.phase(phase);
        self.app()?.add_update_system(sys, phase);
        Ok(())
    }

    /// Run `func` once, before the first step.
    #[pyo3(signature = (func, phase = "setup", *, label = None))]
    fn add_setup_system(
        &mut self,
        py: Python<'_>,
        func: Py<PyAny>,
        phase: &str,
        label: Option<String>,
    ) -> PyResult<()> {
        self.app()?;
        let sys = self.make_system(py, func, label, vec![], vec![], None)?;
        let phase = self.phase(phase);
        self.app()?.add_setup_system(sys, phase);
        Ok(())
    }

    /// Remove update systems by label (a function's name by default).
    fn remove_system(&mut self, py: Python<'_>, label: &Bound<'_, PyAny>) -> PyResult<()> {
        let _ = py;
        let label = one_name(Some(label))?
            .pop()
            .ok_or_else(|| PyValueError::new_err("label"))?;
        self.app()?.remove_update_system_by_label(&label);
        self.needs.retain(|(l, _, _)| *l != label);
        Ok(())
    }

    /// The update schedule in execution order, as
    /// `(namespace, phase, system)` tuples.
    fn schedule(&mut self) -> PyResult<Vec<(u32, String, String)>> {
        let prepared = self.prepared;
        let app = self.app_any()?;
        if !prepared {
            app.add_resource(SchedulerManager::new());
            app.organize_systems();
        }
        Ok(app
            .update_system_listing()
            .into_iter()
            .map(|(ns, _, phase, name)| (ns, phase.to_string(), short(&name)))
            .collect())
    }

    // ── plugins ──

    /// Add a plugin: a `grass.plugins` Rust plugin, or any object with a
    /// `build(app)` method. Each plugin type may be added once.
    fn add_plugin(slf: &Bound<'_, Self>, plugin: &Bound<'_, PyAny>) -> PyResult<()> {
        if let Ok(p) = plugin.extract::<RustPluginPy>() {
            let mut me = slf.borrow_mut();
            let key = format!("rust:{}", p.label());
            if !me.plugins.insert(key) {
                return Err(PyValueError::new_err(format!(
                    "plugin {} was already added",
                    p.label()
                )));
            }
            let app = me.app()?;
            match p.kind {
                RustPluginKind::Run => {
                    app.add_plugins(RunPlugin);
                }
                RustPluginKind::Checkpoint => {
                    app.add_plugins(CheckpointPlugin);
                }
                RustPluginKind::Provenance {
                    name,
                    version,
                    notes,
                } => {
                    let mut pl = ProvenancePlugin::new(&name, &version);
                    for (k, v) in &notes {
                        pl = pl.note(k, v);
                    }
                    app.add_plugins(pl);
                }
            }
            return Ok(());
        }
        let ty = plugin.get_type();
        let key = format!("py:{}", ty.fully_qualified_name()?);
        if let Ok(deps) = plugin.getattr("requires") {
            let me = slf.borrow();
            for d in names(Some(&deps))? {
                let found = me.plugins.iter().any(|p| {
                    p.ends_with(&format!(".{d}")) || p.ends_with(&format!(":{d}"))
                });
                if !found {
                    return Err(PyValueError::new_err(format!(
                        "plugin {} requires {d}, which has not been added",
                        ty.name()?
                    )));
                }
            }
        }
        {
            let mut me = slf.borrow_mut();
            me.app()?;
            if !me.plugins.insert(key) {
                return Err(PyValueError::new_err(format!(
                    "plugin {} was already added",
                    ty.name()?
                )));
            }
        }
        plugin.call_method1("build", (slf,))?;
        Ok(())
    }

    /// Include these resources in grass_io checkpoints (pickled) and
    /// restore them on restart. Needs `grass.plugins.Checkpoint()` first.
    #[pyo3(signature = (*keys))]
    fn checkpoint(&mut self, py: Python<'_>, keys: Vec<Py<PyAny>>) -> PyResult<()> {
        if !self.plugins.contains("rust:Checkpoint") {
            return Err(PyRuntimeError::new_err(
                "add grass.plugins.Checkpoint() before app.checkpoint(...)",
            ));
        }
        let first = self.checkpointed.borrow().is_empty();
        self.checkpointed.borrow_mut().extend(keys);
        if first {
            let snap = PySnapshot {
                dict: Some(self.world.clone_ref(py)),
                names: self.checkpointed.clone(),
                restored: None,
            };
            let app = self.app()?;
            app.add_resource(snap);
            app.checkpoint::<PySnapshot>("python");
            app.set_schedule_namespace::<PyRestore>(PY_RESTORE_NAMESPACE);
            app.add_setup_system(py_restore, PyRestore::Apply);
        }
        Ok(())
    }

    // ── child Apps ──

    /// Make `child` part of this App. `key` is a class (parent systems
    /// annotate a parameter with it) or a string (matched by parameter name).
    fn add_app(&mut self, py: Python<'_>, key: &Bound<'_, PyAny>, child: &Bound<'_, App>) -> PyResult<()> {
        let name = child_name(key)?;
        if self.world.bind(py).contains(key)? || name == "sim" || name == "res" {
            return Err(PyValueError::new_err(format!("'{name}' is already a resource name")));
        }
        if self.children.iter().any(|c| c.name == name) {
            return Err(PyValueError::new_err(format!("child App '{name}' already added")));
        }
        let mut c = child.borrow_mut();
        if c.prepared {
            return Err(PyRuntimeError::new_err("child App has already run"));
        }
        let inner = c.inner.take().ok_or_else(|| {
            PyRuntimeError::new_err("child App was already added to a parent")
        })?;
        c.moved_into = Some(name.clone());
        let first = self.children.is_empty();
        let app = self.app()?;
        app.add_subapp(&name, inner);
        if first {
            app.set_schedule_namespace::<ChildDone>(CHILD_DONE_NAMESPACE);
            app.add_update_system(
                (|subs: Res<SubApps>, mut m: ResMut<SchedulerManager>| {
                    if subs.any_done() {
                        m.state = SchedulerState::End;
                    }
                })
                .label("end when a child ends"),
                ChildDone::Check,
            );
        }
        self.children.push(Child {
            key: key.clone().unbind(),
            name,
            dict: c.world.clone_ref(py),
        });
        self.child_errors.push(c.error.clone());
        Ok(())
    }

    /// Advance a child by `n` steps each parent step, in `phase`.
    #[pyo3(signature = (child, n = 1, phase = "tick"))]
    fn tick(&mut self, child: &Bound<'_, PyAny>, n: usize, phase: &str) -> PyResult<()> {
        let name = child_name(child)?;
        if !self.children.iter().any(|c| c.name == name) {
            return Err(PyKeyError::new_err(format!("no child App '{name}'")));
        }
        let phase = self.phase(phase);
        self.app()?
            .add_update_system(tick_subapp(&name, n).label(format!("tick {name}")), phase);
        Ok(())
    }

    // ── running ──

    /// Run until a plugin or a system ends the run, or for `steps` steps.
    /// Returns the number of steps taken by this call. Call again to
    /// continue (the schedule is fixed once running).
    #[pyo3(signature = (steps = None))]
    fn run(&mut self, py: Python<'_>, steps: Option<u64>) -> PyResult<u64> {
        if self.finished {
            return Err(PyRuntimeError::new_err("this App's run has finished"));
        }
        let prepared = self.prepared;
        let app = self.app_any()?;
        if !prepared {
            app.prepare();
        }
        self.prepared = true;
        if let Some(e) = self.take_error() {
            return Err(e);
        }
        if !prepared {
            // Every update system's resources must exist before step 1
            // (setup systems have had their chance to insert them).
            let world = self.world.bind(py);
            let mut missing = Vec::new();
            for (system, key, label) in &self.needs {
                if !world.contains(key.bind(py))? {
                    missing.push(format!("  system '{system}' needs {label}"));
                }
            }
            if !missing.is_empty() {
                self.finish();
                return Err(PyKeyError::new_err(format!(
                    "missing resources (have: {:?}):\n{}",
                    self.resources(py),
                    missing.join("\n")
                )));
            }
        }
        let mut n = 0u64;
        loop {
            let app = self.inner.as_mut().expect("app");
            if app.is_done() || steps.is_some_and(|s| n >= s) {
                break;
            }
            app.run();
            n += 1;
            self.steps_run += 1;
            if let Some(e) = self.take_error() {
                self.finish();
                return Err(e);
            }
            if n.is_multiple_of(64) {
                py.check_signals()?;
            }
        }
        if self.inner.as_ref().expect("app").is_done() {
            self.finish();
        }
        Ok(n)
    }

    /// Steps taken so far.
    #[getter]
    fn steps(&self) -> u64 {
        self.steps_run
    }

    /// Whether the run has ended.
    #[getter]
    fn done(&self) -> bool {
        self.finished
    }

    /// The `[section]` reference for every plugin's config, as Markdown.
    fn config_reference(&mut self) -> PyResult<String> {
        Ok(self.app_any()?.config_reference())
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        format!(
            "<grass.App phases={:?} resources={:?} children={:?}>",
            self.phases,
            self.resources(py),
            self.children.iter().map(|c| &c.name).collect::<Vec<_>>()
        )
    }
}

impl App {
    fn finish(&mut self) {
        if !self.finished {
            self.finished = true;
            if let Some(app) = self.inner.as_mut() {
                app.run_cleanup();
            }
        }
    }
}

/// Shorten Rust type paths in system names (`grass_io::run::advance_step`
/// → `advance_step`; closures keep their owning function).
fn short(name: &str) -> String {
    let base = name.split('<').next().unwrap_or(name);
    let parts: Vec<&str> = base.split("::").filter(|p| !p.starts_with('{')).collect();
    let tail = parts.last().copied().unwrap_or(base);
    let owner = parts.iter().rev().nth(1).copied().unwrap_or("");
    if name.contains("{{closure}}") && !owner.is_empty() {
        format!("{owner}::{tail}")
    } else {
        tail.to_string()
    }
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<App>()?;
    m.add_class::<Sim>()?;
    m.add_class::<AppView>()?;
    m.add_class::<RustPluginPy>()?;
    let plugins = PyModule::new(m.py(), "plugins")?;
    plugins.add_function(wrap_pyfunction!(run_plugin, &plugins)?)?;
    plugins.add_function(wrap_pyfunction!(checkpoint_plugin, &plugins)?)?;
    plugins.add_function(wrap_pyfunction!(provenance_plugin, &plugins)?)?;
    m.add_submodule(&plugins)?;
    m.py()
        .import("sys")?
        .getattr("modules")?
        .set_item("grass.plugins", &plugins)?;
    Ok(())
}
