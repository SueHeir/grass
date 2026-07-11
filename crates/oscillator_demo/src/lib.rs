//! A small, pedagogical harmonic-oscillator scientific library.
//!
//! The crate is deliberately simulation-neutral: it owns one oscillator's
//! state, semi-implicit-Euler integrator, configuration, and exchange value.
//! Applications decide how many instances exist and how they are coupled.

use std::marker::PhantomData;

use grass_app::{App, Plugin};
use grass_io::Config;
use grass_scheduler::{Res, ResMut, ScheduleSet};
use serde::Deserialize;

/// Position and velocity owned by one oscillator solver.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OscillatorState {
    pub x: f64,
    pub v: f64,
}

/// The externally supplied position used by an optional interface spring.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PeerPosition(pub f64);

/// Stable exchange value published by an oscillator library.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OscillatorPosition(pub f64);

/// A source-distinguished typed port payload. The marker is supplied by the
/// application, allowing two oscillator instances to publish independently.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionPort<Source> {
    pub position: OscillatorPosition,
    _source: PhantomData<Source>,
}
impl<Source> PositionPort<Source> {
    pub fn from_state(state: &OscillatorState) -> Self {
        Self {
            position: OscillatorPosition(state.x),
            _source: PhantomData,
        }
    }
}

/// Constant physical and numerical parameters for one oscillator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OscillatorParameters {
    pub stiffness: f64,
    pub damping: f64,
    pub coupling_stiffness: f64,
    pub mass: f64,
    pub dt: f64,
}

/// Plugin configuration read from a declarative `[oscillator]` TOML table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OscillatorConfig {
    pub x0: f64,
    pub v0: f64,
    #[serde(default)]
    pub peer_x0: f64,
    pub stiffness: f64,
    #[serde(default)]
    pub damping: f64,
    #[serde(default)]
    pub coupling_stiffness: f64,
    pub mass: f64,
    pub dt: f64,
}
impl Default for OscillatorConfig {
    fn default() -> Self {
        Self {
            x0: 1.0,
            v0: 0.0,
            peer_x0: 0.0,
            stiffness: 1.0,
            damping: 0.0,
            coupling_stiffness: 0.0,
            mass: 1.0,
            dt: 0.01,
        }
    }
}

/// Local schedule for the library integrator.
#[derive(Debug, Clone, Copy)]
pub enum OscillatorSchedule {
    Integrate,
}
impl ScheduleSet for OscillatorSchedule {
    fn to_index(&self) -> u32 {
        0
    }
    fn name(&self) -> &'static str {
        "OscillatorIntegrate"
    }
}

/// Advance one semi-implicit-Euler step. A coupling layer updates
/// [`PeerPosition`] between calls; this library does not know the peer.
pub fn integrate(
    mut state: ResMut<OscillatorState>,
    peer: Res<PeerPosition>,
    params: Res<OscillatorParameters>,
) {
    let acceleration = (-params.stiffness * state.x
        - params.damping * state.v
        - params.coupling_stiffness * (state.x - peer.0))
        / params.mass;
    state.v += acceleration * params.dt;
    state.x += state.v * params.dt;
}

/// Installs one configured oscillator and its integrator.
pub struct OscillatorPlugin;
impl Plugin for OscillatorPlugin {
    fn build(&self, app: &mut App) {
        let cfg = Config::load::<OscillatorConfig>(app, "oscillator");
        assert!(cfg.mass > 0.0, "oscillator.mass must be positive");
        assert!(cfg.dt > 0.0, "oscillator.dt must be positive");
        app.add_resource(OscillatorState {
            x: cfg.x0,
            v: cfg.v0,
        });
        app.add_resource(PeerPosition(cfg.peer_x0));
        app.add_resource(OscillatorParameters {
            stiffness: cfg.stiffness,
            damping: cfg.damping,
            coupling_stiffness: cfg.coupling_stiffness,
            mass: cfg.mass,
            dt: cfg.dt,
        });
        app.add_update_system(integrate, OscillatorSchedule::Integrate);
    }
    fn default_config(&self) -> Option<&str> {
        Some("[oscillator]\nx0 = 1.0\nv0 = 0.0\npeer_x0 = 0.0\nstiffness = 1.0\ndamping = 0.0\ncoupling_stiffness = 0.0\nmass = 1.0\ndt = 0.01\n")
    }
}

/// Deterministic two-oscillator summary suitable for regression output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FinalState {
    pub a: OscillatorState,
    pub b: OscillatorState,
}
impl FinalState {
    /// Stable bit-level fingerprint, independent of display formatting.
    pub fn fingerprint(self) -> [u64; 4] {
        [
            self.a.x.to_bits(),
            self.a.v.to_bits(),
            self.b.x.to_bits(),
            self.b.v.to_bits(),
        ]
    }
}

/// Exact state of an undamped, uncoupled harmonic oscillator.
pub fn analytical_uncoupled(
    initial: OscillatorState,
    stiffness: f64,
    mass: f64,
    time: f64,
) -> OscillatorState {
    let omega = (stiffness / mass).sqrt();
    OscillatorState {
        x: initial.x * (omega * time).cos() + initial.v / omega * (omega * time).sin(),
        v: -initial.x * omega * (omega * time).sin() + initial.v * (omega * time).cos(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn analytical_initial_state_is_preserved() {
        let s = OscillatorState { x: 2.0, v: -3.0 };
        assert_eq!(analytical_uncoupled(s, 4.0, 2.0, 0.0), s);
    }
    #[test]
    fn port_payload_carries_position() {
        let state = OscillatorState { x: 1.25, v: 9.0 };
        assert_eq!(PositionPort::<()>::from_state(&state).position.0, 1.25);
    }
}
