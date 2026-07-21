/// Failure modes a generated command can report. `exit_code`/`report` provide
/// stable categories across regeneration, with richer detail in the structured
/// stderr JSON body than the exit code alone carries.
#[derive(Debug, Clone)]
pub enum ClientError {
    /// HTTP transport or local IO failed.
    Transport(String),
    /// Credential storage failed.
    CredentialStore(String),
    /// Required credential was not available.
    MissingCredential(String),
    /// No declared OpenAPI authentication alternative could be satisfied.
    AuthenticationRequired {
        /// OR alternatives; every scheme inside one inner list is required.
        alternatives: Vec<Vec<AuthenticationSchemeRequirement>>,
    },
    /// Operation requested an authentication mode this runtime cannot satisfy.
    UnsupportedAuth(String),
    /// Required runtime configuration (currently just `--base-url`) wasn't
    /// supplied. Distinct from `MissingCredential` since it's not a secret —
    /// commands that need neither (`schema`, `auth ...`) never hit this.
    MissingConfig(&'static str),
    /// Response or request value could not be decoded.
    Decode(String),
    /// API returned a non-success status.
    Api {
        /// HTTP status code.
        status: u16,
        /// Raw response body.
        body: Vec<u8>,
        /// Response content type, if present.
        content_type: Option<String>,
    },
}

/// One named scheme inside a structured authentication recovery error.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthenticationSchemeRequirement {
    /// OpenAPI security-scheme component name.
    pub name: String,
    /// OAuth/OIDC scopes required by the operation.
    pub scopes: Vec<String>,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Transport(message) => write!(formatter, "transport error: {message}"),
            ClientError::CredentialStore(message) => {
                write!(formatter, "credential store error: {message}")
            }
            ClientError::MissingCredential(hint) => {
                write!(formatter, "missing credential: {hint}")
            }
            ClientError::AuthenticationRequired { alternatives } => write!(
                formatter,
                "authentication required: no OpenAPI security alternative is satisfied ({})",
                format_authentication_alternatives(alternatives)
            ),
            ClientError::UnsupportedAuth(message) => {
                write!(formatter, "unsupported authentication: {message}")
            }
            ClientError::MissingConfig(hint) => {
                write!(formatter, "missing configuration: {hint}")
            }
            ClientError::Decode(message) => {
                write!(formatter, "could not decode response: {message}")
            }
            ClientError::Api { status, .. } => write!(formatter, "API error (status {status})"),
        }
    }
}

impl std::error::Error for ClientError {}

impl ClientError {
    /// 0 success (never constructed as an error) / 1 general failure / 3 not
    /// found / 4 permission / 5 conflict.
    /// Usage errors (exit 2) never reach this: clap exits directly on parse failure.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            ClientError::Api {
                status: 401 | 403, ..
            }
            | ClientError::CredentialStore(_)
            | ClientError::MissingCredential(_)
            | ClientError::AuthenticationRequired { .. }
            | ClientError::UnsupportedAuth(_) => 4,
            ClientError::Api { status: 404, .. } => 3,
            ClientError::Api { status: 409, .. } => 5,
            ClientError::Api { .. }
            | ClientError::Transport(_)
            | ClientError::Decode(_)
            | ClientError::MissingConfig(_) => 1,
        }
    }

    /// True when retrying the same call unchanged can plausibly succeed:
    /// transport failures, rate limits, and server-side errors.
    #[must_use]
    pub fn retryable(&self) -> bool {
        match self {
            ClientError::Transport(_) => true,
            ClientError::Api { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }

    /// A concrete next action an agent can take to recover, when one exists.
    #[must_use]
    pub fn recovery_hint(&self) -> Option<String> {
        let cli = &crate::config::runtime_config().identity.command_name;
        match self {
            ClientError::Api { status: 401, .. } => Some(format!(
                "Not authenticated — run `{cli} auth login` first."
            )),
            ClientError::Api { status: 403, .. } => Some(format!(
                "Your identity cannot reach this operation — run `{cli} start` to see the resources and capabilities available to you."
            )),
            ClientError::Api { status: 404, .. } => Some(format!(
                "Verify the resource ID — list the resource first, or run `{cli} start` to see relevant resources."
            )),
            ClientError::Api { status: 409, .. } => Some(
                "The resource changed since you last read it — fetch its current state, then retry with updated values.".to_string(),
            ),
            ClientError::Api { status: 429, .. } => Some(
                "Rate limited — wait briefly and retry the same call.".to_string(),
            ),
            ClientError::Api { status, .. } if *status >= 500 => Some(
                "Server error — retry the same call; if it persists, the API is having trouble.".to_string(),
            ),
            ClientError::Api { .. } | ClientError::Decode(_) => None,
            ClientError::Transport(_) => Some(
                "Network failure — check connectivity and the configured base URL, then retry.".to_string(),
            ),
            ClientError::AuthenticationRequired { alternatives } => {
                authentication_recovery_command(alternatives)
            }
            ClientError::MissingCredential(_) | ClientError::CredentialStore(_) => Some(
                format!("Run `{cli} auth login`, or pass --token explicitly."),
            ),
            ClientError::UnsupportedAuth(_) => Some(format!(
                "Run `{cli} auth doctor` to inspect the configured authentication schemes."
            )),
            ClientError::MissingConfig(_) => Some(format!(
                "Pass --base-url or --environment, or run `{cli} env list` to see the compiled-in environments."
            )),
        }
    }

    /// Builds the structured JSON error envelope printed by generated CLIs.
    #[must_use]
    pub fn report_json(&self) -> serde_json::Value {
        let mut report = match self {
            ClientError::Api {
                status,
                body,
                content_type,
            } => {
                let message = if content_type.as_deref().is_some_and(|value| {
                    let essence = value.split(';').next().unwrap_or(value).trim();
                    essence.eq_ignore_ascii_case("application/json")
                        || essence.to_ascii_lowercase().ends_with("+json")
                }) {
                    serde_json::from_slice(body).unwrap_or_else(|_| {
                        serde_json::Value::String(String::from_utf8_lossy(body).into_owned())
                    })
                } else {
                    serde_json::Value::String(String::from_utf8_lossy(body).into_owned())
                };
                serde_json::json!({
                    "error": {
                        "code": "api_error",
                        "http_status": status,
                        "message": sanitize_untrusted_json_text(message),
                    }
                })
            }
            ClientError::AuthenticationRequired { alternatives } => serde_json::json!({
                "error": {
                    "code": "authentication_required",
                    "message": self.to_string(),
                    "authentication": {
                        "mode": "authenticated",
                        "alternatives": alternatives.iter().map(|schemes| {
                            serde_json::json!({ "schemes": schemes })
                        }).collect::<Vec<_>>(),
                        "recovery_command": authentication_recovery_command(alternatives),
                    },
                }
            }),
            other => serde_json::json!({
                "error": {
                    "code": "cli_error",
                    "message": other.to_string(),
                }
            }),
        };
        report["error"]["retryable"] = serde_json::Value::Bool(self.retryable());
        if let Some(hint) = self.recovery_hint() {
            report["error"]["hint"] = serde_json::Value::String(hint);
        }
        report
    }

    /// Prints a structured JSON error report to stderr. Every report carries
    /// `code`, `message`, and `retryable`; API failures add `http_status`, and
    /// `hint` appears whenever a concrete recovery action exists so an agent
    /// can act without a clarification round trip.
    pub fn report(&self) {
        eprintln!("{}", self.report_json());
    }
}

fn format_authentication_alternatives(
    alternatives: &[Vec<AuthenticationSchemeRequirement>],
) -> String {
    alternatives
        .iter()
        .map(|alternative| {
            alternative
                .iter()
                .map(|scheme| scheme.name.as_str())
                .collect::<Vec<_>>()
                .join(" + ")
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

fn authentication_recovery_command(
    alternatives: &[Vec<AuthenticationSchemeRequirement>],
) -> Option<String> {
    let cli = &crate::config::runtime_config().identity.command_name;
    if let Some(scheme) = alternatives
        .iter()
        .filter(|alternative| alternative.len() == 1)
        .filter_map(|alternative| alternative.first())
        .find(|scheme| crate::oauth::oauth_provider_for_scheme(&scheme.name).is_some())
    {
        return Some(format!(
            "{cli} auth ensure --scheme {} --interaction relay",
            scheme.name
        ));
    }
    alternatives
        .first()
        .filter(|alternative| !alternative.is_empty())
        .map(|alternative| {
            let arguments = alternative
                .iter()
                .map(|scheme| format!("--credential {}=<value>", scheme.name))
                .collect::<Vec<_>>()
                .join(" ");
            format!("{cli} {arguments} <resource> <command>")
        })
}

/// Strips characters an API response could use to visually spoof or inject
/// into an agent's terminal transcript: zero-width characters and Unicode
/// bidirectional overrides. ANSI escapes are already neutralized because
/// serde_json escapes control characters when serializing.
fn sanitize_untrusted_json_text(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(
            text.chars()
                .filter(|character| {
                    !matches!(
                        character,
                        '\u{200B}'..='\u{200F}'
                            | '\u{202A}'..='\u{202E}'
                            | '\u{2066}'..='\u{2069}'
                            | '\u{FEFF}'
                    )
                })
                .collect(),
        ),
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(sanitize_untrusted_json_text)
                .collect(),
        ),
        serde_json::Value::Object(entries) => serde_json::Value::Object(
            entries
                .into_iter()
                .map(|(key, entry)| (key, sanitize_untrusted_json_text(entry)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_failures_are_retryable_and_permanent_ones_are_not() {
        assert!(ClientError::Transport("reset".into()).retryable());
        assert!(api_error(429).retryable());
        assert!(api_error(503).retryable());
        assert!(!api_error(401).retryable());
        assert!(!api_error(404).retryable());
        assert!(!ClientError::Decode("bad".into()).retryable());
        assert!(!ClientError::MissingConfig("--base-url").retryable());
    }

    #[test]
    fn recoverable_failures_carry_a_concrete_hint() {
        for error in [
            api_error(401),
            api_error(403),
            api_error(404),
            api_error(409),
            api_error(429),
            api_error(500),
            ClientError::Transport("reset".into()),
            ClientError::MissingCredential("token".into()),
            ClientError::UnsupportedAuth("basic".into()),
            ClientError::MissingConfig("--base-url"),
        ] {
            let hint = error.recovery_hint();
            assert!(hint.is_some(), "{error} should carry a hint");
            assert!(!hint.unwrap().is_empty());
        }
        assert!(ClientError::Decode("bad".into()).recovery_hint().is_none());
        assert!(api_error(400).recovery_hint().is_none());
    }

    #[test]
    fn untrusted_response_text_loses_invisible_characters() {
        let sanitized = sanitize_untrusted_json_text(serde_json::json!({
            "message": "ok\u{200B}\u{202E}reversed\u{2066}",
            "nested": ["a\u{FEFF}b"],
        }));
        assert_eq!(sanitized["message"], "okreversed");
        assert_eq!(sanitized["nested"][0], "ab");
    }

    #[test]
    fn authentication_required_report_preserves_or_and_scopes() {
        let error = ClientError::AuthenticationRequired {
            alternatives: vec![
                vec![AuthenticationSchemeRequirement {
                    name: "customerOAuth".to_string(),
                    scopes: vec!["projects:read".to_string()],
                }],
                vec![
                    AuthenticationSchemeRequirement {
                        name: "primaryKey".to_string(),
                        scopes: Vec::new(),
                    },
                    AuthenticationSchemeRequirement {
                        name: "organizationKey".to_string(),
                        scopes: Vec::new(),
                    },
                ],
            ],
        };
        let report = error.report_json();
        assert_eq!(report["error"]["code"], "authentication_required");
        assert_eq!(
            report["error"]["authentication"]["alternatives"][0]["schemes"][0]["scopes"],
            serde_json::json!(["projects:read"])
        );
        assert_eq!(
            report["error"]["authentication"]["alternatives"][1]["schemes"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(report["error"]["hint"].as_str().is_some());
    }

    fn api_error(status: u16) -> ClientError {
        ClientError::Api {
            status,
            body: b"{}".to_vec(),
            content_type: Some("application/json".to_string()),
        }
    }
}
