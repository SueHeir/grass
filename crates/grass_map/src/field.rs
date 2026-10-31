//! Fields tagged by how they transform under mapping.
//!
//! * [`Intensive`] — a value *at* a location: temperature, velocity, pressure.
//!   Mapped by sampling ([`ConsistentMap`](crate::ConsistentMap)); constants are
//!   preserved.
//! * [`Density`] — an amount *per unit measure*: heat flux in W/m², mass per
//!   volume. Also sampled, and convertible to and from [`Extensive`] with the
//!   entity measures.
//! * [`Extensive`] — an amount *owned by* an entity: force, heat, mass. Mapped
//!   by redistribution ([`ConservativeMap`](crate::ConservativeMap)); totals
//!   are preserved.
//!
//! The tag is a type parameter, so handing a force to a sampling map, or a
//! temperature to a conservative one, is a compile error:
//!
//! ```compile_fail
//! use grass_map::*;
//! let a = PointCloud::new(vec![[0.0; 3]]);
//! let map = MapBuilder::new(&a, &a).consistent();
//! let force = Field::<Extensive>::scalar(vec![1.0]);
//! let mut out = Field::<Extensive>::scalar(vec![0.0]);
//! map.apply(&force, &mut out); // error: Extensive is not Sampled
//! ```

use std::marker::PhantomData;

mod sealed {
    pub trait Sealed {}
}

/// Marker trait for the three field kinds.
pub trait FieldKind: sealed::Sealed + 'static {
    /// Human-readable kind name.
    const NAME: &'static str;
}

/// Kinds that are mapped by sampling: [`Intensive`] and [`Density`].
pub trait Sampled: FieldKind {}

/// A value at a location (temperature, velocity).
#[derive(Debug, Clone, Copy)]
pub struct Intensive;
/// An amount per unit length, area or volume (heat flux, mass density).
#[derive(Debug, Clone, Copy)]
pub struct Density;
/// An amount owned by an entity (force, heat, mass).
#[derive(Debug, Clone, Copy)]
pub struct Extensive;

impl sealed::Sealed for Intensive {}
impl sealed::Sealed for Density {}
impl sealed::Sealed for Extensive {}
impl FieldKind for Intensive {
    const NAME: &'static str = "intensive";
}
impl FieldKind for Density {
    const NAME: &'static str = "density";
}
impl FieldKind for Extensive {
    const NAME: &'static str = "extensive";
}
impl Sampled for Intensive {}
impl Sampled for Density {}

/// Values on the entities of one discretization, `ncomp` components per
/// entity, interleaved (`values[entity * ncomp + component]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Field<K: FieldKind> {
    /// Interleaved values.
    pub values: Vec<f64>,
    /// Components per entity (1 for a scalar, 3 for a vector).
    pub ncomp: usize,
    kind: PhantomData<K>,
}

impl<K: FieldKind> Field<K> {
    /// A field with `ncomp` interleaved components per entity.
    ///
    /// # Panics
    /// Panics if `ncomp == 0` or `values.len()` is not a multiple of `ncomp`.
    pub fn new(values: Vec<f64>, ncomp: usize) -> Self {
        assert!(ncomp > 0, "ncomp must be positive");
        assert_eq!(
            values.len() % ncomp,
            0,
            "values length is not a multiple of ncomp"
        );
        Self {
            values,
            ncomp,
            kind: PhantomData,
        }
    }

    /// A scalar field.
    pub fn scalar(values: Vec<f64>) -> Self {
        Self::new(values, 1)
    }

    /// A field of `len` entities, all components zero.
    pub fn zeros(len: usize, ncomp: usize) -> Self {
        Self::new(vec![0.0; len * ncomp], ncomp)
    }

    /// Number of entities.
    pub fn len(&self) -> usize {
        self.values.len() / self.ncomp
    }

    /// Whether the field has no entities.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Sum of each component over all entities.
    pub fn totals(&self) -> Vec<f64> {
        let mut t = vec![0.0; self.ncomp];
        for (i, v) in self.values.iter().enumerate() {
            t[i % self.ncomp] += v;
        }
        t
    }
}

impl Field<Density> {
    /// Multiply by entity measures (lengths, areas or volumes) to get the
    /// amount each entity owns.
    pub fn to_extensive(&self, measures: &[f64]) -> Field<Extensive> {
        assert_eq!(measures.len(), self.len(), "measures length mismatch");
        let values = self
            .values
            .iter()
            .enumerate()
            .map(|(i, v)| v * measures[i / self.ncomp])
            .collect();
        Field::new(values, self.ncomp)
    }
}

impl Field<Extensive> {
    /// Divide by entity measures to get an amount per unit measure.
    /// Entities with zero measure get zero density.
    pub fn to_density(&self, measures: &[f64]) -> Field<Density> {
        assert_eq!(measures.len(), self.len(), "measures length mismatch");
        let values = self
            .values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let m = measures[i / self.ncomp];
                if m > 0.0 {
                    v / m
                } else {
                    0.0
                }
            })
            .collect();
        Field::new(values, self.ncomp)
    }
}
