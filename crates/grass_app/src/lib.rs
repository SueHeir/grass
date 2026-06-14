//! Plugin-based application framework for explicit, time-stepping particle and grid solvers.
//!
//! Provides [`App`] as the central container and the [`Plugin`] trait for modular registration
//! of resources and systems.

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
        app::App, app::ConfigSnippets, app::GenerateConfigFlag, setup::ScheduleSetupSet,
        sub_app::SubApp, Plugin, PluginGroup, PluginGroupBuilder, StageAdvancePlugin, StageNames,
        StatesPlugin,
    };
}
