use serde_json::Value;

/// Upgrades an OpenAPI 3.0.x document's schema dialect to 3.1's so it can be fed
/// through `oas3` (which only targets 3.1). The structural delta between 3.0 and
/// 3.1 that actually breaks parsing is almost entirely in how schemas express
/// nullability and exclusive bounds; paths/operations/parameters/responses are
/// otherwise compatible as-is. This is a best-effort upconversion, not a full 3.0
/// implementation — it unblocks the common cases, not every exotic edge case.
pub fn upgrade_3_0_to_3_1(mut doc: Value) -> Value {
    let is_3_0 = doc
        .get("openapi")
        .and_then(Value::as_str)
        .map(|version| version.starts_with("3.0"))
        .unwrap_or(false);

    if !is_3_0 {
        return doc;
    }

    normalize_schemas(&mut doc);

    if let Some(document_object) = doc.as_object_mut() {
        document_object.insert("openapi".to_string(), Value::String("3.1.0".to_string()));
    }

    doc
}

fn normalize_schemas(value: &mut Value) {
    match value {
        Value::Object(schema_object) => {
            rewrite_nullable_schema_object(schema_object);
            fixup_exclusive_bound(schema_object, "minimum", "exclusiveMinimum");
            fixup_exclusive_bound(schema_object, "maximum", "exclusiveMaximum");
            for nested_value in schema_object.values_mut() {
                normalize_schemas(nested_value);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_schemas(item);
            }
        }
        _ => {}
    }
}

fn rewrite_nullable_schema_object(schema_object: &mut serde_json::Map<String, Value>) {
    let Some(nullable) = schema_object.remove("nullable") else {
        return;
    };
    if nullable != Value::Bool(true) {
        return;
    }

    match schema_object.get_mut("type") {
        Some(Value::String(type_name)) => {
            let type_name = type_name.clone();
            schema_object.insert(
                "type".to_string(),
                Value::Array(vec![
                    Value::String(type_name),
                    Value::String("null".to_string()),
                ]),
            );
        }
        Some(Value::Array(type_names)) => {
            let has_null = type_names
                .iter()
                .any(|type_name| type_name.as_str() == Some("null"));
            if !has_null {
                type_names.push(Value::String("null".to_string()));
            }
        }
        _ => {
            // No `type` to attach nullability to (e.g. a bare $ref or an
            // untyped schema) — nothing safe to do without more context.
        }
    }
}

/// OpenAPI 3.0 encoded "exclusive" as a boolean modifier alongside `minimum`/
/// `maximum`; 3.1 (following JSON Schema 2020-12) uses a standalone numeric
/// `exclusiveMinimum`/`exclusiveMaximum` instead.
fn fixup_exclusive_bound(
    schema_object: &mut serde_json::Map<String, Value>,
    bound_key: &str,
    exclusive_key: &str,
) {
    let is_exclusive = matches!(schema_object.get(exclusive_key), Some(Value::Bool(true)));
    if !is_exclusive {
        return;
    }
    match schema_object.remove(bound_key) {
        Some(bound_value) => {
            schema_object.insert(exclusive_key.to_string(), bound_value);
        }
        None => {
            schema_object.remove(exclusive_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rewrites_nullable_true_into_type_array() {
        let input = json!({ "openapi": "3.0.3", "type": "string", "nullable": true });
        let out = upgrade_3_0_to_3_1(input);
        assert_eq!(out["type"], json!(["string", "null"]));
        assert!(out.get("nullable").is_none());
    }

    #[test]
    fn rewrites_boolean_exclusive_minimum() {
        let input = json!({ "openapi": "3.0.3", "minimum": 5, "exclusiveMinimum": true });
        let out = upgrade_3_0_to_3_1(input);
        assert_eq!(out["exclusiveMinimum"], json!(5));
        assert!(out.get("minimum").is_none());
    }

    #[test]
    fn leaves_3_1_documents_untouched() {
        let input = json!({ "openapi": "3.1.0", "type": ["string", "null"] });
        let out = upgrade_3_0_to_3_1(input.clone());
        assert_eq!(out, input);
    }

    #[test]
    fn bumps_version_string_after_normalizing() {
        let input = json!({ "openapi": "3.0.0" });
        let out = upgrade_3_0_to_3_1(input);
        assert_eq!(out["openapi"], json!("3.1.0"));
    }
}
