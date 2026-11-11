//! Plugin lifecycle against a scripted stand-in participant (runs without
//! the preCICE library). The script repeats every window once, as an
//! implicit coupling that converges on its second iteration would.

use std::cell::RefCell;
use std::rc::Rc;

use grass_app::prelude::*;
use grass_precice::{CouplingInterface, CouplingStep, PreciceAppExt, PrecicePlugin, Result};
use grass_scheduler::prelude::*;

#[derive(Default)]
struct Log {
    calls: Vec<String>,
    written: Vec<Vec<f64>>,
}

/// Windows of length 1, two windows, each attempted twice.
struct Scripted {
    log: Rc<RefCell<Log>>,
    t: f64,
    window_start: f64,
    attempt: u32,
    ongoing: bool,
    write_ckpt: bool,
    read_ckpt: bool,
    window_done: bool,
}

impl CouplingInterface for Scripted {
    fn mesh_dimensions(&self, _: &str) -> Result<usize> {
        Ok(2)
    }
    fn data_dimensions(&self, _: &str, _: &str) -> Result<usize> {
        Ok(1)
    }
    fn set_mesh_vertices(&mut self, mesh: &str, p: &[f64]) -> Result<Vec<i32>> {
        self.log
            .borrow_mut()
            .calls
            .push(format!("mesh {mesh} {}", p.len() / 2));
        Ok((0..(p.len() / 2) as i32).map(|i| 10 + i).collect())
    }
    fn set_mesh_triangles(&mut self, _: &str, _: &[i32]) -> Result<()> {
        Ok(())
    }
    fn requires_initial_data(&mut self) -> Result<bool> {
        Ok(true)
    }
    fn initialize(&mut self) -> Result<()> {
        self.log.borrow_mut().calls.push("initialize".into());
        self.write_ckpt = true;
        Ok(())
    }
    fn advance(&mut self, dt: f64) -> Result<()> {
        self.log.borrow_mut().calls.push(format!("advance {dt}"));
        self.t += dt;
        self.write_ckpt = false;
        self.read_ckpt = false;
        self.window_done = false;
        if self.t >= self.window_start + 1.0 - 1e-12 {
            if self.attempt == 0 {
                self.attempt = 1;
                self.t = self.window_start;
                self.read_ckpt = true;
            } else {
                self.attempt = 0;
                self.window_start += 1.0;
                self.window_done = true;
                self.write_ckpt = true;
                self.ongoing = self.window_start < 2.0 - 1e-12;
            }
        }
        Ok(())
    }
    fn finalize(&mut self) -> Result<()> {
        self.log.borrow_mut().calls.push("finalize".into());
        Ok(())
    }
    fn is_coupling_ongoing(&self) -> Result<bool> {
        Ok(self.ongoing)
    }
    fn is_time_window_complete(&self) -> Result<bool> {
        Ok(self.window_done)
    }
    fn max_time_step_size(&self) -> Result<f64> {
        Ok(self.window_start + 1.0 - self.t)
    }
    fn requires_writing_checkpoint(&mut self) -> Result<bool> {
        Ok(self.write_ckpt)
    }
    fn requires_reading_checkpoint(&mut self) -> Result<bool> {
        Ok(self.read_ckpt)
    }
    fn write_data(&mut self, _: &str, _: &str, ids: &[i32], v: &[f64]) -> Result<()> {
        assert_eq!(ids, &[10, 11]);
        self.log.borrow_mut().written.push(v.to_vec());
        Ok(())
    }
    fn read_data(&self, _: &str, _: &str, _: &[i32], _: f64, v: &mut [f64]) -> Result<()> {
        // The partner's value is the current attempt number.
        v.iter_mut().for_each(|x| *x = self.attempt as f64);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
struct State {
    u: f64,
    input: f64,
}

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase {
    Step,
}

fn step(mut s: ResMut<State>, c: Res<CouplingStep>) {
    s.u += c.dt * (1.0 + s.input);
}

#[test]
fn plugin_saves_restores_subcycles_and_stops() {
    thread_local! {
        static LOG: Rc<RefCell<Log>> = Rc::new(RefCell::new(Log::default()));
    }
    let log = LOG.with(Rc::clone);
    let mut app = App::new();
    app.add_resource(State { u: 0.0, input: 0.0 });
    app.add_plugins(
        PrecicePlugin::new(|| {
            Ok(Box::new(Scripted {
                log: LOG.with(Rc::clone),
                t: 0.0,
                window_start: 0.0,
                attempt: 0,
                ongoing: true,
                write_ckpt: false,
                read_ckpt: false,
                window_done: false,
            }) as Box<dyn CouplingInterface>)
        })
        .solver_dt(0.5),
    );
    app.precice_mesh::<State>("M", |_| vec![0.0, 0.0, 1.0, 0.0]);
    app.precice_write::<State>("M", "U", |s| vec![s.u, s.u]);
    app.precice_read::<State>("M", "In", |s, v| s.input = v[0]);
    app.precice_checkpoint::<State>();
    app.add_update_system(step, Phase::Step);
    app.start();

    let c = app.get_resource_ref::<CouplingStep>().unwrap().clone();
    assert!(c.done);
    assert_eq!(c.windows, 2);
    // Two windows × two attempts × two substeps of 0.5.
    assert_eq!(c.advances, 8);
    assert!((c.time - 2.0).abs() < 1e-12);

    // Each window's accepted attempt reads input 1: u grows by 2 per window.
    // The rejected attempt (input 0) was rolled back.
    let s = app.get_resource_ref::<State>().unwrap().clone();
    assert!((s.u - 4.0).abs() < 1e-12, "u = {}", s.u);

    let log = log.borrow();
    assert_eq!(log.calls[0], "mesh M 2");
    assert_eq!(log.calls[1], "initialize");
    assert_eq!(log.calls.last().unwrap(), "finalize");
    // Initial data plus one write per advance.
    assert_eq!(log.written.len(), 9);
    assert_eq!(log.written[0], vec![0.0, 0.0]);
}
