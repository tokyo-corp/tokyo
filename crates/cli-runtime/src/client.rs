#![allow(
    clippy::items_after_test_module,
    clippy::result_large_err,
    clippy::too_many_arguments
)]

use base64::Engine as _;
use sha2::Digest as _;
use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::io::Read as _;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
/// HTTP method accepted by generated commands.
pub enum Method {
    /// GET request.
    Get,
    /// HEAD request.
    Head,
    /// POST request.
    Post,
    /// PUT request.
    Put,
    /// PATCH request.
    Patch,
    /// DELETE request.
    Delete,
    /// OPTIONS request.
    Options,
    /// TRACE request.
    Trace,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
        }
    }
}

#[derive(Debug, Clone, Copy)]
/// Target URL strategy for a generated request.
pub enum RequestTarget<'a> {
    /// Resolve a relative path against the configured base URL.
    Relative(&'a str),
    /// Use an absolute URL supplied by the caller.
    Absolute(&'a str),
    /// Resolve a path against an operation-specific server URL.
    ServerAndPath {
        /// Operation-specific server URL.
        server: &'a str,
        /// Request path to append to the server URL.
        path: &'a str,
    },
}

#[derive(Debug)]
/// Request body bytes and metadata after CLI argument serialization.
pub enum RequestBody {
    /// JSON request body.
    Json(Vec<u8>),
    /// URL-encoded form fields.
    Form(Vec<QueryParameter>),
    /// Multipart form parts.
    Multipart(Vec<MultipartPart>),
    /// UTF-8 text request body.
    Text(Vec<u8>),
    /// Raw binary request body.
    Binary(Vec<u8>),
}

#[derive(Debug)]
/// One multipart request part.
pub struct MultipartPart {
    name: String,
    filename: Option<String>,
    content_type: Option<&'static str>,
    headers: Vec<(&'static str, &'static str)>,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
/// One serialized query or form parameter.
pub struct QueryParameter {
    name: String,
    value: String,
    allow_reserved: bool,
}

#[derive(Debug, Clone, Copy)]
/// Serialization metadata for one URL-encoded form field.
pub struct FormFieldEncoding {
    /// Wire-format field name.
    pub name: &'static str,
    /// OpenAPI serialization style.
    pub style: ParameterStyle,
    /// Whether reserved URI characters remain unescaped.
    pub allow_reserved: bool,
}

#[derive(Debug, Clone, Copy)]
/// Serialization metadata for one multipart field.
pub struct MultipartFieldEncoding {
    /// Wire-format field name.
    pub name: &'static str,
    /// Optional multipart part content type.
    pub content_type: Option<&'static str>,
    /// Static headers attached to the multipart part.
    pub headers: &'static [(&'static str, &'static str)],
}

#[derive(Debug, Clone, Copy)]
/// Streaming response format.
pub enum StreamKind {
    /// JSON item stream.
    Json,
    /// UTF-8 text stream.
    Text,
    /// Server-sent event stream.
    Sse,
}

#[derive(Debug, Clone, Copy)]
/// OpenAPI parameter serialization style.
pub enum ParameterStyle {
    /// Form style with exploded array/object values.
    FormExplode,
    /// Form style without exploded values.
    Form,
    /// Space-delimited array style.
    SpaceDelimited,
    /// Pipe-delimited array style.
    PipeDelimited,
    /// Deep-object style.
    DeepObject,
    /// Simple style without exploded values.
    Simple,
    /// Simple style with exploded values.
    SimpleExplode,
    /// Label style without exploded values.
    Label,
    /// Label style with exploded values.
    LabelExplode,
    /// Matrix style without exploded values.
    Matrix,
    /// Matrix style with exploded values.
    MatrixExplode,
}

#[derive(Debug, Clone)]
/// Buffered HTTP response returned by generated request calls.
pub struct BufferedResponse {
    /// HTTP status code.
    pub status: u16,
    /// Raw response body.
    pub body: Vec<u8>,
    /// Response content type header, if present.
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Copy)]
/// Authentication policy for one generated operation.
pub enum AuthMode {
    /// No authentication is required.
    None,
    /// One of the listed alternatives must be satisfied.
    Requirements(&'static [AuthAlternative]),
}

#[derive(Debug, Clone, Copy)]
/// One acceptable authentication alternative.
pub struct AuthAlternative {
    /// Schemes that must all be satisfied.
    pub schemes: &'static [AuthScheme],
}

#[derive(Debug, Clone, Copy)]
/// One required credential scheme.
pub struct AuthScheme {
    /// OpenAPI security-scheme name.
    pub name: &'static str,
    /// Credential transport or OAuth flow.
    pub kind: AuthSchemeKind,
    /// OAuth scopes required by this scheme.
    pub scopes: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
/// Credential transport used by a generated operation.
pub enum AuthSchemeKind {
    /// Bearer token.
    Bearer,
    /// API key in the named header.
    Header(&'static str),
    /// API key in the named query parameter.
    QueryKey(&'static str),
    /// HTTP basic credentials.
    Basic,
    /// API key in the named cookie.
    CookieKey(&'static str),
    /// OAuth2 client-credentials flow using the named token endpoint.
    OAuth2ClientCredentials(&'static str),
    /// OAuth2 bearer token managed by the runtime.
    OAuth2Bearer,
    /// Credential inferred from another API call.
    Inferred,
}

/// Synchronous HTTP client used by generated commands.
pub struct Client {
    agent: ureq::Agent,
    base_url: String,
    token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    credentials: BTreeMap<String, String>,
    profile: String,
    store: Arc<dyn crate::profile::CredentialStore>,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    debug: bool,
}

impl Client {
    /// Creates a client with static credentials and runtime profile storage.
    #[must_use]
    pub fn new(
        base_url: String,
        token: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
        credentials: BTreeMap<String, String>,
        profile: String,
        store: Arc<dyn crate::profile::CredentialStore>,
        debug: bool,
    ) -> Self {
        Self {
            agent: ureq::Agent::new(),
            base_url,
            token,
            client_id,
            client_secret,
            credentials,
            profile,
            store,
            now: Arc::new(current_unix_timestamp_seconds),
            debug,
        }
    }

    /// Executes a request and buffers the response body.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ClientError`] if URL construction,
    /// authentication, transport, response buffering, or error decoding fails.
    #[must_use = "request results must be handled"]
    pub fn request(
        &self,
        method: Method,
        target: RequestTarget<'_>,
        query: &[QueryParameter],
        headers: &[(String, String)],
        auth: AuthMode,
        body: Option<RequestBody>,
        accept: Option<&str>,
        request_media_type: Option<&str>,
        wildcard_error_media_type: Option<&str>,
    ) -> Result<BufferedResponse, crate::error::ClientError> {
        let selected_auth = self.select_authentication_for_request(auth)?;
        let url = self.resolve_request_url(target, query, selected_auth)?;
        let started = std::time::Instant::now();
        let result = self.execute_buffered_http_request(
            method,
            &url,
            headers,
            selected_auth,
            body,
            accept,
            request_media_type,
            wildcard_error_media_type,
        );
        let elapsed_ms = started.elapsed().as_millis();
        let outcome = match &result {
            Ok(response) => {
                crate::session::record_json_response_as_last_response(&response.body);
                "ok".to_string()
            }
            Err(error) => format!("error: {error}"),
        };
        crate::session::append_completed_request_to_session_transcript(
            method.as_str(),
            &url,
            &outcome,
            elapsed_ms,
        );
        if self.debug {
            eprintln!(
                "[debug] {} {} -> {outcome} ({elapsed_ms}ms)",
                method.as_str(),
                url,
            );
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    /// Executes a request and streams response chunks to a callback.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ClientError`] if URL construction,
    /// authentication, transport, stream parsing, or the callback fails.
    #[must_use = "streaming request results must be handled"]
    pub fn request_stream<F>(
        &self,
        method: Method,
        target: RequestTarget<'_>,
        query: &[QueryParameter],
        headers: &[(String, String)],
        auth: AuthMode,
        body: Option<RequestBody>,
        accept: Option<&str>,
        request_media_type: Option<&str>,
        wildcard_error_media_type: Option<&str>,
        kind: StreamKind,
        mut on_item: F,
    ) -> Result<(), crate::error::ClientError>
    where
        F: FnMut(&[u8]) -> Result<(), crate::error::ClientError>,
    {
        let selected_auth = self.select_authentication_for_request(auth)?;
        let url = self.resolve_request_url(target, query, selected_auth)?;
        let started = std::time::Instant::now();
        let result = self.execute_streaming_http_request(
            method,
            &url,
            headers,
            selected_auth,
            body,
            accept,
            request_media_type,
            wildcard_error_media_type,
            kind,
            &mut on_item,
        );
        let elapsed_ms = started.elapsed().as_millis();
        let outcome = match &result {
            Ok(()) => "ok".to_string(),
            Err(error) => format!("error: {error}"),
        };
        crate::session::append_completed_request_to_session_transcript(
            method.as_str(),
            &url,
            &outcome,
            elapsed_ms,
        );
        if self.debug {
            eprintln!(
                "[debug] {} {} -> {outcome} ({elapsed_ms}ms)",
                method.as_str(),
                url,
            );
        }
        result
    }

    fn execute_buffered_http_request(
        &self,
        method: Method,
        url: &str,
        headers: &[(String, String)],
        selected_auth: Option<&'static AuthAlternative>,
        body: Option<RequestBody>,
        accept: Option<&str>,
        request_media_type: Option<&str>,
        wildcard_error_media_type: Option<&str>,
    ) -> Result<BufferedResponse, crate::error::ClientError> {
        let request = self.prepare_http_request_with_auth_and_body(
            method,
            url,
            headers,
            selected_auth,
            accept,
        )?;
        let result = send_prepared_http_request(request, body, request_media_type);
        let (status, response) = match result {
            Ok(response) => (response.status(), response),
            Err(ureq::Error::Status(status, response)) => (status, response),
            Err(error) => return Err(crate::error::ClientError::Transport(error.to_string())),
        };
        let content_type = response.header("Content-Type").map(str::to_string);
        let mut bytes = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut bytes)
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        if (200..300).contains(&status) {
            Ok(BufferedResponse {
                status,
                body: bytes,
                content_type,
            })
        } else {
            Err(crate::error::ClientError::Api {
                status,
                body: bytes,
                content_type: content_type
                    .or_else(|| wildcard_error_media_type.map(str::to_string)),
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_streaming_http_request<F>(
        &self,
        method: Method,
        url: &str,
        headers: &[(String, String)],
        selected_auth: Option<&'static AuthAlternative>,
        body: Option<RequestBody>,
        accept: Option<&str>,
        request_media_type: Option<&str>,
        wildcard_error_media_type: Option<&str>,
        kind: StreamKind,
        on_item: &mut F,
    ) -> Result<(), crate::error::ClientError>
    where
        F: FnMut(&[u8]) -> Result<(), crate::error::ClientError>,
    {
        let request = self.prepare_http_request_with_auth_and_body(
            method,
            url,
            headers,
            selected_auth,
            accept,
        )?;
        let result = send_prepared_http_request(request, body, request_media_type);
        let (status, response) = match result {
            Ok(response) => (response.status(), response),
            Err(ureq::Error::Status(status, response)) => (status, response),
            Err(error) => return Err(crate::error::ClientError::Transport(error.to_string())),
        };
        if !(200..300).contains(&status) {
            let content_type = response.header("Content-Type").map(str::to_string);
            let mut bytes = Vec::new();
            response
                .into_reader()
                .read_to_end(&mut bytes)
                .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
            return Err(crate::error::ClientError::Api {
                status,
                body: bytes,
                content_type: content_type
                    .or_else(|| wildcard_error_media_type.map(str::to_string)),
            });
        }

        let mut last_item = Vec::new();
        match kind {
            StreamKind::Text => {
                let mut reader = response.into_reader();
                let mut chunk = [0_u8; 8192];
                loop {
                    let read = reader
                        .read(&mut chunk)
                        .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
                    if read == 0 {
                        break;
                    }
                    last_item.clear();
                    last_item.extend_from_slice(&chunk[..read]);
                    on_item(&chunk[..read])?;
                }
            }
            StreamKind::Json => {
                let mut reader = std::io::BufReader::new(response.into_reader());
                let mut line = Vec::new();
                while reader
                    .read_until(b'\n', &mut line)
                    .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?
                    != 0
                {
                    trim_crlf_line_ending(&mut line);
                    if !line.is_empty() {
                        last_item.clone_from(&line);
                        on_item(&line)?;
                    }
                    line.clear();
                }
            }
            StreamKind::Sse => {
                let mut reader = std::io::BufReader::new(response.into_reader());
                let mut line = String::new();
                let mut data = Vec::new();
                loop {
                    line.clear();
                    let read = reader
                        .read_line(&mut line)
                        .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
                    if read == 0 {
                        dispatch_server_sent_event(&mut data, &mut last_item, on_item)?;
                        break;
                    }
                    let line = line.trim_end_matches(['\r', '\n']);
                    if line.is_empty() {
                        if dispatch_server_sent_event(&mut data, &mut last_item, on_item)? {
                            break;
                        }
                    } else if let Some(value) = line.strip_prefix("data:") {
                        data.push(value.strip_prefix(' ').unwrap_or(value).to_string());
                    }
                }
            }
        }
        if !last_item.is_empty() {
            crate::session::record_json_response_as_last_response(&last_item);
        }
        Ok(())
    }

    fn prepare_http_request_with_auth_and_body(
        &self,
        method: Method,
        url: &str,
        headers: &[(String, String)],
        selected_auth: Option<&'static AuthAlternative>,
        accept: Option<&str>,
    ) -> Result<ureq::Request, crate::error::ClientError> {
        let mut request = self.agent.request(method.as_str(), url);
        let mut cookies = Vec::new();
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("cookie") {
                cookies.push(value.clone());
            } else {
                request = request.set(name, value);
            }
        }
        if let Some(accept) = accept {
            request = request.set("Accept", accept);
        }
        if let Some(selected) = selected_auth {
            let allow_token_fallback = selected.schemes.len() == 1;
            for scheme in selected.schemes {
                match scheme.kind {
                    AuthSchemeKind::Bearer | AuthSchemeKind::OAuth2Bearer => {
                        let token = self
                            .credential_value_for_scheme(scheme.name, allow_token_fallback)?
                            .expect("selected credential exists");
                        request = request.set("Authorization", &format!("Bearer {token}"));
                    }
                    AuthSchemeKind::Basic => {
                        let credential = self
                            .credential_value_for_scheme(scheme.name, false)?
                            .expect("selected credential exists");
                        let encoded =
                            base64::engine::general_purpose::STANDARD.encode(credential.as_bytes());
                        request = request.set("Authorization", &format!("Basic {encoded}"));
                    }
                    AuthSchemeKind::Header(header) => {
                        let value = self
                            .credential_value_for_scheme(scheme.name, allow_token_fallback)?
                            .expect("selected credential exists");
                        request = request.set(header, &value);
                    }
                    AuthSchemeKind::QueryKey(_) => {}
                    AuthSchemeKind::CookieKey(cookie) => {
                        let value = self
                            .credential_value_for_scheme(scheme.name, allow_token_fallback)?
                            .expect("selected credential exists");
                        cookies.push(format!("{cookie}={value}"));
                    }
                    AuthSchemeKind::OAuth2ClientCredentials(token_endpoint) => {
                        let token = self.fetch_client_credentials_token(
                            scheme.name,
                            token_endpoint,
                            scheme.scopes,
                        )?;
                        request = request.set("Authorization", &format!("Bearer {token}"));
                    }
                    AuthSchemeKind::Inferred => unreachable!("inferred auth is never selected"),
                }
            }
            if !cookies.is_empty() {
                request = request.set("Cookie", &cookies.join("; "));
            }
        }
        Ok(request)
    }

    fn resolve_request_url(
        &self,
        target: RequestTarget<'_>,
        query: &[QueryParameter],
        selected_auth: Option<&'static AuthAlternative>,
    ) -> Result<String, crate::error::ClientError> {
        let raw = match target {
            RequestTarget::Relative(path) => {
                if self.base_url.is_empty() {
                    return Err(crate::error::ClientError::MissingConfig(
                        "--base-url or $..._BASE_URL",
                    ));
                }
                format!("{}{}", self.base_url.trim_end_matches('/'), path)
            }
            RequestTarget::Absolute(url) => url.to_string(),
            RequestTarget::ServerAndPath { server, path } => {
                format!("{}{}", server.trim_end_matches('/'), path)
            }
        };
        let url = url::Url::parse(&raw).map_err(|error| {
            crate::error::ClientError::Transport(format!("invalid request URL {raw:?}: {error}"))
        })?;
        let mut additions = query.to_vec();
        if let Some(selected) = selected_auth {
            let allow_token_fallback = selected.schemes.len() == 1;
            for scheme in selected.schemes {
                if let AuthSchemeKind::QueryKey(name) = scheme.kind {
                    let value = self
                        .credential_value_for_scheme(scheme.name, allow_token_fallback)?
                        .expect("selected credential exists");
                    additions.push(QueryParameter {
                        name: name.to_string(),
                        value,
                        allow_reserved: false,
                    });
                }
            }
        }
        append_query_parameters_to_url(url.to_string(), &additions)
    }

    fn select_authentication_for_request(
        &self,
        auth: AuthMode,
    ) -> Result<Option<&'static AuthAlternative>, crate::error::ClientError> {
        match auth {
            AuthMode::None => Ok(None),
            AuthMode::Requirements(alternatives) => self
                .select_satisfied_auth_alternative(alternatives)
                .map(Some),
        }
    }

    fn credential_value_for_scheme(
        &self,
        scheme_name: &str,
        allow_token_fallback: bool,
    ) -> Result<Option<String>, crate::error::ClientError> {
        if let Some(value) = self.credentials.get(scheme_name) {
            return Ok(Some(value.clone()));
        }
        if allow_token_fallback && let Some(token) = &self.token {
            return Ok(Some(token.clone()));
        }
        if let Some(token) = crate::oauth::load_or_refresh_managed_oauth_access_token(
            self.store.as_ref(),
            &self.profile,
            scheme_name,
        )? {
            return Ok(Some(token));
        }
        self.store.get_credential_secret(&self.profile, scheme_name)
    }

    fn oauth_client_credentials_pair(
        &self,
        scheme_name: &str,
    ) -> Result<Option<(String, String)>, crate::error::ClientError> {
        if let Some(value) = self.credentials.get(scheme_name) {
            return Ok(value
                .split_once(':')
                .map(|(id, secret)| (id.to_string(), secret.to_string())));
        }
        if let Some(value) = self
            .store
            .get_credential_secret(&self.profile, scheme_name)?
        {
            return Ok(value
                .split_once(':')
                .map(|(id, secret)| (id.to_string(), secret.to_string())));
        }
        Ok(match (&self.client_id, &self.client_secret) {
            (Some(client_id), Some(client_secret)) => {
                Some((client_id.clone(), client_secret.clone()))
            }
            _ => None,
        })
    }

    fn select_satisfied_auth_alternative(
        &self,
        alternatives: &'static [AuthAlternative],
    ) -> Result<&'static AuthAlternative, crate::error::ClientError> {
        let mut inferred = None;
        let anonymous = alternatives
            .iter()
            .find(|alternative| alternative.schemes.is_empty());
        // Prefer caller identity when an optional-auth operation offers both
        // anonymous and authenticated alternatives, independent of their
        // ordering in the OpenAPI document. Fall back to anonymous only after
        // checking every credential-bearing alternative.
        for alternative in alternatives
            .iter()
            .filter(|alternative| !alternative.schemes.is_empty())
        {
            let mut satisfied = true;
            let allow_token_fallback = alternative.schemes.len() == 1;
            for scheme in alternative.schemes {
                let result = match scheme.kind {
                    AuthSchemeKind::Inferred => {
                        inferred.get_or_insert(scheme.name);
                        satisfied = false;
                        continue;
                    }
                    AuthSchemeKind::OAuth2ClientCredentials(_) => self
                        .oauth_client_credentials_pair(scheme.name)
                        .map(|pair| pair.is_some()),
                    AuthSchemeKind::Basic => self
                        .credential_value_for_scheme(scheme.name, false)
                        .map(|credential| credential.is_some()),
                    _ => self
                        .credential_value_for_scheme(scheme.name, allow_token_fallback)
                        .map(|credential| credential.is_some()),
                };
                match result {
                    Ok(true) => {}
                    Ok(false) => {
                        satisfied = false;
                    }
                    Err(crate::error::ClientError::MissingCredential(_)) if anonymous.is_some() => {
                        satisfied = false;
                    }
                    Err(error) => return Err(error),
                }
            }
            if satisfied {
                return Ok(alternative);
            }
        }
        if let Some(anonymous) = anonymous {
            return Ok(anonymous);
        }
        if let Some(name) = inferred {
            return Err(crate::error::ClientError::UnsupportedAuth(format!(
                "inferred authentication scheme {name:?} cannot be emitted because the IR does not describe its credential exchange"
            )));
        }
        Err(crate::error::ClientError::AuthenticationRequired {
            alternatives: alternatives
                .iter()
                .filter(|alternative| !alternative.schemes.is_empty())
                .map(|alternative| {
                    alternative
                        .schemes
                        .iter()
                        .map(|scheme| crate::error::AuthenticationSchemeRequirement {
                            name: scheme.name.to_string(),
                            scopes: scheme
                                .scopes
                                .iter()
                                .map(|scope| (*scope).to_string())
                                .collect(),
                        })
                        .collect()
                })
                .collect(),
        })
    }

    fn fetch_client_credentials_token(
        &self,
        scheme_name: &str,
        token_endpoint: &str,
        scopes: &[&str],
    ) -> Result<String, crate::error::ClientError> {
        let (client_id, client_secret) = self.oauth_client_credentials_pair(scheme_name)?.ok_or_else(|| {
            crate::error::ClientError::MissingCredential(format!(
                "OAuth2 scheme {scheme_name:?} needs --client-id/--client-secret or a named SCHEME=CLIENT_ID:CLIENT_SECRET credential"
            ))
        })?;
        let token_url = url::Url::parse(token_endpoint)
            .or_else(|_| {
                let base = url::Url::parse(&self.base_url)?;
                base.join(token_endpoint)
            })
            .map_err(|error| {
                crate::error::ClientError::Transport(format!(
                    "invalid OAuth2 token endpoint {token_endpoint:?}: {error}"
                ))
            })?;
        let cache_scheme = oauth_client_credentials_cache_scheme(
            &self.profile,
            scheme_name,
            &client_id,
            scopes,
            token_url.as_str(),
        );
        let now = (self.now)();
        if let Some(cached) = self
            .store
            .get_credential_secret(&self.profile, &cache_scheme)?
            && let Ok(cached) = serde_json::from_str::<OAuthTokenCache>(&cached)
            && cached.expires_at > now
        {
            return Ok(cached.access_token);
        }
        let scope = scopes.join(" ");
        let mut form = vec![
            ("grant_type", "client_credentials"),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
        ];
        if !scope.is_empty() {
            form.push(("scope", scope.as_str()));
        }
        let result = self.agent.post(token_url.as_str()).send_form(&form);
        let response = match result {
            Ok(response) => response,
            Err(ureq::Error::Status(status, _)) => {
                return Err(crate::error::ClientError::Transport(format!(
                    "token endpoint returned status {status}"
                )));
            }
            Err(error) => return Err(crate::error::ClientError::Transport(error.to_string())),
        };
        let mut bytes = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut bytes)
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        let body: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
        let access_token = body
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                crate::error::ClientError::Decode(
                    "token endpoint response missing access_token".to_string(),
                )
            })?;
        let expires_in = body
            .get("expires_in")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                crate::error::ClientError::Decode(
                    "token endpoint response missing or invalid expires_in".to_string(),
                )
            })?;
        let skew = std::cmp::min(30, expires_in / 10);
        let cached = OAuthTokenCache {
            access_token: access_token.clone(),
            expires_at: now.saturating_add(expires_in.saturating_sub(skew)),
        };
        let encoded = serde_json::to_string(&cached)
            .expect("OAuth token cache contains only a string and integer");
        self.store
            .save_credential_secret(&self.profile, &cache_scheme, &encoded)?;
        Ok(access_token)
    }
}

fn send_prepared_http_request(
    request: ureq::Request,
    body: Option<RequestBody>,
    request_media_type: Option<&str>,
) -> Result<ureq::Response, ureq::Error> {
    match body {
        Some(RequestBody::Json(bytes)) => request
            .set(
                "Content-Type",
                request_media_type.unwrap_or("application/json"),
            )
            .send_bytes(&bytes),
        Some(RequestBody::Form(fields)) => {
            let bytes = encode_form_urlencoded_pairs(&fields);
            request
                .set(
                    "Content-Type",
                    request_media_type.unwrap_or("application/x-www-form-urlencoded"),
                )
                .send_bytes(bytes.as_bytes())
        }
        Some(RequestBody::Multipart(parts)) => {
            let (content_type, bytes) = encode_multipart_body_with_boundary(
                parts,
                request_media_type.unwrap_or("multipart/form-data"),
            );
            request
                .set("Content-Type", &content_type)
                .send_bytes(&bytes)
        }
        Some(RequestBody::Text(bytes)) => request
            .set(
                "Content-Type",
                request_media_type.unwrap_or("text/plain; charset=utf-8"),
            )
            .send_bytes(&bytes),
        Some(RequestBody::Binary(bytes)) => request
            .set(
                "Content-Type",
                request_media_type.unwrap_or("application/octet-stream"),
            )
            .send_bytes(&bytes),
        None => request.call(),
    }
}

fn encode_form_urlencoded_pairs(fields: &[QueryParameter]) -> String {
    fields
        .iter()
        .map(|field| {
            format!(
                "{}={}",
                percent_encode_query_component(&field.name, false),
                percent_encode_query_component(&field.value, field.allow_reserved),
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn append_query_parameters_to_url(
    url: String,
    additions: &[QueryParameter],
) -> Result<String, crate::error::ClientError> {
    if additions.is_empty() {
        return Ok(url);
    }
    let (without_fragment, fragment) = url
        .split_once('#')
        .map_or((url.as_str(), ""), |(url, fragment)| (url, fragment));
    let mut result = without_fragment.to_string();
    let mut separator = if without_fragment.contains('?') {
        '&'
    } else {
        '?'
    };
    for parameter in additions {
        result.push(separator);
        separator = '&';
        result.push_str(&percent_encode_query_component(&parameter.name, false));
        result.push('=');
        result.push_str(&percent_encode_query_component(
            &parameter.value,
            parameter.allow_reserved,
        ));
    }
    if !fragment.is_empty() {
        result.push('#');
        result.push_str(fragment);
    }
    Ok(result)
}

fn percent_encode_query_component(value: &str, allow_reserved: bool) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        let unreserved = matches!(
            byte,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~'
        );
        let reserved = matches!(
            byte,
            b':' | b'/'
                | b'?'
                | b'#'
                | b'['
                | b']'
                | b'@'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
        );
        if unreserved || (allow_reserved && reserved) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn encode_multipart_body_with_boundary(
    parts: Vec<MultipartPart>,
    media_type: &str,
) -> (String, Vec<u8>) {
    let mut hasher = sha2::Sha256::new();
    for part in &parts {
        hasher.update(part.name.as_bytes());
        hasher.update(&part.bytes);
    }
    let digest = format!("{:x}", hasher.finalize());
    let generated_boundary = format!("tokyo-{}", &digest[..32]);
    let declared_boundary = media_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        if !name.eq_ignore_ascii_case("boundary") {
            return None;
        }
        let value = value.trim().trim_matches('"');
        (!value.is_empty()
            && !value
                .chars()
                .any(|character| matches!(character, '\r' | '\n')))
        .then(|| value.to_string())
    });
    let boundary = declared_boundary.as_deref().unwrap_or(&generated_boundary);
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let name = quote_multipart_header_value(&part.name);
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"").as_bytes(),
        );
        if let Some(filename) = part.filename {
            let filename = quote_multipart_header_value(&filename);
            body.extend_from_slice(format!("; filename=\"{filename}\"").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        if let Some(content_type) = part.content_type {
            body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        }
        for (name, value) in part.headers {
            if name.eq_ignore_ascii_case("content-disposition")
                || name.eq_ignore_ascii_case("content-type")
                || name.contains(['\r', '\n'])
                || value.contains(['\r', '\n'])
            {
                continue;
            }
            body.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(&part.bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let content_type = if declared_boundary.is_some() {
        media_type.to_string()
    } else {
        format!("{}; boundary={boundary}", media_type.trim_end_matches(';'))
    };
    (content_type, body)
}

fn quote_multipart_header_value(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, '\r' | '\n'))
        .flat_map(|character| match character {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect(),
            other => vec![other],
        })
        .collect()
}

fn trim_crlf_line_ending(line: &mut Vec<u8>) {
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
}

fn dispatch_server_sent_event<F>(
    data: &mut Vec<String>,
    last_item: &mut Vec<u8>,
    on_item: &mut F,
) -> Result<bool, crate::error::ClientError>
where
    F: FnMut(&[u8]) -> Result<(), crate::error::ClientError>,
{
    if data.is_empty() {
        return Ok(false);
    }
    let payload = data.join("\n");
    data.clear();
    if payload == "[DONE]" {
        return Ok(true);
    }
    last_item.clear();
    last_item.extend_from_slice(payload.as_bytes());
    on_item(payload.as_bytes())?;
    Ok(false)
}

/// Appends serialized query/form parameter values for the given style.
pub fn append_serialized_parameter_values(
    output: &mut Vec<QueryParameter>,
    name: &str,
    value: &serde_json::Value,
    style: ParameterStyle,
    allow_reserved: bool,
) {
    let mut push = |name: String, value: String| {
        output.push(QueryParameter {
            name,
            value,
            allow_reserved,
        });
    };
    match style {
        ParameterStyle::DeepObject => {
            let mut values = Vec::new();
            append_deep_object_parameter_values(&mut values, name, value);
            for (name, value) in values {
                push(name, value);
            }
        }
        ParameterStyle::FormExplode => match value {
            serde_json::Value::Array(items) => {
                for item in items {
                    push(name.to_string(), render_parameter_scalar_value(item));
                }
            }
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    push(key.clone(), render_parameter_scalar_value(value));
                }
            }
            serde_json::Value::Null => {}
            scalar => push(name.to_string(), render_parameter_scalar_value(scalar)),
        },
        ParameterStyle::Form
        | ParameterStyle::Simple
        | ParameterStyle::SimpleExplode
        | ParameterStyle::Label
        | ParameterStyle::LabelExplode
        | ParameterStyle::Matrix
        | ParameterStyle::MatrixExplode => {
            let value = serialize_delimited_parameter_value(value, ",");
            if !value.is_empty() {
                push(name.to_string(), value);
            }
        }
        ParameterStyle::SpaceDelimited => {
            let value = serialize_delimited_parameter_value(value, " ");
            if !value.is_empty() {
                push(name.to_string(), value);
            }
        }
        ParameterStyle::PipeDelimited => {
            let value = serialize_delimited_parameter_value(value, "|");
            if !value.is_empty() {
                push(name.to_string(), value);
            }
        }
    }
}

fn serialize_delimited_parameter_value(value: &serde_json::Value, separator: &str) -> String {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(render_parameter_scalar_value)
            .collect::<Vec<_>>()
            .join(separator),
        serde_json::Value::Object(object) => object
            .iter()
            .flat_map(|(key, value)| [key.clone(), render_parameter_scalar_value(value)])
            .collect::<Vec<_>>()
            .join(separator),
        serde_json::Value::Null => String::new(),
        scalar => render_parameter_scalar_value(scalar),
    }
}

fn append_deep_object_parameter_values(
    output: &mut Vec<(String, String)>,
    prefix: &str,
    value: &serde_json::Value,
) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                append_deep_object_parameter_values(output, &format!("{prefix}[{key}]"), value);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                append_deep_object_parameter_values(output, &format!("{prefix}[]"), item);
            }
        }
        serde_json::Value::Null => {}
        scalar => output.push((prefix.to_string(), render_parameter_scalar_value(scalar))),
    }
}

/// Serializes a path parameter value according to its OpenAPI style.
#[must_use]
pub fn serialize_path_parameter_value(
    name: &str,
    value: &serde_json::Value,
    style: ParameterStyle,
) -> String {
    serialize_parameter_value_with_style(name, value, style, true)
}

/// Serializes a header parameter value according to its OpenAPI style.
#[must_use]
pub fn serialize_header_parameter_value(
    value: &serde_json::Value,
    style: ParameterStyle,
) -> String {
    serialize_parameter_value_with_style("", value, style, false)
}

/// Serializes a cookie parameter value according to its OpenAPI style.
#[must_use]
pub fn serialize_cookie_parameter_value(
    name: &str,
    value: &serde_json::Value,
    style: ParameterStyle,
) -> Vec<String> {
    match style {
        ParameterStyle::FormExplode => match value {
            serde_json::Value::Array(items) => items
                .iter()
                .map(|item| format!("{name}={}", render_parameter_scalar_value(item)))
                .collect(),
            serde_json::Value::Object(object) => object
                .iter()
                .map(|(key, value)| format!("{key}={}", render_parameter_scalar_value(value)))
                .collect(),
            serde_json::Value::Null => Vec::new(),
            scalar => vec![format!("{name}={}", render_parameter_scalar_value(scalar))],
        },
        _ => {
            let value = serialize_delimited_parameter_value(value, ",");
            if value.is_empty() {
                Vec::new()
            } else {
                vec![format!("{name}={value}")]
            }
        }
    }
}

fn serialize_parameter_value_with_style(
    name: &str,
    value: &serde_json::Value,
    style: ParameterStyle,
    percent_encode: bool,
) -> String {
    let scalar = |value: &serde_json::Value| {
        let value = render_parameter_scalar_value(value);
        if percent_encode {
            percent_encode_path_segment(&value)
        } else {
            value
        }
    };
    let array = |items: &[serde_json::Value], separator: &str| {
        items
            .iter()
            .map(&scalar)
            .collect::<Vec<_>>()
            .join(separator)
    };
    let object = |values: &serde_json::Map<String, serde_json::Value>,
                  pair_separator: &str,
                  key_value_separator: &str| {
        let mut entries = values.iter().collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(right.0));
        entries
            .into_iter()
            .map(|(key, value)| {
                let key = if percent_encode {
                    percent_encode_path_segment(key)
                } else {
                    key.clone()
                };
                format!("{key}{key_value_separator}{}", scalar(value))
            })
            .collect::<Vec<_>>()
            .join(pair_separator)
    };
    match style {
        ParameterStyle::Simple => match value {
            serde_json::Value::Array(items) => array(items, ","),
            serde_json::Value::Object(values) => object(values, ",", ","),
            serde_json::Value::Null => String::new(),
            value => scalar(value),
        },
        ParameterStyle::SimpleExplode => match value {
            serde_json::Value::Array(items) => array(items, ","),
            serde_json::Value::Object(values) => object(values, ",", "="),
            serde_json::Value::Null => String::new(),
            value => scalar(value),
        },
        ParameterStyle::Label | ParameterStyle::LabelExplode => {
            let explode = matches!(style, ParameterStyle::LabelExplode);
            let content = match value {
                serde_json::Value::Array(items) => array(items, if explode { "." } else { "," }),
                serde_json::Value::Object(values) => object(
                    values,
                    if explode { "." } else { "," },
                    if explode { "=" } else { "," },
                ),
                serde_json::Value::Null => String::new(),
                value => scalar(value),
            };
            format!(".{content}")
        }
        ParameterStyle::Matrix | ParameterStyle::MatrixExplode => {
            let explode = matches!(style, ParameterStyle::MatrixExplode);
            match value {
                serde_json::Value::Array(items) if explode => items
                    .iter()
                    .map(|item| format!(";{name}={}", scalar(item)))
                    .collect::<String>(),
                serde_json::Value::Object(values) if explode => {
                    let mut entries = values.iter().collect::<Vec<_>>();
                    entries.sort_by(|left, right| left.0.cmp(right.0));
                    entries
                        .into_iter()
                        .map(|(key, value)| {
                            let key = if percent_encode {
                                percent_encode_path_segment(key)
                            } else {
                                key.clone()
                            };
                            format!(";{key}={}", scalar(value))
                        })
                        .collect::<String>()
                }
                serde_json::Value::Null => String::new(),
                serde_json::Value::Array(items) => format!(";{name}={}", array(items, ",")),
                serde_json::Value::Object(values) => {
                    format!(";{name}={}", object(values, ",", ","))
                }
                value => format!(";{name}={}", scalar(value)),
            }
        }
        ParameterStyle::Form
        | ParameterStyle::FormExplode
        | ParameterStyle::SpaceDelimited
        | ParameterStyle::PipeDelimited
        | ParameterStyle::DeepObject => match value {
            serde_json::Value::Array(items) => array(items, ","),
            serde_json::Value::Object(values) => object(values, ",", ","),
            serde_json::Value::Null => String::new(),
            value => scalar(value),
        },
    }
}

fn render_parameter_scalar_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

/// Parses a CLI argument as JSON.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the argument is not valid JSON.
#[must_use = "parsed JSON or decode errors should be handled"]
pub fn parse_json_cli_argument(
    name: &str,
    value: &str,
) -> Result<serde_json::Value, crate::error::ClientError> {
    serde_json::from_str(value).map_err(|error| {
        crate::error::ClientError::Decode(format!("parameter {name:?} must be valid JSON: {error}"))
    })
}

/// Builds URL-encoded form fields from a JSON object body.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the value is not a JSON object.
#[must_use = "serialized form body or decode errors should be handled"]
pub fn build_form_urlencoded_request_body(
    value: serde_json::Value,
    overrides: &[FormFieldEncoding],
) -> Result<Vec<QueryParameter>, crate::error::ClientError> {
    let serde_json::Value::Object(object) = value else {
        return Err(crate::error::ClientError::Decode(
            "URL-encoded form body must serialize as an object".to_string(),
        ));
    };
    let mut output = Vec::new();
    for (name, value) in object {
        let encoding = overrides
            .iter()
            .find(|encoding| encoding.name == name.as_str());
        let style = encoding
            .map(|encoding| encoding.style)
            .unwrap_or(ParameterStyle::FormExplode);
        let allow_reserved = encoding
            .map(|encoding| encoding.allow_reserved)
            .unwrap_or(false);
        append_serialized_parameter_values(&mut output, &name, &value, style, allow_reserved);
    }
    Ok(output)
}

/// Builds multipart form parts from a JSON object body.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the value is not a JSON object or a
/// declared binary field cannot be read.
#[must_use = "serialized multipart body or decode errors should be handled"]
pub fn build_multipart_request_body(
    value: serde_json::Value,
    binary_fields: &[&str],
    field_encodings: &[MultipartFieldEncoding],
) -> Result<Vec<MultipartPart>, crate::error::ClientError> {
    let serde_json::Value::Object(object) = value else {
        return Err(crate::error::ClientError::Decode(
            "multipart body must serialize as an object".to_string(),
        ));
    };
    let mut parts = Vec::new();
    for (name, value) in object {
        append_multipart_field_value(
            &mut parts,
            &name,
            value,
            binary_fields.contains(&name.as_str()),
            field_encodings
                .iter()
                .find(|encoding| encoding.name == name.as_str())
                .copied(),
        )?;
    }
    Ok(parts)
}

fn append_multipart_field_value(
    parts: &mut Vec<MultipartPart>,
    name: &str,
    value: serde_json::Value,
    binary: bool,
    encoding: Option<MultipartFieldEncoding>,
) -> Result<(), crate::error::ClientError> {
    if let serde_json::Value::Array(items) = value {
        for item in items {
            append_multipart_field_value(parts, name, item, binary, encoding)?;
        }
        return Ok(());
    }
    if value.is_null() {
        return Ok(());
    }
    if binary {
        let path = value.as_str().ok_or_else(|| {
            crate::error::ClientError::Decode(format!(
                "multipart binary field {name:?} must be a file path string"
            ))
        })?;
        let bytes = read_request_body_bytes(path)?;
        let filename = std::path::Path::new(path)
            .file_name()
            .and_then(|filename| filename.to_str())
            .unwrap_or("upload.bin")
            .to_string();
        parts.push(MultipartPart {
            name: name.to_string(),
            filename: Some(filename),
            content_type: encoding
                .and_then(|encoding| encoding.content_type)
                .or(Some("application/octet-stream")),
            headers: encoding
                .map(|encoding| encoding.headers.to_vec())
                .unwrap_or_default(),
            bytes,
        });
    } else if value.is_object() {
        parts.push(MultipartPart {
            name: name.to_string(),
            filename: None,
            content_type: encoding
                .and_then(|encoding| encoding.content_type)
                .or(Some("application/json")),
            headers: encoding
                .map(|encoding| encoding.headers.to_vec())
                .unwrap_or_default(),
            bytes: serde_json::to_vec(&value)
                .expect("a validated multipart object always serializes"),
        });
    } else {
        parts.push(MultipartPart {
            name: name.to_string(),
            filename: None,
            content_type: encoding.and_then(|encoding| encoding.content_type),
            headers: encoding
                .map(|encoding| encoding.headers.to_vec())
                .unwrap_or_default(),
            bytes: render_parameter_scalar_value(&value).into_bytes(),
        });
    }
    Ok(())
}

/// Returns whether a JSON request-body field is exactly `true`.
#[must_use]
pub fn json_request_body_field_is_true(body: &Option<RequestBody>, field: &str) -> bool {
    let Some(RequestBody::Json(bytes)) = body else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| value.get(field).and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OAuthTokenCache {
    access_token: String,
    expires_at: u64,
}

fn current_unix_timestamp_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn oauth_client_credentials_cache_scheme(
    profile: &str,
    scheme_name: &str,
    client_id: &str,
    scopes: &[&str],
    token_endpoint: &str,
) -> String {
    let material = serde_json::to_vec(&(profile, scheme_name, client_id, scopes, token_endpoint))
        .expect("OAuth cache key fields always serialize");
    let digest = sha2::Sha256::digest(material);
    format!("oauth-cache:{digest:x}")
}

/// Loads named credentials from inline assignments, a file, and a JSON object.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if a credential source cannot be read
/// or decoded.
#[must_use = "loaded credentials or credential errors should be handled"]
pub fn load_named_credentials_from_cli_inputs(
    assignments: &[String],
    file: Option<&std::path::Path>,
    json: Option<&str>,
) -> Result<BTreeMap<String, String>, crate::error::ClientError> {
    let mut credentials = BTreeMap::new();
    if let Some(json) = json {
        credentials.extend(parse_named_credentials_json_object(
            json,
            "credentials environment variable",
        )?);
    }
    if let Some(path) = file {
        let text = std::fs::read_to_string(path).map_err(|error| {
            crate::error::ClientError::Transport(format!("{}: {error}", path.display()))
        })?;
        credentials.extend(parse_named_credentials_json_object(
            &text,
            &path.display().to_string(),
        )?);
    }
    for assignment in assignments {
        let (name, value) = assignment.split_once('=').ok_or_else(|| {
            crate::error::ClientError::Decode(format!(
                "invalid --credential {assignment:?}; expected SCHEME=VALUE"
            ))
        })?;
        if name.is_empty() || value.is_empty() {
            return Err(crate::error::ClientError::Decode(
                "named credential scheme and value must both be non-empty".to_string(),
            ));
        }
        credentials.insert(name.to_string(), value.to_string());
    }
    Ok(credentials)
}

fn parse_named_credentials_json_object(
    text: &str,
    source: &str,
) -> Result<BTreeMap<String, String>, crate::error::ClientError> {
    serde_json::from_str(text).map_err(|error| {
        crate::error::ClientError::Decode(format!(
            "{source} must be a JSON object mapping OpenAPI scheme names to credential strings: {error}"
        ))
    })
}

#[cfg(test)]
mod auth_tests {
    use super::*;
    use crate::profile::CredentialStore as _;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryStore {
        values: Mutex<BTreeMap<(String, String), String>>,
    }

    impl crate::profile::CredentialStore for MemoryStore {
        fn get_credential_secret(
            &self,
            profile: &str,
            scheme: &str,
        ) -> Result<Option<String>, crate::error::ClientError> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(&(profile.to_string(), scheme.to_string()))
                .cloned())
        }

        fn save_credential_secret(
            &self,
            profile: &str,
            scheme: &str,
            value: &str,
        ) -> Result<(), crate::error::ClientError> {
            self.values
                .lock()
                .unwrap()
                .insert((profile.to_string(), scheme.to_string()), value.to_string());
            Ok(())
        }

        fn delete_credential_secret(
            &self,
            profile: &str,
            scheme: &str,
        ) -> Result<bool, crate::error::ClientError> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .remove(&(profile.to_string(), scheme.to_string()))
                .is_some())
        }
    }

    fn client(token: Option<&str>, credentials: &[(&str, &str)]) -> Client {
        Client::new(
            "https://api.example.test".to_string(),
            token.map(str::to_string),
            None,
            None,
            credentials
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            "test".to_string(),
            Arc::new(MemoryStore::default()),
            false,
        )
    }

    fn oauth_client(base_url: String, store: Arc<MemoryStore>, now: u64) -> Client {
        let mut client = Client::new(
            base_url,
            None,
            Some("client-id".to_string()),
            Some("client-secret".to_string()),
            BTreeMap::new(),
            "test-profile".to_string(),
            store,
            false,
        );
        client.now = Arc::new(move || now);
        client
    }

    fn token_server(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 1024];
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some(headers_end) = text.find("\r\n\r\n") {
                    let content_length = text[..headers_end]
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("Content-Length: ")
                                .or_else(|| line.strip_prefix("content-length: "))
                        })
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= headers_end + 4 + content_length {
                        break;
                    }
                }
            }
            let request = String::from_utf8_lossy(&request).to_string();
            let reason = if status == 200 { "OK" } else { "Error" };
            write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            stream.flush().unwrap();
            request
        });
        (format!("http://{address}"), handle)
    }

    #[test]
    fn token_selects_bearer_not_basic_alternative() {
        static ALTERNATIVES: &[AuthAlternative] = &[
            AuthAlternative {
                schemes: &[AuthScheme {
                    name: "basicAuth",
                    kind: AuthSchemeKind::Basic,
                    scopes: &[],
                }],
            },
            AuthAlternative {
                schemes: &[AuthScheme {
                    name: "bearerAuth",
                    kind: AuthSchemeKind::Bearer,
                    scopes: &[],
                }],
            },
        ];
        let selected = client(Some("token"), &[])
            .select_satisfied_auth_alternative(ALTERNATIVES)
            .expect("bearer alternative should be satisfied");
        assert!(matches!(selected.schemes[0].kind, AuthSchemeKind::Bearer));
    }

    #[test]
    fn token_does_not_satisfy_named_and_requirement() {
        static ALTERNATIVES: &[AuthAlternative] = &[AuthAlternative {
            schemes: &[
                AuthScheme {
                    name: "primaryKey",
                    kind: AuthSchemeKind::Header("X-Primary-Key"),
                    scopes: &[],
                },
                AuthScheme {
                    name: "secondaryKey",
                    kind: AuthSchemeKind::Header("X-Secondary-Key"),
                    scopes: &[],
                },
            ],
        }];
        let error = client(Some("token"), &[])
            .select_satisfied_auth_alternative(ALTERNATIVES)
            .expect_err("every named scheme must be present");
        assert!(matches!(
            error,
            crate::error::ClientError::AuthenticationRequired { .. }
        ));
    }

    #[test]
    fn query_key_auth_is_added_to_the_selected_url() {
        static ALTERNATIVES: &[AuthAlternative] = &[AuthAlternative {
            schemes: &[AuthScheme {
                name: "queryKey",
                kind: AuthSchemeKind::QueryKey("api_key"),
                scopes: &[],
            }],
        }];
        let client = client(None, &[("queryKey", "secret/value")]);
        let selected = client
            .select_satisfied_auth_alternative(ALTERNATIVES)
            .unwrap();
        let url = client
            .resolve_request_url(RequestTarget::Relative("/items"), &[], Some(selected))
            .unwrap();
        assert_eq!(url, "https://api.example.test/items?api_key=secret%2Fvalue");
    }

    #[test]
    fn cookie_parameters_and_cookie_auth_share_one_header() {
        static ALTERNATIVES: &[AuthAlternative] = &[AuthAlternative {
            schemes: &[AuthScheme {
                name: "cookieKey",
                kind: AuthSchemeKind::CookieKey("auth"),
                scopes: &[],
            }],
        }];
        let client = client(None, &[("cookieKey", "secret")]);
        let selected = client
            .select_satisfied_auth_alternative(ALTERNATIVES)
            .unwrap();
        let request = client
            .prepare_http_request_with_auth_and_body(
                Method::Get,
                "https://api.example.test/items",
                &[("Cookie".to_string(), "preference=compact".to_string())],
                Some(selected),
                None,
            )
            .unwrap();
        assert_eq!(
            request.header("Cookie"),
            Some("preference=compact; auth=secret")
        );
    }

    #[test]
    fn reserved_query_values_and_path_styles_follow_openapi() {
        let mut query = Vec::new();
        append_serialized_parameter_values(
            &mut query,
            "target",
            &serde_json::json!("https://example.test/a?x=1"),
            ParameterStyle::FormExplode,
            true,
        );
        let url =
            append_query_parameters_to_url("https://api.example.test/items".to_string(), &query)
                .unwrap();
        assert_eq!(
            url,
            "https://api.example.test/items?target=https://example.test/a?x=1"
        );
        assert_eq!(
            serialize_path_parameter_value(
                "id",
                &serde_json::json!(["a/b", "c"]),
                ParameterStyle::LabelExplode,
            ),
            ".a%2Fb.c"
        );
        assert_eq!(
            serialize_path_parameter_value(
                "id",
                &serde_json::json!({"role": "admin", "name": "Alex"}),
                ParameterStyle::MatrixExplode,
            ),
            ";name=Alex;role=admin"
        );
    }

    #[test]
    fn multipart_metadata_sets_part_headers_and_declared_boundary() {
        let parts = build_multipart_request_body(
            serde_json::json!({"file": "-"}),
            &[],
            &[MultipartFieldEncoding {
                name: "file",
                content_type: Some("image/png"),
                headers: &[("X-Part-Kind", "avatar")],
            }],
        )
        .unwrap();
        let (content_type, body) =
            encode_multipart_body_with_boundary(parts, "multipart/form-data; boundary=declared");
        let body = String::from_utf8(body).unwrap();
        assert_eq!(content_type, "multipart/form-data; boundary=declared");
        assert!(body.contains("--declared\r\n"));
        assert!(body.contains("Content-Type: image/png\r\n"));
        assert!(body.contains("X-Part-Kind: avatar\r\n"));
    }

    #[test]
    fn explicit_anonymous_alternative_is_allowed() {
        static ALTERNATIVES: &[AuthAlternative] = &[
            AuthAlternative {
                schemes: &[AuthScheme {
                    name: "bearerAuth",
                    kind: AuthSchemeKind::Bearer,
                    scopes: &[],
                }],
            },
            AuthAlternative { schemes: &[] },
        ];
        let selected = client(None, &[])
            .select_satisfied_auth_alternative(ALTERNATIVES)
            .expect("anonymous alternative should be satisfied");
        assert!(selected.schemes.is_empty());
    }

    #[test]
    fn optional_auth_prefers_available_identity_even_when_anonymous_is_first() {
        static ALTERNATIVES: &[AuthAlternative] = &[
            AuthAlternative { schemes: &[] },
            AuthAlternative {
                schemes: &[AuthScheme {
                    name: "bearerAuth",
                    kind: AuthSchemeKind::Bearer,
                    scopes: &["profile"],
                }],
            },
        ];
        let selected = client(Some("token"), &[])
            .select_satisfied_auth_alternative(ALTERNATIVES)
            .expect("available identity should be preferred");
        assert_eq!(selected.schemes[0].name, "bearerAuth");
    }

    #[test]
    fn inferred_auth_is_explicitly_unsupported() {
        static ALTERNATIVES: &[AuthAlternative] = &[AuthAlternative {
            schemes: &[AuthScheme {
                name: "inferredLogin",
                kind: AuthSchemeKind::Inferred,
                scopes: &[],
            }],
        }];
        let error = client(None, &[])
            .select_satisfied_auth_alternative(ALTERNATIVES)
            .expect_err("inferred auth must not become anonymous");
        assert!(matches!(
            error,
            crate::error::ClientError::UnsupportedAuth(_)
        ));
    }

    #[test]
    fn oauth_cache_hit_avoids_token_endpoint() {
        let store = Arc::new(MemoryStore::default());
        let client = oauth_client("http://127.0.0.1:9".to_string(), store.clone(), 1_000);
        let endpoint = "http://127.0.0.1:9/token";
        let key = oauth_client_credentials_cache_scheme(
            "test-profile",
            "machineOAuth",
            "client-id",
            &["read"],
            endpoint,
        );
        store
            .save_credential_secret(
                "test-profile",
                &key,
                "{\"access_token\":\"cached\",\"expires_at\":1001}",
            )
            .unwrap();

        let token = client
            .fetch_client_credentials_token("machineOAuth", endpoint, &["read"])
            .unwrap();
        assert_eq!(token, "cached");
    }

    #[test]
    fn expired_oauth_cache_refreshes_and_overwrites_without_secret() {
        let store = Arc::new(MemoryStore::default());
        let (base_url, server) =
            token_server(200, "{\"access_token\":\"fresh\",\"expires_in\":100}");
        let endpoint = format!("{base_url}/token");
        let client = oauth_client(base_url, store.clone(), 1_000);
        let key = oauth_client_credentials_cache_scheme(
            "test-profile",
            "machineOAuth",
            "client-id",
            &["read"],
            &endpoint,
        );
        store
            .save_credential_secret(
                "test-profile",
                &key,
                "{\"access_token\":\"stale\",\"expires_at\":999}",
            )
            .unwrap();

        let token = client
            .fetch_client_credentials_token("machineOAuth", &endpoint, &["read"])
            .unwrap();
        assert_eq!(token, "fresh");
        let request = server.join().unwrap();
        assert!(request.contains("scope=read"));
        let cached = store
            .get_credential_secret("test-profile", &key)
            .unwrap()
            .unwrap();
        assert!(cached.contains("\"access_token\":\"fresh\""));
        assert!(cached.contains("\"expires_at\":1090"));
        assert!(!cached.contains("client-secret"));
    }

    #[test]
    fn malformed_oauth_response_is_rejected() {
        let store = Arc::new(MemoryStore::default());
        let (base_url, server) = token_server(200, "{\"access_token\":\"token\"}");
        let client = oauth_client(base_url.clone(), store, 1_000);
        let error = client
            .fetch_client_credentials_token("machineOAuth", &format!("{base_url}/token"), &[])
            .expect_err("expires_in is required for a safe cache");
        server.join().unwrap();
        assert!(matches!(error, crate::error::ClientError::Decode(_)));
    }

    #[test]
    fn oauth_endpoint_error_is_reported() {
        let store = Arc::new(MemoryStore::default());
        let (base_url, server) = token_server(503, "{\"error\":\"unavailable\"}");
        let client = oauth_client(base_url.clone(), store, 1_000);
        let error = client
            .fetch_client_credentials_token("machineOAuth", &format!("{base_url}/token"), &[])
            .expect_err("non-success token status must fail");
        server.join().unwrap();
        assert!(
            matches!(error, crate::error::ClientError::Transport(message) if message.contains("503"))
        );
    }
}

/// Percent-encodes a single path segment (RFC 3986 unreserved set preserved).
pub fn percent_encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Reads a `--body` argument's content: `-` means stdin, anything else is a file path.
pub fn read_request_body_text(source: &str) -> Result<String, crate::error::ClientError> {
    if source == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        Ok(buffer)
    } else {
        std::fs::read_to_string(source)
            .map_err(|error| crate::error::ClientError::Transport(format!("{source}: {error}")))
    }
}

/// Reads raw bytes for binary request bodies and multipart file fields.
pub fn read_request_body_bytes(source: &str) -> Result<Vec<u8>, crate::error::ClientError> {
    if source == "-" {
        let mut buffer = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buffer)
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        Ok(buffer)
    } else {
        std::fs::read(source)
            .map_err(|error| crate::error::ClientError::Transport(format!("{source}: {error}")))
    }
}
