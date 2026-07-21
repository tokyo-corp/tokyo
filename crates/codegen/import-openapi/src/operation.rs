use std::collections::{BTreeMap, HashMap, HashSet};

use heck::ToLowerCamelCase;
use oas3::spec::{
    ObjectOrReference, Operation, ParameterIn, ParameterStyle, PathItem, Schema, SchemaType,
    SchemaTypeSet, Server,
};
use tokyo_ir::http::{
    BodyEncoding, CliOverrides, ConditionalStreaming, Endpoint, FormFieldEncoding, HttpMethod,
    MultipartFieldEncoding, MultipartHeader, Parameter, QuerySerialization, Response,
    ResponseEncoding, StreamingKind, UrlResolution,
};
use tokyo_ir::id::{ChannelId, EndpointId};
use tokyo_ir::pagination::Pagination;
use tokyo_ir::types::{PrimitiveType, TypeRef};
use tokyo_ir::websocket::{WebSocketChannel, WebSocketDirection};

use crate::error::ImportError;
use crate::naming;
use crate::schema::{Context, convert_openapi_schema_to_ir_type_ref};
use crate::security::resolve_auth_schemes;

const METHODS: &[(HttpMethod, &str)] = &[
    (HttpMethod::Get, "get"),
    (HttpMethod::Head, "head"),
    (HttpMethod::Post, "post"),
    (HttpMethod::Put, "put"),
    (HttpMethod::Patch, "patch"),
    (HttpMethod::Delete, "delete"),
    (HttpMethod::Options, "options"),
    (HttpMethod::Trace, "trace"),
];

/// Matches a content-type tolerantly (case, and a trailing `; charset=...` /
/// `; boundary=...` parameter some frameworks attach) rather than an exact match.
fn find_exact_response_media_type<'a>(
    content: impl IntoIterator<Item = (&'a String, &'a oas3::spec::MediaType)>,
    expected: &str,
) -> Option<&'a oas3::spec::MediaType> {
    find_exact_response_media_type_entry(content, expected).map(|(_, media_type)| media_type)
}

fn find_exact_response_media_type_entry<'a>(
    content: impl IntoIterator<Item = (&'a String, &'a oas3::spec::MediaType)>,
    expected: &str,
) -> Option<(&'a String, &'a oas3::spec::MediaType)> {
    content.into_iter().find_map(|(content_type, media_type)| {
        let base = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim();
        base.eq_ignore_ascii_case(expected)
            .then_some((content_type, media_type))
    })
}

fn find_json_response_media_type<'a>(
    content: impl IntoIterator<Item = (&'a String, &'a oas3::spec::MediaType)>,
) -> Option<&'a oas3::spec::MediaType> {
    find_json_response_media_type_entry(content).map(|(_, media_type)| media_type)
}

fn find_json_response_media_type_entry<'a>(
    content: impl IntoIterator<Item = (&'a String, &'a oas3::spec::MediaType)>,
) -> Option<(&'a String, &'a oas3::spec::MediaType)> {
    content.into_iter().find_map(|(content_type, media_type)| {
        let base = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
            .to_ascii_lowercase();
        (base == "application/json" || base.ends_with("+json"))
            .then_some((content_type, media_type))
    })
}

fn serialize_openapi_schema_to_json_value(
    schema: &Schema,
) -> Result<serde_json::Value, ImportError> {
    Ok(serde_json::to_value(schema)?)
}

fn find_text_response_media_type_entry<'a>(
    content: impl IntoIterator<Item = (&'a String, &'a oas3::spec::MediaType)>,
) -> Option<(&'a String, &'a oas3::spec::MediaType)> {
    content.into_iter().find_map(|(content_type, media_type)| {
        let base = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
            .to_ascii_lowercase();
        (base.starts_with("text/")
            || matches!(
                base.as_str(),
                "application/yaml" | "application/x-yaml" | "application/xml" | "application/csv"
            )
            || base.ends_with("+yaml")
            || base.ends_with("+xml"))
        .then_some((content_type, media_type))
    })
}

fn http_status_code_is_success(status: &str) -> bool {
    status
        .parse::<u16>()
        .is_ok_and(|status| (200..300).contains(&status))
}

fn response_map_has_json_success_response(
    operation: &Operation,
    spec: &oas3::Spec,
) -> Result<bool, ImportError> {
    let Some(responses) = &operation.responses else {
        return Ok(false);
    };
    for (status, response) in responses.iter() {
        if http_status_code_is_success(status) {
            let response = response.resolve(spec)?;
            if find_json_response_media_type(&response.content).is_some() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn detect_streaming_response_media_type(
    response: &oas3::spec::Response,
) -> Result<Option<(StreamingKind, &oas3::spec::MediaType)>, ImportError> {
    let sse = find_exact_response_media_type(&response.content, "text/event-stream");
    let ndjson = find_exact_response_media_type(&response.content, "application/x-ndjson")
        .or_else(|| find_exact_response_media_type(&response.content, "application/ndjson"));
    match (sse, ndjson) {
        (Some(_), Some(_)) => Err(ImportError::Unsupported(
            "a success response advertises both SSE and NDJSON streams".to_string(),
        )),
        (Some(media), None) => Ok(Some((StreamingKind::Sse { resumable: false }, media))),
        (None, Some(media)) => Ok(Some((StreamingKind::Json, media))),
        (None, None) => Ok(None),
    }
}

fn schema_resolves_to_boolean_value(
    schema: &Schema,
    spec: &oas3::Spec,
) -> Result<bool, ImportError> {
    let schema = schema.resolve(spec)?;
    let Schema::Object(schema) = schema else {
        return Ok(false);
    };
    let ObjectOrReference::Object(schema) = schema.as_ref() else {
        unreachable!("Schema::resolve never returns a Ref");
    };
    Ok(match &schema.schema_type {
        Some(SchemaTypeSet::Single(SchemaType::Boolean)) => true,
        Some(SchemaTypeSet::Multiple(types)) => {
            types.contains(&SchemaType::Boolean)
                && types
                    .iter()
                    .all(|kind| matches!(kind, SchemaType::Boolean | SchemaType::Null))
        }
        _ => false,
    })
}

fn request_body_has_boolean_stream_selector(
    operation: &Operation,
    spec: &oas3::Spec,
) -> Result<bool, ImportError> {
    let Some(body_ref) = &operation.request_body else {
        return Ok(false);
    };
    let body = body_ref.resolve(spec)?;
    let Some(media) = find_json_response_media_type(&body.content) else {
        return Ok(false);
    };
    let Some(schema) = &media.schema else {
        return Ok(false);
    };
    let schema = schema.resolve(spec)?;
    let Schema::Object(schema) = schema else {
        return Ok(false);
    };
    let ObjectOrReference::Object(schema) = schema.as_ref() else {
        unreachable!("Schema::resolve never returns a Ref");
    };
    let Some(stream) = schema.properties.get("stream") else {
        return Ok(false);
    };
    schema_resolves_to_boolean_value(stream, spec)
}

fn substitute_openapi_server_url_variables(server: &Server) -> String {
    let mut url = server.url.clone();
    for (name, variable) in &server.variables {
        url = url.replace(&format!("{{{name}}}"), &variable.default);
    }
    url
}

fn extract_absolute_url_parameter_name(path: &str) -> Option<&str> {
    path.strip_prefix("/{")
        .and_then(|path| path.strip_suffix('}'))
        .or_else(|| {
            path.strip_prefix("/<")
                .and_then(|path| path.strip_suffix('>'))
        })
        .filter(|name| !name.is_empty() && !name.contains(['/', '{', '}']))
}

fn convert_openapi_response_to_ir_response(
    ctx: &mut Context,
    response: &oas3::spec::Response,
    name_hint: &str,
    force_empty: bool,
) -> Result<Response, ImportError> {
    if force_empty || response.content.is_empty() {
        return Ok(Response::empty());
    }

    // Prefer the standard JSON representation when an operation advertises
    // alternate vendor media types. The generated client does not expose an
    // Accept selector, so its typed contract is the default JSON representation.
    if let Some((content_type, media_type)) = find_json_response_media_type_entry(&response.content)
    {
        let body = match &media_type.schema {
            Some(schema) => convert_openapi_schema_to_ir_type_ref(ctx, schema, name_hint)?,
            None => TypeRef::Primitive(PrimitiveType::Any),
        };
        return Ok(Response::with_media_type(
            body,
            ResponseEncoding::Json,
            content_type,
        ));
    }

    if response.content.len() > 1 {
        return Err(ImportError::Unsupported(format!(
            "{name_hint} declares multiple non-JSON response content representations; content negotiation is not supported"
        )));
    }

    if let Some((content_type, media_type)) = find_text_response_media_type_entry(&response.content)
    {
        let body = match &media_type.schema {
            Some(schema) => convert_openapi_schema_to_ir_type_ref(ctx, schema, name_hint)?,
            None => TypeRef::Primitive(PrimitiveType::String),
        };
        return Ok(Response::with_media_type(
            body,
            ResponseEncoding::Text,
            content_type,
        ));
    }
    if let Some((content_type, media_type)) =
        find_exact_response_media_type_entry(&response.content, "application/x-ndjson")
    {
        let body = match &media_type.schema {
            Some(schema) => convert_openapi_schema_to_ir_type_ref(ctx, schema, name_hint)?,
            None => TypeRef::Primitive(PrimitiveType::Any),
        };
        return Ok(Response::with_media_type(
            body,
            ResponseEncoding::Json,
            content_type,
        ));
    }

    if let Some((content_type, _)) =
        response
            .content
            .iter()
            .find_map(|(content_type, media_type)| {
                let base = content_type
                    .split(';')
                    .next()
                    .unwrap_or(content_type)
                    .trim()
                    .to_ascii_lowercase();
                matches!(
                    base.as_str(),
                    "application/octet-stream" | "application/pdf" | "application/octocat-stream"
                )
                .then_some((content_type, media_type))
            })
    {
        // Fetch exposes non-text application media as Blob regardless of the
        // OpenAPI convention used to describe its bytes (often plain string).
        let body = TypeRef::Primitive(PrimitiveType::Binary);
        return Ok(Response::with_media_type(
            body,
            ResponseEncoding::Binary,
            content_type,
        ));
    }

    let content_types: Vec<&str> = response.content.keys().map(String::as_str).collect();
    Err(ImportError::Unsupported(format!(
        "response has no supported content type (found: {}); only JSON, text/*, and binary application media are supported",
        content_types.join(", ")
    )))
}

/// Resolves a parameter's declared example: the parameter's own `example`
/// takes precedence (OpenAPI's own precedence rule), falling back to its
/// schema's `example`/first `examples` entry.
fn extract_parameter_example_value(
    spec: &oas3::Spec,
    param: &oas3::spec::Parameter,
) -> Option<serde_json::Value> {
    if let Some(example) = &param.example {
        return Some(example.clone());
    }
    let schema = param.schema.as_ref()?;
    let resolved = schema.resolve(spec).ok()?;
    let Schema::Object(obj_or_ref) = resolved else {
        return None;
    };
    let ObjectOrReference::Object(object_schema) = obj_or_ref.as_ref() else {
        return None;
    };
    object_schema
        .example
        .clone()
        .or_else(|| object_schema.examples.first().cloned())
}

fn determine_parameter_serialization(
    param: &oas3::spec::Parameter,
) -> Result<QuerySerialization, ImportError> {
    let unsupported = |style: &str| {
        ImportError::Unsupported(format!(
            "{} parameter `{}` cannot use serialization style `{style}`",
            match param.location {
                ParameterIn::Path => "path",
                ParameterIn::Query => "query",
                ParameterIn::Header => "header",
                ParameterIn::Cookie => "cookie",
            },
            param.name
        ))
    };
    match param.location {
        ParameterIn::Path => match param.style {
            None | Some(ParameterStyle::Simple) => Ok(QuerySerialization::Simple {
                explode: param.explode.unwrap_or(false),
            }),
            Some(ParameterStyle::Label) => Ok(QuerySerialization::Label {
                explode: param.explode.unwrap_or(false),
            }),
            Some(ParameterStyle::Matrix) => Ok(QuerySerialization::Matrix {
                explode: param.explode.unwrap_or(false),
            }),
            Some(ref style) => Err(unsupported(&format!("{style:?}"))),
        },
        ParameterIn::Query => match param.style {
            None | Some(ParameterStyle::Form) => Ok(QuerySerialization::Form {
                explode: param.explode.unwrap_or(true),
            }),
            Some(ParameterStyle::SpaceDelimited) if param.explode == Some(true) => {
                Err(ImportError::Unsupported(format!(
                    "query parameter `{}` uses spaceDelimited with explode=true, which OpenAPI does not define",
                    param.name
                )))
            }
            Some(ParameterStyle::SpaceDelimited) => Ok(QuerySerialization::SpaceDelimited),
            Some(ParameterStyle::PipeDelimited) if param.explode == Some(true) => {
                Err(ImportError::Unsupported(format!(
                    "query parameter `{}` uses pipeDelimited with explode=true, which OpenAPI does not define",
                    param.name
                )))
            }
            Some(ParameterStyle::PipeDelimited) => Ok(QuerySerialization::PipeDelimited),
            Some(ParameterStyle::DeepObject) if param.explode == Some(false) => {
                Err(ImportError::Unsupported(format!(
                    "query parameter `{}` uses deepObject with explode=false, which OpenAPI does not define",
                    param.name
                )))
            }
            Some(ParameterStyle::DeepObject) => Ok(QuerySerialization::DeepObject),
            Some(ref style) => Err(unsupported(&format!("{style:?}"))),
        },
        ParameterIn::Header => match param.style {
            None | Some(ParameterStyle::Simple) => Ok(QuerySerialization::Simple {
                explode: param.explode.unwrap_or(false),
            }),
            Some(ref style) => Err(unsupported(&format!("{style:?}"))),
        },
        ParameterIn::Cookie => match param.style {
            None | Some(ParameterStyle::Form) => Ok(QuerySerialization::Form {
                explode: param.explode.unwrap_or(true),
            }),
            Some(ref style) => Err(unsupported(&format!("{style:?}"))),
        },
    }
}

fn parse_pagination_extension(
    operation: &Operation,
    method: &str,
    path: &str,
) -> Result<Option<Pagination>, ImportError> {
    let Some(value) = operation.extensions.get("tokyo-pagination") else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| {
        ImportError::Unsupported(format!(
            "{method} {path} x-tokyo-pagination must be an object"
        ))
    })?;
    let string = |name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                ImportError::Unsupported(format!(
                    "{method} {path} x-tokyo-pagination requires string `{name}`"
                ))
            })
    };
    let kind = string("kind")?;
    let pagination = match kind.as_str() {
        "cursor" => Pagination::Cursor {
            page_param: string("pageParam")?,
            next_field: string("nextField")?,
            results_field: string("resultsField")?,
        },
        "offset" => {
            let step = object
                .get("step")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    ImportError::Unsupported(format!(
                        "{method} {path} offset pagination requires integer `step`"
                    ))
                })?;
            let step = u32::try_from(step).map_err(|_| {
                ImportError::Unsupported(format!("{method} {path} pagination step exceeds u32"))
            })?;
            if step == 0 {
                return Err(ImportError::Unsupported(format!(
                    "{method} {path} pagination step must be greater than zero"
                )));
            }
            Pagination::Offset {
                offset_param: string("offsetParam")?,
                has_next_field: string("hasNextField")?,
                step: Some(step),
            }
        }
        "uri" | "path" | "custom" => {
            return Err(ImportError::Unsupported(format!(
                "{method} {path} pagination kind `{kind}` has no safe generic traversal semantics"
            )));
        }
        _ => {
            return Err(ImportError::Unsupported(format!(
                "{method} {path} has unknown pagination kind `{kind}`"
            )));
        }
    };
    Ok(Some(pagination))
}

fn parse_cli_overrides_extension(
    operation: &Operation,
    method: &str,
    path: &str,
) -> Result<Option<CliOverrides>, ImportError> {
    let name = openapi_extension_string_value(operation, "tokyo-cli-name", method, path)?;
    let hidden = openapi_extension_boolean_value(operation, "tokyo-cli-hidden", method, path)?
        .unwrap_or(false);
    let ignore = openapi_extension_boolean_value(operation, "tokyo-cli-ignore", method, path)?
        .unwrap_or(false);
    let aliases = match operation.extensions.get("tokyo-cli-aliases") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| {
                ImportError::Unsupported(format!(
                    "{method} {path} x-tokyo-cli-aliases must be an array of strings"
                ))
            })?
            .iter()
            .map(|item| {
                item.as_str().map(str::to_owned).ok_or_else(|| {
                    ImportError::Unsupported(format!(
                        "{method} {path} x-tokyo-cli-aliases must be an array of strings"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    if name.is_none() && !hidden && !ignore && aliases.is_empty() {
        return Ok(None);
    }
    Ok(Some(CliOverrides {
        name,
        hidden,
        ignore,
        aliases,
    }))
}

fn openapi_extension_string_value(
    operation: &Operation,
    key: &str,
    method: &str,
    path: &str,
) -> Result<Option<String>, ImportError> {
    match operation.extensions.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or_else(|| {
                ImportError::Unsupported(format!("{method} {path} x-{key} must be a string"))
            }),
    }
}

fn openapi_extension_boolean_value(
    operation: &Operation,
    key: &str,
    method: &str,
    path: &str,
) -> Result<Option<bool>, ImportError> {
    match operation.extensions.get(key) {
        None => Ok(None),
        Some(value) => value.as_bool().map(Some).ok_or_else(|| {
            ImportError::Unsupported(format!("{method} {path} x-{key} must be a boolean"))
        }),
    }
}

fn parse_streaming(
    operation: &Operation,
    spec: &oas3::Spec,
    method: &str,
    path: &str,
) -> Result<Option<StreamingKind>, ImportError> {
    let declared = operation.extensions.get("tokyo-streaming");
    let mut response_types = Vec::new();
    if let Some(responses) = &operation.responses {
        for (status, response) in responses.iter() {
            if !http_status_code_is_success(status) {
                continue;
            }
            let response = response.resolve(spec)?;
            response_types.extend(response.content.keys().map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or(value)
                    .trim()
                    .to_ascii_lowercase()
            }));
        }
    }
    let has_sse = response_types
        .iter()
        .any(|value| value == "text/event-stream");
    let has_ndjson = response_types
        .iter()
        .any(|value| value == "application/x-ndjson" || value == "application/ndjson");
    let has_json = response_types
        .iter()
        .any(|value| value == "application/json" || value.ends_with("+json"));
    let Some(value) = declared else {
        return match (has_sse, has_ndjson) {
            (true, false) if !has_json => Ok(Some(StreamingKind::Sse { resumable: false })),
            (false, true) if !has_json => Ok(Some(StreamingKind::Json)),
            (true, false) | (false, true) => Ok(None),
            (true, true) => Err(ImportError::Unsupported(format!(
                "{method} {path} advertises both SSE and NDJSON success streams"
            ))),
            (false, false) => Ok(None),
        };
    };
    let (kind, resumable) = if let Some(kind) = value.as_str() {
        (kind, false)
    } else {
        let object = value.as_object().ok_or_else(|| {
            ImportError::Unsupported(format!(
                "{method} {path} x-tokyo-streaming must be a string or object"
            ))
        })?;
        (
            object
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ImportError::Unsupported(format!(
                        "{method} {path} x-tokyo-streaming requires string `kind`"
                    ))
                })?,
            object
                .get("resumable")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        )
    };
    let (streaming, expected): (StreamingKind, &[&str]) = match kind {
        "sse" if !resumable => (
            StreamingKind::Sse { resumable: false },
            &["text/event-stream"],
        ),
        "sse" => {
            return Err(ImportError::Unsupported(format!(
                "{method} {path} requests resumable SSE, but reconnect and Last-Event-ID semantics are not implemented"
            )));
        }
        "ndjson" | "json" if !resumable => (
            StreamingKind::Json,
            &[
                "application/x-ndjson",
                "application/ndjson",
                "application/json",
            ],
        ),
        "text" if !resumable => (StreamingKind::Text, &["text/plain"]),
        "ndjson" | "json" | "text" => {
            return Err(ImportError::Unsupported(format!(
                "{method} {path} only SSE streams may set resumable=true"
            )));
        }
        _ => {
            return Err(ImportError::Unsupported(format!(
                "{method} {path} has unknown streaming kind `{kind}`"
            )));
        }
    };
    if !response_types
        .iter()
        .any(|actual| expected.contains(&actual.as_str()))
    {
        return Err(ImportError::Unsupported(format!(
            "{method} {path} declares `{kind}` streaming but no success response uses a matching content type"
        )));
    }
    if has_json && (has_sse || has_ndjson) {
        Ok(None)
    } else {
        Ok(Some(streaming))
    }
}

pub fn convert_openapi_paths_to_endpoints(ctx: &mut Context) -> Result<Vec<Endpoint>, ImportError> {
    let Some(paths) = ctx.spec.paths.clone() else {
        return Ok(Vec::new());
    };

    let mut endpoints = Vec::new();
    let mut method_names = HashMap::<String, String>::new();
    for generated in [
        "authentication",
        "clientCredentialsToken",
        "constructor",
        "encodeBasic",
        "headers",
        "requestWithRetry",
        "retryDelay",
        "setAuthHeader",
        "tokenCache",
        "tokenRequests",
        "waitForRetry",
    ] {
        method_names.insert(
            generated.to_string(),
            "generated ApiClient member".to_string(),
        );
    }
    for (path, path_item) in paths.iter() {
        if path_item.reference.is_some() {
            return Err(ImportError::Unsupported(format!(
                "path item reference on `{path}` is not supported; bundle path item references before importing"
            )));
        }
        if path_item.extensions.contains_key("tokyo-websocket") {
            if !http_operations_from_path_item(path_item).is_empty() {
                return Err(ImportError::Unsupported(format!(
                    "`{path}` declares x-tokyo-websocket alongside HTTP methods; a path is either a WebSocket channel or a set of HTTP operations, not both"
                )));
            }
            continue;
        }
        for (method, operation) in http_operations_from_path_item(path_item) {
            let endpoint =
                convert_openapi_operation_to_endpoint(ctx, path, path_item, method, operation)?;
            let operation_context = format!(
                "{} {path}",
                METHODS
                    .iter()
                    .find(|(candidate, _)| *candidate == method)
                    .map_or("operation", |(_, name)| *name)
            );
            if !naming::string_is_valid_identifier(&endpoint.name) {
                return Err(ImportError::Unsupported(format!(
                    "{operation_context} normalizes to invalid generated method identifier `{}`",
                    endpoint.name
                )));
            }
            register_method_name(&mut method_names, &endpoint.name, &operation_context)?;
            if endpoint.pagination.is_some() {
                let suffix = if matches!(endpoint.pagination, Some(Pagination::Cursor { .. })) {
                    "All"
                } else {
                    "Pages"
                };
                register_method_name(
                    &mut method_names,
                    &format!("{}{suffix}", endpoint.name),
                    &format!("generated pagination method for {operation_context}"),
                )?;
            }
            for tag in &endpoint.tags {
                register_method_name(
                    &mut method_names,
                    &tag.to_lower_camel_case(),
                    &format!("generated tag group for `{tag}`"),
                )?;
            }
            endpoints.push(endpoint);
        }
    }
    Ok(endpoints)
}

pub fn convert_channels(ctx: &mut Context) -> Result<Vec<WebSocketChannel>, ImportError> {
    let Some(paths) = ctx.spec.paths.clone() else {
        return Ok(Vec::new());
    };

    let mut channels = Vec::new();
    let mut names = HashSet::new();
    for (path, path_item) in paths.iter() {
        if path_item.reference.is_some() {
            continue;
        }
        if !path_item.extensions.contains_key("tokyo-websocket") {
            continue;
        }
        let channel = parse_websocket_channel(ctx, path, path_item)?;
        if !naming::string_is_valid_identifier(&channel.name) {
            return Err(ImportError::Unsupported(format!(
                "`{path}` websocket channel normalizes to invalid generated method identifier `{}`",
                channel.name
            )));
        }
        if let Some(existing) = names.replace(channel.name.clone()) {
            return Err(ImportError::Unsupported(format!(
                "websocket channel method name collision: `{existing}` and `{}` both normalize to `{}`",
                channel.path, channel.name
            )));
        }
        channels.push(channel);
    }
    Ok(channels)
}

fn parse_websocket_channel(
    ctx: &mut Context,
    path: &str,
    path_item: &PathItem,
) -> Result<WebSocketChannel, ImportError> {
    let value = path_item
        .extensions
        .get("tokyo-websocket")
        .expect("caller checked presence");
    let object = value.as_object().ok_or_else(|| {
        ImportError::Unsupported(format!("`{path}` x-tokyo-websocket must be an object"))
    })?;

    let explicit_id = object
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let name = naming::openapi_endpoint_name(explicit_id.as_deref(), "connect", path);
    let id = ChannelId(format!("WS:{path}"));

    let summary = object
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let docs = object
        .get("description")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let direction_str = object
        .get("direction")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            ImportError::Unsupported(format!(
                "`{path}` x-tokyo-websocket requires string `direction`"
            ))
        })?;

    let send_schema = object.get("send");
    let receive_schema = object.get("receive");

    let mut schema_ref =
        |value: &serde_json::Value, suffix: &str| -> Result<TypeRef, ImportError> {
            let schema: Schema = serde_json::from_value(value.clone()).map_err(|error| {
                ImportError::Unsupported(format!(
                    "`{path}` x-tokyo-websocket `{suffix}` is not a valid schema: {error}"
                ))
            })?;
            let name_hint = naming::synthetic_openapi_type_name(&name, suffix);
            convert_openapi_schema_to_ir_type_ref(ctx, &schema, &name_hint)
        };

    let direction = match direction_str {
        "client_to_server" => {
            if receive_schema.is_some() {
                return Err(ImportError::Unsupported(format!(
                    "`{path}` direction `client_to_server` must not declare `receive`"
                )));
            }
            let send = send_schema.ok_or_else(|| {
                ImportError::Unsupported(format!(
                    "`{path}` direction `client_to_server` requires `send`"
                ))
            })?;
            WebSocketDirection::ClientToServer {
                send: schema_ref(send, "Send")?,
            }
        }
        "server_to_client" => {
            if send_schema.is_some() {
                return Err(ImportError::Unsupported(format!(
                    "`{path}` direction `server_to_client` must not declare `send`"
                )));
            }
            let receive = receive_schema.ok_or_else(|| {
                ImportError::Unsupported(format!(
                    "`{path}` direction `server_to_client` requires `receive`"
                ))
            })?;
            WebSocketDirection::ServerToClient {
                receive: schema_ref(receive, "Receive")?,
            }
        }
        "bidirectional" => {
            let send = send_schema.ok_or_else(|| {
                ImportError::Unsupported(format!(
                    "`{path}` direction `bidirectional` requires `send`"
                ))
            })?;
            let receive = receive_schema.ok_or_else(|| {
                ImportError::Unsupported(format!(
                    "`{path}` direction `bidirectional` requires `receive`"
                ))
            })?;
            WebSocketDirection::Bidirectional {
                send: schema_ref(send, "Send")?,
                receive: schema_ref(receive, "Receive")?,
            }
        }
        other => {
            return Err(ImportError::Unsupported(format!(
                "`{path}` has unknown x-tokyo-websocket direction `{other}`"
            )));
        }
    };

    let effective_servers = &path_item.servers;
    if effective_servers.len() > 1 {
        return Err(ImportError::Unsupported(format!(
            "`{path}` declares multiple path servers; generated channels expose one base URL"
        )));
    }
    let server_url = effective_servers
        .first()
        .map(substitute_openapi_server_url_variables);

    let mut path_parameters = Vec::new();
    let mut query_parameters = Vec::new();
    for param_ref in &path_item.parameters {
        let param = param_ref.resolve(ctx.spec)?;
        let name_hint =
            naming::synthetic_openapi_type_name(&name, &naming::openapi_type_name(&param.name));
        let schema = param.schema.as_ref().ok_or_else(|| {
            ImportError::Unsupported(format!(
                "parameter `{}` on `{path}` websocket channel has no schema",
                param.name
            ))
        })?;
        let type_ref = convert_openapi_schema_to_ir_type_ref(ctx, schema, &name_hint)?;
        let type_ref = if param.required.unwrap_or(false) {
            type_ref
        } else {
            TypeRef::Optional(Box::new(type_ref))
        };
        let serialization = determine_parameter_serialization(&param)?;
        let ir_param = Parameter {
            wire_name: param.name.clone(),
            name: naming::openapi_field_name(&param.name),
            r#type: type_ref,
            docs: param.description.clone(),
            serialization,
            allow_reserved: param.allow_reserved.unwrap_or(false),
            example: extract_parameter_example_value(ctx.spec, &param),
        };
        match param.location {
            ParameterIn::Path => path_parameters.push(ir_param),
            ParameterIn::Query => query_parameters.push(ir_param),
            ParameterIn::Header => {
                return Err(ImportError::Unsupported(format!(
                    "header parameter `{}` on `{path}` websocket channel is not yet supported",
                    param.name
                )));
            }
            ParameterIn::Cookie => {
                return Err(ImportError::Unsupported(format!(
                    "cookie parameter `{}` on `{path}` websocket channel is not supported",
                    param.name
                )));
            }
        }
    }

    let auth = resolve_auth_schemes(ctx.spec, &ctx.spec.security, ctx.oauth_token_endpoints())?;

    Ok(WebSocketChannel {
        id,
        name,
        path: path.to_string(),
        server_url,
        summary,
        docs,
        tags: Vec::new(),
        path_parameters,
        query_parameters,
        auth,
        direction,
    })
}

fn register_method_name(
    names: &mut HashMap<String, String>,
    name: &str,
    context: &str,
) -> Result<(), ImportError> {
    if name.is_empty() {
        return Err(ImportError::Unsupported(format!(
            "{context} normalizes to an empty generated method name"
        )));
    }
    if let Some(existing) = names.insert(name.to_string(), context.to_string()) {
        if existing == context {
            return Ok(());
        }
        return Err(ImportError::Unsupported(format!(
            "endpoint method name collision: {existing} and {context} both normalize to `{name}`"
        )));
    }
    Ok(())
}

fn http_operations_from_path_item(path_item: &PathItem) -> Vec<(HttpMethod, &Operation)> {
    let mut ops = Vec::new();
    macro_rules! push {
        ($field:ident, $method:expr) => {
            if let Some(op) = &path_item.$field {
                ops.push(($method, op));
            }
        };
    }
    push!(get, HttpMethod::Get);
    push!(head, HttpMethod::Head);
    push!(post, HttpMethod::Post);
    push!(put, HttpMethod::Put);
    push!(patch, HttpMethod::Patch);
    push!(delete, HttpMethod::Delete);
    push!(options, HttpMethod::Options);
    push!(trace, HttpMethod::Trace);
    ops
}

fn convert_openapi_operation_to_endpoint(
    ctx: &mut Context,
    path: &str,
    path_item: &PathItem,
    method: HttpMethod,
    operation: &Operation,
) -> Result<Endpoint, ImportError> {
    let method_str = METHODS
        .iter()
        .find(|(m, _)| *m == method)
        .map(|(_, s)| *s)
        .unwrap_or("op");
    let name = naming::openapi_endpoint_name(operation.operation_id.as_deref(), method_str, path);
    let id = EndpointId(format!("{}:{path}", method_str.to_ascii_uppercase()));
    if !operation.callbacks.is_empty() {
        return Err(ImportError::Unsupported(format!(
            "{method_str} {path} declares callbacks, which cannot be represented by the generated client"
        )));
    }
    let effective_servers = if operation.servers.is_empty() {
        &path_item.servers
    } else {
        &operation.servers
    };
    if effective_servers.len() > 1 {
        let level = if operation.servers.is_empty() {
            "path"
        } else {
            "operation"
        };
        return Err(ImportError::Unsupported(format!(
            "{method_str} {path} declares multiple {level} servers; generated methods expose one base URL"
        )));
    }
    let resolved_server = effective_servers
        .first()
        .map(substitute_openapi_server_url_variables);
    let absolute_parameter = match resolved_server.as_deref() {
        Some("https://") => extract_absolute_url_parameter_name(path).ok_or_else(|| {
            ImportError::Unsupported(format!(
                "{method_str} {path} combines scheme-only server `https://` with a path that is not exactly one parameter"
            ))
        })?,
        Some(url) if url.ends_with("://") => {
            return Err(ImportError::Unsupported(format!(
                "{method_str} {path} uses ambiguous scheme-only server `{url}`"
            )));
        }
        _ => "",
    };
    let server_url = if absolute_parameter.is_empty() {
        resolved_server
    } else {
        None
    };
    let streaming = parse_streaming(operation, ctx.spec, method_str, path)?;
    let has_json_success = response_map_has_json_success_response(operation, ctx.spec)?;
    let has_boolean_stream_selector =
        request_body_has_boolean_stream_selector(operation, ctx.spec)?;
    let pagination = parse_pagination_extension(operation, method_str, path)?;

    let mut path_parameters = Vec::new();
    let mut query_parameters = Vec::new();
    let mut headers = Vec::new();
    let mut cookies = Vec::new();

    let mut parameter_refs = Vec::new();
    for param_ref in &path_item.parameters {
        let param = param_ref.resolve(ctx.spec)?;
        parameter_refs.push((param.name.clone(), param.location, param_ref));
    }
    for param_ref in &operation.parameters {
        let param = param_ref.resolve(ctx.spec)?;
        if let Some(existing) = parameter_refs
            .iter_mut()
            .find(|(name, location, _)| name == &param.name && *location == param.location)
        {
            *existing = (param.name.clone(), param.location, param_ref);
        } else {
            parameter_refs.push((param.name.clone(), param.location, param_ref));
        }
    }

    for (_, _, param_ref) in parameter_refs {
        let param = param_ref.resolve(ctx.spec)?;
        let name_hint =
            naming::synthetic_openapi_type_name(&name, &naming::openapi_type_name(&param.name));
        let type_ref = match (&param.schema, &param.content) {
            (Some(schema), None) => convert_openapi_schema_to_ir_type_ref(ctx, schema, &name_hint)?,
            (None, Some(content)) if content.len() == 1 => {
                let (content_type, media) = content.iter().next().expect("length checked");
                if !content_type
                    .split(';')
                    .next()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
                {
                    return Err(ImportError::Unsupported(format!(
                        "parameter `{}` on {method_str} {path} uses unsupported content type `{content_type}`",
                        param.name
                    )));
                }
                let schema = media.schema.as_ref().ok_or_else(|| {
                    ImportError::Unsupported(format!(
                        "parameter `{}` on {method_str} {path} has content without a schema",
                        param.name
                    ))
                })?;
                convert_openapi_schema_to_ir_type_ref(ctx, schema, &name_hint)?
            }
            (None, Some(_)) => {
                return Err(ImportError::Unsupported(format!(
                    "parameter `{}` on {method_str} {path} declares multiple content representations",
                    param.name
                )));
            }
            (Some(_), Some(_)) => {
                return Err(ImportError::Unsupported(format!(
                    "parameter `{}` on {method_str} {path} declares both schema and content",
                    param.name
                )));
            }
            (None, None) => {
                return Err(ImportError::Unsupported(format!(
                    "parameter `{}` on {method_str} {path} has neither schema nor content",
                    param.name
                )));
            }
        };
        let type_ref = if param.required.unwrap_or(false) {
            type_ref
        } else {
            tokyo_ir::types::TypeRef::Optional(Box::new(type_ref))
        };
        let serialization = determine_parameter_serialization(&param)?;
        let allow_reserved = param.allow_reserved.unwrap_or(false);
        if allow_reserved && !matches!(param.location, ParameterIn::Query) {
            return Err(ImportError::Unsupported(format!(
                "parameter `{}` sets allowReserved outside the query location",
                param.name
            )));
        }
        let ir_param = Parameter {
            wire_name: param.name.clone(),
            name: naming::openapi_field_name(&param.name),
            r#type: type_ref,
            docs: param.description.clone(),
            serialization,
            allow_reserved,
            example: extract_parameter_example_value(ctx.spec, &param),
        };
        match param.location {
            ParameterIn::Path => path_parameters.push(ir_param),
            ParameterIn::Query => query_parameters.push(ir_param),
            ParameterIn::Header => headers.push(ir_param),
            ParameterIn::Cookie => cookies.push(ir_param),
        }
    }
    let url_resolution = if absolute_parameter.is_empty() {
        UrlResolution::BaseUrlAndPath
    } else {
        let parameter_name = if let Some(parameter) = path_parameters
            .iter()
            .find(|parameter| parameter.wire_name == absolute_parameter)
        {
            if parameter.r#type != TypeRef::Primitive(PrimitiveType::String) {
                return Err(ImportError::Unsupported(format!(
                    "{method_str} {path} absolute URL parameter `{absolute_parameter}` must be a required string"
                )));
            }
            parameter.name.clone()
        } else if path == format!("/<{absolute_parameter}>") {
            let name = naming::openapi_field_name(absolute_parameter);
            path_parameters.push(Parameter {
                wire_name: absolute_parameter.to_string(),
                name: name.clone(),
                r#type: TypeRef::Primitive(PrimitiveType::String),
                docs: Some("Complete caller-supplied absolute URL.".to_string()),
                serialization: QuerySerialization::Simple { explode: false },
                allow_reserved: false,
                example: None,
            });
            name
        } else {
            return Err(ImportError::Unsupported(format!(
                "{method_str} {path} scheme-only server requires path parameter `{absolute_parameter}`"
            )));
        };
        UrlResolution::CallerSuppliedAbsolute { parameter_name }
    };
    if let Some(pagination) = &pagination {
        let page_param = match pagination {
            Pagination::Cursor { page_param, .. } => page_param,
            Pagination::Offset { offset_param, .. } => offset_param,
            _ => unreachable!("unsupported pagination kinds are rejected while parsing"),
        };
        if !query_parameters
            .iter()
            .any(|parameter| parameter.wire_name == *page_param)
        {
            return Err(ImportError::Unsupported(format!(
                "{method_str} {path} pagination references missing query parameter `{page_param}`"
            )));
        }
    }
    if pagination.is_some() && streaming.is_some() {
        return Err(ImportError::Unsupported(format!(
            "{method_str} {path} cannot combine pagination and streaming"
        )));
    }

    let (
        request_body,
        request_schema,
        request_body_encoding,
        request_media_type,
        form_field_serializations,
        multipart_field_encodings,
    ) = match &operation.request_body {
        Some(body_ref) => {
            let body = body_ref.resolve(ctx.spec)?;
            let wrap_optionality = |type_ref| {
                if body.required.unwrap_or(false) {
                    type_ref
                } else {
                    tokyo_ir::types::TypeRef::Optional(Box::new(type_ref))
                }
            };

            if let Some((content_type, media_type)) =
                find_json_response_media_type_entry(&body.content)
            {
                let name_hint = naming::synthetic_openapi_type_name(&name, "Request");
                let request_schema = media_type
                    .schema
                    .as_ref()
                    .map(serialize_openapi_schema_to_json_value)
                    .transpose()?;
                let type_ref = match &media_type.schema {
                    Some(schema) => convert_openapi_schema_to_ir_type_ref(ctx, schema, &name_hint)?,
                    None => TypeRef::Primitive(PrimitiveType::Any),
                };
                (
                    Some(wrap_optionality(type_ref)),
                    request_schema,
                    BodyEncoding::Json,
                    Some(content_type.clone()),
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
            } else if let Some((content_type, media_type)) =
                find_exact_response_media_type_entry(&body.content, "multipart/form-data")
            {
                let mut field_encodings = BTreeMap::new();
                for (field_name, encoding) in &media_type.encoding {
                    let mut headers = Vec::new();
                    for (header_name, header_ref) in &encoding.headers {
                        let header = header_ref.resolve(ctx.spec)?;
                        let header_type = match &header.schema {
                            Some(schema) => convert_openapi_schema_to_ir_type_ref(
                                ctx,
                                schema,
                                &naming::synthetic_openapi_type_name(
                                    &name,
                                    &format!(
                                        "{}{}Header",
                                        naming::openapi_type_name(field_name),
                                        naming::openapi_type_name(header_name)
                                    ),
                                ),
                            )?,
                            None => TypeRef::Primitive(PrimitiveType::String),
                        };
                        headers.push(MultipartHeader {
                            wire_name: header_name.to_string(),
                            r#type: header_type,
                            docs: header.description.clone(),
                        });
                    }
                    field_encodings.insert(
                        field_name.clone(),
                        MultipartFieldEncoding {
                            content_type: encoding.content_type.clone(),
                            headers,
                        },
                    );
                }
                let schema = media_type.schema.as_ref().ok_or_else(|| {
                    ImportError::Unsupported(format!(
                        "{method_str} {path} has a multipart/form-data body without a schema"
                    ))
                })?;
                let name_hint = naming::synthetic_openapi_type_name(&name, "Request");
                let type_ref = convert_openapi_schema_to_ir_type_ref(ctx, schema, &name_hint)?;
                (
                    Some(wrap_optionality(type_ref)),
                    None,
                    BodyEncoding::Multipart,
                    Some(content_type.clone()),
                    BTreeMap::new(),
                    field_encodings,
                )
            } else if let Some((content_type, media_type)) = find_exact_response_media_type_entry(
                &body.content,
                "application/x-www-form-urlencoded",
            ) {
                if media_type
                    .encoding
                    .values()
                    .any(|encoding| !encoding.headers.is_empty() || encoding.content_type.is_some())
                {
                    return Err(ImportError::Unsupported(format!(
                        "{method_str} {path} customizes URL-encoded form fields with headers or contentType"
                    )));
                }
                let mut serializations = BTreeMap::new();
                for (field_name, encoding) in &media_type.encoding {
                    let serialization = match encoding.style.as_deref() {
                        None | Some("form") => QuerySerialization::Form {
                            explode: encoding.explode.unwrap_or(true),
                        },
                        Some("deepObject") if encoding.explode.unwrap_or(true) => {
                            QuerySerialization::DeepObject
                        }
                        Some(style) => {
                            return Err(ImportError::Unsupported(format!(
                                "{method_str} {path} form field `{field_name}` uses unsupported style `{style}`"
                            )));
                        }
                    };
                    serializations.insert(
                        field_name.clone(),
                        FormFieldEncoding {
                            serialization,
                            allow_reserved: encoding.allow_reserved.unwrap_or(false),
                        },
                    );
                }
                let schema = media_type.schema.as_ref().ok_or_else(|| {
                    ImportError::Unsupported(format!(
                        "{method_str} {path} has a form body without a schema"
                    ))
                })?;
                let name_hint = naming::synthetic_openapi_type_name(&name, "Request");
                let type_ref = convert_openapi_schema_to_ir_type_ref(ctx, schema, &name_hint)?;
                (
                    Some(wrap_optionality(type_ref)),
                    None,
                    BodyEncoding::Form,
                    Some(content_type.clone()),
                    serializations,
                    BTreeMap::new(),
                )
            } else if let Some((content_type, media_type)) =
                find_text_response_media_type_entry(&body.content)
            {
                let type_ref = match &media_type.schema {
                    Some(schema) => {
                        let name_hint = naming::synthetic_openapi_type_name(&name, "Request");
                        convert_openapi_schema_to_ir_type_ref(ctx, schema, &name_hint)?
                    }
                    None => TypeRef::Primitive(PrimitiveType::String),
                };
                (
                    Some(wrap_optionality(type_ref)),
                    None,
                    BodyEncoding::Text,
                    Some(content_type.clone()),
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
            } else if let Some((content_type, media_type)) =
                find_exact_response_media_type_entry(&body.content, "application/octet-stream")
            {
                let type_ref = match &media_type.schema {
                    Some(schema) => {
                        let name_hint = naming::synthetic_openapi_type_name(&name, "Request");
                        convert_openapi_schema_to_ir_type_ref(ctx, schema, &name_hint)?
                    }
                    None => TypeRef::Primitive(PrimitiveType::Binary),
                };
                (
                    Some(wrap_optionality(type_ref)),
                    None,
                    BodyEncoding::Binary,
                    Some(content_type.clone()),
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
            } else if body.content.is_empty() {
                (
                    None,
                    None,
                    BodyEncoding::Json,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
            } else {
                // A body was declared but none of its content types are JSON or
                // multipart — silently generating a client that sends no body at
                // all would be a worse bug than refusing to import it.
                let content_types: Vec<&str> = body.content.keys().map(String::as_str).collect();
                return Err(ImportError::Unsupported(format!(
                    "{method_str} {path} has a request body with no supported content type (found: {}); only application/json, multipart/form-data, and application/x-www-form-urlencoded are supported",
                    content_types.join(", ")
                )));
            }
        }
        None => (
            None,
            None,
            BodyEncoding::Json,
            None,
            BTreeMap::new(),
            BTreeMap::new(),
        ),
    };
    reject_colliding_generated_parameter_names(
        method_str,
        path,
        &path_parameters,
        &query_parameters,
        &headers,
        &cookies,
        request_body.is_some(),
    )?;

    let mut responses = BTreeMap::new();
    let mut response_schemas = BTreeMap::new();
    let mut wildcard_error = None;
    let mut wildcard_error_encoding = ResponseEncoding::Json;
    let mut wildcard_error_media_type = None;
    let mut accept = None;
    let mut conditional_streaming = None;
    if let Some(response_map) = &operation.responses {
        for (status, response_ref) in response_map.iter() {
            let response = response_ref.resolve(ctx.spec)?;
            if let Some((_, media_type)) = find_json_response_media_type_entry(&response.content)
                && let Some(schema) = &media_type.schema
            {
                response_schemas.insert(
                    status.clone(),
                    serialize_openapi_schema_to_json_value(schema)?,
                );
            }
            let conditional_stream = if http_status_code_is_success(status) && has_json_success {
                detect_streaming_response_media_type(&response)?
            } else {
                None
            };
            if conditional_stream.is_some() && !has_boolean_stream_selector {
                return Err(ImportError::Unsupported(format!(
                    "{method_str} {path} advertises JSON and streaming success responses without a boolean request-body `stream` field"
                )));
            }
            if response.content.len() > 1
                && find_json_response_media_type(&response.content).is_some()
            {
                accept = Some("application/json".to_string());
            }
            // OpenAPI links describe optional follow-up operations. They do not
            // change this response's wire representation, so a direct HTTP
            // client can safely omit automatic traversal while retaining the
            // linked operations as independently callable endpoints.
            let name_hint =
                naming::synthetic_openapi_type_name(&name, &format!("Response{status}"));
            let status_code = status.parse::<u16>();
            let converted = convert_openapi_response_to_ir_response(
                ctx,
                &response,
                &name_hint,
                method == HttpMethod::Head || status_code == Ok(204),
            )?;
            if let Some((kind, media)) = conditional_stream {
                let payload_hint =
                    naming::synthetic_openapi_type_name(&name, &format!("StreamResponse{status}"));
                let payload = match &media.schema {
                    Some(schema) => {
                        convert_openapi_schema_to_ir_type_ref(ctx, schema, &payload_hint)?
                    }
                    None if matches!(kind, StreamingKind::Sse { .. }) => {
                        TypeRef::Primitive(PrimitiveType::String)
                    }
                    None => TypeRef::Primitive(PrimitiveType::Any),
                };
                let candidate = ConditionalStreaming {
                    request_body_field: "stream".to_string(),
                    kind,
                    payload,
                };
                if conditional_streaming
                    .as_ref()
                    .is_some_and(|existing| existing != &candidate)
                {
                    return Err(ImportError::Unsupported(format!(
                        "{method_str} {path} declares incompatible conditional stream representations across success responses"
                    )));
                }
                conditional_streaming = Some(candidate);
            }

            match status_code {
                Ok(code) => {
                    responses.insert(code, converted);
                }
                Err(_) if status == "default" => {
                    wildcard_error = converted.body;
                    wildcard_error_encoding = converted.encoding;
                    wildcard_error_media_type = converted.media_type;
                }
                Err(_) if matches!(status.as_str(), "1XX" | "2XX" | "3XX" | "4XX" | "5XX") => {
                    return Err(ImportError::Unsupported(format!(
                        "{method_str} {path} response status pattern `{status}` is not supported; use explicit status codes or `default`"
                    )));
                }
                Err(_) => {
                    return Err(ImportError::Unsupported(format!(
                        "{method_str} {path} has invalid response status key `{status}`"
                    )));
                }
            }
        }
    }

    let requirements = if ctx.operation_has_security(path, method_str) {
        &operation.security
    } else {
        &ctx.spec.security
    };
    let auth = resolve_auth_schemes(ctx.spec, requirements, ctx.oauth_token_endpoints())?;
    let cli = parse_cli_overrides_extension(operation, method_str, path)?;

    Ok(Endpoint {
        id,
        name,
        method,
        path: path.to_string(),
        url_resolution,
        server_url,
        accept,
        summary: operation.summary.clone(),
        docs: operation.description.clone(),
        tags: operation.tags.clone(),
        path_parameters,
        query_parameters,
        headers,
        cookies,
        request_body,
        request_schema,
        request_body_encoding,
        request_media_type,
        form_field_serializations,
        multipart_field_encodings,
        responses,
        response_schemas,
        wildcard_error,
        wildcard_error_encoding,
        wildcard_error_media_type,
        auth,
        authz: operation
            .extensions
            .get("authz")
            .or_else(|| operation.extensions.get("x-authz"))
            .cloned(),
        pagination,
        streaming,
        conditional_streaming,
        cli,
    })
}

fn reject_colliding_generated_parameter_names(
    method: &str,
    path: &str,
    path_parameters: &[Parameter],
    query_parameters: &[Parameter],
    headers: &[Parameter],
    cookies: &[Parameter],
    has_body: bool,
) -> Result<(), ImportError> {
    let mut names = HashMap::<&str, String>::new();
    for (location, parameters) in [
        ("path", path_parameters),
        ("query", query_parameters),
        ("header", headers),
        ("cookie", cookies),
    ] {
        for parameter in parameters {
            let context = format!("{location} parameter `{}`", parameter.wire_name);
            if let Some(existing) = names.insert(&parameter.name, context.clone()) {
                return Err(ImportError::Unsupported(format!(
                    "{method} {path} parameter object name collision: {existing} and {context} both normalize to `{}`",
                    parameter.name
                )));
            }
        }
    }
    if has_body && let Some(existing) = names.get("body") {
        return Err(ImportError::Unsupported(format!(
            "{method} {path} parameter object name collision: {existing} conflicts with request body property `body`"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_operation_authz_policy_for_cli_orientation() {
        let api = crate::import_openapi_yaml_document(
            r#"
openapi: 3.1.0
info: { title: Authz, version: "1.0.0" }
paths:
  /drivers:
    post:
      operationId: createDriver
      x-authz:
        org_types: [provider]
        min_org_role: operator
      responses:
        "200": { description: ok }
"#,
        )
        .expect("authz extension imports");
        assert_eq!(
            api.endpoints[0].authz,
            Some(serde_json::json!({
                "org_types": ["provider"],
                "min_org_role": "operator"
            }))
        );
    }

    #[test]
    fn imports_empty_json_text_binary_and_wildcard_responses() {
        let api = crate::import_openapi_yaml_document(include_str!(
            "../../../../examples/http-runtime.yaml"
        ))
        .expect("HTTP runtime fixture should import");
        let mixed = api
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == "getMixed")
            .expect("mixed endpoint should exist");

        assert_eq!(mixed.responses[&200].encoding, ResponseEncoding::Json);
        assert_eq!(mixed.responses[&202].encoding, ResponseEncoding::Text);
        assert_eq!(mixed.responses[&204], Response::empty());
        assert!(mixed.wildcard_error.is_some());
        assert_eq!(mixed.wildcard_error_encoding, ResponseEncoding::Json);

        let download = api
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == "download")
            .expect("download endpoint should exist");
        assert_eq!(download.responses[&200].encoding, ResponseEncoding::Binary);
    }

    const WEBSOCKET_SPEC_HEADER: &str = r#"
openapi: 3.1.0
info: { title: WS, version: "1.0.0" }
components:
  schemas:
    ClientMessage:
      type: object
      required: [text]
      properties: { text: { type: string } }
    ServerMessage:
      type: object
      required: [text]
      properties: { text: { type: string } }
paths:
"#;

    #[test]
    fn imports_bidirectional_websocket_channel() {
        let yaml = format!(
            "{WEBSOCKET_SPEC_HEADER}  /chat/ws:\n    x-tokyo-websocket:\n      id: chat\n      direction: bidirectional\n      send:\n        $ref: '#/components/schemas/ClientMessage'\n      receive:\n        $ref: '#/components/schemas/ServerMessage'\n"
        );
        let api =
            crate::import_openapi_yaml_document(&yaml).expect("websocket channel should import");
        assert!(api.endpoints.is_empty());
        let channel = api
            .channels
            .first()
            .expect("one channel should be imported");
        assert_eq!(channel.name, "chat");
        assert_eq!(channel.path, "/chat/ws");
        match &channel.direction {
            WebSocketDirection::Bidirectional { send, receive } => {
                assert_eq!(
                    *send,
                    TypeRef::Named(tokyo_ir::id::TypeId("ClientMessage".into()))
                );
                assert_eq!(
                    *receive,
                    TypeRef::Named(tokyo_ir::id::TypeId("ServerMessage".into()))
                );
            }
            other => panic!("expected bidirectional direction, got {other:?}"),
        }
    }

    #[test]
    fn rejects_websocket_channel_with_http_methods() {
        let yaml = format!(
            "{WEBSOCKET_SPEC_HEADER}  /chat/ws:\n    x-tokyo-websocket:\n      direction: server_to_client\n      receive:\n        $ref: '#/components/schemas/ServerMessage'\n    get:\n      operationId: getChat\n      responses:\n        \"200\": {{ description: ok }}\n"
        );
        let error = crate::import_openapi_yaml_document(&yaml)
            .expect_err("mixing WS and HTTP should be rejected");
        assert!(matches!(error, ImportError::Unsupported(_)));
    }

    #[test]
    fn rejects_client_to_server_channel_with_receive_field() {
        let yaml = format!(
            "{WEBSOCKET_SPEC_HEADER}  /chat/ws:\n    x-tokyo-websocket:\n      direction: client_to_server\n      send:\n        $ref: '#/components/schemas/ClientMessage'\n      receive:\n        $ref: '#/components/schemas/ServerMessage'\n"
        );
        let error = crate::import_openapi_yaml_document(&yaml)
            .expect_err("client_to_server with receive should be rejected");
        assert!(matches!(error, ImportError::Unsupported(_)));
    }
}
