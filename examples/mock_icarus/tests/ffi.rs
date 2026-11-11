//! The Fortran and C codes behave through the safe wrappers.
use mock_icarus::{Ablator, Aero, AeroConfig};

#[test]
fn ablator_save_restore_is_exact() {
    let mut a = Ablator::new(2, 40, 0.03, 300.0).unwrap();
    for s in 0..2 {
        a.set_bc(s, 0.1, 2.0e7);
    }
    for _ in 0..50 {
        a.step(0.05).unwrap();
    }
    let saved = a.save();
    for _ in 0..50 {
        a.step(0.05).unwrap();
    }
    let first = (a.surface(0), a.profile(1));
    a.restore(&saved);
    for _ in 0..50 {
        a.step(0.05).unwrap();
    }
    assert_eq!(first, (a.surface(0), a.profile(1)));
}

#[test]
fn ablator_heats_pyrolyzes_and_recedes() {
    let mut a = Ablator::new(1, 60, 0.05, 300.0).unwrap();
    a.set_bc(0, 0.1, 2.0e7);
    for _ in 0..400 {
        a.step(0.05).unwrap();
    }
    let s = a.surface(0);
    let (_, beta) = a.profile(0);
    assert!(s.t_wall > 2000.0 && s.t_wall < 3000.0, "{s:?}");
    assert!(s.recession > 0.0 && s.mdot_gas > 0.0);
    assert!(
        beta[0] > 0.99 && beta[59] < 1e-6,
        "char at the surface, virgin at the back"
    );
    // energy balance at the wall: conduction in = convection - re-radiation > 0
    assert!(s.q_conv > s.q_rad);
}

#[test]
fn bad_material_name_is_an_error() {
    let mut a = Ablator::new(1, 10, 0.01, 300.0).unwrap();
    assert!(a.set_material("k_char", 0.5).is_ok());
    assert!(a.set_material("nonsense", 1.0).is_err());
}

#[test]
fn blowing_reduces_heating() {
    let mut aero = Aero::new(&AeroConfig {
        stations_deg: vec![0.0, 45.0],
        ..AeroConfig::default()
    })
    .unwrap();
    for _ in 0..400 {
        aero.step(0.1);
    }
    let bare = aero.env(0);
    assert!(bare.q_cw > 1e6, "{bare:?}");
    assert!(aero.env(1).q_cw < bare.q_cw);
    aero.set_blowing(0, 0.05);
    let blown = aero.env(0);
    assert!(blown.ch < bare.ch && blown.ch0 == bare.ch0);
}
