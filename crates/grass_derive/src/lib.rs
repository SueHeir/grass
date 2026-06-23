//! Proc-macros for the grass simulation framework.
//!
//! Provides **three** derives:
//!
//! - **`#[derive(ScheduleSet)]`** — implements `grass_scheduler::ScheduleSet`.
//!   Works on an **enum** (each variant gets a sequential index in declaration
//!   order) *or* on a **unit struct** (a single phase with `to_index() = 0`).
//!   Used to define solver phases (Setup → ComputeFluxes → Integrate → …).
//!
//! - **`#[derive(StageEnum)]`** — implements `grass_scheduler::StageName` for
//!   an **enum** where every variant carries a `#[stage("name")]` attribute.
//!   Used to bind multi-stage `[[run]]` workflows in TOML to a Rust enum.
//!
//! - **`#[derive(Namespace)]`** — implements `grass_multi::Namespace` for a
//!   **unit struct**, using the struct's identifier as the namespace string
//!   (`struct Cfd;` → `Namespace::NAME == "Cfd"`). Used to tag sub-Apps in a
//!   `grass_multi` coupling.
//!
//! So only `StageEnum` is enum-only; `ScheduleSet` also accepts unit structs,
//! and `Namespace` is unit-struct-only.
//!
//! # Required companion derives & dependencies
//!
//! The generated code references trait paths in `grass_scheduler::*` and
//! `grass_multi::*` **literally** (not re-exported), so the corresponding
//! crate must be in your dependency graph: `grass_scheduler` for `ScheduleSet`
//! / `StageEnum`, `grass_multi` for `Namespace`.
//!
//! `#[derive(ScheduleSet)]` does **not** add the trait's supertrait bounds for
//! you. `ScheduleSet: Copy + Clone + Debug + 'static`, so the target type must
//! *also* derive `Copy`, `Clone`, and `Debug` itself — e.g.
//! `#[derive(Clone, Copy, Debug, ScheduleSet)]`. (`StageEnum` similarly needs
//! whatever `Clone`/`PartialEq`/`Default` your `[[run]]` driver expects.)
//!
//! # Two invariants worth memorizing
//!
//! 1. **Enum declaration order = schedule index.** `ScheduleSet`'s `to_index()`
//!    is the variant's positional index, so reordering variants silently
//!    reorders the schedule. Treat the variant order as load-bearing.
//! 2. **`#[stage("...")]` strings are the `[[run]]` TOML contract.** Each
//!    `StageEnum` stage name must exactly match a stage `name` in the
//!    `[[run]]` config that drives it; renaming one without the other breaks
//!    the binding.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields};

// ─── #[derive(StageEnum)] ─────────────────────────────────────────────────────

/// Implements `grass_scheduler::StageName` for an enum whose variants carry
/// `#[stage("name")]` attributes.
///
/// ```rust,ignore
/// #[derive(Clone, PartialEq, Default, StageEnum)]
/// enum Phase {
///     #[default]
///     #[stage("settle")]
///     Settle,
///     #[stage("compress")]
///     Compress,
/// }
/// ```
///
/// # Panics
///
/// Produces a compile-time error if:
/// - Applied to a struct or union (must be an enum)
/// - Any variant is missing the `#[stage("...")]` attribute
/// - Two variants share the same stage name
#[proc_macro_derive(StageEnum, attributes(stage))]
pub fn derive_stage_enum(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let variants = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => {
            return syn::Error::new_spanned(
                &input,
                "StageEnum can only be derived for enums, not structs or unions",
            )
            .to_compile_error()
            .into();
        }
    };

    let mut match_arms = Vec::new();
    let mut stage_names = Vec::new();

    for variant in variants {
        let ident = &variant.ident;

        let stage_attr = variant.attrs.iter().find(|a| a.path().is_ident("stage"));
        let Some(attr) = stage_attr else {
            return syn::Error::new_spanned(
                variant,
                format!(
                    "StageEnum: variant `{ident}` is missing a #[stage(\"name\")] attribute. \
                     Every variant must specify its TOML stage name, e.g.:\n\n    \
                     #[stage(\"my_stage\")]\n    {ident},"
                ),
            )
            .to_compile_error()
            .into();
        };

        let stage_name: syn::LitStr = match attr.parse_args() {
            Ok(lit) => lit,
            Err(_) => {
                return syn::Error::new_spanned(
                    attr,
                    "StageEnum: #[stage(...)] expects a string literal, \
                     e.g. #[stage(\"settle\")]",
                )
                .to_compile_error()
                .into();
            }
        };

        let name_str = stage_name.value();
        stage_names.push(name_str.clone());
        match_arms.push(quote! { #name::#ident => #name_str, });
    }

    for (i, a) in stage_names.iter().enumerate() {
        for b in &stage_names[i + 1..] {
            if a == b {
                return syn::Error::new_spanned(
                    &input,
                    format!(
                        "StageEnum: duplicate stage name \"{a}\". \
                         Each variant must have a unique stage name."
                    ),
                )
                .to_compile_error()
                .into();
            }
        }
    }

    let num_stages = stage_names.len();
    let variant_idents: Vec<_> = variants.iter().map(|v| &v.ident).collect();
    let indices: Vec<_> = (0..variant_idents.len()).collect();

    let expanded = quote! {
        impl grass_scheduler::StageName for #name {
            fn stage_name(&self) -> &'static str {
                match self {
                    #(#match_arms)*
                }
            }

            fn stage_names() -> &'static [&'static str] {
                &[#(#stage_names),*]
            }

            fn num_stages() -> usize {
                #num_stages
            }

            fn from_index(i: usize) -> Option<Self> {
                match i {
                    #(#indices => Some(#name::#variant_idents),)*
                    _ => None,
                }
            }
        }
    };

    expanded.into()
}

// ─── #[derive(ScheduleSet)] ───────────────────────────────────────────────────

/// Implements `grass_scheduler::ScheduleSet` for an enum, assigning each variant
/// an index in declaration order (0, 1, 2, …).
///
/// ```rust,ignore
/// #[derive(Clone, Copy, Debug, PartialEq, ScheduleSet)]
/// enum CfdSchedule {
///     Setup,           // index 0
///     AssembleFluxes,  // index 1
///     SolvePressure,   // index 2
/// }
/// ```
///
/// Also accepts **unit structs** (`struct Foo;`) — those become a one-variant
/// `ScheduleSet` whose `to_index() = 0` and `name() = "Foo"`. Useful as
/// type-level markers for distinct schedule positions when you'd otherwise
/// write `enum Foo { Run }`.
///
/// # Compile error
///
/// Tuple structs (`struct Foo(...)`) and named structs (`struct Foo { ... }`)
/// are rejected — they have no canonical "the variant" to pick.
#[proc_macro_derive(ScheduleSet)]
pub fn derive_schedule_set(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    match &input.data {
        Data::Enum(data) => derive_for_enum(name, &data.variants),
        Data::Struct(data) => match &data.fields {
            Fields::Unit => derive_for_unit_struct(name),
            _ => syn::Error::new_spanned(
                &input,
                "ScheduleSet can only be derived for enums or unit structs (`struct Foo;`)",
            )
            .to_compile_error()
            .into(),
        },
        Data::Union(_) => {
            syn::Error::new_spanned(&input, "ScheduleSet cannot be derived for unions")
                .to_compile_error()
                .into()
        }
    }
}

fn derive_for_enum(
    name: &syn::Ident,
    variants: &syn::punctuated::Punctuated<syn::Variant, syn::token::Comma>,
) -> TokenStream {
    let mut match_index_arms = Vec::new();
    let mut match_name_arms = Vec::new();

    for (index, variant) in variants.iter().enumerate() {
        let ident = &variant.ident;
        let index_val = index as u32;
        let variant_name = ident.to_string();
        match_index_arms.push(quote! { #name::#ident => #index_val, });
        match_name_arms.push(quote! { #name::#ident => #variant_name, });
    }

    let expanded = quote! {
        impl grass_scheduler::ScheduleSet for #name {
            fn to_index(&self) -> u32 {
                match self {
                    #(#match_index_arms)*
                }
            }

            fn name(&self) -> &'static str {
                match self {
                    #(#match_name_arms)*
                }
            }
        }
    };
    expanded.into()
}

fn derive_for_unit_struct(name: &syn::Ident) -> TokenStream {
    let stem = name.to_string();
    let expanded = quote! {
        impl grass_scheduler::ScheduleSet for #name {
            fn to_index(&self) -> u32 { 0 }
            fn name(&self) -> &'static str { #stem }
        }
    };
    expanded.into()
}

// ─── #[derive(Namespace)] ─────────────────────────────────────────────────────

/// Implements `grass_multi::Namespace` for a unit struct, using the
/// struct's identifier as the namespace string.
///
/// ```rust,ignore
/// #[derive(Namespace)]
/// pub struct Cfd;   // -> Namespace::NAME = "Cfd"
/// ```
///
/// If you want a different namespace string than the struct name,
/// implement `grass_multi::Namespace` by hand or use the
/// `namespace!` macro:
///
/// ```rust,ignore
/// namespace!(pub Cfd = "cfd");
/// ```
///
/// # Compile error
///
/// Rejected for everything except unit structs (no canonical namespace
/// for tuple/named struct fields or enum variants).
#[proc_macro_derive(Namespace)]
pub fn derive_namespace(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Unit => {
                let stem = name.to_string();
                let expanded = quote! {
                    impl grass_multi::Namespace for #name {
                        const NAME: &'static str = #stem;
                    }
                };
                expanded.into()
            }
            _ => syn::Error::new_spanned(
                &input,
                "Namespace can only be derived for unit structs (`struct Foo;`)",
            )
            .to_compile_error()
            .into(),
        },
        _ => syn::Error::new_spanned(&input, "Namespace can only be derived for unit structs")
            .to_compile_error()
            .into(),
    }
}
