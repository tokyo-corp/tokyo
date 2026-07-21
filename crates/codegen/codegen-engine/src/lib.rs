//! Path-free orchestration shared by Tokyo code generator front ends.
//!
//! Importing, configuration, canonicalization, emission, and snapshot creation
//! operate only on in-memory values. Front ends retain ownership of paths,
//! manifests, presentation, and filesystem transactions.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;

use serde::{Deserialize, Serialize};
use tokyo_ir::Api;
use tokyo_ir::cli_behavior::{
    CliAuthProvider, CliDispatchGroup, CliDispatchMember, CliScenario, OAuthEndpoints,
};
use tokyo_ir::http::{Endpoint, HttpMethod};

/// One deterministic file produced by a code-generation target.
#[must_use = "generated files should be written, compared, or inspected"]
pub struct GeneratedFile {
    /// Path relative to the generated project root.
    pub relative_path: String,
    /// UTF-8 file contents.
    pub contents: String,
}

/// Filename used for the persisted IR snapshot inside generated output.
pub const SNAPSHOT_FILE: &str = ".tokyo/ir.json";
/// Snapshot path written by earlier engines; front ends read it as a
/// fallback so existing generated projects migrate on their next generation.
pub const LEGACY_SNAPSHOT_FILE: &str = ".tokyo-ir.json";
/// Version of the snapshot container written by this engine.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 1;

/// Explicit or inferred encoding of an OpenAPI input document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InputFormat {
    /// Infer JSON vs YAML from the first non-whitespace character.
    Auto,
    /// Parse the input as JSON.
    Json,
    /// Parse the input as YAML.
    Yaml,
}

/// User configuration that customizes IR import and CLI generation.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Generated Cargo package name.
    #[serde(alias = "package_name")]
    pub package: Option<String>,
    /// Optional package/executable name for the generated CLI target.
    pub cli_name: Option<String>,
    /// Named API environments exposed by generated CLIs.
    pub environments: BTreeMap<String, String>,
    /// Default API base URL for generated clients.
    pub base_url: Option<String>,
    /// Front ends may interpret this value as an output location.
    ///
    /// The engine deliberately treats it as opaque configuration and never
    /// performs path operations.
    pub output: Option<String>,
    /// Interactive CLI login providers keyed by OpenAPI security-scheme name.
    pub cli_auth: BTreeMap<String, CliAuthProvider>,
    /// Named scenario programs embedded in generated CLI binaries.
    pub cli_scenarios: Vec<CliScenarioConfig>,
    /// Public CLI commands which dispatch across compatible operations.
    pub cli_dispatch_groups: Vec<CliDispatchGroupConfig>,
    /// SDK target configuration.
    pub sdk: SdkTargetConfig,
}

/// Configuration for generated SDK targets (`[sdk]` in tokyo.toml).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SdkTargetConfig {
    /// Generated SDK package name (e.g. an npm package name).
    pub package: Option<String>,
    /// Exported client type name (e.g. `Stripe`).
    pub client_name: Option<String>,
    /// Front ends may interpret this value as the SDK output location. The
    /// engine treats it as opaque configuration, like [`Config::output`].
    pub output: Option<String>,
}

/// Configuration for one generated facade command that dispatches to multiple
/// compatible operations.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliDispatchGroupConfig {
    /// Resource group the facade command belongs to.
    pub resource: String,
    /// Public command name.
    pub name: String,
    /// Optional public command description.
    pub description: Option<String>,
    /// Member selected when no dispatch-specific view or identity rule matches.
    pub default_member: String,
    /// Operations eligible for this facade command.
    pub members: Vec<CliDispatchMemberConfig>,
}

/// Configuration for one operation inside a dispatch group.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliDispatchMemberConfig {
    /// Public member name used in config and diagnostics.
    pub name: String,
    /// HTTP method override; when absent the member is resolved by path only.
    pub method: Option<String>,
    /// OpenAPI operation path template.
    pub path: String,
    /// Identity fields that must match for this member to be selected.
    #[serde(default)]
    pub identity: BTreeMap<String, String>,
    /// Optional `--view` selector value for this member.
    pub view: Option<String>,
}

/// Configuration for one scenario embedded into the generated CLI.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliScenarioConfig {
    /// Scenario command name.
    pub name: String,
    /// Scenario description shown to users.
    pub description: String,
    /// Inline scenario program body.
    pub body: Option<String>,
    /// Path to a scenario program file, resolved relative to the config file by
    /// front ends.
    pub file: Option<String>,
    /// Named environments where the scenario may run.
    #[serde(default)]
    pub allowed_environments: Vec<String>,
}

/// Persisted generation snapshot used by `check` and `diff`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// Snapshot container version.
    pub format_version: u32,
    /// Generator version that wrote the snapshot.
    pub generator_version: String,
    /// Canonical API IR captured during generation.
    pub api: Api,
}

/// In-memory result of a full generation run.
#[must_use = "generated files and API snapshot should be written or compared"]
pub struct Generation {
    /// Imported and configured API IR.
    pub api: Api,
    /// Files emitted for the generated target.
    pub files: Vec<GeneratedFile>,
}

/// Backend that renders a configured API IR into target-specific files.
pub trait Emitter {
    /// Error type returned by the target emitter.
    type Error: Display;

    /// Emits all files for the generated target project (CLI, SDK, ...).
    ///
    /// # Errors
    ///
    /// Returns the emitter-specific error if target source generation fails.
    fn emit_target_files(&self, api: &Api) -> Result<Vec<GeneratedFile>, Self::Error>;
}

/// Error returned by code-generation orchestration.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Input bytes were not valid UTF-8.
    #[error("input is not valid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    /// OpenAPI import failed.
    #[error("{0}")]
    Import(String),
    /// Configuration validation failed.
    #[error("{0}")]
    Config(String),
    /// Target emitter failed.
    #[error("{0}")]
    Emit(String),
    /// The IR schema version is not writable by this engine.
    #[error("cannot write IR schema version {actual} (expected {expected})")]
    UnsupportedSchema {
        /// Schema version found in the input IR.
        actual: u32,
        /// Schema version supported by this engine.
        expected: u32,
    },
    /// Generated metadata could not be serialized.
    #[error("cannot serialize generated metadata: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Imports an OpenAPI document, applies configuration, emits target files, and
/// returns the in-memory generation result.
///
/// # Errors
///
/// Returns [`Error`] if the bytes are not UTF-8, the OpenAPI input cannot be
/// imported, configuration is invalid, emission fails, or the IR snapshot cannot
/// be serialized.
#[must_use = "the generated files or generation error should be handled"]
pub fn generate_from_openapi_bytes<E: Emitter>(
    openapi_input_bytes: &[u8],
    input_format: InputFormat,
    codegen_config: &Config,
    emitter: &E,
    generator_version: &str,
) -> Result<Generation, Error> {
    let configured_api_ir =
        import_openapi_bytes(openapi_input_bytes, input_format, codegen_config)?;
    let generated_files = build_output_plan(&configured_api_ir, emitter, generator_version)?;
    Ok(Generation {
        api: configured_api_ir,
        files: generated_files,
    })
}

/// Imports an OpenAPI text document, applies configuration, emits target files,
/// and returns the in-memory generation result.
///
/// # Errors
///
/// Returns [`Error`] if the OpenAPI input cannot be imported, configuration is
/// invalid, emission fails, or the IR snapshot cannot be serialized.
#[must_use = "the generated files or generation error should be handled"]
pub fn generate_from_openapi_text<E: Emitter>(
    openapi_input_text: &str,
    input_format: InputFormat,
    codegen_config: &Config,
    emitter: &E,
    generator_version: &str,
) -> Result<Generation, Error> {
    generate_from_openapi_bytes(
        openapi_input_text.as_bytes(),
        input_format,
        codegen_config,
        emitter,
        generator_version,
    )
}

/// Imports OpenAPI bytes into configured API IR.
///
/// # Errors
///
/// Returns [`Error`] if the bytes are not UTF-8, the OpenAPI input cannot be
/// imported, or configuration cannot be applied.
#[must_use = "the imported API or import error should be handled"]
pub fn import_openapi_bytes(
    openapi_input_bytes: &[u8],
    input_format: InputFormat,
    codegen_config: &Config,
) -> Result<Api, Error> {
    import_openapi_text(
        std::str::from_utf8(openapi_input_bytes)?,
        input_format,
        codegen_config,
    )
}

/// Imports OpenAPI text into configured API IR.
///
/// # Errors
///
/// Returns [`Error`] if the OpenAPI input cannot be imported or configuration
/// cannot be applied.
#[must_use = "the imported API or import error should be handled"]
pub fn import_openapi_text(
    openapi_input_text: &str,
    input_format: InputFormat,
    codegen_config: &Config,
) -> Result<Api, Error> {
    let imported_api_ir = match input_format {
        InputFormat::Json => tokyo_import_openapi::import_openapi_json_document(openapi_input_text),
        InputFormat::Yaml => tokyo_import_openapi::import_openapi_yaml_document(openapi_input_text),
        InputFormat::Auto => {
            let trimmed_openapi_input_text = openapi_input_text.trim_start();
            if trimmed_openapi_input_text.starts_with('{')
                || trimmed_openapi_input_text.starts_with('[')
            {
                tokyo_import_openapi::import_openapi_json_document(openapi_input_text).or_else(
                    |_| tokyo_import_openapi::import_openapi_yaml_document(openapi_input_text),
                )
            } else {
                tokyo_import_openapi::import_openapi_yaml_document(openapi_input_text).or_else(
                    |_| tokyo_import_openapi::import_openapi_json_document(openapi_input_text),
                )
            }
        }
    }
    .map_err(|error| Error::Import(error.to_string()))?;
    let mut configured_api_ir = imported_api_ir;
    apply_codegen_config_to_api(&mut configured_api_ir, codegen_config)?;
    configured_api_ir.canonicalize();
    Ok(configured_api_ir)
}

/// Applies user configuration to imported API IR in place.
///
/// # Errors
///
/// Returns [`Error`] when dispatch groups, scenario definitions, OAuth
/// providers, or mock-login environment constraints are invalid.
pub fn apply_codegen_config_to_api(api_ir: &mut Api, codegen_config: &Config) -> Result<(), Error> {
    api_ir.cli.package_name.clone_from(&codegen_config.package);
    api_ir.cli.cli_name.clone_from(&codegen_config.cli_name);
    api_ir
        .sdk
        .package_name
        .clone_from(&codegen_config.sdk.package);
    api_ir
        .sdk
        .client_name
        .clone_from(&codegen_config.sdk.client_name);
    if let Some(default_base_url) = &codegen_config.base_url {
        api_ir.sdk.base_url = Some(default_base_url.clone());
    }
    if !codegen_config.environments.is_empty() {
        api_ir
            .cli
            .environments
            .clone_from(&codegen_config.environments);
    }
    if let Some(default_base_url) = &codegen_config.base_url {
        api_ir.cli.base_url = Some(default_base_url.clone());
    }
    if !codegen_config.cli_auth.is_empty() {
        let available_openapi_security_scheme_names: BTreeSet<_> = api_ir
            .endpoints
            .iter()
            .flat_map(|endpoint| &endpoint.auth)
            .flat_map(|alternative| &alternative.schemes)
            .map(|requirement| requirement.scheme.name.as_str())
            .collect();
        for (security_scheme_name, cli_auth_provider) in &codegen_config.cli_auth {
            if !available_openapi_security_scheme_names.contains(security_scheme_name.as_str()) {
                return codegen_config_error(format!(
                    "cli_auth provider {security_scheme_name:?} does not match an OpenAPI security scheme"
                ));
            }
            validate_cli_auth_provider(
                security_scheme_name,
                cli_auth_provider,
                &api_ir.cli.environments,
            )?;
        }
        api_ir.cli.cli_auth.clone_from(&codegen_config.cli_auth);
    }
    if !codegen_config.cli_scenarios.is_empty() {
        let mut configured_scenario_names = BTreeSet::new();
        let mut cli_scenarios = Vec::with_capacity(codegen_config.cli_scenarios.len());
        for configured_scenario in &codegen_config.cli_scenarios {
            if configured_scenario.name.trim().is_empty() {
                return codegen_config_error("cli_scenarios name must not be empty");
            }
            if configured_scenario.name == "list" {
                return codegen_config_error("cli_scenarios name \"list\" is reserved");
            }
            if !configured_scenario_names.insert(configured_scenario.name.as_str()) {
                return codegen_config_error(format!(
                    "duplicate cli_scenarios name {:?}",
                    configured_scenario.name
                ));
            }
            let scenario_body = match (&configured_scenario.body, &configured_scenario.file) {
                (Some(inline_scenario_body), None) => inline_scenario_body.clone(),
                (None, Some(file)) => {
                    return codegen_config_error(format!(
                        "cli_scenarios {:?} file {file:?} must be resolved by the codegen front end",
                        configured_scenario.name
                    ));
                }
                (Some(_), Some(_)) => {
                    return codegen_config_error(format!(
                        "cli_scenarios {:?} must set exactly one of body or file",
                        configured_scenario.name
                    ));
                }
                (None, None) => {
                    return codegen_config_error(format!(
                        "cli_scenarios {:?} must set exactly one of body or file",
                        configured_scenario.name
                    ));
                }
            };
            for allowed_environment_name in &configured_scenario.allowed_environments {
                if !api_ir
                    .cli
                    .environments
                    .contains_key(allowed_environment_name)
                {
                    return codegen_config_error(format!(
                        "cli_scenarios {:?} allowed_environments references unknown environment {allowed_environment_name:?}",
                        configured_scenario.name
                    ));
                }
            }
            cli_scenarios.push(CliScenario {
                name: configured_scenario.name.clone(),
                description: configured_scenario.description.clone(),
                body: scenario_body,
                allowed_environments: configured_scenario.allowed_environments.clone(),
            });
        }
        api_ir.cli.cli_scenarios = cli_scenarios;
    }
    if !codegen_config.cli_dispatch_groups.is_empty() {
        let safe_identity_fields_exposed_by_auth_providers = api_ir
            .cli
            .cli_auth
            .values()
            .filter_map(|provider| match &provider.endpoints {
                OAuthEndpoints::BrowserToken {
                    identity_fields, ..
                } => Some(identity_fields.keys()),
                _ => None,
            })
            .flatten()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        api_ir.cli.cli_dispatch_groups = validate_dispatch_groups(
            &api_ir.endpoints,
            &codegen_config.cli_dispatch_groups,
            &safe_identity_fields_exposed_by_auth_providers,
        )?;
    }
    Ok(())
}

fn validate_dispatch_groups(
    endpoints: &[Endpoint],
    groups: &[CliDispatchGroupConfig],
    safe_identity_fields: &BTreeSet<&str>,
) -> Result<Vec<CliDispatchGroup>, Error> {
    let mut public_commands = BTreeSet::new();
    let mut result = Vec::with_capacity(groups.len());
    for group in groups {
        if group.resource.trim().is_empty() || group.name.trim().is_empty() {
            return codegen_config_error("cli_dispatch_groups resource and name must not be empty");
        }
        if !public_commands.insert((group.resource.as_str(), group.name.as_str())) {
            return codegen_config_error(format!(
                "duplicate cli_dispatch_groups command {:?}.{:?}",
                group.resource, group.name
            ));
        }
        if group.members.is_empty() {
            return codegen_config_error(format!(
                "cli_dispatch_groups {:?}.{:?} requires at least one member",
                group.resource, group.name
            ));
        }

        let mut member_names = BTreeSet::new();
        let mut views = BTreeSet::new();
        let mut resolved = Vec::with_capacity(group.members.len());
        let mut selected = Vec::with_capacity(group.members.len());
        for member in &group.members {
            if member.name.trim().is_empty() || member.path.trim().is_empty() {
                return codegen_config_error(format!(
                    "cli_dispatch_groups {:?}.{:?} member name and path must not be empty",
                    group.resource, group.name
                ));
            }
            if !member_names.insert(member.name.as_str()) {
                return codegen_config_error(format!(
                    "cli_dispatch_groups {:?}.{:?} has duplicate member {:?}",
                    group.resource, group.name, member.name
                ));
            }
            if let Some(field) = member
                .identity
                .keys()
                .find(|field| !safe_identity_fields.contains(field.as_str()))
            {
                return codegen_config_error(format!(
                    "cli_dispatch_groups {:?}.{:?} member {:?} identity field {field:?} is not exposed by cli_auth identity_fields",
                    group.resource, group.name, member.name
                ));
            }
            if let Some(view) = member.view.as_deref()
                && (view.trim().is_empty() || !views.insert(view))
            {
                return codegen_config_error(format!(
                    "cli_dispatch_groups {:?}.{:?} has an empty or duplicate view {view:?}",
                    group.resource, group.name
                ));
            }
            let method = member
                .method
                .as_deref()
                .map(parse_http_method)
                .transpose()?;
            let matches = endpoints
                .iter()
                .filter(|endpoint| {
                    endpoint.path == member.path
                        && method.is_none_or(|method| endpoint.method == method)
                })
                .collect::<Vec<_>>();
            let endpoint = match matches.as_slice() {
                [endpoint] => *endpoint,
                [] => {
                    return codegen_config_error(format!(
                        "cli_dispatch_groups {:?}.{:?} member {:?} does not match an endpoint",
                        group.resource, group.name, member.name
                    ));
                }
                _ => {
                    return codegen_config_error(format!(
                        "cli_dispatch_groups {:?}.{:?} member {:?} path is ambiguous; set method",
                        group.resource, group.name, member.name
                    ));
                }
            };
            selected.push(endpoint);
            resolved.push(CliDispatchMember {
                name: member.name.clone(),
                method: endpoint.method,
                path: endpoint.path.clone(),
                identity: member.identity.clone(),
                view: member.view.clone(),
            });
        }
        if !member_names.contains(group.default_member.as_str()) {
            return codegen_config_error(format!(
                "cli_dispatch_groups {:?}.{:?} default_member {:?} is not a member",
                group.resource, group.name, group.default_member
            ));
        }
        let baseline = selected[0];
        for endpoint in selected.iter().skip(1) {
            if baseline.method != endpoint.method
                || !compatible_parameters(&baseline.path_parameters, &endpoint.path_parameters)
                || baseline.query_parameters != endpoint.query_parameters
                || baseline.headers != endpoint.headers
                || baseline.cookies != endpoint.cookies
                || baseline.request_body != endpoint.request_body
                || baseline.request_body_encoding != endpoint.request_body_encoding
                || baseline.request_media_type != endpoint.request_media_type
                || baseline.auth != endpoint.auth
            {
                return codegen_config_error(format!(
                    "cli_dispatch_groups {:?}.{:?} members must have compatible path/query/header/cookie parameters, request type/encoding/media type, and auth",
                    group.resource, group.name
                ));
            }
        }
        result.push(CliDispatchGroup {
            resource: group.resource.clone(),
            name: group.name.clone(),
            description: group.description.clone(),
            default_member: group.default_member.clone(),
            members: resolved,
        });
    }
    Ok(result)
}

fn compatible_parameters(
    left: &[tokyo_ir::http::Parameter],
    right: &[tokyo_ir::http::Parameter],
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.wire_name == right.wire_name
                && left.r#type == right.r#type
                && left.serialization == right.serialization
        })
}

fn parse_http_method(value: &str) -> Result<HttpMethod, Error> {
    match value.to_ascii_uppercase().as_str() {
        "GET" => Ok(HttpMethod::Get),
        "HEAD" => Ok(HttpMethod::Head),
        "POST" => Ok(HttpMethod::Post),
        "PUT" => Ok(HttpMethod::Put),
        "PATCH" => Ok(HttpMethod::Patch),
        "DELETE" => Ok(HttpMethod::Delete),
        "OPTIONS" => Ok(HttpMethod::Options),
        "TRACE" => Ok(HttpMethod::Trace),
        _ => codegen_config_error(format!(
            "cli_dispatch_groups member method {value:?} is not a supported HTTP method"
        )),
    }
}

fn validate_cli_auth_provider(
    scheme: &str,
    provider: &CliAuthProvider,
    environments: &BTreeMap<String, String>,
) -> Result<(), Error> {
    if !matches!(
        provider.endpoints,
        OAuthEndpoints::BrowserToken { .. }
            | OAuthEndpoints::Broker { .. }
            | OAuthEndpoints::WorkloadIdentity { .. }
            | OAuthEndpoints::AgentRegistration { .. }
    ) && provider.client_id.trim().is_empty()
    {
        return codegen_config_error(format!(
            "cli_auth provider {scheme:?} requires a non-empty client_id"
        ));
    }
    match &provider.endpoints {
        OAuthEndpoints::Discovery { issuer } => {
            if !is_secure_auth_url(issuer) {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} issuer must be an https URL (http is allowed only for loopback hosts)"
                ));
            }
        }
        OAuthEndpoints::ResourceDiscovery { resource, issuer } => {
            if !is_secure_auth_url(resource)
                || issuer
                    .as_deref()
                    .is_some_and(|value| !is_secure_auth_url(value))
            {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} resource and selected issuer must be https URLs (http is allowed only for loopback hosts)"
                ));
            }
        }
        OAuthEndpoints::Explicit {
            authorization_url,
            token_url,
            device_authorization_url,
        } => {
            for (field, value) in [
                ("authorization_url", authorization_url.as_deref()),
                ("token_url", Some(token_url.as_str())),
                (
                    "device_authorization_url",
                    device_authorization_url.as_deref(),
                ),
            ] {
                if let Some(value) = value
                    && !is_secure_auth_url(value)
                {
                    return codegen_config_error(format!(
                        "cli_auth provider {scheme:?} {field} must be an https URL (http is allowed only for loopback hosts)"
                    ));
                }
            }
        }
        OAuthEndpoints::Workos { authkit_domain } => {
            if !is_secure_auth_url(authkit_domain) {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} authkit_domain must be an https URL"
                ));
            }
        }
        OAuthEndpoints::Clerk { issuer } => {
            if !is_secure_auth_url(issuer) {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} Clerk issuer must be an https URL"
                ));
            }
        }
        OAuthEndpoints::WorkloadIdentity {
            token_url,
            subject_token_env,
            subject_token_type,
        } => {
            if !is_secure_auth_url(token_url) {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} token_url must be an https URL"
                ));
            }
            if subject_token_env.trim().is_empty() || subject_token_type.trim().is_empty() {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} workload subject_token_env and subject_token_type must not be empty"
                ));
            }
        }
        OAuthEndpoints::Ciba {
            backchannel_authentication_url,
            token_url,
            login_hint_env,
            client_secret_env,
        } => {
            if !is_secure_auth_url(backchannel_authentication_url) || !is_secure_auth_url(token_url)
            {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} CIBA endpoints must use https"
                ));
            }
            if login_hint_env.trim().is_empty()
                || client_secret_env.as_deref().is_some_and(str::is_empty)
            {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} CIBA environment-variable names must not be empty"
                ));
            }
        }
        OAuthEndpoints::Broker { begin_url } => {
            if !is_secure_auth_url(begin_url) {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} broker begin_url must be an https URL"
                ));
            }
        }
        OAuthEndpoints::AgentRegistration {
            authorization_server,
            identity_type,
            login_hint_env,
        } => {
            if !is_secure_auth_url(authorization_server) {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} agent authorization_server must be an https URL"
                ));
            }
            if !matches!(identity_type.as_str(), "anonymous" | "service_auth") {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} agent identity_type must be \"anonymous\" or \"service_auth\""
                ));
            }
            if (identity_type == "service_auth") != login_hint_env.is_some()
                || login_hint_env.as_deref().is_some_and(str::is_empty)
            {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} service_auth requires login_hint_env and anonymous must omit it"
                ));
            }
        }
        OAuthEndpoints::Mock {
            private_key_pem,
            allowed_environments,
            default_ttl_seconds,
        } => {
            if !private_key_pem.contains("PRIVATE KEY") {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} private_key_pem does not look like a PEM-encoded private key"
                ));
            }
            if allowed_environments.is_empty() {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} mode \"mock\" requires a non-empty allowed_environments list"
                ));
            }
            for name in allowed_environments {
                if !environments.contains_key(name) {
                    return codegen_config_error(format!(
                        "cli_auth provider {scheme:?} allowed_environments references unknown environment {name:?}"
                    ));
                }
            }
            if *default_ttl_seconds == 0 {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} default_ttl_seconds must be greater than zero"
                ));
            }
        }
        OAuthEndpoints::MockEnvironment {
            private_key_env,
            allowed_environments,
            default_ttl_seconds,
        } => {
            if private_key_env.trim().is_empty() {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} private_key_env must not be empty"
                ));
            }
            if allowed_environments.is_empty() {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} mode \"mock_environment\" requires allowed_environments"
                ));
            }
            for name in allowed_environments {
                if !environments.contains_key(name) {
                    return codegen_config_error(format!(
                        "cli_auth provider {scheme:?} allowed_environments references unknown environment {name:?}"
                    ));
                }
            }
            if *default_ttl_seconds == 0 {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} default_ttl_seconds must be greater than zero"
                ));
            }
        }
        OAuthEndpoints::BrowserToken {
            login_url,
            validation_url,
            allowed_environments,
            identity_fields,
        } => {
            for (field, value) in [
                ("login_url", login_url.as_str()),
                ("validation_url", validation_url.as_str()),
            ] {
                if !is_secure_auth_url(value) {
                    return codegen_config_error(format!(
                        "cli_auth provider {scheme:?} {field} must be an https URL (http is allowed only for loopback hosts)"
                    ));
                }
            }
            if allowed_environments.is_empty() {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} mode \"browser_token\" requires a non-empty allowed_environments list"
                ));
            }
            for name in allowed_environments {
                if !environments.contains_key(name) {
                    return codegen_config_error(format!(
                        "cli_auth provider {scheme:?} allowed_environments references unknown environment {name:?}"
                    ));
                }
            }
            if provider.redirect_uri.is_some() {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} mode \"browser_token\" does not accept redirect_uri"
                ));
            }
            if !provider.scopes.is_empty() || provider.audience.is_some() {
                return codegen_config_error(format!(
                    "cli_auth provider {scheme:?} mode \"browser_token\" does not accept scopes or audience"
                ));
            }
            for (name, pointer) in identity_fields {
                if name.trim().is_empty() || pointer.is_empty() || !pointer.starts_with('/') {
                    return codegen_config_error(format!(
                        "cli_auth provider {scheme:?} identity field {name:?} must use a non-empty name and an absolute JSON pointer"
                    ));
                }
            }
        }
    }
    if let Some(redirect_uri) = provider.redirect_uri.as_deref()
        && !is_cli_loopback_redirect(redirect_uri)
    {
        return codegen_config_error(format!(
            "cli_auth provider {scheme:?} redirect_uri must match http://127.0.0.1:<PORT>/callback"
        ));
    }
    if provider.scopes.iter().any(|scope| scope.trim().is_empty()) {
        return codegen_config_error(format!(
            "cli_auth provider {scheme:?} scopes must not contain empty values"
        ));
    }
    Ok(())
}

fn is_cli_loopback_redirect(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some_and(|port| port != 0)
        && url.path() == "/callback"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

fn is_secure_auth_url(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => true,
        "http" => matches!(
            url.host_str(),
            Some("127.0.0.1" | "::1" | "[::1]" | "localhost")
        ),
        _ => false,
    }
}

/// Builds the complete generated-file plan, including the persisted IR snapshot.
///
/// # Errors
///
/// Returns [`Error`] when the IR schema version is unsupported, target emission
/// fails, or the snapshot metadata cannot be serialized.
#[must_use = "generated files or generation errors should be handled"]
pub fn build_output_plan<E: Emitter>(
    api: &Api,
    emitter: &E,
    generator_version: &str,
) -> Result<Vec<GeneratedFile>, Error> {
    if !api.has_supported_schema_version() {
        return Err(Error::UnsupportedSchema {
            actual: api.schema_version,
            expected: tokyo_ir::IR_SCHEMA_VERSION,
        });
    }
    let mut files = emitter
        .emit_target_files(api)
        .map_err(|error| Error::Emit(error.to_string()))?;
    let snapshot = Snapshot {
        format_version: SNAPSHOT_FORMAT_VERSION,
        generator_version: generator_version.to_string(),
        api: api.clone(),
    };
    let mut contents = serde_json::to_string_pretty(&snapshot)?;
    contents.push('\n');
    files.push(GeneratedFile {
        relative_path: SNAPSHOT_FILE.to_string(),
        contents,
    });
    Ok(files)
}

fn codegen_config_error<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error::Config(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_applies_cli_dispatch_groups() {
        let source = r##"
openapi: 3.0.3
info: { title: Dispatch, version: 1.0.0 }
components:
  securitySchemes:
    bearerAuth: { type: http, scheme: bearer }
  schemas:
    Lookup: { type: object, required: [include], properties: { include: { type: boolean } } }
paths:
  /orders/provider/{order_id}:
    post:
      operationId: providerOrder
      security: [{ bearerAuth: [] }]
      parameters:
        - { name: order_id, in: path, required: true, schema: { type: string } }
      requestBody:
        required: true
        content: { application/json: { schema: { $ref: "#/components/schemas/Lookup" } } }
      responses: { "200": { description: ok, content: { application/json: { schema: { type: object } } } } }
  /orders/shipper/{order_id}:
    post:
      operationId: shipperOrder
      security: [{ bearerAuth: [] }]
      parameters:
        - { name: order_id, in: path, required: true, schema: { type: string } }
      requestBody:
        required: true
        content: { application/json: { schema: { $ref: "#/components/schemas/Lookup" } } }
      responses: { "200": { description: ok, content: { application/json: { schema: { type: string } } } } }
"##;
        let mut api =
            tokyo_import_openapi::import_openapi_yaml_document(source).expect("fixture imports");
        api.cli.cli_auth.insert(
            "bearerAuth".to_string(),
            CliAuthProvider {
                client_id: String::new(),
                redirect_uri: None,
                endpoints: OAuthEndpoints::BrowserToken {
                    login_url: "https://app.example.test/login".to_string(),
                    validation_url: "https://api.example.test/me".to_string(),
                    allowed_environments: vec!["Development".to_string()],
                    identity_fields: BTreeMap::from([(
                        "org_type".to_string(),
                        "/caller/org_type".to_string(),
                    )]),
                },
                scopes: Vec::new(),
                audience: None,
            },
        );
        let group = CliDispatchGroupConfig {
            resource: "orders".to_string(),
            name: "expanded".to_string(),
            description: Some("Role-aware order".to_string()),
            default_member: "provider".to_string(),
            members: vec![
                CliDispatchMemberConfig {
                    name: "provider".to_string(),
                    method: Some("POST".to_string()),
                    path: "/orders/provider/{order_id}".to_string(),
                    identity: BTreeMap::new(),
                    view: Some("provider".to_string()),
                },
                CliDispatchMemberConfig {
                    name: "shipper".to_string(),
                    method: None,
                    path: "/orders/shipper/{order_id}".to_string(),
                    identity: BTreeMap::from([("org_type".to_string(), "shipper".to_string())]),
                    view: Some("shipper".to_string()),
                },
            ],
        };
        apply_codegen_config_to_api(
            &mut api,
            &Config {
                cli_dispatch_groups: vec![group.clone()],
                ..Config::default()
            },
        )
        .expect("compatible dispatch group applies");
        assert_eq!(api.cli.cli_dispatch_groups[0].default_member, "provider");
        assert_eq!(
            api.cli.cli_dispatch_groups[0].members[1].identity["org_type"],
            "shipper"
        );

        let mut incompatible = tokyo_import_openapi::import_openapi_yaml_document(
            &source.replace("type: boolean", "type: integer"),
        )
        .expect("fixture imports");
        incompatible.cli.cli_auth = api.cli.cli_auth.clone();
        let mut invalid_group = group;
        invalid_group.members[1].path = "/orders/provider/{order_id}".to_string();
        invalid_group.members[1].method = Some("GET".to_string());
        let error = apply_codegen_config_to_api(
            &mut incompatible,
            &Config {
                cli_dispatch_groups: vec![invalid_group],
                ..Config::default()
            },
        )
        .expect_err("missing GET endpoint is rejected");
        assert!(error.to_string().contains("does not match an endpoint"));
    }

    #[test]
    fn applies_interactive_cli_oauth_to_a_named_openapi_scheme() {
        let mut api = tokyo_import_openapi::import_openapi_yaml_document(
            r#"
openapi: 3.0.3
info: { title: OAuth, version: 1.0.0 }
components:
  securitySchemes:
    bearerAuth: { type: http, scheme: bearer }
paths:
  /me:
    get:
      operationId: getMe
      security: [{ bearerAuth: [] }]
      responses: { "200": { description: ok } }
"#,
        )
        .expect("fixture imports");
        let provider = CliAuthProvider {
            client_id: "public-cli-client".to_string(),
            redirect_uri: None,
            endpoints: OAuthEndpoints::Discovery {
                issuer: "https://identity.example.test".to_string(),
            },
            scopes: vec!["openid".to_string(), "offline_access".to_string()],
            audience: Some("https://api.example.test".to_string()),
        };
        let config = Config {
            cli_auth: BTreeMap::from([("bearerAuth".to_string(), provider.clone())]),
            ..Config::default()
        };

        apply_codegen_config_to_api(&mut api, &config).expect("valid OAuth config applies");
        assert_eq!(api.cli.cli_auth["bearerAuth"], provider);
    }

    #[test]
    fn rejects_cli_oauth_for_an_unknown_security_scheme() {
        let mut api = Api::default();
        let config = Config {
            cli_auth: BTreeMap::from([(
                "missing".to_string(),
                CliAuthProvider {
                    client_id: "public-client".to_string(),
                    redirect_uri: None,
                    endpoints: OAuthEndpoints::Discovery {
                        issuer: "https://identity.example.test".to_string(),
                    },
                    scopes: Vec::new(),
                    audience: None,
                },
            )]),
            ..Config::default()
        };

        assert!(
            apply_codegen_config_to_api(&mut api, &config)
                .unwrap_err()
                .to_string()
                .contains("does not match an OpenAPI security scheme")
        );
    }

    #[test]
    fn permits_http_oauth_only_on_exact_loopback_hosts() {
        assert!(is_secure_auth_url("http://localhost:8080/token"));
        assert!(is_secure_auth_url("http://127.0.0.1:8080/token"));
        assert!(is_secure_auth_url("http://[::1]:8080/token"));
        assert!(!is_secure_auth_url("http://localhost.example.com/token"));
        assert!(!is_secure_auth_url("http://127.0.0.1.example.com/token"));
        assert!(!is_secure_auth_url(
            "https://user:password@example.com/token"
        ));
        assert!(is_cli_loopback_redirect("http://127.0.0.1:49152/callback"));
        assert!(!is_cli_loopback_redirect("http://127.0.0.1/callback"));
        assert!(!is_cli_loopback_redirect("http://localhost:49152/callback"));
        assert!(!is_cli_loopback_redirect("http://127.0.0.1:49152/other"));
    }

    #[test]
    fn validates_browser_token_provider_configuration() {
        let environments = BTreeMap::from([(
            "Development".to_string(),
            "https://api.dev.example.test".to_string(),
        )]);
        let provider = CliAuthProvider {
            client_id: String::new(),
            redirect_uri: None,
            endpoints: OAuthEndpoints::BrowserToken {
                login_url: "https://app.example.test/login".to_string(),
                validation_url: "https://api.dev.example.test/me".to_string(),
                allowed_environments: vec!["Development".to_string()],
                identity_fields: BTreeMap::new(),
            },
            scopes: Vec::new(),
            audience: None,
        };
        validate_cli_auth_provider("bearerAuth", &provider, &environments)
            .expect("browser_token does not require client_id");

        let mut unknown_environment = provider.clone();
        let OAuthEndpoints::BrowserToken {
            allowed_environments,
            ..
        } = &mut unknown_environment.endpoints
        else {
            unreachable!()
        };
        *allowed_environments = vec!["Production".to_string()];
        assert!(
            validate_cli_auth_provider("bearerAuth", &unknown_environment, &environments)
                .unwrap_err()
                .to_string()
                .contains("unknown environment")
        );

        let mut invalid = provider;
        invalid.redirect_uri = Some("http://127.0.0.1:49152/callback".to_string());
        invalid.scopes.push("openid".to_string());
        assert!(
            validate_cli_auth_provider("bearerAuth", &invalid, &environments)
                .unwrap_err()
                .to_string()
                .contains("does not accept redirect_uri")
        );
    }

    #[test]
    fn applies_inline_cli_scenarios_and_validates_environment_gates() {
        let mut api = Api::default();
        let scenario = CliScenarioConfig {
            name: "smoke".to_string(),
            description: "Smoke test".to_string(),
            body: Some("items list\n".to_string()),
            file: None,
            allowed_environments: vec!["Development".to_string()],
        };
        let config = Config {
            environments: BTreeMap::from([(
                "Development".to_string(),
                "https://dev.example.test".to_string(),
            )]),
            cli_scenarios: vec![scenario.clone()],
            ..Config::default()
        };
        apply_codegen_config_to_api(&mut api, &config).expect("valid scenario applies");
        assert_eq!(api.cli.cli_scenarios[0].name, "smoke");
        assert_eq!(api.cli.cli_scenarios[0].body, "items list\n");

        let invalid = Config {
            cli_scenarios: vec![CliScenarioConfig {
                allowed_environments: vec!["Production".to_string()],
                ..scenario
            }],
            ..config
        };
        assert!(
            apply_codegen_config_to_api(&mut Api::default(), &invalid)
                .unwrap_err()
                .to_string()
                .contains("unknown environment")
        );
    }

    #[test]
    fn rejects_unresolved_or_ambiguous_scenario_sources() {
        for (body, file) in [
            (None, None),
            (
                Some("items list\n".to_string()),
                Some("smoke.txt".to_string()),
            ),
            (None, Some("smoke.txt".to_string())),
        ] {
            let config = Config {
                cli_scenarios: vec![CliScenarioConfig {
                    name: "smoke".to_string(),
                    description: String::new(),
                    body,
                    file,
                    allowed_environments: Vec::new(),
                }],
                ..Config::default()
            };
            assert!(apply_codegen_config_to_api(&mut Api::default(), &config).is_err());
        }
    }

    #[test]
    fn parses_agent_friendly_provider_modes() {
        let config: Config = serde_json::from_value(serde_json::json!({
            "cli_auth": {
                "workos": {
                    "mode": "workos",
                    "client_id": "client_public",
                    "authkit_domain": "https://example.authkit.app"
                },
                "clerk": {
                    "mode": "clerk",
                    "client_id": "client_public",
                    "issuer": "https://clerk.example.test"
                },
                "workload": {
                    "mode": "workload_identity",
                    "token_url": "https://identity.example.test/token",
                    "subject_token_env": "CI_ID_TOKEN"
                },
                "agent": {
                    "mode": "agent_registration",
                    "authorization_server": "https://identity.example.test",
                    "identity_type": "service_auth",
                    "login_hint_env": "AGENT_USER_EMAIL"
                }
            }
        }))
        .expect("parse provider modes");
        assert!(matches!(
            config.cli_auth["workos"].endpoints,
            OAuthEndpoints::Workos { .. }
        ));
        assert!(matches!(
            config.cli_auth["clerk"].endpoints,
            OAuthEndpoints::Clerk { .. }
        ));
        let OAuthEndpoints::WorkloadIdentity {
            subject_token_type, ..
        } = &config.cli_auth["workload"].endpoints
        else {
            panic!("workload mode")
        };
        assert_eq!(subject_token_type, "urn:ietf:params:oauth:token-type:jwt");
        validate_cli_auth_provider("agent", &config.cli_auth["agent"], &BTreeMap::new())
            .expect("service agent config is valid");
    }
}
