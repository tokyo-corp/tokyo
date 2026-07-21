use tokyo_ir::cli_behavior::{CliBehavior, OAuthEndpoints};

pub fn render_generated_cli_runtime_config_source_file(
    cli_behavior_extracted_from_openapi_spec: &CliBehavior,
    generated_cargo_package_name: &str,
    generated_command_name: &str,
) -> String {
    let environment_variable_prefix = generated_command_name
        .to_uppercase()
        .replace(['-', ' '], "_");
    let default_base_url_rust_literal = render_optional_string_as_rust_option_literal(
        cli_behavior_extracted_from_openapi_spec.base_url.as_deref(),
    );
    let named_environment_entries_rust_source = cli_behavior_extracted_from_openapi_spec
        .environments
        .iter()
        .map(|(environment_name, environment_base_url)| {
            format!("    ({environment_name:?}, {environment_base_url:?}),\n")
        })
        .collect::<String>();
    let oauth_provider_entries_rust_source = cli_behavior_extracted_from_openapi_spec
        .cli_auth
        .iter()
        .map(|(security_scheme_name, oauth_provider_config)| {
            let oauth_endpoints_rust_literal = match &oauth_provider_config.endpoints {
                OAuthEndpoints::Discovery { issuer } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::Discovery {{ issuer: {issuer:?} }}"
                ),
                OAuthEndpoints::ResourceDiscovery { resource, issuer } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::ResourceDiscovery {{ resource: {resource:?}, issuer: {} }}",
                    render_optional_string_as_rust_option_literal(issuer.as_deref()),
                ),
                OAuthEndpoints::Explicit {
                    authorization_url,
                    token_url,
                    device_authorization_url,
                } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::Explicit {{ authorization_url: {}, token_url: {token_url:?}, device_authorization_url: {} }}",
                    render_optional_string_as_rust_option_literal(authorization_url.as_deref()),
                    render_optional_string_as_rust_option_literal(device_authorization_url.as_deref()),
                ),
                OAuthEndpoints::Workos { authkit_domain } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::Workos {{ authkit_domain: {authkit_domain:?} }}"
                ),
                OAuthEndpoints::Clerk { issuer } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::Clerk {{ issuer: {issuer:?} }}"
                ),
                OAuthEndpoints::WorkloadIdentity {
                    token_url,
                    subject_token_env,
                    subject_token_type,
                } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::WorkloadIdentity {{ token_url: {token_url:?}, subject_token_env: {subject_token_env:?}, subject_token_type: {subject_token_type:?} }}"
                ),
                OAuthEndpoints::Ciba {
                    backchannel_authentication_url,
                    token_url,
                    login_hint_env,
                    client_secret_env,
                } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::Ciba {{ backchannel_authentication_url: {backchannel_authentication_url:?}, token_url: {token_url:?}, login_hint_env: {login_hint_env:?}, client_secret_env: {} }}",
                    render_optional_string_as_rust_option_literal(client_secret_env.as_deref()),
                ),
                OAuthEndpoints::Broker { begin_url } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::Broker {{ begin_url: {begin_url:?} }}"
                ),
                OAuthEndpoints::AgentRegistration {
                    authorization_server,
                    identity_type,
                    login_hint_env,
                } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::AgentRegistration {{ authorization_server: {authorization_server:?}, identity_type: {identity_type:?}, login_hint_env: {} }}",
                    render_optional_string_as_rust_option_literal(login_hint_env.as_deref()),
                ),
                OAuthEndpoints::Mock {
                    private_key_pem,
                    allowed_environments,
                    default_ttl_seconds,
                } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::Mock {{ private_key_pem: {private_key_pem:?}, allowed_environments: &{:?}, default_ttl_seconds: {default_ttl_seconds} }}",
                    allowed_environments,
                ),
                OAuthEndpoints::MockEnvironment {
                    private_key_env,
                    allowed_environments,
                    default_ttl_seconds,
                } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::MockEnvironment {{ private_key_env: {private_key_env:?}, allowed_environments: &{:?}, default_ttl_seconds: {default_ttl_seconds} }}",
                    allowed_environments,
                ),
                OAuthEndpoints::BrowserToken {
                    login_url,
                    validation_url,
                    allowed_environments,
                    identity_fields,
                } => format!(
                    "tokyo_cli_runtime::oauth::OAuthEndpoints::BrowserToken {{ login_url: {login_url:?}, validation_url: {validation_url:?}, allowed_environments: &{:?}, identity_fields: &{:?} }}",
                    allowed_environments,
                    identity_fields.iter().collect::<Vec<_>>(),
                ),
            };
            format!(
                "    tokyo_cli_runtime::oauth::OAuthProvider {{ scheme: {security_scheme_name:?}, client_id: {:?}, redirect_uri: {}, endpoints: {oauth_endpoints_rust_literal}, scopes: &{:?}, audience: {} }},\n",
                oauth_provider_config.client_id,
                render_optional_string_as_rust_option_literal(oauth_provider_config.redirect_uri.as_deref()),
                oauth_provider_config.scopes,
                render_optional_string_as_rust_option_literal(oauth_provider_config.audience.as_deref()),
            )
        })
        .collect::<String>();
    let embedded_scenario_entries_rust_source = cli_behavior_extracted_from_openapi_spec
        .cli_scenarios
        .iter()
        .map(|embedded_scenario| {
            format!(
                "    tokyo_cli_runtime::CliScenario {{ name: {:?}, description: {:?}, body: {:?}, allowed_environments: &{:?} }},\n",
                embedded_scenario.name,
                embedded_scenario.description,
                embedded_scenario.body,
                embedded_scenario.allowed_environments,
            )
        })
        .collect::<String>();

    format!(
        "pub const CONFIG: tokyo_cli_runtime::RuntimeConfig = \n\
         tokyo_cli_runtime::RuntimeConfig {{\n\
         \x20   identity: tokyo_cli_runtime::ProductIdentity {{\n\
         \x20       package_name: {generated_cargo_package_name:?},\n\
         \x20       command_name: {generated_command_name:?},\n\
         \x20       env_prefix: {environment_variable_prefix:?},\n\
         \x20   }},\n\
         \x20   default_base_url: {default_base_url_rust_literal},\n\
         \x20   environments: &[\n{named_environment_entries_rust_source}],\n\
         \x20   oauth_providers: &[\n{oauth_provider_entries_rust_source}],\n\
         \x20   scenarios: &[\n{embedded_scenario_entries_rust_source}],\n\
         \x20   update: None,\n\
         }};\n"
    )
}

/// Renders `Some({value:?})` / `None` as Rust source for an `Option<&str>` config field.
fn render_optional_string_as_rust_option_literal(optional_string_value: Option<&str>) -> String {
    optional_string_value
        .map(|inner_string_value| format!("Some({inner_string_value:?})"))
        .unwrap_or_else(|| "None".to_string())
}
