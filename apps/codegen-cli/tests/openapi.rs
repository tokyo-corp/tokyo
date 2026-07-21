#![allow(missing_docs)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

const SPEC_A: &str = r#"
openapi: 3.0.3
info:
  version: 1.0.0
  title: Pets
paths:
  /pets:
    get:
      operationId: listPets
      tags: [pets]
      responses:
        "200": { description: ok }
"#;

const SPEC_B: &str = r#"{"paths":{"/pets":{"get":{"responses":{"200":{"description":"ok"}},"tags":["pets"],"operationId":"listPets"}}},"info":{"title":"Pets","version":"2.0.0"},"openapi":"3.0.3"}"#;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "tokyo-openapi-{name}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tokyo(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tokyo"))
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap()
}

fn project(temp: &TempDir) -> PathBuf {
    let project = temp.0.join("project");
    let output = tokyo(
        &temp.0,
        &["init", project.to_str().unwrap(), "--name", "test-cli"],
    );
    assert!(output.status.success(), "{output:?}");
    project
}

fn read_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn serve(status: &str, content_type: &str, body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let status = status.to_string();
    let content_type = content_type.to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    });
    format!("http://{address}/openapi")
}

#[test]
fn local_add_is_canonical_deterministic_and_generates_with_routes() {
    let temp = TempDir::new("local");
    let project = project(&temp);
    fs::write(project.join("source.yaml"), SPEC_A).unwrap();
    fs::write(
        project.join("src/routes/local.rs"),
        "use tokyo_cli_runtime::prelude::*;\npub fn route() -> Route { Route::new(RouteSpec::new(\"local\"), |_| Ok(RouteResponse::text(\"ok\"))) }\n",
    )
    .unwrap();

    let added = tokyo(&project, &["openapi", "add", "source.yaml"]);
    assert!(added.status.success(), "{added:?}");
    let snapshot = fs::read(project.join("openapi/upstream.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
    assert_eq!(parsed["info"]["title"], "Pets");
    assert_eq!(snapshot.last(), Some(&b'\n'));
    let config = fs::read_to_string(project.join("tokyo.toml")).unwrap();
    assert!(config.contains("source = \"source.yaml\""), "{config}");
    assert!(
        config.contains("snapshot = \"openapi/upstream.json\""),
        "{config}"
    );
    let lock = fs::read_to_string(project.join("tokyo.lock")).unwrap();
    assert!(lock.contains("format_version = 1"), "{lock}");
    assert!(lock.contains("generator_version = \"0.1.4\""), "{lock}");
    assert!(lock.contains("sha256 = \""), "{lock}");
    assert!(!lock.contains("time"), "{lock}");

    let before = read_tree(&project);
    let added_again = tokyo(&project, &["openapi", "add", "source.yaml"]);
    assert!(added_again.status.success(), "{added_again:?}");
    assert_eq!(read_tree(&project), before);

    fs::remove_file(project.join("source.yaml")).unwrap();
    let generated = tokyo(&project, &["generate"]);
    assert!(generated.status.success(), "{generated:?}");
    let registry = fs::read_to_string(project.join(".tokyo/src/tokyo/routes.rs")).unwrap();
    assert!(registry.contains("local.rs"), "{registry}");
    let commands = fs::read_to_string(project.join(".tokyo/src/tokyo/commands/mod.rs")).unwrap();
    assert!(commands.contains("pets"), "{commands}");
}

#[test]
fn sync_updates_drift_and_check_never_writes() {
    let temp = TempDir::new("drift");
    let project = project(&temp);
    fs::write(project.join("source.yaml"), SPEC_A).unwrap();
    assert!(
        tokyo(&project, &["openapi", "add", "source.yaml"])
            .status
            .success()
    );
    fs::write(project.join("source.yaml"), SPEC_B).unwrap();

    let before = read_tree(&project);
    let checked = tokyo(&project, &["openapi", "check"]);
    assert_eq!(checked.status.code(), Some(1), "{checked:?}");
    assert!(
        String::from_utf8_lossy(&checked.stderr).contains("differs"),
        "{checked:?}"
    );
    assert_eq!(read_tree(&project), before, "check must perform no writes");

    let synced = tokyo(&project, &["openapi", "sync"]);
    assert!(synced.status.success(), "{synced:?}");
    assert_ne!(read_tree(&project), before);
    assert!(tokyo(&project, &["openapi", "check"]).status.success());

    fs::write(project.join("tokyo.lock"), "tampered\n").unwrap();
    let before_lock_check = read_tree(&project);
    let checked = tokyo(&project, &["openapi", "check"]);
    assert_eq!(checked.status.code(), Some(1), "{checked:?}");
    assert_eq!(
        read_tree(&project),
        before_lock_check,
        "check must not repair a stale lock"
    );
    assert!(tokyo(&project, &["openapi", "sync"]).status.success());
    assert!(tokyo(&project, &["openapi", "check"]).status.success());
}

#[test]
fn http_source_succeeds_and_reports_status_and_size_errors() {
    let temp = TempDir::new("http");
    let project = project(&temp);
    let url = serve("200 OK", "application/yaml", SPEC_A.as_bytes().to_vec());
    let added = tokyo(&project, &["openapi", "add", &url]);
    assert!(added.status.success(), "{added:?}");

    let url = serve(
        "503 Service Unavailable",
        "text/plain",
        b"try later".to_vec(),
    );
    let failed = tokyo(&project, &["openapi", "add", &url]);
    assert_eq!(failed.status.code(), Some(2), "{failed:?}");
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("503"), "{stderr}");
    assert!(stderr.contains("try later"), "{stderr}");

    let oversized = vec![b' '; 16 * 1024 * 1024 + 1];
    let url = serve("200 OK", "application/json", oversized);
    let failed = tokyo(&project, &["openapi", "add", &url]);
    assert_eq!(failed.status.code(), Some(2), "{failed:?}");
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("byte limit"),
        "{failed:?}"
    );
}

#[test]
fn missing_header_environment_variable_fails_without_writes() {
    let temp = TempDir::new("header");
    let project = project(&temp);
    fs::write(
        project.join("tokyo.toml"),
        "[project]\nname = \"test-cli\"\nroutes = \"src/routes\"\n\n[openapi]\nsource = \"http://127.0.0.1:9/spec\"\nsnapshot = \"openapi/upstream.json\"\n\n[openapi.headers]\nAuthorization = \"TOKYO_TEST_MISSING_OPENAPI_TOKEN\"\n",
    )
    .unwrap();
    let before = read_tree(&project);
    let checked = Command::new(env!("CARGO_BIN_EXE_tokyo"))
        .current_dir(&project)
        .env_remove("TOKYO_TEST_MISSING_OPENAPI_TOKEN")
        .args(["openapi", "check"])
        .output()
        .unwrap();
    assert_eq!(checked.status.code(), Some(2), "{checked:?}");
    assert!(
        String::from_utf8_lossy(&checked.stderr).contains("TOKYO_TEST_MISSING_OPENAPI_TOKEN"),
        "{checked:?}"
    );
    assert_eq!(read_tree(&project), before);
}

#[test]
fn generated_top_level_conflicts_still_apply_with_vendored_openapi() {
    let temp = TempDir::new("conflict");
    let project = project(&temp);
    fs::write(project.join("source.yaml"), SPEC_A).unwrap();
    fs::write(project.join("src/routes/pets.rs"), "pub fn route() {}\n").unwrap();
    assert!(
        tokyo(&project, &["openapi", "add", "source.yaml"])
            .status
            .success()
    );
    let generated = tokyo(&project, &["generate"]);
    assert_eq!(generated.status.code(), Some(2), "{generated:?}");
    assert!(
        String::from_utf8_lossy(&generated.stderr).contains("generated top-level command"),
        "{generated:?}"
    );
}
