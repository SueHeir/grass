//! Minimal hand-rolled byte serialization for Phase 3 remote sub-Apps.
//!
//! The agnostic-coupling design wants to keep coupling crates (e.g.
//! `mddem_cfd_fsi`) **wire-agnostic** — `SphereSet` is a plain struct with
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
                fn unpack(buf: &[u8]) -> Self {
                    let mut a = [0u8; $n];
                    a.copy_from_slice(&buf[..$n]);
                    <$t>::from_le_bytes(a)
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
    fn unpack(buf: &[u8]) -> Self {
        buf[0] != 0
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
    fn unpack(buf: &[u8]) -> Self {
        let mut out = [0.0f64; 3];
        for (i, slot) in out.iter_mut().enumerate() {
            let mut a = [0u8; 8];
            a.copy_from_slice(&buf[i * 8..i * 8 + 8]);
            *slot = f64::from_le_bytes(a);
        }
        out
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
    fn unpack(buf: &[u8]) -> Self {
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&buf[..4]);
        let n = u32::from_le_bytes(len_bytes) as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let off = 4 + i * 8;
            let mut a = [0u8; 8];
            a.copy_from_slice(&buf[off..off + 8]);
            out.push(f64::from_le_bytes(a));
        }
        out
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
    fn unpack(buf: &[u8]) -> Self {
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&buf[..4]);
        let n = u32::from_le_bytes(len_bytes) as usize;
        String::from_utf8(buf[4..4 + n].to_vec())
            .expect("Wire: String unpack found non-UTF-8 bytes")
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
}
