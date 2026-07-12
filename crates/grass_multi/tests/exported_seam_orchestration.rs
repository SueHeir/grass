use grass_app::App;
use grass_multi::{advance_to_seam, complete_subapp_step, MultiAppExt, MultiResMut, Namespace};
use grass_scheduler::{ResMut, Schedule, ScheduleSet};

#[derive(Default)]
struct Events(Vec<&'static str>);

#[derive(Clone, Copy, Debug)]
enum ChildPhase {
    A,
    B,
}
impl ScheduleSet for ChildPhase {
    fn to_index(&self) -> u32 {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ParentPhase {
    Advance,
    Couple,
    Complete,
}
impl ScheduleSet for ParentPhase {
    fn to_index(&self) -> u32 {
        match self {
            Self::Advance => 0,
            Self::Couple => 1,
            Self::Complete => 2,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::Advance => "Advance",
            Self::Couple => "Couple",
            Self::Complete => "Complete",
        }
    }
}

struct Child;
impl Namespace for Child {
    const NAME: &'static str = "child";
}

fn phase_a(mut events: ResMut<Events>) {
    events.0.push("a");
}
fn phase_b(mut events: ResMut<Events>) {
    events.0.push("b");
}
fn couple(mut events: MultiResMut<Events, Child>) {
    events.0.push("couple");
}

fn child() -> App {
    let mut child = App::new();
    child.add_resource(Events::default());
    child.add_update_system(phase_a, ChildPhase::A);
    child.add_update_system(phase_b, ChildPhase::B);
    child.set_schedule(
        Schedule::builder()
            .then_variant(ChildPhase::A)
            .export_seam("child.output")
            .then_variant(ChildPhase::B)
            .build(),
    );
    child
}

#[test]
fn parent_couples_between_child_phases_without_reentrant_borrow() {
    let mut parent = App::new();
    parent.add_subapp_typed::<Child>(child());
    parent.add_update_system(
        advance_to_seam::<Child>("child.output"),
        ParentPhase::Advance,
    );
    parent.add_update_system(couple, ParentPhase::Couple);
    parent.add_update_system(complete_subapp_step::<Child>(), ParentPhase::Complete);
    parent.prepare();
    parent.run();

    let subs = parent.get_resource_ref::<grass_multi::SubApps>().unwrap();
    let child = subs.find(Child::NAME).unwrap();
    let cell = child
        .resource_cell(std::any::TypeId::of::<Events>())
        .unwrap();
    let events = cell.borrow();
    let events = events.downcast_ref::<Events>().unwrap();
    assert_eq!(events.0, ["a", "couple", "b"]);
}

#[test]
#[should_panic(expected = "expected seam `wrong`, got `child.output`")]
fn advance_fails_closed_on_unexpected_seam() {
    let mut parent = App::new();
    parent.add_subapp_typed::<Child>(child());
    parent.add_update_system(advance_to_seam::<Child>("wrong"), ParentPhase::Advance);
    parent.prepare();
    parent.run();
}

#[test]
#[should_panic(expected = "expected timestep completion, got seam `child.output`")]
fn completion_fails_closed_on_unexpected_seam() {
    let mut parent = App::new();
    parent.add_subapp_typed::<Child>(child());
    parent.add_update_system(complete_subapp_step::<Child>(), ParentPhase::Advance);
    parent.prepare();
    parent.run();
}
