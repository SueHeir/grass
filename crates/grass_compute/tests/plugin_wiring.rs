//! `ComputeDevicePlugin` puts the device — and the coherence machinery — on the
//! App, and does nothing else.

#![cfg(feature = "cubecl")]

use cubecl::cpu::CpuRuntime;
use grass_app::prelude::*;
use grass_compute::prelude::*;
use grass_scheduler::CoherenceRegistry;

#[test]
fn plugin_registers_device_and_coherence_registry() {
    let mut app = App::new();
    app.add_plugins(ComputeDevicePlugin::<CpuRuntime>::default_device());

    let device = app
        .get_resource_ref::<ComputeDevice<CpuRuntime>>()
        .expect("ComputeDevice resource");
    assert!(!device.runtime_name().is_empty());
    drop(device);

    assert!(
        app.get_resource_ref::<CoherenceRegistry>().is_some(),
        "the plugin should install a CoherenceRegistry so a later tier can \
         register host<->device mirrors"
    );
}

#[test]
fn plugin_announces_the_compute_device_capability() {
    let plugin = ComputeDevicePlugin::<CpuRuntime>::default_device();
    assert!(plugin.provides_capabilities().contains(&COMPUTE_DEVICE));
}

#[test]
fn an_existing_coherence_registry_is_left_alone() {
    let mut app = App::new();
    let mut registry = CoherenceRegistry::new();
    registry.suppress_warnings = true;
    app.add_resource(registry);

    app.add_plugins(ComputeDevicePlugin::<CpuRuntime>::default_device());

    assert!(
        app.get_resource_ref::<CoherenceRegistry>()
            .expect("registry")
            .suppress_warnings,
        "the plugin must not replace a registry the application already \
         configured"
    );
}
