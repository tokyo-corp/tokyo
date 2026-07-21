use std::collections::{HashMap, HashSet};

use oas3::Spec;
use oas3::spec::{ObjectOrReference, ObjectSchema, Schema, SchemaType, SchemaTypeSet};
use tokyo_ir::id::TypeId;
use tokyo_ir::types::{
    EnumType, EnumValue, FieldDeclaration, ObjectType, PrimitiveType, TypeDeclaration, TypeRef,
    TypeShape, UnionType, UnionVariant,
};

use crate::error::ImportError;
use crate::naming;

pub struct Context<'a> {
    pub spec: &'a Spec,
    operation_security: HashSet<(String, String)>,
    oauth_token_endpoints: HashMap<String, String>,
    declared: Vec<TypeDeclaration>,
    allocated: HashMap<TypeId, String>,
    declared_ids: HashSet<TypeId>,
    component_type_ids: HashMap<String, TypeId>,
}

impl<'a> Context<'a> {
    pub fn new(
        spec: &'a Spec,
        operation_security: HashSet<(String, String)>,
        oauth_token_endpoints: HashMap<String, String>,
    ) -> Self {
        Self {
            spec,
            operation_security,
            oauth_token_endpoints,
            declared: Vec::new(),
            allocated: HashMap::new(),
            declared_ids: HashSet::new(),
            component_type_ids: HashMap::new(),
        }
    }

    pub fn operation_has_security(&self, path: &str, method: &str) -> bool {
        self.operation_security
            .contains(&(path.to_string(), method.to_string()))
    }

    pub fn oauth_token_endpoints(&self) -> &HashMap<String, String> {
        &self.oauth_token_endpoints
    }

    pub fn into_declarations(self) -> Vec<TypeDeclaration> {
        self.declared
    }

    /// Reserves a generated type identifier for a named component schema.
    /// Distinct raw names that normalize to the same identifier (Stripe has
    /// `billing.alert.triggered` and `billing.alert_triggered`) are
    /// disambiguated with a numeric suffix in document order, which is stable
    /// for a given input document.
    fn reserve_component(&mut self, raw_name: &str) -> Result<TypeId, ImportError> {
        let context = format!("component schema `{raw_name}`");
        let base = naming::openapi_type_name(raw_name);
        if !naming::string_is_valid_identifier(&base) {
            return Err(ImportError::Unsupported(format!(
                "{context} normalizes to invalid generated type identifier `{base}`"
            )));
        }
        let mut candidate = base.clone();
        let mut suffix = 2;
        while self.allocated.contains_key(&TypeId(candidate.clone())) {
            candidate = format!("{base}{suffix}");
            suffix += 1;
        }
        let type_id = TypeId(candidate);
        self.allocated.insert(type_id.clone(), context);
        self.component_type_ids
            .insert(raw_name.to_string(), type_id.clone());
        Ok(type_id)
    }

    /// Resolves a `$ref`/`allOf` component name to its reserved `TypeId`.
    /// Unreserved names (a ref to a component that never declared) fall back to
    /// plain normalization, preserving the previous dangling-ref behavior.
    fn component_type_id(&self, raw_name: &str) -> TypeId {
        self.component_type_ids
            .get(raw_name)
            .cloned()
            .unwrap_or_else(|| TypeId(naming::openapi_type_name(raw_name)))
    }

    fn allocate_synthetic(&mut self, name_hint: &str) -> TypeId {
        let base = naming::openapi_type_name(name_hint);
        let mut candidate = base.clone();
        let mut suffix = 2;
        while self.allocated.contains_key(&TypeId(candidate.clone())) {
            candidate = format!("{base}{suffix}");
            suffix += 1;
        }
        let type_id = TypeId(candidate);
        self.allocated.insert(
            type_id.clone(),
            format!("inline schema `{name_hint}` normalized from generated context"),
        );
        type_id
    }

    fn declare_reserved(&mut self, decl: TypeDeclaration) -> Result<(), ImportError> {
        if !naming::string_is_valid_identifier(&decl.name) {
            return Err(ImportError::Unsupported(format!(
                "allocated type `{}` has invalid generated type identifier `{}`",
                decl.id.0, decl.name
            )));
        }
        if !self.allocated.contains_key(&decl.id) {
            return Err(ImportError::Unsupported(format!(
                "type `{}` was declared without reserving its emitted identifier",
                decl.id.0
            )));
        }
        if !self.declared_ids.insert(decl.id.clone()) {
            return Err(ImportError::Unsupported(format!(
                "type `{}` was declared more than once",
                decl.id.0
            )));
        }
        self.declared.push(decl);
        Ok(())
    }
}

/// Declares every named schema under `components.schemas` up front, so `$ref`
/// targets always resolve to an already-known `TypeId` regardless of visit order.
pub fn declare_openapi_component_schemas_as_ir_types(ctx: &mut Context) -> Result<(), ImportError> {
    let Some(components) = &ctx.spec.components else {
        return Ok(());
    };

    // Reserve the complete component namespace first. Inline schemas encountered
    // while converting an earlier component must not take a later component's ID.
    for name in components.schemas.keys() {
        ctx.reserve_component(name)?;
    }

    for (name, schema) in components.schemas.iter() {
        let type_id = ctx.component_type_id(name);
        let resolved = resolve_schema_without_ref_cycles(ctx.spec, schema)?;
        let object_schema = match &resolved {
            Schema::Object(obj_or_ref) => match obj_or_ref.as_ref() {
                ObjectOrReference::Object(schema) => schema.clone(),
                ObjectOrReference::Ref { .. } => {
                    unreachable!("Schema::resolve never returns a Ref")
                }
            },
            Schema::Boolean(_) => continue,
        };
        let decl = convert_object_schema_to_type_declaration(ctx, type_id, name, &object_schema)?;
        ctx.declare_reserved(decl)?;
    }

    Ok(())
}

/// Resolves a schema's `$ref` chain to a concrete object/boolean schema while
/// rejecting reference cycles. `oas3`'s own `Schema::resolve` follows `$ref`
/// links recursively with no visited-set, so a cyclic component reference
/// (`A` → `B` → `A`, or a self-reference) recurses until the process aborts on
/// a stack overflow. A cycle can't be represented as a generated Rust type
/// regardless, so return a clean error instead of crashing. Only schema
/// references are supported, mirroring `convert_openapi_schema_to_ir_type_ref`.
fn resolve_schema_without_ref_cycles(spec: &Spec, schema: &Schema) -> Result<Schema, ImportError> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut current = schema.clone();
    loop {
        let Schema::Object(obj_or_ref) = &current else {
            return Ok(current);
        };
        let ref_path = match obj_or_ref.as_ref() {
            ObjectOrReference::Object(_) => return Ok(current),
            ObjectOrReference::Ref { ref_path, .. } => ref_path.clone(),
        };
        if !visited.insert(ref_path.clone()) {
            return Err(ImportError::Unsupported(format!(
                "cyclic schema reference `{ref_path}`: $ref cycles cannot be represented as generated types; break the cycle before importing"
            )));
        }
        let name = ref_path.strip_prefix("#/components/schemas/").ok_or_else(|| {
            ImportError::Unsupported(format!(
                "external or unsupported schema reference `{ref_path}`; bundle external references before importing"
            ))
        })?;
        current = spec
            .components
            .as_ref()
            .and_then(|components| components.schemas.get(name))
            .cloned()
            .ok_or_else(|| {
                ImportError::Unsupported(format!("unresolvable schema reference `{ref_path}`"))
            })?;
    }
}

/// Converts any inline or referenced schema into a `TypeRef` usable wherever a
/// field/param/response needs one. Named (`$ref`) schemas resolve to `TypeRef::Named`
/// without re-declaring anything (they were already declared up front). Inline
/// object/enum/union schemas are synthesized as new named declarations using
/// `name_hint`, since our IR only allows nominal types for those shapes.
pub fn convert_openapi_schema_to_ir_type_ref(
    ctx: &mut Context,
    schema: &Schema,
    name_hint: &str,
) -> Result<TypeRef, ImportError> {
    match schema {
        Schema::Boolean(_) => Ok(TypeRef::Primitive(PrimitiveType::Any)),
        Schema::Object(obj_or_ref) => match obj_or_ref.as_ref() {
            ObjectOrReference::Ref { ref_path, .. } => {
                if !ref_path.starts_with("#/components/schemas/") {
                    return Err(ImportError::Unsupported(format!(
                        "external schema reference `{ref_path}` is not supported; bundle external references before importing"
                    )));
                }
                let name = ref_path
                    .rsplit('/')
                    .next()
                    .ok_or_else(|| ImportError::Unsupported(format!("bad $ref: {ref_path}")))?;
                Ok(TypeRef::Named(ctx.component_type_id(name)))
            }
            ObjectOrReference::Object(object_schema) => {
                inline_object_convert_openapi_schema_to_ir_type_ref(ctx, object_schema, name_hint)
            }
        },
    }
}

fn inline_object_convert_openapi_schema_to_ir_type_ref(
    ctx: &mut Context,
    schema: &ObjectSchema,
    name_hint: &str,
) -> Result<TypeRef, ImportError> {
    if schema.discriminator.is_none()
        && let Some(member) = single_non_null_schema_union_member(schema)
    {
        let member = convert_openapi_schema_to_ir_type_ref(ctx, member, name_hint)?;
        return Ok(match member {
            TypeRef::Nullable(_) => member,
            other => TypeRef::Nullable(Box::new(other)),
        });
    }

    if let Some(primitive) = try_convert_schema_to_primitive_type(schema) {
        return Ok(wrap_nullable_schema_type_ref(
            schema,
            TypeRef::Primitive(primitive),
        ));
    }

    if schema.schema_type == Some(SchemaTypeSet::Single(SchemaType::Array)) {
        if !schema.prefix_items.is_empty() {
            return tuple_convert_openapi_schema_to_ir_type_ref(ctx, schema, name_hint);
        }
        let item_schema = schema
            .items
            .as_deref()
            .ok_or_else(|| ImportError::Unsupported("array schema missing items".into()))?;
        let item_name = naming::synthetic_openapi_type_name(name_hint, "Item");
        let item = convert_openapi_schema_to_ir_type_ref(ctx, item_schema, &item_name)?;
        return Ok(wrap_nullable_schema_type_ref(
            schema,
            TypeRef::List(Box::new(item)),
        ));
    }

    if schema.properties.is_empty()
        && let Some(additional) = &schema.additional_properties
        && !matches!(additional, Schema::Boolean(value) if !value.0)
    {
        let value = match additional {
            Schema::Boolean(value) if value.0 => TypeRef::Primitive(PrimitiveType::Any),
            Schema::Boolean(_) => unreachable!("false boolean schemas are excluded"),
            schema => {
                let value_name = naming::synthetic_openapi_type_name(name_hint, "Value");
                convert_openapi_schema_to_ir_type_ref(ctx, schema, &value_name)?
            }
        };
        return Ok(wrap_nullable_schema_type_ref(
            schema,
            TypeRef::Map {
                key: Box::new(TypeRef::Primitive(PrimitiveType::String)),
                value: Box::new(value),
            },
        ));
    }

    // Object-shaped, enum, or union: these need a name of their own.
    let type_id = ctx.allocate_synthetic(name_hint);
    let decl = convert_object_schema_to_type_declaration(ctx, type_id.clone(), name_hint, schema)?;
    ctx.declare_reserved(decl)?;
    Ok(wrap_nullable_schema_type_ref(
        schema,
        TypeRef::Named(type_id),
    ))
}

fn single_non_null_schema_union_member(schema: &ObjectSchema) -> Option<&Schema> {
    let members = if !schema.one_of.is_empty() {
        &schema.one_of
    } else {
        &schema.any_of
    };
    if members.len() != 2
        || members
            .iter()
            .filter(|member| schema_is_explicit_null_type(member))
            .count()
            != 1
    {
        return None;
    }
    members
        .iter()
        .find(|member| !schema_is_explicit_null_type(member))
}

fn schema_is_explicit_null_type(schema: &Schema) -> bool {
    let Schema::Object(schema) = schema else {
        return false;
    };
    let ObjectOrReference::Object(schema) = schema.as_ref() else {
        return false;
    };
    match &schema.schema_type {
        Some(SchemaTypeSet::Single(SchemaType::Null)) => true,
        Some(SchemaTypeSet::Multiple(types)) => types
            .iter()
            .all(|schema_type| *schema_type == SchemaType::Null),
        _ => false,
    }
}

fn tuple_convert_openapi_schema_to_ir_type_ref(
    ctx: &mut Context,
    schema: &ObjectSchema,
    name_hint: &str,
) -> Result<TypeRef, ImportError> {
    let prefix_len = schema.prefix_items.len();
    if schema
        .max_items
        .is_some_and(|maximum| maximum < prefix_len as u64)
    {
        return Err(ImportError::Unsupported(format!(
            "tuple schema `{name_hint}` has maxItems smaller than prefixItems"
        )));
    }
    if schema
        .min_items
        .is_some_and(|minimum| minimum > prefix_len as u64)
    {
        return Err(ImportError::Unsupported(format!(
            "tuple schema `{name_hint}` requires additional items beyond prefixItems"
        )));
    }

    let minimum = schema.min_items.unwrap_or(0) as usize;
    let mut items = Vec::with_capacity(prefix_len);
    for (index, item_schema) in schema.prefix_items.iter().enumerate() {
        if matches!(item_schema, Schema::Boolean(value) if !value.0) {
            return Err(ImportError::Unsupported(format!(
                "tuple schema `{name_hint}` has an impossible false schema at prefixItems index {index}"
            )));
        }
        let item_name = naming::synthetic_openapi_type_name(name_hint, &format!("Item{index}"));
        let mut item = convert_openapi_schema_to_ir_type_ref(ctx, item_schema, &item_name)?;
        if index >= minimum {
            item = TypeRef::Optional(Box::new(item));
        }
        items.push(item);
    }

    let fixed_by_maximum = schema.max_items == Some(prefix_len as u64);
    if schema.max_items.is_some() && !fixed_by_maximum {
        return Err(ImportError::Unsupported(format!(
            "tuple schema `{name_hint}` has finitely bounded additional items, which the IR cannot represent exactly"
        )));
    }
    let rest = match schema.items.as_deref() {
        Some(Schema::Boolean(value)) if !value.0 => None,
        _ if fixed_by_maximum => None,
        Some(Schema::Boolean(_)) | None => Some(Box::new(TypeRef::Primitive(PrimitiveType::Any))),
        Some(item_schema) => {
            let rest_name = naming::synthetic_openapi_type_name(name_hint, "RestItem");
            Some(Box::new(convert_openapi_schema_to_ir_type_ref(
                ctx,
                item_schema,
                &rest_name,
            )?))
        }
    };

    Ok(wrap_nullable_schema_type_ref(
        schema,
        TypeRef::Tuple { items, rest },
    ))
}

fn convert_object_schema_to_type_declaration(
    ctx: &mut Context,
    type_id: TypeId,
    name: &str,
    schema: &ObjectSchema,
) -> Result<TypeDeclaration, ImportError> {
    let shape = if !schema.prefix_items.is_empty()
        && schema.schema_type == Some(SchemaTypeSet::Single(SchemaType::Array))
    {
        TypeShape::Alias {
            target: tuple_convert_openapi_schema_to_ir_type_ref(ctx, schema, name)?,
        }
    } else if !schema.enum_values.is_empty() {
        convert_object_schema_to_enum_shape(schema)
    } else if let Some(enumeration) = extract_forward_compatible_enum_shape(schema) {
        TypeShape::Enum(enumeration)
    } else if !schema.one_of.is_empty() && schema.discriminator.is_some() {
        convert_object_schema_to_discriminated_union_shape(ctx, name, schema)?
    } else if !schema.one_of.is_empty() || !schema.any_of.is_empty() {
        // `oneOf` without a discriminator can't be narrowed as strictly (TS can't
        // enforce "exactly one matched" the way a discriminant can), but it's still
        // representable as a plain union — better than refusing to import it.
        let members = if !schema.one_of.is_empty() {
            &schema.one_of
        } else {
            &schema.any_of
        };
        convert_object_schema_to_undiscriminated_union_shape(ctx, name, members)?
    } else {
        convert_object_schema_to_object_shape(ctx, name, schema)?
    };

    Ok(TypeDeclaration {
        name: type_id.0.clone(),
        id: type_id,
        docs: combine_summary_and_description_docs(
            schema.title.as_deref(),
            schema.description.as_deref(),
        ),
        shape,
    })
}

fn extract_forward_compatible_enum_shape(schema: &ObjectSchema) -> Option<EnumType> {
    if schema.one_of.len() != 2 || schema.discriminator.is_some() {
        return None;
    }
    let mut enum_schema = None;
    let mut has_open_string = false;
    for member in &schema.one_of {
        let Schema::Object(member) = member else {
            return None;
        };
        let ObjectOrReference::Object(member) = member.as_ref() else {
            return None;
        };
        if !member.enum_values.is_empty() {
            enum_schema = Some(member);
        } else if member.schema_type == Some(SchemaTypeSet::Single(SchemaType::String))
            && member.one_of.is_empty()
            && member.any_of.is_empty()
        {
            has_open_string = true;
        }
    }
    let mut enumeration = match convert_object_schema_to_enum_shape(enum_schema?) {
        TypeShape::Enum(enumeration) => enumeration,
        _ => unreachable!("object_schema_to_enum always returns an enum"),
    };
    if !has_open_string {
        return None;
    }
    enumeration.forward_compatible = true;
    Some(enumeration)
}

fn convert_object_schema_to_enum_shape(schema: &ObjectSchema) -> TypeShape {
    let values = schema
        .enum_values
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .map(|wire_value| EnumValue {
            name: naming::openapi_type_name(&wire_value),
            wire_value,
            docs: None,
        })
        .collect();

    TypeShape::Enum(EnumType {
        values,
        forward_compatible: false,
    })
}

fn convert_object_schema_to_discriminated_union_shape(
    ctx: &mut Context,
    name: &str,
    schema: &ObjectSchema,
) -> Result<TypeShape, ImportError> {
    // Caller (`object_schema_to_declaration`) only reaches here when a discriminator
    // is present.
    let discriminator = schema
        .discriminator
        .as_ref()
        .expect("discriminator checked by caller");

    let mut variants = Vec::with_capacity(schema.one_of.len());
    for (i, variant_schema) in schema.one_of.iter().enumerate() {
        let variant_name_hint = naming::synthetic_openapi_type_name(name, &format!("Variant{i}"));
        let type_ref =
            convert_openapi_schema_to_ir_type_ref(ctx, variant_schema, &variant_name_hint)?;

        // The wire discriminant value is *not* necessarily the schema/type name: OpenAPI lets
        // `discriminator.mapping` remap it explicitly (e.g. wire value "person" -> #/.../Person).
        // Only fall back to the implicit "wire value == schema name" convention when no mapping
        // entry points at this variant's $ref.
        let ref_path = match variant_schema {
            Schema::Object(obj_or_ref) => match obj_or_ref.as_ref() {
                ObjectOrReference::Ref { ref_path, .. } => Some(ref_path.as_str()),
                ObjectOrReference::Object(_) => None,
            },
            Schema::Boolean(_) => None,
        };
        let discriminant_value = discriminator
            .mapping
            .as_ref()
            .zip(ref_path)
            .and_then(|(mapping, ref_path)| {
                mapping
                    .iter()
                    .find(|(_, v)| v.as_str() == ref_path)
                    .map(|(k, _)| k.clone())
            })
            .or_else(|| match &type_ref {
                TypeRef::Named(id) => Some(id.0.clone()),
                _ => None,
            })
            .unwrap_or_else(|| variant_name_hint.clone());

        variants.push(UnionVariant {
            discriminant_value,
            variant_name: variant_name_hint,
            r#type: type_ref,
        });
    }

    Ok(TypeShape::Union(UnionType {
        discriminant_wire_name: discriminator.property_name.clone(),
        variants,
    }))
}

fn convert_object_schema_to_undiscriminated_union_shape(
    ctx: &mut Context,
    name: &str,
    members: &[Schema],
) -> Result<TypeShape, ImportError> {
    let mut variants = Vec::with_capacity(members.len());
    let mut has_unconstrained_member = false;
    for (i, member_schema) in members.iter().enumerate() {
        if schema_is_unconstrained_empty_object(member_schema) {
            has_unconstrained_member = true;
            continue;
        }
        let variant_name_hint = naming::synthetic_openapi_type_name(name, &format!("Variant{i}"));
        variants.push(convert_openapi_schema_to_ir_type_ref(
            ctx,
            member_schema,
            &variant_name_hint,
        )?);
    }
    if has_unconstrained_member {
        let fallback = if !variants.is_empty()
            && variants
                .iter()
                .all(|variant| union_member_type_ref_is_type_ref_is_string_like(ctx, variant))
        {
            TypeRef::Primitive(PrimitiveType::String)
        } else {
            TypeRef::Primitive(PrimitiveType::Any)
        };
        variants.push(fallback);
    }
    Ok(TypeShape::UndiscriminatedUnion { variants })
}

fn union_member_type_ref_is_type_ref_is_string_like(ctx: &Context, type_ref: &TypeRef) -> bool {
    match type_ref {
        TypeRef::Nullable(inner) => type_ref_is_string_like(ctx, inner),
        other => type_ref_is_string_like(ctx, other),
    }
}

fn schema_is_unconstrained_empty_object(schema: &Schema) -> bool {
    let Schema::Object(schema) = schema else {
        return false;
    };
    let ObjectOrReference::Object(schema) = schema.as_ref() else {
        return false;
    };
    schema.schema_type.is_none()
        && schema.format.is_none()
        && schema.properties.is_empty()
        && schema.enum_values.is_empty()
        && schema.one_of.is_empty()
        && schema.any_of.is_empty()
        && schema.all_of.is_empty()
        && schema.items.is_none()
        && schema.prefix_items.is_empty()
        && schema.additional_properties.is_none()
        && schema.discriminator.is_none()
}

fn convert_object_schema_to_object_shape(
    ctx: &mut Context,
    name: &str,
    schema: &ObjectSchema,
) -> Result<TypeShape, ImportError> {
    let mut extends = Vec::new();
    let mut fields = Vec::new();

    for parent_schema in &schema.all_of {
        match parent_schema {
            Schema::Object(obj_or_ref) => match obj_or_ref.as_ref() {
                ObjectOrReference::Ref { ref_path, .. } => {
                    let parent_name = ref_path.rsplit('/').next().unwrap_or(ref_path);
                    extends.push(ctx.component_type_id(parent_name));
                }
                ObjectOrReference::Object(inline) => {
                    fields.extend(extract_object_fields_from_schema_properties(
                        ctx, name, inline,
                    )?);
                }
            },
            Schema::Boolean(_) => {}
        }
    }

    fields.extend(extract_object_fields_from_schema_properties(
        ctx, name, schema,
    )?);
    fields = merge_all_of_extract_object_fields_from_schema_properties(ctx, fields);

    let extra_properties_type = match &schema.additional_properties {
        Some(Schema::Boolean(value)) if value.0 => {
            Some(Box::new(TypeRef::Primitive(PrimitiveType::Any)))
        }
        Some(Schema::Boolean(_)) | None => None,
        Some(additional) => {
            let hint = naming::synthetic_openapi_type_name(name, "AdditionalProperty");
            Some(Box::new(convert_openapi_schema_to_ir_type_ref(
                ctx, additional, &hint,
            )?))
        }
    };
    Ok(TypeShape::Object(ObjectType {
        extends,
        fields,
        extra_properties: extra_properties_type.is_some(),
        extra_properties_type,
    }))
}

fn merge_all_of_extract_object_fields_from_schema_properties(
    ctx: &Context,
    fields: Vec<FieldDeclaration>,
) -> Vec<FieldDeclaration> {
    let mut merged = Vec::<FieldDeclaration>::with_capacity(fields.len());
    let mut positions = HashMap::<String, usize>::with_capacity(fields.len());
    for field in fields {
        let Some(&position) = positions.get(&field.wire_name) else {
            positions.insert(field.wire_name.clone(), merged.len());
            merged.push(field);
            continue;
        };

        let existing = &mut merged[position];
        let (existing_type, existing_optional) =
            strip_optional_wrapper_from_type_ref(&existing.r#type);
        let (new_type, new_optional) = strip_optional_wrapper_from_type_ref(&field.r#type);
        let narrowed =
            if let Some(narrowed) = narrow_all_of_intersection_type(ctx, existing_type, new_type) {
                narrowed
            } else if existing_optional != new_optional {
                if existing_optional {
                    new_type.clone()
                } else {
                    existing_type.clone()
                }
            } else {
                TypeRef::Intersection(Box::new(existing_type.clone()), Box::new(new_type.clone()))
            };
        existing.r#type = if existing_optional && new_optional {
            TypeRef::Optional(Box::new(narrowed))
        } else {
            narrowed
        };
        if existing.docs.is_none() {
            existing.docs = field.docs;
        }
    }
    merged
}

fn narrow_all_of_intersection_type(
    ctx: &Context,
    left: &TypeRef,
    right: &TypeRef,
) -> Option<TypeRef> {
    if left == right {
        return Some(left.clone());
    }
    match (left, right) {
        (TypeRef::Nullable(left), TypeRef::Nullable(right)) => {
            return narrow_all_of_intersection_type(ctx, left, right)
                .map(|narrowed| TypeRef::Nullable(Box::new(narrowed)));
        }
        (TypeRef::Nullable(inner), other) | (other, TypeRef::Nullable(inner)) => {
            return narrow_all_of_intersection_type(ctx, inner, other);
        }
        _ => {}
    }
    if matches!(left, TypeRef::Primitive(PrimitiveType::String))
        && type_ref_has_string_variant(ctx, right)
    {
        return Some(left.clone());
    }
    if matches!(right, TypeRef::Primitive(PrimitiveType::String))
        && type_ref_has_string_variant(ctx, left)
    {
        return Some(right.clone());
    }
    if type_ref_is_numeric_primitive(left) && type_ref_is_numeric_primitive(right) {
        return Some(if type_ref_is_integer_primitive(left) {
            left.clone()
        } else {
            right.clone()
        });
    }
    if type_ref_is_string_like(ctx, left) && type_ref_is_string_like(ctx, right) {
        return Some(
            if matches!(left, TypeRef::Primitive(PrimitiveType::String)) {
                right.clone()
            } else {
                left.clone()
            },
        );
    }
    None
}

fn type_ref_is_numeric_primitive(type_ref: &TypeRef) -> bool {
    matches!(
        type_ref,
        TypeRef::Primitive(
            PrimitiveType::Integer
                | PrimitiveType::Int64
                | PrimitiveType::Float
                | PrimitiveType::Double
        )
    )
}

fn type_ref_is_integer_primitive(type_ref: &TypeRef) -> bool {
    matches!(
        type_ref,
        TypeRef::Primitive(PrimitiveType::Integer | PrimitiveType::Int64)
    )
}

fn type_ref_has_string_variant(ctx: &Context, type_ref: &TypeRef) -> bool {
    if type_ref_is_string_like(ctx, type_ref) {
        return true;
    }
    match type_ref {
        TypeRef::Named(id) => ctx
            .declared
            .iter()
            .find(|declaration| declaration.id == *id)
            .is_some_and(|declaration| match &declaration.shape {
                TypeShape::UndiscriminatedUnion { variants } => variants
                    .iter()
                    .any(|variant| type_ref_has_string_variant(ctx, variant)),
                _ => false,
            }),
        _ => false,
    }
}

fn strip_optional_wrapper_from_type_ref(type_ref: &TypeRef) -> (&TypeRef, bool) {
    match type_ref {
        TypeRef::Optional(inner) => (inner, true),
        other => (other, false),
    }
}

fn type_ref_is_string_like(ctx: &Context, type_ref: &TypeRef) -> bool {
    match type_ref {
        TypeRef::Primitive(
            PrimitiveType::String
            | PrimitiveType::Date
            | PrimitiveType::DateTime
            | PrimitiveType::Uuid
            | PrimitiveType::Bytes,
        ) => true,
        TypeRef::Named(id) => ctx
            .declared
            .iter()
            .find(|declaration| declaration.id == *id)
            .is_some_and(|declaration| match &declaration.shape {
                TypeShape::Enum(_) => true,
                TypeShape::Alias { target } => type_ref_is_string_like(ctx, target),
                _ => false,
            }),
        _ => false,
    }
}

fn extract_object_fields_from_schema_properties(
    ctx: &mut Context,
    parent_name: &str,
    schema: &ObjectSchema,
) -> Result<Vec<FieldDeclaration>, ImportError> {
    let mut fields = Vec::new();
    for (field_name, field_schema) in schema.properties.iter() {
        let name_hint = naming::synthetic_openapi_type_name(
            parent_name,
            &naming::openapi_type_name(field_name),
        );
        let mut type_ref = convert_openapi_schema_to_ir_type_ref(ctx, field_schema, &name_hint)?;
        if !schema.required.iter().any(|r| r == field_name) {
            type_ref = TypeRef::Optional(Box::new(type_ref));
        }
        fields.push(FieldDeclaration {
            wire_name: field_name.clone(),
            field_name: naming::openapi_field_name(field_name),
            r#type: type_ref,
            docs: extract_schema_description(field_schema),
            example: extract_schema_example_value(ctx, field_schema),
        });
    }
    Ok(fields)
}

/// Resolves a field's `example`/first `examples` entry, if declared. `$ref`s
/// are followed the same way `schema_description` would if it needed to —
/// examples live on the concrete schema, not the reference.
fn extract_schema_example_value(ctx: &Context, schema: &Schema) -> Option<serde_json::Value> {
    let resolved = schema.resolve(ctx.spec).ok()?;
    let Schema::Object(obj_or_ref) = resolved else {
        return None;
    };
    let ObjectOrReference::Object(schema) = obj_or_ref.as_ref() else {
        return None;
    };
    schema
        .example
        .clone()
        .or_else(|| schema.examples.first().cloned())
}

fn extract_schema_description(schema: &Schema) -> Option<String> {
    match schema {
        Schema::Object(schema) => match schema.as_ref() {
            ObjectOrReference::Object(schema) => combine_summary_and_description_docs(
                schema.title.as_deref(),
                schema.description.as_deref(),
            ),
            ObjectOrReference::Ref { .. } => None,
        },
        Schema::Boolean(_) => None,
    }
}

fn combine_summary_and_description_docs(
    summary: Option<&str>,
    description: Option<&str>,
) -> Option<String> {
    match (summary, description) {
        (Some(summary), Some(description)) if summary != description => {
            Some(format!("{summary}\n\n{description}"))
        }
        (Some(summary), _) => Some(summary.to_string()),
        (_, Some(description)) => Some(description.to_string()),
        _ => None,
    }
}

fn try_convert_schema_to_primitive_type(schema: &ObjectSchema) -> Option<PrimitiveType> {
    let single_type = match &schema.schema_type {
        Some(SchemaTypeSet::Single(t)) => Some(*t),
        Some(SchemaTypeSet::Multiple(types)) => {
            types.iter().find(|t| **t != SchemaType::Null).copied()
        }
        None => None,
    }?;

    Some(match single_type {
        SchemaType::String => match schema.format.as_deref() {
            Some("date-time") => PrimitiveType::DateTime,
            Some("date") => PrimitiveType::Date,
            Some("uuid") => PrimitiveType::Uuid,
            Some("byte") => PrimitiveType::Bytes,
            Some("binary") => PrimitiveType::Binary,
            _ => PrimitiveType::String,
        },
        SchemaType::Integer => match schema.format.as_deref() {
            Some("int64") => PrimitiveType::Int64,
            _ => PrimitiveType::Integer,
        },
        SchemaType::Number => match schema.format.as_deref() {
            Some("float") => PrimitiveType::Float,
            _ => PrimitiveType::Double,
        },
        SchemaType::Boolean => PrimitiveType::Boolean,
        SchemaType::Object | SchemaType::Array | SchemaType::Null => return None,
    })
}

fn wrap_nullable_schema_type_ref(schema: &ObjectSchema, type_ref: TypeRef) -> TypeRef {
    if schema.is_nullable().unwrap_or(false) {
        TypeRef::Nullable(Box::new(type_ref))
    } else {
        type_ref
    }
}
