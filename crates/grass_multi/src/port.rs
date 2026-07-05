//! Coupling ports — the minimal cross-solver exchange contract.
//!
//! A [`Port<T>`] is a typed, named exchange slot living on the **parent** App.
//! One solver **exposes** a value of the contract type `T` into it; one or more
//! other solvers **consume** that value. The only thing the coupled solvers
//! share is the contract type `T` — neither names the other's namespace or
//! internal resource types. That is what makes coupling *ergonomic* and keeps
//! solvers independent: a producer can be replaced by any other producer that
//! publishes the same `T`, and a consumer by any consumer that reads it.
//!
//! `T` is deliberately paradigm-agnostic. It can be:
//!   - a scalar source term or boundary value (`struct Flux(f64)`),
//!   - a sampled field (`Vec<f64>`),
//!   - particle data, an aggregate, or anything else `'static`.
//!
//! Nothing here assumes particles, meshes, or any discretization: it is just a
//! typed mailbox plus two system constructors that copy between a sub-App's own
//! resource and that mailbox.
//!
//! ## The contract, in three lines
//!
//! ```rust,ignore
//! use grass_multi::{expose_field, consume_field, MultiAppExt};
//!
//! // The shared contract: a scalar source term. Only this type is shared.
//! struct Flux(f64);
//!
//! parent.add_port::<Flux>();                                       // 1. the slot
//! parent.add_update_system(                                        // 2. producer exposes
//!     expose_field::<HeatField, Flux>("field", |f| Flux(f.total())),
//!     Phase::Couple);
//! parent.add_update_system(                                        // 3. consumer reads
//!     consume_field::<Particle, Flux>("particle", |p, flux| p.force = flux.0),
//!     Phase::Couple);
//! ```
//!
//! `HeatField` is private to the `"field"` solver and `Particle` to the
//! `"particle"` solver; the coupling only mentions each solver's *own* type
//! plus the shared `Flux`. See the `coupling_port` integration test for a
//! runnable end-to-end example that couples a mesh-style field solver to a
//! point-particle solver and checks the coupled physics.
//!
//! ## Ordering
//!
//! `expose_field` must run *after* the producer's tick (so it reads fresh
//! state) and `consume_field` *before* the consumer's tick (so the consumer
//! integrates with the value it was handed). The canonical parent phase order
//! is therefore `TickProducer → Couple(expose → consume) → TickConsumer →
//! Check`. Ordering is the contract, exactly as for hand-written [`Multi`]
//! couplers — see the crate-level docs.
//!
//! ## Why a port instead of reading the producer directly?
//!
//! You *can* couple with a bare [`MultiRes`](crate::MultiRes) /
//! [`MultiResMut`](crate::MultiResMut) system that reads the producer's
//! resource and writes the consumer's. The port adds one thing: the consumer
//! no longer names the producer. It depends only on `Port<Flux>`, so the two
//! solvers compile independently and either side can be swapped for another
//! that speaks the same `T`. For a one-off, tightly-bound pair a direct
//! `Multi` coupler is fine; reach for a port when you want the exchange itself
//! to be the stable, reusable interface.

use crate::multi::Multi;
use grass_scheduler::{Res, ResMut};

/// A typed exchange slot — the coupling contract between solvers.
///
/// A producer publishes with [`set`](Self::set); consumers read the latest
/// value with [`get`](Self::get). Registered on the parent App as an ordinary
/// resource (see [`MultiAppExt::add_port`](crate::MultiAppExt::add_port)).
///
/// The port holds only the *most recent* published value — it is a
/// last-writer-wins slot, not a queue. Multiple producers writing the same
/// port in one outer iter is a scheduling mistake (order-dependent); give each
/// producer its own port type instead.
pub struct Port<T> {
    value: Option<T>,
}

impl<T> Default for Port<T> {
    fn default() -> Self {
        Self { value: None }
    }
}

impl<T> Port<T> {
    /// A fresh, empty port. Same as [`Default::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// The most recently published value, or `None` if no producer has
    /// published yet this run.
    pub fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }

    /// Publish a value, replacing any previous one (last-writer-wins).
    pub fn set(&mut self, v: T) {
        self.value = Some(v);
    }

    /// `true` once a producer has published at least once.
    pub fn is_published(&self) -> bool {
        self.value.is_some()
    }
}

/// System constructor: **expose** a field/source term from sub-App `ns` into
/// the shared [`Port<T>`] on the parent.
///
/// `extract` reads the producer's *own* resource `Src` — a type only the
/// producer knows — and returns the contract value `T`. Register it in the
/// coupling phase, after the producer's tick:
///
/// ```rust,ignore
/// parent.add_update_system(
///     expose_field::<HeatField, Flux>("field", |f| Flux(f.total())),
///     Phase::Couple);
/// ```
///
/// Panics at run time if `ns` is not a registered sub-App or has no resource
/// of type `Src` (same contract as [`Multi::expect_read`]). The parent must
/// hold a `Port<T>` resource — add one with
/// [`add_port`](crate::MultiAppExt::add_port).
pub fn expose_field<Src, T>(
    ns: &'static str,
    extract: impl Fn(&Src) -> T + 'static,
) -> impl FnMut(Multi, ResMut<Port<T>>)
where
    Src: 'static,
    T: 'static,
{
    move |world: Multi, mut port: ResMut<Port<T>>| {
        let src = world.expect_read::<Src>(ns);
        port.set(extract(&src));
    }
}

/// System constructor: **consume** the shared [`Port<T>`] into sub-App `ns`.
///
/// `apply` writes the contract value into the consumer's *own* resource `Dst`
/// — a type only the consumer knows. It is a **no-op until the port has been
/// published**, so it is safe to schedule unconditionally; wire the producer's
/// `expose_field` before it in the same phase (or an earlier one) to guarantee
/// a value is present.
///
/// ```rust,ignore
/// parent.add_update_system(
///     consume_field::<Particle, Flux>("particle", |p, flux| p.force = flux.0),
///     Phase::Couple);
/// ```
///
/// Panics at run time if `ns` is not a registered sub-App or has no resource
/// of type `Dst` once a value is present to apply.
pub fn consume_field<Dst, T>(
    ns: &'static str,
    apply: impl Fn(&mut Dst, &T) + 'static,
) -> impl FnMut(Multi, Res<Port<T>>)
where
    Dst: 'static,
    T: 'static,
{
    move |world: Multi, port: Res<Port<T>>| {
        if let Some(v) = port.get() {
            let mut dst = world.expect_write::<Dst>(ns);
            apply(&mut dst, v);
            // `dst` derefs to `&mut Dst`; `apply` takes `&mut Dst`.
        }
    }
}
