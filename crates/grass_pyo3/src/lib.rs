//! Python bindings for GRASS Apps, shared by every GRASS extension module.
//!
//! A code's extension module (e.g. `dirt`) depends on this crate, calls
//! [`register`] in its `#[pymodule]`, and adds its own Rust plugins
//! ([`RustPluginPy::new`]), phases ([`RustPhase::new`]) and resource views
//! ([`register_view`]). Apps from different modules compose as parent and
//! children through the stable C ABI in [`abi`], so modules can be built
//! separately, against different GRASS versions.

pub mod abi;
mod app;

pub use app::{register, register_view, RustPhase, RustPluginPy, RustView, PY_NAMESPACE};
