//! Prints which GPU each rank binds to.
//!
//! ```sh
//! cargo build -p grass_gpu --features mpi_backend --example which_gpu
//! mpirun -n 4 target/debug/examples/which_gpu
//! ```

use grass_app::prelude::*;
use grass_gpu::{GpuDevice, GpuPlugin};
use grass_mpi::CommResource;

fn main() {
    let mut app = App::new();
    app.add_plugins(GpuPlugin::default());
    let comm = app.get_resource_ref::<CommResource>().unwrap();
    let gpu = app.get_resource_ref::<GpuDevice>().unwrap();
    println!(
        "rank {} of {}: node rank {}, adapter {} = {}",
        comm.0.rank(),
        comm.0.size(),
        comm.0.node_rank(),
        gpu.adapter_index(),
        gpu.adapter_name(),
    );
}
