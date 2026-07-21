use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use oauth2::basic::{BasicClient, BasicTokenResponse};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, DeviceAuthorizationUrl, PkceCodeChallenge,
    RedirectUrl, RefreshToken, Scope, StandardDeviceAuthorizationResponse, TokenResponse as _,
    TokenUrl,
};

mod acquisition;
mod discovery;
mod storage;

use acquisition::*;
use discovery::*;
use storage::*;
#[derive(Clone, Copy, Debug)]
/// OAuth provider compiled into a generated CLI.
pub struct OAuthProvider {
    /// OpenAPI security-scheme name this provider satisfies.
    pub scheme: &'static str,
    /// Public OAuth client identifier.
    pub client_id: &'static str,
    /// Optional fixed redirect URI.
    pub redirect_uri: Option<&'static str>,
    /// Endpoint discovery or explicit endpoint configuration.
    pub endpoints: OAuthEndpoints,
    /// OAuth scopes requested during login.
    pub scopes: &'static [&'static str],
    /// Optional OAuth audience parameter.
    pub audience: Option<&'static str>,
}

/// A provider's endpoints are either discovered from an OIDC issuer, given
/// explicitly, or minted locally against a mock signing key; the generator
/// only ever emits one of the configured modes (see
/// `tokyo_ir::cli_behavior::OAuthEndpoints`).
#[derive(Clone, Copy, Debug)]
pub enum OAuthEndpoints {
    /// Discover endpoints from an OIDC issuer.
    Discovery {
        /// OIDC issuer URL.
        issuer: &'static str,
    },
    /// Discover an authorization server through RFC 9728 protected-resource
    /// metadata.
    ResourceDiscovery {
        /// Protected-resource identifier.
        resource: &'static str,
        /// Optional selected issuer when several are advertised.
        issuer: Option<&'static str>,
    },
    /// Use explicitly configured endpoints.
    Explicit {
        /// Optional authorization endpoint.
        authorization_url: Option<&'static str>,
        /// Token endpoint.
        token_url: &'static str,
        /// Optional device authorization endpoint.
        device_authorization_url: Option<&'static str>,
    },
    /// WorkOS AuthKit public-client preset.
    Workos {
        /// AuthKit domain.
        authkit_domain: &'static str,
    },
    /// Clerk public-client OAuth preset.
    Clerk {
        /// Clerk issuer/Frontend API URL.
        issuer: &'static str,
    },
    /// RFC 8693 exchange of an ambient workload assertion.
    WorkloadIdentity {
        /// Token exchange endpoint.
        token_url: &'static str,
        /// Environment variable containing the subject assertion.
        subject_token_env: &'static str,
        /// RFC 8693 subject token type.
        subject_token_type: &'static str,
    },
    /// OpenID CIBA polling flow.
    Ciba {
        /// Backchannel authentication endpoint.
        backchannel_authentication_url: &'static str,
        /// OAuth token endpoint.
        token_url: &'static str,
        /// Environment variable containing a login hint.
        login_hint_env: &'static str,
        /// Optional environment variable containing a client secret.
        client_secret_env: Option<&'static str>,
    },
    /// HTTPS custom-auth broker.
    Broker {
        /// Endpoint used to begin an authentication attempt.
        begin_url: &'static str,
    },
    /// WorkOS-compatible delegated-agent registration and assertion exchange.
    AgentRegistration {
        /// Authorization-server origin.
        authorization_server: &'static str,
        /// `anonymous` or `service_auth`.
        identity_type: &'static str,
        /// Optional environment variable containing a user login hint.
        login_hint_env: Option<&'static str>,
    },
    /// Mint mock credentials locally.
    Mock {
        /// PEM-encoded private key used to sign mock tokens.
        private_key_pem: &'static str,
        /// Named environments where mock credentials are allowed.
        allowed_environments: &'static [&'static str],
        /// Default token lifetime in seconds.
        default_ttl_seconds: u64,
    },
    /// Mint mock credentials from a runtime-supplied signing key.
    MockEnvironment {
        /// Environment variable containing the PEM key.
        private_key_env: &'static str,
        /// Named environments where mock auth is allowed.
        allowed_environments: &'static [&'static str],
        /// Default token lifetime.
        default_ttl_seconds: u64,
    },
    /// Accept a token copied from a browser login flow.
    BrowserToken {
        /// Browser login URL.
        login_url: &'static str,
        /// Token validation URL.
        validation_url: &'static str,
        /// Named environments where this flow is allowed.
        allowed_environments: &'static [&'static str],
        /// Public identity fields extracted from validation responses.
        identity_fields: &'static [(&'static str, &'static str)],
    },
}

#[derive(Clone, Debug, serde::Serialize)]
/// Current OAuth credential status for a profile and scheme.
pub struct OAuthStatus {
    /// Whether a managed OAuth credential exists.
    pub managed: bool,
    /// Expiration timestamp, if known.
    pub expires_at: Option<u64>,
    /// Whether the credential has a refresh token.
    pub refreshable: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
/// One OAuth diagnostic check.
pub struct AuthDiagnosticCheck {
    /// Check name.
    pub name: &'static str,
    /// Check status.
    pub status: &'static str,
    /// Human-readable check detail.
    pub detail: String,
}

#[derive(Clone, Debug, serde::Serialize)]
/// OAuth diagnostic report for one provider.
pub struct AuthDoctorReport {
    /// OpenAPI security-scheme name.
    pub scheme: String,
    /// Whether all checks passed.
    pub healthy: bool,
    /// Individual diagnostic checks.
    pub checks: Vec<AuthDiagnosticCheck>,
}

/// Returns the only configured OAuth scheme name, when exactly one exists.
#[must_use]
pub fn default_oauth_scheme_name() -> Option<&'static str> {
    let providers = crate::config::runtime_config().oauth_providers;
    (providers.len() == 1).then(|| providers[0].scheme)
}

/// Finds a configured OAuth provider by OpenAPI security-scheme name.
#[must_use]
pub fn oauth_provider_for_scheme(scheme: &str) -> Option<&'static OAuthProvider> {
    crate::config::runtime_config()
        .oauth_providers
        .iter()
        .find(|provider| provider.scheme == scheme)
}

/// Whether a provider's acquisition path requires the user to copy a bearer
/// secret from another application. Agent relay mode refuses this legacy flow.
#[must_use]
pub fn provider_requires_pasted_secret(scheme: &str) -> bool {
    oauth_provider_for_scheme(scheme)
        .is_some_and(|provider| matches!(provider.endpoints, OAuthEndpoints::BrowserToken { .. }))
}

/// Whether a missing credential can be acquired without any user ceremony.
#[must_use]
pub fn provider_supports_noninteractive_acquisition(scheme: &str) -> bool {
    oauth_provider_for_scheme(scheme).is_some_and(|provider| {
        matches!(provider.endpoints, OAuthEndpoints::WorkloadIdentity { .. })
            || matches!(
                provider.endpoints,
                OAuthEndpoints::AgentRegistration {
                    identity_type: "anonymous",
                    ..
                }
            )
    })
}

/// Stable result label for the configured acquisition protocol.
#[must_use]
pub fn provider_acquisition_kind(scheme: &str, device: bool) -> &'static str {
    let Some(provider) = oauth_provider_for_scheme(scheme) else {
        return "manual";
    };
    match provider.endpoints {
        OAuthEndpoints::BrowserToken { .. } => "browser-token",
        OAuthEndpoints::WorkloadIdentity { .. } => "workload-identity",
        OAuthEndpoints::Ciba { .. } => "ciba",
        OAuthEndpoints::Broker { .. } => "broker",
        OAuthEndpoints::AgentRegistration { .. } => "agent-registration",
        _ if device => "oauth-device",
        _ => "oauth-pkce",
    }
}

/// Whether RFC 8628 device authorization is available and should be preferred
/// by an agent relaying a user action.
pub fn provider_supports_device_authorization(
    scheme: &str,
) -> Result<bool, crate::error::ClientError> {
    let Some(provider) = oauth_provider_for_scheme(scheme) else {
        return Ok(false);
    };
    match provider.endpoints {
        OAuthEndpoints::Workos { .. } => Ok(true),
        OAuthEndpoints::Explicit {
            device_authorization_url,
            ..
        } => Ok(device_authorization_url.is_some()),
        OAuthEndpoints::Discovery { .. }
        | OAuthEndpoints::ResourceDiscovery { .. }
        | OAuthEndpoints::Clerk { .. } => Ok(resolve_oauth_provider_for_scheme(provider)?
            .device_authorization_url
            .is_some()),
        _ => Ok(false),
    }
}

/// Reads a credential from a terminal without echoing it.
pub fn prompt_hidden_credential(
    prompt: impl std::fmt::Display,
) -> Result<String, crate::error::ClientError> {
    rpassword::prompt_password(prompt)
        .map_err(|error| crate::error::ClientError::Transport(error.to_string()))
}

/// Returns an exact, copy-pasteable login command for the first configured
/// interactive provider among the missing security schemes.
pub fn login_hint_for_auth_schemes<'a>(
    schemes: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let command = crate::config::runtime_config().identity.command_name;
    schemes.into_iter().find_map(|scheme| {
        let provider = oauth_provider_for_scheme(scheme)?;
        let environment = match provider.endpoints {
            OAuthEndpoints::BrowserToken {
                allowed_environments,
                ..
            }
            | OAuthEndpoints::Mock {
                allowed_environments,
                ..
            }
            | OAuthEndpoints::MockEnvironment {
                allowed_environments,
                ..
            } => allowed_environments.first().copied(),
            _ => None,
        };
        Some(match environment {
            Some(environment) => format!(
                "authenticate first with `{command} --environment {environment} auth login --scheme {scheme}`"
            ),
            None => {
                format!(
                    "authenticate with `{command} auth ensure --scheme {scheme} --interaction relay`"
                )
            }
        })
    })
}

/// Runs diagnostic checks for one configured OAuth provider.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] when the scheme is unknown or a
/// required provider check cannot be performed.
#[must_use = "OAuth diagnostic reports or errors should be handled"]
pub fn run_oauth_provider_doctor(
    scheme: &str,
) -> Result<AuthDoctorReport, crate::error::ClientError> {
    let provider = oauth_provider_for_scheme(scheme).ok_or_else(|| {
        crate::error::ClientError::UnsupportedAuth(format!(
            "security scheme {scheme:?} has no interactive OAuth configuration"
        ))
    })?;
    doctor_oauth_provider_for_scheme(provider)
}

fn doctor_oauth_provider_for_scheme(
    provider: &OAuthProvider,
) -> Result<AuthDoctorReport, crate::error::ClientError> {
    let scheme = provider.scheme;
    let mut checks = Vec::new();
    match provider.endpoints {
        OAuthEndpoints::Mock {
            private_key_pem,
            allowed_environments,
            default_ttl_seconds,
        } => {
            record_oauth_diagnostic_check(
                &mut checks,
                "mock_signing_key",
                jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.as_bytes()).map(|_| {
                    (
                        (),
                        "private_key_pem parses as a valid RSA private key".to_string(),
                    )
                }),
            );
            oauth_diagnostic_check(
                &mut checks,
                "mock_environments",
                "pass",
                format!(
                    "mock credentials are honored for: {} (default TTL {default_ttl_seconds}s)",
                    allowed_environments.join(", ")
                ),
            );
            oauth_diagnostic_check(
                &mut checks,
                "network",
                "pass",
                "mock mode performs no network calls; nothing else to check".to_string(),
            );
            return Ok(build_oauth_doctor_report(scheme, checks));
        }
        OAuthEndpoints::MockEnvironment {
            private_key_env,
            allowed_environments,
            default_ttl_seconds,
        } => {
            let key = std::env::var(private_key_env).map_err(|_| {
                crate::error::ClientError::MissingCredential(format!(
                    "mock auth diagnostics require environment variable {private_key_env}"
                ))
            })?;
            record_oauth_diagnostic_check(
                &mut checks,
                "mock_signing_key",
                jsonwebtoken::EncodingKey::from_rsa_pem(key.as_bytes()).map(|_| {
                    (
                        (),
                        format!("{private_key_env} contains a valid RSA private key"),
                    )
                }),
            );
            oauth_diagnostic_check(
                &mut checks,
                "mock_environments",
                "pass",
                format!(
                    "mock credentials are honored for: {} (default TTL {default_ttl_seconds}s)",
                    allowed_environments.join(", ")
                ),
            );
            return Ok(build_oauth_doctor_report(scheme, checks));
        }
        OAuthEndpoints::BrowserToken {
            login_url,
            validation_url,
            allowed_environments,
            ..
        } => {
            record_oauth_diagnostic_check(
                &mut checks,
                "login_url",
                validate_oauth_endpoint_url(login_url).map(|()| {
                    (
                        (),
                        "browser-token login URL uses a secure transport".to_string(),
                    )
                }),
            );
            record_oauth_diagnostic_check(
                &mut checks,
                "validation_url",
                validate_oauth_endpoint_url(validation_url).map(|()| {
                    (
                        (),
                        "browser-token validation URL uses a secure transport".to_string(),
                    )
                }),
            );
            oauth_diagnostic_check(
                &mut checks,
                "environments",
                "pass",
                format!(
                    "browser-token login is allowed for: {}",
                    allowed_environments.join(", ")
                ),
            );
            return Ok(build_oauth_doctor_report(scheme, checks));
        }
        OAuthEndpoints::Discovery { .. }
        | OAuthEndpoints::ResourceDiscovery { .. }
        | OAuthEndpoints::Explicit { .. }
        | OAuthEndpoints::Workos { .. }
        | OAuthEndpoints::Clerk { .. } => {}
        OAuthEndpoints::WorkloadIdentity { .. } => {
            oauth_diagnostic_check(
                &mut checks,
                "workload_identity",
                "pass",
                "RFC 8693 workload assertion exchange is configured".to_string(),
            );
            return Ok(build_oauth_doctor_report(scheme, checks));
        }
        OAuthEndpoints::Ciba { .. } => {
            oauth_diagnostic_check(
                &mut checks,
                "ciba",
                "pass",
                "OpenID CIBA backchannel polling is configured".to_string(),
            );
            return Ok(build_oauth_doctor_report(scheme, checks));
        }
        OAuthEndpoints::Broker { .. } => {
            oauth_diagnostic_check(
                &mut checks,
                "broker",
                "pass",
                "HTTPS authentication broker is configured".to_string(),
            );
            return Ok(build_oauth_doctor_report(scheme, checks));
        }
        OAuthEndpoints::AgentRegistration { identity_type, .. } => {
            oauth_diagnostic_check(
                &mut checks,
                "agent_registration",
                "pass",
                format!("delegated-agent identity type {identity_type:?} is configured"),
            );
            return Ok(build_oauth_doctor_report(scheme, checks));
        }
    }

    let discovered = match provider.endpoints {
        OAuthEndpoints::Discovery { issuer } | OAuthEndpoints::Clerk { issuer } => {
            let metadata = record_oauth_diagnostic_check(
                &mut checks,
                "discovery",
                discover_authorization_server_metadata(issuer)
                    .map(|metadata| (metadata, format!("validated OIDC metadata for {issuer}"))),
            );
            match metadata {
                Some(metadata) => Some(metadata),
                None => return Ok(build_oauth_doctor_report(scheme, checks)),
            }
        }
        OAuthEndpoints::ResourceDiscovery { resource, issuer } => {
            let metadata = record_oauth_diagnostic_check(
                &mut checks,
                "resource_discovery",
                discover_provider_from_protected_resource(resource, issuer).map(|metadata| {
                    (
                        metadata,
                        format!("validated RFC 9728 metadata for {resource}"),
                    )
                }),
            );
            match metadata {
                Some(metadata) => Some(metadata),
                None => return Ok(build_oauth_doctor_report(scheme, checks)),
            }
        }
        OAuthEndpoints::Workos { .. } => {
            oauth_diagnostic_check(
                &mut checks,
                "discovery",
                "pass",
                "using WorkOS AuthKit public-client device endpoints".to_string(),
            );
            None
        }
        _ => {
            oauth_diagnostic_check(
                &mut checks,
                "discovery",
                "pass",
                "using explicit OAuth endpoints".to_string(),
            );
            None
        }
    };
    let Some(resolved) = record_oauth_diagnostic_check(
        &mut checks,
        "endpoints",
        resolved_oauth_provider_for_scheme(provider, discovered.as_ref()).map(|resolved| {
            (
                resolved,
                "configured endpoints use HTTPS or an exact loopback host".to_string(),
            )
        }),
    ) else {
        return Ok(build_oauth_doctor_report(scheme, checks));
    };

    let pkce_methods = discovered
        .as_ref()
        .and_then(|metadata| metadata.code_challenge_methods_supported.as_ref());
    let (status, detail) = match (resolved.authorization_url.as_deref(), pkce_methods) {
        (None, _) => (
            "warn",
            "provider has no authorization endpoint; browser PKCE is unavailable".to_string(),
        ),
        (Some(_), Some(methods)) if methods.iter().any(|method| method == "S256") => {
            ("pass", "provider advertises S256".to_string())
        }
        (Some(_), Some(_)) => (
            "fail",
            "provider metadata does not advertise S256".to_string(),
        ),
        (Some(_), None) => (
            "warn",
            "authorization endpoint is available, but metadata does not declare PKCE methods"
                .to_string(),
        ),
    };
    oauth_diagnostic_check(&mut checks, "pkce", status, detail);

    let (status, detail) = if resolved.device_authorization_url.is_some() {
        (
            "pass",
            "RFC 8628 device authorization endpoint is available",
        )
    } else {
        (
            "warn",
            "provider does not advertise RFC 8628 device authorization",
        )
    };
    oauth_diagnostic_check(
        &mut checks,
        "device_authorization",
        status,
        detail.to_string(),
    );

    record_oauth_diagnostic_check(
        &mut checks,
        "callback",
        start_loopback_callback_server(provider.redirect_uri).map(|(server, redirect_uri)| {
            drop(server);
            ((), format!("loopback callback can bind at {redirect_uri}"))
        }),
    );

    let (status, detail) = match discovered
        .as_ref()
        .and_then(|metadata| metadata.scopes_supported.as_ref())
    {
        Some(supported) => {
            let missing = provider
                .scopes
                .iter()
                .filter(|scope| !supported.iter().any(|value| value == **scope))
                .copied()
                .collect::<Vec<_>>();
            if missing.is_empty() {
                ("pass", "all configured scopes are advertised".to_string())
            } else {
                (
                    "warn",
                    format!(
                        "metadata does not advertise configured scopes: {} (custom scopes may be omitted)",
                        missing.join(", ")
                    ),
                )
            }
        }
        None => (
            "warn",
            "provider metadata does not advertise supported scopes".to_string(),
        ),
    };
    oauth_diagnostic_check(&mut checks, "scopes", status, detail);

    let requests_refresh = provider.scopes.contains(&"offline_access");
    let (status, detail) = match discovered
        .as_ref()
        .and_then(|metadata| metadata.grant_types_supported.as_ref())
    {
        Some(grants) if grants.iter().any(|grant| grant == "refresh_token") => {
            ("pass", "provider advertises the refresh_token grant")
        }
        Some(_) if requests_refresh => (
            "fail",
            "offline_access is requested but metadata omits the refresh_token grant",
        ),
        Some(_) => ("warn", "provider metadata omits the refresh_token grant"),
        None => ("warn", "provider metadata does not declare grant types"),
    };
    oauth_diagnostic_check(&mut checks, "refresh", status, detail.to_string());

    let (status, detail) = match discovered
        .as_ref()
        .and_then(|metadata| metadata.token_endpoint_auth_methods_supported.as_ref())
    {
        Some(methods) if methods.iter().any(|method| method == "none") => (
            "pass",
            "token endpoint accepts public clients without a client secret",
        ),
        Some(_) => (
            "warn",
            "token endpoint metadata does not advertise authentication method none; some providers, including Microsoft Entra, support public clients without declaring it",
        ),
        None => (
            "warn",
            "provider metadata does not declare token endpoint authentication methods",
        ),
    };
    oauth_diagnostic_check(&mut checks, "public_client", status, detail.to_string());

    Ok(build_oauth_doctor_report(scheme, checks))
}

/// Records a pass/fail check from a `Result` carrying `(value, pass detail)`,
/// returning the value so callers can keep it (or bail on `None`).
fn record_oauth_diagnostic_check<T>(
    checks: &mut Vec<AuthDiagnosticCheck>,
    name: &'static str,
    result: Result<(T, String), impl std::fmt::Display>,
) -> Option<T> {
    match result {
        Ok((value, detail)) => {
            oauth_diagnostic_check(checks, name, "pass", detail);
            Some(value)
        }
        Err(error) => {
            oauth_diagnostic_check(checks, name, "fail", error.to_string());
            None
        }
    }
}

/// Performs an interactive login for a configured OAuth provider.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] when the provider is unknown,
/// environment policy rejects the login, browser/device flow fails, or the
/// credential store cannot be updated.
#[must_use = "OAuth login tokens or errors should be handled"]
pub fn login_with_configured_oauth_provider(
    store: &dyn crate::profile::CredentialStore,
    profile: &str,
    scheme: &str,
    environment_name: Option<&str>,
    use_device_code: bool,
    no_browser: bool,
) -> Result<String, crate::error::ClientError> {
    let provider = oauth_provider_for_scheme(scheme).ok_or_else(|| {
        crate::error::ClientError::UnsupportedAuth(format!(
            "security scheme {scheme:?} has no interactive OAuth configuration"
        ))
    })?;
    if matches!(provider.endpoints, OAuthEndpoints::BrowserToken { .. }) {
        if use_device_code {
            return Err(crate::error::ClientError::UnsupportedAuth(
                "browser_token does not support --device".to_string(),
            ));
        }
        return login_with_browser_token_provider(
            store,
            profile,
            scheme,
            provider,
            environment_name,
            no_browser,
        );
    }
    let non_browser_token = match provider.endpoints {
        OAuthEndpoints::WorkloadIdentity { .. } => Some(login_with_workload_identity(provider)?),
        OAuthEndpoints::Ciba { .. } => Some(login_with_ciba(provider)?),
        OAuthEndpoints::Broker { .. } => Some(login_with_auth_broker(provider)?),
        OAuthEndpoints::AgentRegistration { .. } => {
            Some(login_with_agent_registration(provider, None)?)
        }
        _ => None,
    };
    if let Some(token) = non_browser_token {
        let access_token = token.access_token.clone();
        save_oauth_token_record(store, profile, scheme, token)?;
        return Ok(access_token);
    }
    let resolved = resolve_oauth_provider_for_scheme(provider)?;
    let token = if use_device_code || resolved.authorization_url.is_none() {
        login_with_device_authorization(provider, &resolved, no_browser)?
    } else {
        login_with_pkce_authorization_code(provider, &resolved, no_browser)?
    };
    let access_token = token.access_token.clone();
    save_oauth_token_record(store, profile, scheme, token)?;
    Ok(access_token)
}

fn login_with_browser_token_provider(
    store: &dyn crate::profile::CredentialStore,
    profile: &str,
    scheme: &str,
    provider: &OAuthProvider,
    environment_name: Option<&str>,
    no_browser: bool,
) -> Result<String, crate::error::ClientError> {
    let OAuthEndpoints::BrowserToken {
        login_url,
        validation_url,
        allowed_environments,
        ..
    } = provider.endpoints
    else {
        unreachable!("browser_token_login is only called for browser-token providers");
    };
    let environment_name = require_allowed_browser_token_environment(
        "browser-token auth",
        environment_name,
        allowed_environments,
    )?;
    validate_oauth_endpoint_url(login_url)?;
    validate_oauth_endpoint_url(validation_url)?;

    if no_browser {
        eprintln!("Open this URL to sign in and copy your token:\n{login_url}");
    } else if webbrowser::open(login_url).is_err() {
        eprintln!("warning: could not open a browser; open this URL manually:\n{login_url}");
    }
    let token = rpassword::prompt_password(format!(
        "Paste bearer token for {environment_name} (input hidden): "
    ))
    .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
    login_with_browser_token_provider_token(store, profile, scheme, validation_url, &token)
}

fn login_with_browser_token_provider_token(
    store: &dyn crate::profile::CredentialStore,
    profile: &str,
    scheme: &str,
    validation_url: &str,
    token: &str,
) -> Result<String, crate::error::ClientError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(crate::error::ClientError::MissingCredential(
            "bearer token must not be empty".to_string(),
        ));
    }
    validate_browser_token_with_provider_endpoint(validation_url, token)?;
    let record = StoredOAuthToken {
        access_token: token.to_string(),
        refresh_token: None,
        expires_at: extract_jwt_expiration_timestamp(token),
    };
    save_oauth_token_record(store, profile, scheme, record)?;
    Ok(token.to_string())
}

fn require_allowed_browser_token_environment<'a>(
    mode: &str,
    environment_name: Option<&'a str>,
    allowed_environments: &[&str],
) -> Result<&'a str, crate::error::ClientError> {
    let environment_name = environment_name.ok_or_else(|| {
        crate::error::ClientError::UnsupportedAuth(format!(
            "{mode} requires a selected named environment (use --environment or configure the profile; allowed: {}); refusing to guess from --base-url",
            allowed_environments.join(", ")
        ))
    })?;
    if !allowed_environments.contains(&environment_name) {
        return Err(crate::error::ClientError::UnsupportedAuth(format!(
            "{mode} is not permitted for environment {environment_name:?}; allowed: {}",
            allowed_environments.join(", ")
        )));
    }
    Ok(environment_name)
}

fn validate_browser_token_with_provider_endpoint(
    validation_url: &str,
    token: &str,
) -> Result<(), crate::error::ClientError> {
    validate_oauth_endpoint_url(validation_url)?;
    match oauth_http_agent()
        .get(validation_url)
        .set("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(response) if (200..300).contains(&response.status()) => Ok(()),
        Ok(response) => Err(crate::error::ClientError::UnsupportedAuth(format!(
            "token validation failed with HTTP {}; no credential was stored",
            response.status()
        ))),
        Err(ureq::Error::Status(status, _)) => Err(crate::error::ClientError::UnsupportedAuth(
            format!("token validation failed with HTTP {status}; no credential was stored"),
        )),
        Err(ureq::Error::Transport(error)) => Err(crate::error::ClientError::Transport(format!(
            "token validation request failed: {error}"
        ))),
    }
}

/// Resolves the caller's safe identity projection from the single configured
/// CLI auth provider: an explicit `--token` wins, then the stored profile
/// credential. Errors when no provider, credential, or identity fields are
/// configured — callers that can proceed anonymously should not use this.
pub fn authenticated_caller_project_identity_from_token(
    store: &dyn crate::profile::CredentialStore,
    profile: &str,
    explicit_token: Option<&str>,
) -> Result<serde_json::Value, crate::error::ClientError> {
    let scheme = default_oauth_scheme_name().ok_or_else(|| {
        crate::error::ClientError::MissingCredential(
            "caller identity requires exactly one configured CLI auth provider".to_string(),
        )
    })?;
    let token = match explicit_token {
        Some(token) => token.to_string(),
        None => store
            .get_credential_secret(profile, scheme)?
            .ok_or_else(|| {
                crate::error::ClientError::MissingCredential(format!(
                    "no stored credential for identity scheme {scheme:?} in profile {profile:?}"
                ))
            })?,
    };
    project_identity_from_token(scheme, &token)?.ok_or_else(|| {
        crate::error::ClientError::UnsupportedAuth(format!(
            "identity scheme {scheme:?} has no configured safe identity fields"
        ))
    })
}

/// Revalidates a browser token and returns only explicitly configured caller
/// attributes. This avoids leaking arbitrary validation payloads such as raw
/// claims or credential records through `auth whoami`.
pub fn project_identity_from_token(
    scheme: &str,
    token: &str,
) -> Result<Option<serde_json::Value>, crate::error::ClientError> {
    let Some(provider) = oauth_provider_for_scheme(scheme) else {
        return Ok(project_standard_identity_claims_from_jwt(token));
    };
    let OAuthEndpoints::BrowserToken {
        validation_url,
        identity_fields,
        ..
    } = provider.endpoints
    else {
        return Ok(project_standard_identity_claims_from_jwt(token));
    };
    if identity_fields.is_empty() {
        return Ok(None);
    }
    validate_oauth_endpoint_url(validation_url)?;
    let response = oauth_http_agent()
        .get(validation_url)
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(status, _) => crate::error::ClientError::UnsupportedAuth(format!(
                "token validation failed with HTTP {status}"
            )),
            ureq::Error::Transport(error) => crate::error::ClientError::Transport(format!(
                "token validation request failed: {error}"
            )),
        })?;
    let text = response.into_string().map_err(|error| {
        crate::error::ClientError::Transport(format!(
            "failed to read token validation response: {error}"
        ))
    })?;
    let body: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        crate::error::ClientError::Decode(format!(
            "token validation response is not valid JSON: {error}"
        ))
    })?;
    Ok(Some(project_configured_identity_fields_from_json(
        &body,
        identity_fields,
    )))
}

fn project_standard_identity_claims_from_jwt(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let object = claims.as_object()?;
    let allowlist = [
        "sub",
        "iss",
        "aud",
        "exp",
        "scope",
        "client_id",
        "org_id",
        "act",
    ];
    let selected = allowlist
        .into_iter()
        .filter_map(|name| {
            object
                .get(name)
                .cloned()
                .map(|value| (name.to_string(), value))
        })
        .collect::<serde_json::Map<_, _>>();
    (!selected.is_empty()).then(|| serde_json::Value::Object(selected))
}

fn project_configured_identity_fields_from_json(
    body: &serde_json::Value,
    identity_fields: &[(&str, &str)],
) -> serde_json::Value {
    let selected = identity_fields
        .iter()
        .filter_map(|(name, pointer)| {
            body.pointer(pointer)
                .cloned()
                .map(|value| ((*name).to_string(), value))
        })
        .collect();
    serde_json::Value::Object(selected)
}

fn extract_jwt_expiration_timestamp(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()?
        .get("exp")?
        .as_u64()
}

/// Mint a locally-signed mock credential and store it exactly like a manual
/// `--token`. Performs no network call and opens no browser: `subject`
/// becomes the `sub` claim, `claims` are additional key/value pairs merged in
/// verbatim (each value is parsed as a JSON scalar when possible, so
/// `role=operator` becomes a string and `org_subscription=true` becomes a
/// boolean), and the whole payload is RS256-signed with the provider's
/// configured private key.
///
/// Refuses to run unless `environment_name` is both present and listed in the
/// provider's `allowed_environments` — an explicit `--base-url` or an
/// unlisted environment name is rejected rather than guessed at, since a
/// mock credential is only meaningful against a backend that was itself
/// configured to trust this exact signing key in that environment.
pub fn mint_and_store_mock_oauth_token(
    store: &dyn crate::profile::CredentialStore,
    profile: &str,
    scheme: &str,
    environment_name: Option<&str>,
    subject: &str,
    claims: &[(String, String)],
    ttl_seconds: Option<u64>,
) -> Result<String, crate::error::ClientError> {
    let provider = oauth_provider_for_scheme(scheme).ok_or_else(|| {
        crate::error::ClientError::UnsupportedAuth(format!(
            "security scheme {scheme:?} has no interactive OAuth configuration"
        ))
    })?;
    let (private_key_pem, allowed_environments, default_ttl_seconds) = match provider.endpoints {
        OAuthEndpoints::Mock {
            private_key_pem,
            allowed_environments,
            default_ttl_seconds,
        } => (
            private_key_pem.to_string(),
            allowed_environments,
            default_ttl_seconds,
        ),
        OAuthEndpoints::MockEnvironment {
            private_key_env,
            allowed_environments,
            default_ttl_seconds,
        } => (
            std::env::var(private_key_env).map_err(|_| {
                crate::error::ClientError::MissingCredential(format!(
                    "mock auth requires environment variable {private_key_env}"
                ))
            })?,
            allowed_environments,
            default_ttl_seconds,
        ),
        _ => {
            return Err(crate::error::ClientError::UnsupportedAuth(format!(
                "security scheme {scheme:?} is not configured for mock authentication"
            )));
        }
    };
    let environment_name = environment_name.ok_or_else(|| {
        crate::error::ClientError::UnsupportedAuth(format!(
            "mock auth requires an explicit --environment (one of: {}); refusing to guess from --base-url or a stored profile",
            allowed_environments.join(", ")
        ))
    })?;
    if !allowed_environments.contains(&environment_name) {
        return Err(crate::error::ClientError::UnsupportedAuth(format!(
            "mock auth is not permitted for environment {environment_name:?}; allowed: {}",
            allowed_environments.join(", ")
        )));
    }

    let now = current_unix_timestamp_seconds();
    let ttl = ttl_seconds.unwrap_or(default_ttl_seconds);
    let mut payload = serde_json::Map::new();
    payload.insert(
        "sub".to_string(),
        serde_json::Value::String(subject.to_string()),
    );
    payload.insert(
        "client_id".to_string(),
        serde_json::Value::String(provider.client_id.to_string()),
    );
    payload.insert("iat".to_string(), serde_json::Value::from(now));
    payload.insert(
        "nbf".to_string(),
        serde_json::Value::from(now.saturating_sub(10)),
    );
    payload.insert(
        "exp".to_string(),
        serde_json::Value::from(now.saturating_add(ttl)),
    );
    for (key, value) in claims {
        payload.insert(key.clone(), parse_mock_claim_value(value));
    }

    let key =
        jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.as_bytes()).map_err(|error| {
            crate::error::ClientError::UnsupportedAuth(format!("invalid mock signing key: {error}"))
        })?;
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &serde_json::Value::Object(payload),
        &key,
    )
    .map_err(|error| {
        crate::error::ClientError::UnsupportedAuth(format!(
            "failed to sign mock credential: {error}"
        ))
    })?;
    save_manual_token_as_oauth_credential(store, profile, scheme, &token)?;
    Ok(token)
}

/// Parses a `--claim key=value` value as a JSON scalar (`true`/`false`/`null`,
/// an integer, or a float) when it unambiguously looks like one, and falls
/// back to a plain string otherwise — so `--claim org_subscription=true`
/// round-trips as a boolean the way a hand-authored claim set would.
fn parse_mock_claim_value(raw: &str) -> serde_json::Value {
    match raw {
        "true" => serde_json::Value::Bool(true),
        "false" => serde_json::Value::Bool(false),
        "null" => serde_json::Value::Null,
        _ => raw
            .parse::<i64>()
            .map(serde_json::Value::from)
            .or_else(|_| raw.parse::<f64>().map(serde_json::Value::from))
            .unwrap_or_else(|_| serde_json::Value::String(raw.to_string())),
    }
}

/// Saves a manually supplied bearer token for an OAuth-backed scheme.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the credential store cannot be
/// updated.
pub fn save_manual_token_as_oauth_credential(
    store: &dyn crate::profile::CredentialStore,
    profile: &str,
    scheme: &str,
    token: &str,
) -> Result<(), crate::error::ClientError> {
    let _ = store.delete_credential_secret(profile, &oauth_metadata_cache_key(scheme))?;
    store.save_credential_secret(profile, scheme, token)
}

/// Removes a saved OAuth credential and any cached metadata.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the credential store cannot be
/// updated.
#[must_use = "the boolean indicates whether anything was removed"]
pub fn remove_oauth_credential_and_cached_tokens(
    store: &dyn crate::profile::CredentialStore,
    profile: &str,
    scheme: &str,
) -> Result<bool, crate::error::ClientError> {
    let credential = store.delete_credential_secret(profile, scheme)?;
    let metadata = store.delete_credential_secret(profile, &oauth_metadata_cache_key(scheme))?;
    Ok(credential || metadata)
}

/// Reads status metadata for a saved OAuth credential.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the credential store cannot be read
/// or stored metadata cannot be decoded.
#[must_use = "credential status or errors should be handled"]
pub fn oauth_credential_status(
    store: &dyn crate::profile::CredentialStore,
    profile: &str,
    scheme: &str,
) -> Result<Option<OAuthStatus>, crate::error::ClientError> {
    let Some(raw) = store.get_credential_secret(profile, &oauth_metadata_cache_key(scheme))? else {
        return Ok(None);
    };
    let token = decode_bound_oauth_token_record(scheme, &raw)?;
    Ok(Some(OAuthStatus {
        managed: true,
        expires_at: token.expires_at,
        refreshable: token.refresh_token.is_some(),
    }))
}

/// Return a managed OAuth access token, refreshing shortly before expiry.
/// `None` means this credential is a manual token handled by the normal store.
pub fn load_or_refresh_managed_oauth_access_token(
    store: &dyn crate::profile::CredentialStore,
    profile: &str,
    scheme: &str,
) -> Result<Option<String>, crate::error::ClientError> {
    let Some(raw) = store.get_credential_secret(profile, &oauth_metadata_cache_key(scheme))? else {
        return Ok(None);
    };
    let token = decode_bound_oauth_token_record(scheme, &raw)?;
    let now = current_unix_timestamp_seconds();
    let needs_refresh = token
        .expires_at
        .is_some_and(|expires_at| expires_at <= now.saturating_add(30));
    if !needs_refresh {
        return Ok(Some(token.access_token));
    }
    let _refresh_lock = OAuthRefreshLock::acquire(profile, scheme)?;
    // Another CLI process may have completed rotation while this process was
    // waiting. Re-read under the lock before contacting the provider.
    let raw = store
        .get_credential_secret(profile, &oauth_metadata_cache_key(scheme))?
        .ok_or_else(|| {
            crate::error::ClientError::MissingCredential(format!(
                "stored OAuth metadata for {scheme:?} disappeared during refresh"
            ))
        })?;
    let token = decode_bound_oauth_token_record(scheme, &raw)?;
    let now = current_unix_timestamp_seconds();
    if token
        .expires_at
        .is_none_or(|expires_at| expires_at > now.saturating_add(30))
    {
        return Ok(Some(token.access_token));
    }

    let provider = oauth_provider_for_scheme(scheme);
    if let Some(provider) = provider {
        let reacquired = match provider.endpoints {
            OAuthEndpoints::WorkloadIdentity { .. } => {
                Some(login_with_workload_identity(provider)?)
            }
            OAuthEndpoints::Broker { .. } => Some(login_with_auth_broker(provider)?),
            OAuthEndpoints::AgentRegistration { .. } if token.refresh_token.is_none() => {
                Some(login_with_agent_registration(provider, None)?)
            }
            _ => None,
        };
        if let Some(reacquired) = reacquired {
            let access_token = reacquired.access_token.clone();
            save_oauth_token_record(store, profile, scheme, reacquired)?;
            return Ok(Some(access_token));
        }
    }
    let refresh_token = token.refresh_token.clone().ok_or_else(|| {
        let credential_kind = if oauth_provider_for_scheme(scheme).is_some_and(|provider| {
            matches!(provider.endpoints, OAuthEndpoints::BrowserToken { .. })
        }) {
            "browser-token"
        } else {
            "OAuth"
        };
        crate::error::ClientError::MissingCredential(format!(
            "{credential_kind} credential {scheme:?} expired and cannot be refreshed; rerun `auth login --scheme {scheme}`"
        ))
    })?;
    let provider = provider.ok_or_else(|| {
        crate::error::ClientError::UnsupportedAuth(format!(
            "stored OAuth credential {scheme:?} no longer has provider configuration"
        ))
    })?;
    let token = match provider.endpoints {
        OAuthEndpoints::AgentRegistration { .. } => {
            login_with_agent_registration(provider, Some(&refresh_token))?
        }
        OAuthEndpoints::Ciba {
            token_url,
            client_secret_env,
            ..
        } => refresh_ciba_access_token(provider, token_url, client_secret_env, refresh_token)?,
        _ => {
            let resolved = resolve_oauth_provider_for_scheme(provider)?;
            refresh_managed_oauth_access_token(provider, &resolved, refresh_token)?
        }
    };
    let access_token = token.access_token.clone();
    save_oauth_token_record(store, profile, scheme, token)?;
    Ok(Some(access_token))
}

/// Scope literals converted to owned `Scope` values, shared by the PKCE,
/// device, and refresh request builders (all take `impl IntoIterator<Item = Scope>`).
fn oauth_scopes_for_provider(provider: &OAuthProvider) -> impl Iterator<Item = Scope> + '_ {
    provider
        .scopes
        .iter()
        .map(|scope| Scope::new((*scope).to_string()))
}

fn refresh_managed_oauth_access_token(
    provider: &OAuthProvider,
    resolved: &ResolvedProvider,
    refresh_token: String,
) -> Result<StoredOAuthToken, crate::error::ClientError> {
    let client = BasicClient::new(ClientId::new(provider.client_id.to_string()))
        .set_token_uri(parse_oauth_token_url(&resolved.token_url)?);
    let response = client
        .exchange_refresh_token(&RefreshToken::new(refresh_token.clone()))
        .request(&oauth_http_agent())
        .map_err(oauth_client_error)?;
    Ok(build_oauth_token_record(&response, Some(refresh_token)))
}

fn login_with_pkce_authorization_code(
    provider: &OAuthProvider,
    resolved: &ResolvedProvider,
    no_browser: bool,
) -> Result<StoredOAuthToken, crate::error::ClientError> {
    login_with_pkce_authorization_code_client(
        provider,
        resolved,
        Duration::from_secs(300),
        |authorization_url| {
            eprintln!(
                "{}",
                serde_json::json!({
                    "status": "action_required",
                    "kind": "open_url",
                    "flow": "oauth_pkce",
                    "verification_uri_complete": authorization_url,
                })
            );
            if !no_browser && webbrowser::open(authorization_url.as_str()).is_err() {
                eprintln!("warning: could not open a browser; open the URL manually");
            }
        },
    )
}

fn login_with_pkce_authorization_code_client(
    provider: &OAuthProvider,
    resolved: &ResolvedProvider,
    callback_timeout: Duration,
    open_authorization_url: impl FnOnce(&url::Url),
) -> Result<StoredOAuthToken, crate::error::ClientError> {
    let authorization_url = resolved.authorization_url.as_deref().ok_or_else(|| {
        crate::error::ClientError::UnsupportedAuth(
            "provider does not advertise an authorization endpoint".to_string(),
        )
    })?;
    let (server, redirect_uri) = start_loopback_callback_server(provider.redirect_uri)?;
    let client = BasicClient::new(ClientId::new(provider.client_id.to_string()))
        .set_auth_uri(parse_oauth_authorization_url(authorization_url)?)
        .set_token_uri(parse_oauth_token_url(&resolved.token_url)?)
        .set_redirect_uri(
            RedirectUrl::new(redirect_uri.clone())
                .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?,
        );
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut request = client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(challenge)
        .add_scopes(oauth_scopes_for_provider(provider));
    if let Some(audience) = provider.audience {
        request = request.add_extra_param("audience", audience);
    }
    let (authorization_url, csrf) = request.url();

    open_authorization_url(&authorization_url);
    let callback = wait_for_loopback_oauth_callback(&server, callback_timeout)?;
    let returned_state = CsrfToken::new(callback.state);
    if returned_state != csrf {
        return Err(crate::error::ClientError::UnsupportedAuth(
            "OAuth callback state did not match the login request".to_string(),
        ));
    }
    let response = client
        .exchange_code(AuthorizationCode::new(callback.code))
        .set_pkce_verifier(verifier)
        .request(&oauth_http_agent())
        .map_err(oauth_client_error)?;
    Ok(build_oauth_token_record(&response, None))
}

fn start_loopback_callback_server(
    configured_redirect_uri: Option<&str>,
) -> Result<(tiny_http::Server, String), crate::error::ClientError> {
    if let Some(redirect_uri) = configured_redirect_uri {
        let url = url::Url::parse(redirect_uri)
            .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.path() != "/callback"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(crate::error::ClientError::UnsupportedAuth(
                "OAuth redirect_uri must match http://127.0.0.1:<PORT>/callback".to_string(),
            ));
        }
        let port = url.port().ok_or_else(|| {
            crate::error::ClientError::UnsupportedAuth(
                "OAuth redirect_uri must include a fixed port".to_string(),
            )
        })?;
        let server = tiny_http::Server::http(("127.0.0.1", port))
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        return Ok((server, redirect_uri.to_string()));
    }

    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| {
            crate::error::ClientError::Transport(
                "OAuth callback listener did not bind an IP address".to_string(),
            )
        })?
        .port();
    Ok((server, format!("http://127.0.0.1:{port}/callback")))
}

fn login_with_device_authorization(
    provider: &OAuthProvider,
    resolved: &ResolvedProvider,
    no_browser: bool,
) -> Result<StoredOAuthToken, crate::error::ClientError> {
    login_with_device_authorization_timeout(
        provider,
        resolved,
        no_browser,
        Duration::from_secs(600),
    )
}

fn login_with_device_authorization_timeout(
    provider: &OAuthProvider,
    resolved: &ResolvedProvider,
    no_browser: bool,
    timeout: Duration,
) -> Result<StoredOAuthToken, crate::error::ClientError> {
    let device_url = resolved
        .device_authorization_url
        .as_deref()
        .ok_or_else(|| {
            crate::error::ClientError::UnsupportedAuth(
                "provider does not advertise a device authorization endpoint".to_string(),
            )
        })?;
    let client = BasicClient::new(ClientId::new(provider.client_id.to_string()))
        .set_token_uri(parse_oauth_token_url(&resolved.token_url)?)
        .set_device_authorization_url(
            DeviceAuthorizationUrl::new(device_url.to_string())
                .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?,
        );
    let mut request = client
        .exchange_device_code()
        .add_scopes(oauth_scopes_for_provider(provider));
    if let Some(audience) = provider.audience {
        request = request.add_extra_param("audience", audience);
    }
    let details: StandardDeviceAuthorizationResponse = request
        .request(&oauth_http_agent())
        .map_err(oauth_client_error)?;
    let browser_url = details
        .verification_uri_complete()
        .map(|url| url.secret().to_string())
        .unwrap_or_else(|| details.verification_uri().to_string());
    eprintln!(
        "{}",
        serde_json::json!({
            "status": "action_required",
            "kind": "user_approval",
            "flow": "oauth_device",
            "verification_uri": details.verification_uri().as_str(),
            "verification_uri_complete": browser_url,
            "user_code": details.user_code().secret(),
            "expires_in": details.expires_in().as_secs(),
        })
    );
    if !no_browser && webbrowser::open(&browser_url).is_err() {
        eprintln!("warning: could not open a browser; open the URL manually");
    }
    let response = client
        .exchange_device_access_token(&details)
        .request(&oauth_http_agent(), std::thread::sleep, Some(timeout))
        .map_err(oauth_client_error)?;
    Ok(build_oauth_token_record(&response, None))
}

struct Callback {
    code: String,
    state: String,
}

fn wait_for_loopback_oauth_callback(
    server: &tiny_http::Server,
    timeout: Duration,
) -> Result<Callback, crate::error::ClientError> {
    let request = server
        .recv_timeout(timeout)
        .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?
        .ok_or_else(|| {
            crate::error::ClientError::Transport(
                "timed out waiting for OAuth browser callback".to_string(),
            )
        })?;
    if request.method() != &tiny_http::Method::Get {
        respond_to_browser_callback(request, 405, "Only GET callbacks are accepted.");
        return Err(crate::error::ClientError::Decode(
            "unexpected OAuth callback method".to_string(),
        ));
    }
    let result = parse_loopback_callback_target(request.url());
    match &result {
        Ok(_) => respond_to_browser_callback(
            request,
            200,
            "Authentication complete. You can close this window.",
        ),
        Err(_) => respond_to_browser_callback(request, 400, "Authentication was not completed."),
    }
    result
}

fn parse_loopback_callback_target(target: &str) -> Result<Callback, crate::error::ClientError> {
    let url = url::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
    if url.path() != "/callback" {
        return Err(crate::error::ClientError::Decode(
            "unexpected OAuth callback path".to_string(),
        ));
    }
    let values = url
        .query_pairs()
        .collect::<std::collections::BTreeMap<_, _>>();
    if let Some(error) = values.get("error") {
        return Err(crate::error::ClientError::UnsupportedAuth(format!(
            "authorization server returned {error}"
        )));
    }
    let code = values.get("code").map(|value| value.to_string());
    let state = values.get("state").map(|value| value.to_string());
    match (code, state) {
        (Some(code), Some(state)) => Ok(Callback { code, state }),
        _ => Err(crate::error::ClientError::Decode(
            "OAuth callback omitted code or state".to_string(),
        )),
    }
}

fn respond_to_browser_callback(request: tiny_http::Request, status: u16, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>CLI authentication</title><p>{message}</p>"
    );
    let mut response = tiny_http::Response::from_string(body).with_status_code(status);
    if let Ok(header) = tiny_http::Header::from_bytes(b"Content-Type", b"text/html; charset=utf-8")
    {
        response.add_header(header);
    }
    if let Ok(header) = tiny_http::Header::from_bytes(b"Cache-Control", b"no-store") {
        response.add_header(header);
    }
    let _ = request.respond(response);
}

fn build_oauth_token_record(
    response: &BasicTokenResponse,
    previous_refresh: Option<String>,
) -> StoredOAuthToken {
    StoredOAuthToken {
        access_token: response.access_token().secret().to_string(),
        refresh_token: response
            .refresh_token()
            .map(|token| token.secret().to_string())
            .or(previous_refresh),
        expires_at: response
            .expires_in()
            .map(|duration| current_unix_timestamp_seconds().saturating_add(duration.as_secs())),
    }
}

fn parse_oauth_authorization_url(value: &str) -> Result<AuthUrl, crate::error::ClientError> {
    AuthUrl::new(value.to_string())
        .map_err(|error| crate::error::ClientError::Decode(error.to_string()))
}

fn oauth_http_agent() -> ureq::Agent {
    // OAuth token and discovery requests must not follow redirects: doing so can
    // forward credentials to an attacker-controlled destination.
    ureq::AgentBuilder::new().redirects(0).build()
}

fn parse_oauth_token_url(value: &str) -> Result<TokenUrl, crate::error::ClientError> {
    TokenUrl::new(value.to_string())
        .map_err(|error| crate::error::ClientError::Decode(error.to_string()))
}

fn validate_oauth_endpoint_url(value: &str) -> Result<(), crate::error::ClientError> {
    let url = url::Url::parse(value)
        .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
    let allowed = match url.scheme() {
        "https" => true,
        "http" => matches!(
            url.host_str(),
            Some("127.0.0.1" | "::1" | "[::1]" | "localhost")
        ),
        _ => false,
    };
    if !allowed || !url.username().is_empty() || url.password().is_some() {
        return Err(crate::error::ClientError::UnsupportedAuth(
            "OAuth endpoints must use https (http is allowed only for loopback hosts)".to_string(),
        ));
    }
    Ok(())
}

fn oauth_client_error(error: impl std::fmt::Display) -> crate::error::ClientError {
    crate::error::ClientError::Transport(format!("OAuth request failed: {error}"))
}

fn oauth_diagnostic_check(
    checks: &mut Vec<AuthDiagnosticCheck>,
    name: &'static str,
    status: &'static str,
    detail: String,
) {
    checks.push(AuthDiagnosticCheck {
        name,
        status,
        detail,
    });
}

fn build_oauth_doctor_report(scheme: &str, checks: Vec<AuthDiagnosticCheck>) -> AuthDoctorReport {
    AuthDoctorReport {
        scheme: scheme.to_string(),
        healthy: !checks.iter().any(|check| check.status == "fail"),
        checks,
    }
}

fn current_unix_timestamp_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests;
