#![allow(missing_docs)]

use std::ffi::OsString;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

#[test]
#[ignore = "runs nested Cargo commands; invoke explicitly as the generated CLI compile gate"]
fn checked_in_golden_clis_compile_and_report_schema() {
    let workspace = workspace_root();
    let golden_root = workspace.join("crates/codegen/emit-cli/tests/golden");
    let target_dir = workspace.join("target");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let manifests = golden_manifests(&golden_root);

    assert!(
        !manifests.is_empty(),
        "no generated CLI fixture crates found under {}",
        golden_root.display()
    );

    // All fixtures share one target directory so their identical dependency
    // graphs compile once. They intentionally use the same generated
    // package/binary name, so run them serially to prevent one fixture from
    // replacing another fixture's executable while it is still being tested.
    // `run_fixture` cleans only that package between fixtures so Cargo cannot
    // mistake the previous fixture's same-named binary for a current artifact.
    for manifest in manifests {
        run_fixture(&cargo, &workspace, &manifest, &target_dir);
    }
}

fn run_fixture(cargo: &OsString, workspace: &Path, manifest: &Path, target_dir: &Path) {
    let fixture = manifest
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("<unknown>");
    let clean = cargo_command(cargo, workspace)
        .args(["clean", "--manifest-path"])
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .args(["-p", "generated-cli"])
        .output()
        .unwrap_or_else(|error| panic!("failed to clean generated CLI for {fixture}: {error}"));
    assert_success(fixture, "cargo clean", &clean);

    if fixture == "auth" {
        let tests = cargo_command(cargo, workspace)
            .args(["test", "--quiet", "--locked", "--manifest-path"])
            .arg(manifest)
            .arg("--target-dir")
            .arg(target_dir)
            .output()
            .unwrap_or_else(|error| {
                panic!("failed to start generated OAuth tests for {fixture}: {error}")
            });
        assert_success(fixture, "generated OAuth tests", &tests);

        let doctor = cargo_command(cargo, workspace)
            .args(["run", "--quiet", "--locked", "--manifest-path"])
            .arg(manifest)
            .arg("--target-dir")
            .arg(target_dir)
            .args([
                "--",
                "--output",
                "json",
                "auth",
                "doctor",
                "--scheme",
                "bearerAuth",
            ])
            .output()
            .unwrap_or_else(|error| {
                panic!("failed to start generated OAuth doctor for {fixture}: {error}")
            });
        assert_success(fixture, "generated OAuth doctor", &doctor);
        let report: serde_json::Value =
            serde_json::from_slice(&doctor.stdout).expect("OAuth doctor emits JSON");
        assert_eq!(report["healthy"], true, "{report}");
        assert_authentication_access_contract(cargo, manifest, target_dir);
    }

    let schema = cargo_command(cargo, workspace)
        .args(["run", "--quiet", "--locked", "--manifest-path"])
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .args(["--", "schema"])
        .output()
        .unwrap_or_else(|error| {
            panic!("failed to start generated schema command for {fixture}: {error}")
        });
    assert_success(fixture, "generated CLI schema command", &schema);
    assert_schema(fixture, &schema.stdout);

    if fixture == "cli-types" {
        assert_typed_primitive_commands(cargo, manifest, target_dir);
    }
    if fixture == "petstore" {
        assert_connection_profiles(cargo, manifest, target_dir);
        assert_custom_command(cargo, manifest, target_dir);
        assert_agent_navigation_contract(cargo, manifest, target_dir);
        assert_presentation_control(cargo, manifest, target_dir);
    }
    match fixture {
        "serialization" => assert_parameter_and_multipart_wire(cargo, manifest, target_dir),
        "openapi-coverage" => assert_form_stream_and_binary_wire(cargo, manifest, target_dir),
        "http-runtime" => assert_response_encodings(cargo, manifest, target_dir),
        _ => {}
    }
}

fn assert_authentication_access_contract(cargo: &OsString, manifest: &Path, target_dir: &Path) {
    let public_schema = run_generated(
        cargo,
        manifest,
        target_dir,
        &["schema", "--access", "public"],
    );
    assert_success("auth", "public schema filter", &public_schema);
    let public_schema: serde_json::Value =
        serde_json::from_slice(&public_schema.stdout).expect("public schema emits JSON");
    assert_eq!(public_schema["access_filter"], "public");
    let commands = public_schema["resources"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|resource| resource["commands"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    assert_eq!(commands.len(), 1, "{public_schema}");
    assert_eq!(commands[0]["authentication"]["mode"], "public");

    let start = run_generated(cargo, manifest, target_dir, &["--output", "json", "start"]);
    assert_success("auth", "unauthenticated access orientation", &start);
    let start: serde_json::Value = serde_json::from_slice(&start.stdout).expect("start emits JSON");
    assert!(
        start["available_now"]
            .as_array()
            .is_some_and(|commands| !commands.is_empty()),
        "{start}"
    );
    assert!(
        start["authentication_required"]
            .as_array()
            .is_some_and(|commands| !commands.is_empty()),
        "{start}"
    );

    let protected = run_generated(
        cargo,
        manifest,
        target_dir,
        &[
            "--base-url",
            "http://127.0.0.1:9",
            "--output",
            "json",
            "--no-input",
            "default",
            "inherited-auth",
        ],
    );
    assert!(
        !protected.status.success(),
        "protected command unexpectedly ran"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&protected.stderr).expect("auth failure emits one JSON envelope");
    assert_eq!(
        report["error"]["code"], "authentication_required",
        "{report}"
    );
    assert_eq!(
        report["error"]["authentication"]["alternatives"][0]["schemes"][0]["name"], "primaryKey",
        "{report}"
    );
}

fn assert_custom_command(cargo: &OsString, manifest: &Path, target_dir: &Path) {
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &["--output", "json", "hello", "--name", "Tokyo"],
    );
    assert_success("petstore", "custom command", &output);
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("custom command emits JSON");
    assert_eq!(value["greeting"], "Hello, Tokyo!");
    assert_eq!(value["profile"], "default");
}

/// Phase 3 agent navigation contract: a cold agent must reach an executable
/// invocation from `start` plus at most one schema lookup, and discovery
/// responses must stay compact enough to load in one round trip.
fn assert_agent_navigation_contract(cargo: &OsString, manifest: &Path, target_dir: &Path) {
    const SCHEMA_INDEX_BUDGET_BYTES: usize = 64 * 1024;
    const COMMAND_DETAIL_BUDGET_BYTES: usize = 32 * 1024;

    let config_home = target_dir.join("navigation-test-config");
    let _ = fs::remove_dir_all(&config_home);

    let start = run_generated_with_config(
        cargo,
        manifest,
        target_dir,
        &config_home,
        &["--output", "json", "start"],
    );
    assert_success("petstore", "cold start", &start);
    let start: serde_json::Value = serde_json::from_slice(&start.stdout).expect("start emits JSON");
    assert_eq!(start["authenticated"], false, "{start}");
    let next_steps = start["next_steps"]
        .as_array()
        .expect("start emits next_steps");
    assert!(!next_steps.is_empty(), "cold start offered no next steps");

    // Developer-owned guidance is carried in responses the agent already
    // loads: the CLI-level opinion in `start`, the per-command note in
    // `schema --command` detail.
    assert!(
        start["guidance"]
            .as_str()
            .is_some_and(|note| note.contains("achieve")),
        "start omitted the developer's cli_guidance: {start}"
    );
    let guided = run_generated_with_config(
        cargo,
        manifest,
        target_dir,
        &config_home,
        &["schema", "--command", "default.create-pet"],
    );
    assert_success("petstore", "guided command detail", &guided);
    let guided: serde_json::Value =
        serde_json::from_slice(&guided.stdout).expect("command detail emits JSON");
    assert_eq!(guided["command"], "default.create-pet", "{guided}");
    assert!(
        guided["guidance"]
            .as_str()
            .is_some_and(|note| note.contains("achieve create pet")),
        "command detail omitted the developer's note: {guided}"
    );

    // Every emitted next step must resolve to a real command: run its
    // command path (tokens before any placeholder or flag) with --help.
    for step in next_steps {
        let step = step.as_str().expect("next step is a string");
        let command_path: Vec<&str> = step
            .split_whitespace()
            .skip(1)
            .take_while(|token| !token.starts_with('-') && !token.contains('<'))
            .collect();
        assert!(
            !command_path.is_empty(),
            "next step {step:?} has no command path"
        );
        let mut args = command_path.clone();
        args.push("--help");
        let help = run_generated_with_config(cargo, manifest, target_dir, &config_home, &args);
        assert!(
            help.status.success(),
            "next step {step:?} does not resolve to a command:\n{}",
            String::from_utf8_lossy(&help.stderr)
        );
    }

    // The schema index must stay compact: no request/response schema bodies,
    // and a fixed size budget.
    let index = run_generated_with_config(cargo, manifest, target_dir, &config_home, &["schema"]);
    assert_success("petstore", "schema index", &index);
    assert!(
        index.stdout.len() < SCHEMA_INDEX_BUDGET_BYTES,
        "schema index is {} bytes, over the {SCHEMA_INDEX_BUDGET_BYTES}-byte budget",
        index.stdout.len()
    );
    let index: serde_json::Value =
        serde_json::from_slice(&index.stdout).expect("schema index emits JSON");
    let index_text = index.to_string();
    for heavy_key in [
        "\"request_schema\"",
        "\"response_schemas\"",
        "\"components\"",
    ] {
        assert!(
            !index_text.contains(heavy_key),
            "schema index leaks {heavy_key} bodies into normal discovery"
        );
    }

    // One exact lookup returns the full command contract, within budget.
    let command_id = index["resources"]
        .as_array()
        .and_then(|resources| resources.first())
        .and_then(|resource| {
            let resource_name = resource["name"].as_str()?;
            let command_name = resource["commands"].as_array()?.first()?["name"].as_str()?;
            Some(format!("{resource_name}.{command_name}"))
        })
        .expect("schema index lists at least one command");
    let detail = run_generated_with_config(
        cargo,
        manifest,
        target_dir,
        &config_home,
        &["schema", "--command", &command_id],
    );
    assert_success("petstore", "schema command detail", &detail);
    assert!(
        detail.stdout.len() < COMMAND_DETAIL_BUDGET_BYTES,
        "command detail for {command_id} is {} bytes, over the {COMMAND_DETAIL_BUDGET_BYTES}-byte budget",
        detail.stdout.len()
    );
    let detail: serde_json::Value =
        serde_json::from_slice(&detail.stdout).expect("command detail emits JSON");
    assert_eq!(detail["command"], command_id.as_str(), "{detail}");
    assert!(
        detail["scripting"]["direct"].as_str().is_some(),
        "command detail lacks a scripting recipe: {detail}"
    );

    // Failures carry a machine-actionable envelope: retryable plus a concrete
    // recovery hint, so a script can branch without a model round trip.
    let failure = run_generated_with_config(
        cargo,
        manifest,
        target_dir,
        &config_home,
        &["--output", "json", "default", "list-pets"],
    );
    assert!(
        !failure.status.success(),
        "list-pets without configuration should fail"
    );
    let failure_line = String::from_utf8_lossy(&failure.stderr);
    let envelope: serde_json::Value =
        serde_json::from_str(failure_line.trim()).unwrap_or_else(|error| {
            panic!("stderr is not a JSON error envelope: {error}\n{failure_line}")
        });
    assert_eq!(envelope["error"]["retryable"], false, "{envelope}");
    assert!(
        envelope["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("--base-url")),
        "missing-config failure lacks an actionable hint: {envelope}"
    );

    // The complete schema graph stays available behind --json-schema.
    let json_schema = run_generated_with_config(
        cargo,
        manifest,
        target_dir,
        &config_home,
        &["schema", "--command", &command_id, "--json-schema"],
    );
    assert_success("petstore", "schema --json-schema", &json_schema);
    let json_schema: serde_json::Value =
        serde_json::from_slice(&json_schema.stdout).expect("--json-schema emits JSON");
    assert!(
        json_schema.get("components").is_some(),
        "--json-schema omitted the component graph"
    );
}

/// The user-owned `src/presentation.rs` receives the complete clap tree, so
/// local presentation edits must be visible in help output and survive
/// regeneration (the file is an unmanaged starter).
fn assert_presentation_control(cargo: &OsString, manifest: &Path, target_dir: &Path) {
    let help = run_generated(cargo, manifest, target_dir, &["--help"]);
    assert_success("petstore", "root --help", &help);
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(
        help_text.contains("styled by src/presentation.rs"),
        "user presentation did not reach --help output:\n{help_text}"
    );
}

fn assert_connection_profiles(cargo: &OsString, manifest: &Path, target_dir: &Path) {
    let config_home = target_dir.join("profile-test-config");
    let _ = fs::remove_dir_all(&config_home);

    let set = run_generated_with_config(
        cargo,
        manifest,
        target_dir,
        &config_home,
        &[
            "--output",
            "json",
            "--profile",
            "staging",
            "profile",
            "set",
            "--base-url",
            "https://staging.example.test",
        ],
    );
    assert_success("petstore", "profile set", &set);

    let show = run_generated_with_config(
        cargo,
        manifest,
        target_dir,
        &config_home,
        &[
            "--output",
            "json",
            "--profile",
            "staging",
            "profile",
            "show",
        ],
    );
    assert_success("petstore", "profile show", &show);
    let value: serde_json::Value =
        serde_json::from_slice(&show.stdout).expect("profile show should emit JSON");
    assert_eq!(value["name"], "staging");
    assert_eq!(value["resolved_base_url"], "https://staging.example.test");

    let list = run_generated_with_config(
        cargo,
        manifest,
        target_dir,
        &config_home,
        &["--output", "json", "profile", "list"],
    );
    assert_success("petstore", "profile list", &list);
    let value: serde_json::Value =
        serde_json::from_slice(&list.stdout).expect("profile list should emit JSON");
    assert!(
        value
            .as_array()
            .expect("profile list should be an array")
            .iter()
            .any(|profile| profile["name"] == "staging")
    );
}

fn assert_typed_primitive_commands(cargo: &OsString, manifest: &Path, target_dir: &Path) {
    let valid_id = "550e8400-e29b-41d4-a716-446655440000";
    let valid_datetime = "2026-07-13T12:34:56Z";
    let valid_date = "2026-07-13";

    let parsed = run_generated(
        cargo,
        manifest,
        target_dir,
        &[
            "--base-url",
            "http://127.0.0.1:9",
            "events",
            "get-event",
            valid_id,
            "--since",
            valid_datetime,
            "--X-Report-Date",
            valid_date,
        ],
    );
    assert_eq!(
        parsed.status.code(),
        Some(1),
        "valid typed path/query/header values should parse before the intentional transport failure\nstderr:\n{}",
        String::from_utf8_lossy(&parsed.stderr)
    );

    let serialized = run_generated(
        cargo,
        manifest,
        target_dir,
        &[
            "--base-url",
            "http://127.0.0.1:9",
            "events",
            "create-event",
            "--id",
            valid_id,
            "--occurredAt",
            valid_datetime,
            "--localDate",
            valid_date,
        ],
    );
    assert_eq!(
        serialized.status.code(),
        Some(1),
        "typed object values should parse and serialize before the intentional transport failure\nstderr:\n{}",
        String::from_utf8_lossy(&serialized.stderr)
    );

    let rejected = run_generated(
        cargo,
        manifest,
        target_dir,
        &[
            "--base-url",
            "http://127.0.0.1:9",
            "events",
            "get-event",
            "not-a-uuid",
        ],
    );
    assert_eq!(
        rejected.status.code(),
        Some(2),
        "an invalid UUID should be rejected by clap\nstderr:\n{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

fn assert_parameter_and_multipart_wire(cargo: &OsString, manifest: &Path, target_dir: &Path) {
    let (base_url, capture) = serve_once(204, "text/plain", Vec::new());
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &[
            "--base-url",
            &base_url,
            "default",
            "search",
            "--csv",
            "a",
            "--csv",
            "b",
            "--piped",
            "x",
            "--piped",
            "y",
            "--spaced",
            "m",
            "--spaced",
            "n",
        ],
    );
    assert_success("serialization", "parameter serialization request", &output);
    let request = capture.join().expect("query capture server should finish");
    let request_line = request_text(&request)
        .lines()
        .next()
        .expect("captured request has a request line")
        .to_string();
    assert!(request_line.contains("csv=a%2Cb"), "{request_line}");
    assert!(request_line.contains("piped=x%7Cy"), "{request_line}");
    assert!(request_line.contains("spaced=m%20n"), "{request_line}");

    let upload_bytes = b"\0tokyo\xff";
    let upload = temporary_file("multipart-upload.bin", upload_bytes);
    let body = temporary_file(
        "multipart-body.json",
        serde_json::to_string(&serde_json::json!({
            "display-name": "wire",
            "file": upload,
            "tags": ["one", "two"],
            "metadata": { "source-name": "compile-gate" }
        }))
        .expect("multipart fixture serializes")
        .as_bytes(),
    );
    let (base_url, capture) = serve_once(204, "text/plain", Vec::new());
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &[
            "--base-url",
            &base_url,
            "default",
            "upload",
            "--body",
            body.to_str().expect("temporary path is UTF-8"),
        ],
    );
    assert_success("serialization", "multipart request", &output);
    let request = capture
        .join()
        .expect("multipart capture server should finish");
    let text = request_text(&request);
    assert!(text.contains("Content-Type: multipart/form-data; boundary="));
    assert!(text.contains("name=\"display-name\""));
    assert!(text.contains("name=\"file\"; filename=\""));
    assert!(text.contains("multipart-upload.bin\""));
    assert!(text.contains("name=\"tags\""));
    assert!(
        request
            .windows(upload_bytes.len())
            .any(|window| window == upload_bytes)
    );

    let (base_url, capture) = serve_once(204, "text/plain", Vec::new());
    let upload_path = upload.to_str().expect("temporary path is UTF-8");
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &[
            "--base-url",
            &base_url,
            "default",
            "upload",
            "--field",
            "display-name=fields",
            "-f",
            &format!("file={upload_path}"),
            "-f",
            "tags=[\"one\",\"two\"]",
            "-f",
            "metadata.source-name=compile-gate",
        ],
    );
    assert_success("serialization", "multipart field request", &output);
    let request = capture
        .join()
        .expect("multipart field capture server should finish");
    let text = request_text(&request);
    assert!(text.contains("name=\"display-name\""));
    assert!(text.contains("fields"));
    assert!(text.contains("name=\"metadata\""));
}

fn assert_form_stream_and_binary_wire(cargo: &OsString, manifest: &Path, target_dir: &Path) {
    let (base_url, capture) = serve_once(
        200,
        "application/json",
        br#"{"items":[],"nextCursor":null}"#.to_vec(),
    );
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &[
            "--base-url",
            &base_url,
            "items",
            "list-items",
            "--cursor",
            "next",
            "--filter",
            r#"{"team":"sdk"}"#,
        ],
    );
    assert_success("openapi-coverage", "deep-object query request", &output);
    let request = capture
        .join()
        .expect("deep-object capture server should finish");
    let request_line = request_text(&request)
        .lines()
        .next()
        .expect("captured request has a request line")
        .to_string();
    assert!(request_line.contains("cursor=next"), "{request_line}");
    assert!(
        request_line.contains("filter%5Bteam%5D=sdk"),
        "{request_line}"
    );

    let form = temporary_file(
        "form-body.json",
        br#"{"name":"garden","tags":["one","two"]}"#,
    );
    let (base_url, capture) = serve_once(204, "text/plain", Vec::new());
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &[
            "--base-url",
            &base_url,
            "items",
            "submit-form",
            "--body",
            form.to_str().expect("temporary path is UTF-8"),
        ],
    );
    assert_success("openapi-coverage", "URL-encoded form request", &output);
    let request = capture.join().expect("form capture server should finish");
    let text = request_text(&request);
    assert!(text.contains("Content-Type: application/x-www-form-urlencoded"));
    assert!(text.contains("name=garden"));
    assert!(text.contains("tags=one"));
    assert!(text.contains("tags=two"));

    let sse = b"data: {\"id\":\"one\"}\r\n\r\ndata: {\"id\":\"two\"}\r\n\r\ndata: [DONE]\r\n\r\n";
    let (base_url, capture) = serve_once(200, "text/event-stream", sse.to_vec());
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &["--base-url", &base_url, "events", "watch-events"],
    );
    assert_success("openapi-coverage", "SSE request", &output);
    capture.join().expect("SSE server should finish");
    assert_eq!(
        String::from_utf8(output.stdout).expect("stream output is UTF-8"),
        "{\"id\":\"one\",\"payload\":null}\n{\"id\":\"two\",\"payload\":null}\n"
    );

    let ndjson = b"{\"id\":\"one\"}\n{\"id\":\"two\"}\n";
    let (base_url, capture) = serve_once(200, "application/x-ndjson", ndjson.to_vec());
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &["--base-url", &base_url, "events", "stream-records"],
    );
    assert_success("openapi-coverage", "NDJSON request", &output);
    capture.join().expect("NDJSON server should finish");
    assert_eq!(
        String::from_utf8(output.stdout).expect("stream output is UTF-8"),
        "{\"id\":\"one\",\"payload\":null}\n{\"id\":\"two\",\"payload\":null}\n"
    );

    let binary = temporary_file("presigned-upload.bin", b"\0\x01\xfe\xff");
    let (base_url, capture) = serve_once(204, "text/plain", Vec::new());
    let upload_url = format!("{base_url}/upload?signature=a%2Bb");
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &[
            "events",
            "upload-presigned",
            &upload_url,
            "--body",
            binary.to_str().expect("temporary path is UTF-8"),
        ],
    );
    assert_success("openapi-coverage", "absolute binary upload", &output);
    let request = capture.join().expect("binary capture server should finish");
    let (headers, body) = split_request(&request);
    assert!(request_text(headers).starts_with("PUT /upload?signature=a%2Bb HTTP/1.1"));
    assert!(request_text(headers).contains("Content-Type: application/octet-stream"));
    assert_eq!(body, b"\0\x01\xfe\xff");
}

fn assert_response_encodings(cargo: &OsString, manifest: &Path, target_dir: &Path) {
    let expected = b"\0\x01tokyo\xfe\xff".to_vec();
    let (base_url, capture) = serve_once(200, "application/octet-stream", expected.clone());
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &["--base-url", &base_url, "default", "download"],
    );
    assert_success("http-runtime", "binary download", &output);
    capture
        .join()
        .expect("binary response server should finish");
    assert_eq!(output.stdout, expected);

    let (base_url, capture) = serve_once(202, "text/plain", b"accepted".to_vec());
    let output = run_generated(
        cargo,
        manifest,
        target_dir,
        &["--base-url", &base_url, "default", "get-mixed"],
    );
    assert_success("http-runtime", "text response", &output);
    capture.join().expect("text response server should finish");
    assert_eq!(output.stdout, b"accepted\n");
}

fn serve_once(
    status: u16,
    content_type: &'static str,
    response_body: Vec<u8>,
) -> (String, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("capture server should bind");
    listener
        .set_nonblocking(true)
        .expect("capture server should be nonblocking");
    let address = listener
        .local_addr()
        .expect("capture server has an address");
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "generated CLI did not reach capture server"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("capture server accept failed: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("capture stream timeout should configure");
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 8192];
            let read = stream
                .read(&mut chunk)
                .unwrap_or_else(|error| panic!("capture server failed reading request: {error}"));
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if let Some((headers, body)) = split_request_checked(&request) {
                let content_length = request_text(headers)
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if body.len() >= content_length {
                    break;
                }
            }
        }
        let reason = if (200..300).contains(&status) {
            "OK"
        } else {
            "Error"
        };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response_body.len()
        )
        .expect("capture response headers should write");
        stream
            .write_all(&response_body)
            .and_then(|()| stream.flush())
            .expect("capture response body should write");
        request
    });
    (format!("http://{address}"), handle)
}

fn split_request(request: &[u8]) -> (&[u8], &[u8]) {
    split_request_checked(request).expect("captured request should contain headers")
}

fn split_request_checked(request: &[u8]) -> Option<(&[u8], &[u8])> {
    let index = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?;
    Some((&request[..index], &request[index + 4..]))
}

fn request_text(request: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(request)
}

fn temporary_file(name: &str, contents: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("tokyo-{}-{name}", std::process::id()));
    fs::write(&path, contents)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
    path
}

fn run_generated(cargo: &OsString, manifest: &Path, target_dir: &Path, args: &[&str]) -> Output {
    cargo_command(cargo, &workspace_root())
        .args(["run", "--quiet", "--locked", "--manifest-path"])
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--")
        .args(args)
        .output()
        .expect("failed to run generated typed-primitives CLI")
}

fn run_generated_with_config(
    cargo: &OsString,
    manifest: &Path,
    target_dir: &Path,
    config_home: &Path,
    args: &[&str],
) -> Output {
    cargo_command(cargo, &workspace_root())
        .args(["run", "--quiet", "--locked", "--manifest-path"])
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .env("XDG_CONFIG_HOME", config_home)
        .arg("--")
        .args(args)
        .output()
        .expect("failed to run generated connection-profile CLI")
}

fn cargo_command(cargo: &OsString, workspace: &Path) -> Command {
    let runtime = workspace.join("crates/cli-runtime");
    let mut command = Command::new(cargo);
    command.arg("--config").arg(format!(
        "patch.crates-io.tokyo-cli-runtime.path={:?}",
        runtime
    ));
    command
}

fn golden_manifests(golden_root: &Path) -> Vec<PathBuf> {
    let mut manifests: Vec<_> = fs::read_dir(golden_root)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", golden_root.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| {
                    panic!("failed to inspect {} entry: {error}", golden_root.display())
                })
                .path()
                .join("Cargo.toml")
        })
        .filter(|manifest| manifest.is_file())
        .collect();
    manifests.sort();
    manifests
}

fn assert_success(fixture: &str, action: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{action} failed for {fixture} with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_schema(fixture: &str, stdout: &[u8]) {
    let schema: serde_json::Value = serde_json::from_slice(stdout).unwrap_or_else(|error| {
        panic!(
            "schema command for {fixture} did not emit valid JSON: {error}\nstdout:\n{}",
            String::from_utf8_lossy(stdout)
        )
    });

    assert_eq!(
        schema.get("name").and_then(serde_json::Value::as_str),
        Some("generated-cli"),
        "schema command for {fixture} emitted an unexpected CLI name"
    );
    for array_key in ["resources", "global_flags", "output_formats"] {
        assert!(
            schema
                .get(array_key)
                .is_some_and(serde_json::Value::is_array),
            "schema command for {fixture} omitted array field {array_key:?}"
        );
    }
    assert!(
        schema
            .get("escape_hatch")
            .is_some_and(serde_json::Value::is_object),
        "schema command for {fixture} omitted object field \"escape_hatch\""
    );
    assert!(
        schema
            .get("exit_codes")
            .is_some_and(serde_json::Value::is_object),
        "schema command for {fixture} omitted object field \"exit_codes\""
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("crate lives at crates/codegen/emit-cli")
        .to_path_buf()
}
