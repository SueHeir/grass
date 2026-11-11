//! Restarting from a checkpoint reproduces an uninterrupted run exactly.

use std::path::{Path, PathBuf};

use grass_app::prelude::*;
use grass_io::{
    Checkpoint, CheckpointAppExt, CheckpointPlugin, Checkpoints, Config, Input, Provenance,
    ProvenancePlugin, RunPlugin, SimClock,
};
use grass_scheduler::prelude::*;
use serde::{Deserialize, Serialize};

/// A chaotic state (logistic map) plus a random-number generator: any
/// state that is not restored exactly shows up as a different answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct State {
    x: Vec<f64>,
    rng: u64,
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    Step,
}

fn step(mut s: ResMut<State>, mut clock: ResMut<SimClock>) {
    s.rng ^= s.rng << 13;
    s.rng ^= s.rng >> 7;
    s.rng ^= s.rng << 17;
    let kick = (s.rng >> 11) as f64 / (1u64 << 53) as f64 * 1e-3;
    for v in s.x.iter_mut() {
        *v = (3.97 * *v * (1.0 - *v) + kick).clamp(0.0, 1.0);
    }
    clock.time += 0.01;
}

fn run(dir: &Path, toml: &str) -> (State, SimClock, Vec<PathBuf>) {
    let mut app = App::new();
    app.add_resource(Config::from_str(toml));
    app.add_resource(Input {
        filename: String::new(),
        output_dir: Some(dir.display().to_string()),
    });
    app.add_resource(State {
        x: vec![0.1, 0.2, 0.3, 0.4],
        rng: 0x9e37_79b9_7f4a_7c15,
    });
    app.add_plugins(ProvenancePlugin::new("restart-test", "0.0.1").note("case", "logistic"));
    app.add_plugins(CheckpointPlugin);
    app.checkpoint::<State>("state");
    app.add_update_system(step, Phase::Step);
    app.add_plugins(RunPlugin);
    app.start();
    let s = app.get_resource_ref::<State>().unwrap().clone();
    let c = *app.get_resource_ref::<SimClock>().unwrap();
    let w = app
        .get_resource_ref::<Checkpoints>()
        .unwrap()
        .written
        .clone();
    (s, c, w)
}

#[test]
fn restart_is_bit_identical_to_an_uninterrupted_run() {
    let dir = std::env::temp_dir().join(format!("grass_restart_{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();

    let (full, clock, written) = run(
        &dir,
        "[run]\nsteps = 40\n[checkpoint]\nevery = 10\nkeep = 0\n",
    );
    assert_eq!(clock.step, 40);
    let names: Vec<String> = written
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [10, 20, 30, 40].map(|s| format!("checkpoint_{s:010}.grass"))
    );

    // The checkpoint records the completed step, the time and the run's provenance.
    let c = Checkpoint::read(&written[1]).unwrap();
    assert_eq!(c.header.step, 20);
    assert!((c.header.time - 0.2).abs() < 1e-12);
    let prov: &Provenance = c.header.provenance.as_ref().unwrap();
    assert_eq!(prov.app, "restart-test");
    assert_eq!(prov.notes["case"], "logistic");
    assert!(dir.join("provenance.toml").exists());

    // Restart from step 20 and run the remaining 20 steps.
    let (restarted, clock2, _) = run(
        &dir,
        "[run]\nsteps = 20\n[checkpoint]\nevery = 0\nrestart = \"checkpoints/checkpoint_0000000020.grass\"\n",
    );
    assert_eq!(clock2.step, 40);
    assert_eq!(clock2.time.to_bits(), clock.time.to_bits());
    // Bit-identical, including the RNG state.
    assert_eq!(restarted, full);
    for (a, b) in restarted.x.iter().zip(&full.x) {
        assert_eq!(a.to_bits(), b.to_bits());
    }

    // Without restoring the state the answer differs (the test can tell).
    let (cold, _, _) = run(&dir, "[run]\nsteps = 20\n");
    assert_ne!(cold, full);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn keep_limits_the_files_on_disk() {
    let dir = std::env::temp_dir().join(format!("grass_keep_{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let (_, _, written) = run(
        &dir,
        "[run]\nsteps = 50\n[checkpoint]\nevery = 10\nkeep = 2\n",
    );
    assert_eq!(written.len(), 2);
    let mut on_disk: Vec<_> = std::fs::read_dir(dir.join("checkpoints"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    assert_eq!(
        on_disk,
        ["checkpoint_0000000040.grass", "checkpoint_0000000050.grass"]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn resources_registered_before_the_plugin_are_still_checkpointed() {
    let dir = std::env::temp_dir().join(format!("grass_order_{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let mut app = App::new();
    app.add_resource(Config::from_str(
        "[run]\nsteps = 10\n[checkpoint]\nevery = 10\nkeep = 0\n",
    ));
    app.add_resource(Input {
        filename: String::new(),
        output_dir: Some(dir.display().to_string()),
    });
    app.add_resource(State {
        x: vec![0.5],
        rng: 1,
    });
    // Registered before CheckpointPlugin adds its own Collect system.
    app.checkpoint::<State>("state");
    app.add_plugins(CheckpointPlugin);
    app.add_update_system(step, Phase::Step);
    app.add_plugins(RunPlugin);
    app.start();
    let written = app
        .get_resource_ref::<Checkpoints>()
        .unwrap()
        .written
        .clone();
    assert_eq!(written.len(), 1);
    let c = Checkpoint::read(&written[0]).unwrap();
    let saved: State = c.get("state").expect("state entry present");
    assert_eq!(&saved, &*app.get_resource_ref::<State>().unwrap());
    std::fs::remove_dir_all(&dir).ok();
}
