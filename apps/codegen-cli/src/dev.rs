//! Development watcher, rebuild loop, and stable binary management.

use crate::cli::*;
use crate::config::*;
use crate::error::*;
use crate::generate::*;
use crate::prelude::*;

/// `tokyo dev`: the framework's edit loop. Watches generation inputs (spec,
/// config, scenario files) and the generated CLI's `src/` for OS-level file
/// events (via `notify`, debounced); a spec or config change regenerates
/// managed files and rebuilds, a source change rebuilds only. Every
/// successful build refreshes a stable path at `.tokyo/bin/<name>` so the
/// generated CLI can always be invoked at the same location without going
/// through `cargo` — see [`refresh_stable_binary_path`].
/// Regeneration/build failures (mid-edit YAML, hand-edited managed files,
/// compile errors) are reported and the loop keeps watching.
pub(crate) fn run_dev_command(dev_command_arguments: DevArgs) -> AppResult<()> {
    let common = dev_command_arguments.common;
    let runtime_path = match dev_command_arguments.runtime_path {
        Some(path) => Some(fs::canonicalize(&path).map_err(|error| {
            input_error(format!(
                "cannot resolve --runtime-path {}: {error}",
                path.display()
            ))
        })?),
        None => None,
    };
    let (codegen_config, output_directory) =
        load_generation_settings_and_output_directory(&common)?;
    let config_path = configured_or_default_config_path(&common);

    let mut generation_input_paths = Vec::new();
    if let Some(spec_path) = resolve_optional_openapi_input_path(&common)? {
        generation_input_paths.push(spec_path);
    }
    if let Some(config_path) = &config_path {
        let config_directory = config_path.parent().unwrap_or(Path::new("."));
        for configured_scenario in &codegen_config.cli_scenarios {
            if let Some(scenario_file) = &configured_scenario.file {
                generation_input_paths.push(config_directory.join(scenario_file));
            }
        }
        generation_input_paths.push(config_path.clone());
    }
    let user_source_directory = output_directory.join("src");
    let routes_directory = resolve_configured_routes_directory(&common)?;

    println!(
        "tokyo dev: watching {} generation input(s) and {}; Ctrl-C to stop",
        generation_input_paths.len(),
        user_source_directory.display(),
    );
    dev_regenerate_and_build(&common, &output_directory, runtime_path.as_deref());

    let (event_tx, event_rx) = mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(Duration::from_millis(300), move |result| {
        let _ = event_tx.send(result);
    })
    .map_err(|error| output_error(format!("cannot start file watcher: {error}")))?;

    // Editors often replace a file via a rename over the original path,
    // which breaks a watch held on the file itself (the inode changes
    // under it). Watching each input's parent directory non-recursively
    // survives that; events are filtered back down to relevant paths below.
    let mut watched_directories = BTreeSet::new();
    for path in &generation_input_paths {
        if let Some(parent) = path.parent() {
            watched_directories.insert(parent.to_path_buf());
        }
    }
    for directory in &watched_directories {
        if let Err(error) = debouncer
            .watcher()
            .watch(directory, RecursiveMode::NonRecursive)
        {
            eprintln!("tokyo dev: cannot watch {}: {error}", directory.display());
        }
    }
    if user_source_directory.is_dir() {
        debouncer
            .watcher()
            .watch(&user_source_directory, RecursiveMode::Recursive)
            .map_err(|error| {
                output_error(format!(
                    "cannot watch {}: {error}",
                    user_source_directory.display()
                ))
            })?;
    }

    while let Ok(result) = event_rx.recv() {
        let events = match result {
            Ok(events) => events,
            Err(error) => {
                eprintln!("tokyo dev: watch error: {error}");
                continue;
            }
        };
        let changed: Vec<PathBuf> = events
            .into_iter()
            .map(|event| event.path)
            .filter(|path| {
                is_relevant_watch_path(path)
                    && (generation_input_paths.contains(path)
                        || path.starts_with(&user_source_directory))
            })
            .collect();
        if changed.is_empty() {
            continue;
        }
        println!(
            "tokyo dev: {} changed",
            changed
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );
        let generation_changed = dev_change_requires_regeneration(
            &changed,
            &generation_input_paths,
            routes_directory.as_deref(),
        );
        if generation_changed {
            dev_regenerate_and_build(&common, &output_directory, runtime_path.as_deref());
        } else {
            dev_build_only(&output_directory, runtime_path.as_deref());
        }
        // Regenerating and building both write into the watched tree
        // (managed `.tokyo/src/**` files, `.tokyo/bin/<name>`). Drain the
        // events those writes themselves produce so they don't immediately
        // trigger a second, redundant rebuild.
        while event_rx.recv_timeout(Duration::from_millis(400)).is_ok() {}
    }
    Ok(())
}

/// Filters out editor swap/backup artifacts (`.foo.swp`, `foo~`,
/// `.#foo`, atomic-save temp files like `.!1234!foo.rs`) that would
/// otherwise register as spurious source changes.
pub(crate) fn is_relevant_watch_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    !file_name.starts_with('.') && !file_name.ends_with('~')
}

pub(crate) fn dev_regenerate_and_build(
    common: &CommonArgs,
    output_directory: &Path,
    runtime_path: Option<&Path>,
) {
    match run_generate_or_check_command(
        GenerationArgs {
            common: common.clone(),
        },
        false,
    ) {
        Ok(()) => dev_build_only(output_directory, runtime_path),
        Err(error) => eprintln!("tokyo dev: regeneration failed: {error:#}"),
    }
}

pub(crate) fn dev_build_only(output_directory: &Path, runtime_path: Option<&Path>) {
    match build_generated_cli_binary(output_directory, runtime_path) {
        Ok(executable_path) => match refresh_stable_binary_path(output_directory, &executable_path)
        {
            Ok(stable_path) => println!("tokyo dev: ✓ built → {}", stable_path.display()),
            Err(error) => println!(
                "tokyo dev: ✓ built (cannot refresh {}: {error:#})",
                stable_binary_directory(output_directory).display()
            ),
        },
        Err(error) => eprintln!("tokyo dev: ✗ build failed: {error}"),
    }
}

pub(crate) fn dev_change_requires_regeneration(
    changed_paths: &[PathBuf],
    generation_input_paths: &[PathBuf],
    routes_directory: Option<&Path>,
) -> bool {
    changed_paths.iter().any(|path| {
        generation_input_paths.contains(path)
            || routes_directory.is_some_and(|routes| path.starts_with(routes))
    })
}

/// Runs `cargo build` for the generated CLI project and returns the path to
/// its executable, resolved from cargo's own JSON build messages rather than
/// guessed from the package name — this stays correct regardless of target
/// triple, custom `CARGO_TARGET_DIR`, or workspace nesting. Compiler
/// diagnostics stream to stderr as they happen (`json-render-diagnostics`);
/// only the artifact messages are parsed from stdout.
pub(crate) fn build_generated_cli_binary(
    output_directory: &Path,
    runtime_path: Option<&Path>,
) -> Result<PathBuf, String> {
    let cargo = std::env::var_os("TOKYO_CODEGEN_CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = std::process::Command::new(cargo);
    if let Some(runtime_path) = runtime_path {
        command.arg("--config").arg(format!(
            "patch.crates-io.tokyo-cli-runtime.path={runtime_path:?}"
        ));
    }
    let mut child = command
        .args([
            "build",
            "--quiet",
            "--message-format=json-render-diagnostics",
        ])
        .current_dir(output_directory)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run cargo build: {error}"))?;
    let stdout = child.stdout.take().expect("stdout was piped");

    let mut executable_path = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { continue };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if message.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let is_bin = message
            .get("target")
            .and_then(|target| target.get("kind"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")));
        if !is_bin {
            continue;
        }
        if let Some(executable) = message
            .get("executable")
            .and_then(serde_json::Value::as_str)
        {
            executable_path = Some(PathBuf::from(executable));
        }
    }

    let status = child
        .wait()
        .map_err(|error| format!("cannot wait for cargo build: {error}"))?;
    if !status.success() {
        return Err("cargo build failed (see errors above)".to_string());
    }
    executable_path
        .ok_or_else(|| "cargo build succeeded but produced no `bin` executable artifact".into())
}

pub(crate) fn stable_binary_directory(output_directory: &Path) -> PathBuf {
    output_directory.join(".tokyo").join("bin")
}

/// Publishes `executable_path` at a fixed location, `.tokyo/bin/<file name>`,
/// so the generated CLI has one address that never changes across rebuilds —
/// nothing to add to `PATH`, nothing to look up in `target/debug/`. A plain
/// symlink is used on Unix; Windows falls back to a hard link, then a copy,
/// since symlinks there require developer mode or elevation.
pub(crate) fn refresh_stable_binary_path(
    output_directory: &Path,
    executable_path: &Path,
) -> Result<PathBuf, String> {
    let bin_directory = stable_binary_directory(output_directory);
    fs::create_dir_all(&bin_directory)
        .map_err(|error| format!("cannot create {}: {error}", bin_directory.display()))?;
    let file_name = executable_path
        .file_name()
        .ok_or_else(|| "built executable has no file name".to_string())?;
    let stable_path = bin_directory.join(file_name);
    let _ = fs::remove_file(&stable_path);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(executable_path, &stable_path)
            .map_err(|error| format!("cannot symlink {}: {error}", stable_path.display()))?;
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(executable_path, &stable_path)
            .or_else(|_| fs::hard_link(executable_path, &stable_path))
            .or_else(|_| fs::copy(executable_path, &stable_path).map(|_| ()))
            .map_err(|error| format!("cannot link {}: {error}", stable_path.display()))?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::copy(executable_path, &stable_path)
            .map(|_| ())
            .map_err(|error| format!("cannot copy {}: {error}", stable_path.display()))?;
    }

    Ok(stable_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("tokyo-dev-test-{}-{sequence}", std::process::id()));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn dev_change_requires_regeneration_matches_inputs_and_routes() {
        let temp = TestDir::new();
        let spec = temp.0.join("openapi.yaml");
        let routes_directory = temp.0.join("cli/src/routes");
        let other_source_file = temp.0.join("cli/src/middleware.rs");
        let inputs = vec![spec.clone()];

        assert!(dev_change_requires_regeneration(
            std::slice::from_ref(&spec),
            &inputs,
            Some(&routes_directory),
        ));
        assert!(dev_change_requires_regeneration(
            &[routes_directory.join("customers/list.rs")],
            &inputs,
            Some(&routes_directory),
        ));
        assert!(!dev_change_requires_regeneration(
            &[other_source_file],
            &inputs,
            Some(&routes_directory),
        ));
    }

    #[test]
    fn refresh_stable_binary_path_points_at_latest_build() {
        let temp = TestDir::new();
        let executable = temp.0.join("target/debug/example-cli");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"v1").unwrap();

        let stable_path = refresh_stable_binary_path(&temp.0, &executable).unwrap();
        assert_eq!(stable_path, temp.0.join(".tokyo/bin/example-cli"));
        assert_eq!(fs::read(&stable_path).unwrap(), b"v1");

        fs::write(&executable, b"v2").unwrap();
        let stable_path_again = refresh_stable_binary_path(&temp.0, &executable).unwrap();
        assert_eq!(stable_path_again, stable_path);
        assert_eq!(fs::read(&stable_path).unwrap(), b"v2");
    }
}
