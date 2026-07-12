//! Typed cross-namespace borrow handles — [`MultiRes`] / [`MultiResMut`].
//!
//! These are the type-checked counterparts to the runtime-string
//! [`Multi`](crate::Multi) accessor. Use them as system parameters when
//! the namespace is known at compile time:
//!
//! ```rust,ignore
//! use grass_multi::{Namespace, MultiRes, MultiResMut};
//!
//! #[derive(Namespace)] pub struct A;
//! #[derive(Namespace)] pub struct B;
//!
//! fn exchange_positions(
//!     a_state: MultiRes<OscState, A>,
//!     b_state: MultiRes<OscState, B>,
//!     mut a_other: MultiResMut<OtherX, A>,
//!     mut b_other: MultiResMut<OtherX, B>,
//! ) {
//!     a_other.0 = b_state.x;
//!     b_other.0 = a_state.x;
//! }
//! ```
//!
//! The signature is the contract: which sub-App is read, which is
//! written, what types cross. Typo a namespace marker → compile error.
//!
//! ## How they work
//!
//! Each `MultiRes*` resolves a non-owning participant handle from the shared
//! [`MultiContext`](crate::MultiContext), then holds both the participant guard
//! and the `Ref` / `RefMut` on the specific resource cell for `T`. The same
//! context is installed in the parent and every local child before preparation,
//! so an unchanged system signature works at either scheduler level.
//!
//! Because both must coexist in one struct (self-referential), the
//! constructor uses one `unsafe` lifetime extension. The retained `Rc`
//! participant owns the physics object, while its `Ref` guard prevents that
//! participant from being stepped or destroyed until the resource borrow ends.
//!
//! ## When to use which Multi
//!
//! - **`Multi` / `MultiRef` / `MultiMut`** (string-keyed): namespace is
//!   runtime data — config-driven, debug taps, per-instance bindings.
//! - **`MultiRes<T, NS>` / `MultiResMut<T, NS>`** (typed): namespace is fixed at
//!   the system's site of definition. The common case for coupling
//!   systems between known sub-Apps.

use crate::multi::{MultiContext, Namespace, Participant};
use crate::Physics;
use grass_scheduler::{Res, ResMut, SystemParam};
use std::any::{Any, TypeId};
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

// ─── MultiRes ──────────────────────────────────────────────────────────────────

/// Read-only borrow of resource `T` from sub-App `NS`. Created
/// automatically as a system parameter; derefs to `&T`.
pub struct MultiRes<'w, T: 'static, NS: Namespace> {
    inner: Ref<'w, T>,
    _physics: Ref<'w, Box<dyn Physics>>,
    _participant: Participant,
    _ns: PhantomData<NS>,
}

impl<T: 'static, NS: Namespace> Deref for MultiRes<'_, T, NS> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<'res, T: 'static, NS: Namespace> SystemParam for MultiRes<'res, T, NS> {
    type Item<'new> = MultiRes<'new, T, NS>;
    fn retrieve<'r>(
        resources: &'r [RefCell<Box<dyn Any>>],
        index: usize,
        _locals: *mut HashMap<TypeId, Box<dyn Any>>,
    ) -> Self::Item<'r> {
        let context_guard = resources[index].borrow();
        let context = context_guard
            .downcast_ref::<MultiContext>()
            .expect("MultiRes: MultiContext resource type mismatch (impossible)");
        let participant = context.resolve(NS::NAME).unwrap_or_else(|| {
            panic!(
                "MultiRes: namespace `{}` is not registered on the parent App",
                NS::NAME
            )
        });
        drop(context_guard);
        let participant_cell = Rc::as_ptr(&participant);
        // SAFETY: `participant` is retained in the returned handle.
        let physics: Ref<'r, Box<dyn Physics>> = unsafe { (&*participant_cell).borrow() };
        let inner_cell = physics.resource_cell(TypeId::of::<T>()).unwrap_or_else(|| {
            panic!(
                "MultiRes: sub-App `{}` has no resource of type `{}`",
                NS::NAME,
                std::any::type_name::<T>()
            )
        });

        // SAFETY: `inner_cell` is borrowed from `subapps`, which is
        // borrowed from `outer`. Both `outer` and the resulting
        // `Ref<T>` are stored in the same struct; as long as `outer`
        // lives, `inner_cell`'s underlying address is valid (no one can
        // mutate `SubApps` to drop or move the underlying `Physics` /
        // its resource cell). Lifetime extension to 'r is therefore
        // sound for the duration of the returned struct.
        let inner_cell_extended: &'r RefCell<Box<dyn Any>> = unsafe { &*(inner_cell as *const _) };

        let inner = Ref::map(inner_cell_extended.borrow(), |b| {
            b.downcast_ref::<T>().expect(
                "MultiRes: resource type mismatch — registered as a different concrete type",
            )
        });

        MultiRes {
            inner,
            _physics: physics,
            _participant: participant,
            _ns: PhantomData,
        }
    }
    fn resource_type_id() -> Option<(TypeId, &'static str)> {
        Some((TypeId::of::<MultiContext>(), "grass_multi::MultiContext"))
    }
}

// ─── MultiResMut ──────────────────────────────────────────────────────────────────

/// Mutable borrow of resource `T` from sub-App `NS`. Created
/// automatically as a system parameter; derefs to `&mut T`.
pub struct MultiResMut<'w, T: 'static, NS: Namespace> {
    inner: RefMut<'w, T>,
    _physics: Ref<'w, Box<dyn Physics>>,
    _participant: Participant,
    _ns: PhantomData<NS>,
}

impl<T: 'static, NS: Namespace> Deref for MultiResMut<'_, T, NS> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T: 'static, NS: Namespace> DerefMut for MultiResMut<'_, T, NS> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<'res, T: 'static, NS: Namespace> SystemParam for MultiResMut<'res, T, NS> {
    type Item<'new> = MultiResMut<'new, T, NS>;
    fn retrieve<'r>(
        resources: &'r [RefCell<Box<dyn Any>>],
        index: usize,
        _locals: *mut HashMap<TypeId, Box<dyn Any>>,
    ) -> Self::Item<'r> {
        let context_guard = resources[index].borrow();
        let context = context_guard
            .downcast_ref::<MultiContext>()
            .expect("MultiResMut: MultiContext resource type mismatch (impossible)");
        let participant = context.resolve(NS::NAME).unwrap_or_else(|| {
            panic!(
                "MultiResMut: namespace `{}` is not registered on the parent App",
                NS::NAME
            )
        });
        drop(context_guard);
        let participant_cell = Rc::as_ptr(&participant);
        let physics: Ref<'r, Box<dyn Physics>> = unsafe { (&*participant_cell).borrow() };
        let inner_cell = physics.resource_cell(TypeId::of::<T>()).unwrap_or_else(|| {
            panic!(
                "MultiResMut: sub-App `{}` has no resource of type `{}`",
                NS::NAME,
                std::any::type_name::<T>()
            )
        });

        // SAFETY: same invariant as `MultiRes::retrieve` — `outer` keeps
        // SubApps borrowed for the whole lifetime of the struct, so
        // `inner_cell`'s pointer remains valid. `borrow_mut` here gives
        // exclusive access to T's cell; concurrent MultiRes<T, NS> /
        // MultiResMut<T, NS> on the same (T, NS) pair is a programmer error
        // and will RefCell-panic at runtime, just like normal Res/ResMut.
        let inner_cell_extended: &'r RefCell<Box<dyn Any>> = unsafe { &*(inner_cell as *const _) };

        let inner = RefMut::map(inner_cell_extended.borrow_mut(), |b| {
            b.downcast_mut::<T>().expect(
                "MultiResMut: resource type mismatch — registered as a different concrete type",
            )
        });

        MultiResMut {
            inner,
            _physics: physics,
            _participant: participant,
            _ns: PhantomData,
        }
    }
    fn resource_type_id() -> Option<(TypeId, &'static str)> {
        Some((TypeId::of::<MultiContext>(), "grass_multi::MultiContext"))
    }
}

// Mark `_res` lifetime params unused so users don't have to spell them.
// Already implicit via `'res` in the impl signature; nothing else to do.

// Suppress unused-lifetime warnings on Res / ResMut imports — they're
// here for documentation alignment with the accessor pair, not used
// directly in this module.
#[allow(dead_code)]
fn _unused_imports() {
    let _: fn(Res<u8>) = |_| {};
    let _: fn(ResMut<u8>) = |_| {};
}
