use serde::{Deserialize, Deserializer, Serialize};

use crate::id::EndpointId;

/// Whether an operation can run anonymously, can optionally use caller
/// identity, or always requires credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAccess {
    /// The operation declares no security requirements.
    Public,
    /// At least one anonymous alternative exists alongside authenticated
    /// alternatives.
    Optional,
    /// Every alternative requires one or more credential schemes.
    Authenticated,
}

impl CommandAccess {
    /// Derives command access from OpenAPI Security Requirement Objects.
    #[must_use]
    pub fn from_requirements(requirements: &[AuthRequirement]) -> Self {
        if requirements.is_empty() {
            Self::Public
        } else if requirements
            .iter()
            .any(|requirement| requirement.schemes.is_empty())
        {
            Self::Optional
        } else {
            Self::Authenticated
        }
    }

    /// Stable wire value used by generated schemas and filtering.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Optional => "optional",
            Self::Authenticated => "authenticated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
/// Authentication mechanism declared by an OpenAPI security scheme.
pub enum AuthSchemeKind {
    /// HTTP bearer token.
    Bearer,
    /// HTTP basic authentication.
    Basic,
    /// API key carried in a request header.
    Header {
        /// Header name.
        name: String,
    },
    /// API key carried in a query parameter.
    QueryKey {
        /// Query parameter name.
        name: String,
    },
    /// An API key sent as a cookie. Note this is only reliably settable from a
    /// Node-like `fetch` — browsers forbid scripts from setting the `Cookie` header
    /// directly, so a browser-targeted client can't fully honor this on its own.
    CookieKey {
        /// Cookie name.
        name: String,
    },
    /// OAuth2 bearer token acquired through a configured provider.
    #[serde(alias = "OAuthClientCredentials")]
    OAuth2 {
        /// Token endpoint for client-credentials flows when declared by OpenAPI.
        #[serde(default)]
        token_endpoint: Option<String>,
    },
    /// Token is obtained by calling another endpoint in the same API and mapping
    /// its response onto auth headers, rather than a static scheme OpenAPI can express.
    Inferred {
        /// Endpoint used to acquire credentials.
        via_endpoint: EndpointId,
    },
}

/// A named OpenAPI security scheme. The name is part of its identity: two
/// header keys or bearer schemes with different component names may require
/// different credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthScheme {
    /// OpenAPI security-scheme component name.
    pub name: String,
    /// Concrete credential transport and flow.
    #[serde(flatten)]
    pub kind: AuthSchemeKind,
}

/// One scheme within a Security Requirement Object. Every entry in the object
/// must be satisfied, and OAuth2/OpenID scopes are retained verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSchemeRequirement {
    /// Required scheme.
    pub scheme: AuthScheme,
    /// OAuth scopes required by this scheme.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// One alternative in OpenAPI's security array. An empty `schemes` list is the
/// explicit anonymous-access alternative (`{}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthRequirement {
    /// Schemes that must all be satisfied for this alternative.
    pub schemes: Vec<AuthSchemeRequirement>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AuthRequirementWire {
    Current(CurrentAuthRequirement),
    Legacy(AuthSchemeKind),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentAuthRequirement {
    #[serde(default)]
    schemes: Vec<AuthSchemeRequirement>,
}

impl<'de> Deserialize<'de> for AuthRequirement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match AuthRequirementWire::deserialize(deserializer)? {
            AuthRequirementWire::Current(CurrentAuthRequirement { schemes }) => {
                Ok(Self { schemes })
            }
            AuthRequirementWire::Legacy(kind) => Ok(Self {
                schemes: vec![AuthSchemeRequirement {
                    scheme: AuthScheme {
                        // Older IR did not preserve component identity.
                        name: String::new(),
                        kind,
                    },
                    scopes: Vec::new(),
                }],
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_legacy_flat_auth_scheme() {
        let requirement: AuthRequirement =
            serde_json::from_str(r#"{"kind":"Header","name":"X-Api-Key"}"#)
                .expect("legacy scheme should deserialize");
        assert_eq!(requirement.schemes.len(), 1);
        assert_eq!(requirement.schemes[0].scheme.name, "");
        assert_eq!(
            requirement.schemes[0].scheme.kind,
            AuthSchemeKind::Header {
                name: "X-Api-Key".to_string()
            }
        );
    }

    #[test]
    fn deserializes_legacy_client_credentials_scheme() {
        let requirement: AuthRequirement = serde_json::from_str(
            r#"{"kind":"OAuthClientCredentials","token_endpoint":"https://example.test/token"}"#,
        )
        .expect("legacy OAuth scheme should deserialize");
        assert_eq!(
            requirement.schemes[0].scheme.kind,
            AuthSchemeKind::OAuth2 {
                token_endpoint: Some("https://example.test/token".to_string())
            }
        );
    }

    #[test]
    fn classifies_public_optional_and_authenticated_requirements() {
        let named = AuthRequirement {
            schemes: vec![AuthSchemeRequirement {
                scheme: AuthScheme {
                    name: "bearerAuth".to_string(),
                    kind: AuthSchemeKind::Bearer,
                },
                scopes: Vec::new(),
            }],
        };
        assert_eq!(CommandAccess::from_requirements(&[]), CommandAccess::Public);
        assert_eq!(
            CommandAccess::from_requirements(&[
                AuthRequirement {
                    schemes: Vec::new()
                },
                named.clone(),
            ]),
            CommandAccess::Optional
        );
        assert_eq!(
            CommandAccess::from_requirements(&[named]),
            CommandAccess::Authenticated
        );
    }
}
