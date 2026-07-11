//! The [`Plugin`] trait, [`PluginGroup`] builder, and state/stage management plugins.
//!
//! Every feature is implemented as a [`Plugin`]. Plugins register
//! resources, systems, and sub-plugins with the [`App`](crate::App) during
//! the build phase.
//!
//! # Implementing a plugin
//!
//! ```rust,ignore
//! use grass_app::prelude::*;
//! use grass_scheduler::ScheduleSet;
//!
//! pub struct GravityPlugin;
//!
//! impl Plugin for GravityPlugin {
//!     fn build(&self, app: &mut App) {
//!         app.add_update_system(apply_gravity, ScheduleSet::PreForce);
//!     }
//! }
//! ```

use downcast_rs::{impl_downcast, Downcast};
use grass_scheduler::{
    apply_state_transitions, check_stage_advance, CurrentState, NextState, ScheduleSet, StageName,
    StoredPhase,
};
use std::{borrow::Cow, marker::PhantomData};

use crate::{App, AppError};
use core::any::Any;
use std::any::TypeId;
use std::collections::HashSet;

/// A typed, stable identifier for a capability that a plugin provides or requires.
///
/// Export capability identifiers as constants from the crate that defines the
/// contract, then consume those constants instead of repeating string literals:
///
/// ```rust,ignore
/// pub const NEIGHBOR_SEARCH: CapabilityId = CapabilityId::new("neighbor_search");
///
/// fn requires_capabilities(&self) -> Vec<CapabilityId> {
///     vec![NEIGHBOR_SEARCH]
/// }
/// ```
///
/// `Plugin::provides` and `Plugin::requires` remain available during the
/// migration from the earlier string-tag API. New contracts should use the
/// typed hooks.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityId(Cow<'static, str>);

impl CapabilityId {
    /// Creates a capability identifier suitable for exporting as a constant.
    pub const fn new(name: &'static str) -> Self {
        Self(Cow::Borrowed(name))
    }

    /// Returns the stable textual name used in diagnostics and generated docs.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn legacy(name: &str) -> Self {
        Self(Cow::Owned(name.to_owned()))
    }
}

impl std::fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Convenience macro to build a `Vec<TypeId>` from a list of types.
///
/// ```rust,ignore
/// fn dependencies(&self) -> Vec<TypeId> {
///     type_ids![DemAtomPlugin, NeighborPlugin]
/// }
/// ```
#[macro_export]
macro_rules! type_ids {
    ($($t:ty),* $(,)?) => {
        vec![$(std::any::TypeId::of::<$t>()),*]
    };
}

/// Convenience macro to build dependency display names matching [`type_ids!`].
///
/// Use this with [`Plugin::dependency_names`] when fallible plugin registration
/// should report concrete missing dependency names instead of raw [`TypeId`]s.
///
/// ```rust,ignore
/// fn dependencies(&self) -> Vec<TypeId> {
///     type_ids![DemAtomPlugin, NeighborPlugin]
/// }
/// fn dependency_names(&self) -> Vec<&'static str> {
///     dependency_names![DemAtomPlugin, NeighborPlugin]
/// }
/// ```
#[macro_export]
macro_rules! dependency_names {
    ($($t:ty),* $(,)?) => {
        vec![$(std::any::type_name::<$t>()),*]
    };
}

/// A self-contained module that registers resources and systems with an [`App`].
///
/// Every simulation feature — physics models, I/O, analysis — is implemented
/// as a `Plugin`. The [`build`](Plugin::build) method is called once during
/// [`App::add_plugins`] to wire everything up.
///
/// # Example
///
/// ```rust,ignore
/// pub struct MyPlugin;
///
/// impl Plugin for MyPlugin {
///     fn build(&self, app: &mut App) {
///         app.add_resource(MyResource::default())
///            .add_update_system(my_system, ScheduleSet::Update);
///     }
/// }
/// ```
///
/// Closures `Fn(&mut App)` also implement `Plugin`, which is handy for tests:
///
/// ```rust,ignore
/// app.add_plugins(|app: &mut App| {
///     app.add_resource(TestResource);
/// });
/// ```
pub trait Plugin: Downcast + Any + Send + Sync {
    /// Configures the [`App`] to which this plugin is added.
    ///
    /// This is the main entry point for plugin setup. Register resources,
    /// systems, and sub-plugins here.
    fn build(&self, app: &mut App);

    /// Fallible form of [`build`](Self::build).
    ///
    /// New plugins can override this hook to report configuration, preflight,
    /// or construction failures to an outer runner. Existing plugins need no
    /// changes: the compatibility default calls their infallible `build`.
    ///
    /// A failing implementation must leave its own resources in a state that
    /// its registered cleanup callbacks can safely tear down. [`App`] invokes
    /// that cleanup and stops registration before returning the error.
    fn try_build(&self, app: &mut App) -> Result<(), AppError> {
        self.build(app);
        Ok(())
    }

    /// Returns the plugin's name, used for duplicate detection and diagnostics.
    ///
    /// Defaults to the Rust type name (e.g. `"my_crate::MyPlugin"`).
    fn name(&self) -> &str {
        core::any::type_name::<Self>()
    }

    /// Whether this plugin may only be added once.
    ///
    /// Returns `true` by default. Override to return `false` if the plugin can
    /// be meaningfully instantiated multiple times (e.g. with different configs).
    fn is_unique(&self) -> bool {
        true
    }

    /// Returns a TOML snippet showing this plugin's config section with defaults.
    ///
    /// Used by `--generate-config` to print a complete example config file.
    /// Return `None` (the default) if the plugin has no configuration.
    fn default_config(&self) -> Option<&str> {
        None
    }

    /// Returns the [`TypeId`]s of plugins that must be registered before this one.
    ///
    /// The app validates that all listed plugins have been registered before
    /// this plugin's [`build`](Plugin::build) runs, and produces a clear error
    /// message if any are missing.
    ///
    /// ```rust,ignore
    /// fn dependencies(&self) -> Vec<TypeId> {
    ///     type_ids![DemAtomPlugin, NeighborPlugin]
    /// }
    /// ```
    fn dependencies(&self) -> Vec<TypeId> {
        Vec::new()
    }

    /// Returns human-readable names for [`dependencies`](Self::dependencies).
    ///
    /// The list should have the same order as [`dependencies`](Self::dependencies).
    /// These names are used only for diagnostics; dependency validation still
    /// uses the [`TypeId`]s returned by [`dependencies`](Self::dependencies).
    ///
    /// ```rust,ignore
    /// fn dependency_names(&self) -> Vec<&'static str> {
    ///     dependency_names![DemAtomPlugin, NeighborPlugin]
    /// }
    /// ```
    fn dependency_names(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// Returns capability tags that this plugin provides.
    ///
    /// Other plugins can declare these as requirements via [`requires`](Plugin::requires).
    /// The app validates at startup that every required capability has at least one provider.
    ///
    /// ```rust,ignore
    /// fn provides(&self) -> Vec<&str> { vec!["contact_forces"] }
    /// ```
    fn provides(&self) -> Vec<&str> {
        Vec::new()
    }

    /// Returns capability tags that this plugin requires from other plugins.
    ///
    /// The app validates at startup that every required capability is provided
    /// by at least one registered plugin. This catches wiring errors early with
    /// clear error messages.
    ///
    /// ```rust,ignore
    /// fn requires(&self) -> Vec<&str> { vec!["dem_particles", "neighbor_list"] }
    /// ```
    fn requires(&self) -> Vec<&str> {
        Vec::new()
    }

    /// Returns typed capabilities this plugin provides.
    ///
    /// Prefer this hook for new code and export the identifiers from the crate
    /// that owns the contract. The legacy string-based [`provides`](Self::provides)
    /// hook is collected as well for staged compatibility.
    fn provides_capabilities(&self) -> Vec<CapabilityId> {
        Vec::new()
    }

    /// Returns typed capabilities this plugin requires from other plugins.
    ///
    /// Prefer this hook for new code and use exported [`CapabilityId`] constants
    /// at consumer call sites. The legacy string-based [`requires`](Self::requires)
    /// hook is collected as well for staged compatibility.
    fn requires_capabilities(&self) -> Vec<CapabilityId> {
        Vec::new()
    }
}

impl_downcast!(Plugin);

/// Blanket impl: any `Fn(&mut App) + Send + Sync + 'static` can be used as a
/// [`Plugin`]. This is convenient for quick inline registrations and tests.
impl<T: Fn(&mut App) + Send + Sync + 'static> Plugin for T {
    fn build(&self, app: &mut App) {
        self(app);
    }
}

// ─── PluginGroup ──────────────────────────────────────────────────────────────

/// A collection of plugins that can be added to an [`App`] as a single unit.
///
/// Implement this to bundle multiple plugins together for reuse:
///
/// ```rust,ignore
/// pub struct MyDefaultPlugins;
///
/// impl PluginGroup for MyDefaultPlugins {
///     fn build(self) -> PluginGroupBuilder {
///         PluginGroupBuilder::start::<Self>()
///             .add(FooPlugin)
///             .add(BarPlugin)
///     }
/// }
///
/// App::new().add_plugins(MyDefaultPlugins).start();
/// ```
pub trait PluginGroup: Sized {
    /// Constructs a [`PluginGroupBuilder`] containing the group's plugins.
    fn build(self) -> PluginGroupBuilder;
}

/// Builder for a [`PluginGroup`]. Add plugins in order; they will be registered
/// in that order when the group is applied to an [`App`].
pub struct PluginGroupBuilder {
    plugins: Vec<Box<dyn Plugin>>,
    disabled: HashSet<TypeId>,
}

impl PluginGroupBuilder {
    /// Creates a new, empty builder for plugin group `G`.
    pub fn start<G: PluginGroup>() -> Self {
        Self {
            plugins: Vec::new(),
            disabled: HashSet::new(),
        }
    }

    /// Adds a plugin to the group.
    ///
    /// Plugins that have been [`disable`](Self::disable)d are intentionally
    /// skipped, even when they are added again later in the same builder chain.
    /// This supports the downstream override pattern:
    ///
    /// ```rust,ignore
    /// Defaults.build()
    ///     .disable::<OutputPlugin>()
    ///     .add(CustomOutputPlugin)
    /// ```
    ///
    /// To inspect wiring mistakes around a skipped plugin, register the group
    /// with [`App::try_add_plugins`](crate::App::try_add_plugins). If a later
    /// plugin has a [`Plugin::dependencies`] entry for the skipped type, the
    /// returned [`AppError::MissingDependencies`](crate::app::AppError::MissingDependencies)
    /// names the dependent plugin and missing dependency.
    #[allow(clippy::should_implement_trait)]
    pub fn add<P: Plugin>(mut self, plugin: P) -> Self {
        if self.disabled.contains(&TypeId::of::<P>()) {
            return self;
        }
        self.plugins.push(Box::new(plugin));
        self
    }

    /// Marks a plugin type as disabled so that subsequent [`add`](Self::add) calls
    /// for that type are ignored.
    pub fn disable<P: Plugin>(mut self) -> Self {
        self.disabled.insert(TypeId::of::<P>());
        self
    }

    /// Registers all collected plugins with the given [`App`].
    pub(crate) fn try_finish(self, app: &mut App) -> Result<(), crate::app::AppError> {
        for plugin in self.plugins {
            app.add_boxed_plugin(plugin)?;
        }
        Ok(())
    }

    /// Registers all collected plugins with the given [`App`].
    ///
    /// # Panics
    ///
    /// Panics if any plugin fails registration (duplicate or missing dependencies).
    pub(crate) fn finish(self, app: &mut App) {
        self.try_finish(app)
            .unwrap_or_else(|err| err.panic_with_context());
    }
}

// ─── StatesPlugin ─────────────────────────────────────────────────────────────

/// Registers [`CurrentState<S>`] and [`NextState<S>`] resources and wires up the
/// end-of-step transition system at the given schedule phase.
///
/// ```rust,ignore
/// #[derive(Clone, PartialEq, Default)]
/// enum Phase { #[default] Settling, Production }
///
/// App::new()
///     .add_plugins(StatesPlugin::new(Phase::Settling, ScheduleSet::PostFinalIntegration))
///     ...
/// ```
pub struct StatesPlugin<S: Clone + PartialEq + Default + Send + Sync + 'static> {
    /// The initial state value to use at simulation start.
    pub initial: S,
    /// The schedule phase at which state transitions are applied.
    phase: StoredPhase,
}

impl<S: Clone + PartialEq + Default + Send + Sync + 'static> StatesPlugin<S> {
    /// Creates a new [`StatesPlugin`] with the given initial state and schedule phase.
    pub fn new(initial: S, phase: impl ScheduleSet) -> Self {
        Self {
            initial,
            phase: StoredPhase::new(phase),
        }
    }
}

impl<S: Clone + PartialEq + Default + Send + Sync + 'static> Plugin for StatesPlugin<S> {
    fn build(&self, app: &mut App) {
        app.add_resource(CurrentState(self.initial.clone()));
        app.add_resource(NextState::<S>(None));
        app.add_update_system(apply_state_transitions::<S>, self.phase);
    }
}

// ─── StageAdvancePlugin ──────────────────────────────────────────────────────

/// Watches for [`CurrentState<S>`] changes and sets `SchedulerManager::advance_requested`.
///
/// Add alongside [`StatesPlugin`] when using `#[derive(StageEnum)]`:
///
/// ```rust,ignore
/// app.add_plugins(StatesPlugin::new(Phase::Settle, ScheduleSet::PostFinalIntegration));
/// app.add_plugins(StageAdvancePlugin::<Phase>::new(ScheduleSet::PostFinalIntegration));
/// ```
pub struct StageAdvancePlugin<S: StageName + Clone + PartialEq + Default + Send + Sync + 'static> {
    _marker: PhantomData<S>,
    phase: StoredPhase,
}

impl<S: StageName + Clone + PartialEq + Default + Send + Sync + 'static> StageAdvancePlugin<S> {
    /// Creates a new [`StageAdvancePlugin`] for state type `S` at the given schedule phase.
    pub fn new(phase: impl ScheduleSet) -> Self {
        Self {
            _marker: PhantomData,
            phase: StoredPhase::new(phase),
        }
    }
}

impl<S: StageName + Clone + PartialEq + Default + Send + Sync + 'static> Plugin
    for StageAdvancePlugin<S>
{
    fn build(&self, app: &mut App) {
        // Store stage names for validation and DOT export
        app.add_resource(StageNames(S::stage_names()));
        app.set_stage_names(S::stage_names());
        app.add_update_system(check_stage_advance::<S>, self.phase);
    }
}

/// Resource storing stage name strings, used for validation and DOT export.
pub struct StageNames(pub &'static [&'static str]);

// ─── Plugins sealed trait ─────────────────────────────────────────────────────

/// Types that represent a set of [`Plugin`]s.
///
/// This trait is implemented for:
/// - Individual types implementing [`Plugin`]
/// - Types implementing [`PluginGroup`]
/// - Tuples of [`Plugins`]
///
/// You do not need to implement this trait yourself.
pub trait Plugins<Marker>: sealed::Plugins<Marker> {}
impl<Marker, T> Plugins<Marker> for T where T: sealed::Plugins<Marker> {}

pub(crate) mod sealed {
    use crate::app::AppError;
    use crate::{App, Plugin, PluginGroup};

    pub trait Plugins<Marker> {
        fn try_add_to_app(self, app: &mut App) -> Result<(), AppError>;

        #[track_caller]
        fn add_to_app(self, app: &mut App);
    }

    pub struct PluginMarker;
    pub struct PluginGroupMarker;

    impl<P: Plugin> Plugins<PluginMarker> for P {
        fn try_add_to_app(self, app: &mut App) -> Result<(), AppError> {
            app.add_boxed_plugin(Box::new(self)).map(|_| ())
        }

        #[track_caller]
        fn add_to_app(self, app: &mut App) {
            self.try_add_to_app(app)
                .unwrap_or_else(|err| err.panic_with_context());
        }
    }

    impl<G: PluginGroup> Plugins<PluginGroupMarker> for G {
        fn try_add_to_app(self, app: &mut App) -> Result<(), AppError> {
            self.build().try_finish(app)
        }

        #[track_caller]
        fn add_to_app(self, app: &mut App) {
            self.build().finish(app);
        }
    }
}
