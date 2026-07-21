use heck::ToKebabCase;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use tokyo_ir::auth::{AuthRequirement, AuthSchemeKind, CommandAccess};
use tokyo_ir::cli_behavior::CliBehavior;
use tokyo_ir::http::{Endpoint, HttpMethod, Parameter};

use crate::types::TypeCatalog;

/// Bump whenever a field is added, removed, or reinterpreted in the shape
/// this module renders. An agent that caches `schema`'s output across a
/// generator upgrade needs a way to notice the shape moved out from under it;
/// the top-level `"schema_version"` field is that signal.
///
/// v2: `path_params`/`query_params`/`headers` moved from plain wire-name
/// strings to `{name, description}` objects, and commands gained
/// `summary`/`description`. This full, description-bearing shape is what
/// gets baked in; `tokyo_cli_runtime::schema::render` is what strips
/// it down to a cheap index by default and only returns full detail (this
/// shape, for one command) when the generated CLI's `schema --command`
/// asks for it — so a CLI with hundreds of operations never forces an agent
/// to load every description just to find the one it needs.
/// v3: command detail gained normalized request/response JSON Schemas and
/// transitive OpenAPI component schemas.
/// v4: commands gained aliases plus explicit invocation/body-input metadata.
/// v5: named embedded CLI scenarios were added.
/// v6: role-aware dispatch commands and member routing metadata were added.
/// v7: auth metadata gained exact generated login commands for first-use
/// onboarding.
/// v8: commands preserve operation `x-authz` policy for caller-aware
/// orientation.
/// v9: auth metadata gained an agent acquisition contract and `auth ensure`.
/// v10: every command gained an exact public/optional/authenticated contract.
const SCHEMA_FORMAT_VERSION: u32 = 10;

/// Builds the static, full-detail JSON baked in at codegen time (rather than
/// introspected at runtime, since the command surface is fixed once
/// generated) that `tokyo_cli_runtime::schema::render` filters down at
/// runtime into either the lightweight index or one command's full detail.
/// Must work with no configuration (no `--base-url`, no credentials), so
/// agents can discover the CLI's shape before setup is complete.
pub fn render_schema_json(
    product_name: &str,
    groups: &[(String, Vec<&Endpoint>)],
    sdk: &CliBehavior,
    schema_components: &BTreeMap<String, serde_json::Value>,
    catalog: &TypeCatalog,
) -> String {
    let auth_schemes: BTreeSet<_> = groups
        .iter()
        .flat_map(|(_, endpoints)| endpoints)
        .flat_map(|endpoint| &endpoint.auth)
        .flat_map(|requirement| &requirement.schemes)
        .map(|required| required.scheme.name.as_str())
        .collect();
    let login_hints = sdk
        .cli_auth
        .iter()
        .flat_map(|(scheme, provider)| {
            let environments = match &provider.endpoints {
                tokyo_ir::cli_behavior::OAuthEndpoints::BrowserToken {
                    allowed_environments,
                    ..
                }
                | tokyo_ir::cli_behavior::OAuthEndpoints::Mock {
                    allowed_environments,
                    ..
                }
                | tokyo_ir::cli_behavior::OAuthEndpoints::MockEnvironment {
                    allowed_environments,
                    ..
                } => allowed_environments.as_slice(),
                _ => &[],
            };
            if environments.is_empty() {
                vec![format!("{product_name} auth login --scheme {scheme}")]
            } else {
                environments
                    .iter()
                    .map(|environment| {
                        format!(
                            "{product_name} --environment {environment} auth login --scheme {scheme}"
                        )
                    })
                    .collect()
            }
        })
        .collect::<Vec<_>>();
    let resources: Vec<_> = groups
        .iter()
        .map(|(tag, endpoints)| {
            let mut commands: Vec<_> = endpoints
                .iter()
                .filter(|endpoint| {
                    !endpoint
                        .cli
                        .as_ref()
                        .is_some_and(|overrides| overrides.ignore || overrides.hidden)
                })
                .map(|endpoint| {
                    let resource = tag.to_kebab_case();
                    let name = endpoint
                        .cli
                        .as_ref()
                        .and_then(|overrides| overrides.name.clone())
                        .unwrap_or_else(|| endpoint.name.to_kebab_case());
                    let aliases = endpoint
                        .cli
                        .as_ref()
                        .map(|overrides| overrides.aliases.clone())
                        .unwrap_or_default();
                    json!({
                        "name": name,
                        "aliases": aliases,
                        "invocation": format!("{resource} {name}"),
                        "body_mode": crate::commands::request_body_mode(endpoint, catalog),
                        "summary": endpoint.summary,
                        "description": endpoint.docs,
                        "method": http_method_name(endpoint.method),
                        "path": endpoint.path,
                        "path_params": describe_parameters(&endpoint.path_parameters),
                        "query_params": describe_parameters(&endpoint.query_parameters),
                        "headers": describe_parameters(&endpoint.headers),
                        "has_body": endpoint.request_body.is_some(),
                        "request_schema": endpoint.request_schema,
                        "response_schemas": endpoint.response_schemas,
                        "authentication": render_authentication_metadata(
                            product_name,
                            &endpoint.auth,
                            sdk,
                        ),
                        "authz": render_authz_metadata_json(endpoint),
                    })
                })
                .collect();
            for dispatch in sdk
                .cli_dispatch_groups
                .iter()
                .filter(|dispatch| dispatch.resource.to_kebab_case() == tag.to_kebab_case())
            {
                let members = dispatch
                    .members
                    .iter()
                    .filter_map(|member| {
                        let endpoint = groups.iter().flat_map(|(_, endpoints)| endpoints).find(
                            |endpoint| {
                                endpoint.method == member.method && endpoint.path == member.path
                            },
                        )?;
                        Some(json!({
                            "name": member.name,
                            "view": member.view,
                            "identity": member.identity,
                            "method": http_method_name(endpoint.method),
                            "path": endpoint.path,
                            "response_schemas": endpoint.response_schemas,
                        }))
                    })
                    .collect::<Vec<_>>();
                let default = dispatch
                    .members
                    .iter()
                    .find(|member| member.name == dispatch.default_member)
                    .and_then(|member| {
                        groups
                            .iter()
                            .flat_map(|(_, endpoints)| endpoints)
                            .find(|endpoint| {
                                endpoint.method == member.method && endpoint.path == member.path
                            })
                    })
                    .expect("dispatch groups are validated");
                let resource = tag.to_kebab_case();
                let mut dispatch_auth = Vec::new();
                for endpoint in dispatch.members.iter().filter_map(|member| {
                    groups
                        .iter()
                        .flat_map(|(_, endpoints)| endpoints)
                        .find(|endpoint| {
                            endpoint.method == member.method && endpoint.path == member.path
                        })
                }) {
                    if endpoint.auth.is_empty() {
                        let anonymous = AuthRequirement {
                            schemes: Vec::new(),
                        };
                        if !dispatch_auth.contains(&anonymous) {
                            dispatch_auth.push(anonymous);
                        }
                    } else {
                        for requirement in &endpoint.auth {
                            if !dispatch_auth.contains(requirement) {
                                dispatch_auth.push(requirement.clone());
                            }
                        }
                    }
                }
                commands.push(json!({
                    "name": dispatch.name,
                    "aliases": [],
                    "invocation": format!("{resource} {}", dispatch.name),
                    "body_mode": crate::commands::request_body_mode(default, catalog),
                    "summary": dispatch.description,
                    "description": dispatch.description,
                    "method": http_method_name(default.method),
                    "path": default.path,
                    "path_params": describe_parameters(&default.path_parameters),
                    "query_params": describe_parameters(&default.query_parameters),
                    "headers": describe_parameters(&default.headers),
                    "has_body": default.request_body.is_some(),
                    "request_schema": default.request_schema,
                    "response_schemas": default.response_schemas,
                    "authentication": render_authentication_metadata(
                        product_name,
                        &dispatch_auth,
                        sdk,
                    ),
                    "authz": render_authz_metadata_json(default),
                    "routing": {
                        "kind": "identity_dispatch",
                        "default_member": dispatch.default_member,
                        "view_flag": dispatch.members.iter().any(|member| member.view.is_some())
                            .then_some("--view"),
                        "members": members,
                    },
                }));
            }
            json!({ "name": tag.to_kebab_case(), "commands": commands })
        })
        .collect();

    let value = json!({
        "schema_version": SCHEMA_FORMAT_VERSION,
        "name": product_name,
        "resources": resources,
        "components": { "schemas": schema_components },
        "escape_hatch": {
            "command": "api",
            "usage": "api <method> <path> [--body FILE | --body-json JSON | --field path=value ...]",
            "body_modes": ["file", "json", "fields"],
        },
        "auth": {
            "commands": [
                "auth doctor [--scheme SCHEME]",
                "auth ensure [--scheme SCHEME] [--interaction forbid|relay|allow] [--device]",
                "auth login [--scheme SCHEME]",
                "auth login [--scheme SCHEME] --device",
                "auth logout [--scheme SCHEME]",
                "auth whoami [--scheme SCHEME]",
            ],
            "schemes": auth_schemes,
            "interactive_oauth_schemes": sdk.cli_auth.keys().collect::<Vec<_>>(),
            "agent_contract": {
                "preferred_command": "auth ensure --interaction relay --output json",
                "action_events": "JSON objects on stderr with status=action_required; the command keeps polling",
                "interaction_policies": ["forbid", "relay", "allow"],
            },
            "login_hints": login_hints,
            "default_scheme": "token",
            "storage": "native OS keychain; owner-only JSON only when unavailable",
        },
        "discovery": {
            "commands": [
                "schema",
                "schema --access public|optional|authenticated|all",
                "schema --command RESOURCE.COMMAND",
            ],
            "default_access": "all",
            "authentication_visibility": "authenticated commands remain discoverable before login",
        },
        "connection": {
            "commands": [
                "profile list",
                "profile show",
                "profile set (--base-url URL | --environment NAME)",
                "env list",
            ],
            "default_base_url": sdk.base_url,
            "environments": sdk.environments,
            "precedence": [
                "--base-url or its environment variable",
                "--environment or its environment variable",
                "profile base_url",
                "profile environment",
                "generated default base URL",
            ],
            "storage": "non-secret profiles.json",
        },
        "scenarios": sdk.cli_scenarios.iter().map(|scenario| json!({
            "name": scenario.name,
            "description": scenario.description,
            "allowed_environments": scenario.allowed_environments,
        })).collect::<Vec<_>>(),
        "global_flags": [
            "--base-url", "--token", "--client-id", "--client-secret",
            "--credential", "--credentials-json", "--credential-file",
            "--output", "--fields", "--no-input", "--profile", "--environment",
        ],
        "output_formats": ["text", "json", "json-raw"],
        "exit_codes": {
            "0": "success",
            "1": "general failure (network error, unmapped API error)",
            "2": "usage error (invalid flags/arguments)",
            "3": "not found (HTTP 404)",
            "4": "permission/auth failure",
            "5": "conflict (HTTP 409)",
        },
    });
    serde_json::to_string_pretty(&value).expect("a plain data value always serializes")
}

fn render_authentication_metadata(
    product_name: &str,
    requirements: &[AuthRequirement],
    sdk: &CliBehavior,
) -> serde_json::Value {
    let access = CommandAccess::from_requirements(requirements);
    let alternatives = requirements
        .iter()
        .map(|requirement| {
            json!({
                "schemes": requirement.schemes.iter().map(|required| {
                    let acquisition_command = sdk.cli_auth.contains_key(&required.scheme.name)
                        .then(|| format!(
                            "{product_name} auth ensure --scheme {} --interaction relay",
                            required.scheme.name
                        ));
                    json!({
                        "name": required.scheme.name,
                        "kind": auth_scheme_kind_name(&required.scheme.kind),
                        "scopes": required.scopes,
                        "credential_argument": format!("--credential {}=<value>", required.scheme.name),
                        "acquisition_command": acquisition_command,
                    })
                }).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let recovery_command = requirements
        .iter()
        .filter(|requirement| requirement.schemes.len() == 1)
        .filter_map(|requirement| requirement.schemes.first())
        .find(|required| sdk.cli_auth.contains_key(&required.scheme.name))
        .map(|required| {
            format!(
                "{product_name} auth ensure --scheme {} --interaction relay",
                required.scheme.name
            )
        });
    json!({
        "mode": access.as_str(),
        "alternatives": alternatives,
        "recovery_command": recovery_command,
    })
}

fn auth_scheme_kind_name(kind: &AuthSchemeKind) -> &'static str {
    match kind {
        AuthSchemeKind::Bearer => "bearer",
        AuthSchemeKind::Basic => "basic",
        AuthSchemeKind::Header { .. } => "header_api_key",
        AuthSchemeKind::QueryKey { .. } => "query_api_key",
        AuthSchemeKind::CookieKey { .. } => "cookie_api_key",
        AuthSchemeKind::OAuth2 { .. } => "oauth2",
        AuthSchemeKind::Inferred { .. } => "inferred",
    }
}

fn describe_parameters(parameters: &[Parameter]) -> serde_json::Value {
    json!(
        parameters
            .iter()
            .map(|param| json!({ "name": param.wire_name, "description": param.docs }))
            .collect::<Vec<_>>()
    )
}

/// Authz policy for an endpoint: the IR's `x-authz` extension when present,
/// otherwise the explicit `authz: key=value, key=[a, b]` tag some specs embed
/// in operation descriptions (`| authz: allowed_org_types=[provider],
/// min_org_role=operator |`). Values are emitted as written — the runtime
/// normalizes key spellings — and prose is never interpreted as policy.
fn render_authz_metadata_json(endpoint: &Endpoint) -> Option<serde_json::Value> {
    if let Some(authz) = &endpoint.authz {
        return Some(authz.clone());
    }
    parse_authz_tag(endpoint.docs.as_deref()?)
}

fn parse_authz_tag(description: &str) -> Option<serde_json::Value> {
    let authz = description
        .split('|')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("authz:"))?;
    let mut policy = serde_json::Map::new();
    for assignment in split_outside_brackets(authz) {
        let Some((key, value)) = assignment.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let json = match value.strip_prefix('[') {
            // A bracketed list; trailing prose after `]` is ignored.
            Some(rest) => json!(
                rest.split(']')
                    .next()
                    .unwrap_or(rest)
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>()
            ),
            // A scalar; keep the first word so trailing prose is ignored.
            None => match value.split_whitespace().next() {
                Some(scalar) => json!(scalar.trim_end_matches(['.', ',', ';'])),
                None => continue,
            },
        };
        policy.insert(key.trim().to_string(), json);
    }
    (!policy.is_empty()).then(|| serde_json::Value::Object(policy))
}

/// Splits on commas that sit outside `[...]`, so list values survive intact.
fn split_outside_brackets(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (position, character) in input.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&input[start..position]);
                start = position + 1;
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

fn http_method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Head => "HEAD",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Options => "OPTIONS",
        HttpMethod::Trace => "TRACE",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_authz_tags_with_bracketed_lists() {
        assert_eq!(
            parse_authz_tag(
                "Create a driver. | authz: allowed_org_types=[provider, shipper], \
                 min_org_role=operator | (Req) -> (Res)"
            ),
            Some(json!({
                "allowed_org_types": ["provider", "shipper"],
                "min_org_role": "operator",
            }))
        );
        // Trailing prose after a list or scalar is not policy.
        assert_eq!(
            parse_authz_tag(
                "authz: allowed_org_types=[provider] (shippers cannot subscribe -- see docs)"
            ),
            Some(json!({ "allowed_org_types": ["provider"] }))
        );
        assert_eq!(
            parse_authz_tag("authz: min_org_role=administrator (site admins bypass this)"),
            Some(json!({ "min_org_role": "administrator" }))
        );
    }

    #[test]
    fn prose_descriptions_never_become_policy() {
        assert_eq!(parse_authz_tag("Provider orgs only."), None);
        assert_eq!(parse_authz_tag("No policy here at all"), None);
    }
}
