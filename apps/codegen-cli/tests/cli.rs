#![allow(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "tokyo-cli-{name}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tokyo"))
        .args(arguments)
        .output()
        .expect("run tokyo")
}

fn git(repo: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(arguments)
        .output()
        .expect("run git")
}

fn git_with_identity(repo: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Tokyo Test",
            "-c",
            "user.email=tokyo-test@example.invalid",
        ])
        .args(arguments)
        .output()
        .expect("run git")
}

fn generate(input: &Path, output: &Path) -> Output {
    run(&[
        "generate",
        "--input",
        input.to_str().expect("UTF-8 input"),
        "--output",
        output.to_str().expect("UTF-8 output"),
    ])
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/petstore.yaml")
}

fn read_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(directory).expect("read generated directory") {
            let entry = entry.expect("read generated entry");
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).expect("read generated file"),
                ));
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

#[test]
fn init_scaffolds_a_compilable_route_only_project() {
    let temp = TempDir::new("init");
    let project = temp.0.join("hello-cli");
    let initialized = run(&["init", project.to_str().unwrap(), "--name", "hello-cli"]);
    assert!(initialized.status.success(), "{initialized:?}");

    for path in [
        "Cargo.toml",
        "README.md",
        "tokyo.toml",
        ".tokyo/src/main.rs",
        ".tokyo/src/cli.rs",
        ".tokyo/src/tokyo/routes.rs",
        "src/middleware.rs",
        "src/routes/mod.rs",
        "src/routes/index.rs",
        ".cursor/skills/tokyo-project-layout/SKILL.md",
        ".cursor/skills/tokyo-filesystem-routes/SKILL.md",
        ".cursor/skills/tokyo-agent-discovery/SKILL.md",
        ".cursor/skills/tokyo-scripting-protocol/SKILL.md",
        ".cursor/skills/tokyo-auth-profiles/SKILL.md",
        ".cursor/skills/tokyo-achieve-outcomes/SKILL.md",
        ".cursor/skills/tokyo-scenarios-run/SKILL.md",
        ".cursor/skills/tokyo-request-bodies/SKILL.md",
        ".cursor/skills/tokyo-streaming-binary/SKILL.md",
        ".cursor/skills/tokyo-openapi-lifecycle/SKILL.md",
        ".cursor/skills/tokyo-project-config/SKILL.md",
        ".cursor/skills/tokyo-guidance-presentation/SKILL.md",
    ] {
        assert!(project.join(path).is_file(), "missing {path}");
    }
    assert!(!project.join("tokyo.lock").exists());
    assert!(!project.join("openapi.yaml").exists());
    let config = fs::read_to_string(project.join("tokyo.toml")).unwrap();
    assert!(config.contains("[project]"), "{config}");
    assert!(!config.contains("[openapi]"), "{config}");

    let checked = Command::new("cargo")
        .args([
            "check",
            "--config",
            &format!(
                "patch.crates-io.tokyo-cli-runtime.path={:?}",
                runtime_path()
            ),
        ])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(checked.status.success(), "{checked:?}");
    assert!(
        !String::from_utf8_lossy(&checked.stderr).contains("warning:"),
        "fresh projects must compile without warnings: {checked:?}"
    );
}

fn run_in(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tokyo"))
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run tokyo in project")
}

fn runtime_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("crates/cli-runtime")
}

#[test]
fn route_only_generation_discovers_nested_routes_and_dispatches() {
    let temp = TempDir::new("routes");
    let project = temp.0.join("route-cli");
    assert!(
        run(&["init", project.to_str().unwrap(), "--name", "route-cli"])
            .status
            .success()
    );
    fs::create_dir_all(project.join("src/routes/customers")).unwrap();
    fs::write(
        project.join("src/routes/customers/list.rs"),
        r#"use tokyo_cli_runtime::prelude::*;

pub fn route() -> Route {
    Route::new(
        RouteSpec::new("ignored").about("List local customers")
            .arg(Argument::new("name").required()),
        |context| Ok(RouteResponse::text(format!("customer:{}", context.args().require("name")?))),
    )
}
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/middleware.rs"),
        r#"use tokyo_cli_runtime::prelude::Route;

pub fn decorate(route: Route) -> Route {
    route.middleware_fn(|context, next| {
        eprintln!("middleware:filesystem-route");
        next.run(context)
    })
}
"#,
    )
    .unwrap();

    let generated = run_in(&project, &["generate"]);
    assert!(generated.status.success(), "{generated:?}");
    let registry = fs::read_to_string(project.join(".tokyo/src/tokyo/routes.rs")).unwrap();
    assert!(registry.contains("customers/list.rs"), "{registry}");
    assert!(registry.contains("\"customers\""), "{registry}");
    assert!(registry.contains("\"list\""), "{registry}");
    let checked = run_in(&project, &["check"]);
    assert!(checked.status.success(), "{checked:?}");

    let cargo_config = format!(
        "patch.crates-io.tokyo-cli-runtime.path={:?}",
        runtime_path()
    );
    let help = Command::new("cargo")
        .args(["run", "--quiet", "--config", &cargo_config, "--", "--help"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(help.status.success(), "{help:?}");
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("customers"),
        "{help:?}"
    );
    let dispatched = Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "--config",
            &cargo_config,
            "--",
            "customers",
            "list",
            "Ada",
        ])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(dispatched.status.success(), "{dispatched:?}");
    assert_eq!(
        String::from_utf8_lossy(&dispatched.stdout).trim(),
        "customer:Ada"
    );
    assert!(
        String::from_utf8_lossy(&dispatched.stderr).contains("middleware:filesystem-route"),
        "{dispatched:?}"
    );
    let schema = Command::new("cargo")
        .args(["run", "--quiet", "--config", &cargo_config, "--", "schema"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(schema.status.success(), "{schema:?}");
    let schema_text = String::from_utf8_lossy(&schema.stdout);
    assert!(schema_text.contains("customers.list"), "{schema_text}");
    assert!(
        schema_text.contains("List local customers"),
        "{schema_text}"
    );
}

#[test]
fn route_discovery_rejects_conflicts_and_invalid_layouts() {
    for (name, files, expected) in [
        (
            "reserved",
            vec![("src/routes/schema.rs", "pub fn route() {}")],
            "reserved or generated",
        ),
        (
            "invalid",
            vec![("src/routes/bad-name.rs", "pub fn route() {}")],
            "invalid route identifier",
        ),
        (
            "module-layout",
            vec![
                ("src/routes/foo.rs", "pub fn route() {}"),
                ("src/routes/foo/mod.rs", ""),
            ],
            "ambiguous route module layout",
        ),
    ] {
        let temp = TempDir::new(name);
        let project = temp.0.join(name);
        fs::create_dir_all(project.join("src/routes")).unwrap();
        fs::write(
            project.join("tokyo.toml"),
            format!("[project]\nname = {name:?}\nroutes = \"src/routes\"\n"),
        )
        .unwrap();
        for (path, contents) in files {
            let path = project.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let generated = run_in(&project, &["generate"]);
        assert!(!generated.status.success(), "{generated:?}");
        assert!(
            String::from_utf8_lossy(&generated.stderr).contains(expected),
            "{generated:?}"
        );
    }
}

#[test]
fn init_preflights_all_paths_without_partial_writes() {
    let temp = TempDir::new("init-conflict");
    let project = temp.0.join("existing");
    fs::create_dir_all(project.join("src/routes")).unwrap();
    fs::write(project.join("src/routes/index.rs"), "// keep\n").unwrap();

    let initialized = run(&["init", project.to_str().unwrap(), "--name", "existing"]);
    assert!(!initialized.status.success(), "{initialized:?}");
    assert_eq!(
        fs::read_to_string(project.join("src/routes/index.rs")).unwrap(),
        "// keep\n"
    );
    assert!(!project.join("Cargo.toml").exists());
    assert!(!project.join("tokyo.toml").exists());
    assert!(!project.join(".tokyo").exists());
}

#[test]
fn project_config_can_supply_optional_openapi_paths() {
    let temp = TempDir::new("project-config");
    let project = temp.0.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::copy(fixture(), project.join("api.yaml")).unwrap();
    fs::write(
        project.join("tokyo.toml"),
        "[project]\nname = \"pets\"\nroutes = \"src/routes\"\n\n[openapi]\ninput = \"api.yaml\"\noutput = \"generated\"\n",
    )
    .unwrap();

    let generated = run(&[
        "generate",
        "--config",
        project.join("tokyo.toml").to_str().unwrap(),
    ]);
    assert!(generated.status.success(), "{generated:?}");
    assert!(project.join("generated/Cargo.toml").is_file());
    let cli = fs::read_to_string(project.join("generated/.tokyo/src/cli.rs")).unwrap();
    assert!(cli.contains("#[command(name = \"pets\""), "{cli}");
}

#[test]
fn generates_only_a_standalone_cli() {
    let temp = TempDir::new("generate");
    let output = temp.0.join("cli");
    let config = temp.0.join("tokyo.toml");
    fs::write(&config, "package = \"@example/sdk\"\ncli_name = \"pets\"\n").unwrap();

    let generated = run(&[
        "generate",
        "--input",
        fixture().to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(generated.status.success(), "{generated:?}");
    assert!(output.join("Cargo.toml").is_file());
    assert!(output.join(".tokyo/src/main.rs").is_file());
    assert!(output.join(".tokyo/src/cli.rs").is_file());
    assert!(output.join("src/middleware.rs").is_file());
    assert!(output.join("src/commands/custom.rs").is_file());
    assert!(output.join(".tokyo/ir.json").is_file());
    assert!(!output.join("package.json").exists());

    let cli = fs::read_to_string(output.join(".tokyo/src/cli.rs")).unwrap();
    assert!(cli.contains("#[command(name = \"pets\""), "{cli}");
}

#[test]
fn reruns_are_byte_identical() {
    let temp = TempDir::new("deterministic");
    let output = temp.0.join("cli");
    assert!(generate(&fixture(), &output).status.success());
    let first = read_tree(&output);
    assert!(generate(&fixture(), &output).status.success());
    assert_eq!(first, read_tree(&output));
}

#[test]
fn update_branch_commits_only_generated_source_changes() {
    let temp = TempDir::new("update-branch");
    let input = temp.0.join("openapi.yaml");
    let output = temp.0.join("cli");
    fs::copy(fixture(), &input).unwrap();
    assert!(generate(&input, &output).status.success());

    let custom = output.join("src/commands/custom.rs");
    let custom_source = "pub fn app_owned_marker() {}\n";
    fs::write(&custom, custom_source).unwrap();

    let initialized_git = git(&output, &["init"]);
    assert!(initialized_git.status.success(), "{initialized_git:?}");
    assert!(git(&output, &["add", "."]).status.success());
    assert!(
        git_with_identity(&output, &["commit", "-m", "initial cli"])
            .status
            .success()
    );
    let base_branch_output = git(&output, &["branch", "--show-current"]);
    assert!(
        base_branch_output.status.success(),
        "{base_branch_output:?}"
    );
    let base_branch = String::from_utf8_lossy(&base_branch_output.stdout)
        .trim()
        .to_string();

    let text = fs::read_to_string(&input).unwrap();
    fs::write(
        &input,
        text.replace("operationId: listPets", "operationId: listAllPets"),
    )
    .unwrap();
    let summary_file = temp.0.join("pr-summary.md");

    let updated = run(&[
        "update-branch",
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--branch",
        "tokyo/update-test",
        "--summary-file",
        summary_file.to_str().unwrap(),
    ]);
    assert!(updated.status.success(), "{updated:?}");
    let stdout = String::from_utf8_lossy(&updated.stdout);
    assert!(stdout.contains("# Tokyo Generated CLI Update"), "{stdout}");
    assert!(stdout.contains("## API Changes"), "{stdout}");
    assert!(stdout.contains("endpoint"), "{stdout}");
    assert!(
        stdout.contains(".tokyo/src/tokyo/commands/default.rs"),
        "{stdout}"
    );
    assert!(
        stdout.contains("No app-owned files were changed"),
        "{stdout}"
    );

    let summary = fs::read_to_string(&summary_file).unwrap();
    assert_eq!(
        summary.trim_end(),
        stdout[stdout.find("# Tokyo").unwrap()..].trim_end()
    );

    let branch_output = git(&output, &["branch", "--show-current"]);
    assert!(branch_output.status.success(), "{branch_output:?}");
    assert_eq!(
        String::from_utf8_lossy(&branch_output.stdout).trim(),
        "tokyo/update-test"
    );
    assert_eq!(fs::read_to_string(&custom).unwrap(), custom_source);

    let diff = git(
        &output,
        &["diff", "--name-only", &format!("{base_branch}..HEAD")],
    );
    assert!(diff.status.success(), "{diff:?}");
    let changed_paths = String::from_utf8_lossy(&diff.stdout);
    assert!(changed_paths.contains(".tokyo/ir.json"), "{changed_paths}");
    assert!(
        changed_paths.contains(".tokyo/src/tokyo/commands/default.rs"),
        "{changed_paths}"
    );
    assert!(
        !changed_paths.contains("src/commands/custom.rs"),
        "{changed_paths}"
    );
}

#[test]
fn route_extension_starters_are_user_owned() {
    let temp = TempDir::new("custom");
    let output = temp.0.join("cli");
    assert!(generate(&fixture(), &output).status.success());

    let custom = output.join("src/commands/custom.rs");
    let middleware = output.join("src/middleware.rs");
    let manifest_file = output.join("Cargo.toml");
    let readme = output.join("README.md");
    let project_layout_skill = output.join(".cursor/skills/tokyo-project-layout/SKILL.md");
    let routes_skill = output.join(".cursor/skills/tokyo-filesystem-routes/SKILL.md");
    assert!(custom.is_file());
    assert!(middleware.is_file());
    assert!(manifest_file.is_file());
    assert!(readme.is_file());
    assert!(project_layout_skill.is_file());
    assert!(routes_skill.is_file());

    let manifest_path = output.join(".tokyo/manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert!(
        !manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/commands/custom.rs")
    );
    assert!(
        !manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/middleware.rs")
    );
    assert!(
        !manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "Cargo.toml" || path == "README.md")
    );
    assert!(
        !manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == ".cursor/skills/tokyo-project-layout/SKILL.md")
    );

    let local_custom = "pub fn local_custom_marker() {}\n";
    let local_middleware = "pub fn local_middleware_marker() {}\n";
    let local_skill = "---\nname: tokyo-project-layout\ndescription: Local guidance.\n---\n";
    let local_manifest = format!(
        "{}\n# developer dependency marker\n",
        fs::read_to_string(&manifest_file).unwrap()
    );
    let local_readme = "# Developer README\n";
    fs::write(&custom, local_custom).unwrap();
    fs::write(&middleware, local_middleware).unwrap();
    fs::write(&project_layout_skill, local_skill).unwrap();
    fs::write(&manifest_file, &local_manifest).unwrap();
    fs::write(&readme, local_readme).unwrap();
    fs::remove_file(&routes_skill).unwrap();

    assert!(generate(&fixture(), &output).status.success());
    assert_eq!(fs::read_to_string(&custom).unwrap(), local_custom);
    assert_eq!(fs::read_to_string(&middleware).unwrap(), local_middleware);
    assert_eq!(
        fs::read_to_string(&project_layout_skill).unwrap(),
        local_skill
    );
    assert_eq!(fs::read_to_string(&manifest_file).unwrap(), local_manifest);
    assert_eq!(fs::read_to_string(&readme).unwrap(), local_readme);
    assert!(
        routes_skill.is_file(),
        "missing starter skills should be restored"
    );

    let clean = run(&[
        "check",
        "--input",
        fixture().to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(clean.status.success(), "{clean:?}");
}

#[test]
fn legacy_input_argument_still_generates_the_cli() {
    let temp = TempDir::new("legacy");
    let output = temp.0.join("cli");
    let generated = run(&[
        fixture().to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(generated.status.success(), "{generated:?}");
    assert!(output.join("Cargo.toml").is_file());
}

#[test]
fn check_detects_drift_without_mutating() {
    let temp = TempDir::new("check");
    let output = temp.0.join("cli");
    assert!(generate(&fixture(), &output).status.success());

    let clean = run(&[
        "check",
        "--input",
        fixture().to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(clean.status.success(), "{clean:?}");

    let main = output.join(".tokyo/src/main.rs");
    fs::write(&main, "local edit\n").unwrap();
    let dirty = run(&[
        "check",
        "--input",
        fixture().to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert_eq!(dirty.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&dirty.stderr).contains("modified .tokyo/src/main.rs"));
    assert_eq!(fs::read_to_string(main).unwrap(), "local edit\n");
}

#[test]
fn scenario_files_are_resolved_relative_to_config() {
    let temp = TempDir::new("scenario");
    let output = temp.0.join("cli");
    let project = temp.0.join("project");
    fs::create_dir_all(project.join("scenarios")).unwrap();
    fs::write(
        project.join("scenarios/smoke.scenario"),
        "pets list --limit @set:limit\n",
    )
    .unwrap();
    let config = project.join("tokyo.toml");
    fs::write(
        &config,
        r#"
[[cli_scenarios]]
name = "smoke"
description = "List pets"
file = "scenarios/smoke.scenario"
"#,
    )
    .unwrap();

    let generated = run(&[
        "generate",
        "--input",
        fixture().to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(generated.status.success(), "{generated:?}");
    let runtime_config = fs::read_to_string(output.join(".tokyo/src/tokyo/config.rs")).unwrap();
    assert!(runtime_config.contains(r#"name: "smoke""#));
    assert!(runtime_config.contains("pets list --limit @set:limit"));
}

#[test]
fn diff_reports_openapi_changes() {
    let temp = TempDir::new("diff");
    let input = temp.0.join("openapi.yaml");
    let output = temp.0.join("cli");
    fs::copy(fixture(), &input).unwrap();
    assert!(generate(&input, &output).status.success());

    let unchanged = run(&[
        "diff",
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(unchanged.status.success(), "{unchanged:?}");

    let text = fs::read_to_string(&input).unwrap();
    fs::write(
        &input,
        text.replace("operationId: listPets", "operationId: listAllPets"),
    )
    .unwrap();
    let changed = run(&[
        "diff",
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(changed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&changed.stdout).contains("Endpoint"));
}

#[test]
fn manifest_removes_only_generator_owned_files() {
    let temp = TempDir::new("manifest");
    let output = temp.0.join("cli");
    assert!(generate(&fixture(), &output).status.success());

    let manifest_path = output.join(".tokyo/manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"]
        .as_array_mut()
        .unwrap()
        .push("stale-generated.rs".into());
    fs::write(
        &manifest_path,
        format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
    )
    .unwrap();
    fs::write(output.join("stale-generated.rs"), "stale").unwrap();
    fs::write(output.join("user-notes.txt"), "keep").unwrap();

    assert!(generate(&fixture(), &output).status.success());
    assert!(!output.join("stale-generated.rs").exists());
    assert_eq!(
        fs::read_to_string(output.join("user-notes.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn hand_edited_managed_file_fails_regeneration() {
    let temp = TempDir::new("hand-edit");
    let output = temp.0.join("cli");
    assert!(generate(&fixture(), &output).status.success());

    let managed = output.join(".tokyo/src/tokyo/config.rs");
    let mut edited = fs::read_to_string(&managed).unwrap();
    edited.push_str("// hand edit\n");
    fs::write(&managed, edited).unwrap();

    let rerun = generate(&fixture(), &output);
    assert!(!rerun.status.success());
    let stderr = String::from_utf8_lossy(&rerun.stderr);
    assert!(stderr.contains("edited by hand"), "{stderr}");
    assert!(stderr.contains(".tokyo/src/tokyo/config.rs"), "{stderr}");
    // The edit survives: nothing was overwritten.
    assert!(
        fs::read_to_string(&managed)
            .unwrap()
            .contains("// hand edit"),
        "failed generation must not overwrite the hand edit"
    );
}

#[test]
fn removed_managed_file_is_recreated_and_user_edits_are_ignored() {
    let temp = TempDir::new("recreate");
    let output = temp.0.join("cli");
    assert!(generate(&fixture(), &output).status.success());

    fs::remove_file(output.join(".tokyo/src/tokyo/config.rs")).unwrap();
    fs::write(
        output.join("src/commands/custom.rs"),
        "pub fn edited_by_user() {}\n",
    )
    .unwrap();

    assert!(generate(&fixture(), &output).status.success());
    assert!(output.join(".tokyo/src/tokyo/config.rs").is_file());
    assert_eq!(
        fs::read_to_string(output.join("src/commands/custom.rs")).unwrap(),
        "pub fn edited_by_user() {}\n"
    );
}

#[test]
fn unchanged_managed_files_regenerate_without_hash_failures() {
    let temp = TempDir::new("unchanged");
    let output = temp.0.join("cli");
    assert!(generate(&fixture(), &output).status.success());
    assert!(generate(&fixture(), &output).status.success());

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join(".tokyo/manifest.json")).unwrap()).unwrap();
    let hashes = manifest["hashes"]
        .as_object()
        .expect("manifest records hashes");
    assert!(
        hashes.contains_key(".tokyo/src/tokyo/config.rs"),
        "{manifest}"
    );
    assert!(
        !hashes.contains_key("src/commands/custom.rs"),
        "starter files must not be hash-tracked: {manifest}"
    );
}

fn write_executable_stub(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn update_branch_validates_pushes_and_converges_on_one_pr() {
    let temp = TempDir::new("pr-flow");
    let input = temp.0.join("openapi.yaml");
    let output = temp.0.join("cli");
    fs::copy(fixture(), &input).unwrap();
    assert!(generate(&input, &output).status.success());

    let initialized_git = git(&output, &["init"]);
    assert!(initialized_git.status.success(), "{initialized_git:?}");
    assert!(git(&output, &["add", "."]).status.success());
    assert!(
        git_with_identity(&output, &["commit", "-m", "initial cli"])
            .status
            .success()
    );
    let remote = temp.0.join("remote.git");
    assert!(
        Command::new("git")
            .args(["init", "--bare", remote.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        git(
            &output,
            &["remote", "add", "origin", remote.to_str().unwrap()]
        )
        .status
        .success()
    );

    // Stub gh: logs invocations; `pr list` output comes from a control file.
    let gh_log = temp.0.join("gh.log");
    let pr_list_file = temp.0.join("pr-list.txt");
    fs::write(&pr_list_file, "").unwrap();
    let gh_stub = temp.0.join("gh-stub.sh");
    write_executable_stub(
        &gh_stub,
        &format!(
            "#!/bin/sh\necho \"$@\" >> {}\nif [ \"$1 $2\" = \"pr list\" ]; then cat {}; fi\nexit 0\n",
            gh_log.display(),
            pr_list_file.display()
        ),
    );
    // Stub cargo: validation passes.
    let cargo_ok = temp.0.join("cargo-ok.sh");
    write_executable_stub(&cargo_ok, "#!/bin/sh\nexit 0\n");

    let text = fs::read_to_string(&input).unwrap();
    fs::write(
        &input,
        text.replace("operationId: listPets", "operationId: listAllPets"),
    )
    .unwrap();

    let update_arguments = [
        "update-branch",
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--branch",
        "tokyo/pr-test",
        "--validate",
        "--push",
        "--pr",
    ];
    let updated = Command::new(env!("CARGO_BIN_EXE_tokyo"))
        .args(update_arguments)
        .env("TOKYO_CODEGEN_GH", &gh_stub)
        .env("TOKYO_CODEGEN_CARGO", &cargo_ok)
        .output()
        .unwrap();
    assert!(updated.status.success(), "{updated:?}");
    let stdout = String::from_utf8_lossy(&updated.stdout);
    assert!(stdout.contains("`cargo check` passed"), "{stdout}");
    assert!(stdout.contains("pushed branch tokyo/pr-test"), "{stdout}");
    assert!(
        stdout.contains("created pull request for branch tokyo/pr-test"),
        "{stdout}"
    );
    let gh_calls = fs::read_to_string(&gh_log).unwrap();
    assert!(gh_calls.contains("pr create"), "{gh_calls}");

    // The branch really landed on the remote.
    let remote_branches = Command::new("git")
        .args(["-C", remote.to_str().unwrap(), "branch", "--list"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&remote_branches.stdout).contains("tokyo/pr-test"),
        "{remote_branches:?}"
    );

    // Second spec change: the existing open PR is updated, not duplicated.
    fs::write(&pr_list_file, "17\n").unwrap();
    let text = fs::read_to_string(&input).unwrap();
    fs::write(
        &input,
        text.replace("operationId: createPets", "operationId: createManyPets"),
    )
    .unwrap();
    // update-branch requires a clean worktree: return to the base branch state.
    assert!(
        git(&output, &["checkout", "-B", "tokyo/pr-test"])
            .status
            .success()
    );
    let updated_again = Command::new(env!("CARGO_BIN_EXE_tokyo"))
        .args(update_arguments)
        .env("TOKYO_CODEGEN_GH", &gh_stub)
        .env("TOKYO_CODEGEN_CARGO", &cargo_ok)
        .output()
        .unwrap();
    assert!(updated_again.status.success(), "{updated_again:?}");
    let stdout = String::from_utf8_lossy(&updated_again.stdout);
    assert!(
        stdout.contains("updated pull request #17 for branch tokyo/pr-test"),
        "{stdout}"
    );
    let gh_calls = fs::read_to_string(&gh_log).unwrap();
    assert!(gh_calls.contains("pr edit 17"), "{gh_calls}");
}

#[test]
fn update_branch_validation_failure_is_reported_and_fails_clearly() {
    let temp = TempDir::new("pr-validate-fail");
    let input = temp.0.join("openapi.yaml");
    let output = temp.0.join("cli");
    fs::copy(fixture(), &input).unwrap();
    assert!(generate(&input, &output).status.success());
    let initialized_git = git(&output, &["init"]);
    assert!(initialized_git.status.success(), "{initialized_git:?}");
    assert!(git(&output, &["add", "."]).status.success());
    assert!(
        git_with_identity(&output, &["commit", "-m", "initial cli"])
            .status
            .success()
    );

    let cargo_fail = temp.0.join("cargo-fail.sh");
    write_executable_stub(
        &cargo_fail,
        "#!/bin/sh\necho 'error[E0425]: custom.rs no longer compiles' >&2\nexit 101\n",
    );

    let updated = Command::new(env!("CARGO_BIN_EXE_tokyo"))
        .args([
            "update-branch",
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--branch",
            "tokyo/validate-fail",
            "--validate",
        ])
        .env("TOKYO_CODEGEN_CARGO", &cargo_fail)
        .output()
        .unwrap();
    assert!(
        !updated.status.success(),
        "validation failure must be fatal"
    );
    let stdout = String::from_utf8_lossy(&updated.stdout);
    assert!(stdout.contains("`cargo check` failed"), "{stdout}");
    assert!(stdout.contains("custom.rs no longer compiles"), "{stdout}");
    assert!(
        stdout.contains("Tokyo \nnever edits app-owned files itself")
            || stdout.contains("never edits app-owned files"),
        "{stdout}"
    );
}

#[test]
fn legacy_manifest_and_snapshot_migrate_to_tokyo_directory() {
    let temp = TempDir::new("migrate");
    let output = temp.0.join("cli");
    assert!(generate(&fixture(), &output).status.success());

    // Rewrite the project into the legacy layout an older release produced.
    let manifest_text = fs::read_to_string(output.join(".tokyo/manifest.json"))
        .unwrap()
        .replace(".tokyo/manifest.json", ".tokyo-manifest.json")
        .replace(".tokyo/ir.json", ".tokyo-ir.json");
    fs::write(output.join(".tokyo-manifest.json"), manifest_text).unwrap();
    fs::rename(output.join(".tokyo/ir.json"), output.join(".tokyo-ir.json")).unwrap();
    fs::remove_file(output.join(".tokyo/manifest.json")).unwrap();
    fs::write(output.join("src/commands/custom.rs"), "// user edit\n").unwrap();

    assert!(generate(&fixture(), &output).status.success());
    assert!(output.join(".tokyo/manifest.json").is_file());
    assert!(output.join(".tokyo/ir.json").is_file());
    assert!(
        !output.join(".tokyo-manifest.json").exists(),
        "legacy manifest must be cleaned up"
    );
    assert!(
        !output.join(".tokyo-ir.json").exists(),
        "legacy snapshot must be cleaned up"
    );
    assert_eq!(
        fs::read_to_string(output.join("src/commands/custom.rs")).unwrap(),
        "// user edit\n"
    );
}

#[test]
fn previously_managed_starter_file_is_refreshed_once_but_user_edits_survive() {
    fn sha256_hex(contents: &[u8]) -> String {
        use sha2::Digest as _;
        sha2::Sha256::digest(contents)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    let temp = TempDir::new("starter-migration");
    let output = temp.0.join("cli");
    assert!(generate(&fixture(), &output).status.success());

    // Simulate an older release where src/commands/mod.rs was Tokyo-managed:
    // old-layout content on disk, with its hash recorded in the manifest.
    let mod_path = output.join("src/commands/mod.rs");
    let old_managed_content = "pub mod custom;\npub mod guidance;\npub mod default;\n";
    fs::write(&mod_path, old_managed_content).unwrap();
    let manifest_path = output.join(".tokyo/manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["hashes"]["src/commands/mod.rs"] =
        serde_json::Value::String(sha256_hex(old_managed_content.as_bytes()));
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    // Unedited (hash matches): Tokyo refreshes it to the new starter content.
    assert!(generate(&fixture(), &output).status.success());
    let refreshed = fs::read_to_string(&mod_path).unwrap();
    assert!(
        refreshed.contains("crate::tokyo::commands"),
        "unedited previously-managed starter should refresh: {refreshed}"
    );

    // Edited (hash no longer matches): the user's content is preserved.
    let user_content = "pub mod custom;\npub mod guidance;\npub mod mine;\n";
    fs::write(&mod_path, user_content).unwrap();
    assert!(generate(&fixture(), &output).status.success());
    assert_eq!(fs::read_to_string(&mod_path).unwrap(), user_content);
}
