//! One trivial elementwise kernel, and the plain Rust loop it must agree with.
//!
//! This is a toolchain proof, not a library of numerics. GRASS knows nothing
//! about physics and this crate does not change that: elementwise addition of
//! two `f32` arrays is domain-neutral, and it is here so that "does a CubeCL
//! kernel written in this repo compile, launch, and produce the same answer as
//! the equivalent Rust" has an answer that is checked rather than asserted.
//!
//! Real kernels live in the physics tiers (SOIL, then DIRT), per the migration
//! staging.

use cubecl::prelude::*;

use crate::buffer::DeviceBuffer;
use crate::device::{ComputeDevice, ComputeError};

/// CubeCL kernel definitions.
///
/// `#[cube(launch)]` replaces the annotated function with a module of the same
/// name holding the expansion, the kernel definition and `launch`, none of
/// which carry doc comments — hence the blanket `allow` here rather than at
/// crate level.
#[allow(missing_docs)]
pub mod kernels {
    use cubecl::prelude::*;

    /// `out[i] = lhs[i] + rhs[i]`, one unit per element.
    #[cube(launch)]
    pub fn elementwise_add_kernel(lhs: &[f32], rhs: &[f32], out: &mut [f32]) {
        if ABSOLUTE_POS < out.len() {
            out[ABSOLUTE_POS] = lhs[ABSOLUTE_POS] + rhs[ABSOLUTE_POS];
        }
    }
}

/// The host reference for [`kernels::elementwise_add_kernel`]: the plain Rust
/// loop.
///
/// Equivalence tests compare against *this*, not against a recomputed
/// expression, so that the two sides of the comparison are visibly the same
/// operation.
pub fn elementwise_add_host(lhs: &[f32], rhs: &[f32], out: &mut [f32]) {
    for i in 0..out.len() {
        out[i] = lhs[i] + rhs[i];
    }
}

/// Launches the elementwise-add kernel over device-resident buffers.
///
/// Performs no transfer: all three buffers are already on the device, and the
/// result stays there. Pulling `out` back is the caller's explicit
/// [`from_device`](ComputeDevice::from_device) call, which is the whole point
/// of keeping transfers separate from compute.
pub fn elementwise_add<R: Runtime>(
    device: &ComputeDevice<R>,
    lhs: &DeviceBuffer<R, f32>,
    rhs: &DeviceBuffer<R, f32>,
    out: &DeviceBuffer<R, f32>,
) -> Result<(), ComputeError> {
    if lhs.len() != out.len() {
        return Err(ComputeError::LengthMismatch {
            expected: out.len(),
            actual: lhs.len(),
        });
    }
    if rhs.len() != out.len() {
        return Err(ComputeError::LengthMismatch {
            expected: out.len(),
            actual: rhs.len(),
        });
    }

    let client = device.client();
    let cube_dim = device.cube_dim_1d(64);
    let cube_count = cubecl::calculate_cube_count_elemwise::<R>(client, out.len(), cube_dim);

    kernels::elementwise_add_kernel::launch::<R>(
        client,
        cube_count,
        cube_dim,
        lhs.arg(),
        rhs.arg(),
        out.arg(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_reference_adds_elementwise() {
        let lhs = [1.0f32, 2.0, 3.0];
        let rhs = [0.5f32, -2.0, 10.0];
        let mut out = [0.0f32; 3];
        elementwise_add_host(&lhs, &rhs, &mut out);
        assert_eq!(out, [1.5, 0.0, 13.0]);
    }
}
