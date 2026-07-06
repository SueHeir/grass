//! Minimal hand-rolled byte serialization for Phase 3 remote sub-Apps.
//!
//! The agnostic-coupling design wants to keep coupling crates (e.g.
//! `dirt_cfd_fsi`) **wire-agnostic** — `SphereSet` is a plain struct with
//! no `Serialize`/`Deserialize` derive baked in. Wire packing is opted into
//! per type, in the binary that wires up the remote coupling. That binary
//! `impl Wire for SphereSet { ... }` once and registers the type via
//! `.send_each_iter::<SphereSet>()`.
//!
//! ## What's in 1.0
//!
//! - The trait itself.
//! - Impls for primitives (`f32` / `f64` / signed + unsigned ints / `bool`)
//!   and a few common composites (`[f64; 3]`, `Vec<f64>`, `String`).
//! - A fallible [`Wire::try_unpack`] path for diagnostics when a peer sends
//!   malformed or mismatched bytes.
//!
//! ## Future
//!
//! - Optional `serde` integration behind a feature flag (blanket impl over
//!   `T: Serialize + DeserializeOwned` if enabled).
//! - A `#[derive(Wire)]` proc macro that emits a `bincode`-style packing
//!   for plain old data structs.
//!
//! Both are deliberately not in Phase 3 — keep the dependency surface zero
//! until there's real demand.

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Error returned when a [`Wire`] payload cannot be decoded as the requested
/// type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireUnpackError {
    type_name: &'static str,
    payload_len: usize,
    detail: String,
}

impl WireUnpackError {
    /// Build an error for `T` from a payload length and short explanation.
    pub fn new<T: 'static>(payload_len: usize, detail: impl Into<String>) -> Self {
        Self {
            type_name: std::any::type_name::<T>(),
            payload_len,
            detail: detail.into(),
        }
    }

    /// Rust type the caller attempted to unpack.
    pub fn type_name(&self) -> &'static str {
        self.type_name
    }

    /// Number of bytes in the payload that failed to decode.
    pub fn payload_len(&self) -> usize {
        self.payload_len
    }

    /// Human-readable reason the payload failed to decode.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for WireUnpackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot unpack {} from {} byte payload: {}",
            self.type_name, self.payload_len, self.detail
        )
    }
}

impl std::error::Error for WireUnpackError {}

fn require_len<T: 'static>(buf: &[u8], expected: usize) -> Result<(), WireUnpackError> {
    if buf.len() == expected {
        Ok(())
    } else {
        Err(WireUnpackError::new::<T>(
            buf.len(),
            format!("expected exactly {expected} bytes"),
        ))
    }
}

fn read_u32_len<T: 'static>(buf: &[u8]) -> Result<usize, WireUnpackError> {
    if buf.len() < 4 {
        return Err(WireUnpackError::new::<T>(
            buf.len(),
            "expected 4 byte little-endian length header",
        ));
    }
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&buf[..4]);
    Ok(u32::from_le_bytes(len_bytes) as usize)
}

/// Pack a value into bytes and recover it later. Hand-rolled per type.
///
/// `pack` produces an owned `Vec<u8>` so the caller can hand it to a
/// [`Transport`](crate::Transport) without further copying. `unpack`
/// consumes the entire slice — partial / framed parses are out of scope;
/// the [`Transport`](crate::Transport) message boundary is the unit of work.
///
/// `Send + Sync + 'static` keeps `Box<dyn Wire>`-style use cases open for
/// future work, but isn't load-bearing today.
pub trait Wire: Send + Sync + 'static {
    /// Serialise this value into a fresh byte buffer.
    fn pack(&self) -> Vec<u8>;

    /// Reconstruct a value from a buffer produced by [`pack`](Self::pack),
    /// returning a diagnostic instead of panicking on malformed bytes.
    fn try_unpack(buf: &[u8]) -> Result<Self, WireUnpackError>
    where
        Self: Sized,
    {
        catch_unwind(AssertUnwindSafe(|| Self::unpack(buf))).map_err(|panic| {
            let detail = panic
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .map(|msg| format!("legacy unpack panicked: {msg}"))
                .unwrap_or_else(|| "legacy unpack panicked".to_string());
            WireUnpackError::new::<Self>(buf.len(), detail)
        })
    }

    /// Reconstruct a value from a buffer produced by [`pack`](Self::pack).
    /// The slice MUST be exactly one packed message (no framing here).
    fn unpack(buf: &[u8]) -> Self
    where
        Self: Sized;
}

// ─── Primitive impls ────────────────────────────────────────────────────────

macro_rules! impl_wire_le_bytes {
    ($($t:ty: $n:literal),*) => {
        $(
            impl Wire for $t {
                fn pack(&self) -> Vec<u8> { self.to_le_bytes().to_vec() }
                fn try_unpack(buf: &[u8]) -> Result<Self, WireUnpackError> {
                    require_len::<Self>(buf, $n)?;
                    let mut a = [0u8; $n];
                    a.copy_from_slice(buf);
                    Ok(<$t>::from_le_bytes(a))
                }
                fn unpack(buf: &[u8]) -> Self {
                    Self::try_unpack(buf).unwrap()
                }
            }
        )*
    };
}

impl_wire_le_bytes!(
    f32: 4, f64: 8, i32: 4, i64: 8, u32: 4, u64: 8
);

impl Wire for bool {
    fn pack(&self) -> Vec<u8> {
        vec![if *self { 1 } else { 0 }]
    }
    fn try_unpack(buf: &[u8]) -> Result<Self, WireUnpackError> {
        require_len::<Self>(buf, 1)?;
        match buf[0] {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(WireUnpackError::new::<Self>(
                buf.len(),
                format!("expected bool byte 0 or 1, found {other}"),
            )),
        }
    }
    fn unpack(buf: &[u8]) -> Self {
        Self::try_unpack(buf).unwrap()
    }
}

impl Wire for [f64; 3] {
    fn pack(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24);
        for v in self {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
    fn try_unpack(buf: &[u8]) -> Result<Self, WireUnpackError> {
        require_len::<Self>(buf, 24)?;
        let mut out = [0.0f64; 3];
        for (i, slot) in out.iter_mut().enumerate() {
            let mut a = [0u8; 8];
            a.copy_from_slice(&buf[i * 8..i * 8 + 8]);
            *slot = f64::from_le_bytes(a);
        }
        Ok(out)
    }
    fn unpack(buf: &[u8]) -> Self {
        Self::try_unpack(buf).unwrap()
    }
}

impl Wire for Vec<f64> {
    fn pack(&self) -> Vec<u8> {
        // [u32 len LE | n × f64 LE]
        let mut out = Vec::with_capacity(4 + self.len() * 8);
        out.extend_from_slice(&(self.len() as u32).to_le_bytes());
        for v in self {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
    fn try_unpack(buf: &[u8]) -> Result<Self, WireUnpackError> {
        let n = read_u32_len::<Self>(buf)?;
        let expected = 4usize
            .checked_add(n.checked_mul(8).ok_or_else(|| {
                WireUnpackError::new::<Self>(buf.len(), "length header overflows usize")
            })?)
            .ok_or_else(|| {
                WireUnpackError::new::<Self>(buf.len(), "payload length overflows usize")
            })?;
        if buf.len() != expected {
            return Err(WireUnpackError::new::<Self>(
                buf.len(),
                format!("length header declares {n} f64 values ({expected} bytes total)"),
            ));
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let off = 4 + i * 8;
            let mut a = [0u8; 8];
            a.copy_from_slice(&buf[off..off + 8]);
            out.push(f64::from_le_bytes(a));
        }
        Ok(out)
    }
    fn unpack(buf: &[u8]) -> Self {
        Self::try_unpack(buf).unwrap()
    }
}

impl Wire for String {
    fn pack(&self) -> Vec<u8> {
        // [u32 len LE | n × u8]
        let bytes = self.as_bytes();
        let mut out = Vec::with_capacity(4 + bytes.len());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
        out
    }
    fn try_unpack(buf: &[u8]) -> Result<Self, WireUnpackError> {
        let n = read_u32_len::<Self>(buf)?;
        let expected = 4usize.checked_add(n).ok_or_else(|| {
            WireUnpackError::new::<Self>(buf.len(), "payload length overflows usize")
        })?;
        if buf.len() != expected {
            return Err(WireUnpackError::new::<Self>(
                buf.len(),
                format!("length header declares {n} UTF-8 bytes ({expected} bytes total)"),
            ));
        }
        String::from_utf8(buf[4..].to_vec()).map_err(|err| {
            WireUnpackError::new::<Self>(buf.len(), format!("declared bytes are not UTF-8: {err}"))
        })
    }
    fn unpack(buf: &[u8]) -> Self {
        Self::try_unpack(buf).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_f64() {
        let x = 1.234e-5_f64;
        assert_eq!(f64::unpack(&x.pack()), x);
    }
    #[test]
    fn round_trip_array_3() {
        let x = [1.0, 2.0, 3.0];
        assert_eq!(<[f64; 3]>::unpack(&x.pack()), x);
    }
    #[test]
    fn round_trip_vec_f64() {
        let x = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(Vec::<f64>::unpack(&x.pack()), x);
    }
    #[test]
    fn round_trip_string() {
        let x = "hello, peer".to_string();
        assert_eq!(String::unpack(&x.pack()), x);
    }
    #[test]
    fn round_trip_bool() {
        assert!(bool::unpack(&true.pack()));
        assert!(!bool::unpack(&false.pack()));
    }

    #[test]
    fn string_try_unpack_reports_truncated_payload() {
        let mut buf = 5u32.to_le_bytes().to_vec();
        buf.extend_from_slice(b"abc");

        let err = String::try_unpack(&buf).unwrap_err();

        assert_eq!(err.type_name(), "alloc::string::String");
        assert_eq!(err.payload_len(), 7);
        assert!(err.detail().contains("declares 5 UTF-8 bytes"));
    }

    #[test]
    fn string_try_unpack_reports_non_utf8_payload() {
        let mut buf = 2u32.to_le_bytes().to_vec();
        buf.extend_from_slice(&[0xff, 0xff]);

        let err = String::try_unpack(&buf).unwrap_err();

        assert_eq!(err.type_name(), "alloc::string::String");
        assert_eq!(err.payload_len(), 6);
        assert!(err.detail().contains("not UTF-8"));
    }

    #[test]
    fn primitive_try_unpack_rejects_mismatched_payload_size() {
        let err = f64::try_unpack(&1u32.pack()).unwrap_err();

        assert_eq!(err.type_name(), "f64");
        assert_eq!(err.payload_len(), 4);
        assert!(err.detail().contains("expected exactly 8 bytes"));
    }

    #[derive(Debug, PartialEq)]
    struct LegacyChecked(u64);

    impl Wire for LegacyChecked {
        fn pack(&self) -> Vec<u8> {
            self.0.pack()
        }

        fn unpack(buf: &[u8]) -> Self {
            assert_eq!(buf.len(), 8, "legacy payload length mismatch");
            let mut a = [0u8; 8];
            a.copy_from_slice(buf);
            Self(u64::from_le_bytes(a))
        }
    }

    #[test]
    fn default_try_unpack_wraps_legacy_unpack_panic() {
        let err = LegacyChecked::try_unpack(&[1, 2, 3]).unwrap_err();

        assert_eq!(err.type_name(), "grass_multi::wire::tests::LegacyChecked");
        assert_eq!(err.payload_len(), 3);
        assert!(err.detail().contains("legacy unpack panicked"));
    }
}
