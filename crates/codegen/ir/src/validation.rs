use std::collections::{BTreeSet, HashSet};

use crate::api::{Api, IR_SCHEMA_VERSION};
use crate::auth::AuthSchemeKind;
use crate::http::{Endpoint, Parameter, QuerySerialization, ResponseEncoding, UrlResolution};
use crate::id::{EndpointId, TypeId};
use crate::pagination::Pagination;
use crate::types::{TypeRef, TypeShape};

#[derive(Debug, Clone, PartialEq, Eq)]
/// One IR invariant violation found during validation.
pub struct ValidationError {
    /// Path to the invalid IR location.
    pub path: String,
    /// Human-readable validation failure.
    pub message: String,
}

impl ValidationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for ValidationError {}

impl Api {
    /// Validates invariants required by every emitter.
    ///
    /// Importers should call this before returning an API, and emitters should
    /// call it at trust boundaries when accepting persisted or programmatic IR.
    ///
    /// # Errors
    ///
    /// Returns all discovered [`ValidationError`] values when the API cannot be
    /// safely consumed by emitters.
    #[must_use = "validation errors should be handled before emitting code"]
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        if self.schema_version != IR_SCHEMA_VERSION {
            errors.push(ValidationError::new(
                "schema_version",
                format!(
                    "unsupported IR schema {}; expected {IR_SCHEMA_VERSION}",
                    self.schema_version
                ),
            ));
        }

        let type_ids: HashSet<&TypeId> = self
            .types
            .iter()
            .map(|declaration| &declaration.id)
            .collect();
        if type_ids.len() != self.types.len() {
            report_duplicates(
                self.types
                    .iter()
                    .map(|declaration| declaration.id.to_string()),
                "types",
                &mut errors,
            );
        }
        let endpoint_ids: HashSet<&EndpointId> =
            self.endpoints.iter().map(|endpoint| &endpoint.id).collect();
        if endpoint_ids.len() != self.endpoints.len() {
            report_duplicates(
                self.endpoints
                    .iter()
                    .map(|endpoint| endpoint.id.to_string()),
                "endpoints",
                &mut errors,
            );
        }

        for declaration in &self.types {
            let path = format!("types[{}]", declaration.id);
            match &declaration.shape {
                TypeShape::Alias { target } => {
                    validate_type_ref(target, &path, &type_ids, &mut errors)
                }
                TypeShape::Object(object) => {
                    for extended in &object.extends {
                        if !type_ids.contains(extended) {
                            errors.push(ValidationError::new(
                                format!("{path}.extends"),
                                format!("references missing type {extended}"),
                            ));
                        }
                    }
                    for field in &object.fields {
                        validate_type_ref(
                            &field.r#type,
                            &format!("{path}.fields[{}]", field.wire_name),
                            &type_ids,
                            &mut errors,
                        );
                    }
                    if let Some(extra) = &object.extra_properties_type {
                        validate_type_ref(
                            extra,
                            &format!("{path}.extra_properties_type"),
                            &type_ids,
                            &mut errors,
                        );
                    }
                }
                TypeShape::Union(union) => {
                    for variant in &union.variants {
                        validate_type_ref(
                            &variant.r#type,
                            &format!("{path}.variants[{}]", variant.discriminant_value),
                            &type_ids,
                            &mut errors,
                        );
                    }
                }
                TypeShape::UndiscriminatedUnion { variants } => {
                    for (index, variant) in variants.iter().enumerate() {
                        validate_type_ref(
                            variant,
                            &format!("{path}.variants[{index}]"),
                            &type_ids,
                            &mut errors,
                        );
                    }
                }
                TypeShape::Enum(_) => {}
            }
        }

        for endpoint in &self.endpoints {
            validate_endpoint(endpoint, &type_ids, &endpoint_ids, &mut errors);
        }
        for channel in &self.channels {
            let path = format!("channels[{}]", channel.id);
            validate_parameters(
                &channel.path_parameters,
                ParameterLocation::Path,
                &format!("{path}.path_parameters"),
                &type_ids,
                &mut errors,
            );
            validate_parameters(
                &channel.query_parameters,
                ParameterLocation::Query,
                &format!("{path}.query_parameters"),
                &type_ids,
                &mut errors,
            );
            if let Some(send) = channel.direction.send() {
                validate_type_ref(send, &format!("{path}.send"), &type_ids, &mut errors);
            }
            if let Some(receive) = channel.direction.receive() {
                validate_type_ref(receive, &format!("{path}.receive"), &type_ids, &mut errors);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[derive(Clone, Copy)]
enum ParameterLocation {
    Path,
    Query,
    Header,
    Cookie,
}

fn validate_endpoint(
    endpoint: &Endpoint,
    type_ids: &HashSet<&TypeId>,
    endpoint_ids: &HashSet<&EndpointId>,
    errors: &mut Vec<ValidationError>,
) {
    let path = format!("endpoints[{}]", endpoint.id);
    validate_parameters(
        &endpoint.path_parameters,
        ParameterLocation::Path,
        &format!("{path}.path_parameters"),
        type_ids,
        errors,
    );
    validate_parameters(
        &endpoint.query_parameters,
        ParameterLocation::Query,
        &format!("{path}.query_parameters"),
        type_ids,
        errors,
    );
    validate_parameters(
        &endpoint.headers,
        ParameterLocation::Header,
        &format!("{path}.headers"),
        type_ids,
        errors,
    );
    validate_parameters(
        &endpoint.cookies,
        ParameterLocation::Cookie,
        &format!("{path}.cookies"),
        type_ids,
        errors,
    );

    let mut placeholders = path_placeholders(&endpoint.path);
    let absolute_parameter_name = match &endpoint.url_resolution {
        UrlResolution::CallerSuppliedAbsolute { parameter_name } => Some(parameter_name.as_str()),
        UrlResolution::BaseUrlAndPath => None,
    };
    if let Some(parameter_name) = absolute_parameter_name
        && let Some(parameter) = endpoint
            .path_parameters
            .iter()
            .find(|parameter| parameter.name == parameter_name)
    {
        placeholders.remove(parameter.wire_name.as_str());
    }
    let declared: BTreeSet<&str> = endpoint
        .path_parameters
        .iter()
        .filter(|parameter| Some(parameter.name.as_str()) != absolute_parameter_name)
        .map(|parameter| parameter.wire_name.as_str())
        .collect();
    if placeholders != declared {
        errors.push(ValidationError::new(
            format!("{path}.path"),
            format!(
                "path placeholders {placeholders:?} do not match declared parameters {declared:?}"
            ),
        ));
    }
    if let UrlResolution::CallerSuppliedAbsolute { parameter_name } = &endpoint.url_resolution
        && !endpoint
            .path_parameters
            .iter()
            .any(|parameter| parameter.name == *parameter_name)
    {
        errors.push(ValidationError::new(
            format!("{path}.url_resolution"),
            format!("references missing path parameter {parameter_name:?}"),
        ));
    }

    if let Some(body) = &endpoint.request_body {
        validate_type_ref(body, &format!("{path}.request_body"), type_ids, errors);
        if endpoint.request_media_type.is_none() {
            errors.push(ValidationError::new(
                format!("{path}.request_media_type"),
                "request body has no exact media type",
            ));
        }
    } else if endpoint.request_media_type.is_some() {
        errors.push(ValidationError::new(
            format!("{path}.request_media_type"),
            "media type is set without a request body",
        ));
    }
    for (field, encoding) in &endpoint.multipart_field_encodings {
        for header in &encoding.headers {
            validate_type_ref(
                &header.r#type,
                &format!("{path}.multipart_field_encodings[{field}].headers"),
                type_ids,
                errors,
            );
        }
    }
    for (status, response) in &endpoint.responses {
        if let Some(body) = &response.body {
            validate_type_ref(
                body,
                &format!("{path}.responses[{status}]"),
                type_ids,
                errors,
            );
            if response.encoding != ResponseEncoding::Empty && response.media_type.is_none() {
                errors.push(ValidationError::new(
                    format!("{path}.responses[{status}].media_type"),
                    "non-empty response has no exact media type",
                ));
            }
        } else if response.encoding != ResponseEncoding::Empty {
            errors.push(ValidationError::new(
                format!("{path}.responses[{status}].encoding"),
                "non-empty encoding has no response body",
            ));
        }
    }
    if let Some(error) = &endpoint.wildcard_error {
        validate_type_ref(error, &format!("{path}.wildcard_error"), type_ids, errors);
        if endpoint.wildcard_error_media_type.is_none() {
            errors.push(ValidationError::new(
                format!("{path}.wildcard_error_media_type"),
                "wildcard error has no exact media type",
            ));
        }
    }
    if let Some(conditional) = &endpoint.conditional_streaming {
        validate_type_ref(
            &conditional.payload,
            &format!("{path}.conditional_streaming.payload"),
            type_ids,
            errors,
        );
    }
    if let Some(pagination) = &endpoint.pagination {
        let parameter = match pagination {
            Pagination::Cursor { page_param, .. } => Some(page_param),
            Pagination::Offset { offset_param, .. } => Some(offset_param),
            _ => None,
        };
        if parameter.is_some_and(|name| {
            !endpoint
                .query_parameters
                .iter()
                .any(|candidate| candidate.wire_name == *name)
        }) {
            errors.push(ValidationError::new(
                format!("{path}.pagination"),
                "references a missing query parameter",
            ));
        }
    }
    for alternative in &endpoint.auth {
        for requirement in &alternative.schemes {
            if let AuthSchemeKind::Inferred { via_endpoint } = &requirement.scheme.kind
                && !endpoint_ids.contains(via_endpoint)
            {
                errors.push(ValidationError::new(
                    format!("{path}.auth"),
                    format!("references missing inferred-auth endpoint {via_endpoint}"),
                ));
            }
        }
    }
}

fn validate_parameters(
    parameters: &[Parameter],
    location: ParameterLocation,
    path: &str,
    type_ids: &HashSet<&TypeId>,
    errors: &mut Vec<ValidationError>,
) {
    report_duplicates(
        parameters
            .iter()
            .map(|parameter| parameter.wire_name.clone()),
        path,
        errors,
    );
    for parameter in parameters {
        let parameter_path = format!("{path}[{}]", parameter.wire_name);
        validate_type_ref(&parameter.r#type, &parameter_path, type_ids, errors);
        let valid_style = matches!(
            (location, parameter.serialization),
            (
                ParameterLocation::Path,
                QuerySerialization::Simple { .. }
                    | QuerySerialization::Label { .. }
                    | QuerySerialization::Matrix { .. }
            ) | (
                ParameterLocation::Query,
                QuerySerialization::Form { .. }
                    | QuerySerialization::SpaceDelimited
                    | QuerySerialization::PipeDelimited
                    | QuerySerialization::DeepObject
            ) | (ParameterLocation::Header, QuerySerialization::Simple { .. })
                | (ParameterLocation::Cookie, QuerySerialization::Form { .. })
        );
        if !valid_style {
            errors.push(ValidationError::new(
                format!("{parameter_path}.serialization"),
                "serialization style is invalid for the parameter location",
            ));
        }
        if parameter.allow_reserved && !matches!(location, ParameterLocation::Query) {
            errors.push(ValidationError::new(
                format!("{parameter_path}.allow_reserved"),
                "allowReserved is only valid for query parameters",
            ));
        }
        if matches!(location, ParameterLocation::Path)
            && matches!(parameter.r#type, TypeRef::Optional(_))
        {
            errors.push(ValidationError::new(
                parameter_path,
                "path parameters must be required",
            ));
        }
    }
}

fn validate_type_ref(
    type_ref: &TypeRef,
    path: &str,
    type_ids: &HashSet<&TypeId>,
    errors: &mut Vec<ValidationError>,
) {
    match type_ref {
        TypeRef::Named(id) if !type_ids.contains(id) => errors.push(ValidationError::new(
            path,
            format!("references missing type {id}"),
        )),
        TypeRef::List(inner) | TypeRef::Nullable(inner) | TypeRef::Optional(inner) => {
            validate_type_ref(inner, path, type_ids, errors);
        }
        TypeRef::Tuple { items, rest } => {
            for item in items {
                validate_type_ref(item, path, type_ids, errors);
            }
            if let Some(rest) = rest {
                validate_type_ref(rest, path, type_ids, errors);
            }
        }
        TypeRef::Map { key, value } => {
            validate_type_ref(key, path, type_ids, errors);
            validate_type_ref(value, path, type_ids, errors);
        }
        TypeRef::Intersection(left, right) => {
            validate_type_ref(left, path, type_ids, errors);
            validate_type_ref(right, path, type_ids, errors);
        }
        TypeRef::Primitive(_) | TypeRef::Named(_) => {}
    }
}

fn path_placeholders(path: &str) -> BTreeSet<&str> {
    let mut placeholders = BTreeSet::new();
    let mut offset = 0;
    while let Some(relative_start) = path[offset..].find('{') {
        let start = offset + relative_start;
        if start > 0 && path.as_bytes()[start - 1] == b'$' {
            offset = start + 1;
            continue;
        }
        let value_start = start + 1;
        let Some(relative_end) = path[value_start..].find('}') else {
            break;
        };
        let end = value_start + relative_end;
        placeholders.insert(&path[value_start..end]);
        offset = end + 1;
    }
    placeholders
}

fn report_duplicates(
    values: impl IntoIterator<Item = String>,
    path: &str,
    errors: &mut Vec<ValidationError>,
) {
    let mut seen = HashSet::new();
    for value in values {
        if !seen.insert(value.clone()) {
            errors.push(ValidationError::new(
                path,
                format!("contains duplicate {value:?}"),
            ));
        }
    }
}
