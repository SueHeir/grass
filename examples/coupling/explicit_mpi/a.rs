//! # `explicit_mpi_a` — explicit / CSS coupling, binary A of an MPI pair
//!
//! Same physics + same `exchange_positions` coupling system as
//! `examples/coupling/explicit`, but the two oscillators live in
//! separate binaries talking over MPI.
//!
//! ## Layout
//!
//!   - **This binary** owns oscillator A as a real local sub-App.
//!   - **Peer binary** (`explicit_mpi_b`) owns oscillator B.
//!   - Each side keeps a *remote mirror* of the peer's `OscState`,
//!     receiving fresh values each iter via `MpiInterCommTransport`.
//!
//! ## Per-iter flow
//!
//!   1. Tick A locally (advances using current `OtherX`).
//!   2. Copy local `a.OscState` into the mirror's `OscState` slot.
//!   3. Tick the mirror: sends our slot to B, receives B's `OscState`
//!      back into the same slot.
//!   4. Run `exchange_positions`: updates `a.OtherX` from the freshly-
//!      recv'd mirror `b.OscState`.
//!   5. RunPlugin checks step >= [run] steps.
//!
//! ## Launch
//!
//! ```sh
//! cargo build --features mpi --example explicit_mpi_a --example explicit_mpi_b
//! mpirun -np 1 ./target/debug/examples/explicit_mpi_a examples/coupling/explicit_mpi/main.toml \
//!      : -np 1 ./target/debug/examples/explicit_mpi_b examples/coupling/explicit_mpi/main.toml
//! ```

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
    TickA,
    ExportLocalA,
    TickMirrorB,
    Exchange,
}

fn export_local_a(a: MultiRes<OscState, A>, mut b_mirror: MultiResMut<OscState, B>) {
    *b_mirror = *a;
}

fn main() {
    let mut parent = App::new();
    parent.add_plugins(InputPlugin);

    // Local oscillator A — built from main.toml's [a.oscillator] slice.
    parent.add_subapp_with_config(A::NAME, |app| {
        app.add_plugins(OscillatorPlugin);
    });

    // Under `mpirun -np 1 a : -np 1 b`, A is rank 0, B is rank 1.
    let transport = MpiInterCommTransport::new(/* peer = */ 1);
    parent
        .add_remote_subapp(B::NAME, transport)
        .send_each_iter::<OscState>()
        .recv_each_iter::<OscState>()
        .with_resource::<OtherX>();

    parent.add_plugins(RunPlugin);

    parent.add_update_system(tick_subapp(A::NAME, 1), OuterStep::TickA);
    parent.add_update_system(export_local_a, OuterStep::ExportLocalA);
    parent.add_update_system(tick_subapp(B::NAME, 1), OuterStep::TickMirrorB);
    parent.add_update_system(exchange_positions, OuterStep::Exchange);

    parent.start();

    if parent.get_resource_ref::<GenerateConfigFlag>().is_some() {
        return;
    }
    extract_final_state(&parent).print("explicit_mpi_a");
}
