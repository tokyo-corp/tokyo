use super::*;
use crate::profile::CredentialStore as _;

#[derive(Default)]
struct MemoryStore(std::sync::Mutex<std::collections::BTreeMap<(String, String), String>>);

impl crate::profile::CredentialStore for MemoryStore {
    fn get_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
    ) -> Result<Option<String>, crate::error::ClientError> {
        Ok(self
            .0
            .lock()
            .expect("memory store lock")
            .get(&(profile.to_string(), scheme.to_string()))
            .cloned())
    }

    fn save_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
        value: &str,
    ) -> Result<(), crate::error::ClientError> {
        self.0
            .lock()
            .expect("memory store lock")
            .insert((profile.to_string(), scheme.to_string()), value.to_string());
        Ok(())
    }

    fn delete_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
    ) -> Result<bool, crate::error::ClientError> {
        Ok(self
            .0
            .lock()
            .expect("memory store lock")
            .remove(&(profile.to_string(), scheme.to_string()))
            .is_some())
    }
}

fn test_oauth_provider_for_scheme() -> OAuthProvider {
    OAuthProvider {
        scheme: "bearerAuth",
        client_id: "public-client",
        redirect_uri: None,
        endpoints: OAuthEndpoints::Explicit {
            authorization_url: None,
            token_url: "https://identity.example.test/token",
            device_authorization_url: None,
        },
        scopes: &["openid", "offline_access"],
        audience: Some("https://api.example.test"),
    }
}

fn json_response(body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let mut response = tiny_http::Response::from_string(body);
    response.add_header(
        tiny_http::Header::from_bytes(b"Content-Type", b"application/json")
            .expect("JSON content type"),
    );
    response
}

#[test]
fn parses_a_loopback_callback_without_exposing_the_code_in_the_response() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind callback test listener");
    let address = server.server_addr().to_ip().expect("callback IP listener");
    let sender = std::thread::spawn(move || {
        ureq::get(&format!(
            "http://{address}/callback?code=secret-code&state=secret-state"
        ))
        .call()
        .expect("send callback")
        .into_string()
        .expect("read browser response")
    });
    let callback =
        wait_for_loopback_oauth_callback(&server, Duration::from_secs(5)).expect("parse callback");
    assert_eq!(callback.code, "secret-code");
    assert_eq!(callback.state, "secret-state");
    let response = sender.join().expect("callback sender");
    assert!(response.contains("Authentication complete"));
    assert!(!response.contains("secret-code"));
    assert!(!response.contains("secret-state"));
}

#[test]
fn completes_pkce_and_sends_the_verifier_to_the_token_endpoint() {
    let token_server = tiny_http::Server::http("127.0.0.1:0").expect("bind token server");
    let token_address = token_server.server_addr().to_ip().expect("token server IP");
    let token_thread = std::thread::spawn(move || {
        let mut request = token_server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive token request")
            .expect("token request before timeout");
        assert_eq!(request.url(), "/token");
        let mut body = String::new();
        request
            .as_reader()
            .read_to_string(&mut body)
            .expect("read token request");
        let fields = url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(fields.get("code").map(String::as_str), Some("auth-code"));
        assert!(
            fields
                .get("code_verifier")
                .is_some_and(|value| !value.is_empty())
        );
        request
            .respond(json_response(
                "{\"access_token\":\"pkce-access\",\"token_type\":\"bearer\",\"expires_in\":300,\"refresh_token\":\"pkce-refresh\"}",
            ))
            .expect("respond to token request");
    });
    let resolved = ResolvedProvider {
        authorization_url: Some("https://identity.example.test/authorize".to_string()),
        token_url: format!("http://{token_address}/token"),
        device_authorization_url: None,
    };

    let token = login_with_pkce_authorization_code_client(
        &test_oauth_provider_for_scheme(),
        &resolved,
        Duration::from_secs(5),
        |authorization_url| {
            let fields = authorization_url
                .query_pairs()
                .into_owned()
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(
                fields.get("code_challenge_method").map(String::as_str),
                Some("S256")
            );
            assert!(
                fields
                    .get("code_challenge")
                    .is_some_and(|value| !value.is_empty())
            );
            assert_eq!(
                fields.get("audience").map(String::as_str),
                Some("https://api.example.test")
            );
            let state = fields.get("state").expect("authorization state").clone();
            let mut callback =
                url::Url::parse(fields.get("redirect_uri").expect("loopback redirect URI"))
                    .expect("valid loopback redirect URI");
            callback
                .query_pairs_mut()
                .append_pair("code", "auth-code")
                .append_pair("state", &state);
            std::thread::spawn(move || {
                ureq::get(callback.as_str())
                    .call()
                    .expect("send successful callback");
            });
        },
    )
    .expect("PKCE login succeeds");

    assert_eq!(token.access_token, "pkce-access");
    assert_eq!(token.refresh_token.as_deref(), Some("pkce-refresh"));
    token_thread.join().expect("token server");
}

#[test]
fn reports_authorization_denial_and_callback_timeout() {
    let resolved = ResolvedProvider {
        authorization_url: Some("https://identity.example.test/authorize".to_string()),
        token_url: "http://127.0.0.1:1/token".to_string(),
        device_authorization_url: None,
    };
    let denial = login_with_pkce_authorization_code_client(
        &test_oauth_provider_for_scheme(),
        &resolved,
        Duration::from_secs(5),
        |authorization_url| {
            let fields = authorization_url
                .query_pairs()
                .into_owned()
                .collect::<std::collections::BTreeMap<_, _>>();
            let mut callback =
                url::Url::parse(fields.get("redirect_uri").expect("loopback redirect URI"))
                    .expect("valid loopback redirect URI");
            callback
                .query_pairs_mut()
                .append_pair("error", "access_denied")
                .append_pair("state", fields.get("state").expect("authorization state"));
            std::thread::spawn(move || {
                let _ = ureq::get(callback.as_str()).call();
            });
        },
    )
    .expect_err("authorization denial fails login");
    assert!(denial.to_string().contains("access_denied"));

    let timeout = login_with_pkce_authorization_code_client(
        &test_oauth_provider_for_scheme(),
        &resolved,
        Duration::from_millis(10),
        |_| {},
    )
    .expect_err("missing callback times out");
    assert!(
        timeout
            .to_string()
            .contains("timed out waiting for OAuth browser callback")
    );
}

#[test]
fn completes_device_authorization_against_an_oauth_server() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind OAuth test server");
    let address = server.server_addr().to_ip().expect("OAuth test IP");
    let server_thread = std::thread::spawn(move || {
        for (path, body) in [
            (
                "/device",
                "{\"device_code\":\"device-code\",\"user_code\":\"ABCD-EFGH\",\"verification_uri\":\"https://identity.example.test/activate\",\"verification_uri_complete\":\"https://identity.example.test/activate?user_code=ABCD-EFGH\",\"expires_in\":300,\"interval\":0}",
            ),
            (
                "/token",
                "{\"access_token\":\"device-access\",\"token_type\":\"bearer\",\"expires_in\":300,\"refresh_token\":\"device-refresh\"}",
            ),
        ] {
            let request = server
                .recv_timeout(Duration::from_secs(5))
                .expect("receive OAuth request")
                .expect("OAuth request before timeout");
            assert_eq!(request.url(), path);
            request
                .respond(json_response(body))
                .expect("respond to OAuth request");
        }
    });
    let token_url = format!("http://{address}/token");
    let device_url = format!("http://{address}/device");
    let provider = test_oauth_provider_for_scheme();
    let resolved = ResolvedProvider {
        authorization_url: None,
        token_url,
        device_authorization_url: Some(device_url),
    };

    let token = login_with_device_authorization(&provider, &resolved, true)
        .expect("device authorization succeeds");
    assert_eq!(token.access_token, "device-access");
    assert_eq!(token.refresh_token.as_deref(), Some("device-refresh"));
    server_thread.join().expect("OAuth test server");
}

#[test]
fn device_authorization_stops_when_polling_times_out() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind OAuth test server");
    let address = server.server_addr().to_ip().expect("OAuth test IP");
    let server_thread = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive device request")
            .expect("device request before timeout");
        assert_eq!(request.url(), "/device");
        request
            .respond(json_response(
                "{\"device_code\":\"device-code\",\"user_code\":\"ABCD-EFGH\",\"verification_uri\":\"https://identity.example.test/activate\",\"expires_in\":300,\"interval\":0}",
            ))
            .expect("respond with device details");
        while let Some(request) = server
            .recv_timeout(Duration::from_millis(50))
            .expect("receive polling request")
        {
            assert_eq!(request.url(), "/token");
            request
                .respond(
                    json_response(
                        "{\"error\":\"authorization_pending\",\"error_description\":\"waiting\"}",
                    )
                    .with_status_code(400),
                )
                .expect("respond with pending authorization");
        }
    });
    let resolved = ResolvedProvider {
        authorization_url: None,
        token_url: format!("http://{address}/token"),
        device_authorization_url: Some(format!("http://{address}/device")),
    };

    let error = login_with_device_authorization_timeout(
        &test_oauth_provider_for_scheme(),
        &resolved,
        true,
        Duration::from_millis(20),
    )
    .expect_err("pending device login times out");
    assert!(error.to_string().contains("expired_token"));
    server_thread.join().expect("OAuth test server");
}

#[test]
fn refreshes_access_tokens_and_preserves_rotating_refresh_state() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind token server");
    let address = server.server_addr().to_ip().expect("token server IP");
    let server_thread = std::thread::spawn(move || {
        let mut request = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive refresh request")
            .expect("refresh request before timeout");
        let mut body = String::new();
        request
            .as_reader()
            .read_to_string(&mut body)
            .expect("read refresh request");
        let fields = url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            fields.get("grant_type").map(String::as_str),
            Some("refresh_token")
        );
        assert_eq!(
            fields.get("refresh_token").map(String::as_str),
            Some("old-refresh")
        );
        request
            .respond(json_response(
                "{\"access_token\":\"refreshed-access\",\"token_type\":\"bearer\",\"expires_in\":300}",
            ))
            .expect("respond to refresh request");
    });
    let resolved = ResolvedProvider {
        authorization_url: None,
        token_url: format!("http://{address}/token"),
        device_authorization_url: None,
    };

    let token = refresh_managed_oauth_access_token(
        &test_oauth_provider_for_scheme(),
        &resolved,
        "old-refresh".to_string(),
    )
    .expect("refresh succeeds");
    assert_eq!(token.access_token, "refreshed-access");
    assert_eq!(token.refresh_token.as_deref(), Some("old-refresh"));
    server_thread.join().expect("token server");
}

#[test]
fn rejects_malformed_or_untrusted_provider_metadata() {
    let insecure = OAuthProvider {
        endpoints: OAuthEndpoints::Explicit {
            authorization_url: None,
            token_url: "http://identity.example.test/token",
            device_authorization_url: None,
        },
        ..test_oauth_provider_for_scheme()
    };
    let error = resolve_oauth_provider_for_scheme(&insecure).expect_err("remote HTTP is rejected");
    assert!(error.to_string().contains("must use https"));

    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind discovery server");
    let address = server.server_addr().to_ip().expect("discovery server IP");
    let issuer: &'static str = Box::leak(format!("http://{address}").into_boxed_str());
    let server_thread = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive discovery request")
            .expect("discovery request before timeout");
        assert_eq!(request.url(), "/.well-known/openid-configuration");
        request
            .respond(json_response(
                "{\"issuer\":\"https://attacker.example.test\",\"authorization_endpoint\":\"https://attacker.example.test/authorize\",\"token_endpoint\":\"https://attacker.example.test/token\"}",
            ))
            .expect("respond with mismatched discovery");
    });
    let mismatched = OAuthProvider {
        endpoints: OAuthEndpoints::Discovery { issuer },
        ..test_oauth_provider_for_scheme()
    };
    let error =
        resolve_oauth_provider_for_scheme(&mismatched).expect_err("issuer mismatch is rejected");
    assert!(error.to_string().contains("issuer mismatch"));
    server_thread.join().expect("discovery server");

    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind malformed server");
    let address = server.server_addr().to_ip().expect("malformed server IP");
    let issuer: &'static str = Box::leak(format!("http://{address}").into_boxed_str());
    let server_thread = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive malformed discovery request")
            .expect("discovery request before timeout");
        request
            .respond(json_response("{not-json"))
            .expect("respond with malformed discovery");
    });
    let malformed = OAuthProvider {
        endpoints: OAuthEndpoints::Discovery { issuer },
        ..test_oauth_provider_for_scheme()
    };
    let error =
        resolve_oauth_provider_for_scheme(&malformed).expect_err("malformed discovery is rejected");
    assert!(matches!(error, crate::error::ClientError::Decode(_)));
    server_thread.join().expect("malformed discovery server");
}

#[test]
fn doctor_reports_discovered_provider_capabilities() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind doctor server");
    let address = server.server_addr().to_ip().expect("doctor server IP");
    let issuer: &'static str = Box::leak(format!("http://{address}").into_boxed_str());
    let metadata = format!(
        "{{\"issuer\":\"{issuer}\",\"authorization_endpoint\":\"{issuer}/authorize\",\"token_endpoint\":\"{issuer}/token\",\"device_authorization_endpoint\":\"{issuer}/device\",\"scopes_supported\":[\"openid\",\"offline_access\"],\"grant_types_supported\":[\"authorization_code\",\"refresh_token\",\"urn:ietf:params:oauth:grant-type:device_code\"],\"code_challenge_methods_supported\":[\"S256\"],\"token_endpoint_auth_methods_supported\":[\"none\"]}}"
    );
    let server_thread = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive doctor discovery")
            .expect("doctor discovery before timeout");
        request
            .respond(json_response(&metadata))
            .expect("respond to doctor discovery");
    });
    let provider = OAuthProvider {
        endpoints: OAuthEndpoints::Discovery { issuer },
        ..test_oauth_provider_for_scheme()
    };

    let report = doctor_oauth_provider_for_scheme(&provider).expect("doctor completes");
    assert!(report.healthy, "{report:?}");
    for expected in [
        "discovery",
        "endpoints",
        "pkce",
        "device_authorization",
        "callback",
        "scopes",
        "refresh",
        "public_client",
    ] {
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == expected && check.status == "pass")
        );
    }
    server_thread.join().expect("doctor discovery server");
}

#[test]
fn preserves_refresh_tokens_and_computes_expiry() {
    let response: BasicTokenResponse = serde_json::from_str(
        "{\"access_token\":\"access\",\"token_type\":\"bearer\",\"expires_in\":300,\"refresh_token\":\"refresh\"}",
    )
    .expect("valid OAuth token response");
    let before = current_unix_timestamp_seconds();
    let stored = build_oauth_token_record(&response, None);
    assert_eq!(stored.access_token, "access");
    assert_eq!(stored.refresh_token.as_deref(), Some("refresh"));
    assert!(
        stored
            .expires_at
            .is_some_and(|expiry| expiry >= before + 300)
    );
}

#[test]
fn stores_managed_tokens_separately_from_the_credential_value() {
    let store = MemoryStore::default();
    let token = StoredOAuthToken {
        access_token: "access".to_string(),
        refresh_token: Some("refresh".to_string()),
        expires_at: Some(current_unix_timestamp_seconds() + 300),
    };
    save_oauth_token_record(&store, "staging", "bearerAuth", token).expect("save managed token");

    assert_eq!(
        store
            .get_credential_secret("staging", "bearerAuth")
            .expect("read access token")
            .as_deref(),
        Some("access")
    );
    let status = oauth_credential_status(&store, "staging", "bearerAuth")
        .expect("read OAuth status")
        .expect("managed status");
    assert!(status.managed);
    assert!(status.refreshable);
    assert_eq!(
        load_or_refresh_managed_oauth_access_token(&store, "staging", "bearerAuth")
            .expect("load managed token")
            .as_deref(),
        Some("access")
    );
    assert!(
        remove_oauth_credential_and_cached_tokens(&store, "staging", "bearerAuth").expect("logout")
    );
    assert!(
        store
            .get_credential_secret("staging", "bearerAuth")
            .expect("read removed token")
            .is_none()
    );
}

#[test]
fn browser_token_requires_an_allowed_environment() {
    let allowed = &["Development", "Staging"];
    let missing = require_allowed_browser_token_environment("browser-token auth", None, allowed)
        .expect_err("missing environment is rejected");
    assert!(missing.to_string().contains("--environment"));
    let production = require_allowed_browser_token_environment(
        "browser-token auth",
        Some("Production"),
        allowed,
    )
    .expect_err("disallowed environment is rejected");
    assert!(production.to_string().contains("not permitted"));
    assert_eq!(
        require_allowed_browser_token_environment("browser-token auth", Some("Staging"), allowed)
            .expect("allowed environment"),
        "Staging"
    );
}

#[test]
fn extracts_jwt_exp_without_verifying_the_signature() {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(br#"{"sub":"user","exp":1234567890}"#);
    assert_eq!(
        extract_jwt_expiration_timestamp(&format!("header.{payload}.untrusted-signature")),
        Some(1_234_567_890)
    );
    assert_eq!(extract_jwt_expiration_timestamp("opaque-token"), None);
    assert_eq!(
        extract_jwt_expiration_timestamp("header.invalid.signature"),
        None
    );
}

#[test]
fn validates_and_stores_a_browser_token_as_managed() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind validation server");
    let address = server.server_addr().to_ip().expect("validation server IP");
    let server_thread = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive validation request")
            .expect("validation request before timeout");
        assert_eq!(
            request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Authorization"))
                .map(|header| header.value.as_str()),
            Some("Bearer header.payload.signature")
        );
        request
            .respond(tiny_http::Response::empty(204))
            .expect("respond to validation request");
    });
    let store = MemoryStore::default();
    let validation_url = format!("http://{address}/validate");

    let token = login_with_browser_token_provider_token(
        &store,
        "default",
        "bearerAuth",
        &validation_url,
        "  header.payload.signature  ",
    )
    .expect("browser token validates");
    assert_eq!(token, "header.payload.signature");
    assert_eq!(
        store
            .get_credential_secret("default", "bearerAuth")
            .expect("read token")
            .as_deref(),
        Some("header.payload.signature")
    );
    let status = oauth_credential_status(&store, "default", "bearerAuth")
        .expect("read status")
        .expect("managed status");
    assert!(!status.refreshable);
    server_thread.join().expect("validation server");
}

#[test]
fn browser_token_validation_failure_does_not_store_or_leak_token() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind validation server");
    let address = server.server_addr().to_ip().expect("validation server IP");
    let server_thread = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive validation request")
            .expect("validation request before timeout");
        request
            .respond(tiny_http::Response::empty(401))
            .expect("respond to validation request");
    });
    let store = MemoryStore::default();
    let validation_url = format!("http://{address}/validate");
    let secret = "secret-browser-token";

    let error = login_with_browser_token_provider_token(
        &store,
        "default",
        "bearerAuth",
        &validation_url,
        secret,
    )
    .expect_err("unauthorized token is rejected");
    assert!(error.to_string().contains("HTTP 401"));
    assert!(!error.to_string().contains(secret));
    assert!(
        store
            .get_credential_secret("default", "bearerAuth")
            .expect("read absent token")
            .is_none()
    );
    server_thread.join().expect("validation server");
}

#[test]
fn expired_non_refreshable_token_requires_login_again() {
    let store = MemoryStore::default();
    save_oauth_token_record(
        &store,
        "default",
        "bearerAuth",
        StoredOAuthToken {
            access_token: "expired".to_string(),
            refresh_token: None,
            expires_at: Some(current_unix_timestamp_seconds().saturating_sub(1)),
        },
    )
    .expect("save expired token");

    let error = load_or_refresh_managed_oauth_access_token(&store, "default", "bearerAuth")
        .expect_err("expired token cannot be used");
    assert!(error.to_string().contains("rerun `auth login"));
}

#[test]
fn identity_projection_exposes_only_configured_fields() {
    let body = serde_json::json!({
        "caller": { "org_type": "provider", "org_role": "operator" },
        "raw_claims": { "secret": "must-not-leak" },
        "credential": "must-not-leak"
    });
    let projected = project_configured_identity_fields_from_json(
        &body,
        &[
            ("org_type", "/caller/org_type"),
            ("org_role", "/caller/org_role"),
        ],
    );
    assert_eq!(
        projected,
        serde_json::json!({ "org_type": "provider", "org_role": "operator" })
    );
}

#[test]
fn falls_back_to_rfc8414_authorization_server_metadata() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind discovery server");
    let address = server.server_addr().to_ip().expect("discovery IP");
    let issuer: &'static str = Box::leak(format!("http://{address}").into_boxed_str());
    let server_thread = std::thread::spawn(move || {
        let first = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive OIDC discovery")
            .expect("OIDC request");
        assert_eq!(first.url(), "/.well-known/openid-configuration");
        first
            .respond(tiny_http::Response::empty(404))
            .expect("OIDC not found");
        let second = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive RFC 8414 discovery")
            .expect("RFC 8414 request");
        assert_eq!(second.url(), "/.well-known/oauth-authorization-server");
        second
            .respond(json_response(&format!(
                "{{\"issuer\":\"{issuer}\",\"authorization_endpoint\":\"{issuer}/authorize\",\"token_endpoint\":\"{issuer}/token\"}}"
            )))
            .expect("RFC 8414 response");
    });
    let metadata =
        discover_authorization_server_metadata(issuer).expect("RFC 8414 fallback succeeds");
    let expected_token_endpoint = format!("{issuer}/token");
    assert_eq!(
        metadata.token_endpoint.as_deref(),
        Some(expected_token_endpoint.as_str())
    );
    server_thread.join().expect("discovery server");
}

#[test]
fn discovers_authorization_server_from_rfc9728_resource_metadata() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind resource server");
    let address = server.server_addr().to_ip().expect("resource IP");
    let resource: &'static str = Box::leak(format!("http://{address}").into_boxed_str());
    let server_thread = std::thread::spawn(move || {
        let resource_request = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive resource metadata")
            .expect("resource request");
        assert_eq!(
            resource_request.url(),
            "/.well-known/oauth-protected-resource"
        );
        resource_request
            .respond(json_response(&format!(
                "{{\"resource\":\"{resource}\",\"authorization_servers\":[\"{resource}\"]}}"
            )))
            .expect("resource metadata response");
        let issuer_request = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive issuer metadata")
            .expect("issuer request");
        assert_eq!(issuer_request.url(), "/.well-known/openid-configuration");
        issuer_request
            .respond(json_response(&format!(
                "{{\"issuer\":\"{resource}\",\"token_endpoint\":\"{resource}/token\",\"device_authorization_endpoint\":\"{resource}/device\"}}"
            )))
            .expect("issuer metadata response");
    });
    let metadata = discover_provider_from_protected_resource(resource, None)
        .expect("RFC 9728 discovery succeeds");
    let expected_device_endpoint = format!("{resource}/device");
    assert_eq!(
        metadata.device_authorization_endpoint.as_deref(),
        Some(expected_device_endpoint.as_str())
    );
    server_thread.join().expect("resource server");
}

#[test]
fn workos_preset_resolves_public_device_endpoints() {
    let provider = OAuthProvider {
        endpoints: OAuthEndpoints::Workos {
            authkit_domain: "https://example.authkit.app/",
        },
        ..test_oauth_provider_for_scheme()
    };
    let resolved = resolve_oauth_provider_for_scheme(&provider).expect("resolve WorkOS preset");
    assert_eq!(
        resolved.token_url,
        "https://example.authkit.app/oauth2/token"
    );
    assert_eq!(
        resolved.device_authorization_url.as_deref(),
        Some("https://example.authkit.app/oauth2/device_authorization")
    );
    assert!(resolved.authorization_url.is_none());
}

#[test]
fn generic_whoami_projects_only_standard_identity_and_actor_claims() {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(br#"{"sub":"agent_1","org_id":"org_1","act":{"sub":"user_1"},"secret":"hidden"}"#);
    let identity = project_standard_identity_claims_from_jwt(&format!("header.{payload}.sig"))
        .expect("project JWT identity");
    assert_eq!(
        identity,
        serde_json::json!({
            "sub": "agent_1",
            "org_id": "org_1",
            "act": {"sub": "user_1"},
        })
    );
}

#[test]
fn bound_token_records_reject_changed_provider_configuration() {
    let token = StoredOAuthToken {
        access_token: "access".to_string(),
        refresh_token: None,
        expires_at: None,
    };
    let raw = serde_json::to_string(&serde_json::json!({
        "binding": "different-provider",
        "token": token,
    }))
    .expect("encode record");
    let error = decode_bound_oauth_token_record("bearerAuth", &raw)
        .expect_err("binding mismatch is rejected");
    assert!(error.to_string().contains("different provider settings"));
}

#[test]
fn anonymous_agent_registration_exchanges_assertion_and_keeps_nested_refresh_token() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind agent server");
    let address = server.server_addr().to_ip().expect("agent IP");
    let authorization_server: &'static str =
        Box::leak(format!("http://{address}").into_boxed_str());
    let server_thread = std::thread::spawn(move || {
        let registration = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive registration")
            .expect("registration request");
        assert_eq!(registration.url(), "/agent/identity");
        registration
            .respond(json_response(
                r#"{"identity":{"assertion":"agent-assertion","refresh_token":{"value":"identity-refresh"}}}"#,
            ))
            .expect("registration response");

        let exchange = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive assertion exchange")
            .expect("exchange request");
        assert_eq!(exchange.url(), "/oauth2/token");
        exchange
            .respond(json_response(
                r#"{"access_token":"agent-access","expires_in":300}"#,
            ))
            .expect("exchange response");
    });
    let provider = OAuthProvider {
        endpoints: OAuthEndpoints::AgentRegistration {
            authorization_server,
            identity_type: "anonymous",
            login_hint_env: None,
        },
        ..test_oauth_provider_for_scheme()
    };
    let token = login_with_agent_registration(&provider, None).expect("register anonymous agent");
    assert_eq!(token.access_token, "agent-access");
    assert_eq!(token.refresh_token.as_deref(), Some("identity-refresh"));
    assert!(token.expires_at.is_some());
    server_thread.join().expect("agent server");
}

#[test]
fn broker_accepts_immediate_oauth_shaped_credentials() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind broker server");
    let address = server.server_addr().to_ip().expect("broker IP");
    let begin_url: &'static str = Box::leak(format!("http://{address}/begin").into_boxed_str());
    let server_thread = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .expect("receive broker begin")
            .expect("broker request");
        assert_eq!(request.url(), "/begin");
        request
            .respond(json_response(
                r#"{"access_token":"broker-access","refresh_token":"broker-refresh","expires_in":60}"#,
            ))
            .expect("broker response");
    });
    let provider = OAuthProvider {
        endpoints: OAuthEndpoints::Broker { begin_url },
        ..test_oauth_provider_for_scheme()
    };
    let token = login_with_auth_broker(&provider).expect("broker login");
    assert_eq!(token.access_token, "broker-access");
    assert_eq!(token.refresh_token.as_deref(), Some("broker-refresh"));
    server_thread.join().expect("broker server");
}
