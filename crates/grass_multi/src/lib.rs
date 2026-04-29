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
