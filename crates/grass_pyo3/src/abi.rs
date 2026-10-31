//! The stable boundary between GRASS extension modules.
//!
//! Each extension module (`grass`, `dirt`, a lab's own code, ...) is a
//! separate shared library with its own compiled copy of the GRASS crates,
//! possibly from a different GRASS version or compiler. Rust types cannot
//! cross that boundary, so modules meet at the *App* level through a
//! `#[repr(C)]` table of `extern "C"` functions, which is the same contract
//! `grass_multi::Physics` gives a parent for each child (prepare, step,
//! is_done, cleanup). Data crosses as Python objects, which every module
//! shares through the one libpython in the process.
//!
//! The table travels inside a `PyCapsule` named [`CAPSULE_NAME`], returned
//! by `App._grass_participant()`. `ctx` is opaque to the importer, which
//! takes ownership by copying the table and setting the capsule's `ctx` to
//! null, then calls `release` once when done. A capsule whose `ctx` is
//! still set when it is collected releases it itself.
//!
//! Versioning: a new field or function means a new struct (`ParticipantV2`)
//! and a new capsule name; importers keep accepting the old ones.

use std::ffi::{c_char, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};

use grass_app::prelude::App as RustApp;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyCapsule;

use crate::app::ErrorSlot;

/// Capsule name for [`ParticipantV1`].
pub const CAPSULE_NAME: &CStr = c"grass.participant.v1";
/// Value of [`ParticipantV1::abi`].
pub const ABI_V1: u32 = 1;

/// One App, driven from another module. All functions take `ctx`.
///
/// Return codes: 0 ok, nonzero failed (call `take_error` for the
/// exception). The caller must hold the GIL for every call.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ParticipantV1 {
    /// Always [`ABI_V1`].
    pub abi: u32,
    /// Opaque state owned by the exporting module.
    pub ctx: *mut c_void,
    /// Organize systems and run setup.
    pub prepare: unsafe extern "C" fn(*mut c_void) -> i32,
    /// Advance one step.
    pub step: unsafe extern "C" fn(*mut c_void) -> i32,
    /// 1 if the App has ended (a plugin, `sim.stop()`, or an error).
    pub is_done: unsafe extern "C" fn(*mut c_void) -> i32,
    /// Run cleanup hooks once.
    pub cleanup: unsafe extern "C" fn(*mut c_void),
    /// The pending exception as a new reference (`PyObject*`), or null.
    pub take_error: unsafe extern "C" fn(*mut c_void) -> *mut pyo3::ffi::PyObject,
    /// Free `ctx`. Called once by whoever owns it.
    pub release: unsafe extern "C" fn(*mut c_void),
}

// The table is only touched with the GIL held, from one thread.
unsafe impl Send for ParticipantV1 {}

// ─── exporting side ─────────────────────────────────────────────────────────

struct Exported {
    app: RustApp,
    error: ErrorSlot,
    prepared: bool,
    cleaned: bool,
}

fn guard(ctx: *mut c_void, f: impl FnOnce(&mut Exported) -> i32) -> i32 {
    // SAFETY: ctx is the Box<Exported> made in `export`, alive until release.
    let e = unsafe { &mut *(ctx as *mut Exported) };
    match catch_unwind(AssertUnwindSafe(|| f(&mut *e))) {
        Ok(rc) => rc,
        Err(panic) => {
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "panic".into());
            *e.error.borrow_mut() = Some(PyRuntimeError::new_err(msg));
            1
        }
    }
}

unsafe extern "C" fn ex_prepare(ctx: *mut c_void) -> i32 {
    guard(ctx, |e| {
        if !e.prepared {
            e.prepared = true;
            e.app.prepare();
        }
        i32::from(e.error.borrow().is_some())
    })
}

unsafe extern "C" fn ex_step(ctx: *mut c_void) -> i32 {
    guard(ctx, |e| {
        if !e.app.is_done() {
            e.app.run();
        }
        i32::from(e.error.borrow().is_some())
    })
}

unsafe extern "C" fn ex_is_done(ctx: *mut c_void) -> i32 {
    guard(ctx, |e| i32::from(e.prepared && e.app.is_done()))
}

unsafe extern "C" fn ex_cleanup(ctx: *mut c_void) {
    guard(ctx, |e| {
        if !e.cleaned {
            e.cleaned = true;
            e.app.run_cleanup();
        }
        0
    });
}

unsafe extern "C" fn ex_take_error(ctx: *mut c_void) -> *mut pyo3::ffi::PyObject {
    let e = unsafe { &*(ctx as *const Exported) };
    let err = e.error.borrow_mut().take();
    match err {
        // SAFETY: the caller holds the GIL (contract above).
        Some(err) => unsafe {
            let py = Python::assume_gil_acquired();
            err.into_value(py).into_ptr()
        },
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn ex_release(ctx: *mut c_void) {
    drop(unsafe { Box::from_raw(ctx as *mut Exported) });
}

/// Wrap `app` (whose Python systems report into `error`) as a capsule.
pub(crate) fn export(py: Python<'_>, app: RustApp, error: ErrorSlot) -> PyResult<Py<PyCapsule>> {
    let ctx = Box::into_raw(Box::new(Exported {
        app,
        error,
        prepared: false,
        cleaned: false,
    })) as *mut c_void;
    let table = ParticipantV1 {
        abi: ABI_V1,
        ctx,
        prepare: ex_prepare,
        step: ex_step,
        is_done: ex_is_done,
        cleanup: ex_cleanup,
        take_error: ex_take_error,
        release: ex_release,
    };
    let capsule = PyCapsule::new_with_destructor(
        py,
        table,
        Some(CAPSULE_NAME.to_owned()),
        |t: ParticipantV1, _| unsafe {
            if !t.ctx.is_null() {
                // Nobody took it: we still own it.
                (t.release)(t.ctx);
            }
        },
    )?;
    Ok(capsule.unbind())
}

// ─── importing side ─────────────────────────────────────────────────────────

/// A child App from another module, as a `grass_multi::Physics`.
pub(crate) struct ForeignPhysics {
    name: String,
    table: ParticipantV1,
    /// Keeps the capsule (and so the table's code) alive.
    _capsule: Py<PyCapsule>,
    /// Exceptions raised in the child surface through the parent's run().
    error: ErrorSlot,
    failed: bool,
}

impl ForeignPhysics {
    /// Claim the participant in `capsule`.
    pub(crate) fn claim(name: String, capsule: &Bound<'_, PyAny>, error: ErrorSlot) -> PyResult<Self> {
        let capsule = capsule.downcast::<PyCapsule>().map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err("_grass_participant() did not return a capsule")
        })?;
        let cname = capsule.name()?;
        // SAFETY: name pointers from PyCapsule are valid C strings.
        let cname = cname.map(|n| unsafe { CStr::from_ptr(n.as_ptr() as *const c_char) });
        if cname != Some(CAPSULE_NAME) {
            return Err(pyo3::exceptions::PyTypeError::new_err(format!(
                "unsupported GRASS participant ABI {cname:?}; this module speaks {CAPSULE_NAME:?}"
            )));
        }
        // SAFETY: name checked; the exporter stored a ParticipantV1.
        // We hold the GIL, so nobody else touches the table while we take it.
        let t = unsafe { &mut *(capsule.pointer() as *mut ParticipantV1) };
        if t.abi != ABI_V1 {
            return Err(PyRuntimeError::new_err(format!("participant ABI {} != {ABI_V1}", t.abi)));
        }
        if t.ctx.is_null() {
            return Err(PyRuntimeError::new_err("this App was already added to a parent"));
        }
        // Take ownership: the capsule no longer releases ctx; we do, on drop.
        let table = *t;
        t.ctx = std::ptr::null_mut();
        Ok(Self {
            name,
            table,
            _capsule: capsule.clone().unbind(),
            error,
            failed: false,
        })
    }

    fn check(&mut self, rc: i32) {
        if rc != 0 {
            self.failed = true;
        }
        let obj = unsafe { (self.table.take_error)(self.table.ctx) };
        if !obj.is_null() {
            self.failed = true;
            Python::with_gil(|py| {
                // SAFETY: take_error returns a new reference.
                let value = unsafe { Bound::from_owned_ptr(py, obj) };
                let err = PyErr::from_value(value);
                let note = format!("in child App '{}' (another GRASS module)", self.name);
                let _ = err.value(py).call_method1("add_note", (note,));
                *self.error.borrow_mut() = Some(err);
            });
        }
    }
}

impl grass_multi::Physics for ForeignPhysics {
    fn name(&self) -> &str {
        &self.name
    }
    fn prepare(&mut self) {
        let rc = unsafe { (self.table.prepare)(self.table.ctx) };
        self.check(rc);
    }
    fn step(&mut self) -> grass_multi::StepResult {
        if !self.failed {
            let rc = unsafe { (self.table.step)(self.table.ctx) };
            self.check(rc);
        }
        grass_multi::StepResult {
            completed_full_step: true,
        }
    }
    fn is_done(&self) -> bool {
        self.failed || unsafe { (self.table.is_done)(self.table.ctx) } != 0
    }
    fn cleanup(&mut self) {
        unsafe { (self.table.cleanup)(self.table.ctx) };
    }
    fn resource_cell(
        &self,
        _ty: std::any::TypeId,
    ) -> Option<&std::cell::RefCell<Box<dyn std::any::Any>>> {
        // Rust resources of another module are not ours to downcast.
        None
    }
}

impl Drop for ForeignPhysics {
    fn drop(&mut self) {
        unsafe { (self.table.release)(self.table.ctx) };
    }
}
