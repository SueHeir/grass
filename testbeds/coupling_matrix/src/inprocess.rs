//! The same coupled problem in one process: each role is a sub-App of a
//! parent App, and the implicit schemes use `ImplicitCoupling` with
//! `checkpoint_window` / `converge_window` instead of `DistributedImplicit`.

use std::any::TypeId;
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use grass_app::prelude::*;
use grass_multi::{
    checkpoint_window, converge_window, ImplicitCoupling, Multi, MultiAppExt, OuterIteration,
    SubApps,
};
use grass_scheduler::prelude::*;

use crate::distributed::Output;
use crate::model::{Case, System};

/// One role's state, owned by its sub-App.
struct Block {
    range: Range<usize>,
    substeps: usize,
    u: Vec<f64>,
    /// Coupling values the parent writes before each tick.
    ext: Vec<f64>,
}

struct Model(Arc<System>, f64);

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum RolePhase {
    Step,
}

fn step(mut b: ResMut<Block>, m: Res<Model>) {
    let mut start = vec![f64::NAN; m.0.n];
    start[b.range.clone()].copy_from_slice(&b.u);
    b.u = m.0.step(b.range.clone(), &start, &b.ext, m.1, b.substeps);
}

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Phase {
    Checkpoint,
    Sweep,
    Converge,
    Count,
}

fn with_block<R>(subs: &SubApps, name: &str, f: impl FnOnce(&mut Block) -> R) -> R {
    let p = subs.find(name).expect("role sub-App");
    let cell = p.resource_cell(TypeId::of::<Block>()).expect("Block");
    let mut b = cell.borrow_mut();
    f(b.downcast_mut::<Block>().unwrap())
}

pub fn run(case: Case, sys: Arc<System>) -> Output {
    let roles = sys.roles();
    let names: Vec<String> = (0..roles).map(Case::role_name).collect();
    let mut parent = App::new();
    for (r, name) in names.iter().enumerate() {
        let mut app = App::new();
        app.add_resource(Block {
            range: sys.role_range(r),
            substeps: case.substeps(r),
            u: sys.role_range(r).map(|i| sys.u0[i]).collect(),
            ext: vec![f64::NAN; sys.n],
        });
        app.add_resource(Model(sys.clone(), case.dt));
        app.add_update_system(step, RolePhase::Step);
        parent.add_subapp(name, app);
    }

    let gs = case.gauss_seidel();
    let owners: Vec<usize> = if gs {
        sys.gs_owners()
    } else {
        (0..roles).collect()
    };
    let x0: Vec<f64> = owners
        .iter()
        .flat_map(|&q| sys.role_range(q).map(|i| sys.u0[i]))
        .collect();
    // The owners' current guess, in owner order (implicit only).
    let guess = Rc::new(RefCell::new(x0.clone()));

    if case.implicit() {
        parent.add_resource(ImplicitCoupling::new(
            OuterIteration::new(x0, case.relaxation(), 1e-12, 60),
            case.dt,
            case.windows,
        ));
        let saved: Rc<RefCell<Vec<Vec<f64>>>> = Rc::default();
        let (s1, s2, n1, n2) = (saved.clone(), saved, names.clone(), names.clone());
        parent.add_update_system(
            checkpoint_window(
                move |w: &Multi| {
                    *s1.borrow_mut() = n1
                        .iter()
                        .map(|n| w.expect_read::<Block>(n).u.clone())
                        .collect();
                },
                move |w: &Multi| {
                    for (n, u) in n2.iter().zip(s2.borrow().iter()) {
                        w.expect_write::<Block>(n).u.clone_from(u);
                    }
                },
            ),
            Phase::Checkpoint,
        );
    }

    {
        let (sys, names, owners, guess) =
            (sys.clone(), names.clone(), owners.clone(), guess.clone());
        let implicit = case.implicit();
        parent.add_update_system(
            move |mut subs: ResMut<SubApps>| {
                // Everyone's window-start (or, mid-window, restored) state.
                let mut current = vec![f64::NAN; sys.n];
                for (r, n) in names.iter().enumerate() {
                    with_block(&subs, n, |b| {
                        current[sys.role_range(r)].copy_from_slice(&b.u)
                    });
                }
                // What a role sees of the others before they step this sweep.
                let mut seen = current.clone();
                if implicit {
                    let g = guess.borrow();
                    let mut k = 0;
                    for &q in &owners {
                        let rr = sys.role_range(q);
                        seen[rr.clone()].copy_from_slice(&g[k..k + rr.len()]);
                        k += rr.len();
                    }
                }
                for (r, n) in names.iter().enumerate() {
                    with_block(&subs, n, |b| b.ext.clone_from(&seen));
                    subs.tick(n);
                    if gs {
                        // later roles see this role's new values
                        with_block(&subs, n, |b| seen[sys.role_range(r)].copy_from_slice(&b.u));
                    }
                }
            },
            Phase::Sweep,
        );
    }

    if case.implicit() {
        let (n1, o1) = (names.clone(), owners.clone());
        let g = guess.clone();
        parent.add_update_system(
            converge_window(
                move |w: &Multi| {
                    o1.iter()
                        .flat_map(|&q| w.expect_read::<Block>(&n1[q]).u.clone())
                        .collect()
                },
                move |_: &Multi, x: &[f64], _| g.borrow_mut().copy_from_slice(x),
            ),
            Phase::Converge,
        );
    } else {
        let windows = case.windows;
        let mut done = 0;
        parent.add_update_system(
            move |mut sm: ResMut<SchedulerManager>| {
                done += 1;
                if done == windows {
                    sm.state = SchedulerState::End;
                }
            },
            Phase::Count,
        );
    }

    parent.start();

    let subs = parent.get_resource_ref::<SubApps>().unwrap();
    let mut values = Vec::new();
    for n in &names {
        with_block(&subs, n, |b| {
            values.extend(b.range.clone().zip(b.u.iter().copied()))
        });
    }
    let (iterations, unconverged) = parent
        .get_resource_ref::<ImplicitCoupling>()
        .map(|ic| {
            (
                ic.iterations_per_window().to_vec(),
                ic.unconverged_windows().len(),
            )
        })
        .unwrap_or_default();
    Output {
        values,
        iterations,
        report: true,
        unconverged,
    }
}
