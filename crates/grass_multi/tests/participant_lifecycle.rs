use grass_multi::{Physics, StepResult, SubApps};
use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Default)]
struct Counts {
    prepares: Cell<usize>,
    steps: Cell<usize>,
    cleanups: Cell<usize>,
}

struct CountingPhysics {
    name: String,
    counts: Rc<Counts>,
}

impl CountingPhysics {
    fn new(name: &str) -> (Self, Rc<Counts>) {
        let counts = Rc::new(Counts::default());
        (
            Self {
                name: name.to_owned(),
                counts: Rc::clone(&counts),
            },
            counts,
        )
    }
}

impl Physics for CountingPhysics {
    fn name(&self) -> &str {
        &self.name
    }

    fn prepare(&mut self) {
        self.counts.prepares.set(self.counts.prepares.get() + 1);
    }

    fn step(&mut self) -> StepResult {
        self.counts.steps.set(self.counts.steps.get() + 1);
        StepResult::default()
    }

    fn is_done(&self) -> bool {
        false
    }

    fn cleanup(&mut self) {
        self.counts.cleanups.set(self.counts.cleanups.get() + 1);
    }

    fn resource_cell(&self, _ty: TypeId) -> Option<&RefCell<Box<dyn Any>>> {
        None
    }
}

#[test]
fn cleanup_skips_participant_with_zero_ticks() {
    let (physics, counts) = CountingPhysics::new("idle");
    let mut subapps = SubApps::new();
    subapps.register(Box::new(physics));

    subapps.cleanup_all();

    assert_eq!(counts.prepares.get(), 0);
    assert_eq!(counts.steps.get(), 0);
    assert_eq!(counts.cleanups.get(), 0);
}

#[test]
fn repeated_cleanup_cleans_prepared_participant_once() {
    let (physics, counts) = CountingPhysics::new("active");
    let mut subapps = SubApps::new();
    subapps.register(Box::new(physics));
    subapps.tick("active");

    subapps.cleanup_all();
    subapps.cleanup_all();

    assert_eq!(counts.prepares.get(), 1);
    assert_eq!(counts.steps.get(), 1);
    assert_eq!(counts.cleanups.get(), 1);
}

#[test]
fn partial_preparation_only_cleans_prepared_participants() {
    let (prepared, prepared_counts) = CountingPhysics::new("prepared");
    let (idle, idle_counts) = CountingPhysics::new("idle");
    let mut subapps = SubApps::new();
    subapps.register(Box::new(prepared));
    subapps.register(Box::new(idle));

    subapps.prepare("prepared");
    subapps.cleanup_all();

    assert_eq!(prepared_counts.prepares.get(), 1);
    assert_eq!(prepared_counts.steps.get(), 0);
    assert_eq!(prepared_counts.cleanups.get(), 1);
    assert_eq!(idle_counts.prepares.get(), 0);
    assert_eq!(idle_counts.steps.get(), 0);
    assert_eq!(idle_counts.cleanups.get(), 0);
}
