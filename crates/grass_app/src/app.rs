//! The central [`App`] container and its supporting types.
//!
//! [`App`] is the entry point for every simulation. It owns the main
//! [`SubApp`], coordinates plugin registration, and drives the simulation
//! lifecycle (setup → run → cleanup).
//!
//! # Typical usage
//!
//! ```rust,ignore
//! use grass_app::prelude::*;
//!
//! App::new()
//!     .add_plugins(MyPlugins)
//!     .start();
//! ```

use std::{
    any::{Any, TypeId},
    cell::RefCell,
    fmt,
};

use grass_scheduler::{IntoScheduledSystem, IntoSystem, ScheduleSet};

use crate::{Plugin, Plugins, SubApp, SubApps};

/// Collected TOML snippets from all plugins that implement [`Plugin::default_config`].
///
/// This resource is automatically populated during plugin registration. When the
/// [`GenerateConfigFlag`] resource is present, [`App::start`] prints these
/// snippets to stdout and exits.
pub struct ConfigSnippets {
    /// The accumulated TOML snippet strings, one per plugin.
    pub snippets: Vec<String>,
}

/// Marker resource: when present, [`App::start`] prints config snippets and exits
/// instead of running the simulation.
///
/// Add this resource (e.g. via a `--generate-config` CLI flag) to have the app
/// emit a complete example configuration file assembled from all registered plugins.
pub struct GenerateConfigFlag;

/// Central application container. Holds resources, systems, and plugins.
///
/// `App` provides a builder-style API for assembling a simulation from plugins.
/// Most methods return `&mut Self` so calls can be chained:
///
/// ```rust,ignore
/// App::new()
///     .add_plugins(PhysicsPlugins)
///     .add_resource(MyConfig { dt: 0.001 })
///     .add_update_system(my_system, ScheduleSet::Update)
///     .start();
/// ```
pub struct App {
    pub(crate) sub_apps: SubApps,
    cleanup_fns: Vec<Box<dyn FnOnce()>>,
    #[allow(clippy::type_complexity)]
    cleanup_with_app_fns: Vec<Box<dyn FnOnce(&mut App)>>,
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

impl App {
    /// Creates a new, empty [`App`] with default structure.
    ///
    /// This is the preferred constructor for most use cases. After creation,
    /// add plugins via [`add_plugins`](Self::add_plugins) and start the
    /// simulation with [`start`](Self::start).
    pub fn new() -> App {
        Self {
            sub_apps: SubApps {
                main: SubApp::new(),
            },
            cleanup_fns: Vec::new(),
            cleanup_with_app_fns: Vec::new(),
        }
    }

    /// Registers one or more plugins with this app.
    ///
    /// Accepts any type implementing [`Plugin`], [`PluginGroup`](crate::PluginGroup),
    /// or a tuple of plugins.
    ///
    /// # Panics
    ///
    /// Panics if a unique plugin is added twice or if plugin dependencies are
    /// not satisfied. The panic message includes guidance on how to fix the
    /// registration order.
    pub fn add_plugins<M>(&mut self, plugins: impl Plugins<M>) -> &mut Self {
        plugins.add_to_app(self);
        self
    }

    /// Fallibly registers one or more plugins with this app.
    ///
    /// Accepts the same inputs as [`add_plugins`](Self::add_plugins), but
    /// returns an [`AppError`] instead of panicking when a unique plugin is
    /// added twice or a TypeId dependency has not been registered yet. Use this
    /// in applications that need to surface plugin-wiring diagnostics through
    /// their own CLI, GUI, or test harness.
    pub fn try_add_plugins<M>(&mut self, plugins: impl Plugins<M>) -> Result<&mut Self, AppError> {
        plugins.try_add_to_app(self)?;
        Ok(self)
    }

    /// Internal: adds a boxed plugin, checking uniqueness and dependencies.
    pub(crate) fn add_boxed_plugin(
        &mut self,
        plugin: Box<dyn Plugin>,
    ) -> Result<&mut Self, AppError> {
        if plugin.is_unique() && self.main_mut().plugin_names.contains(plugin.name()) {
            return Err(AppError::DuplicatePlugin {
                plugin_name: plugin.name().to_string(),
            });
        }

        self.validate_dependencies(&*plugin)?;

        // Record the plugin's TypeId for TypeId-based dependency checks.
        let plugin_type_id = (*plugin).type_id();
        self.main_mut().plugin_type_ids.insert(plugin_type_id);

        // Record the plugin name *before* build so that nested add_plugins calls
        // within build() can see this plugin as registered (prevents false-positive
        // dependency errors when a plugin group adds a dependency and its dependent
        // in sequence).
        let plugin_name = plugin.name().to_string();
        self.main_mut().plugin_names.insert(plugin_name.clone());

        plugin.build(self);

        self.collect_config_snippet(&*plugin);

        // Collect capability contracts after build.
        for cap in plugin.provides() {
            self.main_mut()
                .provided_capabilities
                .insert(cap.to_string());
        }
        for cap in plugin.requires() {
            self.main_mut()
                .required_capabilities
                .push((cap.to_string(), plugin_name.clone()));
        }

        Ok(self)
    }

    /// Checks that all plugins listed in `plugin.dependencies()` (by [`TypeId`])
    /// have already been registered. Returns `Err(AppError::MissingDependencies)`
    /// if any are missing.
    fn validate_dependencies(&self, plugin: &dyn Plugin) -> Result<(), AppError> {
        let deps = plugin.dependencies();
        if deps.is_empty() {
            return Ok(());
        }

        let dependency_names = plugin.dependency_names();
        let missing: Vec<MissingPluginDependency> = deps
            .into_iter()
            .enumerate()
            .filter(|(_, dep)| !self.main().plugin_type_ids.contains(dep))
            .map(|(idx, type_id)| MissingPluginDependency {
                type_id,
                name: dependency_names.get(idx).map(|name| (*name).to_string()),
            })
            .collect();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(AppError::MissingDependencies {
                plugin_name: plugin.name().to_string(),
                missing,
            })
        }
    }

    /// Fallibly validates that every required capability tag has at least one provider.
    ///
    /// This is the capability-contract counterpart to
    /// [`try_add_plugins`](Self::try_add_plugins): TypeId dependencies are checked
    /// eagerly while plugins register, but capability tags are checked lazily once
    /// all providers and requirements have been collected. External drivers can call
    /// this before [`try_prepare`](Self::try_prepare) or [`try_start`](Self::try_start)
    /// when they need to surface wiring diagnostics without panicking.
    pub fn validate_capability_contracts_result(&self) -> Result<(), AppError> {
        let provided = &self.main().provided_capabilities;
        let required = &self.main().required_capabilities;

        let missing: Vec<_> = required
            .iter()
            .filter(|(cap, _)| !provided.contains(cap))
            .map(|(cap, plugin_name)| MissingCapability {
                capability: cap.clone(),
                requiring_plugin: plugin_name.clone(),
            })
            .collect();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(AppError::MissingCapabilities { missing })
        }
    }

    /// Validates capability contracts and panics with an actionable diagnostic on failure.
    ///
    /// This preserves the convenience path used by [`prepare`](Self::prepare) and
    /// [`start`](Self::start). Use
    /// [`validate_capability_contracts_result`](Self::validate_capability_contracts_result)
    /// when an external driver should receive an [`AppError`] instead.
    #[track_caller]
    pub fn validate_capability_contracts(&self) {
        if let Err(err) = self.validate_capability_contracts_result() {
            err.panic_with_context();
        }
    }

    /// If the plugin provides a [`Plugin::default_config`] snippet, appends it
    /// to the [`ConfigSnippets`] resource (creating the resource if needed).
    fn collect_config_snippet(&mut self, plugin: &dyn Plugin) {
        let Some(snippet) = plugin.default_config() else {
            return;
        };
        let snippet = snippet.to_string();

        if let Some(cell) = self.get_mut_resource(TypeId::of::<ConfigSnippets>()) {
            let mut borrow = cell.borrow_mut();
            let snippets = borrow
                .downcast_mut::<ConfigSnippets>()
                .expect("ConfigSnippets resource has wrong type — this is a framework bug");
            snippets.snippets.push(snippet);
        } else {
            self.add_resource(ConfigSnippets {
                snippets: vec![snippet],
            });
        }
    }

    /// Returns a reference to the main [`SubApp`].
    pub fn main(&self) -> &SubApp {
        &self.sub_apps.main
    }

    /// Returns a mutable reference to the main [`SubApp`].
    pub fn main_mut(&mut self) -> &mut SubApp {
        &mut self.sub_apps.main
    }

    /// Organizes registered systems into their schedule-set order.
    ///
    /// Called automatically by [`start`](Self::start); you only need this if
    /// you are manually driving the setup/run cycle.
    pub fn organize_systems(&mut self) {
        self.sub_apps.main.organize_systems();
    }

    /// Runs all setup systems in their schedule-setup-set order.
    ///
    /// Called automatically by [`start`](Self::start).
    pub fn setup(&mut self) -> &mut Self {
        self.sub_apps.main.setup();
        self
    }

    /// Runs the main simulation loop (all update systems each timestep).
    ///
    /// Called automatically by [`start`](Self::start).
    pub fn run(&mut self) -> &mut Self {
        self.sub_apps.main.run();
        self
    }

    /// Registers a system to run during the setup phase at the given schedule phase.
    pub fn add_setup_system<M>(
        &mut self,
        system: impl IntoScheduledSystem<M>,
        schedule_set: impl ScheduleSet,
    ) -> &mut Self {
        self.sub_apps.main.add_setup_system(system, schedule_set);
        self
    }

    /// Registers a system to run every timestep at the given schedule phase.
    pub fn add_update_system<M>(
        &mut self,
        system: impl IntoScheduledSystem<M>,
        schedule_set: impl ScheduleSet,
    ) -> &mut Self {
        self.sub_apps.main.add_update_system(system, schedule_set);
        self
    }

    /// Assigns a namespace to all systems registered under the given phase enum type.
    ///
    /// Systems sort by `(namespace, index)`, so this controls cross-solver ordering.
    pub fn set_schedule_namespace<P: ScheduleSet + 'static>(
        &mut self,
        namespace: u32,
    ) -> &mut Self {
        self.sub_apps.main.set_schedule_namespace::<P>(namespace);
        self
    }

    /// Install a hierarchical [`grass_scheduler::Schedule`] on this App's
    /// main sub-app. Call after all plugins / systems have been added — the
    /// scheduler walks the tree at install time to assign namespaces and
    /// prepare loop conditions, so adding more systems afterwards leaves
    /// them outside the schedule.
    pub fn set_schedule(&mut self, schedule: grass_scheduler::Schedule) -> &mut Self {
        self.sub_apps.main.set_schedule(schedule);
        self
    }

    /// Inserts a resource into the app's resource store.
    ///
    /// If a resource of the same type already exists, it is replaced.
    pub fn add_resource<R: 'static>(&mut self, res: R) -> &mut Self {
        self.sub_apps.main.add_resource(res);
        self
    }

    /// Returns a mutable reference to the raw resource cell for the given [`TypeId`],
    /// or `None` if no resource of that type exists.
    pub fn get_mut_resource(&mut self, res: TypeId) -> Option<&RefCell<Box<dyn Any>>> {
        self.sub_apps.main.get_mut_resource(res)
    }

    /// Returns a borrowed reference to a resource of type `R`, or `None` if it
    /// has not been added.
    pub fn get_resource_ref<R: 'static>(&self) -> Option<std::cell::Ref<'_, R>> {
        self.sub_apps.main.get_resource_ref::<R>()
    }

    /// Same as [`get_mut_resource`](Self::get_mut_resource) but with shared
    /// (`&self`) receiver. Lets external orchestrators (`grass_multi`) hold
    /// references to multiple resources at once for couplers that read several
    /// fields in one expression.
    pub fn resource_cell(&self, res: TypeId) -> Option<&RefCell<Box<dyn Any>>> {
        self.sub_apps.main.resource_cell(res)
    }

    /// One-shot setup for an externally-driven loop. Validates capability
    /// contracts, then runs `add_scheduler_manager` → `organize_systems` →
    /// `setup` and transitions the `SchedulerManager` to `Run`. After this
    /// you can call [`run`](Self::run) (or `main_mut().run()`) repeatedly
    /// until [`is_done`](Self::is_done).
    ///
    /// Use [`start`](Self::start) for the simple "run until done"
    /// lifecycle; use this when something else (e.g. a parent `App`
    /// driving sub-Apps via [`tick_subapp`](https://docs.rs/grass_multi))
    /// owns the outer loop.
    pub fn prepare(&mut self) -> &mut Self {
        self.validate_capability_contracts();
        self.sub_apps.main.prepare();
        self
    }

    /// Fallible form of [`prepare`](Self::prepare).
    ///
    /// Returns [`AppError::MissingCapabilities`] instead of panicking when a
    /// required capability tag has no registered provider.
    pub fn try_prepare(&mut self) -> Result<&mut Self, AppError> {
        self.validate_capability_contracts_result()?;
        self.sub_apps.main.prepare();
        Ok(self)
    }

    /// Returns `true` if a system has signalled simulation end via
    /// [`SchedulerManager`](grass_scheduler::SchedulerManager).
    pub fn is_done(&self) -> bool {
        self.sub_apps.main.is_done()
    }

    /// Runs all registered cleanup functions (drains the list). Called
    /// automatically by [`start`](Self::start). External orchestrators that
    /// drive the loop themselves must call this once after the loop ends.
    ///
    /// Resource-aware cleanups (registered via [`add_cleanup_with_app`](Self::add_cleanup_with_app))
    /// run **first**, then resource-free cleanups (registered via
    /// [`add_cleanup`](Self::add_cleanup)). This ordering matters when a
    /// resource-aware cleanup writes output that depends on resources still
    /// being intact, before a resource-free cleanup tears those down (e.g.
    /// `grass_mpi::finalize_mpi`).
    pub fn run_cleanup(&mut self) {
        let with_app: Vec<_> = std::mem::take(&mut self.cleanup_with_app_fns);
        for f in with_app {
            f(self);
        }
        for f in self.cleanup_fns.drain(..) {
            f();
        }
    }

    /// Registers a cleanup function that will run after the simulation finishes
    /// (or after config generation). Cleanup functions run in registration order.
    pub fn add_cleanup(&mut self, f: fn()) -> &mut Self {
        self.cleanup_fns.push(Box::new(f));
        self
    }

    /// Registers a cleanup closure that runs after the simulation finishes and
    /// receives `&mut App` so it can pull resources / inspect state. Use this
    /// from a plugin's `build` to wire up final-output writes that need access
    /// to e.g. `FlowField` / `Grid`. Resource-aware cleanups run before plain
    /// cleanups (see [`run_cleanup`](Self::run_cleanup)).
    pub fn add_cleanup_with_app<F>(&mut self, f: F) -> &mut Self
    where
        F: FnOnce(&mut App) + 'static,
    {
        self.cleanup_with_app_fns.push(Box::new(f));
        self
    }

    /// Starts the simulation lifecycle.
    ///
    /// If the [`GenerateConfigFlag`] resource is present, prints all collected
    /// config snippets to stdout and exits. Otherwise, runs
    /// [`organize_systems`](Self::organize_systems) → setup → run → cleanup.
    pub fn start(&mut self) {
        if self.get_resource_ref::<GenerateConfigFlag>().is_some() {
            self.print_generated_config();
            self.run_cleanup();
            return;
        }
        self.validate_capability_contracts();
        self.sub_apps.main.start();
        self.run_cleanup();
    }

    /// Fallible form of [`start`](Self::start).
    ///
    /// Preserves the config-generation behavior of [`start`](Self::start), but
    /// returns [`AppError::MissingCapabilities`] instead of panicking when
    /// capability contracts are unsatisfied.
    pub fn try_start(&mut self) -> Result<(), AppError> {
        if self.get_resource_ref::<GenerateConfigFlag>().is_some() {
            self.print_generated_config();
            self.run_cleanup();
            return Ok(());
        }
        self.validate_capability_contracts_result()?;
        self.sub_apps.main.start();
        self.run_cleanup();
        Ok(())
    }

    /// Prints accumulated config snippets from all registered plugins.
    fn print_generated_config(&self) {
        let Some(snippets) = self.get_resource_ref::<ConfigSnippets>() else {
            return;
        };
        println!("# Generated configuration");
        println!("# Default values for all registered plugins\n");
        for snippet in &snippets.snippets {
            println!("{}", snippet.trim());
            println!();
        }
    }

    /// Removes an update system by its concrete type.
    pub fn remove_update_system<I, S: grass_scheduler::System + 'static>(
        &mut self,
        system: impl IntoSystem<I, System = S>,
    ) -> &mut Self {
        self.sub_apps.main.remove_update_system(system);
        self
    }

    /// Returns `true` if a system with the same name as `system` is
    /// already registered as an update system on this App's main
    /// sub-App. Plugins use this to guard against duplicate
    /// auto-registration when the user has already wired the system
    /// manually.
    pub fn has_update_system<I, S: grass_scheduler::System + 'static>(
        &self,
        system: impl IntoSystem<I, System = S>,
    ) -> bool {
        self.sub_apps.main.has_update_system(system)
    }

    /// Removes an update system identified by its string label.
    pub fn remove_update_system_by_label(&mut self, label: &str) -> &mut Self {
        self.sub_apps.main.remove_update_system_by_label(label);
        self
    }

    /// Enables printing the organized schedule to stdout during setup.
    /// Useful for debugging system ordering.
    pub fn enable_schedule_print(&mut self) -> &mut Self {
        self.sub_apps.main.enable_schedule_print();
        self
    }

    /// Sets human-readable stage names for multi-stage simulations.
    pub fn set_stage_names(&mut self, names: &[&str]) -> &mut Self {
        self.sub_apps.main.set_stage_names(names);
        self
    }

    /// Registers a callback that produces domain-specific schedule warnings.
    pub fn set_warning_fn(&mut self, f: impl Fn(&[&str]) -> Vec<String> + 'static) -> &mut Self {
        self.sub_apps.main.set_warning_fn(f);
        self
    }
}

/// Error returned by fallible app assembly and lifecycle validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// A unique plugin was registered more than once.
    DuplicatePlugin {
        /// The duplicate plugin's human-readable name.
        plugin_name: String,
    },
    /// One or more required dependency plugins (by [`TypeId`]) have not been registered yet.
    MissingDependencies {
        /// The plugin that could not be registered.
        plugin_name: String,
        /// Dependencies that were not registered before this plugin.
        missing: Vec<MissingPluginDependency>,
    },
    /// One or more required capability tags have no provider.
    MissingCapabilities {
        /// Required capability tags with the plugins that required them.
        missing: Vec<MissingCapability>,
    },
}

impl AppError {
    /// Panic with the same diagnostic style used by [`App::add_plugins`].
    #[track_caller]
    pub(crate) fn panic_with_context(self) -> ! {
        match self {
            AppError::DuplicatePlugin { plugin_name } => {
                panic!("Error adding plugin {plugin_name}: plugin was already added in application")
            }
            AppError::MissingDependencies {
                plugin_name,
                missing,
            } => {
                eprintln!();
                eprintln!(
                    "ERROR: Plugin `{}` is missing required dependencies:",
                    plugin_name
                );
                for dep in &missing {
                    eprintln!("  - {}", dep);
                }
                eprintln!();
                eprintln!(
                    "  Hint: Add the missing plugin(s) before `{}`.",
                    plugin_name
                );
                panic!(
                    "Missing plugin dependencies for `{}`: {}",
                    plugin_name,
                    format_missing_dependencies(&missing)
                );
            }
            AppError::MissingCapabilities { missing } => {
                eprintln!();
                eprintln!("ERROR: Missing capability contracts:");
                for cap in &missing {
                    eprintln!("  - {}", cap);
                }
                eprintln!();
                eprintln!("  Hint: Add plugin(s) that provide the required capabilities.");
                panic!(
                    "Missing capabilities: {}",
                    format_missing_capabilities(&missing)
                );
            }
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::DuplicatePlugin { plugin_name } => write!(
                f,
                "Error adding plugin {plugin_name}: plugin was already added in application"
            ),
            AppError::MissingDependencies {
                plugin_name,
                missing,
            } => write!(
                f,
                "Plugin `{plugin_name}` is missing required dependencies: {}",
                format_missing_dependencies(missing)
            ),
            AppError::MissingCapabilities { missing } => write!(
                f,
                "Missing capability contracts: {}. Add plugin(s) that provide the required capabilities.",
                format_missing_capabilities(missing)
            ),
        }
    }
}

impl std::error::Error for AppError {}

/// A missing plugin dependency reported by [`AppError::MissingDependencies`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingPluginDependency {
    /// The [`TypeId`] returned by [`Plugin::dependencies`] for this dependency.
    pub type_id: TypeId,
    /// Human-readable dependency name, when the plugin provided one via
    /// [`Plugin::dependency_names`].
    pub name: Option<String>,
}

/// A required capability tag that has no registered provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingCapability {
    /// The capability tag returned by [`Plugin::requires`].
    pub capability: String,
    /// The plugin that required this capability tag.
    pub requiring_plugin: String,
}

impl fmt::Display for MissingCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "capability `{}` required by `{}`",
            self.capability, self.requiring_plugin
        )
    }
}

impl fmt::Display for MissingPluginDependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => f.write_str(name),
            None => write!(f, "{:?}", self.type_id),
        }
    }
}

fn format_missing_dependencies(missing: &[MissingPluginDependency]) -> String {
    missing
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_missing_capabilities(missing: &[MissingCapability]) -> String {
    missing
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{dependency_names, type_ids, Plugin};

    struct PluginAInstalled;

    struct PluginA;
    impl Plugin for PluginA {
        fn build(&self, app: &mut App) {
            app.add_resource(PluginAInstalled);
        }
        fn provides(&self) -> Vec<&str> {
            vec!["feature_a"]
        }
    }

    struct PluginB;
    impl Plugin for PluginB {
        fn build(&self, _app: &mut App) {}
        fn dependencies(&self) -> Vec<TypeId> {
            type_ids![PluginA]
        }
        fn dependency_names(&self) -> Vec<&'static str> {
            dependency_names![PluginA]
        }
        fn requires(&self) -> Vec<&str> {
            vec!["feature_a"]
        }
    }

    struct PluginC;
    impl Plugin for PluginC {
        fn build(&self, _app: &mut App) {}
        fn requires(&self) -> Vec<&str> {
            vec!["feature_missing"]
        }
    }

    struct PluginD;
    impl Plugin for PluginD {
        fn build(&self, _app: &mut App) {}
        fn requires(&self) -> Vec<&str> {
            vec!["feature_missing_d"]
        }
    }

    #[test]
    fn satisfied_dependencies_and_capabilities() {
        let mut app = App::new();
        app.add_plugins(PluginA);
        app.add_plugins(PluginB);
        // If we got here without panic, deps + capabilities are satisfied at build time.
        // Validate capability contracts explicitly.
        app.validate_capability_contracts(); // should not panic
    }

    #[test]
    #[should_panic(expected = "Missing plugin dependencies")]
    fn missing_typeid_dependency_panics() {
        let mut app = App::new();
        // PluginB depends on PluginA which is not registered.
        app.add_plugins(PluginB);
    }

    #[test]
    fn duplicate_plugin_try_add_returns_error() {
        let mut app = App::new();
        app.try_add_plugins(PluginA).unwrap();

        let err = app.try_add_plugins(PluginA).err().unwrap();

        assert!(matches!(err, AppError::DuplicatePlugin { .. }));
        let msg = err.to_string();
        assert!(msg.contains("PluginA"));
        assert!(msg.contains("already added"));
    }

    #[test]
    fn missing_dependency_try_add_names_plugin_and_dependency() {
        let mut app = App::new();

        let err = app.try_add_plugins(PluginB).err().unwrap();

        match &err {
            AppError::MissingDependencies {
                plugin_name,
                missing,
            } => {
                assert!(plugin_name.contains("PluginB"));
                assert_eq!(missing.len(), 1);
                assert!(missing[0]
                    .name
                    .as_deref()
                    .is_some_and(|name| name.contains("PluginA")));
            }
            other => panic!("expected missing dependency error, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("PluginB"));
        assert!(msg.contains("PluginA"));
    }

    struct DuplicateGroup;
    impl crate::PluginGroup for DuplicateGroup {
        fn build(self) -> crate::PluginGroupBuilder {
            crate::PluginGroupBuilder::start::<Self>()
                .add(PluginA)
                .add(PluginA)
        }
    }

    #[test]
    fn plugin_group_try_add_returns_first_registration_error() {
        let mut app = App::new();

        let err = app.try_add_plugins(DuplicateGroup).err().unwrap();

        assert!(matches!(err, AppError::DuplicatePlugin { .. }));
        assert!(err.to_string().contains("PluginA"));
    }

    #[test]
    #[should_panic(expected = "already added in application")]
    fn plugin_group_add_plugins_still_panics() {
        let mut app = App::new();
        app.add_plugins(DuplicateGroup);
    }

    struct DisableThenReaddGroup;
    impl crate::PluginGroup for DisableThenReaddGroup {
        fn build(self) -> crate::PluginGroupBuilder {
            crate::PluginGroupBuilder::start::<Self>()
                .disable::<PluginA>()
                .add(PluginA)
        }
    }

    #[test]
    fn plugin_group_disable_then_add_skips_that_plugin() {
        let mut app = App::new();

        app.try_add_plugins(DisableThenReaddGroup).unwrap();

        assert!(app.get_resource_ref::<PluginAInstalled>().is_none());
    }

    struct DisableDependencyThenAddDependentGroup;
    impl crate::PluginGroup for DisableDependencyThenAddDependentGroup {
        fn build(self) -> crate::PluginGroupBuilder {
            crate::PluginGroupBuilder::start::<Self>()
                .disable::<PluginA>()
                .add(PluginA)
                .add(PluginB)
        }
    }

    #[test]
    fn disabled_plugin_skip_is_inspectable_through_dependent_error() {
        let mut app = App::new();

        let err = app
            .try_add_plugins(DisableDependencyThenAddDependentGroup)
            .err()
            .unwrap();

        match &err {
            AppError::MissingDependencies {
                plugin_name,
                missing,
            } => {
                assert!(plugin_name.contains("PluginB"));
                assert_eq!(missing.len(), 1);
                assert!(missing[0]
                    .name
                    .as_deref()
                    .is_some_and(|name| name.contains("PluginA")));
            }
            other => panic!("expected missing dependency error, got {other:?}"),
        }
        assert!(err.to_string().contains("PluginA"));
    }

    #[test]
    #[should_panic(expected = "Missing capabilities")]
    fn missing_capability_panics() {
        let mut app = App::new();
        app.add_plugins(PluginC);
        app.validate_capability_contracts();
    }

    #[test]
    fn missing_capability_result_names_required_capability_and_plugin() {
        let mut app = App::new();
        app.add_plugins(PluginC);
        app.add_plugins(PluginD);

        let err = app.validate_capability_contracts_result().err().unwrap();

        match &err {
            AppError::MissingCapabilities { missing } => {
                assert_eq!(missing.len(), 2);
                assert!(missing.iter().any(|cap| {
                    cap.capability == "feature_missing" && cap.requiring_plugin.contains("PluginC")
                }));
                assert!(missing.iter().any(|cap| {
                    cap.capability == "feature_missing_d"
                        && cap.requiring_plugin.contains("PluginD")
                }));
            }
            other => panic!("expected missing capability error, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("feature_missing"));
        assert!(msg.contains("PluginC"));
        assert!(msg.contains("provide"));
    }

    #[test]
    fn try_prepare_returns_missing_capability_error_without_panicking() {
        let mut app = App::new();
        app.add_plugins(PluginC);

        let err = app.try_prepare().err().unwrap();

        assert!(matches!(err, AppError::MissingCapabilities { .. }));
    }

    #[test]
    fn try_start_returns_missing_capability_error_without_panicking() {
        let mut app = App::new();
        app.add_plugins(PluginC);

        let err = app.try_start().err().unwrap();

        assert!(matches!(err, AppError::MissingCapabilities { .. }));
    }

    #[test]
    fn type_ids_macro_produces_correct_ids() {
        let ids = type_ids![PluginA, PluginB];
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], TypeId::of::<PluginA>());
        assert_eq!(ids[1], TypeId::of::<PluginB>());
    }
}
