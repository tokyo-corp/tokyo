use std::collections::HashMap;

use oas3::Spec;
use oas3::spec::SecurityScheme;
use tokyo_ir::auth::{AuthRequirement, AuthScheme, AuthSchemeKind, AuthSchemeRequirement};

use crate::error::ImportError;

pub fn resolve_auth_schemes(
    openapi_spec: &Spec,
    operation_or_global_security_requirements: &[oas3::spec::SecurityRequirement],
    oauth_token_endpoints: &HashMap<String, String>,
) -> Result<Vec<AuthRequirement>, ImportError> {
    let mut auth_requirement_alternatives = Vec::new();
    for security_requirement_alternative in operation_or_global_security_requirements {
        let mut schemes_required_together = Vec::new();
        for (security_scheme_name, required_oauth_scopes) in &security_requirement_alternative.0 {
            schemes_required_together.push(AuthSchemeRequirement {
                scheme: resolve_one_security_scheme(
                    openapi_spec,
                    security_scheme_name,
                    oauth_token_endpoints,
                )?,
                scopes: required_oauth_scopes.clone(),
            });
        }
        auth_requirement_alternatives.push(AuthRequirement {
            schemes: schemes_required_together,
        });
    }
    Ok(auth_requirement_alternatives)
}

fn resolve_one_security_scheme(
    openapi_spec: &Spec,
    scheme_name: &str,
    oauth_token_endpoints: &HashMap<String, String>,
) -> Result<AuthScheme, ImportError> {
    let openapi_components = openapi_spec.components.as_ref().ok_or_else(|| {
        ImportError::Unsupported(format!(
            "security scheme `{scheme_name}` referenced but spec has no components"
        ))
    })?;

    let unresolved_security_scheme = openapi_components
        .security_schemes
        .get(scheme_name)
        .ok_or_else(|| {
            ImportError::Unsupported(format!("unknown security scheme `{scheme_name}`"))
        })?;
    let resolved_security_scheme = unresolved_security_scheme.resolve(openapi_spec)?;

    match resolved_security_scheme {
        SecurityScheme::Http {
            scheme: http_auth_scheme_name,
            ..
        } if http_auth_scheme_name.eq_ignore_ascii_case("bearer") => {
            Ok(build_named_auth_scheme(scheme_name, AuthSchemeKind::Bearer))
        }
        SecurityScheme::Http {
            scheme: http_auth_scheme_name,
            ..
        } if http_auth_scheme_name.eq_ignore_ascii_case("basic") => {
            Ok(build_named_auth_scheme(scheme_name, AuthSchemeKind::Basic))
        }
        SecurityScheme::ApiKey {
            name: api_key_parameter_name,
            location,
            ..
        } if location == "header" => Ok(build_named_auth_scheme(
            scheme_name,
            AuthSchemeKind::Header {
                name: api_key_parameter_name,
            },
        )),
        SecurityScheme::ApiKey {
            name: api_key_parameter_name,
            location,
            ..
        } if location == "query" => Ok(build_named_auth_scheme(
            scheme_name,
            AuthSchemeKind::QueryKey {
                name: api_key_parameter_name,
            },
        )),
        SecurityScheme::ApiKey {
            name: api_key_parameter_name,
            location,
            ..
        } if location == "cookie" => Ok(build_named_auth_scheme(
            scheme_name,
            AuthSchemeKind::CookieKey {
                name: api_key_parameter_name,
            },
        )),
        SecurityScheme::OAuth2 { flows, .. } => {
            if let Some(client_credentials_flow) = flows.client_credentials {
                return Ok(build_named_auth_scheme(
                    scheme_name,
                    AuthSchemeKind::OAuth2 {
                        token_endpoint: Some(
                            oauth_token_endpoints
                                .get(scheme_name)
                                .cloned()
                                .unwrap_or_else(|| client_credentials_flow.token_url.to_string()),
                        ),
                    },
                ));
            }
            if flows.authorization_code.is_some()
                || flows.implicit.is_some()
                || flows.password.is_some()
            {
                return Ok(build_named_auth_scheme(
                    scheme_name,
                    AuthSchemeKind::OAuth2 {
                        token_endpoint: None,
                    },
                ));
            }
            Err(ImportError::Unsupported(format!(
                "security scheme `{scheme_name}` declares an OAuth2 scheme with no flows"
            )))
        }
        unsupported_security_scheme => Err(ImportError::Unsupported(format!(
            "security scheme `{scheme_name}` has unsupported shape: {unsupported_security_scheme:?}"
        ))),
    }
}

fn build_named_auth_scheme(
    openapi_security_scheme_name: &str,
    auth_scheme_kind: AuthSchemeKind,
) -> AuthScheme {
    AuthScheme {
        name: openapi_security_scheme_name.to_string(),
        kind: auth_scheme_kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint<'a>(api: &'a tokyo_ir::Api, name: &str) -> &'a tokyo_ir::http::Endpoint {
        api.endpoints
            .iter()
            .find(|endpoint| endpoint.name == name)
            .expect("fixture endpoint should exist")
    }

    #[test]
    fn preserves_or_and_anonymous_security_semantics() {
        use tokyo_ir::auth::CommandAccess;
        let api =
            crate::import_openapi_yaml_document(include_str!("../../../../examples/auth.yaml"))
                .expect("auth fixture should import");

        let inherited = endpoint(&api, "inheritedAuth");
        assert_eq!(
            CommandAccess::from_requirements(&inherited.auth),
            CommandAccess::Authenticated
        );
        assert_eq!(inherited.auth.len(), 1);
        assert_eq!(inherited.auth[0].schemes[0].scheme.name, "primaryKey");

        let public = endpoint(&api, "publicOperation");
        assert!(public.auth.is_empty());
        assert_eq!(
            CommandAccess::from_requirements(&public.auth),
            CommandAccess::Public
        );

        let optional = endpoint(&api, "optionalAuth");
        assert_eq!(optional.auth.len(), 2);
        assert_eq!(optional.auth[0].schemes[0].scheme.name, "bearerAuth");
        assert!(optional.auth[1].schemes.is_empty());
        assert_eq!(
            CommandAccess::from_requirements(&optional.auth),
            CommandAccess::Optional
        );

        let alternatives = endpoint(&api, "alternativeAuth");
        assert_eq!(alternatives.auth.len(), 2);
        assert_eq!(alternatives.auth[0].schemes.len(), 1);
        assert_eq!(alternatives.auth[1].schemes.len(), 1);

        let combined = endpoint(&api, "combinedAuth");
        assert_eq!(combined.auth.len(), 1);
        assert_eq!(combined.auth[0].schemes.len(), 3);
    }

    #[test]
    fn preserves_scheme_identity_oauth_flow_and_scopes() {
        let api =
            crate::import_openapi_yaml_document(include_str!("../../../../examples/auth.yaml"))
                .expect("auth fixture should import");
        let combined = endpoint(&api, "combinedAuth");
        let names: Vec<&str> = combined.auth[0]
            .schemes
            .iter()
            .map(|requirement| requirement.scheme.name.as_str())
            .collect();
        assert!(names.contains(&"primaryKey"));
        assert!(names.contains(&"secondaryKey"));

        let machine = endpoint(&api, "machineAuth");
        let requirement = &machine.auth[0].schemes[0];
        assert_eq!(requirement.scopes, ["read"]);
        assert!(matches!(
            requirement.scheme.kind,
            AuthSchemeKind::OAuth2 {
                token_endpoint: Some(_)
            }
        ));

        let interactive = endpoint(&api, "interactiveAuth");
        assert!(matches!(
            interactive.auth[0].schemes[0].scheme.kind,
            AuthSchemeKind::OAuth2 {
                token_endpoint: None
            }
        ));

        let relative = endpoint(&api, "relativeMachineAuth");
        assert_eq!(
            relative.auth[0].schemes[0].scheme.kind,
            AuthSchemeKind::OAuth2 {
                token_endpoint: Some("/oauth/relative-token".to_string())
            }
        );
    }
}
