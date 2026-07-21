use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::quote;
use tokyo_ir::id::TypeId;
use tokyo_ir::types::{
    EnumType, ObjectType, PrimitiveType, TypeDeclaration, TypeRef, TypeShape, UnionType,
};

use crate::naming::{
    rust_field_identifier, rust_identifier, rust_variant_identifier, type_identifiers_by_type_id,
};

/// Per-target rendering knobs. Every target-specific derive/attribute a Rust
/// type renderer needs goes here instead of being hardcoded, so one type
/// mapping can serve both a CLI (needs `clap::ValueEnum`) and a library SDK
/// (doesn't) without diverging.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderOptions {
    /// Derive `clap::ValueEnum` (and emit `#[value(name = ...)]`) on generated
    /// enums so they're directly usable as CLI flag/positional values.
    pub derive_clap_value_enum: bool,
}

/// Every declared type's Rust identifier and (for the request-body flattening
/// heuristic) its full declaration, keyed by [`TypeId`] and built once. Every
/// renderer consults this instead of re-deriving names or re-scanning `types`.
pub struct TypeCatalog<'a> {
    idents: HashMap<TypeId, syn::Ident>,
    declarations: HashMap<TypeId, &'a TypeDeclaration>,
}

impl<'a> TypeCatalog<'a> {
    pub fn new(types: &'a [TypeDeclaration]) -> Self {
        Self {
            idents: type_identifiers_by_type_id(types),
            declarations: types.iter().map(|decl| (decl.id.clone(), decl)).collect(),
        }
    }

    pub fn rust_identifier(&self, id: &TypeId) -> &syn::Ident {
        self.idents
            .get(id)
            .unwrap_or_else(|| panic!("{id:?} should have a registered rust_identifier"))
    }

    pub fn declaration(&self, id: &TypeId) -> &'a TypeDeclaration {
        self.declarations
            .get(id)
            .unwrap_or_else(|| panic!("{id:?} should be declared"))
    }
}

pub fn render_type_declaration(
    decl: &TypeDeclaration,
    catalog: &TypeCatalog,
    opts: RenderOptions,
) -> TokenStream {
    let name = catalog.rust_identifier(&decl.id);
    match &decl.shape {
        TypeShape::Alias { target } => {
            let target = render_type_ref(target, catalog);
            quote! { pub type #name = #target; }
        }
        TypeShape::Object(object) => render_object_type_declaration(name, object, catalog),
        TypeShape::Enum(enumeration) => render_enum_type_declaration(name, enumeration, opts),
        TypeShape::Union(union) => {
            render_discriminated_union_type_declaration(name, union, catalog)
        }
        TypeShape::UndiscriminatedUnion { variants } => {
            render_untagged_union(name, variants, catalog)
        }
    }
}

fn render_object_type_declaration(
    name: &syn::Ident,
    object: &ObjectType,
    catalog: &TypeCatalog,
) -> TokenStream {
    let fields = object.fields.iter().map(|field| {
        let field_name = rust_field_identifier(&field.field_name);
        let wire_name = &field.wire_name;
        let ty = render_type_ref(&field.r#type, catalog);
        let rename = field_needs_serde_rename_attribute(&field_name, wire_name).then(|| {
            quote! { #[serde(rename = #wire_name)] }
        });
        quote! {
            #rename
            pub #field_name: #ty,
        }
    });
    // `extends` bases have their fields merged in via `#[serde(flatten)]`, named
    // after the base type's own (snake_case) name so two bases can't collide.
    let bases = object.extends.iter().map(|base_id| {
        let base_ty = catalog.rust_identifier(base_id);
        let field_name = rust_field_identifier(&base_ty.to_string());
        quote! {
            #[serde(flatten)]
            pub #field_name: #base_ty,
        }
    });
    quote! {
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct #name {
            #(#bases)*
            #(#fields)*
        }
    }
}

fn render_enum_type_declaration(
    name: &syn::Ident,
    enumeration: &EnumType,
    opts: RenderOptions,
) -> TokenStream {
    // Two source values can normalize to the same wire value (e.g. a merged
    // `allOf`/`oneOf` re-declaring the same member) — an IR-level dedup, not
    // something an emitter should error on. First occurrence wins.
    let mut seen = std::collections::HashSet::new();
    let values: Vec<_> = enumeration
        .values
        .iter()
        .filter(|value| seen.insert(value.wire_value.clone()))
        .collect();

    let variants = values.iter().map(|value| {
        let variant = rust_variant_identifier(&value.name);
        let wire = &value.wire_value;
        let value_attr = opts
            .derive_clap_value_enum
            .then(|| quote! { #[value(name = #wire)] });
        quote! {
            #[serde(rename = #wire)]
            #value_attr
            #variant,
        }
    });
    // An open enum still round-trips known values through `#[serde(rename)]`
    // above; this only adds a fallback so decoding an unrecognized wire value
    // fails softly instead of erroring. Encoding an `Unknown` value back out is
    // deliberately not supported because there is no original wire value to use.
    let catch_all = enumeration.forward_compatible.then(|| {
        let value_attr = opts
            .derive_clap_value_enum
            .then(|| quote! { #[value(name = "unknown")] });
        quote! {
            #[serde(other)]
            #value_attr
            Unknown,
        }
    });
    let display_arms = values.iter().map(|value| {
        let variant = rust_variant_identifier(&value.name);
        let wire = &value.wire_value;
        quote! { #name::#variant => #wire, }
    });
    let unknown_arm = enumeration
        .forward_compatible
        .then(|| quote! { #name::Unknown => "unknown", });
    let clap_derive = opts
        .derive_clap_value_enum
        .then(|| quote! { , clap::ValueEnum });
    quote! {
        // `Display` makes this directly usable in query-string building
        // (`.to_string()`) for enum-typed path/query/header parameters.
        // Targets that also embed this in a CLI additionally derive
        // `clap::ValueEnum` (see `RenderOptions::derive_clap_value_enum`),
        // matching each variant's CLI value to its wire value via
        // `#[value(name = ...)]`, mirroring `#[serde(rename = ...)]`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize #clap_derive)]
        pub enum #name {
            #(#variants)*
            #catch_all
        }

        impl std::fmt::Display for #name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(match self {
                    #(#display_arms)*
                    #unknown_arm
                })
            }
        }
    }
}

// Rendered as `#[serde(untagged)]` rather than `#[serde(tag = "...")]`: the
// discriminant field the IR already keeps on each variant's own object type
// would otherwise be serialized twice (once by the tag wrapper, once by the
// variant's own field of the same wire name). Untagged loses fast
// discriminant-based dispatch but avoids that duplicate-key output; revisit
// with a per-variant "omit the discriminant field" wrapper type if a real
// endpoint ever needs tag-based rather than structural variant selection.
fn render_discriminated_union_type_declaration(
    name: &syn::Ident,
    union: &UnionType,
    catalog: &TypeCatalog,
) -> TokenStream {
    let variants = union.variants.iter().map(|variant| {
        let variant_name = rust_variant_identifier(&variant.variant_name);
        let ty = render_type_ref(&variant.r#type, catalog);
        quote! { #variant_name(#ty), }
    });
    quote! {
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(untagged)]
        pub enum #name {
            #(#variants)*
        }
    }
}

fn render_untagged_union(
    name: &syn::Ident,
    variants: &[TypeRef],
    catalog: &TypeCatalog,
) -> TokenStream {
    let variants = variants.iter().enumerate().map(|(index, ty)| {
        let variant_name = rust_identifier(&format!("Variant{index}"));
        let rendered = render_type_ref(ty, catalog);
        quote! { #variant_name(#rendered), }
    });
    quote! {
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(untagged)]
        pub enum #name {
            #(#variants)*
        }
    }
}

/// The single place mapping every IR `TypeRef` variant onto a Rust type.
pub fn render_type_ref(ty: &TypeRef, catalog: &TypeCatalog) -> TokenStream {
    match ty {
        TypeRef::Primitive(primitive) => render_primitive(*primitive),
        TypeRef::Named(id) => {
            let name = catalog.rust_identifier(id);
            quote! { crate::tokyo::types::#name }
        }
        TypeRef::List(inner) => {
            let inner = render_type_ref(inner, catalog);
            quote! { Vec<#inner> }
        }
        TypeRef::Tuple { items, rest } => {
            // A trailing `rest` has no direct Rust tuple-struct analogue; degrade
            // the whole tuple to a homogeneous `Vec` of the rest type in that case
            // rather than losing the fixed items silently.
            if let Some(rest) = rest {
                let rest_ty = render_type_ref(rest, catalog);
                quote! { Vec<#rest_ty> }
            } else {
                let items = items.iter().map(|item| render_type_ref(item, catalog));
                quote! { (#(#items),*) }
            }
        }
        TypeRef::Map { key: _, value } => {
            // OpenAPI map keys are always strings; `key` carries no extra
            // information Rust's `BTreeMap<String, _>` doesn't already assume.
            let value = render_type_ref(value, catalog);
            quote! { std::collections::BTreeMap<String, #value> }
        }
        TypeRef::Nullable(inner) => {
            let inner = render_type_ref(inner, catalog);
            quote! { crate::tokyo::types::Nullable<#inner> }
        }
        TypeRef::Optional(inner) => {
            let inner = render_type_ref(inner, catalog);
            quote! { Option<#inner> }
        }
        TypeRef::Intersection(left, _right) => {
            // Rust has no structural intersection type; the first constraint is
            // rendered and the second is documented as unenforced by the type
            // system. The importer merges compatible `allOf` fields upstream.
            render_type_ref(left, catalog)
        }
    }
}

fn render_primitive(primitive: PrimitiveType) -> TokenStream {
    match primitive {
        PrimitiveType::String => quote! { String },
        PrimitiveType::Integer => quote! { i32 },
        PrimitiveType::Int64 => quote! { i64 },
        PrimitiveType::Float => quote! { f32 },
        PrimitiveType::Double => quote! { f64 },
        PrimitiveType::Boolean => quote! { bool },
        PrimitiveType::Uuid => quote! { uuid::Uuid },
        PrimitiveType::DateTime => quote! { chrono::DateTime<chrono::Utc> },
        PrimitiveType::Date => quote! { chrono::NaiveDate },
        PrimitiveType::Bytes | PrimitiveType::Binary => quote! { String },
        PrimitiveType::Any => quote! { serde_json::Value },
    }
}

fn field_needs_serde_rename_attribute(rust_field_identifier: &syn::Ident, wire_name: &str) -> bool {
    rust_field_identifier.to_string().trim_start_matches("r#") != wire_name
}

/// Whether any declared type transitively references `TypeRef::Nullable`, i.e.
/// whether the generated `Nullable<T>` helper type is actually needed.
pub fn generated_types_use_nullable_wrapper(types: &[TypeDeclaration]) -> bool {
    fn type_ref_uses_nullable_wrapper(ty: &TypeRef) -> bool {
        match ty {
            TypeRef::Nullable(_) => true,
            TypeRef::List(inner) | TypeRef::Optional(inner) => {
                type_ref_uses_nullable_wrapper(inner)
            }
            TypeRef::Map { value, .. } => type_ref_uses_nullable_wrapper(value),
            TypeRef::Tuple { items, rest } => {
                items.iter().any(type_ref_uses_nullable_wrapper)
                    || rest.as_deref().is_some_and(type_ref_uses_nullable_wrapper)
            }
            TypeRef::Intersection(left, right) => {
                type_ref_uses_nullable_wrapper(left) || type_ref_uses_nullable_wrapper(right)
            }
            TypeRef::Primitive(_) | TypeRef::Named(_) => false,
        }
    }
    types.iter().any(|decl| match &decl.shape {
        TypeShape::Alias { target } => type_ref_uses_nullable_wrapper(target),
        TypeShape::Object(object) => object
            .fields
            .iter()
            .any(|field| type_ref_uses_nullable_wrapper(&field.r#type)),
        TypeShape::Union(union) => union
            .variants
            .iter()
            .any(|variant| type_ref_uses_nullable_wrapper(&variant.r#type)),
        TypeShape::UndiscriminatedUnion { variants } => {
            variants.iter().any(type_ref_uses_nullable_wrapper)
        }
        TypeShape::Enum(_) => false,
    })
}

pub fn render_nullable_helper() -> TokenStream {
    quote! {
        /// A field that may be present-and-null, distinct from `Option`'s
        /// absent/present. Composes with `Option<Nullable<T>>` for fields that can
        /// be absent, null, or a value (JSON's actual three-state shape).
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(untagged)]
        pub enum Nullable<T> {
            Null,
            Value(T),
        }

        impl<T> From<Option<T>> for Nullable<T> {
            fn from(value: Option<T>) -> Self {
                match value {
                    Some(value) => Nullable::Value(value),
                    None => Nullable::Null,
                }
            }
        }

        impl<T> From<Nullable<T>> for Option<T> {
            fn from(value: Nullable<T>) -> Self {
                match value {
                    Nullable::Value(value) => Some(value),
                    Nullable::Null => None,
                }
            }
        }
    }
}
