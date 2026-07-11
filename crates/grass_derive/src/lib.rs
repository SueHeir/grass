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
//! / `StageEnum`, `grass_multi` for `Namespace`. `ConfigDescription` needs
//! only `grass_io`; its implementation paths are re-exported there so config
//! consumers do not need hidden `grass_app` or `toml` dependencies.
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

#![warn(missing_docs)]

use heck::{
    ToKebabCase, ToLowerCamelCase, ToShoutyKebabCase, ToShoutySnakeCase, ToSnakeCase,
    ToUpperCamelCase,
};
use proc_macro::TokenStream;
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{parse_macro_input, Data, DeriveInput, Fields};

// ─── #[derive(ConfigDescription)] ───────────────────────────────────────────

/// Generates `grass_io::DescribedConfig` directly from a Serde config struct,
/// or `grass_io::ConfigChoices` from an enum's declared variants.
///
/// The derive reads the same field names, `#[serde(rename = ...)]`,
/// `#[serde(rename_all = ...)]`, `#[serde(default)]`, and doc comments that
/// define the TOML parser contract. Defaults are serialized from `Default` at
/// runtime, so a changed default or added field is reflected in generated TOML
/// without a second handwritten list. Function-valued Serde defaults are
/// invoked directly, rather than guessed from `Default`.
/// The described type must derive both `Deserialize` and `Serialize` (the
/// latter lets the generated reference render TOML defaults).
#[proc_macro_derive(ConfigDescription, attributes(config_description, serde))]
pub fn derive_config_description(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    if let Data::Enum(data) = &input.data {
        let rename_all = match serde_rename_all(&input.attrs) {
            Ok(rename_all) => rename_all,
            Err(error) => return error.to_compile_error().into(),
        };
        let choices = data
            .variants
            .iter()
            .map(|variant| {
                if !matches!(variant.fields, Fields::Unit) {
                    return syn::Error::new_spanned(
                        variant,
                        "ConfigDescription enum choices must be unit variants",
                    )
                    .to_compile_error();
                }
                let mut value = match rename_field(&variant.ident, rename_all.as_deref(), variant.span()) {
                    Ok(value) => value,
                    Err(error) => return error.to_compile_error(),
                };
                for attr in &variant.attrs {
                    if attr.path().is_ident("serde") {
                        let result = attr.parse_nested_meta(|meta| {
                            if meta.path.is_ident("rename") {
                                if meta.input.peek(syn::token::Paren) {
                                    return Err(meta.error("ConfigDescription does not support directional serde rename; use one parser key"));
                                }
                                value = meta.value()?.parse::<syn::LitStr>()?.value();
                            }
                            Ok(())
                        });
                        if let Err(error) = result {
                            return error.to_compile_error();
                        }
                    }
                }
                quote!(#value.to_string())
            })
            .collect::<Vec<_>>();
        return quote! {
            impl ::grass_io::ConfigChoices for #name {
                fn choices() -> ::std::vec::Vec<::std::string::String> {
                    ::std::vec![#(#choices),*]
                }
            }
        }
        .into();
    }
    let Data::Struct(data) = &input.data else {
        return syn::Error::new_spanned(
            input,
            "ConfigDescription can only be derived for structs or enums",
        )
        .to_compile_error()
        .into();
    };
    let Fields::Named(fields) = &data.fields else {
        return syn::Error::new_spanned(&input, "ConfigDescription requires named fields")
            .to_compile_error()
            .into();
    };
    let mut section = None;
    let mut narrative = String::new();
    let mut array_table = false;
    let mut rename_all = None;
    for attr in &input.attrs {
        if attr.path().is_ident("serde") {
            let result = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename_all") {
                    if meta.input.peek(syn::token::Paren) {
                        return Err(meta.error(
                            "ConfigDescription does not support directional serde rename_all; use one parser key",
                        ));
                    }
                    rename_all = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                }
                Ok(())
            });
            if let Err(error) = result {
                return error.to_compile_error().into();
            }
            continue;
        }
        if !attr.path().is_ident("config_description") {
            continue;
        }
        let result = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("section") {
                section = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("narrative") {
                narrative = meta.value()?.parse::<syn::LitStr>()?.value();
            } else if meta.path.is_ident("array_table") {
                array_table = true;
            } else {
                return Err(
                    meta.error("expected section = \"...\", narrative = \"...\", or array_table")
                );
            }
            Ok(())
        });
        if let Err(error) = result {
            return error.to_compile_error().into();
        }
    }
    let Some(section) = section else {
        return syn::Error::new_spanned(&input, "missing #[config_description(section = \"...\")]")
            .to_compile_error()
            .into();
    };
    let mut generated_fields = Vec::new();
    let mut default_overrides = Vec::new();
    let mut optional_field_names = Vec::new();
    for field in &fields.named {
        let ident = field.ident.as_ref().unwrap();
        let field_ty = &field.ty;
        let mut field_name = match rename_field(ident, rename_all.as_deref(), field.span()) {
            Ok(name) => name,
            Err(error) => return error.to_compile_error().into(),
        };
        let mut has_default = false;
        let mut serde_default = None;
        let mut flattened = false;
        let mut skipped_on_deserialize = false;
        for attr in &field.attrs {
            if !attr.path().is_ident("serde") {
                continue;
            }
            let result = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default") {
                    has_default = true;
                    if meta.input.peek(syn::Token![=]) {
                        let path = meta.value()?.parse::<syn::LitStr>()?.value();
                        serde_default = Some(match syn::parse_str::<syn::ExprPath>(&path) {
                            Ok(path) => path,
                            Err(_) => {
                                return Err(meta.error("serde default must name a function path"))
                            }
                        });
                    }
                }
                if meta.path.is_ident("flatten") {
                    flattened = true;
                }
                // These attributes remove a member from Serde's input
                // contract.  Do not document it as a TOML key: with
                // `deny_unknown_fields` such a key would be rejected, and
                // without it the key would silently have no effect.
                if meta.path.is_ident("skip") || meta.path.is_ident("skip_deserializing") {
                    skipped_on_deserialize = true;
                }
                if meta.path.is_ident("rename") {
                    if meta.input.peek(syn::token::Paren) {
                        return Err(meta.error(
                            "ConfigDescription does not support directional serde rename; use one parser key",
                        ));
                    }
                    field_name = meta.value()?.parse::<syn::LitStr>()?.value();
                } else if !meta.path.is_ident("default") && !meta.input.is_empty() {
                    // Consume other Serde value attributes (for example
                    // skip_serializing_if), which do not change parser keys
                    // or default values represented by this description.
                    let _ = meta.value()?.parse::<syn::Expr>()?;
                }
                Ok(())
            });
            if let Err(error) = result {
                return error.to_compile_error().into();
            }
        }
        // A flattened map accepts downstream-specific keys rather than
        // declaring one TOML field of its own, so it has no generated sample.
        if flattened || skipped_on_deserialize {
            continue;
        }
        let is_option = match &field.ty {
            syn::Type::Path(path) => path
                .path
                .segments
                .last()
                .is_some_and(|s| s.ident == "Option"),
            _ => false,
        };
        let required = !(has_default || is_option);
        if !required {
            // Serde optionality, rather than Rust's `Default`, decides which
            // fields may be emitted as usable TOML values.
            optional_field_names.push(field_name.clone());
        }
        let ty = config_type_name(&field.ty);
        let description = field
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("doc"))
            .filter_map(|a| match &a.meta {
                syn::Meta::NameValue(value) => match &value.value {
                    syn::Expr::Lit(lit) => match &lit.lit {
                        syn::Lit::Str(text) => Some(text.value().trim().to_owned()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        let line = field.span().start().line;
        let source = quote_spanned! {field.span()=> concat!(file!(), ":", #line, ":", stringify!(#name), ".", stringify!(#ident)).to_string() };
        if let Some(function) = serde_default {
            default_overrides.push(quote! {
                let parser_default = ::grass_io::__private::toml::Value::try_from(#function())
                        .expect("serde default must serialize to TOML")
                        .to_string();
                defaults.insert(#field_name.to_string(), parser_default.clone());
                examples.insert(#field_name.to_string(), parser_default);
            });
        }
        generated_fields.push(quote! {
            ::grass_io::__private::grass_app::ConfigFieldDescription {
                name: #field_name.to_string(), ty: #ty.to_string(),
                default: defaults.get(#field_name).cloned(),
                example: examples.get(#field_name).cloned(), required: #required,
                choices: <#field_ty as ::grass_io::ConfigChoices>::choices(), description: #description.to_string(), source: #source,
            }
        });
    }
    quote! {
        impl ::grass_io::DescribedConfig for #name {
            fn description() -> ::grass_io::__private::grass_app::ConfigDescription {
                let mut defaults: ::std::collections::BTreeMap<::std::string::String, ::std::string::String> = ::grass_io::__private::toml::to_string(&Self::default())
                    .expect("default config must serialize to TOML")
                    .parse::<::grass_io::__private::toml::Table>().expect("serialized default must be a TOML table")
                    .into_iter()
                    // A `Default` impl can construct a value for a field that
                    // Serde nevertheless requires callers to supply. Keep
                    // only fields whose Serde definition explicitly permits
                    // omission.
                    .filter(|(key, _)| [#(#optional_field_names),*].contains(&key.as_str()))
                    .map(|(key, value)| (key, value.to_string())).collect();
                let mut examples: ::std::collections::BTreeMap<::std::string::String, ::std::string::String> = ::grass_io::__private::toml::to_string(&Self::default())
                    .expect("default config must serialize to TOML")
                    .parse::<::grass_io::__private::toml::Table>().expect("serialized default must be a TOML table")
                    .into_iter().map(|(key, value)| (key, value.to_string())).collect();
                #(#default_overrides)*
                ::grass_io::__private::grass_app::ConfigDescription {
                    section: #section.to_string(), array_table: #array_table,
                    narrative: #narrative.to_string(), fields: ::std::vec![#(#generated_fields),*],
                }
            }
        }
        impl ::grass_io::ConfigChoices for #name {
            fn choices() -> ::std::vec::Vec<::std::string::String> { ::std::vec![] }
        }
    }.into()
}

fn serde_rename_all(attrs: &[syn::Attribute]) -> Result<Option<String>, syn::Error> {
    let mut rename_all = None;
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                if meta.input.peek(syn::token::Paren) {
                    return Err(meta.error(
                        "ConfigDescription does not support directional serde rename_all; use one parser key",
                    ));
                }
                rename_all = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            }
            Ok(())
        })?;
    }
    Ok(rename_all)
}

fn rename_field(
    ident: &syn::Ident,
    rule: Option<&str>,
    span: proc_macro2::Span,
) -> Result<String, syn::Error> {
    let name = ident.to_string();
    let Some(rule) = rule else {
        return Ok(name);
    };
    let renamed = match rule {
        "lowercase" => name.to_lowercase(),
        "UPPERCASE" => name.to_uppercase(),
        "PascalCase" => name.to_upper_camel_case(),
        "camelCase" => name.to_lower_camel_case(),
        "snake_case" => name.to_snake_case(),
        "SCREAMING_SNAKE_CASE" => name.to_shouty_snake_case(),
        "kebab-case" => name.to_kebab_case(),
        "SCREAMING-KEBAB-CASE" => name.to_shouty_kebab_case(),
        other => {
            return Err(syn::Error::new(
                span,
                format!("unsupported serde(rename_all = {other:?}) for ConfigDescription"),
            ));
        }
    };
    Ok(renamed)
}

fn config_type_name(ty: &syn::Type) -> String {
    let syn::Type::Path(path) = ty else {
        return quote!(#ty).to_string();
    };
    let Some(segment) = path.path.segments.last() else {
        return quote!(#ty).to_string();
    };
    match segment.ident.to_string().as_str() {
        "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize" => {
            "integer".to_string()
        }
        "f32" | "f64" => "float".to_string(),
        "bool" => "boolean".to_string(),
        "String" => "string".to_string(),
        "Vec" => "array".to_string(),
        "Option" => "optional".to_string(),
        _ => segment.ident.to_string(),
    }
}

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
