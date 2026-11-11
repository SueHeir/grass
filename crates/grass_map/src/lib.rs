//! # grass_map — mapping data between any two discretizations
//!
//! Coupled solvers rarely share a mesh. A fluid wall and a heat-shield
//! surface are triangulated differently; particles sit inside fluid cells;
//! radiation needs the flow field along rays. `grass_map` moves data between
//! all of these with one idea.
//!
//! Every [`Discretization`] answers two questions about itself:
//!
//! * **"Where and how do I integrate?"** — weighted
//!   [`sample_points`](Discretization::sample_points) per entity (Gauss
//!   points of a cell, a particle weighted by its volume).
//! * **"What is my value at this point?"** —
//!   [`evaluate`](Discretization::evaluate) returns the entities whose basis
//!   is nonzero there (the enclosing cell, the triangle under a projected
//!   point, the particles within a kernel radius).
//!
//! Sampling one side at the other's points gives one sparse operator `W`.
//! Intensive fields (temperature, velocity) map through `W`; extensive fields
//! (force, heat, mass) map through a transpose, so totals are conserved
//! exactly. Fields carry their [`Intensive`], [`Density`] or [`Extensive`]
//! kind as a type parameter, so mapping in the wrong direction does not
//! compile.
//!
//! ```
//! use grass_map::*;
//!
//! // A coarse and a fine grid over the same box.
//! let coarse = UniformGrid::new([0.0; 3], [1.0; 3], [4, 4, 1]);
//! let fine = UniformGrid::new([0.0; 3], [1.0; 3], [9, 7, 1]);
//!
//! // Temperature: sampled, constants preserved.
//! let to_fine = MapBuilder::new(&coarse, &fine).consistent();
//! let t_coarse = Field::<Intensive>::scalar(vec![300.0; 16]);
//! let mut t_fine = Field::<Intensive>::zeros(63, 1);
//! to_fine.apply(&t_coarse, &mut t_fine);
//! assert!(t_fine.values.iter().all(|t| (t - 300.0).abs() < 1e-9));
//!
//! // Heat: redistributed, total preserved.
//! let heat_map = MapBuilder::new(&coarse, &fine).conservative();
//! let q_coarse = Field::<Extensive>::scalar((0..16).map(|i| i as f64).collect());
//! let mut q_fine = Field::<Extensive>::zeros(63, 1);
//! heat_map.apply(&q_coarse, &mut q_fine);
//! assert!((q_fine.totals()[0] - 120.0).abs() < 1e-9);
//! ```
//!
//! GRASS itself knows nothing about particles or meshes: solver libraries
//! implement [`Discretization`] for their own types. The built-ins
//! ([`PointCloud`], [`UniformGrid`], [`TriSurface`]) cover common shapes and
//! serve as reference implementations.

#![warn(missing_docs)]

mod builtin;
mod discretization;
mod field;
mod geom;
mod index;
mod map;
mod sparse;

pub use builtin::{CellBasis, PointBasis, PointCloud, SurfaceDofs, TriSurface, UniformGrid};
pub use discretization::{Discretization, SamplePoint, Sampling, TopoDim};
pub use field::{Density, Extensive, Field, FieldKind, Intensive, Sampled};
pub use geom::{Aabb, Point};
pub use index::SpatialIndex;
pub use map::{AdjointPair, ConservativeMap, ConsistentMap, Map, MapBuilder, MapReport, Method};
pub use sparse::CsrMatrix;
