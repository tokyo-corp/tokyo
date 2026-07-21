use serde::{Deserialize, Serialize};

use crate::id::TypeId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Named type declaration in the IR graph.
pub struct TypeDeclaration {
    /// Stable identifier used by references.
    pub id: TypeId,
    /// Rust-facing type name after OpenAPI name normalization.
    pub name: String,
    /// Optional human-facing documentation.
    pub docs: Option<String>,
    /// Shape of the declared type.
    pub shape: TypeShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
/// Top-level shape of a declared type.
pub enum TypeShape {
    /// Named alias to another type expression.
    Alias {
        /// Aliased type expression.
        target: TypeRef,
    },
    /// Object with fields and optional composition.
    Object(ObjectType),
    /// String enum.
    Enum(EnumType),
    /// Discriminated union.
    Union(UnionType),
    /// Union without a discriminator.
    UndiscriminatedUnion {
        /// Candidate type expressions.
        variants: Vec<TypeRef>,
    },
}

/// A reference to a type usable inline anywhere a field/param/response needs one.
/// Optionality, lists, and maps are structural (composable) rather than flags,
/// so e.g. `Optional<List<Named>>` is expressible without a combinatorial explosion
/// of declared types.
// Deliberately not internally-tagged (no `#[serde(tag = "kind")]`): this enum is
// self-recursive via `Box<TypeRef>`, and combining internal tagging with recursion
// overflows the current rustc/serde trait solver (E0275 on `Serializer` resolution).
// Default external tagging (`{"List": {...}}`) avoids it and is fine for our use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeRef {
    /// Primitive scalar or untyped JSON value.
    Primitive(PrimitiveType),
    /// Reference to a declared type.
    Named(TypeId),
    /// Homogeneous JSON array.
    List(Box<TypeRef>),
    /// A positional JSON array. `Optional` members are permitted only as a
    /// trailing suffix; `rest` describes additional elements when present.
    Tuple {
        /// Fixed positional item types.
        items: Vec<TypeRef>,
        /// Repeated trailing item type.
        rest: Option<Box<TypeRef>>,
    },
    /// JSON object whose values share one type.
    Map {
        /// Map key type.
        key: Box<TypeRef>,
        /// Map value type.
        value: Box<TypeRef>,
    },
    /// Value may be JSON null.
    Nullable(Box<TypeRef>),
    /// Value may be absent.
    Optional(Box<TypeRef>),
    /// Both constraints must hold, as when repeated `allOf` properties differ.
    Intersection(Box<TypeRef>, Box<TypeRef>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Primitive scalar type used by generated clients.
pub enum PrimitiveType {
    /// UTF-8 string.
    String,
    /// JSON integer.
    Integer,
    /// 64-bit integer.
    Int64,
    /// 32-bit floating point value.
    Float,
    /// 64-bit floating point value.
    Double,
    /// Boolean value.
    Boolean,
    /// UUID string.
    Uuid,
    /// RFC 3339 date-time string.
    DateTime,
    /// Calendar date string.
    Date,
    /// A base64-encoded string (`format: byte` in OpenAPI).
    Bytes,
    /// Raw binary data (`format: binary` in OpenAPI).
    Binary,
    /// Arbitrary JSON value.
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Object type with fields, inherited bases, and optional extra properties.
pub struct ObjectType {
    /// Base object type IDs merged into this object.
    pub extends: Vec<TypeId>,
    /// Declared object properties.
    pub fields: Vec<FieldDeclaration>,
    /// Whether arbitrary keys are accepted. Retained for snapshot compatibility;
    /// `extra_properties_type` carries the value type when OpenAPI declares one.
    pub extra_properties: bool,
    /// Type of additional properties when declared.
    #[serde(default)]
    pub extra_properties_type: Option<Box<TypeRef>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One object field or property.
pub struct FieldDeclaration {
    /// Wire-format property name.
    pub wire_name: String,
    /// Rust-safe field name.
    pub field_name: String,
    /// Field type.
    pub r#type: TypeRef,
    /// Optional human-facing documentation.
    pub docs: Option<String>,
    /// An OpenAPI-declared example value, if any. Additive metadata: emitters
    /// may use it (e.g. as a generated CLI flag's default) or ignore it.
    #[serde(default)]
    pub example: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Enum type and compatibility policy.
pub struct EnumType {
    /// Declared enum values.
    pub values: Vec<EnumValue>,
    /// Inferred from OpenAPI's `oneOf: [enum, string]` escape hatch. When true,
    /// generated code must tolerate unknown wire values instead of failing to parse.
    pub forward_compatible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One string enum value.
pub struct EnumValue {
    /// Exact serialized value.
    pub wire_value: String,
    /// Rust-safe variant name.
    pub name: String,
    /// Optional human-facing documentation.
    pub docs: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Discriminated union type.
pub struct UnionType {
    /// Wire-format discriminator property name.
    pub discriminant_wire_name: String,
    /// Possible variants.
    pub variants: Vec<UnionVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One discriminated union variant.
pub struct UnionVariant {
    /// Serialized discriminator value.
    pub discriminant_value: String,
    /// Rust-safe variant name.
    pub variant_name: String,
    /// Payload type selected by this variant.
    pub r#type: TypeRef,
}
