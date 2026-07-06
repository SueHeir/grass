//! Lifecycle checks for `init_app_color`.
//!
//! Each scenario runs in its own child process because MPI initialization and
//! finalization are process-global.
#![cfg(feature = "mpi_backend")]

use std::process::Command;

use grass_mpi::{
    finalize_mpi, get_mpi_world, init_app_color, try_init_app_color, InitAppColorError,
};
use mpi::topology::Communicator;

const MODE_ENV: &str = "GRASS_MPI_INIT_APP_COLOR_LIFECYCLE";

fn same_color_is_idempotent() {
    init_app_color(2);
    init_app_color(2);
    try_init_app_color(2).expect("same color should remain idempotent");

    let world = get_mpi_world();
    assert!(world.size() >= 1, "communicator size must be positive");
    drop(world);
    finalize_mpi();
}

fn different_color_is_rejected() {
    init_app_color(3);

    let err = try_init_app_color(4).expect_err("different color must be rejected");
    assert_eq!(
        err,
        InitAppColorError::DifferentColor {
            existing: 3,
            requested: 4,
        }
    );
    assert!(
        err.to_string().contains("idempotent for the same color"),
        "error should explain the same-color-only idempotence contract: {err}"
    );

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let panic = std::panic::catch_unwind(|| init_app_color(4));
    std::panic::set_hook(hook);
    assert!(
        panic.is_err(),
        "init_app_color should panic on a different second color"
    );
    finalize_mpi();
}

fn after_get_mpi_world_is_rejected() {
    let world = get_mpi_world();

    let err = try_init_app_color(1).expect_err("late color split must be rejected");
    assert_eq!(
        err,
        InitAppColorError::CommunicatorAlreadyFixed { requested: 1 }
    );
    assert!(
        err.to_string().contains("before get_mpi_world"),
        "error should name the ordering fix: {err}"
    );

    drop(world);
    finalize_mpi();
}

#[test]
fn init_app_color_enforces_lifecycle() {
    match std::env::var(MODE_ENV).as_deref() {
        Ok("same") => {
            same_color_is_idempotent();
            return;
        }
        Ok("different") => {
            different_color_is_rejected();
            return;
        }
        Ok("after-world") => {
            after_get_mpi_world_is_rejected();
            return;
        }
        Ok(other) => panic!("unknown {MODE_ENV} mode {other:?}"),
        Err(_) => {}
    }

    let exe = std::env::current_exe().expect("locate current test binary");
    for mode in ["same", "different", "after-world"] {
        let status = Command::new(&exe)
            .args([
                "--exact",
                "init_app_color_enforces_lifecycle",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(MODE_ENV, mode)
            .status()
            .unwrap_or_else(|err| panic!("spawn lifecycle child {mode:?}: {err}"));
        assert!(
            status.success(),
            "lifecycle child {mode:?} failed: {status}"
        );
    }
}
