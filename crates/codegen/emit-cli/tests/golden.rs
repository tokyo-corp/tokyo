#![allow(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};

// The general protocol fixtures keep the CLI emitter aligned with the shared
// IR's complete HTTP surface, not only its resource-oriented command UX.
const FIXTURES: &[&str] = &[
    "petstore.yaml",
    "cli-coverage.yaml",
    "cli-types.yaml",
    "auth.yaml",
    "serialization.yaml",
    "http-runtime.yaml",
    "openapi-coverage.yaml",
];

#[test]
fn examples_match_cli_goldens() {
    for fixture in FIXTURES {
        assert_fixture(fixture);
    }
}

#[test]
fn route_extensions_and_legacy_custom_commands_are_wired() {
    let files = emit_fixture("petstore.yaml");
    let cli = generated_contents(&files, ".tokyo/src/cli.rs");
    let custom = generated_contents(&files, "src/commands/custom.rs");
    let main = generated_contents(&files, ".tokyo/src/main.rs");
    let middleware = generated_contents(&files, "src/middleware.rs");

    assert!(cli.contains("pub struct CommandContext<'a>"), "{cli}");
    assert!(cli.contains("crate::tokyo::routes::augment"), "{cli}");
    assert!(main.contains("mod middleware;"), "{main}");
    assert!(
        middleware.contains("pub fn decorate(route: Route) -> Route"),
        "{middleware}"
    );
    assert!(cli.contains("crate::commands::custom::augment"), "{cli}");
    assert!(cli.contains("execute_custom_command(&matches)"), "{cli}");
    assert!(
        cli.contains("crate::commands::custom::dispatch(matches, &context)"),
        "{cli}"
    );
    assert!(custom.contains("_matches: &clap::ArgMatches"), "{custom}");
    assert!(
        custom.contains("_context: &crate::cli::CommandContext<'_>"),
        "{custom}"
    );
}

#[test]
fn typed_primitives_render_for_serde_and_clap() {
    let files = emit_fixture("cli-types.yaml");
    let types = generated_contents(&files, ".tokyo/src/tokyo/types.rs");
    assert!(types.contains("pub id: uuid::Uuid"));
    assert!(types.contains("pub occurred_at: chrono::DateTime<chrono::Utc>"));
    assert!(types.contains("pub local_date: chrono::NaiveDate"));

    let commands = generated_contents(&files, ".tokyo/src/tokyo/commands/events.rs");
    assert!(commands.contains("event_id: uuid::Uuid"));
    assert!(commands.contains("since: Option<chrono::DateTime<chrono::Utc>>"));
    assert!(commands.contains("x_report_date: Option<chrono::NaiveDate>"));

    let manifest = generated_contents(&files, "Cargo.toml");
    assert!(manifest.contains(r#"chrono = { version = "0.4.42", features = ["serde"] }"#));
    assert!(manifest.contains(r#"uuid = { version = "1.18.1", features = ["serde"] }"#));
    assert!(manifest.contains(r#"tokyo-cli-runtime = "=0.1.4""#));
}

#[test]
fn schema_and_structured_body_modes_render_for_generated_commands() {
    let cli_coverage = emit_fixture("cli-coverage.yaml");
    let cli = generated_contents(&cli_coverage, ".tokyo/src/cli.rs");
    assert!(cli.contains(r#"\"aliases\": ["#));
    assert!(cli.contains(r#"\"list\""#));
    assert!(cli.contains(r#"\"invocation\": \"items ls\""#));
    assert!(cli.contains(r#"\"body_mode\": \"flattened_flags\""#));
    assert!(cli.contains(r#"\"schema_version\": 10"#));

    let serialization = emit_fixture("serialization.yaml");
    let commands = generated_contents(&serialization, ".tokyo/src/tokyo/commands/default.rs");
    assert!(commands.contains("long = \"body-json\""));
    assert!(commands.contains("short = 'f'"));
    assert!(
        commands
            .contains("tokyo_cli_runtime::body::parse_json_request_body_from_field_assignments")
    );

    let api_cli = generated_contents(&serialization, ".tokyo/src/cli.rs");
    assert!(api_cli.contains("body_json: Option<String>"));
    assert!(api_cli.contains("body_fields: Vec<String>"));
}

#[test]
fn named_environments_and_default_url_render_into_connection_profiles() {
    let fixture_path = workspace_root().join("examples/petstore.yaml");
    let source = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", fixture_path.display()));
    let mut api = tokyo_import_openapi::import_openapi_yaml_document(&source)
        .expect("petstore should import");
    api.cli.base_url = Some("https://api.example.test".to_string());
    api.cli.environments.insert(
        "Development".to_string(),
        "https://api.dev.example.test".to_string(),
    );
    api.cli
        .environments
        .insert("Local".to_string(), "http://localhost:8000".to_string());
    api.cli.environments.insert(
        "Production".to_string(),
        "https://api.example.test".to_string(),
    );

    let files = tokyo_emit_cli::emit_generated_cli_project_files(&api);
    let cli = generated_contents(&files, ".tokyo/src/cli.rs");
    let config = generated_contents(&files, ".tokyo/src/tokyo/config.rs");

    assert!(cli.contains("pub environment: Option<String>"));
    assert!(cli.contains("ProfileCommand"));
    assert!(cli.contains("EnvCommand"));
    assert!(config.contains(r#"default_base_url: Some("https://api.example.test")"#));
    assert!(config.contains(r#"("Development", "https://api.dev.example.test")"#));
    assert!(config.contains("tokyo_cli_runtime::RuntimeConfig"));
    assert!(cli.contains("\\\"connection\\\""));
    assert!(cli.contains("\\\"Production\\\": \\\"https://api.example.test\\\""));
}

#[test]
fn embedded_scenarios_render_into_config_program_and_schema() {
    let fixture_path = workspace_root().join("examples/petstore.yaml");
    let source = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", fixture_path.display()));
    let mut api = tokyo_import_openapi::import_openapi_yaml_document(&source)
        .expect("petstore should import");
    api.cli
        .cli_scenarios
        .push(tokyo_ir::cli_behavior::CliScenario {
            name: "smoke".to_string(),
            description: "Create and inspect a pet".to_string(),
            body: "pets list\n".to_string(),
            allowed_environments: vec!["Development".to_string()],
        });

    let files = tokyo_emit_cli::emit_generated_cli_project_files(&api);
    let cli = generated_contents(&files, ".tokyo/src/cli.rs");
    let config = generated_contents(&files, ".tokyo/src/tokyo/config.rs");

    assert!(config.contains(r#"name: "smoke""#), "{config}");
    assert!(config.contains(r#"body: "pets list\n""#), "{config}");
    assert!(config.contains(r#"allowed_environments: &["Development"]"#));
    assert!(cli.contains("target == \"list\""), "{cli}");
    assert!(
        cli.contains("resolve_scenario_set_variable_references_in_cli_arguments"),
        "{cli}"
    );
    assert!(
        cli.contains("resolve_authenticated_identity_references_in_cli_arguments"),
        "{cli}"
    );
    assert!(cli.contains("Command::Start"), "{cli}");
    assert!(cli.contains("args.push(\"start\".to_string())"), "{cli}");
    assert!(cli.contains("print_entry_help_banner"), "{cli}");
    assert!(cli.contains("Authenticated"), "{cli}");
    assert!(cli.contains("AGENT QUICKSTART"), "{cli}");
    assert!(cli.contains("Command::Achieve"), "{cli}");
    assert!(cli.contains("find_capability"), "{cli}");
    assert!(cli.contains("request_body"), "{cli}");
    assert!(
        cli.contains(r#".any(|arg| arg == "--help" || arg == "-h")"#),
        "{cli}"
    );
    assert!(cli.contains("relevant_resources"), "{cli}");
    assert!(cli.contains("command_error = Some(error)"), "{cli}");
    assert!(
        cli.contains("crate::oauth::default_oauth_scheme_name()"),
        "{cli}"
    );
    assert!(cli.contains("\\\"scenarios\\\""), "{cli}");
    assert!(cli.contains("\\\"Create and inspect a pet\\\""), "{cli}");
}

#[test]
fn interactive_oauth_provider_renders_through_the_oauth2_adapter() {
    let fixture_path = workspace_root().join("examples/auth.yaml");
    let source = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", fixture_path.display()));
    let mut api = tokyo_import_openapi::import_openapi_yaml_document(&source)
        .expect("auth fixture should import");
    api.cli.cli_auth.insert(
        "bearerAuth".to_string(),
        tokyo_ir::cli_behavior::CliAuthProvider {
            client_id: "public-cli-client".to_string(),
            redirect_uri: Some("http://127.0.0.1:49152/callback".to_string()),
            endpoints: tokyo_ir::cli_behavior::OAuthEndpoints::Discovery {
                issuer: "https://identity.example.test".to_string(),
            },
            scopes: vec!["openid".to_string(), "offline_access".to_string()],
            audience: Some("https://api.example.test".to_string()),
        },
    );

    let files = tokyo_emit_cli::emit_generated_cli_project_files(&api);
    let config = generated_contents(&files, ".tokyo/src/tokyo/config.rs");
    let cli = generated_contents(&files, ".tokyo/src/cli.rs");
    let commands = generated_contents(&files, ".tokyo/src/tokyo/commands/default.rs");
    let manifest = generated_contents(&files, "Cargo.toml");

    assert!(config.contains(r#"scheme: "bearerAuth""#));
    assert!(config.contains(r#"client_id: "public-cli-client""#));
    assert!(config.contains(r#"redirect_uri: Some("http://127.0.0.1:49152/callback")"#));
    assert!(config.contains("OAuthEndpoints::Discovery"));
    assert!(config.contains(r#"issuer: "https://identity.example.test""#));
    assert!(cli.contains("device: bool"));
    assert!(cli.contains("no_browser: bool"));
    assert!(cli.contains(r#"\"mode\": \"public\""#));
    assert!(cli.contains(r#"\"mode\": \"optional\""#));
    assert!(cli.contains(r#"\"mode\": \"authenticated\""#));
    assert!(cli.contains("render_cli_schema_index_for_access"));
    assert!(cli.contains("command_access_inventory"));
    assert!(commands.contains("[public]"));
    assert!(commands.contains("[authentication optional]"));
    assert!(commands.contains("[authentication required]"));
    assert!(manifest.contains(r#"tokyo-cli-runtime = "=0.1.4""#));
}

#[test]
fn browser_token_provider_renders_into_runtime_config_and_program() {
    let fixture_path = workspace_root().join("examples/auth.yaml");
    let source = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", fixture_path.display()));
    let mut api = tokyo_import_openapi::import_openapi_yaml_document(&source)
        .expect("auth fixture should import");
    api.cli.environments.insert(
        "Development".to_string(),
        "https://api.dev.example.test".to_string(),
    );
    api.cli
        .environments
        .insert("Local".to_string(), "http://localhost:8000".to_string());
    api.cli.cli_auth.insert(
        "bearerAuth".to_string(),
        tokyo_ir::cli_behavior::CliAuthProvider {
            client_id: String::new(),
            redirect_uri: None,
            endpoints: tokyo_ir::cli_behavior::OAuthEndpoints::BrowserToken {
                login_url: "https://app.example.test/login".to_string(),
                validation_url: "https://api.dev.example.test/me".to_string(),
                allowed_environments: vec!["Development".to_string(), "Local".to_string()],
                identity_fields: std::collections::BTreeMap::from([(
                    "org_type".to_string(),
                    "/caller/org_type".to_string(),
                )]),
            },
            scopes: Vec::new(),
            audience: None,
        },
    );

    let files = tokyo_emit_cli::emit_generated_cli_project_files(&api);
    let config = generated_contents(&files, ".tokyo/src/tokyo/config.rs");
    let cli = generated_contents(&files, ".tokyo/src/cli.rs");
    let readme = generated_contents(&files, "README.md");
    assert!(config.contains("OAuthEndpoints::BrowserToken"));
    assert!(config.contains(r#"validation_url: "https://api.dev.example.test/me""#));
    assert!(config.contains(r#"allowed_environments: &["Development", "Local"]"#));
    assert!(config.contains(r#"identity_fields: &[("org_type", "/caller/org_type")]"#));
    assert!(cli.contains("resolve_environment_name"));
    assert!(cli.contains("provider_acquisition_kind"));
    assert!(
        readme.contains("generated-cli --environment Development auth login --scheme bearerAuth")
    );
    assert!(readme.contains("generated-cli --environment Local auth login --scheme bearerAuth"));
    assert!(readme.contains("http://localhost:8000"));
}

#[test]
fn role_aware_dispatch_group_renders_additively_with_typed_member_decoding() {
    let source = r#"
openapi: 3.0.3
info: { title: Dispatch, version: 1.0.0 }
paths:
  /orders/provider/{order_id}:
    post:
      operationId: providerOrder
      tags: [orders]
      parameters:
        - { name: order_id, in: path, required: true, schema: { type: string } }
      responses:
        "200": { description: ok, content: { application/json: { schema: { type: object, properties: { provider: { type: string } } } } } }
  /orders/shipper/{order_id}:
    post:
      operationId: shipperOrder
      tags: [orders]
      parameters:
        - { name: order_id, in: path, required: true, schema: { type: string } }
      responses:
        "200": { description: ok, content: { application/json: { schema: { type: object, properties: { shipper: { type: string } } } } } }
"#;
    let mut api = tokyo_import_openapi::import_openapi_yaml_document(source)
        .expect("dispatch fixture imports");
    api.cli
        .cli_dispatch_groups
        .push(tokyo_ir::cli_behavior::CliDispatchGroup {
            resource: "orders".to_string(),
            name: "expanded".to_string(),
            description: Some("Role-aware expanded order".to_string()),
            default_member: "provider".to_string(),
            members: vec![
                tokyo_ir::cli_behavior::CliDispatchMember {
                    name: "provider".to_string(),
                    method: tokyo_ir::http::HttpMethod::Post,
                    path: "/orders/provider/{order_id}".to_string(),
                    identity: std::collections::BTreeMap::new(),
                    view: Some("provider".to_string()),
                },
                tokyo_ir::cli_behavior::CliDispatchMember {
                    name: "shipper".to_string(),
                    method: tokyo_ir::http::HttpMethod::Post,
                    path: "/orders/shipper/{order_id}".to_string(),
                    identity: std::collections::BTreeMap::from([(
                        "org_type".to_string(),
                        "shipper".to_string(),
                    )]),
                    view: Some("shipper".to_string()),
                },
            ],
        });

    let files = tokyo_emit_cli::emit_generated_cli_project_files(&api);
    let commands = generated_contents(&files, ".tokyo/src/tokyo/commands/orders.rs");
    let cli = generated_contents(&files, ".tokyo/src/cli.rs");
    assert!(commands.contains("ProviderOrder"));
    assert!(commands.contains("ShipperOrder"));
    assert!(commands.contains("Expanded"));
    assert!(commands.contains("value_parser = [\"provider\", \"shipper\"]"));
    assert!(commands.contains("value.get(\"org_type\")"));
    assert!(commands.contains("\"/orders/provider/{}\""));
    assert!(commands.contains("\"/orders/shipper/{}\""));
    assert!(commands.contains("crate::tokyo::types::ProviderOrderResponse200"));
    assert!(commands.contains("crate::tokyo::types::ShipperOrderResponse200"));
    assert!(cli.contains("crate::oauth::authenticated_caller_project_identity_from_token"));
    assert!(cli.contains("\\\"kind\\\": \\\"identity_dispatch\\\""));
    assert!(cli.contains("\\\"default_member\\\": \\\"provider\\\""));
}

#[test]
fn schema_v4_wire_metadata_is_rendered() {
    let source = r##"
openapi: 3.0.3
info: { title: Wire V4, version: 1.0.0 }
servers: [{ url: https://api.example.test }]
components:
  schemas:
    PartKind:
      type: string
      enum: [avatar]
  securitySchemes:
    queryKey: { type: apiKey, in: query, name: api_key }
    cookieKey: { type: apiKey, in: cookie, name: auth_cookie }
paths:
  /widgets/{coords}:
    get:
      operationId: getWidget
      tags: [widgets]
      security:
        - queryKey: []
          cookieKey: []
      parameters:
        - name: coords
          in: path
          required: true
          style: label
          explode: true
          schema: { type: array, items: { type: string } }
        - name: target
          in: query
          allowReserved: true
          schema: { type: string }
        - name: X-Flags
          in: header
          style: simple
          explode: false
          schema: { type: array, items: { type: string } }
        - name: prefs
          in: cookie
          style: form
          explode: false
          schema: { type: array, items: { type: string } }
      responses:
        "200":
          description: ok
          content:
            application/vnd.wire+json:
              schema: { type: string }
  /widgets:
    post:
      operationId: createWidget
      tags: [widgets]
      requestBody:
        required: true
        content:
          application/vnd.wire+json:
            schema:
              type: object
              required: [name]
              properties: { name: { type: string } }
      responses: { "204": { description: created } }
  /forms:
    post:
      operationId: submitForm
      tags: [widgets]
      requestBody:
        required: true
        content:
          application/x-www-form-urlencoded:
            schema:
              type: object
              required: [labels]
              properties:
                labels: { type: array, items: { type: string } }
            encoding:
              labels: { style: form, explode: false, allowReserved: true }
      responses: { "204": { description: accepted } }
  /uploads:
    post:
      operationId: upload
      tags: [widgets]
      requestBody:
        required: true
        content:
          multipart/form-data:
            schema:
              type: object
              required: [file]
              properties:
                file: { type: string, format: binary }
            encoding:
              file:
                contentType: image/png
                headers:
                  X-Part-Kind:
                    schema: { $ref: "#/components/schemas/PartKind" }
      responses: { "204": { description: uploaded } }
"##;
    let api = tokyo_import_openapi::import_openapi_yaml_document(source)
        .expect("v4 wire fixture should import");
    let files = tokyo_emit_cli::emit_generated_cli_project_files(&api);
    let commands = generated_contents(&files, ".tokyo/src/tokyo/commands/widgets.rs");

    assert!(commands.contains("ParameterStyle::LabelExplode"));
    assert!(commands.contains("ParameterStyle::Simple"));
    assert!(commands.contains("serialize_cookie_parameter"));
    assert!(commands.contains("allow_reserved: true"));
    assert!(commands.contains("AuthSchemeKind::QueryKey(\"api_key\")"));
    assert!(commands.contains("Some(\"application/vnd.wire+json\")"));
    assert!(commands.contains("content_type: Some(\"image/png\")"));
    assert!(commands.contains("\"X-Part-Kind\""));
    assert!(commands.contains("\"avatar\""));
}

#[test]
fn command_schema_detail_bakes_normalized_json_schemas_and_components() {
    let source = r##"
openapi: 3.1.0
info: { title: Schema Detail, version: 1.0.0 }
components:
  schemas:
    Widget: { type: object, properties: { id: { type: string } } }
paths:
  /widgets:
    post:
      operationId: createWidget
      tags: [widgets]
      requestBody:
        content:
          application/json:
            schema: { $ref: "#/components/schemas/Widget" }
      responses:
        "201":
          description: created
          content:
            application/json:
              schema: { $ref: "#/components/schemas/Widget" }
"##;
    let api = tokyo_import_openapi::import_openapi_yaml_document(source)
        .expect("schema fixture should import");
    let files = tokyo_emit_cli::emit_generated_cli_project_files(&api);
    let cli = generated_contents(&files, ".tokyo/src/cli.rs");

    assert!(cli.contains("json_schema: bool"));
    assert!(cli.contains("render_json_schema"));
    assert!(cli.contains("\\\"request_schema\\\""));
    assert!(cli.contains("\\\"response_schemas\\\""));
    assert!(cli.contains("\\\"#/components/schemas/Widget\\\""));
    assert!(cli.contains("\\\"components\\\""));
}

fn assert_fixture(fixture: &str) {
    let workspace = workspace_root();
    let generated_files = emit_fixture(fixture);

    let case_name = fixture.trim_end_matches(".yaml");
    let golden_dir = workspace
        .join("crates/codegen/emit-cli/tests/golden")
        .join(case_name);

    for generated in generated_files {
        let golden_path = golden_dir.join(&generated.relative_path);
        if tokyo_emit_cli::UNMANAGED_STARTER_FILES.contains(&generated.relative_path.as_str())
            && golden_path.exists()
        {
            continue;
        }
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            fs::create_dir_all(
                golden_path
                    .parent()
                    .expect("golden output should have a parent directory"),
            )
            .expect("golden directory should be writable");
            fs::write(&golden_path, &generated.contents).unwrap_or_else(|error| {
                panic!("failed to write {}: {error}", golden_path.display())
            });
            continue;
        }

        let expected = fs::read_to_string(&golden_path).unwrap_or_else(|error| {
            panic!(
                "failed to read {}: {error}; regenerate with UPDATE_GOLDENS=1 cargo test -p tokyo-emit-cli --test golden",
                golden_path.display()
            )
        });
        assert_eq!(
            generated.contents,
            expected,
            "generated output differs for {}",
            golden_path.display()
        );
    }
}

#[test]
fn project_skills_are_unmanaged_starters() {
    let skills = tokyo_emit_cli::project_skill_starter_files();
    assert_eq!(skills.len(), 14);
    for skill in skills {
        assert!(
            tokyo_emit_cli::UNMANAGED_STARTER_FILES.contains(&skill.relative_path.as_str()),
            "{} must remain scaffold-once",
            skill.relative_path
        );
        assert!(skill.relative_path.starts_with(".skills/tokyo-"));
        assert!(skill.relative_path.ends_with("/SKILL.md"));
        assert!(skill.contents.starts_with("---\nname: tokyo-"));
        assert!(!skill.contents.contains("disable-model-invocation"));
    }
}

fn emit_fixture(fixture: &str) -> Vec<tokyo_emit_cli::GeneratedFile> {
    let fixture_path = workspace_root().join("examples").join(fixture);
    let source = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", fixture_path.display()));
    let mut api = tokyo_import_openapi::import_openapi_yaml_document(&source)
        .unwrap_or_else(|error| panic!("failed to import {}: {error}", fixture_path.display()));
    if fixture == "auth.yaml" {
        api.cli.cli_auth.insert(
            "bearerAuth".to_string(),
            tokyo_ir::cli_behavior::CliAuthProvider {
                client_id: "golden-public-client".to_string(),
                redirect_uri: None,
                endpoints: tokyo_ir::cli_behavior::OAuthEndpoints::Explicit {
                    authorization_url: Some("https://identity.example.test/authorize".to_string()),
                    token_url: "https://identity.example.test/token".to_string(),
                    device_authorization_url: Some(
                        "https://identity.example.test/device".to_string(),
                    ),
                },
                scopes: vec!["openid".to_string(), "offline_access".to_string()],
                audience: Some("https://api.example.test".to_string()),
            },
        );
    }
    tokyo_emit_cli::emit_generated_cli_project_files(&api)
}

fn generated_contents<'a>(
    files: &'a [tokyo_emit_cli::GeneratedFile],
    relative_path: &str,
) -> &'a str {
    &files
        .iter()
        .find(|file| file.relative_path == relative_path)
        .unwrap_or_else(|| panic!("missing generated file {relative_path}"))
        .contents
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("crate lives at crates/codegen/emit-cli")
        .to_path_buf()
}
