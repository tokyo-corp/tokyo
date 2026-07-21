//! Non-PKCE credential acquisition protocols.

use std::time::Duration;

use super::{
    OAuthEndpoints, OAuthProvider, StoredOAuthToken, current_unix_timestamp_seconds,
    oauth_http_agent, validate_oauth_endpoint_url,
};
use crate::error::ClientError;

pub(super) fn login_with_workload_identity(
    provider: &OAuthProvider,
) -> Result<StoredOAuthToken, ClientError> {
    let OAuthEndpoints::WorkloadIdentity {
        token_url,
        subject_token_env,
        subject_token_type,
    } = provider.endpoints
    else {
        unreachable!("workload login is called only for workload providers");
    };
    validate_oauth_endpoint_url(token_url)?;
    let subject_token = std::env::var(subject_token_env).map_err(|_| {
        ClientError::MissingCredential(format!(
            "workload identity requires environment variable {subject_token_env}"
        ))
    })?;
    let mut form = vec![
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:token-exchange",
        ),
        ("subject_token", subject_token.as_str()),
        ("subject_token_type", subject_token_type),
    ];
    if !provider.client_id.is_empty() {
        form.push(("client_id", provider.client_id));
    }
    let scope = provider.scopes.join(" ");
    if !scope.is_empty() {
        form.push(("scope", scope.as_str()));
    }
    if let Some(audience) = provider.audience {
        form.push(("audience", audience));
    }
    let body = send_form_for_json(token_url, &form, "workload token exchange")?;
    stored_token_from_json(&body, None)
}

pub(super) fn login_with_ciba(provider: &OAuthProvider) -> Result<StoredOAuthToken, ClientError> {
    let OAuthEndpoints::Ciba {
        backchannel_authentication_url,
        token_url,
        login_hint_env,
        client_secret_env,
    } = provider.endpoints
    else {
        unreachable!("CIBA login is called only for CIBA providers");
    };
    validate_oauth_endpoint_url(backchannel_authentication_url)?;
    validate_oauth_endpoint_url(token_url)?;
    let login_hint = required_environment(login_hint_env, "CIBA login")?;
    let client_secret = client_secret_env
        .map(|name| required_environment(name, "CIBA client authentication"))
        .transpose()?;
    let scope = provider.scopes.join(" ");
    let mut form = vec![
        ("client_id", provider.client_id),
        ("login_hint", login_hint.as_str()),
    ];
    if !scope.is_empty() {
        form.push(("scope", scope.as_str()));
    }
    if let Some(audience) = provider.audience {
        form.push(("audience", audience));
    }
    if let Some(secret) = client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    let started = send_form_for_json(
        backchannel_authentication_url,
        &form,
        "CIBA authentication request",
    )?;
    let auth_req_id = required_json_string(&started, "auth_req_id", "CIBA response")?;
    let expires_in = json_u64_or(&started, "expires_in", 300);
    let mut interval = json_u64_or(&started, "interval", 5).max(1);
    emit_action(serde_json::json!({
        "status": "action_required",
        "kind": "out_of_band_approval",
        "flow": "ciba",
        "expires_in": expires_in,
    }));
    let deadline = std::time::Instant::now() + Duration::from_secs(expires_in);
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(ClientError::UnsupportedAuth(
                "CIBA authentication request expired".to_string(),
            ));
        }
        std::thread::sleep(Duration::from_secs(interval));
        let mut poll_form = vec![
            ("grant_type", "urn:openid:params:grant-type:ciba"),
            ("auth_req_id", auth_req_id.as_str()),
            ("client_id", provider.client_id),
        ];
        if let Some(secret) = client_secret.as_deref() {
            poll_form.push(("client_secret", secret));
        }
        match send_form_for_json_allow_oauth_error(token_url, &poll_form, "CIBA token request")? {
            OAuthJsonResponse::Success(body) => return stored_token_from_json(&body, None),
            OAuthJsonResponse::Error(error) if error == "authorization_pending" => {}
            OAuthJsonResponse::Error(error) if error == "slow_down" => {
                interval = interval.saturating_add(5);
            }
            OAuthJsonResponse::Error(error) => {
                return Err(ClientError::UnsupportedAuth(format!(
                    "CIBA token request failed: {error}"
                )));
            }
        }
    }
}

pub(super) fn refresh_ciba_access_token(
    provider: &OAuthProvider,
    token_url: &str,
    client_secret_env: Option<&str>,
    refresh_token: String,
) -> Result<StoredOAuthToken, ClientError> {
    let client_secret = client_secret_env
        .map(|name| required_environment(name, "CIBA refresh"))
        .transpose()?;
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
        ("client_id", provider.client_id),
    ];
    if let Some(secret) = client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    let body = send_form_for_json(token_url, &form, "CIBA refresh request")?;
    stored_token_from_json(&body, Some(refresh_token))
}

pub(super) fn login_with_auth_broker(
    provider: &OAuthProvider,
) -> Result<StoredOAuthToken, ClientError> {
    let OAuthEndpoints::Broker { begin_url } = provider.endpoints else {
        unreachable!("broker login is called only for broker providers");
    };
    validate_oauth_endpoint_url(begin_url)?;
    let body = send_json_for_json(
        begin_url,
        &serde_json::json!({
            "scheme": provider.scheme,
            "client_id": provider.client_id,
            "scopes": provider.scopes,
            "audience": provider.audience,
        }),
        "authentication broker",
    )?;
    if body.get("access_token").is_some() {
        return stored_token_from_json(&body, None);
    }
    poll_auth_broker(begin_url, body)
}

fn poll_auth_broker(
    begin_url: &str,
    body: serde_json::Value,
) -> Result<StoredOAuthToken, ClientError> {
    let poll_url = required_json_string(&body, "poll_url", "authentication broker response")?;
    validate_oauth_endpoint_url(&poll_url)?;
    require_same_origin(begin_url, &poll_url)?;
    let attempt_id = required_json_string(&body, "attempt_id", "authentication broker response")?;
    let expires_in = json_u64_or(&body, "expires_in", 300);
    let mut interval = json_u64_or(&body, "interval", 2).max(1);
    emit_action(serde_json::json!({
        "status": "action_required",
        "flow": "broker",
        "action": body.get("action").cloned().unwrap_or_else(|| serde_json::json!({"kind": "provider_approval"})),
        "expires_in": expires_in,
    }));
    let deadline = std::time::Instant::now() + Duration::from_secs(expires_in);
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(ClientError::UnsupportedAuth(
                "authentication broker attempt expired".to_string(),
            ));
        }
        std::thread::sleep(Duration::from_secs(interval));
        let poll = send_json_for_json(
            &poll_url,
            &serde_json::json!({"attempt_id": attempt_id}),
            "authentication broker poll",
        )?;
        match poll.get("status").and_then(serde_json::Value::as_str) {
            Some("pending") => {
                if poll.get("slow_down").and_then(serde_json::Value::as_bool) == Some(true) {
                    interval = interval.saturating_add(2);
                }
            }
            Some("denied") | Some("expired") => {
                return Err(ClientError::UnsupportedAuth(format!(
                    "authentication broker returned {}",
                    poll["status"].as_str().unwrap_or("failure")
                )));
            }
            _ if poll.get("access_token").is_some() => {
                return stored_token_from_json(&poll, None);
            }
            _ => {
                return Err(ClientError::Decode(
                    "authentication broker poll returned neither pending nor tokens".to_string(),
                ));
            }
        }
    }
}

pub(super) fn login_with_agent_registration(
    provider: &OAuthProvider,
    refresh_token: Option<&str>,
) -> Result<StoredOAuthToken, ClientError> {
    let OAuthEndpoints::AgentRegistration {
        authorization_server,
        identity_type,
        login_hint_env,
    } = provider.endpoints
    else {
        unreachable!("agent registration is called only for agent providers");
    };
    validate_oauth_endpoint_url(authorization_server)?;
    let origin = authorization_server.trim_end_matches('/');
    let login_hint = login_hint_env
        .map(|name| required_environment(name, "agent service_auth"))
        .transpose()?;
    let request = match refresh_token {
        Some(refresh_token) => {
            serde_json::json!({"type": "refresh", "refresh_token": refresh_token})
        }
        None => serde_json::json!({"type": identity_type, "login_hint": login_hint}),
    };
    let mut registration = send_json_for_json(
        &format!("{origin}/agent/identity"),
        &request,
        "agent registration",
    )?;
    if registration.pointer("/identity/assertion").is_none() {
        registration =
            complete_agent_claim(origin, identity_type, login_hint.as_deref(), &registration)?;
    }
    exchange_agent_assertion(origin, &registration, refresh_token)
}

fn complete_agent_claim(
    origin: &str,
    identity_type: &str,
    login_hint: Option<&str>,
    registration: &serde_json::Value,
) -> Result<serde_json::Value, ClientError> {
    let claim_token = registration
        .pointer("/claim/token")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            ClientError::Decode(
                "agent registration returned neither an assertion nor a claim token".to_string(),
            )
        })?;
    let claim_attempt = if registration
        .pointer("/claim/attempt/verification_uri")
        .is_some()
    {
        registration.clone()
    } else {
        let login_hint = login_hint.ok_or_else(|| {
            ClientError::MissingCredential(
                "agent claim requires the registration login hint".to_string(),
            )
        })?;
        send_json_for_json(
            &format!("{origin}/agent/identity/claim"),
            &serde_json::json!({
                "type": identity_type,
                "claim_token": claim_token,
                "login_hint": login_hint,
            }),
            "agent claim attempt",
        )?
    };
    let verification_uri = claim_attempt
        .pointer("/claim/attempt/verification_uri")
        .or_else(|| claim_attempt.pointer("/attempt/verification_uri"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            ClientError::Decode("agent claim response omitted verification_uri".to_string())
        })?;
    validate_oauth_endpoint_url(verification_uri)?;
    emit_action(serde_json::json!({
        "status": "action_required",
        "kind": "agent_claim",
        "flow": "agent_registration",
        "verification_uri": verification_uri,
        "input": "user_code",
    }));
    let user_code = read_agent_claim_code()?;
    send_json_for_json(
        &format!("{origin}/agent/identity/claim/complete"),
        &serde_json::json!({"claim_token": claim_token, "user_code": user_code}),
        "agent claim completion",
    )
}

fn read_agent_claim_code() -> Result<String, ClientError> {
    eprint!("Agent claim code: ");
    use std::io::Write as _;
    std::io::stderr().flush().ok();
    let mut user_code = String::new();
    std::io::stdin()
        .read_line(&mut user_code)
        .map_err(|error| ClientError::Transport(error.to_string()))?;
    let user_code = user_code.trim().to_string();
    if user_code.is_empty() {
        return Err(ClientError::MissingCredential(
            "agent claim requires the short user_code shown after approval".to_string(),
        ));
    }
    Ok(user_code)
}

fn exchange_agent_assertion(
    origin: &str,
    registration: &serde_json::Value,
    previous_refresh: Option<&str>,
) -> Result<StoredOAuthToken, ClientError> {
    let assertion = registration
        .pointer("/identity/assertion")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            ClientError::Decode(
                "agent registration response omitted identity.assertion".to_string(),
            )
        })?;
    let identity_refresh = registration
        .pointer("/identity/refresh_token")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            registration
                .pointer("/identity/refresh_token/value")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| previous_refresh.map(str::to_string));
    let body = send_form_for_json(
        &format!("{origin}/oauth2/token"),
        &[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", assertion),
        ],
        "agent assertion exchange",
    )?;
    let mut token = stored_token_from_json(&body, None)?;
    token.refresh_token = identity_refresh;
    Ok(token)
}

fn required_environment(name: &str, context: &str) -> Result<String, ClientError> {
    std::env::var(name).map_err(|_| {
        ClientError::MissingCredential(format!("{context} requires environment variable {name}"))
    })
}

fn json_u64_or(body: &serde_json::Value, field: &str, default: u64) -> u64 {
    body.get(field)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(default)
}

fn emit_action(action: serde_json::Value) {
    eprintln!("{action}");
}

pub(super) fn stored_token_from_json(
    body: &serde_json::Value,
    previous_refresh: Option<String>,
) -> Result<StoredOAuthToken, ClientError> {
    let access_token = required_json_string(body, "access_token", "OAuth token response")?;
    let refresh_token = body
        .get("refresh_token")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or(previous_refresh);
    let expires_at = body
        .get("expires_in")
        .and_then(serde_json::Value::as_u64)
        .map(|seconds| current_unix_timestamp_seconds().saturating_add(seconds));
    Ok(StoredOAuthToken {
        access_token,
        refresh_token,
        expires_at,
    })
}

fn required_json_string(
    body: &serde_json::Value,
    field: &str,
    context: &str,
) -> Result<String, ClientError> {
    body.get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            ClientError::Decode(format!(
                "{context} is missing non-empty string field {field:?}"
            ))
        })
}

enum OAuthJsonResponse {
    Success(serde_json::Value),
    Error(String),
}

fn send_form_for_json(
    url: &str,
    form: &[(&str, &str)],
    context: &str,
) -> Result<serde_json::Value, ClientError> {
    match send_form_for_json_allow_oauth_error(url, form, context)? {
        OAuthJsonResponse::Success(body) => Ok(body),
        OAuthJsonResponse::Error(error) => Err(ClientError::UnsupportedAuth(format!(
            "{context} failed: {error}"
        ))),
    }
}

fn send_form_for_json_allow_oauth_error(
    url: &str,
    form: &[(&str, &str)],
    context: &str,
) -> Result<OAuthJsonResponse, ClientError> {
    let response = match oauth_http_agent().post(url).send_form(form) {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => {
            return Err(ClientError::Transport(format!("{context} failed: {error}")));
        }
    };
    let body: serde_json::Value = serde_json::from_reader(response.into_reader())
        .map_err(|error| ClientError::Decode(format!("{context}: {error}")))?;
    if let Some(error) = body.get("error").and_then(serde_json::Value::as_str) {
        return Ok(OAuthJsonResponse::Error(error.to_string()));
    }
    Ok(OAuthJsonResponse::Success(body))
}

fn send_json_for_json(
    url: &str,
    request: &serde_json::Value,
    context: &str,
) -> Result<serde_json::Value, ClientError> {
    let encoded = serde_json::to_string(request).expect("JSON value always serializes");
    let response = oauth_http_agent()
        .post(url)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_string(&encoded)
        .map_err(|error| ClientError::Transport(format!("{context} failed: {error}")))?;
    serde_json::from_reader(response.into_reader())
        .map_err(|error| ClientError::Decode(format!("{context}: {error}")))
}

fn require_same_origin(left: &str, right: &str) -> Result<(), ClientError> {
    let left = url::Url::parse(left).map_err(|error| ClientError::Decode(error.to_string()))?;
    let right = url::Url::parse(right).map_err(|error| ClientError::Decode(error.to_string()))?;
    if left.scheme() != right.scheme()
        || left.host_str() != right.host_str()
        || left.port_or_known_default() != right.port_or_known_default()
    {
        return Err(ClientError::UnsupportedAuth(
            "authentication broker poll_url must use the begin_url origin".to_string(),
        ));
    }
    Ok(())
}
