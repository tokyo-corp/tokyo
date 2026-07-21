use heck::{ToLowerCamelCase, ToUpperCamelCase};

pub fn openapi_type_name(raw: &str) -> String {
    raw.to_upper_camel_case()
}

pub fn openapi_field_name(raw: &str) -> String {
    raw.to_lower_camel_case()
}

pub fn openapi_endpoint_name(operation_id: Option<&str>, method: &str, path: &str) -> String {
    if let Some(id) = operation_id {
        return id.to_lower_camel_case();
    }
    let cleaned: String = path
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect();
    format!("{method} {cleaned}").to_lower_camel_case()
}

/// Synthesizes a type name for a schema with no name of its own (e.g. an inline
/// request/response body), so every generated type still gets a stable identity.
pub fn synthetic_openapi_type_name(operation_name: &str, suffix: &str) -> String {
    format!("{}{}", operation_name.to_upper_camel_case(), suffix)
}

pub fn string_is_valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|character| {
        character == '_' || character == '$' || character.is_ascii_alphabetic()
    }) && chars
        .all(|character| character == '_' || character == '$' || character.is_ascii_alphanumeric())
}
