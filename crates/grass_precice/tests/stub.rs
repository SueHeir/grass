//! Confirm the stub-mode (no `precice` feature) compiles and panics with a
//! useful diagnostic when someone tries to actually use it. Real preCICE
//! integration tests require libprecice and live behind `--features precice`
//! in `tests/ping_pong.rs`.

#![cfg(not(feature = "precice"))]

#[test]
#[should_panic(expected = "grass_precice: built without the `precice` feature")]
fn stub_participant_plugin_panics_with_helpful_message() {
    let _ = grass_precice::PreciceParticipantPlugin::new("Solver", "config.xml");
}
