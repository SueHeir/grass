//! # `explicit_mpi_b` — explicit / CSS coupling, binary B of an MPI pair
//!
//! Mirror of `explicit_mpi_a`: this binary owns oscillator B locally
//! and keeps oscillator A as a remote mirror over MPI.
//!
//! See `a.rs` for the per-iter flow + launch instructions; this file is
//! the symmetric pair.

use grass_app::prelude::*;
use grass_io::{InputPlugin, MultiIoExt, RunPlugin};
use grass_multi::{
    tick_subapp, MpiInterCommTransport, MultiAppExt, MultiRes, MultiResMut, Namespace,
};
use grass_scheduler::prelude::*;
use oscillator_demo::{
    exchange_positions, extract_final_state, OscState, OscillatorPlugin, OtherX, A, B,
};

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum OuterStep {
    TickB,
    ExportLocalB,
    TickMirrorA,
    Exchange,
}

fn export_local_b(b: MultiRes<OscState, B>, mut a_mirror: MultiResMut<OscState, A>) {
    *a_mirror = *b;
}

fn main() {
    let mut parent = App::new();
    parent.add_plugins(InputPlugin);

    // Local oscillator B — built from main.toml's [b.oscillator] slice.
    parent.add_subapp_with_config(B::NAME, |app| {
        app.add_plugins(OscillatorPlugin);
    });

    // Under `mpirun -np 1 a : -np 1 b`, B is rank 1, peer A is rank 0.
    let transport = MpiInterCommTransport::new(/* peer = */ 0);
    parent
        .add_remote_subapp(A::NAME, transport)
        .send_each_iter::<OscState>()
        .recv_each_iter::<OscState>()
        .with_resource::<OtherX>();

    parent.add_plugins(RunPlugin);

    parent.add_update_system(tick_subapp(B::NAME, 1), OuterStep::TickB);
    parent.add_update_system(export_local_b, OuterStep::ExportLocalB);
    parent.add_update_system(tick_subapp(A::NAME, 1), OuterStep::TickMirrorA);
    parent.add_update_system(exchange_positions, OuterStep::Exchange);

    parent.start();

    if parent.get_resource_ref::<GenerateConfigFlag>().is_some() {
        return;
    }
    extract_final_state(&parent).print("explicit_mpi_b");
}
