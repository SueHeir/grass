//! # grass_multi — Tier-0 primitives for multi-physics coupling
//!
//! `grass_multi` provides a small set of primitives for running several
//! independent [`grass_app::App`] subsystems together inside a single parent
//! `App`. Each subsystem (a "sub-App") has its own scheduler and resource
//! store; the parent `App`'s schedule decides when each sub-App ticks and
//! when cross-namespace couplers run.
//!
//! There is no orchestrator type, no Strategy enum, no Coupler trait — just:
//!
//!   - [`SubApps`] resource + [`MultiAppExt::add_subapp`] /
//!     [`MultiAppExt::add_remote_subapp`] for registration
//!   - [`Multi`] / [`MultiRes`] / [`MultiResMut`] SystemParams for
//!     cross-namespace reads and writes from ordinary parent-App systems
//!   - [`tick_subapp`] / [`tick_n_times`] system constructors that drive
//!     a sub-App's step loop from the parent's schedule
//!   - [`Physics`] trait + [`AppPhysics`] adapter (local sub-App) +
//!     [`RemoteMirrorPhysics`] (cross-process mirror) so MPI mirrors slot
//!     into the same `SubApps` machinery as local sub-Apps
//!   - [`Wire`] / [`Transport`] / `MpiInterCommTransport` (behind the
//!     `mpi` feature) for cross-process coupling
//!   - [`OuterIterStopPlugin`] for fixed-iter termination
//!   - [`snapshot_subapp_resource`] / [`restore_subapp_resource`] for opt-in
//!     reversibility (Picard / adaptive retries)
//!
//! ## The coupling loop: the parent schedule *is* the orchestrator
//!
//! There is no hidden driver loop. The parent `App`'s own schedule decides
//! everything; one outer iteration (one `parent.run()`) is just the parent's
//! systems firing in `(namespace, index)` order. The Tier-0 convention maps
//! that onto three logical bands, expressed as parent `ScheduleSet` phases:
//!
//! ```text
//! one outer iter = parent.run() =
//!     Tick   →  tick_subapp(..) / tick_n_times(..) systems advance each sub-App
//!     Couple →  Multi / MultiRes / MultiResMut systems move data across namespaces
//!     Check  →  a stop system (e.g. OuterIterStopPlugin) decides whether to end
//! ```
//!
//! You wire those phases yourself with `add_update_system(sys, Phase::Tick)`
//! etc.; nothing forces this exact shape, but couplers must run *after* the
//! ticks that produce the data they read, so phase ordering is the contract.
//!
//! ## Borrow rules (read before writing a coupler)
//!
//! Per-resource isolation comes from a `RefCell` on **each** sub-App resource,
//! keyed by `(type T, namespace NS)` — not from `Multi` itself. Consequences:
//!
//! - **Allowed:** one system holding several cross-namespace handles at once,
//!   e.g. read `"cfd"`'s `T` and write `"dem"`'s `U` in the same statement —
//!   different cells, different borrows.
//! - **Panics:** taking two `&mut` handles to the *same* `(T, NS)` cell, or a
//!   `&` and a `&mut` to it, live at the same time (`RefCell` double-borrow).
//!   `expect_read`/`expect_write` also panic if the namespace or resource type
//!   isn't registered.
//! - **The big hazard:** a single system must **not** take a `Multi` /
//!   `MultiRes*` parameter **and** `ResMut<SubApps>` together. `Multi` borrows
//!   `Res<SubApps>` (shared) while `tick_subapp`'s `ResMut<SubApps>` borrows it
//!   exclusively; holding both in one system double-borrows the `SubApps` cell
//!   and panics at run time. Keep ticking (which mutates `SubApps`) and
//!   coupling (which reads `SubApps` to reach *into* sub-Apps) in **separate
//!   systems / phases**.
//!
//! ## Driving it: `start()` vs a manual loop
//!
//! - **Self-driving:** [`grass_app::App::start`] on the parent runs the whole
//!   thing and calls the parent's `run_cleanup` at the end. Sub-App cleanups,
//!   though, are driven by [`SubApps::cleanup_all`] — register it (e.g. as a
//!   cleanup-with-app via a plugin) so it fires; the parent's own
//!   `run_cleanup` does not reach into sub-Apps automatically.
//! - **Externally driven:** `parent.prepare()`, then `parent.run()` in a loop
//!   you own until a stop condition, then call [`SubApps::cleanup_all`]
//!   yourself before dropping the parent. (The integration tests use exactly
//!   this manual `prepare` → `run`×N shape.)
//!
//! ## Example
//!
//! ```rust,ignore
//! use grass_app::prelude::*;
//! use grass_multi::{tick_subapp, MultiAppExt, MultiRes, MultiResMut};
//! use grass_scheduler::prelude::*;
//!
//! #[derive(Debug, Clone, Copy, ScheduleSet)]
//! enum Phase { Tick, Couple, Check }
//!
//! fn exchange(a: MultiRes<MyState, A>, mut b_other: MultiResMut<MyOther, B>) {
//!     b_other.x = a.x;
//! }
//!
//! let mut parent = App::new();
//! parent.add_subapp("a", app_a);
//! parent.add_subapp("b", app_b);
//! parent.add_update_system(tick_subapp("a", 1), Phase::Tick);
//! parent.add_update_system(tick_subapp("b", 1), Phase::Tick);
//! parent.add_update_system(exchange, Phase::Couple);
//! parent.start();
//! ```

mod multi;
mod outer_iter;
mod physics;
mod remote;
mod snapshot;
mod transport;
mod typed_multi;
mod wire;

// Re-export the `Namespace` derive macro alongside the trait so users
// get both with `use grass_multi::*;`.
pub use grass_derive::Namespace;

pub use multi::{
    tick_n_times, tick_subapp, Multi, MultiAppExt, MultiMut, MultiRef, Namespace,
    RemoteSubAppBuilder, SubApps,
};
pub use outer_iter::{check_done_outer_iter, NIters, OuterIter, OuterIterStopPlugin};
pub use physics::{AppPhysics, Physics, StepResult};
pub use remote::RemoteMirrorPhysics;
pub use snapshot::{restore_subapp_resource, snapshot_subapp_resource};
#[cfg(feature = "mpi")]
pub use transport::MpiInterCommTransport;
pub use transport::{LocalTransport, Transport};
pub use typed_multi::{MultiRes, MultiResMut};
pub use wire::Wire;
