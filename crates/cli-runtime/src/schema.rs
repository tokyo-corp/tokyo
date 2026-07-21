//! Splits the generated CLI's baked-in schema JSON into two views so an
//! agent never has to load full descriptions for every command just to find
//! the one it needs: a cheap index (names, methods, paths — no prose) by
//! default, and a full-detail lookup (summary, description, and every
//! parameter's description) for exactly one command when asked by name.

/// Access classification used to filter the generated command index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CommandAccessFilter {
    /// Include every generated command.
    All,
    /// Include operations that never use authentication.
    Public,
    /// Include operations that accept but do not require authentication.
    Optional,
    /// Include operations that require credentials.
    Authenticated,
}

/// Renders the schema view for `command`. `None` returns the lightweight
/// index across every resource; `Some(name)` accepts an exact
/// `"<resource>.<command>"` identifier or a globally unambiguous bare command
/// name or alias and returns that command's full detail.
pub fn render_cli_schema_json_response(
    schema_json: &'static str,
    command: Option<&str>,
) -> Result<serde_json::Value, crate::error::ClientError> {
    let full: serde_json::Value =
        serde_json::from_str(schema_json).expect("generated SCHEMA_JSON is always valid JSON");

    match command {
        Some(id) => resolve_command(&full, id).map(|resolved| {
            let scripting = scripting_recipe_for_command(&resolved);
            let mut detail = resolved.detail;
            let entries = detail
                .as_object_mut()
                .expect("generated command schema is an object");
            entries.insert(
                "command".to_string(),
                serde_json::Value::String(resolved.id.clone()),
            );
            entries.insert("scripting".to_string(), scripting);
            detail
        }),
        None => Ok(build_schema_index_view(&full, CommandAccessFilter::All)),
    }
}

/// Renders a lightweight command index restricted to one access class.
pub fn render_cli_schema_index_for_access(
    schema_json: &'static str,
    access: CommandAccessFilter,
) -> serde_json::Value {
    let full: serde_json::Value =
        serde_json::from_str(schema_json).expect("generated SCHEMA_JSON is always valid JSON");
    build_schema_index_view(&full, access)
}

/// Returns command identifiers grouped by their generated access contract.
pub fn command_access_inventory(schema_json: &'static str) -> serde_json::Value {
    let full: serde_json::Value =
        serde_json::from_str(schema_json).expect("generated SCHEMA_JSON is always valid JSON");
    let mut public = Vec::new();
    let mut optional = Vec::new();
    let mut authenticated = Vec::new();
    for command in command_catalog(&full) {
        match command_access_mode(command.value) {
            "public" => public.push(command.id),
            "optional" => optional.push(command.id),
            "authenticated" => authenticated.push(command.id),
            _ => {}
        }
    }
    serde_json::json!({
        "public": public,
        "optional": optional,
        "authenticated": authenticated,
    })
}

/// Copy-paste invocation forms for scripts, carried in command detail so an
/// agent writing a subprocess call never needs a separate lookup. Errors
/// arrive as JSON on stderr with `retryable` and `hint`, so scripts can
/// branch without another model round trip.
fn scripting_recipe_for_command(resolved: &ResolvedCommand) -> serde_json::Value {
    let cli = &crate::config::runtime_config().identity.command_name;
    let invocation = resolved.id.replace('.', " ");
    let mut recipe = serde_json::json!({
        "direct": format!("{cli} {invocation} <args> --output json --no-input"),
        "python": "subprocess.run(argv_list, input=body_json, capture_output=True, text=True): stdout is the JSON result; failures exit nonzero with {\"error\": {..., \"retryable\", \"hint\"}} on stderr",
    });
    if resolved
        .detail
        .get("body_mode")
        .and_then(serde_json::Value::as_str)
        == Some("structured")
    {
        recipe["body"] = serde_json::Value::String(
            "pass the JSON request body on stdin with `--body -`, inline with `--body-json`, or per field with `--field path=value`".to_string(),
        );
    }
    recipe
}

/// Returns the schema-only portion of one command detail. This explicit view is
/// useful to validators and form generators that do not need CLI metadata.
pub fn render_json_schema(
    schema_json: &'static str,
    command: &str,
) -> Result<serde_json::Value, crate::error::ClientError> {
    let full: serde_json::Value =
        serde_json::from_str(schema_json).expect("generated SCHEMA_JSON is always valid JSON");
    let resolved = resolve_command(&full, command)?;
    let detail = resolved.detail;
    Ok(serde_json::json!({
        "command": resolved.id,
        "request_schema": detail.get("request_schema").cloned().unwrap_or(serde_json::Value::Null),
        "response_schemas": detail.get("response_schemas").cloned().unwrap_or_else(|| serde_json::json!({})),
        "components": detail.get("components").cloned().unwrap_or_else(|| serde_json::json!({ "schemas": {} })),
    }))
}

/// Returns the resource groups with at least one command reachable by the
/// caller identity according to generated `x-authz` metadata.
pub fn relevant_resources(schema_json: &'static str, identity: &serde_json::Value) -> Vec<String> {
    let full: serde_json::Value =
        serde_json::from_str(schema_json).expect("generated SCHEMA_JSON is always valid JSON");
    let org_type = identity.get("org_type").and_then(serde_json::Value::as_str);
    let org_role = identity.get("org_role").and_then(serde_json::Value::as_str);
    full.get("resources")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|resource| {
            resource
                .get("commands")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|commands| {
                    commands.iter().any(|command| {
                        authorization_policy_allows_identity(
                            command.get("authz"),
                            org_type,
                            org_role,
                        )
                    })
                })
        })
        .filter_map(|resource| resource.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Returns the first generated authentication command appropriate for a caller
/// that has not configured credentials yet.
pub fn schema_login_hint_for_auth_commands(schema_json: &'static str) -> Option<String> {
    let full: serde_json::Value =
        serde_json::from_str(schema_json).expect("generated SCHEMA_JSON is always valid JSON");
    full.pointer("/auth/login_hints")
        .and_then(serde_json::Value::as_array)
        .and_then(|hints| hints.first())
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Returns every security-scheme name referenced by generated commands.
pub fn schema_auth_scheme_names(schema_json: &'static str) -> Vec<String> {
    let full: serde_json::Value =
        serde_json::from_str(schema_json).expect("generated SCHEMA_JSON is always valid JSON");
    full.pointer("/auth/schemes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect()
}

fn authorization_policy_allows_identity(
    authz: Option<&serde_json::Value>,
    org_type: Option<&str>,
    org_role: Option<&str>,
) -> bool {
    let Some(authz) = authz.filter(|value| !value.is_null()) else {
        return false;
    };
    let allowed_types = authorization_policy_values_for_keys(
        authz,
        &[
            "orgtype",
            "orgtypes",
            "allowedorgtypes",
            "organizationtypes",
        ],
    );
    if !allowed_types.is_empty()
        && !org_type.is_some_and(|actual| {
            allowed_types
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(actual))
        })
    {
        return false;
    }
    let minimum_role =
        authorization_policy_values_for_keys(authz, &["minorgrole", "minimumorgrole"])
            .into_iter()
            .next();
    match (minimum_role.as_deref(), org_role) {
        (Some(minimum), Some(actual)) => actual_role_satisfies_minimum_role(actual, minimum),
        (Some(_), None) => false,
        (None, _) => true,
    }
}

/// Reads one policy field from a flat authz object. Values may be a string
/// or an array of strings; key spelling is normalized (`allowed_org_types`,
/// `orgTypes`, ...). Authz metadata is a flat `{key: scalar-or-list}` object
/// — nothing nested inside it is policy.
fn authorization_policy_values_for_keys(
    authz: &serde_json::Value,
    normalized_keys: &[&str],
) -> Vec<String> {
    authz
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| {
            let normalized = key
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(|character| character.to_lowercase())
                .collect::<String>();
            normalized_keys.contains(&normalized.as_str())
        })
        .flat_map(|(_, value)| match value {
            serde_json::Value::String(value) => vec![value.clone()],
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn actual_role_satisfies_minimum_role(actual: &str, minimum: &str) -> bool {
    let rank = |role: &str| match role.to_ascii_lowercase().as_str() {
        "viewer" | "guest" => Some(0_u8),
        "member" => Some(1),
        "operator" => Some(2),
        "admin" | "administrator" => Some(3),
        "owner" => Some(4),
        "root" | "superuser" => Some(5),
        _ => None,
    };
    match (rank(actual), rank(minimum)) {
        (Some(actual), Some(minimum)) => actual >= minimum,
        _ => actual.eq_ignore_ascii_case(minimum),
    }
}

struct ResolvedCommand {
    id: String,
    detail: serde_json::Value,
}

fn resolve_command(
    full: &serde_json::Value,
    query: &str,
) -> Result<ResolvedCommand, crate::error::ClientError> {
    let commands = command_catalog(full);
    if query.contains('.') {
        if let Some(command) = commands.iter().find(|command| command.id == query) {
            return Ok(command.with_command_detail_from_full_schema(full));
        }
        return Err(unknown_command(query, &commands));
    }

    let matches: Vec<_> = commands
        .iter()
        .filter(|command| command.name == query || command.aliases.contains(&query))
        .collect();
    match matches.as_slice() {
        [command] => Ok(command.with_command_detail_from_full_schema(full)),
        [] => Err(unknown_command(query, &commands)),
        matches => Err(crate::error::ClientError::Decode(format!(
            "ambiguous command {query:?}; candidates: {}",
            matches
                .iter()
                .map(|command| command.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

struct CatalogCommand<'a> {
    id: String,
    name: &'a str,
    aliases: Vec<&'a str>,
    value: &'a serde_json::Value,
}

impl CatalogCommand<'_> {
    fn with_command_detail_from_full_schema(&self, full: &serde_json::Value) -> ResolvedCommand {
        let mut detail = self.value.clone();
        let schemas = full.pointer("/components/schemas");
        let needed = needed_components(&detail, schemas);
        detail
            .as_object_mut()
            .expect("generated command schema is an object")
            .insert(
                "components".to_string(),
                serde_json::json!({ "schemas": needed }),
            );
        ResolvedCommand {
            id: self.id.clone(),
            detail,
        }
    }
}

fn command_catalog(full: &serde_json::Value) -> Vec<CatalogCommand<'_>> {
    let Some(resources) = full.get("resources").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut catalog = Vec::new();
    for resource in resources {
        let Some(resource_name) = resource.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(commands) = resource
            .get("commands")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for command in commands {
            let Some(name) = command.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let aliases = command
                .get("aliases")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect();
            catalog.push(CatalogCommand {
                id: format!("{resource_name}.{name}"),
                name,
                aliases,
                value: command,
            });
        }
    }
    catalog
}

fn unknown_command(query: &str, commands: &[CatalogCommand<'_>]) -> crate::error::ClientError {
    let mut ranked: Vec<_> = commands
        .iter()
        .map(|command| {
            let distance = std::iter::once(command.id.as_str())
                .chain(std::iter::once(command.name))
                .chain(command.aliases.iter().copied())
                .map(|candidate| levenshtein_edit_distance(query, candidate))
                .min()
                .unwrap_or(usize::MAX);
            (distance, command.id.as_str())
        })
        .collect();
    ranked.sort_unstable();
    ranked.dedup_by_key(|(_, id)| *id);
    let suggestions = ranked
        .into_iter()
        .take(3)
        .map(|(_, id)| id)
        .collect::<Vec<_>>();
    let suffix = if suggestions.is_empty() {
        String::new()
    } else {
        format!("; nearest commands: {}", suggestions.join(", "))
    };
    crate::error::ClientError::Decode(format!(
        "unknown command {query:?}{suffix}; run `schema` for the command index"
    ))
}

fn levenshtein_edit_distance(left: &str, right: &str) -> usize {
    let mut previous: Vec<usize> = (0..=right.chars().count()).collect();
    for (left_index, left_char) in left.chars().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_char) in right.chars().enumerate() {
            current.push(std::cmp::min(
                std::cmp::min(current[right_index] + 1, previous[right_index + 1] + 1),
                previous[right_index] + usize::from(left_char != right_char),
            ));
        }
        previous = current;
    }
    previous[right.chars().count()]
}

fn needed_components(
    command: &serde_json::Value,
    components: Option<&serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let Some(components) = components.and_then(serde_json::Value::as_object) else {
        return serde_json::Map::new();
    };
    let mut pending = Vec::new();
    collect_component_refs(command.get("request_schema"), &mut pending);
    collect_component_refs(command.get("response_schemas"), &mut pending);
    let mut selected = serde_json::Map::new();
    while let Some(name) = pending.pop() {
        if selected.contains_key(&name) {
            continue;
        }
        let Some(schema) = components.get(&name) else {
            continue;
        };
        collect_component_refs(Some(schema), &mut pending);
        selected.insert(name, schema.clone());
    }
    selected
}

fn collect_component_refs(value: Option<&serde_json::Value>, names: &mut Vec<String>) {
    let Some(value) = value else { return };
    match value {
        serde_json::Value::Object(object) => {
            if let Some(name) = object
                .get("$ref")
                .and_then(serde_json::Value::as_str)
                .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
            {
                names.push(name.replace("~1", "/").replace("~0", "~"));
            }
            for child in object.values() {
                collect_component_refs(Some(child), names);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_component_refs(Some(child), names);
            }
        }
        _ => {}
    }
}

/// Strips prose (`summary`, `description`, and per-parameter descriptions)
/// from every command, keeping just enough to name and call `schema` again
/// for the one an agent actually wants: identifier, method, path, parameter
/// names, and whether it takes a body.
fn build_schema_index_view(
    full: &serde_json::Value,
    access: CommandAccessFilter,
) -> serde_json::Value {
    let mut view = full.clone();
    view.as_object_mut()
        .map(|object| object.remove("components"));
    let Some(resources) = view.get_mut("resources").and_then(|v| v.as_array_mut()) else {
        return view;
    };
    for resource in &mut *resources {
        let Some(commands) = resource.get_mut("commands").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        commands.retain(|command| command_matches_access(command, access));
        for command in commands {
            let Some(object) = command.as_object_mut() else {
                continue;
            };
            object.remove("summary");
            object.remove("description");
            object.remove("request_schema");
            object.remove("response_schemas");
            for key in ["path_params", "query_params", "headers"] {
                if let Some(names) = object
                    .get(key)
                    .and_then(extract_parameter_names_from_schema_array)
                {
                    object.insert(key.to_string(), serde_json::Value::Array(names));
                }
            }
        }
    }
    resources.retain(|resource| {
        resource
            .get("commands")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|commands| !commands.is_empty())
    });
    view["access_filter"] = serde_json::Value::String(
        match access {
            CommandAccessFilter::All => "all",
            CommandAccessFilter::Public => "public",
            CommandAccessFilter::Optional => "optional",
            CommandAccessFilter::Authenticated => "authenticated",
        }
        .to_string(),
    );
    view
}

fn command_matches_access(command: &serde_json::Value, access: CommandAccessFilter) -> bool {
    match access {
        CommandAccessFilter::All => true,
        CommandAccessFilter::Public => command_access_mode(command) == "public",
        CommandAccessFilter::Optional => command_access_mode(command) == "optional",
        CommandAccessFilter::Authenticated => command_access_mode(command) == "authenticated",
    }
}

fn command_access_mode(command: &serde_json::Value) -> &str {
    command
        .pointer("/authentication/mode")
        .and_then(serde_json::Value::as_str)
        // Schemas generated before v10 had no command-level contract. Treat
        // them as public for backwards-compatible discovery.
        .unwrap_or("public")
}

fn extract_parameter_names_from_schema_array(
    value: &serde_json::Value,
) -> Option<Vec<serde_json::Value>> {
    let items = value.as_array()?;
    Some(
        items
            .iter()
            .map(|item| item.get("name").cloned().unwrap_or(serde_json::Value::Null))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r##"{
        "schema_version": 3,
        "name": "test-cli",
        "components": {
            "schemas": {
                "Member": {
                    "type": "object",
                    "properties": {
                        "role": {"$ref": "#/components/schemas/Role"}
                    }
                },
                "Role": {"type": "string", "enum": ["admin"]},
                "Unused": {"type": "boolean"}
            }
        },
        "resources": [
            {
                "name": "orgs",
                "commands": [
                    {
                        "name": "get-org-members-v1",
                        "aliases": ["members"],
                        "invocation": "orgs get-org-members-v1",
                        "body_mode": null,
                        "summary": "List members",
                        "description": "Lists all members.",
                        "method": "GET",
                        "path": "/orgs/members",
                        "path_params": [],
                        "query_params": [{"name": "page", "description": "Page number"}],
                        "headers": [],
                        "has_body": false,
                        "request_schema": null,
                        "response_schemas": {
                            "200": {"type": "array", "items": {"$ref": "#/components/schemas/Member"}}
                        }
                    }
                ]
            }
        ]
    }"##;

    const LOOKUP_SCHEMA: &str = r#"{
        "resources": [
            {"name": "orgs", "commands": [
                {"name": "list", "aliases": ["ls"], "request_schema": null, "response_schemas": {}},
                {"name": "show", "aliases": ["get"], "request_schema": null, "response_schemas": {}}
            ]},
            {"name": "users", "commands": [
                {"name": "list", "aliases": ["ls"], "request_schema": null, "response_schemas": {}}
            ]}
        ],
        "components": {"schemas": {}}
    }"#;

    #[test]
    fn index_view_strips_descriptions_but_keeps_names() {
        let view = render_cli_schema_json_response(SCHEMA, None).expect("index renders");
        let command = &view["resources"][0]["commands"][0];
        assert!(command.get("summary").is_none());
        assert!(command.get("description").is_none());
        assert_eq!(command["query_params"], serde_json::json!(["page"]));
        assert_eq!(command["method"], "GET");
        assert_eq!(command["aliases"], serde_json::json!(["members"]));
        assert_eq!(command["invocation"], "orgs get-org-members-v1");
        assert!(command.get("response_schemas").is_none());
        assert!(view.get("components").is_none());
    }

    #[test]
    fn detail_view_returns_one_command_with_descriptions() {
        let command = render_cli_schema_json_response(SCHEMA, Some("orgs.get-org-members-v1"))
            .expect("detail renders");
        assert_eq!(command["summary"], "List members");
        assert_eq!(command["query_params"][0]["description"], "Page number");
        assert_eq!(command["response_schemas"]["200"]["type"], "array");
        assert!(command["components"]["schemas"].get("Member").is_some());
        assert!(command["components"]["schemas"].get("Role").is_some());
        assert!(command["components"]["schemas"].get("Unused").is_none());
    }

    #[test]
    fn detail_view_carries_stable_id_and_scripting_recipe() {
        let command = render_cli_schema_json_response(SCHEMA, Some("orgs.get-org-members-v1"))
            .expect("detail renders");
        assert_eq!(command["command"], "orgs.get-org-members-v1");
        let direct = command["scripting"]["direct"]
            .as_str()
            .expect("detail carries a direct invocation form");
        assert!(direct.contains("orgs get-org-members-v1"), "{direct}");
        assert!(direct.contains("--output json"), "{direct}");
        // No structured body on this command, so no body form is advertised.
        assert!(command["scripting"].get("body").is_none());
        // The index never carries scripting noise.
        let index = render_cli_schema_json_response(SCHEMA, None).expect("index renders");
        assert!(
            index["resources"][0]["commands"][0]
                .get("scripting")
                .is_none()
        );
    }

    #[test]
    fn structured_body_commands_advertise_stdin_body_forms() {
        let schema = r#"{
            "resources": [
                {"name": "orders", "commands": [
                    {"name": "create", "aliases": [], "body_mode": "structured",
                     "request_schema": {"type": "object"}, "response_schemas": {}}
                ]}
            ],
            "components": {"schemas": {}}
        }"#;
        let command =
            render_cli_schema_json_response(schema, Some("orders.create")).expect("detail");
        let body = command["scripting"]["body"].as_str().expect("body form");
        assert!(body.contains("--body -"), "{body}");
        assert!(body.contains("--field"), "{body}");
    }

    #[test]
    fn explicit_json_schema_view_omits_cli_metadata() {
        let schema =
            render_json_schema(SCHEMA, "orgs.get-org-members-v1").expect("schema view renders");
        assert_eq!(schema["command"], "orgs.get-org-members-v1");
        assert!(schema.get("method").is_none());
        assert_eq!(schema["response_schemas"]["200"]["type"], "array");
        assert!(schema["components"]["schemas"].get("Role").is_some());
    }

    #[test]
    fn detail_view_reports_an_unknown_command_by_name() {
        let error = render_cli_schema_json_response(SCHEMA, Some("orgs.nonexistent"))
            .expect_err("unknown command errors");
        assert!(error.to_string().contains("orgs.nonexistent"));
        assert!(error.to_string().contains("nearest commands"));
    }

    #[test]
    fn lookup_accepts_qualified_ids_and_unambiguous_bare_names_or_aliases() {
        assert_eq!(
            render_cli_schema_json_response(LOOKUP_SCHEMA, Some("orgs.list"))
                .expect("qualified id")["name"],
            "list"
        );
        assert_eq!(
            render_cli_schema_json_response(LOOKUP_SCHEMA, Some("show")).expect("unique bare name")
                ["name"],
            "show"
        );
        assert_eq!(
            render_json_schema(LOOKUP_SCHEMA, "get").expect("unique alias")["command"],
            "orgs.show"
        );
    }

    #[test]
    fn ambiguous_bare_names_and_aliases_list_qualified_candidates() {
        for query in ["list", "ls"] {
            let error = render_cli_schema_json_response(LOOKUP_SCHEMA, Some(query))
                .expect_err("lookup is ambiguous");
            assert_eq!(
                error.to_string(),
                format!(
                    "could not decode response: ambiguous command {query:?}; candidates: orgs.list, users.list"
                )
            );
        }
    }

    #[test]
    fn unknown_lookup_returns_three_nearest_qualified_suggestions() {
        let error = render_cli_schema_json_response(LOOKUP_SCHEMA, Some("lstt"))
            .expect_err("lookup is unknown");
        let message = error.to_string();
        assert!(message.contains("nearest commands:"));
        assert!(message.contains("orgs.list"));
        assert!(message.contains("users.list"));
        assert!(message.contains("orgs.show"));
    }

    #[test]
    fn orientation_filters_resources_by_org_type_and_minimum_role() {
        const AUTHZ_SCHEMA: &str = r#"{
            "resources": [
                {"name": "drivers", "commands": [
                    {"authz": {"org_types": ["provider"], "min_org_role": "operator"}}
                ]},
                {"name": "billing", "commands": [
                    {"authz": {"org_types": ["shipper"], "min_org_role": "owner"}}
                ]},
                {"name": "admin", "commands": [
                    {"authz": {"org_types": ["provider"], "min_org_role": "root"}}
                ]}
            ],
            "auth": {"login_hints": ["chrt auth login"]}
        }"#;
        let identity = serde_json::json!({
            "org_type": "provider",
            "org_role": "owner"
        });
        assert_eq!(
            relevant_resources(AUTHZ_SCHEMA, &identity),
            ["drivers".to_string()]
        );
        assert_eq!(
            schema_login_hint_for_auth_commands(AUTHZ_SCHEMA).as_deref(),
            Some("chrt auth login")
        );
    }

    #[test]
    fn access_filters_and_inventory_preserve_all_three_command_classes() {
        const ACCESS_SCHEMA: &str = r#"{
            "resources": [{"name": "items", "commands": [
                {"name": "health", "authentication": {"mode": "public"}},
                {"name": "search", "authentication": {"mode": "optional"}},
                {"name": "create", "authentication": {"mode": "authenticated"}}
            ]}]
        }"#;
        let public = render_cli_schema_index_for_access(ACCESS_SCHEMA, CommandAccessFilter::Public);
        assert_eq!(public["access_filter"], "public");
        assert_eq!(public["resources"][0]["commands"][0]["name"], "health");
        assert_eq!(
            public["resources"][0]["commands"].as_array().unwrap().len(),
            1
        );

        let inventory = command_access_inventory(ACCESS_SCHEMA);
        assert_eq!(inventory["public"], serde_json::json!(["items.health"]));
        assert_eq!(inventory["optional"], serde_json::json!(["items.search"]));
        assert_eq!(
            inventory["authenticated"],
            serde_json::json!(["items.create"])
        );
    }
}
