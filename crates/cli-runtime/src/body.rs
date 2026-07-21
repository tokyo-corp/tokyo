//! Shared parsing for structured CLI request-body inputs.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::error::ClientError;

/// Parses a JSON document supplied inline.
pub fn parse_inline_json_request_body(source_json_document: &str) -> Result<Value, ClientError> {
    serde_json::from_str(source_json_document)
        .map_err(|error| ClientError::Decode(format!("invalid --body-json value: {error}")))
}

/// Builds one JSON value from repeatable `path=value` field assignments.
///
/// Dotted path segments address object properties and numeric segments address
/// array indexes. Values that are valid JSON literals retain their JSON type;
/// all other values become strings.
pub fn parse_json_request_body_from_field_assignments(
    field_assignments: &[String],
) -> Result<Value, ClientError> {
    if field_assignments.is_empty() {
        return Err(ClientError::Decode(
            "--field requires at least one path=value assignment".to_string(),
        ));
    }

    let mut root_json_value = Value::Null;
    let mut seen_field_paths = BTreeSet::new();
    for field_assignment in field_assignments {
        let (field_path, raw_field_value) = field_assignment.split_once('=').ok_or_else(|| {
            ClientError::Decode(format!(
                "invalid --field {field_assignment:?}: expected path=value"
            ))
        })?;
        let field_path_segments = parse_field_assignment_path(field_path)?;
        if !seen_field_paths.insert(field_path.to_string()) {
            return Err(ClientError::Decode(format!(
                "duplicate --field path {field_path:?}"
            )));
        }
        if let Some(conflicting_field_path) = seen_field_paths.iter().find(|seen_field_path| {
            *seen_field_path != field_path
                && (field_path
                    .strip_prefix(seen_field_path.as_str())
                    .is_some_and(|remaining_field_path_suffix| {
                        remaining_field_path_suffix.starts_with('.')
                    })
                    || seen_field_path.strip_prefix(field_path).is_some_and(
                        |remaining_field_path_suffix| remaining_field_path_suffix.starts_with('.'),
                    ))
        }) {
            return Err(ClientError::Decode(format!(
                "conflicting --field paths {conflicting_field_path:?} and {field_path:?}"
            )));
        }
        let parsed_field_value = serde_json::from_str(raw_field_value)
            .unwrap_or_else(|_| Value::String(raw_field_value.to_string()));
        insert_value_at_field_assignment_path(
            &mut root_json_value,
            &field_path_segments,
            parsed_field_value,
            field_path,
        )?;
    }
    Ok(root_json_value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Key(String),
    Index(usize),
}

fn parse_field_assignment_path(field_path: &str) -> Result<Vec<Segment>, ClientError> {
    if field_path.is_empty() {
        return Err(ClientError::Decode(
            "invalid --field path: path must not be empty".to_string(),
        ));
    }
    field_path
        .split('.')
        .map(|field_path_segment| {
            if field_path_segment.is_empty() {
                return Err(ClientError::Decode(format!(
                    "invalid --field path {field_path:?}: empty path segment"
                )));
            }
            if field_path_segment.bytes().all(|byte| byte.is_ascii_digit()) {
                field_path_segment
                    .parse::<usize>()
                    .map(Segment::Index)
                    .map_err(|_| {
                        ClientError::Decode(format!(
                            "invalid --field path {field_path:?}: array index {field_path_segment:?} is too large"
                        ))
                    })
            } else {
                Ok(Segment::Key(field_path_segment.to_string()))
            }
        })
        .collect()
}

fn insert_value_at_field_assignment_path(
    current_json_slot: &mut Value,
    remaining_path_segments: &[Segment],
    value_to_insert: Value,
    original_field_path: &str,
) -> Result<(), ClientError> {
    let Some((current_path_segment, remaining_child_segments)) =
        remaining_path_segments.split_first()
    else {
        *current_json_slot = value_to_insert;
        return Ok(());
    };
    let expected_json_container_kind = match current_path_segment {
        Segment::Key(_) => "object",
        Segment::Index(_) => "array",
    };
    if current_json_slot.is_null() {
        *current_json_slot = match current_path_segment {
            Segment::Key(_) => Value::Object(Map::new()),
            Segment::Index(_) => Value::Array(Vec::new()),
        };
    }

    let child_json_slot = match current_path_segment {
        Segment::Key(object_property_name) => {
            if !current_json_slot.is_object() {
                return Err(build_field_path_container_conflict_error(
                    original_field_path,
                    expected_json_container_kind,
                    current_json_slot,
                ));
            }
            current_json_slot
                .as_object_mut()
                .expect("value kind checked above")
                .entry(object_property_name.clone())
                .or_insert(Value::Null)
        }
        Segment::Index(array_index) => {
            if !current_json_slot.is_array() {
                return Err(build_field_path_container_conflict_error(
                    original_field_path,
                    expected_json_container_kind,
                    current_json_slot,
                ));
            }
            let array_items = current_json_slot
                .as_array_mut()
                .expect("value kind checked above");
            if array_items.len() <= *array_index {
                array_items.resize(array_index + 1, Value::Null);
            }
            &mut array_items[*array_index]
        }
    };
    insert_value_at_field_assignment_path(
        child_json_slot,
        remaining_child_segments,
        value_to_insert,
        original_field_path,
    )
}

fn build_field_path_container_conflict_error(
    field_path: &str,
    expected_json_container_kind: &str,
    actual_json_value: &Value,
) -> ClientError {
    ClientError::Decode(format!(
        "invalid --field path {field_path:?}: expected {expected_json_container_kind}, found {}",
        describe_json_value_kind(actual_json_value)
    ))
}

fn describe_json_value_kind(json_value: &Value) -> &'static str {
    match json_value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_build_nested_objects_and_arrays() {
        let value = parse_json_request_body_from_field_assignments(&[
            "name=garden".to_string(),
            "enabled=true".to_string(),
            "items.0.count=2".to_string(),
            "items.1.count=null".to_string(),
        ])
        .expect("fields parse");
        assert_eq!(
            value,
            serde_json::json!({
                "name": "garden",
                "enabled": true,
                "items": [{"count": 2}, {"count": null}]
            })
        );
    }

    #[test]
    fn valid_json_literals_are_preserved_and_other_values_are_strings() {
        let value = parse_json_request_body_from_field_assignments(&[
            "object={\"x\":1}".to_string(),
            "array=[1,2]".to_string(),
            "plain=01".to_string(),
        ])
        .expect("fields parse");
        assert_eq!(value["object"], serde_json::json!({"x": 1}));
        assert_eq!(value["array"], serde_json::json!([1, 2]));
        assert_eq!(value["plain"], "01");
    }

    #[test]
    fn malformed_and_conflicting_paths_have_stable_errors() {
        assert_eq!(
            parse_json_request_body_from_field_assignments(&["missing".to_string()])
                .expect_err("missing equals errors")
                .to_string(),
            "could not decode response: invalid --field \"missing\": expected path=value"
        );
        assert_eq!(
            parse_json_request_body_from_field_assignments(&[
                "a=1".to_string(),
                "a.b=2".to_string()
            ])
            .expect_err("scalar traversal errors")
            .to_string(),
            "could not decode response: conflicting --field paths \"a\" and \"a.b\""
        );
        assert_eq!(
            parse_json_request_body_from_field_assignments(&["a=1".to_string(), "a=2".to_string()])
                .expect_err("duplicate errors")
                .to_string(),
            "could not decode response: duplicate --field path \"a\""
        );
    }

    #[test]
    fn inline_json_reports_its_input_mode() {
        let error = parse_inline_json_request_body("{").expect_err("invalid JSON errors");
        assert!(error.to_string().contains("invalid --body-json value"));
    }
}
