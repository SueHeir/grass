//! Plugin-based application framework for explicit, time-stepping particle and grid solvers.
//!
//! Provides [`App`] as the central container and the [`Plugin`] trait for modular registration
//! of resources and systems.
//!
//! A complete, runnable starting point lives in
//! `examples/hello_app/main.rs` (`cargo run --example hello_app`):
//! a `ScheduleSet`, a resource, a system, a `Plugin`, and a done-condition,
//! driven by `App::new().add_plugins(..).start()`.
//!
//! # Validation model: two independent mechanisms
//!
//! `App` checks plugin wiring two different ways, at two different times:
//!
//! | Mechanism | Declared by | Checked when | Order-sensitive? |
//! |-----------|-------------|--------------|------------------|
//! | **TypeId dependencies** ([`Plugin::dependencies`]) | `type_ids![A, B]` | **eagerly**, during [`add_plugins`](App::add_plugins) | **Yes** — the dependency must already be registered |
//! | **Capability contracts** ([`Plugin::provides_capabilities`] / [`Plugin::requires_capabilities`]) | exported [`CapabilityId`] constants | **lazily**, at [`start`](App::start) / [`prepare`](App::prepare) | **No** — provider may be added before *or* after |
//!
//! Use **TypeId dependencies** when plugin B genuinely cannot `build()`
//! without plugin A's resources/systems already present (a hard ordering
//! constraint). Use **capability contracts** for looser "some plugin must
//! supply `contact_forces`" requirements, where any provider in any order
//! satisfies the need — the order-independence is the point.
//!
//! [`App::add_plugins`] remains the convenient path for fixed application
//! wiring and panics with a diagnostic on duplicate plugins or missing TypeId
//! dependencies. Use [`App::try_add_plugins`] when a CLI, GUI, or test harness
//! should handle those registration failures as an [`AppError`] instead.
//! Capability contracts have the same split: [`App::prepare`] and
//! [`App::start`] keep the panic-on-invalid convenience behavior, while
//! [`App::validate_capability_contracts_result`], [`App::try_prepare`], and
//! [`App::try_start`] return [`AppError::MissingCapabilities`] so external
//! drivers can report every missing capability tag and the plugin that required it.
//!
//! # Two lifecycle paths
//!
//! - **Self-driving:** [`App::start`] runs the whole thing —
//!   organize → setup → run-loop-until-`End` → [`run_cleanup`](App::run_cleanup).
//!   Cleanup is automatic.
//! - **Externally driven:** [`App::prepare`] (validate + organize + setup,
//!   leaving the scheduler in `Run`), then call [`App::run`] in a loop you own
//!   (e.g. a `grass_multi` parent ticking sub-Apps) until [`App::is_done`],
//!   then **call [`App::run_cleanup`] yourself** — nothing calls it for you on
//!   this path. Forgetting it skips final-output writes and `finalize_mpi`.
//!
//! ## Cleanup ordering
//!
//! [`run_cleanup`](App::run_cleanup) runs **resource-aware** cleanups
//! (registered with [`add_cleanup_with_app`](App::add_cleanup_with_app), which
//! receive `&mut App`) **before** **resource-free** cleanups (registered with
//! [`add_cleanup`](App::add_cleanup)). This matters when a resource-aware
//! cleanup writes final output that reads live resources, before a
//! resource-free cleanup tears the world down (e.g. `grass_mpi::finalize_mpi`).
//!
//! ## Config-generation recipe
//!
//! Each plugin can return a TOML snippet from [`Plugin::default_config`]; the
//! `App` accumulates them all into the [`ConfigSnippets`] resource as plugins
//! register. If the [`GenerateConfigFlag`] resource is present when
//! [`start`](App::start) is called, the `App` prints the assembled config to
//! stdout and exits **without running the simulation**. Wire it to a
//! `--generate-config` CLI flag:
//!
//! ```rust,ignore
//! let mut app = App::new();
//! app.add_plugins(MyPlugins);
//! if std::env::args().any(|a| a == "--generate-config") {
//!     app.add_resource(GenerateConfigFlag); // start() prints snippets + exits
//! }
//! app.start();
//! ```

#![warn(missing_docs)]

mod app;
mod plugin;
mod setup;
mod sub_app;

pub use app::*;
pub use plugin::*;
pub use setup::ScheduleSetupSet;
pub use sub_app::*;

/// The `grass_app` prelude.
///
/// Re-exports the most commonly used types so plugins can import them with a
/// single `use grass_app::prelude::*;`.
pub mod prelude {
    pub use crate::{
        app::App, app::AppError, app::CapabilityProvider, app::ConfigSnippets,
        app::GenerateConfigFlag, app::MissingCapability, app::MissingPluginDependency,
        app::PluginContract, app::PluginContracts, app::PluginDependency, setup::ScheduleSetupSet,
        sub_app::SubApp, CapabilityId, FallibleSetupSystem, Plugin, PluginGroup,
        PluginGroupBuilder, StageAdvancePlugin, StageNames, StatesPlugin,
    };
}
