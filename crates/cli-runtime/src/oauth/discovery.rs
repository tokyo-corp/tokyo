//! Standards-based OAuth/OIDC endpoint discovery.

use super::{OAuthEndpoints, OAuthProvider, oauth_http_agent, validate_oauth_endpoint_url};
use crate::error::ClientError;

#[derive(Debug, serde::Deserialize)]
pub(super) struct DiscoveryDocument {
    pub(super) issuer: Option<String>,
    pub(super) authorization_endpoint: Option<String>,
    pub(super) token_endpoint: Option<String>,
    pub(super) device_authorization_endpoint: Option<String>,
    pub(super) scopes_supported: Option<Vec<String>>,
    pub(super) grant_types_supported: Option<Vec<String>>,
    pub(super) code_challenge_methods_supported: Option<Vec<String>>,
    pub(super) token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize)]
struct ProtectedResourceDocument {
    resource: String,
    #[serde(default)]
    authorization_servers: Vec<String>,
}

#[derive(Debug)]
pub(super) struct ResolvedProvider {
    pub(super) authorization_url: Option<String>,
    pub(super) token_url: String,
    pub(super) device_authorization_url: Option<String>,
}

pub(super) fn resolve_oauth_provider_for_scheme(
    provider: &OAuthProvider,
) -> Result<ResolvedProvider, ClientError> {
    let discovered = match provider.endpoints {
        OAuthEndpoints::Discovery { issuer } | OAuthEndpoints::Clerk { issuer } => {
            Some(discover_authorization_server_metadata(issuer)?)
        }
        OAuthEndpoints::ResourceDiscovery { resource, issuer } => {
            Some(discover_provider_from_protected_resource(resource, issuer)?)
        }
        OAuthEndpoints::Explicit { .. } | OAuthEndpoints::Workos { .. } => None,
        OAuthEndpoints::WorkloadIdentity { .. }
        | OAuthEndpoints::Ciba { .. }
        | OAuthEndpoints::Broker { .. }
        | OAuthEndpoints::AgentRegistration { .. } => {
            return Err(ClientError::UnsupportedAuth(
                "this provider acquires credentials without OAuth endpoint resolution".to_string(),
            ));
        }
        OAuthEndpoints::Mock { .. } | OAuthEndpoints::MockEnvironment { .. } => {
            return Err(ClientError::UnsupportedAuth(format!(
                "security scheme {:?} is configured for mock auth; use `auth login --mock` instead of a network OAuth flow",
                provider.scheme
            )));
        }
        OAuthEndpoints::BrowserToken { .. } => {
            return Err(ClientError::UnsupportedAuth(format!(
                "security scheme {:?} is configured for browser-token auth; rerun `auth login --scheme {}`",
                provider.scheme, provider.scheme
            )));
        }
    };
    resolved_oauth_provider_for_scheme(provider, discovered.as_ref())
}

pub(super) fn resolved_oauth_provider_for_scheme(
    provider: &OAuthProvider,
    discovered: Option<&DiscoveryDocument>,
) -> Result<ResolvedProvider, ClientError> {
    let (authorization_url, token_url, device_authorization_url) = match provider.endpoints {
        OAuthEndpoints::Discovery { .. }
        | OAuthEndpoints::ResourceDiscovery { .. }
        | OAuthEndpoints::Clerk { .. } => {
            let metadata = discovered.expect("discovery endpoints are resolved with metadata");
            let token_url = metadata.token_endpoint.clone().ok_or_else(|| {
                ClientError::UnsupportedAuth(
                    "provider does not advertise a token endpoint".to_string(),
                )
            })?;
            (
                metadata.authorization_endpoint.clone(),
                token_url,
                metadata.device_authorization_endpoint.clone(),
            )
        }
        OAuthEndpoints::Explicit {
            authorization_url,
            token_url,
            device_authorization_url,
        } => (
            authorization_url.map(str::to_string),
            token_url.to_string(),
            device_authorization_url.map(str::to_string),
        ),
        OAuthEndpoints::Workos { authkit_domain } => {
            let domain = authkit_domain.trim_end_matches('/');
            (
                None,
                format!("{domain}/oauth2/token"),
                Some(format!("{domain}/oauth2/device_authorization")),
            )
        }
        _ => unreachable!("nonstandard acquisition modes do not resolve OAuth endpoints"),
    };
    validate_oauth_endpoint_url(&token_url)?;
    if let Some(url) = authorization_url.as_deref() {
        validate_oauth_endpoint_url(url)?;
    }
    if let Some(url) = device_authorization_url.as_deref() {
        validate_oauth_endpoint_url(url)?;
    }
    Ok(ResolvedProvider {
        authorization_url,
        token_url,
        device_authorization_url,
    })
}

pub(super) fn discover_authorization_server_metadata(
    issuer: &str,
) -> Result<DiscoveryDocument, ClientError> {
    let issuer = issuer.trim_end_matches('/');
    let oidc_url = well_known_url(issuer, "openid-configuration")?;
    let oauth_url = well_known_url(issuer, "oauth-authorization-server")?;
    let response = match oauth_http_agent().get(&oidc_url).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(_, _)) => {
            oauth_http_agent().get(&oauth_url).call().map_err(|error| {
                ClientError::Transport(format!(
                    "OAuth discovery failed for both {oidc_url} and {oauth_url}: {error}"
                ))
            })?
        }
        Err(error) => {
            return Err(ClientError::Transport(format!(
                "OAuth discovery failed for {oidc_url}: {error}"
            )));
        }
    };
    let metadata: DiscoveryDocument = serde_json::from_reader(response.into_reader())
        .map_err(|error| ClientError::Decode(error.to_string()))?;
    if let Some(discovered_issuer) = metadata.issuer.as_deref()
        && discovered_issuer.trim_end_matches('/') != issuer
    {
        return Err(ClientError::UnsupportedAuth(format!(
            "OAuth discovery issuer mismatch: expected {issuer:?}, received {discovered_issuer:?}"
        )));
    }
    Ok(metadata)
}

pub(super) fn discover_provider_from_protected_resource(
    resource: &str,
    selected_issuer: Option<&str>,
) -> Result<DiscoveryDocument, ClientError> {
    validate_oauth_endpoint_url(resource)?;
    let metadata_url = well_known_url(resource, "oauth-protected-resource")?;
    let response = oauth_http_agent()
        .get(&metadata_url)
        .call()
        .map_err(|error| {
            ClientError::Transport(format!(
                "protected-resource discovery failed for {metadata_url}: {error}"
            ))
        })?;
    let metadata: ProtectedResourceDocument = serde_json::from_reader(response.into_reader())
        .map_err(|error| ClientError::Decode(error.to_string()))?;
    if metadata.resource.trim_end_matches('/') != resource.trim_end_matches('/') {
        return Err(ClientError::UnsupportedAuth(format!(
            "protected-resource metadata mismatch: expected {resource:?}, received {:?}",
            metadata.resource
        )));
    }
    let issuer = match selected_issuer {
        Some(selected)
            if metadata
                .authorization_servers
                .iter()
                .any(|value| value == selected) =>
        {
            selected
        }
        Some(selected) => {
            return Err(ClientError::UnsupportedAuth(format!(
                "selected issuer {selected:?} is not advertised by protected resource {resource:?}"
            )));
        }
        None => match metadata.authorization_servers.as_slice() {
            [issuer] => issuer,
            [] => {
                return Err(ClientError::UnsupportedAuth(
                    "protected-resource metadata advertises no authorization server".to_string(),
                ));
            }
            _ => {
                return Err(ClientError::UnsupportedAuth(
                    "protected-resource metadata advertises multiple authorization servers; configure issuer explicitly".to_string(),
                ));
            }
        },
    };
    discover_authorization_server_metadata(issuer)
}

fn well_known_url(base: &str, suffix: &str) -> Result<String, ClientError> {
    let parsed = url::Url::parse(base).map_err(|error| ClientError::Decode(error.to_string()))?;
    let mut result = format!(
        "{}://{}",
        parsed.scheme(),
        parsed
            .host_str()
            .ok_or_else(|| ClientError::Decode("discovery URL is missing a host".to_string()))?
    );
    if let Some(port) = parsed.port() {
        result.push_str(&format!(":{port}"));
    }
    result.push_str("/.well-known/");
    result.push_str(suffix);
    let path = parsed.path().trim_end_matches('/');
    if !path.is_empty() {
        result.push_str(path);
    }
    Ok(result)
}
