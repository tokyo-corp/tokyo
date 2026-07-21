//! OpenAPI importer that converts JSON or YAML specifications into Tokyo IR.
//!
//! The importer normalizes supported OpenAPI 3.0 shapes to the 3.1-compatible
//! model consumed by `oas3`, preserves generator-specific extension metadata,
//! and validates the resulting IR before returning it.

mod error;
mod naming;
mod normalize;
mod operation;
mod schema;
mod security;

use std::collections::{BTreeMap, HashMap, HashSet};

use tokyo_ir::Api;
use tokyo_ir::api::OmissionMetadata;
use tokyo_ir::http::HttpMethod;

pub use error::ImportError;

/// Imports an OpenAPI document encoded as JSON.
///
/// # Errors
///
/// Returns [`ImportError`] if the JSON cannot be decoded, `$ref` resolution
/// fails, the specification uses unsupported constructs, or the produced IR
/// violates generator invariants.
#[must_use = "the imported API or import error should be handled"]
pub fn import_openapi_json_document(text: &str) -> Result<Api, ImportError> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    import_openapi_json_value(value)
}

/// Imports an OpenAPI document encoded as YAML.
///
/// # Errors
///
/// Returns [`ImportError`] if the YAML cannot be decoded, `$ref` resolution
/// fails, the specification uses unsupported constructs, or the produced IR
/// violates generator invariants.
#[must_use = "the imported API or import error should be handled"]
pub fn import_openapi_yaml_document(text: &str) -> Result<Api, ImportError> {
    let normalized = normalize_oversized_yaml_integers(text);
    let value: serde_json::Value = yaml_serde::from_str(&normalized)?;
    import_openapi_json_value(value)
}

fn normalize_oversized_yaml_integers(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let line_without_newline = line.strip_suffix('\n').unwrap_or(line);
        let normalized = line_without_newline
            .split_once(':')
            .and_then(|(prefix, remainder)| {
                let value = remainder.trim();
                let (number, comment) = value
                    .split_once(" #")
                    .map_or((value, ""), |(number, comment)| (number.trim(), comment));
                let is_integer = !number.is_empty()
                    && number
                        .strip_prefix('-')
                        .unwrap_or(number)
                        .chars()
                        .all(|character| character.is_ascii_digit());
                let fits_json_integer =
                    number.parse::<i64>().is_ok() || number.parse::<u64>().is_ok();
                if !is_integer || fits_json_integer {
                    return None;
                }
                let float = number
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())?;
                let indentation = &remainder[..remainder.len() - remainder.trim_start().len()];
                let suffix = if comment.is_empty() {
                    String::new()
                } else {
                    format!(" #{comment}")
                };
                Some(format!("{prefix}:{indentation}{float:e}{suffix}"))
            })
            .unwrap_or_else(|| line_without_newline.to_string());
        output.push_str(&normalized);
        if line.ends_with('\n') {
            output.push('\n');
        }
    }
    output
}

fn import_openapi_json_value(value: serde_json::Value) -> Result<Api, ImportError> {
    let mut upgraded = normalize::upgrade_3_0_to_3_1(value);
    let operation_security = operation_security_fields(&upgraded);
    let oauth_token_endpoints = preserve_oauth_token_endpoints(&mut upgraded);
    let schema_components = upgraded
        .pointer("/components/schemas")
        .and_then(serde_json::Value::as_object)
        .map(|schemas| {
            schemas
                .iter()
                .map(|(name, schema)| (name.clone(), schema.clone()))
                .collect()
        })
        .unwrap_or_default();
    let json = serde_json::to_string(&upgraded)?;
    let spec = oas3::from_json(&json)?;
    convert_openapi_spec_to_api_ir(
        &spec,
        operation_security,
        oauth_token_endpoints,
        schema_components,
    )
}

fn convert_openapi_spec_to_api_ir(
    spec: &oas3::Spec,
    operation_security: HashSet<(String, String)>,
    oauth_token_endpoints: HashMap<String, String>,
    schema_components: BTreeMap<String, serde_json::Value>,
) -> Result<Api, ImportError> {
    let mut ctx = schema::Context::new(spec, operation_security, oauth_token_endpoints);
    schema::declare_openapi_component_schemas_as_ir_types(&mut ctx)?;
    let endpoints = operation::convert_openapi_paths_to_endpoints(&mut ctx)?;
    let channels = operation::convert_channels(&mut ctx)?;
    let webhook_handlers = webhook_handler_counts(spec);

    let base_url = spec
        .primary_server()
        .map(|server| {
            let mut url = server.url.clone();
            for (name, variable) in &server.variables {
                url = url.replace(&format!("{{{name}}}"), &variable.default);
            }
            url
        })
        .filter(|url| url != "/");
    let api = Api {
        types: ctx.into_declarations(),
        endpoints,
        channels,
        omissions: OmissionMetadata { webhook_handlers },
        cli: tokyo_ir::CliBehavior {
            base_url: base_url.clone(),
            ..Default::default()
        },
        sdk: tokyo_ir::SdkBehavior {
            base_url,
            ..Default::default()
        },
        schema_components,
        ..Default::default()
    };
    api.validate().map_err(|errors| {
        ImportError::Unsupported(format!(
            "imported OpenAPI violates IR invariants: {}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        ))
    })?;
    Ok(api)
}

fn webhook_handler_counts(spec: &oas3::Spec) -> BTreeMap<HttpMethod, u32> {
    let mut counts = BTreeMap::new();
    for path_item in spec.webhooks.values() {
        for (method, _) in path_item.methods() {
            let method = match method.as_str() {
                "GET" => HttpMethod::Get,
                "HEAD" => HttpMethod::Head,
                "POST" => HttpMethod::Post,
                "PUT" => HttpMethod::Put,
                "PATCH" => HttpMethod::Patch,
                "DELETE" => HttpMethod::Delete,
                "OPTIONS" => HttpMethod::Options,
                "TRACE" => HttpMethod::Trace,
                _ => continue,
            };
            *counts.entry(method).or_default() += 1;
        }
    }
    counts
}

fn preserve_oauth_token_endpoints(value: &mut serde_json::Value) -> HashMap<String, String> {
    let mut endpoints = HashMap::new();
    let Some(schemes) = value
        .get_mut("components")
        .and_then(|components| components.get_mut("securitySchemes"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return endpoints;
    };
    for (scheme_name, scheme) in schemes {
        let Some(token_url) = scheme
            .get_mut("flows")
            .and_then(|flows| flows.get_mut("clientCredentials"))
            .and_then(|flow| flow.get_mut("tokenUrl"))
            .and_then(|value| value.as_str())
            .map(str::to_owned)
        else {
            continue;
        };
        endpoints.insert(scheme_name.clone(), token_url.clone());
        if !is_absolute_uri(&token_url)
            && let Some(value) = scheme
                .get_mut("flows")
                .and_then(|flows| flows.get_mut("clientCredentials"))
                .and_then(|flow| flow.get_mut("tokenUrl"))
        {
            *value = serde_json::Value::String(
                "https://tokyo.invalid/oauth-token-placeholder".to_string(),
            );
        }
    }
    endpoints
}

fn is_absolute_uri(value: &str) -> bool {
    let Some(colon) = value.find(':') else {
        return false;
    };
    let scheme = &value[..colon];
    !scheme.is_empty()
        && scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || (index > 0 && (byte.is_ascii_digit() || b"+-.".contains(&byte)))
        })
}

/// `oas3::Operation` stores `security` as a defaulted Vec and therefore cannot
/// distinguish an omitted field (inherit root security) from an explicit empty
/// array (override root security with no authentication). Preserve that bit
/// while the source is still raw JSON.
fn operation_security_fields(value: &serde_json::Value) -> HashSet<(String, String)> {
    const METHODS: &[&str] = &[
        "get", "head", "post", "put", "patch", "delete", "options", "trace",
    ];
    let mut fields = HashSet::new();
    let Some(paths) = value.get("paths").and_then(serde_json::Value::as_object) else {
        return fields;
    };
    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for method in METHODS {
            if item
                .get(*method)
                .and_then(serde_json::Value::as_object)
                .is_some_and(|operation| operation.contains_key("security"))
            {
                fields.insert((path.clone(), (*method).to_string()));
            }
        }
    }
    fields
}
