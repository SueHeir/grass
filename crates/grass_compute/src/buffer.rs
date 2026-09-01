//! [`DeviceBuffer`] — a typed, length-carrying handle on a CubeCL allocation.

use core::marker::PhantomData;

use cubecl::prelude::*;
use cubecl::server::Handle;

/// A device allocation of `len` elements of type `E`, owned by a
/// [`ComputeDevice`](crate::ComputeDevice) of runtime `R`.
///
/// CubeCL's own `Handle` is untyped and length-free: it is a range of device
/// bytes, and turning it into a kernel argument goes through
/// `unsafe BufferArg::from_raw_parts(handle, length)`, where passing the wrong
/// length is an out-of-bounds read or write. This wrapper exists to record the
/// element type and the element count at the point of allocation, so that
/// [`arg`](Self::arg) can be safe and every later use is checked against the
/// same number.
///
/// It deliberately does **not** know whether its contents are current. That is
/// the schedule's job — see [`ComputeSyncSet`](crate::ComputeSyncSet).
///
/// Cloning clones the underlying CubeCL handle, which refers to the *same*
/// device memory rather than copying it.
pub struct DeviceBuffer<R: Runtime, E: CubeElement> {
    handle: Handle,
    len: usize,
    _marker: PhantomData<(R, E)>,
}

impl<R: Runtime, E: CubeElement> DeviceBuffer<R, E> {
    /// Wraps a raw CubeCL handle that is known to hold `len` elements of `E`.
    ///
    /// # Safety
    ///
    /// `handle` must refer to an allocation of at least `len * size_of::<E>()`
    /// bytes, correctly aligned for `E`. Everything else in this crate builds
    /// buffers through [`ComputeDevice`](crate::ComputeDevice), which upholds
    /// that; this constructor is for interoperating with CubeCL code that
    /// produced a handle some other way.
    pub unsafe fn from_raw_parts(handle: Handle, len: usize) -> Self {
        Self {
            handle,
            len,
            _marker: PhantomData,
        }
    }

    /// Number of elements of `E` in the buffer.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the buffer holds no elements.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Size of the buffer's contents in bytes (`len * size_of::<E>()`).
    ///
    /// The underlying allocation may be larger — runtimes are free to round up
    /// or pad — so this is the size of the *data*, not of the allocation.
    pub fn size_bytes(&self) -> usize {
        self.len * core::mem::size_of::<E>()
    }

    /// The underlying CubeCL handle.
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// A kernel argument for this buffer.
    ///
    /// Safe, unlike the CubeCL call it wraps: the length passed to
    /// `BufferArg::from_raw_parts` is the one recorded when the allocation was
    /// made, so a kernel bounds-checking against `buf.len()` sees the real
    /// element count.
    pub fn arg(&self) -> BufferArg<R> {
        // SAFETY: `self.len` is the element count this allocation was created
        // with (see the `from_raw_parts` safety contract above), so the
        // argument's declared length matches the allocation.
        unsafe { BufferArg::from_raw_parts(self.handle.clone(), self.len) }
    }
}

impl<R: Runtime, E: CubeElement> Clone for DeviceBuffer<R, E> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            len: self.len,
            _marker: PhantomData,
        }
    }
}

impl<R: Runtime, E: CubeElement> core::fmt::Debug for DeviceBuffer<R, E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeviceBuffer")
            .field("elem", &E::type_name())
            .field("len", &self.len)
            .finish()
    }
}
