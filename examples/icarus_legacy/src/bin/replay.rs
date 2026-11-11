//! Run the stand-alone deck through GRASS instead of the code's own main
//! program, and check nothing changed.
//!
//! The code still reads its environment table and writes its own files; the
//! only difference is who owns MPI, the time loop and the output directory.
//! Under the same rank count the code's output files are byte-identical to
//! the stand-alone program's:
//!
//!     (cd cases && mpirun -np 2 ../fortran/micarus oneway.nml)
//!     mpirun -np 2 target/release/replay replay.toml
//!     cmp cases/out/oneway_surf_0160.dat out/replay/oneway_surf_0160.dat

use grass_app::prelude::*;
use grass_io::prelude::*;
use grass_io::ProvenancePlugin;
use grass_scheduler::prelude::*;
use icarus_legacy::{Control, Micarus, MicarusPlugin};

/// Runs after the code's step (namespace 20).
#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Driver {
    Stop,
}

fn stop_at_end(mi: Res<Micarus>, c: Res<Control>, mut sm: ResMut<SchedulerManager>) {
    if mi.time() >= c.t_end {
        sm.state = SchedulerState::End;
    }
}

fn main() {
    let mut app = App::new();
    app.add_plugins(InputPlugin)
        .add_plugins(
            ProvenancePlugin::new("icarus_legacy/replay", env!("CARGO_PKG_VERSION"))
                .note("micarus sources", &icarus_legacy::FORTRAN_SOURCE_HASH),
        )
        .add_plugins(MicarusPlugin);
    app.set_schedule_namespace::<Driver>(50);
    app.add_update_system(stop_at_end, Driver::Stop);
    let t0 = std::time::Instant::now();
    app.start();
    if std::env::args().any(|a| a == "--config-docs" || a == "--generate-config") {
        return;
    }
    let mi = app.get_resource_ref::<Micarus>().unwrap();
    let c = mi.counters();
    if grass_mpi::world_rank() == 0 {
        println!(
            "replay: t = {} s on {} rank(s), {} steps, {} rejected, wall {:.2} s",
            mi.time(),
            c.ranks,
            c.steps,
            c.rejected,
            t0.elapsed().as_secs_f64()
        );
    }
}
