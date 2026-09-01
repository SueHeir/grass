//! The kernel must agree with the plain Rust loop.
//!
//! Migration rule 3: equivalence is demonstrated, not asserted. These tests run
//! on `cubecl-cpu`, the only runtime that can execute on elizabeth-hpc, and
//! they say nothing whatsoever about speed.
//!
//! # Tolerance, and why it is exactly zero
//!
//! Both sides compute one IEEE-754 binary32 addition per element, on the same
//! inputs, with no reassociation and no opportunity for FMA contraction (a lone
//! `a + b` has nothing to contract with). IEEE-754 addition is correctly
//! rounded, so a correct implementation on either side must produce the same
//! bit pattern. A non-zero tolerance here would hide exactly the class of bug
//! this test exists to catch — an off-by-one index, a dropped element, a stale
//! buffer — so the comparison is bitwise equality.

#![cfg(feature = "cubecl")]

use cubecl::cpu::CpuRuntime;
use grass_compute::prelude::*;
use grass_compute::{elementwise_add, elementwise_add_host, TransferSnapshot};

/// Deterministic, sign-mixed, non-round inputs. Round numbers would still
/// compare equal if the kernel silently truncated precision somewhere.
fn inputs(n: usize) -> (Vec<f32>, Vec<f32>) {
    let lhs = (0..n)
        .map(|i| (i as f32) * 0.1 - 3.7 + (i % 7) as f32 * 0.013)
        .collect();
    let rhs = (0..n)
        .map(|i| 1.0 / (i as f32 + 1.5) - (i % 5) as f32 * 0.31)
        .collect();
    (lhs, rhs)
}

#[test]
fn kernel_matches_plain_rust_loop() {
    let device = ComputeDevice::<CpuRuntime>::default_device();

    // Sizes that are not multiples of the cube dimension, so the kernel's
    // bounds check is exercised rather than accidentally aligned away.
    for n in [1usize, 3, 17, 64, 65, 1000] {
        let (lhs, rhs) = inputs(n);

        let mut expected = vec![0.0f32; n];
        elementwise_add_host(&lhs, &rhs, &mut expected);

        let d_lhs = device.to_device(&lhs);
        let d_rhs = device.to_device(&rhs);
        let d_out = device.alloc::<f32>(n);
        elementwise_add(&device, &d_lhs, &d_rhs, &d_out).expect("launch");
        let actual = device.from_device(&d_out).expect("readback");

        assert_eq!(actual.len(), n, "readback length for n = {n}");
        for i in 0..n {
            assert_eq!(
                actual[i].to_bits(),
                expected[i].to_bits(),
                "n = {n}, i = {i}: device {} vs host {}",
                actual[i],
                expected[i]
            );
        }
    }
}

#[test]
fn readback_into_caller_slice_matches() {
    let device = ComputeDevice::<CpuRuntime>::default_device();
    let (lhs, rhs) = inputs(33);

    let mut expected = vec![0.0f32; 33];
    elementwise_add_host(&lhs, &rhs, &mut expected);

    let d_out = device.alloc::<f32>(33);
    elementwise_add(
        &device,
        &device.to_device(&lhs),
        &device.to_device(&rhs),
        &d_out,
    )
    .expect("launch");

    let mut actual = vec![0.0f32; 33];
    device.from_device_into(&d_out, &mut actual).expect("readback");
    assert_eq!(actual, expected);
}

#[test]
fn transfers_are_counted() {
    let device = ComputeDevice::<CpuRuntime>::default_device();
    assert_eq!(device.transfers(), TransferSnapshot::default());

    let lhs = vec![1.0f32; 8];
    let rhs = vec![2.0f32; 8];
    let d_lhs = device.to_device(&lhs);
    let d_rhs = device.to_device(&rhs);
    let d_out = device.alloc::<f32>(8);

    // `alloc` is not a transfer; only the two uploads are.
    let after_upload = device.transfers();
    assert_eq!(after_upload.host_to_device, 2);
    assert_eq!(after_upload.host_to_device_bytes, 2 * 8 * 4);
    assert_eq!(after_upload.device_to_host, 0);

    elementwise_add(&device, &d_lhs, &d_rhs, &d_out).expect("launch");
    device.barrier().expect("barrier");
    let _ = device.from_device(&d_out).expect("readback");

    let after = device.transfers();
    assert_eq!(after.host_to_device, 2);
    assert_eq!(after.device_to_host, 1);
    assert_eq!(after.device_to_host_bytes, 8 * 4);
    assert_eq!(after.barriers, 1);

    device.counts().reset();
    assert_eq!(device.transfers(), TransferSnapshot::default());
}

#[test]
fn mismatched_lengths_are_rejected() {
    let device = ComputeDevice::<CpuRuntime>::default_device();
    let d_lhs = device.to_device(&[1.0f32, 2.0, 3.0]);
    let d_rhs = device.to_device(&[1.0f32, 2.0]);
    let d_out = device.alloc::<f32>(3);

    assert_eq!(
        elementwise_add(&device, &d_lhs, &d_rhs, &d_out),
        Err(ComputeError::LengthMismatch {
            expected: 3,
            actual: 2
        })
    );

    let mut host = vec![0.0f32; 2];
    assert_eq!(
        device.from_device_into(&d_out, &mut host),
        Err(ComputeError::LengthMismatch {
            expected: 3,
            actual: 2
        })
    );
}

#[test]
fn buffer_records_its_own_shape() {
    let device = ComputeDevice::<CpuRuntime>::default_device();
    let buffer = device.alloc::<f32>(10);
    assert_eq!(buffer.len(), 10);
    assert_eq!(buffer.size_bytes(), 40);
    assert!(!buffer.is_empty());
    assert!(device.alloc::<f32>(0).is_empty());
    assert!(!device.runtime_name().is_empty());
}
