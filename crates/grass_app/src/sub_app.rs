//! The [`SubApp`] type — a self-contained scheduler with its own resource store.
//!
//! Currently every [`App`](crate::App) has exactly one `SubApp` (the "main"
//! sub-app). The abstraction exists to support future multi-world scenarios
//! (e.g. rendering in a separate sub-app).

use std::{
    any::{Any, TypeId},
    cell::RefCell,
    collections::HashSet,
};

use grass_scheduler::{IntoScheduledSystem, IntoSystem, ScheduleSet, Scheduler};

/// A self-contained simulation world: one [`Scheduler`] with its resource store
/// and system lists.
///
/// Most users interact with `SubApp` indirectly through [`App`](crate::App),
/// which delegates to its main `SubApp`.
#[derive(Default)]
pub struct SubApp {
    pub(crate) scheduler: Scheduler,
    /// The names of plugins that have been added to this sub-app (used to track
    /// duplicates and already-registered plugins).
    pub(crate) plugin_names: HashSet<String>,
    /// TypeIds of all registered plugins (used for TypeId-based dependency checks).
    pub(crate) plugin_type_ids: HashSet<TypeId>,
    /// Capability tags provided by registered plugins.
    pub(crate) provided_capabilities: HashSet<String>,
    /// Capability tags required by registered plugins: (capability, requiring plugin name).
    pub(crate) required_capabilities: Vec<(String, String)>,
}

impl SubApp {
    /// Creates a new, empty [`SubApp`] with default scheduler settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs the full simulation lifecycle: organize → setup → run.
    pub fn start(&mut self) {
        self.scheduler.start();
    }

    /// Sorts systems into their schedule-set execution order.
    pub fn organize_systems(&mut self) {
        self.scheduler.organize_systems();
    }

    /// Executes all setup-phase systems.
    pub fn setup(&mut self) {
        self.scheduler.setup();
    }

    /// Executes one iteration of the update loop: every update system runs once
    /// in schedule order. Mirrors what [`start`](Self::start)'s inner loop calls
    /// each tick. Use directly when an external orchestrator (e.g. `grass_multi`)
    /// owns the outer loop instead of the scheduler.
    pub fn run(&mut self) {
        self.scheduler.run();
    }

    /// One-shot setup for an externally-driven loop:
    /// `add_scheduler_manager` → `organize_systems` → `setup`. After this you
    /// can call [`run`](Self::run) repeatedly until [`is_done`](Self::is_done).
    pub fn prepare(&mut self) {
        use grass_scheduler::{SchedulerManager, SchedulerState};
        self.scheduler.add_scheduler_manager();
        self.scheduler.organize_systems();
        self.scheduler.setup();
        // Move SchedulerManager into the Run state so systems that gate on it
        // (e.g. `system_check_done`) behave the same as under `start()`.
        if let Some(cell) = self
            .scheduler
            .get_mut_resource(std::any::TypeId::of::<SchedulerManager>())
        {
            let mut br = cell.borrow_mut();
            if let Some(sm) = br.downcast_mut::<SchedulerManager>() {
                sm.state = SchedulerState::Run;
            }
        }
    }

    /// Returns `true` if a system has signalled simulation end via
    /// [`SchedulerManager::state`](grass_scheduler::SchedulerManager) == `End`.
    pub fn is_done(&self) -> bool {
        use grass_scheduler::{SchedulerManager, SchedulerState};
        self.scheduler
            .get_resource_ref::<SchedulerManager>()
            .map(|sm| matches!(sm.state, SchedulerState::End))
            .unwrap_or(false)
    }

    /// Same as [`get_mut_resource`](Self::get_mut_resource) but with shared
    /// (`&self`) receiver — see [`grass_scheduler::Scheduler::resource_cell`].
    pub fn resource_cell(&self, res: TypeId) -> Option<&RefCell<Box<dyn Any>>> {
        self.scheduler.resource_cell(res)
    }

    /// Registers a system to run during the setup phase at the given schedule phase.
    pub fn add_setup_system<M>(
        &mut self,
        system: impl IntoScheduledSystem<M>,
        schedule_set: impl ScheduleSet,
    ) {
        self.scheduler.add_setup_system(system, schedule_set);
    }

    /// Registers a system to run every timestep at the given schedule phase.
    pub fn add_update_system<M>(
        &mut self,
        system: impl IntoScheduledSystem<M>,
        schedule_set: impl ScheduleSet,
    ) {
        self.scheduler.add_update_system(system, schedule_set);
    }

    /// Assigns a namespace to all systems registered under the given phase enum type.
    pub fn set_schedule_namespace<P: ScheduleSet + 'static>(&mut self, namespace: u32) {
        self.scheduler.set_schedule_namespace::<P>(namespace);
    }

    /// Install a hierarchical [`grass_scheduler::Schedule`] on this sub-app.
    /// Call after all systems are registered. See [`Scheduler::set_schedule`]
    /// for lowering details.
    pub fn set_schedule(&mut self, schedule: grass_scheduler::Schedule) {
        self.scheduler.set_schedule(schedule);
    }

    /// Inserts a resource into this sub-app's resource store.
    pub fn add_resource<R: 'static>(&mut self, res: R) {
        self.scheduler.add_resource(res);
    }

    /// Returns a mutable reference to the raw resource cell for the given [`TypeId`].
    pub fn get_mut_resource(&mut self, res: TypeId) -> Option<&RefCell<Box<dyn Any>>> {
        self.scheduler.get_mut_resource(res)
    }

    /// Returns a borrowed reference to a resource of type `R`, or `None` if absent.
    pub fn get_resource_ref<R: 'static>(&self) -> Option<std::cell::Ref<'_, R>> {
        self.scheduler.get_resource_ref::<R>()
    }

    /// Removes an update system by its concrete type.
    pub fn remove_update_system<I, S: grass_scheduler::System + 'static>(
        &mut self,
        system: impl IntoSystem<I, System = S>,
    ) {
        self.scheduler.remove_update_system(system);
    }

    /// Returns `true` if a system with the same name as `system` is
    /// already registered as an update system. Useful in plugins for
    /// "register only if the user hasn't already done it" guards.
    pub fn has_update_system<I, S: grass_scheduler::System + 'static>(
        &self,
        system: impl IntoSystem<I, System = S>,
    ) -> bool {
        self.scheduler.has_update_system(system)
    }

    /// Removes an update system identified by its string label.
    pub fn remove_update_system_by_label(&mut self, label: &str) {
        self.scheduler.remove_update_system_by_label(label);
    }

    /// Enables printing the organized schedule to stdout during setup.
    pub fn enable_schedule_print(&mut self) {
        self.scheduler.enable_schedule_print();
    }

    /// Sets human-readable stage names for multi-stage simulations.
    pub fn set_stage_names(&mut self, names: &[&str]) {
        self.scheduler.set_stage_names(names);
    }

    /// Registers a callback that produces domain-specific schedule warnings.
    pub fn set_warning_fn(&mut self, f: impl Fn(&[&str]) -> Vec<String> + 'static) {
        self.scheduler.set_warning_fn(f);
    }
}

/// The collection of sub-apps that belong to an [`App`](crate::App).
#[derive(Default)]
pub struct SubApps {
    /// The primary sub-app that contains the "main" world.
    pub main: SubApp,
}
