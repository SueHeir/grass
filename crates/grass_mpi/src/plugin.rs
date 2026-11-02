//! [`MpiPlugin`]: the one place a GRASS application gets its communicator.
//!
//! Every library that talks across ranks (SOIL, FIELD, a coupler, a new
//! physics tier) adds this plugin, or depends on it, instead of building its
//! own [`CommResource`]. The first one to be added creates the communicator;
//! the rest reuse it. So libraries written separately share one communicator
//! in one App with no edits to either of them.
//!
//! To run on a custom communicator (for example one split off for a
//! sub-group of ranks), add it before any library plugin:
//!
//! ```
//! use grass_app::prelude::*;
//! use grass_mpi::{CommResource, MpiPlugin, SingleProcessComm};
//!
//! let mut app = App::new();
//! app.init_shared_resource(|| CommResource(Box::new(SingleProcessComm::new())));
//! app.add_plugins(MpiPlugin); // reuses the one above
//! ```

use grass_app::prelude::*;

use crate::CommResource;

/// Installs the shared [`CommResource`] if no plugin has yet, and provides the
/// `"comm"` capability.
///
/// With the `mpi_backend` feature the communicator is
/// [`get_mpi_world`](crate::get_mpi_world) (the color-split intra-communicator
/// when [`init_app_color`](crate::init_app_color) was called first); without
/// it, a [`SingleProcessComm`]. Adding it more than once is harmless.
pub struct MpiPlugin;

impl MpiPlugin {
    /// The capability name this plugin provides.
    pub const CAPABILITY: &'static str = "comm";
}

/// The communicator [`MpiPlugin`] installs when none exists yet.
pub fn default_comm() -> CommResource {
    #[cfg(feature = "mpi_backend")]
    {
        CommResource(Box::new(crate::MpiCommBackend::new(crate::get_mpi_world())))
    }
    #[cfg(not(feature = "mpi_backend"))]
    {
        CommResource(Box::new(crate::SingleProcessComm::new()))
    }
}

impl Plugin for MpiPlugin {
    fn build(&self, app: &mut App) {
        // MPI is finalized at process exit by whoever started it (see
        // `register_finalize_at_exit`), never by one App's cleanup: with
        // coupled sub-Apps, or Apps run one after another, an App-owned
        // finalize takes MPI down under every other App in the process.
        app.init_shared_resource(default_comm);
    }

    fn provides(&self) -> Vec<&str> {
        vec![Self::CAPABILITY]
    }

    /// Every library that needs the communicator adds this plugin itself, so
    /// it must be addable more than once. Only the first add creates anything.
    fn is_unique(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommBackend, SingleProcessComm};

    struct LibA;
    impl Plugin for LibA {
        fn build(&self, app: &mut App) {
            app.add_plugins(MpiPlugin);
        }
    }
    struct LibB;
    impl Plugin for LibB {
        fn build(&self, app: &mut App) {
            app.add_plugins(MpiPlugin);
        }
        fn requires(&self) -> Vec<&str> {
            vec!["comm"]
        }
    }

    /// With the real backend, creating the communicator would initialize MPI
    /// inside the test process and break the tests that launch `mpirun`, so
    /// those builds start from a serial communicator installed up front.
    fn app() -> App {
        let mut app = App::new();
        if cfg!(feature = "mpi_backend") {
            app.init_shared_resource(|| CommResource(Box::new(SingleProcessComm::new())));
        }
        app
    }

    #[test]
    fn two_libraries_share_one_communicator() {
        let mut app = app();
        app.add_plugins(LibA).add_plugins(LibB);
        assert!(app.has_resource::<CommResource>());
        app.validate_capability_contracts_result()
            .expect("LibB's \"comm\" requirement is met by MpiPlugin");
    }

    #[test]
    fn a_communicator_added_first_is_kept() {
        let mut app = App::new();
        // Mark the application's own communicator so the test can tell it apart.
        let mut mine = SingleProcessComm::new();
        mine.set_processor_grid([7, 1, 1], [0, 0, 0]);
        app.init_shared_resource(move || CommResource(Box::new(mine)));
        app.add_plugins(LibA).add_plugins(LibB);
        let comm = app.get_resource_ref::<CommResource>().unwrap();
        assert_eq!(comm.processor_decomposition(), [7, 1, 1]);
    }

    #[test]
    #[should_panic(expected = "shared resource")]
    fn overwriting_the_shared_communicator_is_refused() {
        let mut app = app();
        app.add_plugins(MpiPlugin);
        app.add_resource(CommResource(Box::new(SingleProcessComm::new())));
    }
}
