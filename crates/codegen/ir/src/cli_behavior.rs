use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::http::HttpMethod;

/// Configuration consumed by the generated CLI target.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CliBehavior {
    /// Generated Cargo package name.
    pub package_name: Option<String>,
    /// Optional executable name.
    pub cli_name: Option<String>,
    /// Named API environments exposed by the generated CLI.
    pub environments: BTreeMap<String, String>,
    /// Default API base URL advertised by the generated CLI.
    pub base_url: Option<String>,
    /// Interactive login providers keyed by the OpenAPI security-scheme name
    /// whose credential they acquire.
    pub cli_auth: BTreeMap<String, CliAuthProvider>,
    /// Named, ordered CLI programs embedded into generated CLI binaries.
    pub cli_scenarios: Vec<CliScenario>,
    /// Configured public commands which dispatch to one of several compatible
    /// OpenAPI operations using safe caller identity fields.
    pub cli_dispatch_groups: Vec<CliDispatchGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Configuration for one facade command that dispatches across compatible operations.
pub struct CliDispatchGroup {
    /// Resource group containing the command.
    pub resource: String,
    /// Public command name.
    pub name: String,
    /// Optional command description.
    #[serde(default)]
    pub description: Option<String>,
    /// Member selected when no more specific selector matches.
    pub default_member: String,
    /// Candidate operations for this dispatch command.
    pub members: Vec<CliDispatchMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One operation candidate inside a dispatch command.
pub struct CliDispatchMember {
    /// Member name used in diagnostics and configuration.
    pub name: String,
    /// HTTP method of the target operation.
    pub method: HttpMethod,
    /// OpenAPI path template of the target operation.
    pub path: String,
    /// Exact matches against public names from `identity_fields`.
    #[serde(default)]
    pub identity: BTreeMap<String, String>,
    /// Optional value accepted by the facade command's `--view` flag.
    #[serde(default)]
    pub view: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Scenario program embedded into generated CLI help and execution.
pub struct CliScenario {
    /// Scenario command name.
    pub name: String,
    /// Human-facing scenario summary.
    pub description: String,
    /// Scenario command body.
    pub body: String,
    /// Named environments where the scenario is allowed to run.
    #[serde(default)]
    pub allowed_environments: Vec<String>,
}

/// Public OAuth client configuration embedded into a generated CLI. Client
/// secrets are intentionally unsupported: installed CLI binaries cannot keep
/// one confidential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliAuthProvider {
    /// Public OAuth client identifier.
    #[serde(default)]
    pub client_id: String,
    /// Optional fixed loopback callback for providers that require exact
    /// redirect URI registration. When absent, the CLI uses an ephemeral port.
    pub redirect_uri: Option<String>,
    /// Endpoint recipe for acquiring tokens.
    #[serde(flatten)]
    pub endpoints: OAuthEndpoints,
    /// OAuth scopes requested during login.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Optional OAuth audience parameter.
    pub audience: Option<String>,
}

/// A provider's endpoints are either discovered from an OIDC issuer or given
/// explicitly — no configured provider recipe needs both, so the config shape
/// makes the two mutually exclusive instead of leaving it to runtime checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum OAuthEndpoints {
    /// Endpoints and capabilities are discovered from `{issuer}/.well-known/openid-configuration`.
    Discovery {
        /// OIDC issuer URL.
        issuer: String,
    },
    /// Discover the authorization server from RFC 9728 protected-resource
    /// metadata, then discover its OAuth/OIDC endpoints. This is the most
    /// portable mode for agent-operated clients because the API itself is the
    /// trust anchor and can move identity providers without regenerating the
    /// CLI.
    ResourceDiscovery {
        /// RFC 9728 protected-resource identifier.
        resource: String,
        /// Optional authorization-server issuer selection when the resource
        /// advertises more than one.
        issuer: Option<String>,
    },
    /// Endpoints are configured directly, for providers without conforming OIDC discovery.
    Explicit {
        /// Authorization endpoint for browser-based flows.
        authorization_url: Option<String>,
        /// Token endpoint.
        token_url: String,
        /// Device authorization endpoint for device-code flows.
        device_authorization_url: Option<String>,
    },
    /// WorkOS AuthKit/Connect preset. The runtime uses the standards-based
    /// public-client device endpoints below this domain; no WorkOS SDK or
    /// client secret is embedded in the generated binary.
    Workos {
        /// AuthKit domain, for example `https://example.authkit.app`.
        authkit_domain: String,
    },
    /// Clerk OAuth preset. Clerk exposes RFC 8414 authorization-server
    /// metadata and public-client PKCE.
    Clerk {
        /// Clerk Frontend API/custom OAuth issuer URL.
        issuer: String,
    },
    /// Exchange an ambient workload assertion for an access token using RFC
    /// 8693. This is the browserless path for CI, developer automation, and
    /// service agents. Only the environment-variable name is compiled in.
    WorkloadIdentity {
        /// OAuth token-exchange endpoint.
        token_url: String,
        /// Environment variable containing the short-lived subject assertion.
        subject_token_env: String,
        /// RFC 8693 subject token type.
        #[serde(default = "default_jwt_token_type")]
        subject_token_type: String,
    },
    /// Provider-supported OpenID CIBA polling flow. The user approves on a
    /// separate authentication device; the CLI never opens a browser or
    /// receives the user's password.
    Ciba {
        /// Backchannel authentication endpoint.
        backchannel_authentication_url: String,
        /// OAuth token endpoint used to poll `auth_req_id`.
        token_url: String,
        /// Environment variable containing the provider-specific login hint.
        login_hint_env: String,
        /// Optional environment variable containing a confidential-client
        /// secret. Omit for providers accepting public CIBA clients.
        client_secret_env: Option<String>,
    },
    /// A narrow HTTPS broker contract for proprietary authentication systems.
    /// The broker returns either OAuth-shaped tokens or a pollable user action;
    /// arbitrary local shell execution is deliberately not supported.
    Broker {
        /// Broker endpoint that begins an authentication attempt.
        begin_url: String,
    },
    /// WorkOS-compatible delegated-agent registration. The agent receives its
    /// own assertion and can optionally be bound to a user through a claim
    /// ceremony; access tokens identify the delegated actor separately from
    /// the user.
    AgentRegistration {
        /// Authorization-server origin exposing `/agent/identity` and
        /// `/oauth2/token`.
        authorization_server: String,
        /// `anonymous` for restricted autonomous enrollment or `service_auth`
        /// for mandatory user binding.
        identity_type: String,
        /// Environment variable containing the user's login hint. Required
        /// for `service_auth` and omitted for `anonymous`.
        login_hint_env: Option<String>,
    },
    /// Credentials are minted locally by signing a caller-supplied claim set
    /// with a configured RSA private key — no network call, no browser, no
    /// registered OAuth client. Only meaningful when the target backend has a
    /// matching non-production signature-verification bypass; the generated
    /// CLI refuses to use it outside `allowed_environments`.
    Mock {
        /// PEM-encoded RSA private key used to sign mock credentials.
        private_key_pem: String,
        /// Named environments (matching `environments` keys) where mock
        /// credentials are honored. Login is refused everywhere else,
        /// including an explicit `--base-url`.
        allowed_environments: Vec<String>,
        /// Default credential lifetime in seconds when `--ttl` is omitted.
        default_ttl_seconds: u64,
    },
    /// Development-only mock credential signing using a key supplied at
    /// runtime. Prefer this to `mock`, which is retained for compatibility but
    /// embeds its key in the generated binary.
    MockEnvironment {
        /// Environment variable containing a PEM-encoded RSA private key.
        private_key_env: String,
        /// Named environments where mock credentials are accepted.
        allowed_environments: Vec<String>,
        /// Default credential lifetime.
        default_ttl_seconds: u64,
    },
    /// A browser application exposes a bearer token for the user to copy, then
    /// the CLI securely prompts for and validates that token.
    BrowserToken {
        /// Browser login entrypoint.
        login_url: String,
        /// API endpoint used to validate the pasted bearer token.
        validation_url: String,
        /// Named environments where this login flow is permitted.
        allowed_environments: Vec<String>,
        /// Safe caller attributes selected from the validation response.
        /// Keys are public names returned by `auth whoami`; values are JSON
        /// pointers evaluated against the validation endpoint's response.
        #[serde(default)]
        identity_fields: BTreeMap<String, String>,
    },
}

fn default_jwt_token_type() -> String {
    "urn:ietf:params:oauth:token-type:jwt".to_string()
}
