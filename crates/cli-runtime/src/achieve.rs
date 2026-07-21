//! Spec-driven goal inference: discovers "create"/finalize outcomes from the
//! embedded CLI schema and synthesizes request bodies for them. Every rule in
//! this module must be derivable from any OpenAPI-shaped schema — nothing in
//! here may reference a specific API's field names or vocabulary.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::ClientError;

/// Verbs that mark a POST command as a creation outcome.
const CREATE_VERBS: &[&str] = &[
    "create", "register", "open", "draft", "build", "add", "new", "generate",
];

/// Verbs that mark a POST/PATCH command as a finalize outcome (the step that
/// promotes a created draft into its live state). The matched verb becomes
/// the capability's action, so the CLI surface mirrors the API's own word.
const FINALIZE_VERBS: &[&str] = &[
    "submit", "stage", "publish", "finalize", "activate", "approve",
];

/// Property names that accept natural-language intent. `--prompt` lands in
/// the first of these present in the request schema.
const PROMPT_FIELDS: &[&str] = &["prompt", "text", "instructions", "message"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Goal-oriented command capability inferred from the generated CLI schema.
pub struct Capability {
    /// Action verb, such as `create` or `publish`.
    pub action: String,
    /// Resource name the action targets.
    pub resource: String,
    /// Concrete generated CLI invocation.
    pub invocation: String,
    /// Human-facing capability summary.
    pub description: String,
}

/// Infers high-level capabilities from the generated CLI schema.
#[must_use]
pub fn infer_achievable_capabilities_from_schema(schema_json: &str) -> Vec<Capability> {
    let Ok(schema) = serde_json::from_str::<Value>(schema_json) else {
        return Vec::new();
    };
    let mut selected = BTreeMap::<(String, String), (u32, Capability)>::new();
    for resource in schema
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(resource_name) = resource.get("name").and_then(Value::as_str) else {
            continue;
        };
        for command in resource
            .get("commands")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let method = command.get("method").and_then(Value::as_str).unwrap_or("");
            let name = command.get("name").and_then(Value::as_str).unwrap_or("");
            let summary = command.get("summary").and_then(Value::as_str).unwrap_or("");
            let tokens = tokenize_command_verb_words(name, summary);
            let (action, score) =
                if method == "POST" && CREATE_VERBS.iter().any(|verb| tokens.contains(*verb)) {
                    // Among creation candidates for one resource, prefer the one
                    // with the richest request contract: a command that accepts a
                    // populated body creates a more complete resource than a bare
                    // "open an empty shell" endpoint.
                    let rich = command
                        .get("request_schema")
                        .is_some_and(schema_is_populated);
                    (
                        "create",
                        u32::from(rich) * 10_000 + 100 + 99 - name.len().min(99) as u32,
                    )
                } else if matches!(method, "POST" | "PATCH")
                    && let Some(verb) = FINALIZE_VERBS.iter().find(|verb| tokens.contains(**verb))
                {
                    (*verb, 100 + 99 - name.len().min(99) as u32)
                } else {
                    continue;
                };
            let target = capability_resource(resource_name);
            let capability = Capability {
                action: action.to_string(),
                resource: target.clone(),
                invocation: command
                    .get("invocation")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                description: if summary.is_empty() {
                    format!("{action} {target}")
                } else {
                    summary.to_string()
                },
            };
            let key = (action.to_string(), target);
            let replace = selected
                .get(&key)
                .is_none_or(|(current_score, _)| score > *current_score);
            if replace {
                selected.insert(key, (score, capability));
            }
        }
    }
    selected
        .into_values()
        .map(|(_, capability)| capability)
        .collect()
}

/// Renders a synthesized request body as CLI arguments for the capability's
/// generated command: per-field flags when the command flattens its body
/// (`body_mode: flattened_flags`), `--body-json` otherwise. Null fields are
/// omitted; boolean `true` becomes a bare flag.
#[must_use]
pub fn body_invocation_arguments(
    schema_json: &str,
    capability: &Capability,
    body: &Value,
) -> Vec<String> {
    let flattened = serde_json::from_str::<Value>(schema_json)
        .ok()
        .and_then(|schema| {
            schema
                .get("resources")?
                .as_array()?
                .iter()
                .flat_map(|resource| {
                    resource
                        .get("commands")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .find(|command| {
                    command.get("invocation").and_then(Value::as_str)
                        == Some(capability.invocation.as_str())
                })
                .map(|command| {
                    command.get("body_mode").and_then(Value::as_str) == Some("flattened_flags")
                })
        })
        .unwrap_or(false);
    if !flattened {
        return vec![
            "--body-json".to_string(),
            serde_json::to_string(body).expect("JSON body serializes"),
        ];
    }
    let mut arguments = Vec::new();
    for (field, value) in body.as_object().into_iter().flatten() {
        match value {
            Value::Null | Value::Bool(false) => {}
            Value::Bool(true) => arguments.push(format!("--{field}")),
            Value::String(text) => {
                arguments.push(format!("--{field}"));
                arguments.push(text.clone());
            }
            other => {
                arguments.push(format!("--{field}"));
                arguments.push(other.to_string());
            }
        }
    }
    arguments
}

/// Finds one inferred capability by action and resource.
///
/// # Errors
///
/// Returns [`ClientError`] if the requested goal is not supported by the schema.
#[must_use = "capability lookup errors should be handled"]
pub fn find_capability(
    schema_json: &str,
    action: &str,
    resource: &str,
) -> Result<Capability, ClientError> {
    let normalized = resource.trim().to_ascii_lowercase().replace('_', "-");
    infer_achievable_capabilities_from_schema(schema_json)
        .into_iter()
        .find(|capability| capability.action == action && capability.resource == normalized)
        .ok_or_else(|| {
            let available = infer_achievable_capabilities_from_schema(schema_json)
                .into_iter()
                .map(|capability| format!("{} {}", capability.action, capability.resource))
                .collect::<Vec<_>>()
                .join(", ");
            ClientError::Decode(format!(
                "unsupported goal {action} {resource}; available goals: {available}"
            ))
        })
}

/// The finalize outcome for a resource, whatever verb the API uses for it.
pub fn finalize_capability(schema_json: &str, resource: &str) -> Result<Capability, ClientError> {
    infer_achievable_capabilities_from_schema(schema_json)
        .into_iter()
        .find(|capability| {
            capability.resource == resource && FINALIZE_VERBS.contains(&capability.action.as_str())
        })
        .ok_or_else(|| {
            ClientError::Decode(format!(
                "no finalize outcome (submit/stage/publish/...) exists for {resource}"
            ))
        })
}

/// Synthesizes a request body for an inferred capability.
///
/// # Errors
///
/// Returns [`ClientError`] if the embedded schema is invalid, field overlays
/// are invalid, or `--prompt` is requested for a capability without a prompt
/// field.
#[must_use = "synthesized request body or errors should be handled"]
pub fn synthesize_achieve_request_body(
    schema_json: &str,
    capability: &Capability,
    index: usize,
    overrides: &[String],
    context: &BTreeMap<String, Value>,
    prompt: Option<&str>,
) -> Result<Option<Value>, ClientError> {
    let schema: Value = serde_json::from_str(schema_json)
        .map_err(|error| ClientError::Decode(format!("invalid embedded CLI schema: {error}")))?;
    let command =
        find_schema_command_by_invocation(&schema, &capability.invocation).ok_or_else(|| {
            ClientError::Decode("capability command is absent from schema".to_string())
        })?;
    let Some(request_schema) = command
        .get("request_schema")
        .filter(|value| !value.is_null())
    else {
        if overrides.is_empty() {
            return Ok(None);
        }
        return Ok(Some(
            crate::body::parse_json_request_body_from_field_assignments(overrides)?,
        ));
    };
    let components = schema
        .pointer("/components/schemas")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut body = synthesize_json_value_from_schema(
        request_schema,
        &components,
        index,
        "body",
        &capability.resource,
        context,
        prompt.is_some(),
    )?;
    if let Some(prompt) = prompt {
        let placed = PROMPT_FIELDS
            .iter()
            .any(|field| replace_first_key(&mut body, field, Value::String(prompt.to_string())));
        if !placed {
            return Err(ClientError::Decode(format!(
                "{} {} does not support --prompt",
                capability.action, capability.resource
            )));
        }
    }
    if !overrides.is_empty() {
        merge_json_overlay_into_target(
            &mut body,
            crate::body::parse_json_request_body_from_field_assignments(overrides)?,
        );
    }
    Ok(Some(body))
}

/// Returns whether a capability's request schema accepts prompt text.
#[must_use]
pub fn supports_prompt(schema_json: &str, capability: &Capability) -> bool {
    let Ok(schema) = serde_json::from_str::<Value>(schema_json) else {
        return false;
    };
    let components = schema
        .pointer("/components/schemas")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    find_schema_command_by_invocation(&schema, &capability.invocation)
        .and_then(|command| command.get("request_schema"))
        .is_some_and(|request| has_prompt_field(request, &components))
}

/// Request-schema fields that reference other resources: any `*_id` property
/// without a const/default/example. These are the values `achieve` tries to
/// resolve from the caller's identity and cheap list lookups before creating.
pub fn relationship_fields(schema_json: &str, capability: &Capability) -> BTreeSet<String> {
    let Ok(schema) = serde_json::from_str::<Value>(schema_json) else {
        return BTreeSet::new();
    };
    let components = schema
        .pointer("/components/schemas")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let Some(request) = find_schema_command_by_invocation(&schema, &capability.invocation)
        .and_then(|command| command.get("request_schema"))
    else {
        return BTreeSet::new();
    };
    let mut fields = BTreeSet::new();
    collect_relationship_fields(request, &components, &mut BTreeSet::new(), &mut fields);
    fields
}

/// Whether every relationship field is resolvable: present in context, or a
/// member of its conditional group is. Lets the executor stop looking up
/// providers as soon as one branch of each either/or pair is satisfied.
pub fn context_complete(
    schema_json: &str,
    capability: &Capability,
    context: &BTreeMap<String, Value>,
) -> bool {
    let wanted = relationship_fields(schema_json, capability);
    let Ok(schema) = serde_json::from_str::<Value>(schema_json) else {
        return false;
    };
    let components = schema
        .pointer("/components/schemas")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut groups = Vec::new();
    if let Some(request) = find_schema_command_by_invocation(&schema, &capability.invocation)
        .and_then(|command| command.get("request_schema"))
    {
        collect_conditional_groups(request, &components, &mut BTreeSet::new(), &mut groups);
    }
    wanted.iter().all(|field| {
        context.contains_key(field)
            || groups.iter().any(|group| {
                group.contains(field) && group.iter().any(|member| context.contains_key(member))
            })
    })
}

/// Returns lookup command invocations that can satisfy relationship fields.
#[must_use]
pub fn relationship_lookups(schema_json: &str, capability: &Capability) -> Vec<String> {
    let Ok(schema) = serde_json::from_str::<Value>(schema_json) else {
        return Vec::new();
    };
    let wanted = relationship_fields(schema_json, capability);
    if wanted.is_empty() {
        return Vec::new();
    }
    let components = schema
        .pointer("/components/schemas")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut candidates = schema
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|resource| {
            resource
                .get("commands")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|command| {
            command.get("method").and_then(Value::as_str) == Some("GET")
                && command
                    .get("path_params")
                    .and_then(Value::as_array)
                    .is_none_or(Vec::is_empty)
        })
        .filter_map(|command| {
            let invocation = command.get("invocation").and_then(Value::as_str)?;
            let responses = command.get("response_schemas")?;
            let covered = wanted
                .iter()
                .filter(|field| {
                    schema_contains_property(responses, field, &components, &mut BTreeSet::new())
                        || (provider_name_matches(invocation, field)
                            && has_identifier_property(responses, &components))
                })
                .cloned()
                .collect::<BTreeSet<_>>();
            let relevance = covered
                .iter()
                .flat_map(|field| field.trim_end_matches("_id").split('_'))
                .filter(|token| token.len() >= 3)
                .filter(|token| invocation.contains(token))
                .count();
            (!covered.is_empty())
                .then(|| (covered.len() * 10 + relevance * 100, invocation.to_string()))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    candidates
        .into_iter()
        .map(|(_, invocation)| invocation)
        .collect()
}

/// Collects relationship field values from a JSON response.
pub fn collect_context(
    value: &Value,
    wanted: &BTreeSet<String>,
    context: &mut BTreeMap<String, Value>,
) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if wanted.contains(key) && !value.is_null() {
                    context.entry(key.clone()).or_insert_with(|| value.clone());
                }
                collect_context(value, wanted, context);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_context(value, wanted, context);
            }
        }
        _ => {}
    }
}

/// Collects relationship context from a lookup response and provider command.
pub fn collect_lookup_context(
    value: &Value,
    wanted: &BTreeSet<String>,
    invocation: &str,
    context: &mut BTreeMap<String, Value>,
) {
    collect_context(value, wanted, context);
    let Some(identifier) =
        first_named_value(value, "_id").or_else(|| first_named_value(value, "id"))
    else {
        return;
    };
    for field in wanted {
        if provider_name_matches(invocation, field) {
            context
                .entry(field.clone())
                .or_insert_with(|| identifier.clone());
        }
    }
}

/// Collects resource identifier values from a JSON value.
pub fn collect_resource_identifiers_from_value(
    value: &Value,
    resource: &str,
    ids: &mut BTreeMap<String, Vec<Value>>,
) {
    let mut found = false;
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            if (key == "id" || key.ends_with("_id")) && !value.is_null() {
                ids.entry(pluralize_resource_key(key))
                    .or_default()
                    .push(value.clone());
                found = true;
            }
        }
    }
    if !found && (value.is_string() || value.is_number()) {
        ids.entry(format!("{}_ids", resource.replace('-', "_")))
            .or_default()
            .push(value.clone());
    }
}

/// The identifier a creation response assigned to the new resource, for
/// chaining into a finalize call: `id`, then `<resource>_id` at decreasing
/// specificity (`shipping_order_id`, `order_id`).
pub fn extract_created_resource_identifier(value: &Value, resource: &str) -> Option<String> {
    let snake = resource.replace('-', "_");
    let mut keys = vec!["id".to_string(), format!("{snake}_id")];
    if let Some(last) = snake.rsplit('_').next() {
        keys.push(format!("{last}_id"));
    }
    keys.iter().find_map(|key| {
        let value = value.get(key)?;
        value
            .as_str()
            .map(str::to_string)
            .or_else(|| value.as_number().map(|number| number.to_string()))
    })
}

fn tokenize_command_verb_words(name: &str, summary: &str) -> BTreeSet<String> {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .chain(summary.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// A request schema that actually carries data: it has (or references)
/// properties, rather than being a bare `{"type": "object"}` shell.
fn schema_is_populated(schema: &Value) -> bool {
    match schema {
        Value::Object(object) => {
            object.contains_key("$ref")
                || object
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|properties| !properties.is_empty())
                || ["anyOf", "oneOf", "allOf"].iter().any(|keyword| {
                    object
                        .get(*keyword)
                        .and_then(Value::as_array)
                        .is_some_and(|options| options.iter().any(schema_is_populated))
                })
        }
        _ => false,
    }
}

fn find_schema_command_by_invocation<'a>(schema: &'a Value, invocation: &str) -> Option<&'a Value> {
    schema
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|resource| {
            resource
                .get("commands")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .find(|command| command.get("invocation").and_then(Value::as_str) == Some(invocation))
}

fn collect_relationship_fields(
    schema: &Value,
    components: &Map<String, Value>,
    visited: &mut BTreeSet<String>,
    fields: &mut BTreeSet<String>,
) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or(reference);
        if visited.insert(name.to_string())
            && let Some(resolved) = components.get(name)
        {
            collect_relationship_fields(resolved, components, visited, fields);
        }
        return;
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, property) in properties {
            if name.ends_with("_id") && !has_explicit_value(property) {
                fields.insert(name.clone());
            }
            collect_relationship_fields(property, components, visited, fields);
        }
    }
    for keyword in ["anyOf", "oneOf", "allOf"] {
        for option in schema
            .get(keyword)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            collect_relationship_fields(option, components, visited, fields);
        }
    }
}

/// Sibling properties whose descriptions declare conditional requirement
/// ("Required if ..." — the phrasing real specs use for either/or pairs)
/// form one exclusive group per object level: exactly one member should be
/// sent.
fn collect_conditional_groups(
    schema: &Value,
    components: &Map<String, Value>,
    visited: &mut BTreeSet<String>,
    groups: &mut Vec<BTreeSet<String>>,
) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or(reference);
        if visited.insert(name.to_string())
            && let Some(resolved) = components.get(name)
        {
            collect_conditional_groups(resolved, components, visited, groups);
        }
        return;
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        let group = properties
            .iter()
            .filter(|(_, property)| schema_property_is_conditional(property))
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        if !group.is_empty() {
            groups.push(group);
        }
        for property in properties.values() {
            collect_conditional_groups(property, components, visited, groups);
        }
    }
    for keyword in ["anyOf", "oneOf", "allOf"] {
        for option in schema
            .get(keyword)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            collect_conditional_groups(option, components, visited, groups);
        }
    }
}

fn schema_property_is_conditional(property: &Value) -> bool {
    property
        .get("description")
        .and_then(Value::as_str)
        .is_some_and(|description| description.to_ascii_lowercase().contains("required if"))
}

fn has_explicit_value(property: &Value) -> bool {
    ["const", "default", "example"]
        .iter()
        .any(|key| property.get(*key).is_some())
}

fn has_prompt_field(schema: &Value, components: &Map<String, Value>) -> bool {
    PROMPT_FIELDS
        .iter()
        .any(|field| schema_contains_property(schema, field, components, &mut BTreeSet::new()))
}

fn has_identifier_property(schema: &Value, components: &Map<String, Value>) -> bool {
    ["_id", "id"]
        .iter()
        .any(|field| schema_contains_property(schema, field, components, &mut BTreeSet::new()))
}

/// Does a lookup command's resource plausibly provide values for this field?
/// True when every meaningful token of the field's stem appears in the
/// resource name (`driver_id` ← `drivers list`).
fn provider_name_matches(invocation: &str, field: &str) -> bool {
    let resource = invocation.split_whitespace().next().unwrap_or("");
    field
        .trim_end_matches("_id")
        .split('_')
        .filter(|token| token.len() >= 3)
        .all(|token| resource.contains(token))
}

fn first_named_value(value: &Value, name: &str) -> Option<Value> {
    match value {
        Value::Object(object) => object
            .get(name)
            .filter(|value| !value.is_null())
            .cloned()
            .or_else(|| {
                object
                    .values()
                    .find_map(|value| first_named_value(value, name))
            }),
        Value::Array(values) => values
            .iter()
            .find_map(|value| first_named_value(value, name)),
        _ => None,
    }
}

fn schema_contains_property(
    schema: &Value,
    field: &str,
    components: &Map<String, Value>,
    visited: &mut BTreeSet<String>,
) -> bool {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or(reference);
        return visited.insert(name.to_string())
            && components.get(name).is_some_and(|resolved| {
                schema_contains_property(resolved, field, components, visited)
            });
    }
    if schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| properties.contains_key(field))
    {
        return true;
    }
    match schema {
        Value::Object(object) => object
            .values()
            .any(|value| schema_contains_property(value, field, components, visited)),
        Value::Array(values) => values
            .iter()
            .any(|value| schema_contains_property(value, field, components, visited)),
        _ => false,
    }
}

/// A draft collection creates its parent resource (`orders-drafts` creates an
/// `order`), and goals are phrased against the singular resource.
fn capability_resource(resource: &str) -> String {
    let mut parts = resource.split('-').collect::<Vec<_>>();
    if matches!(parts.last(), Some(&"drafts") | Some(&"draft")) {
        parts.pop();
    }
    if let Some(last) = parts.last_mut()
        && last.ends_with('s')
        && last.len() > 1
    {
        *last = &last[..last.len() - 1];
    }
    parts.join("-")
}

fn synthesize_json_value_from_schema(
    schema: &Value,
    components: &Map<String, Value>,
    index: usize,
    path: &str,
    resource: &str,
    context: &BTreeMap<String, Value>,
    prompt_active: bool,
) -> Result<Value, ClientError> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or(reference);
        let resolved = components.get(name).ok_or_else(|| {
            ClientError::Decode(format!("unresolved request-schema reference {reference}"))
        })?;
        return synthesize_json_value_from_schema(
            resolved,
            components,
            index,
            path,
            resource,
            context,
            prompt_active,
        );
    }
    for keyword in ["const", "default", "example"] {
        if let Some(value) = schema.get(keyword) {
            return Ok(value.clone());
        }
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return Ok(value.clone());
    }
    for keyword in ["anyOf", "oneOf"] {
        if let Some(options) = schema.get(keyword).and_then(Value::as_array)
            && let Some(option) = options
                .iter()
                .find(|option| option.get("type").and_then(Value::as_str) != Some("null"))
        {
            return synthesize_json_value_from_schema(
                option,
                components,
                index,
                path,
                resource,
                context,
                prompt_active,
            );
        }
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") | None if schema.get("properties").is_some() => {
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>();
            let mut object = Map::new();
            let mut conditional = Vec::new();
            for (name, property) in properties {
                if schema_property_is_conditional(&property) {
                    conditional.push((name, property));
                    continue;
                }
                // `achieve` executes buffered requests, so any streaming
                // toggle in the body is pinned off regardless of its default.
                if name == "stream" || name.starts_with("stream_") {
                    object.insert(name, Value::Bool(false));
                } else if let Some(value) = context.get(&name) {
                    object.insert(name, value.clone());
                } else if required.contains(name.as_str())
                    || has_explicit_value(&property)
                    || PROMPT_FIELDS.contains(&name.as_str())
                {
                    object.insert(
                        name.clone(),
                        synthesize_json_value_from_schema(
                            &property,
                            components,
                            index,
                            &format!("{path}.{name}"),
                            resource,
                            context,
                            prompt_active,
                        )?,
                    );
                }
            }
            // Exclusive group: send exactly one member. A prompt selects the
            // member that can carry it; otherwise context decides; otherwise
            // the first member we can synthesize.
            let prompt_member = prompt_active
                .then(|| {
                    conditional
                        .iter()
                        .position(|(_, property)| has_prompt_field(property, components))
                })
                .flatten();
            if let Some(position) = prompt_member {
                let (name, property) = &conditional[position];
                let value = synthesize_json_value_from_schema(
                    property,
                    components,
                    index,
                    &format!("{path}.{name}"),
                    resource,
                    context,
                    prompt_active,
                )?;
                object.insert(name.clone(), value);
            } else if let Some((name, _)) = conditional
                .iter()
                .find(|(name, _)| context.contains_key(name))
            {
                object.insert(name.clone(), context[name].clone());
            } else {
                for (name, property) in &conditional {
                    if let Ok(value) = synthesize_json_value_from_schema(
                        property,
                        components,
                        index,
                        &format!("{path}.{name}"),
                        resource,
                        context,
                        prompt_active,
                    ) {
                        object.insert(name.clone(), value);
                        break;
                    }
                }
            }
            Ok(Value::Object(object))
        }
        Some("array") => Ok(Value::Array(Vec::new())),
        Some("string") => {
            let leaf = path.rsplit('.').next().unwrap_or("value");
            if schema.get("format").and_then(Value::as_str) == Some("email")
                || leaf.contains("email")
            {
                Ok(Value::String(format!("generated-{index}@example.com")))
            } else if PROMPT_FIELDS.contains(&leaf) {
                Ok(Value::String(format!(
                    "Create a complete, valid example {} with realistic details.",
                    resource.replace('-', " ")
                )))
            } else if leaf.contains("name") {
                Ok(Value::String(format!("Generated {index}")))
            } else {
                Err(ClientError::Decode(format!(
                    "goal needs --set {path}=VALUE; no safe value can be inferred"
                )))
            }
        }
        Some("integer") => Ok(Value::from(
            schema.get("minimum").and_then(Value::as_i64).unwrap_or(1),
        )),
        Some("number") => Ok(Value::from(
            schema.get("minimum").and_then(Value::as_f64).unwrap_or(1.0),
        )),
        Some("boolean") => Ok(Value::Bool(false)),
        other => Err(ClientError::Decode(format!(
            "cannot synthesize {path} from schema type {other:?}"
        ))),
    }
}

fn merge_json_overlay_into_target(target: &mut Value, overlay: Value) {
    match (target, overlay) {
        (Value::Object(target), Value::Object(overlay)) => {
            for (key, value) in overlay {
                merge_json_overlay_into_target(target.entry(key).or_insert(Value::Null), value);
            }
        }
        (target, overlay) => *target = overlay,
    }
}

fn replace_first_key(value: &mut Value, name: &str, replacement: Value) -> bool {
    match value {
        Value::Object(object) => {
            if let Some(value) = object.get_mut(name) {
                *value = replacement;
                true
            } else {
                object
                    .values_mut()
                    .any(|value| replace_first_key(value, name, replacement.clone()))
            }
        }
        Value::Array(values) => values
            .iter_mut()
            .any(|value| replace_first_key(value, name, replacement.clone())),
        _ => false,
    }
}

fn pluralize_resource_key(key: &str) -> String {
    if key.ends_with('s') {
        key.to_string()
    } else {
        format!("{key}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r##"{
      "resources": [{
        "name": "shipping-orders-drafts",
        "commands": [{
          "name": "post-shipping-orders-drafts-open-v1",
          "invocation": "shipping-orders-drafts post-shipping-orders-drafts-open-v1",
          "method": "POST",
          "summary": "Open draft",
          "description": "Opens a new draft order shell",
          "request_schema": {"$ref":"#/components/schemas/Open"}
        }]
      }],
      "components": {"schemas": {"Open": {
        "type":"object",
        "properties":{"items":{"type":"array","default":[]}}
      }}}
    }"##;

    const BUILDER_SCHEMA: &str = r##"{
      "resources": [
        {"name":"shipping-orders-drafts","commands":[
          {"name":"open","invocation":"shipping-orders-drafts open","method":"POST",
           "summary":"Open draft","description":"Opens a new draft order shell","request_schema":{"type":"object"}},
          {"name":"build","invocation":"shipping-orders-drafts build","method":"POST",
           "summary":"Agentic draft","description":"Creates a populated draft",
           "request_schema":{"$ref":"#/components/schemas/Builder"}}
        ]},
        {"name":"connections","commands":[
          {"name":"list-shippers","invocation":"connections list-shippers","method":"GET",
           "path_params":[],"response_schemas":{"200":{"$ref":"#/components/schemas/Connections"}}}
        ]},
        {"name":"shipping-orders","commands":[
          {"name":"stage","invocation":"shipping-orders stage","method":"PATCH",
           "summary":"Stage","description":"Stage order","request_schema":null}
        ]}
      ],
      "components":{"schemas":{
        "Builder":{"type":"object","properties":{
          "order_template_core":{"description":"Required if order_template_id is omitted.",
            "$ref":"#/components/schemas/Core"},
          "order_template_id":{"description":"Required if order_template_core is omitted.","type":"string"},
          "stream_updates":{"type":"boolean","default":true}
        }},
        "Core":{"type":"object","properties":{
          "schema_version":{"type":"integer","const":1},
          "coordinator_org_id":{"type":"string"},
          "off_chrt_shipper_org_data_id":{"type":"string"},
          "text":{"type":"string"},
          "driver_ids":{"type":"array","default":[]}
        },"required":["schema_version"]},
        "Connections":{"type":"object","properties":{"items":{"type":"array","items":{
          "type":"object","properties":{
            "coordinator_org_id":{"type":"string"},
            "off_chrt_shipper_org_data_id":{"type":"string"}
          }
        }}}}
      }}
    }"##;

    #[test]
    fn infers_create_capability_without_a_scenario() {
        assert_eq!(
            infer_achievable_capabilities_from_schema(SCHEMA),
            vec![Capability {
                action: "create".to_string(),
                resource: "shipping-order".to_string(),
                invocation: "shipping-orders-drafts post-shipping-orders-drafts-open-v1"
                    .to_string(),
                description: "Open draft".to_string(),
            }]
        );
    }

    #[test]
    fn synthesizes_defaults_for_generated_create_body() {
        let capability = infer_achievable_capabilities_from_schema(SCHEMA)
            .pop()
            .expect("capability");
        assert_eq!(
            synthesize_achieve_request_body(SCHEMA, &capability, 1, &[], &BTreeMap::new(), None)
                .expect("body"),
            Some(serde_json::json!({"items":[]}))
        );
    }

    #[test]
    fn prefers_populating_builder_and_discovers_relationship_provider() {
        let capabilities = infer_achievable_capabilities_from_schema(BUILDER_SCHEMA);
        let create = capabilities
            .iter()
            .find(|capability| capability.action == "create")
            .expect("create capability");
        assert_eq!(create.invocation, "shipping-orders-drafts build");
        assert!(capabilities.iter().any(|capability| {
            capability.action == "stage" && capability.resource == "shipping-order"
        }));
        assert_eq!(
            finalize_capability(BUILDER_SCHEMA, "shipping-order")
                .expect("finalize capability")
                .invocation,
            "shipping-orders stage"
        );
        assert_eq!(
            relationship_lookups(BUILDER_SCHEMA, create),
            vec!["connections list-shippers"]
        );

        let wanted = relationship_fields(BUILDER_SCHEMA, create);
        let mut context = BTreeMap::new();
        collect_context(
            &serde_json::json!({
                "coordinator_org_id": "org_1",
                "off_chrt_shipper_org_data_id": "shipper_1"
            }),
            &wanted,
            &mut context,
        );
        let body = synthesize_achieve_request_body(BUILDER_SCHEMA, create, 1, &[], &context, None)
            .expect("body")
            .expect("JSON body");
        assert_eq!(body["stream_updates"], false);
        assert_eq!(body["order_template_core"]["schema_version"], 1);
        assert_eq!(body["order_template_core"]["coordinator_org_id"], "org_1");
        assert_eq!(
            body["order_template_core"]["off_chrt_shipper_org_data_id"],
            "shipper_1"
        );
        assert_eq!(
            body["order_template_core"]["driver_ids"],
            serde_json::json!([])
        );
        assert!(
            body["order_template_core"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("example shipping order"))
        );
        assert!(supports_prompt(BUILDER_SCHEMA, create));
        context.insert(
            "order_template_id".to_string(),
            Value::String("template_1".to_string()),
        );
        let prompted = synthesize_achieve_request_body(
            BUILDER_SCHEMA,
            create,
            1,
            &[],
            &context,
            Some("Move documents from A to B"),
        )
        .expect("prompted body")
        .expect("JSON body");
        assert!(prompted.get("order_template_id").is_none());
        assert_eq!(
            prompted["order_template_core"]["text"],
            "Move documents from A to B"
        );
    }

    #[test]
    fn conditional_groups_satisfy_context_completeness() {
        let create = infer_achievable_capabilities_from_schema(BUILDER_SCHEMA)
            .into_iter()
            .find(|capability| capability.action == "create")
            .expect("create capability");
        let mut context = BTreeMap::new();
        assert!(!context_complete(BUILDER_SCHEMA, &create, &context));
        context.insert("coordinator_org_id".to_string(), Value::from("org_1"));
        context.insert(
            "off_chrt_shipper_org_data_id".to_string(),
            Value::from("s_1"),
        );
        // order_template_id is conditional; any member of its either/or
        // group present in context satisfies the whole group.
        assert!(!context_complete(BUILDER_SCHEMA, &create, &context));
        context.insert("order_template_id".to_string(), Value::from("t_1"));
        assert!(context_complete(BUILDER_SCHEMA, &create, &context));
    }

    #[test]
    fn extracts_created_identifier_at_decreasing_specificity() {
        let response = serde_json::json!({"order_id": "ord_1", "status": "draft"});
        assert_eq!(
            extract_created_resource_identifier(&response, "shipping-order").as_deref(),
            Some("ord_1")
        );
        let response = serde_json::json!({"id": 42});
        assert_eq!(
            extract_created_resource_identifier(&response, "pet").as_deref(),
            Some("42")
        );
        assert_eq!(
            extract_created_resource_identifier(&serde_json::json!({}), "pet"),
            None
        );
    }

    #[test]
    fn body_arguments_flatten_to_flags_or_body_json_by_mode() {
        let schema = r#"{"resources": [{"name": "default", "commands": [
            {"name": "create-pet", "invocation": "default create-pet",
             "body_mode": "flattened_flags"},
            {"name": "create-order", "invocation": "orders create",
             "body_mode": "structured"}
        ]}]}"#;
        let flattened = Capability {
            action: "create".to_string(),
            resource: "pet".to_string(),
            invocation: "default create-pet".to_string(),
            description: String::new(),
        };
        let body = serde_json::json!({
            "name": "Bella", "count": 2, "featured": true,
            "hidden": false, "tag": null,
        });
        // Field ordering depends on serde_json feature flags, so compare as
        // flag/value pairs rather than a fixed sequence.
        let arguments = body_invocation_arguments(schema, &flattened, &body);
        assert_eq!(arguments.len(), 5, "{arguments:?}");
        let name_position = arguments.iter().position(|a| a == "--name").unwrap();
        assert_eq!(arguments[name_position + 1], "Bella");
        let count_position = arguments.iter().position(|a| a == "--count").unwrap();
        assert_eq!(arguments[count_position + 1], "2");
        assert!(arguments.contains(&"--featured".to_string()));
        assert!(!arguments.contains(&"--hidden".to_string()));
        assert!(!arguments.contains(&"--tag".to_string()));

        let structured = Capability {
            invocation: "orders create".to_string(),
            ..flattened
        };
        let arguments = body_invocation_arguments(schema, &structured, &body);
        assert_eq!(arguments[0], "--body-json");
        assert!(arguments[1].contains("\"Bella\""));
    }
}
