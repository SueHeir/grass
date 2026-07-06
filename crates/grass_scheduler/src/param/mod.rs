//! System parameters, system conversion, schedule phases, and ordering helpers.

use std::any::{Any, TypeId};
use std::cell::{Ref, RefCell, RefMut};
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

// ─── System macro ─────────────────────────────────────────────────────────────

macro_rules! impl_system {
    ($($params:ident),*) => {
        #[allow(non_snake_case, unused)]
        impl<F, $($params: SystemParam),*> System for FunctionSystem<($($params,)*), F>
            where
                for<'a, 'b> &'a mut F:
                    FnMut($($params),*) +
                    FnMut($(<$params as SystemParam>::Item<'b>),*)
        {
            fn run(&mut self, resources: &[RefCell<Box<dyn Any>>]) {
                fn call_inner<$($params),*>(
                    mut f: impl FnMut($($params),*),
                    $($params: $params),*
                ) { f($($params),*) }

                let locals_ptr = &mut self.locals as *mut _;
                let mut _param_idx = 0usize;
                $(
                    let $params = $params::retrieve(resources, self.indices[_param_idx], locals_ptr);
                    _param_idx += 1;
                )*
                call_inner(&mut self.f, $($params),*)
            }

            fn prepare(&mut self, index: &HashMap<TypeId, usize>) -> Vec<String> {
                self.indices.clear();
                self.accesses.clear();
                let mut _missing = Vec::new();
                $(
                    let _type_info = <$params as SystemParam>::resource_type_id();
                    let _idx = _type_info
                        .and_then(|(tid, _)| index.get(&tid).copied())
                        .unwrap_or(usize::MAX);
                    if _idx == usize::MAX && !<$params as SystemParam>::is_optional() {
                        if let Some((_, name)) = _type_info {
                            _missing.push(name.to_string());
                        }
                    }
                    let _kind = <$params as SystemParam>::access_kind();
                    if _kind != AccessKind::None && _idx != usize::MAX {
                        self.accesses.push((_idx, _kind));
                    }
                    self.indices.push(_idx);
                )*
                _missing
            }

            fn accesses(&self) -> &[(usize, AccessKind)] {
                &self.accesses
            }

            fn name(&self) -> &str { std::any::type_name::<F>() }
        }
    }
}

macro_rules! impl_into_system {
    ($($params:ident),*) => {
        impl<F, $($params: SystemParam),*> IntoSystem<($($params,)*)> for F
            where
                for<'a, 'b> &'a mut F:
                    FnMut($($params),*) +
                    FnMut($(<$params as SystemParam>::Item<'b>),*)
        {
            type System = FunctionSystem<($($params,)*), Self>;
            fn into_system(self) -> Self::System {
                FunctionSystem { f: self, marker: Default::default(), locals: HashMap::new(), indices: Vec::new(), accesses: Vec::new() }
            }
        }
    }
}

// ─── Condition macro ──────────────────────────────────────────────────────────

macro_rules! impl_condition {
    ($($params:ident),*) => {
        #[allow(non_snake_case, unused)]
        impl<F, $($params: SystemParam),*> Condition for FunctionCondition<($($params,)*), F>
            where
                for<'a, 'b> &'a mut F:
                    FnMut($($params),*) -> bool +
                    FnMut($(<$params as SystemParam>::Item<'b>),*) -> bool
        {
            fn evaluate(&mut self, resources: &[RefCell<Box<dyn Any>>]) -> bool {
                fn call_inner<$($params),*>(
                    mut f: impl FnMut($($params),*) -> bool,
                    $($params: $params),*
                ) -> bool { f($($params),*) }

                let locals_ptr = &mut self.locals as *mut _;
                let mut _param_idx = 0usize;
                $(
                    let $params = $params::retrieve(resources, self.indices[_param_idx], locals_ptr);
                    _param_idx += 1;
                )*
                call_inner(&mut self.f, $($params),*)
            }

            fn prepare(&mut self, index: &HashMap<TypeId, usize>) -> Vec<String> {
                self.indices.clear();
                let mut _missing = Vec::new();
                $(
                    let _type_info = <$params as SystemParam>::resource_type_id();
                    let _idx = _type_info
                        .and_then(|(tid, _)| index.get(&tid).copied())
                        .unwrap_or(usize::MAX);
                    if _idx == usize::MAX && !<$params as SystemParam>::is_optional() {
                        if let Some((_, name)) = _type_info {
                            _missing.push(name.to_string());
                        }
                    }
                    self.indices.push(_idx);
                )*
                _missing
            }

            fn name(&self) -> &str {
                std::any::type_name::<F>()
            }
        }
    }
}

macro_rules! impl_into_condition {
    ($($params:ident),*) => {
        impl<F, $($params: SystemParam),*> IntoCondition<($($params,)*)> for F
            where
                for<'a, 'b> &'a mut F:
                    FnMut($($params),*) -> bool +
                    FnMut($(<$params as SystemParam>::Item<'b>),*) -> bool
        {
            type Condition = FunctionCondition<($($params,)*), Self>;
            fn into_condition(self) -> Self::Condition {
                FunctionCondition { f: self, marker: Default::default(), locals: HashMap::new(), indices: Vec::new() }
            }
        }
    }
}

// ─── SystemParam ──────────────────────────────────────────────────────────────

// ANCHOR: SystemParam
/// How a [`SystemParam`] accesses its resource — surfaced to the scheduler so it
/// can mediate host↔device coherence (see `CoherenceRegistry`). `Res` reads,
/// `ResMut` writes, `Local` and resource-free params access nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessKind {
    /// Does not access a shared resource (e.g. [`Local`]).
    None,
    /// Shared read access ([`Res`], `Option<Res>`).
    Read,
    /// Exclusive write access ([`ResMut`], `Option<ResMut>`).
    Write,
}

/// Types that can be injected as parameters into system functions.
///
/// Implementors define how to retrieve a value from the scheduler's resource storage.
/// Built-in implementations include [`Res<T>`], [`ResMut<T>`], [`Local<T>`], and
/// their `Option<_>` wrappers.
pub trait SystemParam {
    /// The concrete type returned by [`retrieve`](Self::retrieve) for a given lifetime.
    type Item<'new>;

    /// Extracts this parameter from the resource storage.
    ///
    /// # Safety contract
    ///
    /// `locals` must point to a valid, exclusively-owned `HashMap` for the duration of the call.
    fn retrieve<'r>(
        resources: &'r [RefCell<Box<dyn Any>>],
        index: usize,
        locals: *mut HashMap<TypeId, Box<dyn Any>>,
    ) -> Self::Item<'r>;

    /// Returns the [`TypeId`] and human-readable name of the resource this param needs,
    /// or `None` if it doesn't require a resource (e.g., [`Local`]).
    fn resource_type_id() -> Option<(TypeId, &'static str)> {
        None
    }

    /// Returns `true` if this parameter is optional (won't cause a validation error
    /// when the resource is missing). Used by `Option<Res<T>>` and `Option<ResMut<T>>`.
    fn is_optional() -> bool {
        false
    }

    /// How this param accesses its resource (read/write/none). Defaults to `None`
    /// so resource-free params (e.g. [`Local`]) report no access; `Res`/`ResMut`
    /// and their `Option` wrappers override it.
    fn access_kind() -> AccessKind {
        AccessKind::None
    }
}
// ANCHOR_END: SystemParam

// ANCHOR: ResSystemParam
impl<'res, T: 'static> SystemParam for Res<'res, T> {
    type Item<'new> = Res<'new, T>;
    fn retrieve<'r>(
        resources: &'r [RefCell<Box<dyn Any>>],
        index: usize,
        _locals: *mut HashMap<TypeId, Box<dyn Any>>,
    ) -> Self::Item<'r> {
        let guard = resources[index].borrow();
        // Downcast once here; Deref uses the cached pointer.
        let ptr: *const T = guard
            .downcast_ref::<T>()
            .expect("Res<T>: resource type mismatch during downcast");
        Res { _guard: guard, ptr }
    }
    fn resource_type_id() -> Option<(TypeId, &'static str)> {
        Some((TypeId::of::<T>(), std::any::type_name::<T>()))
    }
    fn access_kind() -> AccessKind {
        AccessKind::Read
    }
}
// ANCHOR_END: ResSystemParam

// ANCHOR: ResMutSystemParam
impl<'res, T: 'static> SystemParam for ResMut<'res, T> {
    type Item<'new> = ResMut<'new, T>;
    fn retrieve<'r>(
        resources: &'r [RefCell<Box<dyn Any>>],
        index: usize,
        _locals: *mut HashMap<TypeId, Box<dyn Any>>,
    ) -> Self::Item<'r> {
        let mut guard = resources[index].borrow_mut();
        // Downcast once here; Deref/DerefMut use the cached pointer.
        let ptr: *mut T = guard
            .downcast_mut::<T>()
            .expect("ResMut<T>: resource type mismatch during downcast");
        ResMut { _guard: guard, ptr }
    }
    fn resource_type_id() -> Option<(TypeId, &'static str)> {
        Some((TypeId::of::<T>(), std::any::type_name::<T>()))
    }
    fn access_kind() -> AccessKind {
        AccessKind::Write
    }
}
// ANCHOR_END: ResMutSystemParam

// ─── Res / ResMut / Local ─────────────────────────────────────────────────────

// ANCHOR: Res
/// Shared immutable reference to resource `T`, injected into systems.
///
/// The downcast from `Box<dyn Any>` happens once at construction (in `retrieve`).
/// `Deref` then uses the cached pointer — no virtual calls in hot loops.
pub struct Res<'a, T: 'static> {
    _guard: Ref<'a, Box<dyn Any>>,
    ptr: *const T,
}
// ANCHOR_END: Res

impl<T: 'static> Deref for Res<'_, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        // SAFETY: ptr was obtained from downcast_ref in retrieve() and is valid
        // for the lifetime 'a, guaranteed by _guard holding the Ref borrow.
        unsafe { &*self.ptr }
    }
}

// ANCHOR: ResMut
/// Exclusive mutable reference to resource `T`, injected into systems.
///
/// The downcast from `Box<dyn Any>` happens once at construction (in `retrieve`).
/// `Deref`/`DerefMut` then use the cached pointer — no virtual calls in hot loops.
pub struct ResMut<'a, T: 'static> {
    _guard: RefMut<'a, Box<dyn Any>>,
    ptr: *mut T,
}

impl<T: 'static> Deref for ResMut<'_, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        // SAFETY: ptr was obtained from downcast_mut in retrieve() and is valid
        // for the lifetime 'a, guaranteed by _guard holding the RefMut borrow.
        unsafe { &*self.ptr }
    }
}
impl<T: 'static> DerefMut for ResMut<'_, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: same as Deref; exclusive access guaranteed by RefMut.
        unsafe { &mut *self.ptr }
    }
}
// ANCHOR_END: ResMut

// ANCHOR: Local
/// Per-system local state. Persists across invocations of the same system instance.
/// Initialized with `T::default()` on first access.
///
/// Unlike [`Res`]/[`ResMut`], a `Local<T>` is **not** a shared resource: each
/// system instance owns its own `T`, keyed by `TypeId` in that system's
/// private `locals` map. Two different systems that both take `Local<u32>`
/// see two independent counters, and a `Local<T>` is invisible to every other
/// system (you can't read it through `Res<T>`). It is `Default`-initialized
/// lazily on first access and lives as long as the system is registered — the
/// idiomatic way to keep per-system step counters, "first call" flags, or
/// previous-value caches without polluting the global resource table.
pub struct Local<'a, T: Default + 'static> {
    value: &'a mut T,
    _marker: PhantomData<&'a mut T>,
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
impl<'res, T: Default + 'static> SystemParam for Local<'res, T> {
    type Item<'new> = Local<'new, T>;
    fn retrieve<'r>(
        _resources: &'r [RefCell<Box<dyn Any>>],
        _index: usize,
        locals: *mut HashMap<TypeId, Box<dyn Any>>,
    ) -> Self::Item<'r> {
        // SAFETY: locals points to FunctionSystem::locals, exclusively owned by this
        // system and alive for the duration of this retrieve call.
        let map = unsafe { &mut *locals };
        let entry = map
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()));
        Local {
            value: entry
                .downcast_mut::<T>()
                .expect("Local<T>: type mismatch in per-system local storage"),
            _marker: PhantomData,
        }
    }
}

impl<T: Default + 'static> Deref for Local<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.value
    }
}
impl<T: Default + 'static> DerefMut for Local<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value
    }
}
// ANCHOR_END: Local

// ─── Option<Res<T>> / Option<ResMut<T>> ──────────────────────────────────────

impl<'res, T: 'static> SystemParam for Option<Res<'res, T>> {
    type Item<'new> = Option<Res<'new, T>>;
    fn retrieve<'r>(
        resources: &'r [RefCell<Box<dyn Any>>],
        index: usize,
        _locals: *mut HashMap<TypeId, Box<dyn Any>>,
    ) -> Self::Item<'r> {
        if index == usize::MAX {
            return None;
        }
        let guard = resources[index].borrow();
        let ptr: *const T = guard
            .downcast_ref::<T>()
            .expect("Option<Res<T>>: resource type mismatch during downcast");
        Some(Res { _guard: guard, ptr })
    }
    fn resource_type_id() -> Option<(TypeId, &'static str)> {
        Some((TypeId::of::<T>(), std::any::type_name::<T>()))
    }
    fn is_optional() -> bool {
        true
    }
    fn access_kind() -> AccessKind {
        AccessKind::Read
    }
}

impl<'res, T: 'static> SystemParam for Option<ResMut<'res, T>> {
    type Item<'new> = Option<ResMut<'new, T>>;
    fn retrieve<'r>(
        resources: &'r [RefCell<Box<dyn Any>>],
        index: usize,
        _locals: *mut HashMap<TypeId, Box<dyn Any>>,
    ) -> Self::Item<'r> {
        if index == usize::MAX {
            return None;
        }
        let mut guard = resources[index].borrow_mut();
        let ptr: *mut T = guard
            .downcast_mut::<T>()
            .expect("Option<ResMut<T>>: resource type mismatch during downcast");
        Some(ResMut { _guard: guard, ptr })
    }
    fn resource_type_id() -> Option<(TypeId, &'static str)> {
        Some((TypeId::of::<T>(), std::any::type_name::<T>()))
    }
    fn is_optional() -> bool {
        true
    }
    fn access_kind() -> AccessKind {
        AccessKind::Write
    }
}

// ─── System trait & FunctionSystem ───────────────────────────────────────────

// ANCHOR: System
/// A runnable unit of work that receives resources via dependency injection.
///
/// Most users never implement this directly — instead, write a plain function with
/// [`SystemParam`] parameters and the scheduler converts it via [`IntoSystem`].
pub trait System {
    /// Executes the system, borrowing resources from the scheduler's storage.
    fn run(&mut self, resources: &[RefCell<Box<dyn Any>>]);

    /// Resolves resource indices from the type-id map. Returns names of any missing resources.
    ///
    /// Called once during [`crate::Scheduler::organize_systems`] before the run loop begins.
    fn prepare(&mut self, _index: &HashMap<TypeId, usize>) -> Vec<String> {
        Vec::new()
    }

    /// Returns the system's human-readable name (typically `std::any::type_name::<F>()`).
    fn name(&self) -> &str {
        "unknown"
    }

    /// The resources this system accesses, as `(resource_index, AccessKind)` pairs,
    /// resolved during [`prepare`](Self::prepare). The scheduler reads this to
    /// mediate host↔device coherence. Defaults to empty (no tracked access).
    fn accesses(&self) -> &[(usize, AccessKind)] {
        &[]
    }

    /// Returns the name of this system's run condition, if any.
    fn condition_name(&self) -> Option<&str> {
        None
    }

    /// Returns group introspection info if this system is a [`SystemGroup`].
    fn group_info(&self) -> Option<&SystemGroupInfo> {
        None
    }
}
// ANCHOR_END: System

/// Introspection info for a [`SystemGroup`], exposing inner system structure and timing.
pub struct SystemGroupInfo {
    /// (system_name, phase_name) for each inner system, populated during `prepare()`.
    pub inner_systems: Vec<(String, String)>,
    /// Per-inner-system cumulative timing (seconds), accumulated across `run()` calls.
    pub inner_timings: Vec<f64>,
    /// Total loop iterations executed across all `run()` calls.
    pub total_iterations: usize,
    /// Loop condition name (if looping).
    pub loop_condition_name: Option<String>,
    /// Max iterations per `run()` call (if looping).
    pub max_iterations: Option<usize>,
}

/// A type-erased wrapper around a function that implements [`System`].
///
/// Created automatically by [`IntoSystem`] — users should not construct this directly.
pub struct FunctionSystem<Input, F> {
    f: F,
    marker: PhantomData<fn() -> Input>,
    /// Per-system-instance local state, keyed by TypeId.
    locals: HashMap<TypeId, Box<dyn Any>>,
    /// Cached resource indices resolved during [`System::prepare`].
    indices: Vec<usize>,
    /// Cached (resource_index, access_kind) for params that touch a resource,
    /// resolved during [`System::prepare`]. Drives scheduler coherence mediation.
    accesses: Vec<(usize, AccessKind)>,
}

/// Converts a function (with up to 10 [`SystemParam`] parameters) into a [`System`].
pub trait IntoSystem<Input> {
    /// The concrete [`System`] type produced by this conversion.
    type System: System;
    /// Performs the conversion.
    fn into_system(self) -> Self::System;
}

impl_system!();
impl_system!(T1);
impl_system!(T1, T2);
impl_system!(T1, T2, T3);
impl_system!(T1, T2, T3, T4);
impl_system!(T1, T2, T3, T4, T5);
impl_system!(T1, T2, T3, T4, T5, T6);
impl_system!(T1, T2, T3, T4, T5, T6, T7);
impl_system!(T1, T2, T3, T4, T5, T6, T7, T8);
impl_system!(T1, T2, T3, T4, T5, T6, T7, T8, T9);
impl_system!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);

impl_into_system!();
impl_into_system!(T1);
impl_into_system!(T1, T2);
impl_into_system!(T1, T2, T3);
impl_into_system!(T1, T2, T3, T4);
impl_into_system!(T1, T2, T3, T4, T5);
impl_into_system!(T1, T2, T3, T4, T5, T6);
impl_into_system!(T1, T2, T3, T4, T5, T6, T7);
impl_into_system!(T1, T2, T3, T4, T5, T6, T7, T8);
impl_into_system!(T1, T2, T3, T4, T5, T6, T7, T8, T9);
impl_into_system!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);

// ─── IntoSystemLabel ─────────────────────────────────────────────────────────

/// Marker for string-based labels (`&str`, `String`).
pub struct StrLabelMarker;
/// Marker for function-handle-based labels.
pub struct FnLabelMarker<I>(PhantomData<I>);

/// Converts a label source (string or function handle) into a `String` label.
///
/// Strings pass through directly. Function handles resolve to their
/// `std::any::type_name`, matching the system's registered name.
pub trait IntoSystemLabel<M> {
    /// Resolves this label source to its `String` label.
    fn into_label(self) -> String;
}

impl IntoSystemLabel<StrLabelMarker> for &str {
    fn into_label(self) -> String {
        self.to_string()
    }
}

impl IntoSystemLabel<StrLabelMarker> for String {
    fn into_label(self) -> String {
        self
    }
}

macro_rules! impl_into_system_label {
    ($($params:ident),*) => {
        impl<F, $($params: SystemParam),*> IntoSystemLabel<FnLabelMarker<($($params,)*)>> for F
        where
            for<'a, 'b> &'a mut F:
                FnMut($($params),*) +
                FnMut($(<$params as SystemParam>::Item<'b>),*)
        {
            fn into_label(self) -> String {
                std::any::type_name::<F>().to_string()
            }
        }
    }
}

impl_into_system_label!();
impl_into_system_label!(T1);
impl_into_system_label!(T1, T2);
impl_into_system_label!(T1, T2, T3);
impl_into_system_label!(T1, T2, T3, T4);
impl_into_system_label!(T1, T2, T3, T4, T5);
impl_into_system_label!(T1, T2, T3, T4, T5, T6);
impl_into_system_label!(T1, T2, T3, T4, T5, T6, T7);
impl_into_system_label!(T1, T2, T3, T4, T5, T6, T7, T8);
impl_into_system_label!(T1, T2, T3, T4, T5, T6, T7, T8, T9);
impl_into_system_label!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);

// ─── Condition trait & FunctionCondition ─────────────────────────────────────

/// A dependency-injected predicate that returns `bool`, used with
/// [`.run_if()`](SystemExt::run_if) to conditionally execute systems.
pub trait Condition {
    /// Evaluates the condition against the current resource state.
    fn evaluate(&mut self, resources: &[RefCell<Box<dyn Any>>]) -> bool;

    /// Resolves resource indices. Returns names of any missing resources.
    fn prepare(&mut self, _index: &HashMap<TypeId, usize>) -> Vec<String> {
        Vec::new()
    }

    /// Returns the condition's human-readable name for diagnostics and DOT output.
    fn name(&self) -> &str {
        ""
    }
}

/// A type-erased wrapper around a boolean function that implements [`Condition`].
///
/// Created automatically by [`IntoCondition`] — users should not construct this directly.
pub struct FunctionCondition<Input, F> {
    f: F,
    marker: PhantomData<fn() -> Input>,
    locals: HashMap<TypeId, Box<dyn Any>>,
    indices: Vec<usize>,
}

/// Converts a boolean function (with up to 5 [`SystemParam`] parameters) into a [`Condition`].
pub trait IntoCondition<Input> {
    /// The concrete [`Condition`] type produced by this conversion.
    type Condition: Condition;
    /// Performs the conversion.
    fn into_condition(self) -> Self::Condition;
}

impl_condition!();
impl_condition!(T1);
impl_condition!(T1, T2);
impl_condition!(T1, T2, T3);
impl_condition!(T1, T2, T3, T4);
impl_condition!(T1, T2, T3, T4, T5);

impl_into_condition!();
impl_into_condition!(T1);
impl_into_condition!(T1, T2);
impl_into_condition!(T1, T2, T3);
impl_into_condition!(T1, T2, T3, T4);
impl_into_condition!(T1, T2, T3, T4, T5);

// ─── ConditionalSystem ────────────────────────────────────────────────────────

/// Wraps a system with a run condition. The system only runs when the condition returns true.
pub struct ConditionalSystem<S: System, C: Condition> {
    system: S,
    condition: C,
}

impl<S: System, C: Condition> System for ConditionalSystem<S, C> {
    fn run(&mut self, resources: &[RefCell<Box<dyn Any>>]) {
        if self.condition.evaluate(resources) {
            self.system.run(resources);
        }
    }
    fn prepare(&mut self, index: &HashMap<TypeId, usize>) -> Vec<String> {
        let mut missing = self.condition.prepare(index);
        missing.extend(self.system.prepare(index));
        missing
    }
    fn name(&self) -> &str {
        self.system.name()
    }
    fn accesses(&self) -> &[(usize, AccessKind)] {
        self.system.accesses()
    }
    fn condition_name(&self) -> Option<&str> {
        let n = self.condition.name();
        if n.is_empty() {
            None
        } else {
            Some(n)
        }
    }
    fn group_info(&self) -> Option<&SystemGroupInfo> {
        self.system.group_info()
    }
}

// ─── SystemDescriptor ─────────────────────────────────────────────────────────

/// Wraps a system with ordering metadata (label, before/after constraints).
///
/// Created via the fluent API on [`SystemExt`] (e.g., `my_system.label("x").after("y")`).
/// Supports chaining: `.label()`, `.before()`, `.after()`, `.requires_label()`, `.run_if()`.
pub struct SystemDescriptor<S: System + 'static> {
    /// The underlying system.
    pub system: S,
    /// Optional human-readable label for this system (used as an ordering target).
    pub label: Option<String>,
    /// Labels of systems that must run *after* this one.
    pub befores: Vec<String>,
    /// Labels of systems that must run *before* this one.
    pub afters: Vec<String>,
    /// Labels that must exist in the same [`ScheduleSet`] (validation-only, no ordering).
    pub requires: Vec<String>,
}

impl<S: System + 'static> SystemDescriptor<S> {
    /// Assigns a label to this system, making it addressable by `.before()` / `.after()`.
    pub fn label(mut self, lbl: impl Into<String>) -> Self {
        self.label = Some(lbl.into());
        self
    }

    /// Declares that this system must run *before* the given target.
    pub fn before<M>(mut self, target: impl IntoSystemLabel<M>) -> Self {
        self.befores.push(target.into_label());
        self
    }

    /// Declares that this system must run *after* the given target.
    pub fn after<M>(mut self, target: impl IntoSystemLabel<M>) -> Self {
        self.afters.push(target.into_label());
        self
    }

    /// Declares that the given label must exist in the same [`ScheduleSet`].
    ///
    /// This is a validation constraint only — it does not impose ordering.
    /// Panics during [`crate::Scheduler::organize_systems`] if the label is missing.
    pub fn requires_label<M>(mut self, target: impl IntoSystemLabel<M>) -> Self {
        self.requires.push(target.into_label());
        self
    }

    /// Attaches a run condition, preserving this descriptor's label / ordering
    /// metadata. The wrapped system only runs on steps where `cond` returns `true`.
    pub fn run_if<I2, C: Condition + 'static>(
        self,
        cond: impl IntoCondition<I2, Condition = C>,
    ) -> SystemDescriptor<ConditionalSystem<S, C>> {
        SystemDescriptor {
            system: ConditionalSystem {
                system: self.system,
                condition: cond.into_condition(),
            },
            label: self.label,
            befores: self.befores,
            afters: self.afters,
            requires: self.requires,
        }
    }
}

// ─── SystemExt — fluent API on IntoSystem ────────────────────────────────────

/// Extension trait giving any `IntoSystem` implementor the `.run_if()`, `.label()`,
/// `.before()`, and `.after()` fluent configuration methods.
pub trait SystemExt<I>: IntoSystem<I> + Sized
where
    Self::System: 'static,
{
    /// Runs the system only when `cond` evaluates to `true` for the step,
    /// wrapping it in a [`ConditionalSystem`].
    fn run_if<I2, C: Condition + 'static>(
        self,
        cond: impl IntoCondition<I2, Condition = C>,
    ) -> ConditionalSystem<Self::System, C> {
        ConditionalSystem {
            system: self.into_system(),
            condition: cond.into_condition(),
        }
    }

    /// Attaches an explicit ordering label so other systems can target this
    /// one with `.before()` / `.after()` / `.requires_label()`.
    fn label(self, lbl: impl Into<String>) -> SystemDescriptor<Self::System> {
        SystemDescriptor {
            system: self.into_system(),
            label: Some(lbl.into()),
            befores: vec![],
            afters: vec![],
            requires: vec![],
        }
    }

    /// Orders this system to run before `target` (a label or system handle).
    fn before<M>(self, target: impl IntoSystemLabel<M>) -> SystemDescriptor<Self::System> {
        SystemDescriptor {
            system: self.into_system(),
            label: None,
            befores: vec![target.into_label()],
            afters: vec![],
            requires: vec![],
        }
    }

    /// Orders this system to run after `target` (a label or system handle).
    fn after<M>(self, target: impl IntoSystemLabel<M>) -> SystemDescriptor<Self::System> {
        SystemDescriptor {
            system: self.into_system(),
            label: None,
            befores: vec![],
            afters: vec![target.into_label()],
            requires: vec![],
        }
    }

    /// Declares a hard dependency on `target`: this system is only scheduled
    /// if `target` is also present, and always runs after it.
    fn requires_label<M>(self, target: impl IntoSystemLabel<M>) -> SystemDescriptor<Self::System> {
        SystemDescriptor {
            system: self.into_system(),
            label: None,
            befores: vec![],
            afters: vec![],
            requires: vec![target.into_label()],
        }
    }
}

impl<I, F: IntoSystem<I>> SystemExt<I> for F where F::System: 'static {}

// ─── SystemGroup ─────────────────────────────────────────────────────────────

/// A composite system containing inner systems with their own phase ordering,
/// optional looping, and nesting support.
///
/// Inner systems are sorted by phase index and topologically sorted within each
/// phase group (reusing the same `topo_sort_group` logic as the main scheduler).
///
/// # Example
///
/// ```rust,ignore
/// app.add_update_system(
///     SystemGroup::new("coupling_loop")
///         .add_system(compute_coupling, CouplingPhase::Compute)
///         .add_system(fluid_solve, CouplingPhase::Solve)
///         .add_system(check_convergence, CouplingPhase::Check)
///         .loop_while(coupling_not_converged, 10),
///     ScheduleSet::Force,
/// );
/// ```
pub struct SystemGroup {
    name: String,
    inner_systems: Vec<(StoredSystemEntry, StoredPhase)>,
    loop_condition: Option<Box<dyn Condition>>,
    info: SystemGroupInfo,
}

impl SystemGroup {
    /// Creates a new empty system group with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        SystemGroup {
            name: name.into(),
            inner_systems: Vec::new(),
            loop_condition: None,
            info: SystemGroupInfo {
                inner_systems: Vec::new(),
                inner_timings: Vec::new(),
                total_iterations: 0,
                loop_condition_name: None,
                max_iterations: None,
            },
        }
    }

    /// Adds an inner system at the given phase.
    pub fn add_system<M>(
        mut self,
        system: impl IntoScheduledSystem<M>,
        phase: impl ScheduleSet,
    ) -> Self {
        self.inner_systems
            .push((system.into_stored(), StoredPhase::from_typed(phase)));
        self
    }

    /// Adds a nested `SystemGroup` at the given phase.
    pub fn add_group(mut self, group: SystemGroup, phase: impl ScheduleSet) -> Self {
        let name = group.name.clone();
        self.inner_systems.push((
            StoredSystemEntry {
                system: Box::new(group),
                name,
                label: None,
                befores: vec![],
                afters: vec![],
                requires: vec![],
                condition_name: None,
            },
            StoredPhase::from_typed(phase),
        ));
        self
    }

    /// Sets a loop condition: inner systems repeat while the condition returns true,
    /// up to `max` iterations per outer `run()` call.
    pub fn loop_while<I, C: Condition + 'static>(
        mut self,
        condition: impl IntoCondition<I, Condition = C>,
        max: usize,
    ) -> Self {
        let cond = condition.into_condition();
        self.info.loop_condition_name = Some(cond.name().to_string());
        self.info.max_iterations = Some(max);
        self.loop_condition = Some(Box::new(cond));
        self
    }
}

impl System for SystemGroup {
    fn prepare(&mut self, index: &HashMap<TypeId, usize>) -> Vec<String> {
        // Sort by (namespace, index)
        self.inner_systems
            .sort_by_key(|(_, phase)| phase.sort_key());

        // Topo-sort within each phase group (same sort_key = same group)
        let all = std::mem::take(&mut self.inner_systems);
        let mut groups: Vec<Vec<(StoredSystemEntry, StoredPhase)>> = Vec::new();
        for entry in all {
            let key = entry.1.sort_key();
            if let Some(last) = groups.last_mut() {
                if last[0].1.sort_key() == key {
                    last.push(entry);
                    continue;
                }
            }
            groups.push(vec![entry]);
        }
        for group in &mut groups {
            topo_sort_group(group);
        }
        for group in groups {
            self.inner_systems.extend(group);
        }

        // Populate info
        self.info.inner_systems = self
            .inner_systems
            .iter()
            .map(|(entry, phase)| (entry.name.clone(), phase.phase_name().to_string()))
            .collect();
        self.info.inner_timings = vec![0.0; self.inner_systems.len()];

        // Prepare all inner systems
        let mut missing = Vec::new();
        for (entry, _) in &mut self.inner_systems {
            missing.extend(entry.system.prepare(index));
        }

        // Prepare loop condition
        if let Some(cond) = &mut self.loop_condition {
            missing.extend(cond.prepare(index));
        }

        missing
    }

    fn run(&mut self, resources: &[RefCell<Box<dyn Any>>]) {
        let max = self.info.max_iterations.unwrap_or(1);
        let mut iterations = 0;

        loop {
            for (idx, (entry, _)) in self.inner_systems.iter_mut().enumerate() {
                let t0 = std::time::Instant::now();
                entry.system.run(resources);
                self.info.inner_timings[idx] += t0.elapsed().as_secs_f64();
            }
            iterations += 1;

            if let Some(cond) = &mut self.loop_condition {
                if !cond.evaluate(resources) || iterations >= max {
                    break;
                }
            } else {
                break; // no loop condition = single pass
            }
        }

        self.info.total_iterations += iterations;
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn group_info(&self) -> Option<&SystemGroupInfo> {
        Some(&self.info)
    }
}

/// `SystemGroup` converts to itself, enabling [`SystemExt`] (`.run_if()`, `.label()`, etc.).
impl IntoSystem<()> for SystemGroup {
    type System = SystemGroup;
    fn into_system(self) -> Self::System {
        self
    }
}

// ─── IntoScheduledSystem — accepts fn / ConditionalSystem / SystemDescriptor ──

/// Disambiguation marker for the plain-function [`IntoScheduledSystem`] impl.
pub struct FnMarker<I>(PhantomData<I>);
/// Disambiguation marker for the [`ConditionalSystem`] [`IntoScheduledSystem`] impl.
pub struct CondMarker;
/// Disambiguation marker for the [`SystemDescriptor`] [`IntoScheduledSystem`] impl.
pub struct DescMarker;

/// Converts into a `StoredSystemEntry` (boxed system + optional ordering metadata).
pub trait IntoScheduledSystem<M> {
    /// Boxes the system together with any ordering metadata into a
    /// `StoredSystemEntry` for registration.
    fn into_stored(self) -> StoredSystemEntry;
}

/// Plain function / closure with no ordering metadata.
impl<I, F: IntoSystem<I>> IntoScheduledSystem<FnMarker<I>> for F
where
    F::System: 'static,
{
    fn into_stored(self) -> StoredSystemEntry {
        let sys = self.into_system();
        let name = sys.name().to_string();
        let condition_name = sys.condition_name().map(|s| s.to_string());
        StoredSystemEntry {
            system: Box::new(sys),
            name,
            label: None,
            befores: vec![],
            afters: vec![],
            requires: vec![],
            condition_name,
        }
    }
}

/// ConditionalSystem — no ordering metadata.
impl<S: System + 'static, C: Condition + 'static> IntoScheduledSystem<CondMarker>
    for ConditionalSystem<S, C>
{
    fn into_stored(self) -> StoredSystemEntry {
        let name = self.name().to_string();
        let condition_name = self.condition_name().map(|s| s.to_string());
        StoredSystemEntry {
            system: Box::new(self),
            name,
            label: None,
            befores: vec![],
            afters: vec![],
            requires: vec![],
            condition_name,
        }
    }
}

/// SystemDescriptor — carries label / before / after metadata.
impl<S: System + 'static> IntoScheduledSystem<DescMarker> for SystemDescriptor<S> {
    fn into_stored(self) -> StoredSystemEntry {
        let name = self.system.name().to_string();
        let condition_name = self.system.condition_name().map(|s| s.to_string());
        StoredSystemEntry {
            system: Box::new(self.system),
            name,
            label: self.label,
            befores: self.befores,
            afters: self.afters,
            requires: self.requires,
            condition_name,
        }
    }
}

// ─── StoredSystemEntry ────────────────────────────────────────────────────────

/// A boxed system together with its ordering metadata, ready for storage in the scheduler.
///
/// Created by [`IntoScheduledSystem::into_stored`] during system registration.
pub struct StoredSystemEntry {
    /// The type-erased, boxed system.
    pub system: Box<dyn System>,
    /// The system's name (from `std::any::type_name`).
    pub name: String,
    /// Optional explicit label for ordering references.
    pub label: Option<String>,
    /// Systems that must run after this one (by label or function name).
    pub befores: Vec<String>,
    /// Systems that must run before this one (by label or function name).
    pub afters: Vec<String>,
    /// Labels that must exist in the same [`ScheduleSet`] (validation only).
    pub requires: Vec<String>,
    /// Name of the attached run condition, if any (for diagnostics / DOT output).
    pub condition_name: Option<String>,
}

// ─── Schedule phases ──────────────────────────────────────────────────────────

/// Trait for user-definable schedule phases.
///
/// Any enum implementing this trait can be used as a schedule phase for
/// [`crate::Scheduler::add_update_system`] or [`crate::Scheduler::add_setup_system`].
/// The built-in `ScheduleSet` (this crate) and `ScheduleSetupSet`
/// (in `grass_app`) implement this trait.
///
/// Use `#[derive(ScheduleSet)]` from `grass_derive` to auto-implement this.
/// Variants are assigned indices automatically in declaration order.
pub trait ScheduleSet: Copy + Clone + std::fmt::Debug + 'static {
    /// Returns the numeric ordering index for this phase.
    fn to_index(&self) -> u32;
    /// Returns the human-readable name of this phase (used in DOT output and tracing).
    fn name(&self) -> &'static str;
}

/// A type-erased schedule phase, storing the index, name, namespace, and originating type.
///
/// Use this when you need to store a phase value without knowing the concrete enum type,
/// e.g. in plugins that accept any schedule phase. The `namespace` field controls
/// cross-solver ordering: systems are sorted by `(namespace, index)`.
#[derive(Clone, Copy, Debug)]
pub struct StoredPhase {
    /// Which `ScheduleSet` enum this came from (for `set_schedule_namespace`).
    pub(crate) schedule_type_id: TypeId,
    /// Namespace for cross-solver ordering (default 0).
    pub(crate) namespace: u32,
    /// Numeric ordering index within the namespace.
    pub(crate) index: u32,
    /// Human-readable name (for DOT output and tracing).
    name: &'static str,
}

impl StoredPhase {
    /// Captures the index, name, and type identity from any [`ScheduleSet`] implementor.
    pub fn from(phase: impl ScheduleSet) -> Self {
        Self {
            schedule_type_id: TypeId::of::<Self>(),
            namespace: 0,
            index: phase.to_index(),
            name: phase.name(),
        }
    }

    /// Creates a `StoredPhase` that remembers the concrete phase enum type.
    pub fn from_typed<P: ScheduleSet>(phase: P) -> Self {
        Self {
            schedule_type_id: TypeId::of::<P>(),
            namespace: 0,
            index: phase.to_index(),
            name: phase.name(),
        }
    }

    /// Returns the `(namespace, index)` sort key used for execution ordering.
    pub fn sort_key(&self) -> (u32, u32) {
        (self.namespace, self.index)
    }

    /// Returns the human-readable phase name.
    pub fn phase_name(&self) -> &'static str {
        self.name
    }
}

impl ScheduleSet for StoredPhase {
    fn to_index(&self) -> u32 {
        self.index
    }
    fn name(&self) -> &'static str {
        self.name
    }
}

/// Sets namespace ordering on a scheduler so that phase enums execute in the listed order.
///
/// Each phase enum type is assigned an incrementing namespace (0, 1, 2, …),
/// which controls execution order across different solver phase enums.
///
/// ```rust,ignore
/// chain_namespaces!(app, CouplingPrePhase, FluidPhase, MaterialPhase, CouplingPostPhase);
/// // equivalent to:
/// // app.set_schedule_namespace::<CouplingPrePhase>(0);
/// // app.set_schedule_namespace::<FluidPhase>(1);
/// // app.set_schedule_namespace::<MaterialPhase>(2);
/// // app.set_schedule_namespace::<CouplingPostPhase>(3);
/// ```
#[macro_export]
macro_rules! chain_namespaces {
    ($scheduler:expr, $($phase:ty),+ $(,)?) => {{
        let mut _ns: u32 = 0;
        $(
            $scheduler.set_schedule_namespace::<$phase>(_ns);
            _ns += 1;
        )+
    }};
}

// ─── Topological sort within a ScheduleSet group ─────────────────────────────

/// Topologically sorts systems within a single [`ScheduleSet`] group using Kahn's algorithm.
///
/// # Panics
///
/// Panics with a diagnostic naming the involved systems, labels, and phase if
/// a cycle is detected in the `before`/`after` ordering constraints.
pub(crate) fn topo_sort_group(group: &mut Vec<(StoredSystemEntry, StoredPhase)>) {
    let n = group.len();
    if n <= 1 {
        return;
    }

    let mut label_to_idx: HashMap<String, usize> = HashMap::new();
    for (i, (entry, _)) in group.iter().enumerate() {
        // Index by system name (enables function-handle-based ordering)
        label_to_idx.insert(entry.name.clone(), i);
        // Explicit labels override if present
        if let Some(lbl) = &entry.label {
            label_to_idx.insert(lbl.clone(), i);
        }
    }

    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    let mut in_degree: Vec<usize> = vec![0; n];

    for (i, (entry, _)) in group.iter().enumerate() {
        for b in &entry.befores {
            if let Some(&j) = label_to_idx.get(b) {
                adj[i].push(j);
                in_degree[j] += 1;
            }
        }
        for a in &entry.afters {
            if let Some(&j) = label_to_idx.get(a) {
                adj[j].push(i);
                in_degree[i] += 1;
            }
        }
    }

    let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);

    while let Some(node) = queue.pop_front() {
        order.push(node);
        for &nbr in &adj[node] {
            in_degree[nbr] -= 1;
            if in_degree[nbr] == 0 {
                queue.push_back(nbr);
            }
        }
    }

    if order.len() != n {
        panic!("{}", format_cycle_diagnostic(group, &in_degree));
    }

    let mut temp: Vec<Option<(StoredSystemEntry, StoredPhase)>> =
        group.drain(..).map(Some).collect();
    for idx in order {
        group.push(
            temp[idx]
                .take()
                .expect("topo_sort_group: duplicate index in topological order"),
        );
    }
}

fn format_cycle_diagnostic(
    group: &[(StoredSystemEntry, StoredPhase)],
    in_degree: &[usize],
) -> String {
    let phase = group
        .first()
        .map(|(_, phase)| phase.phase_name())
        .unwrap_or("<unknown>");

    let mut lines = vec![format!(
        "Cycle detected in system ordering constraints within ScheduleSet phase \"{phase}\""
    )];

    for (idx, (entry, _)) in group.iter().enumerate() {
        if in_degree.get(idx).copied().unwrap_or_default() == 0 {
            continue;
        }

        let label = entry.label.as_deref().unwrap_or("<none>");
        let before = if entry.befores.is_empty() {
            "<none>".to_string()
        } else {
            entry.befores.join(", ")
        };
        let after = if entry.afters.is_empty() {
            "<none>".to_string()
        } else {
            entry.afters.join(", ")
        };

        lines.push(format!(
            "  System \"{}\" (label: \"{}\", phase: \"{}\") before [{}] after [{}]",
            entry.name, label, phase, before, after
        ));
    }

    lines.join("\n")
}
