//! `tokyo` project and code-generation command-line front end.
//!
//! The binary owns argument parsing, config/file path handling, output
//! transactions, and human diagnostics. Importing and emission stay in the
//! library crates so this app can use `anyhow` for contextual command errors.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use heck::ToKebabCase;
use notify::RecursiveMode;
use notify_debouncer_mini::{DebounceEventResult, new_debouncer};
use serde::{Deserialize, Serialize};
use tokyo_codegen_engine::{Config, Emitter, InputFormat, Snapshot};
use tokyo_ir::Api;
use tokyo_ir::diff::Change;

mod openapi;

const EXIT_DIFFERENCES: i32 = 1;
const EXIT_INPUT: i32 = 2;
const EXIT_OUTPUT: i32 = 3;
const DEFAULT_INPUT: &str = "examples/petstore.yaml";
const DEFAULT_OUTPUT: &str = "generated";
const DEFAULT_CONFIG: &str = "tokyo.toml";
const SNAPSHOT_FILE: &str = tokyo_codegen_engine::SNAPSHOT_FILE;
const MANIFEST_FILE: &str = ".tokyo/manifest.json";
/// Paths written by earlier releases, read as fallbacks so existing projects
/// migrate to `.tokyo/` on their next generation.
const LEGACY_MANIFEST_FILE: &str = ".tokyo-manifest.json";
const FORMAT_VERSION: u32 = 1;
static NEXT_TRANSACTION: AtomicU64 = AtomicU64::new(0);

#[derive(Parser)]
#[command(name = "tokyo", version, about = "Build route-first agent CLIs")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new route-first Tokyo Cargo project.
    Init(InitArgs),
    /// Generate files and safely remove stale generated files.
    Generate(GenerationArgs),
    /// Verify generated files without modifying the output directory.
    Check(GenerationArgs),
    /// Watch the spec, config, scenarios, and CLI sources; regenerate and
    /// rebuild on every change. Keeps `.tokyo/bin/<name>` pointed at the
    /// latest successful build so the generated CLI can be run directly.
    Dev(DevArgs),
    /// Update a local Git branch with generated-source changes only.
    UpdateBranch(UpdateBranchArgs),
    /// Compare the persisted IR snapshot with the current OpenAPI input.
    Diff(DiffArgs),
    /// OpenAPI project operations (reserved for the OpenAPI workflow).
    Openapi(OpenapiArgs),
}

#[derive(Args)]
struct InitArgs {
    /// Directory to initialize.
    #[arg(default_value = ".", value_name = "DIR")]
    directory: PathBuf,
    /// Cargo package and executable name.
    #[arg(long, value_name = "NAME")]
    name: String,
}

#[derive(Args)]
struct OpenapiArgs {
    #[command(subcommand)]
    command: OpenapiCommand,
}

#[derive(Subcommand)]
enum OpenapiCommand {
    /// Validate and vendor an OpenAPI document.
    Add(OpenapiAddArgs),
    /// Reacquire and update the configured vendored document.
    Sync(OpenapiConfigArgs),
    /// Report whether the configured source differs without writing.
    Check(OpenapiConfigArgs),
}

#[derive(Args)]
struct OpenapiAddArgs {
    /// HTTP(S) URL or local file path.
    #[arg(value_name = "URL|PATH")]
    source: String,
    /// Tokyo project configuration file.
    #[arg(short, long, default_value = DEFAULT_CONFIG, value_name = "FILE")]
    config: PathBuf,
}

#[derive(Args)]
struct OpenapiConfigArgs {
    /// Tokyo project configuration file.
    #[arg(short, long, default_value = DEFAULT_CONFIG, value_name = "FILE")]
    config: PathBuf,
}

#[derive(Args, Clone)]
struct CommonArgs {
    /// OpenAPI JSON or YAML input.
    #[arg(short, long, value_name = "FILE")]
    input: Option<PathBuf>,
    /// Generated output directory (overrides config).
    #[arg(short, long, value_name = "DIR")]
    output: Option<PathBuf>,
    /// TOML configuration file. Defaults to tokyo.toml when present.
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
}

#[derive(Args)]
struct GenerationArgs {
    #[command(flatten)]
    common: CommonArgs,
}

#[derive(Args)]
struct DevArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Use a local tokyo-cli-runtime checkout instead of the published crate
    /// when building the generated CLI.
    #[arg(long, value_name = "DIR")]
    runtime_path: Option<PathBuf>,
}

#[derive(Args)]
struct UpdateBranchArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Branch to create or reset with generated-source updates.
    #[arg(long, default_value = "tokyo/update-generated-cli")]
    branch: String,
    /// Commit message for generated-source updates.
    #[arg(long, default_value = "Update Tokyo generated CLI")]
    message: String,
    /// Write a Markdown PR summary to this file as well as stdout.
    #[arg(long, value_name = "FILE")]
    summary_file: Option<PathBuf>,
    /// Type-check the generated output and include the result in the summary.
    /// The command exits nonzero when validation fails, after the branch,
    /// summary, and any requested push/PR are complete.
    #[arg(long)]
    validate: bool,
    /// Use a local tokyo-cli-runtime checkout instead of the published crate
    /// when validating.
    #[arg(long, value_name = "DIR")]
    runtime_path: Option<PathBuf>,
    /// Push the branch to this remote after committing.
    #[arg(long, value_name = "REMOTE", num_args = 0..=1, default_missing_value = "origin")]
    push: Option<String>,
    /// Create the GitHub pull request for the branch, or update the existing
    /// open Tokyo PR in place. Requires --push.
    #[arg(long, requires = "push")]
    pr: bool,
}

#[derive(Args)]
struct DiffArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Diff rendering format.
    #[arg(long, value_enum, default_value_t = DiffFormat::Human)]
    format: DiffFormat,
}

#[derive(Clone, Copy, ValueEnum)]
enum DiffFormat {
    Human,
    Json,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Manifest {
    format_version: u32,
    files: Vec<String>,
    /// SHA-256 of each managed file as generated. Files without a recorded
    /// hash (older manifests) skip hand-edit detection; the manifest itself
    /// is never hashed.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    hashes: BTreeMap<String, String>,
}

struct DesiredOutputFiles {
    managed_files_by_relative_path: BTreeMap<String, Vec<u8>>,
    unmanaged_starter_files_by_relative_path: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug)]
struct CliExitError {
    code: i32,
    message: String,
}

impl std::fmt::Display for CliExitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliExitError {}

impl CliExitError {
    fn input(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_INPUT,
            message: message.into(),
        }
    }

    fn output(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_OUTPUT,
            message: message.into(),
        }
    }

    fn differences(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_DIFFERENCES,
            message: message.into(),
        }
    }
}

type AppResult<T> = Result<T>;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProjectSection {
    name: Option<String>,
    routes: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OpenapiSection {
    source: Option<String>,
    snapshot: Option<String>,
    #[serde(default)]
    #[serde(rename = "headers")]
    _headers: BTreeMap<String, String>,
    /// Legacy direct input support. New projects use `snapshot`.
    input: Option<String>,
    output: Option<String>,
}

struct ProjectConfig {
    project: Option<ProjectSection>,
    openapi: Option<OpenapiSection>,
    codegen: Config,
}

#[derive(Debug, Clone)]
struct DiscoveredRoute {
    command_path: Vec<String>,
    source_path: PathBuf,
}

fn input_error(message: impl Into<String>) -> anyhow::Error {
    CliExitError::input(message).into()
}

fn output_error(message: impl Into<String>) -> anyhow::Error {
    CliExitError::output(message).into()
}

fn differences_error(message: impl Into<String>) -> anyhow::Error {
    CliExitError::differences(message).into()
}

fn run_init_command(arguments: InitArgs) -> AppResult<()> {
    validate_project_name(&arguments.name)?;
    let files = scaffold_files(&arguments.name);
    install_scaffold(&arguments.directory, &files)?;
    println!(
        "initialized Tokyo project {} in {}",
        arguments.name,
        arguments.directory.display()
    );
    Ok(())
}

fn run_openapi_command(arguments: OpenapiArgs) -> AppResult<()> {
    let result = match arguments.command {
        OpenapiCommand::Add(arguments) => {
            let changed =
                openapi::add(&arguments.config, &arguments.source).map_err(map_openapi_error)?;
            if changed {
                println!("vendored OpenAPI source in openapi/upstream.json");
            } else {
                println!("vendored OpenAPI source is already up to date");
            }
            return Ok(());
        }
        OpenapiCommand::Sync(arguments) => openapi::sync(&arguments.config),
        OpenapiCommand::Check(arguments) => {
            openapi::check(&arguments.config).map_err(map_openapi_error)?;
            println!("vendored OpenAPI source is up to date");
            return Ok(());
        }
    };
    match result {
        Ok(true) => println!("updated vendored OpenAPI snapshot and tokyo.lock"),
        Ok(false) => println!("vendored OpenAPI source is already up to date"),
        Err(error) => return Err(map_openapi_error(error)),
    }
    Ok(())
}

fn map_openapi_error(error: openapi::Error) -> anyhow::Error {
    match error {
        openapi::Error::Input(message) => input_error(message),
        openapi::Error::Output(message) => output_error(message),
        openapi::Error::Differences(message) => differences_error(message),
    }
}

fn validate_project_name(name: &str) -> AppResult<()> {
    let valid = !name.is_empty()
        && !name.starts_with(|character: char| character.is_ascii_digit())
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(input_error(format!(
            "invalid project name {name:?}; use ASCII letters, digits, '-' or '_', starting with a letter"
        )))
    }
}

fn scaffold_files(name: &str) -> BTreeMap<String, Vec<u8>> {
    let runtime_version = env!("CARGO_PKG_VERSION");
    let files = [
        (".gitignore", "/target\n/.tokyo/bin\n"),
        (
            "Cargo.toml",
            &format!(
                "[workspace]\n\n[package]\nname = {name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nclap = {{ version = \"4.6.1\", features = [\"derive\", \"env\"] }}\nclap_complete = \"4.6.1\"\nchrono = {{ version = \"0.4.42\", features = [\"serde\"] }}\nserde = {{ version = \"1.0.228\", features = [\"derive\"] }}\nserde_json = \"1.0.150\"\ntokyo-cli-runtime = \"={runtime_version}\"\nuuid = {{ version = \"1.18.1\", features = [\"serde\"] }}\n"
            ),
        ),
        (
            "tokyo.toml",
            &format!("[project]\nname = {name:?}\nroutes = \"src/routes\"\n"),
        ),
        (
            "src/main.rs",
            "mod middleware;\nmod routes;\n\nfn main() {\n    eprintln!(\"run `tokyo generate` before building this project\");\n}\n",
        ),
        (
            "src/middleware.rs",
            "//! Application-wide middleware for filesystem routes.\n\nuse tokyo_cli_runtime::prelude::Route;\n\n/// Decorates every route before Tokyo registers or runs it.\npub fn decorate(route: Route) -> Route {\n    route\n    // Example:\n    // .middleware_fn(|context, next| {\n    //     eprintln!(\"running filesystem route\");\n    //     next.run(context)\n    // })\n}\n",
        ),
        ("src/routes/mod.rs", "pub mod index;\n"),
        (
            "src/routes/index.rs",
            "use tokyo_cli_runtime::prelude::*;\n\n/// Defines the default local route.\npub fn route() -> Route {\n    Route::new(RouteSpec::new(\"index\").about(\"Print a greeting\"), |_| {\n        Ok(RouteResponse::text(\"Hello from Tokyo\"))\n    })\n}\n",
        ),
    ];
    let mut files: BTreeMap<String, Vec<u8>> = files
        .into_iter()
        .map(|(path, contents)| (path.to_string(), contents.as_bytes().to_vec()))
        .collect();
    files.extend(
        tokyo_emit_cli::project_skill_starter_files()
            .into_iter()
            .map(|file| (file.relative_path, file.contents.into_bytes())),
    );
    files
}

fn install_scaffold(root: &Path, files: &BTreeMap<String, Vec<u8>>) -> AppResult<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(output_error(format!(
                "refusing to initialize unsafe project directory {}",
                root.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(output_error(format!(
                "cannot inspect project directory {}: {error}",
                root.display()
            )));
        }
    }

    for relative_path in files.keys() {
        validate_generated_relative_path(relative_path)?;
        let target = root.join(relative_path);
        if fs::symlink_metadata(&target).is_ok() {
            return Err(output_error(format!(
                "refusing to overwrite existing path {}",
                target.display()
            )));
        }
        let mut parent = target.parent();
        while let Some(path) = parent {
            if path == root {
                break;
            }
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(output_error(format!(
                        "refusing unsafe scaffold path {}",
                        path.display()
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(output_error(format!(
                        "cannot inspect scaffold path {}: {error}",
                        path.display()
                    )));
                }
            }
            parent = path.parent();
        }
    }

    let mut created_directories = Vec::new();
    let mut created_files = Vec::new();
    let result = (|| -> AppResult<()> {
        if !root.exists() {
            fs::create_dir(root).map_err(|error| {
                output_error(format!("cannot create {}: {error}", root.display()))
            })?;
            created_directories.push(root.to_path_buf());
        }
        for (relative_path, contents) in files {
            let target = root.join(relative_path);
            let mut missing_parents = Vec::new();
            let mut parent = target.parent();
            while let Some(path) = parent {
                if path.exists() {
                    break;
                }
                missing_parents.push(path.to_path_buf());
                parent = path.parent();
            }
            for directory in missing_parents.into_iter().rev() {
                fs::create_dir(&directory).map_err(|error| {
                    output_error(format!("cannot create {}: {error}", directory.display()))
                })?;
                created_directories.push(directory);
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|error| {
                    output_error(format!("cannot create {}: {error}", target.display()))
                })?;
            created_files.push(target.clone());
            file.write_all(contents).map_err(|error| {
                output_error(format!("cannot write {}: {error}", target.display()))
            })?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        for path in created_files.iter().rev() {
            let _ = fs::remove_file(path);
        }
        for path in created_directories.iter().rev() {
            let _ = fs::remove_dir(path);
        }
        return Err(error);
    }
    Ok(())
}

fn exit_code_for_error(error: &anyhow::Error) -> i32 {
    error
        .downcast_ref::<CliExitError>()
        .map_or(EXIT_OUTPUT, |cli_exit_error| cli_exit_error.code)
}

struct CliEmitter;

impl Emitter for CliEmitter {
    type Error = Infallible;

    fn emit_target_files(
        &self,
        api: &Api,
    ) -> Result<Vec<tokyo_codegen_engine::GeneratedFile>, Self::Error> {
        Ok(tokyo_emit_cli::emit_generated_cli_project_files(api))
    }
}

fn main() {
    let normalized_command_line_arguments = backwards_compatible_args();
    let parsed_cli_arguments = match Cli::try_parse_from(normalized_command_line_arguments) {
        Ok(parsed_cli_arguments) => parsed_cli_arguments,
        Err(error) => error.exit(),
    };

    let command_result = match parsed_cli_arguments.command {
        Command::Init(init_command_arguments) => run_init_command(init_command_arguments),
        Command::Generate(generate_command_arguments) => {
            run_generate_or_check_command(generate_command_arguments, false)
        }
        Command::Check(check_command_arguments) => {
            run_generate_or_check_command(check_command_arguments, true)
        }
        Command::Dev(dev_command_arguments) => run_dev_command(dev_command_arguments),
        Command::UpdateBranch(update_branch_command_arguments) => {
            run_update_branch_command(update_branch_command_arguments)
        }
        Command::Diff(diff_command_arguments) => {
            run_api_snapshot_diff_command(diff_command_arguments)
        }
        Command::Openapi(openapi_command_arguments) => {
            run_openapi_command(openapi_command_arguments)
        }
    };
    if let Err(error) = command_result {
        let exit_code = exit_code_for_error(&error);
        eprintln!("error: {error:#}");
        std::process::exit(exit_code);
    }
}

fn backwards_compatible_args() -> Vec<OsString> {
    let mut normalized_command_line_arguments: Vec<OsString> = std::env::args_os().collect();
    let first_user_supplied_argument = normalized_command_line_arguments
        .get(1)
        .and_then(|argument| argument.to_str());
    if first_user_supplied_argument.is_none() {
        normalized_command_line_arguments.push("generate".into());
    } else if !matches!(
        first_user_supplied_argument,
        Some("init" | "generate" | "check" | "dev" | "update-branch" | "diff" | "openapi" | "help",)
    ) && !first_user_supplied_argument.is_some_and(|argument| argument.starts_with('-'))
    {
        normalized_command_line_arguments.insert(1, "generate".into());
        normalized_command_line_arguments.insert(2, "--input".into());
    }
    normalized_command_line_arguments
}

fn run_generate_or_check_command(
    generation_command_arguments: GenerationArgs,
    should_check_without_writing: bool,
) -> AppResult<()> {
    let (codegen_config, output_directory) =
        load_generation_settings_and_output_directory(&generation_command_arguments.common)?;
    let imported_api_ir =
        import_generation_api(&generation_command_arguments.common, &codegen_config)?;
    let routes = discover_configured_routes(
        &generation_command_arguments.common,
        &output_directory,
        &imported_api_ir,
    )?;
    let desired_output_files = build_desired_generated_files_by_relative_path(
        &imported_api_ir,
        &routes,
        &output_directory,
    )?;
    let previous_generated_file_manifest =
        read_previous_generated_file_manifest(&output_directory)?;
    let generated_output_differences = detect_generated_output_differences(
        &output_directory,
        &desired_output_files.managed_files_by_relative_path,
        &previous_generated_file_manifest,
        tokyo_emit_cli::UNMANAGED_STARTER_FILES,
    )?;

    if should_check_without_writing {
        if generated_output_differences.is_empty() {
            println!("generated output is up to date");
            return Ok(());
        }
        for difference in &generated_output_differences {
            eprintln!("out of date: {difference}");
        }
        return Err(differences_error(format!(
            "generated output differs ({} file{})",
            generated_output_differences.len(),
            if generated_output_differences.len() == 1 {
                ""
            } else {
                "s"
            }
        )));
    }

    detect_hand_edited_managed_files(
        &output_directory,
        &previous_generated_file_manifest,
        tokyo_emit_cli::UNMANAGED_STARTER_FILES,
    )?;
    write_generated_output_transactionally(
        &output_directory,
        &desired_output_files.managed_files_by_relative_path,
        &desired_output_files.unmanaged_starter_files_by_relative_path,
        &previous_generated_file_manifest,
        tokyo_emit_cli::UNMANAGED_STARTER_FILES,
    )?;
    let desired_file_count = desired_output_files.managed_files_by_relative_path.len()
        + desired_output_files
            .unmanaged_starter_files_by_relative_path
            .len();
    println!(
        "generated {} files in {}",
        desired_file_count,
        output_directory.display()
    );
    Ok(())
}

/// `tokyo dev`: the framework's edit loop. Watches generation inputs (spec,
/// config, scenario files) and the generated CLI's `src/` for OS-level file
/// events (via `notify`, debounced); a spec or config change regenerates
/// managed files and rebuilds, a source change rebuilds only. Every
/// successful build refreshes a stable path at `.tokyo/bin/<name>` so the
/// generated CLI can always be invoked at the same location without going
/// through `cargo` — see [`refresh_stable_binary_path`].
/// Regeneration/build failures (mid-edit YAML, hand-edited managed files,
/// compile errors) are reported and the loop keeps watching.
fn run_dev_command(dev_command_arguments: DevArgs) -> AppResult<()> {
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
        // (managed `src/tokyo/**` files, `.tokyo/bin/<name>`). Drain the
        // events those writes themselves produce so they don't immediately
        // trigger a second, redundant rebuild.
        while event_rx.recv_timeout(Duration::from_millis(400)).is_ok() {}
    }
    Ok(())
}

/// Filters out editor swap/backup artifacts (`.foo.swp`, `foo~`,
/// `.#foo`, atomic-save temp files like `.!1234!foo.rs`) that would
/// otherwise register as spurious source changes.
fn is_relevant_watch_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    !file_name.starts_with('.') && !file_name.ends_with('~')
}

fn dev_regenerate_and_build(
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

fn dev_build_only(output_directory: &Path, runtime_path: Option<&Path>) {
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

fn dev_change_requires_regeneration(
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
fn build_generated_cli_binary(
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

fn stable_binary_directory(output_directory: &Path) -> PathBuf {
    output_directory.join(".tokyo").join("bin")
}

/// Publishes `executable_path` at a fixed location, `.tokyo/bin/<file name>`,
/// so the generated CLI has one address that never changes across rebuilds —
/// nothing to add to `PATH`, nothing to look up in `target/debug/`. A plain
/// symlink is used on Unix; Windows falls back to a hard link, then a copy,
/// since symlinks there require developer mode or elevation.
fn refresh_stable_binary_path(
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

fn run_update_branch_command(update_branch_arguments: UpdateBranchArgs) -> AppResult<()> {
    if update_branch_arguments.branch.trim().is_empty() {
        return Err(input_error("--branch must not be empty"));
    }
    let (codegen_config, output_directory) =
        load_generation_settings_and_output_directory(&update_branch_arguments.common)?;
    let imported_api_ir =
        import_openapi_input_as_api_ir(&update_branch_arguments.common, &codegen_config)?;
    let routes = discover_configured_routes(
        &update_branch_arguments.common,
        &output_directory,
        &imported_api_ir,
    )?;
    let desired_output_files = build_desired_generated_files_by_relative_path(
        &imported_api_ir,
        &routes,
        &output_directory,
    )?;
    let previous_generated_file_manifest =
        read_previous_generated_file_manifest(&output_directory)?;
    let api_snapshot_changes =
        diff_previous_api_snapshot_with_current_api(&output_directory, &imported_api_ir)?;
    let generated_output_differences = detect_generated_output_differences(
        &output_directory,
        &desired_output_files.managed_files_by_relative_path,
        &previous_generated_file_manifest,
        tokyo_emit_cli::UNMANAGED_STARTER_FILES,
    )?;
    ensure_git_worktree_is_clean(&output_directory)?;
    run_git(
        &output_directory,
        &["checkout", "-B", update_branch_arguments.branch.as_str()],
    )?;

    write_generated_output_transactionally(
        &output_directory,
        &desired_output_files.managed_files_by_relative_path,
        &BTreeMap::new(),
        &previous_generated_file_manifest,
        tokyo_emit_cli::UNMANAGED_STARTER_FILES,
    )?;

    let validation = if update_branch_arguments.validate {
        Some(validate_generated_output(
            &output_directory,
            update_branch_arguments.runtime_path.as_deref(),
        ))
    } else {
        None
    };
    let validation_status = match &validation {
        Some(Ok(())) => "`cargo check` passed for the generated output.".to_string(),
        Some(Err(check_output)) => format!(
            "**`cargo check` failed.** The generated update needs app-owned changes \
             (for example in `src/routes/**` or `src/middleware.rs`) before it can merge; Tokyo \
             never edits app-owned files itself.\n\n```text\n{check_output}\n```",
        ),
        None => "Not run by update-branch; pass --validate to include results.".to_string(),
    };
    let pr_summary = render_generated_source_pr_summary(
        &api_snapshot_changes,
        &generated_output_differences,
        &validation_status,
    );

    let managed_paths_to_stage = managed_paths_affected_by_generation(
        &desired_output_files.managed_files_by_relative_path,
        &previous_generated_file_manifest,
        tokyo_emit_cli::UNMANAGED_STARTER_FILES,
    );
    stage_generated_paths(&output_directory, &managed_paths_to_stage)?;

    if git_has_staged_changes(&output_directory)? {
        run_git_with_configured_identity(
            &output_directory,
            &["commit", "-m", update_branch_arguments.message.as_str()],
        )?;
        println!(
            "committed generated-source updates on branch {}",
            update_branch_arguments.branch
        );
    } else {
        println!(
            "branch {} has no generated-source changes",
            update_branch_arguments.branch
        );
    }
    emit_generated_source_pr_summary(update_branch_arguments.summary_file.as_deref(), &pr_summary)?;

    if let Some(remote) = &update_branch_arguments.push {
        run_git(
            &output_directory,
            &[
                "push",
                "--force",
                remote.as_str(),
                update_branch_arguments.branch.as_str(),
            ],
        )?;
        println!(
            "pushed branch {} to {remote}",
            update_branch_arguments.branch
        );
        if update_branch_arguments.pr {
            create_or_update_generated_source_pull_request(
                &output_directory,
                &update_branch_arguments.branch,
                &update_branch_arguments.message,
                &pr_summary,
            )?;
        }
    }

    if matches!(validation, Some(Err(_))) {
        return Err(differences_error(
            "validation failed for the generated update; see the summary for the migration work needed",
        ));
    }
    Ok(())
}

/// Type-checks the freshly written generated output. Returns the failing
/// `cargo check` output on error so the PR summary can explain the required
/// app-owned migration.
fn validate_generated_output(
    output_directory: &Path,
    runtime_path: Option<&Path>,
) -> Result<(), String> {
    let cargo = std::env::var_os("TOKYO_CODEGEN_CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut check = std::process::Command::new(cargo);
    if let Some(runtime_path) = runtime_path {
        check.arg("--config").arg(format!(
            "patch.crates-io.tokyo-cli-runtime.path={runtime_path:?}"
        ));
    }
    let output = check
        .args(["check", "--quiet"])
        .current_dir(output_directory)
        .output()
        .map_err(|error| format!("cannot run cargo check: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<&str> = stderr.lines().rev().take(40).collect();
    Err(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
}

/// Creates the GitHub PR for `branch`, or updates the body of the existing
/// open PR so repeated spec updates converge on one reviewable Tokyo PR. Uses
/// the `gh` CLI; override the executable with `TOKYO_CODEGEN_GH` (tests use a
/// stub).
fn create_or_update_generated_source_pull_request(
    output_directory: &Path,
    branch: &str,
    title: &str,
    summary: &str,
) -> AppResult<()> {
    let gh = std::env::var_os("TOKYO_CODEGEN_GH").unwrap_or_else(|| OsString::from("gh"));
    let summary_file = output_directory.join(".tokyo-pr-summary.tmp.md");
    fs::write(&summary_file, summary).map_err(|error| {
        output_error(format!(
            "cannot write PR body {}: {error}",
            summary_file.display()
        ))
    })?;
    let existing = std::process::Command::new(&gh)
        .current_dir(output_directory)
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "open",
            "--json",
            "number",
            "--jq",
            ".[0].number",
        ])
        .output()
        .map_err(|error| output_error(format!("cannot run gh: {error}")))?;
    if !existing.status.success() {
        let _ = fs::remove_file(&summary_file);
        return Err(output_error(format!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&existing.stderr)
        )));
    }
    let existing_number = String::from_utf8_lossy(&existing.stdout).trim().to_string();
    let body_path = summary_file.display().to_string();
    let gh_arguments: Vec<String> = if existing_number.is_empty() {
        vec![
            "pr".into(),
            "create".into(),
            "--head".into(),
            branch.into(),
            "--title".into(),
            title.into(),
            "--body-file".into(),
            body_path,
        ]
    } else {
        vec![
            "pr".into(),
            "edit".into(),
            existing_number.clone(),
            "--title".into(),
            title.into(),
            "--body-file".into(),
            body_path,
        ]
    };
    let result = std::process::Command::new(&gh)
        .current_dir(output_directory)
        .args(&gh_arguments)
        .status();
    let _ = fs::remove_file(&summary_file);
    let status = result.map_err(|error| output_error(format!("cannot run gh: {error}")))?;
    if !status.success() {
        return Err(output_error("gh pull-request creation/update failed"));
    }
    if existing_number.is_empty() {
        println!("created pull request for branch {branch}");
    } else {
        println!("updated pull request #{existing_number} for branch {branch}");
    }
    Ok(())
}

fn run_api_snapshot_diff_command(diff_command_arguments: DiffArgs) -> AppResult<()> {
    let (codegen_config, output_directory) =
        load_generation_settings_and_output_directory(&diff_command_arguments.common)?;
    let current_api_ir =
        import_openapi_input_as_api_ir(&diff_command_arguments.common, &codegen_config)?;
    let api_snapshot_path = resolve_api_snapshot_path(&output_directory);
    let api_snapshot_json = fs::read_to_string(&api_snapshot_path).map_err(|error| {
        input_error(format!(
            "cannot read IR snapshot {}: {error}; run generate first",
            api_snapshot_path.display()
        ))
    })?;
    let api_snapshot: Snapshot = serde_json::from_str(&api_snapshot_json).map_err(|error| {
        input_error(format!(
            "invalid IR snapshot {}: {error}",
            api_snapshot_path.display()
        ))
    })?;
    if api_snapshot.format_version != FORMAT_VERSION {
        return Err(input_error(format!(
            "unsupported IR snapshot format {} (expected {})",
            api_snapshot.format_version, FORMAT_VERSION
        )));
    }
    if !api_snapshot.api.has_supported_schema_version() {
        return Err(input_error(format!(
            "unsupported IR schema version {} in {} (expected {})",
            api_snapshot.api.schema_version,
            api_snapshot_path.display(),
            tokyo_ir::IR_SCHEMA_VERSION
        )));
    }

    let api_snapshot_changes =
        tokyo_ir::diff::diff_api_snapshots(&api_snapshot.api, &current_api_ir);
    match diff_command_arguments.format {
        DiffFormat::Human => print_human_readable_api_diff(&api_snapshot_changes),
        DiffFormat::Json => {
            let api_changes_json =
                serde_json::to_string_pretty(&api_snapshot_changes).map_err(json_output_error)?;
            println!("{api_changes_json}");
        }
    }
    if api_snapshot_changes.is_empty() {
        Ok(())
    } else {
        Err(differences_error(format!(
            "{} CLI/API change{} detected",
            api_snapshot_changes.len(),
            if api_snapshot_changes.len() == 1 {
                ""
            } else {
                "s"
            }
        )))
    }
}

fn load_generation_settings_and_output_directory(
    common_command_arguments: &CommonArgs,
) -> AppResult<(Config, PathBuf)> {
    let config_path = configured_or_default_config_path(common_command_arguments);
    let parsed_config = if let Some(ref config_path) = config_path {
        read_project_config(config_path)?
    } else {
        ProjectConfig {
            project: None,
            openapi: None,
            codegen: Config::default(),
        }
    };
    let is_route_project = parsed_config
        .project
        .as_ref()
        .and_then(|project| project.routes.as_ref())
        .is_some();
    let has_configured_openapi_output = parsed_config
        .openapi
        .as_ref()
        .and_then(|openapi| openapi.output.as_ref())
        .is_some();
    let mut codegen_config = parsed_config.codegen;
    if codegen_config.package.is_none() {
        codegen_config.package = parsed_config
            .project
            .as_ref()
            .and_then(|project| project.name.clone());
    }
    if codegen_config.cli_name.is_none() {
        codegen_config.cli_name = parsed_config
            .project
            .as_ref()
            .and_then(|project| project.name.clone());
    }
    if let Some(config_path) = config_path.as_deref() {
        resolve_scenario_files(&mut codegen_config, config_path)?;
    }
    let configured_output = codegen_config.output.clone();
    let output_directory = common_command_arguments
        .output
        .clone()
        .or_else(|| {
            parsed_config
                .openapi
                .as_ref()
                .and_then(|openapi| openapi.output.as_deref())
                .map(|output| path_relative_to_config(config_path.as_deref(), output))
        })
        .or_else(|| configured_output.map(PathBuf::from))
        .unwrap_or_else(|| {
            if is_route_project && !has_configured_openapi_output {
                config_path
                    .as_deref()
                    .and_then(Path::parent)
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf()
            } else {
                PathBuf::from(DEFAULT_OUTPUT)
            }
        });
    Ok((codegen_config, output_directory))
}

fn configured_or_default_config_path(common_command_arguments: &CommonArgs) -> Option<PathBuf> {
    if let Some(path) = &common_command_arguments.config {
        return Some(path.clone());
    }
    let mut directory = std::env::current_dir().ok()?;
    loop {
        let candidate = directory.join(DEFAULT_CONFIG);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !directory.pop() {
            return None;
        }
    }
}

fn read_project_config(path: &Path) -> AppResult<ProjectConfig> {
    let config_toml = fs::read_to_string(path)
        .map_err(|error| input_error(format!("cannot read config {}: {error}", path.display())))?;
    let mut root: toml::Value = toml::from_str(&config_toml)
        .map_err(|error| input_error(format!("invalid config {}: {error}", path.display())))?;
    let table = root.as_table_mut().ok_or_else(|| {
        input_error(format!(
            "invalid config {}: expected a TOML table",
            path.display()
        ))
    })?;
    let project = table
        .remove("project")
        .map(toml::Value::try_into)
        .transpose()
        .map_err(|error| {
            input_error(format!(
                "invalid [project] config in {}: {error}",
                path.display()
            ))
        })?;
    let openapi = table
        .remove("openapi")
        .map(toml::Value::try_into)
        .transpose()
        .map_err(|error| {
            input_error(format!(
                "invalid [openapi] config in {}: {error}",
                path.display()
            ))
        })?;
    let codegen = root.try_into().map_err(|error| {
        input_error(format!("invalid legacy config {}: {error}", path.display()))
    })?;
    if let Some(ProjectSection {
        routes: Some(routes),
        ..
    }) = &project
        && (Path::new(routes).is_absolute()
            || Path::new(routes)
                .components()
                .any(|component| !matches!(component, Component::Normal(_))))
    {
        return Err(input_error(format!(
            "invalid [project].routes in {}: expected a relative path without '..'",
            path.display()
        )));
    }
    Ok(ProjectConfig {
        project,
        openapi,
        codegen,
    })
}

fn path_relative_to_config(config_path: Option<&Path>, configured_path: &str) -> PathBuf {
    config_path
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .join(configured_path)
}

fn resolve_openapi_input_path(common_command_arguments: &CommonArgs) -> AppResult<PathBuf> {
    resolve_optional_openapi_input_path(common_command_arguments)?
        .ok_or_else(|| input_error("this command requires an [openapi].input or --input"))
}

fn resolve_optional_openapi_input_path(
    common_command_arguments: &CommonArgs,
) -> AppResult<Option<PathBuf>> {
    if let Some(input) = &common_command_arguments.input {
        return Ok(Some(input.clone()));
    }
    let config_path = configured_or_default_config_path(common_command_arguments);
    if let Some(config_path) = &config_path {
        let parsed = read_project_config(config_path)?;
        if let Some(openapi) = parsed.openapi {
            if let Some(snapshot) = openapi.snapshot {
                return Ok(Some(path_relative_to_config(Some(config_path), &snapshot)));
            }
            if let Some(input) = openapi.input {
                return Ok(Some(path_relative_to_config(Some(config_path), &input)));
            }
            if openapi.source.is_some() {
                return Err(input_error(format!(
                    "[openapi].source in {} requires [openapi].snapshot; run `tokyo openapi sync` after repairing the config",
                    config_path.display()
                )));
            }
        }
        if parsed
            .project
            .as_ref()
            .and_then(|project| project.routes.as_ref())
            .is_some()
        {
            return Ok(None);
        }
    }
    Ok(Some(PathBuf::from(DEFAULT_INPUT)))
}

fn resolve_configured_routes_directory(
    common_command_arguments: &CommonArgs,
) -> AppResult<Option<PathBuf>> {
    let Some(config_path) = configured_or_default_config_path(common_command_arguments) else {
        return Ok(None);
    };
    let parsed = read_project_config(&config_path)?;
    Ok(parsed
        .project
        .and_then(|project| project.routes)
        .map(|routes| path_relative_to_config(Some(&config_path), &routes)))
}

fn resolve_scenario_files(codegen_config: &mut Config, config_path: &Path) -> AppResult<()> {
    let config_parent_directory = config_path.parent().unwrap_or_else(|| Path::new("."));
    for configured_cli_scenario in &mut codegen_config.cli_scenarios {
        let Some(relative_scenario_file_path) = configured_cli_scenario.file.as_deref() else {
            continue;
        };
        if configured_cli_scenario.body.is_some() {
            return Err(input_error(format!(
                "cli_scenarios {:?} must set exactly one of body or file",
                configured_cli_scenario.name
            )));
        }
        let scenario_file_path = config_parent_directory.join(relative_scenario_file_path);
        configured_cli_scenario.body =
            Some(fs::read_to_string(&scenario_file_path).map_err(|error| {
                input_error(format!(
                    "cannot read cli_scenarios {:?} file {}: {error}",
                    configured_cli_scenario.name,
                    scenario_file_path.display()
                ))
            })?);
        configured_cli_scenario.file = None;
    }
    Ok(())
}

fn import_openapi_input_as_api_ir(
    common_command_arguments: &CommonArgs,
    codegen_config: &Config,
) -> AppResult<Api> {
    let openapi_input_path = resolve_openapi_input_path(common_command_arguments)?;
    let openapi_input_text = fs::read_to_string(&openapi_input_path).map_err(|error| {
        input_error(format!(
            "cannot read OpenAPI input {}: {error}",
            openapi_input_path.display()
        ))
    })?;
    tokyo_codegen_engine::import_openapi_text(
        &openapi_input_text,
        InputFormat::Auto,
        codegen_config,
    )
    .map_err(|error| {
        let import_error_message = match error {
            tokyo_codegen_engine::Error::Import(message) => {
                format!(
                    "cannot import OpenAPI input {}: {message}",
                    openapi_input_path.display()
                )
            }
            other => other.to_string(),
        };
        input_error(import_error_message)
    })
}

fn import_generation_api(
    common_command_arguments: &CommonArgs,
    codegen_config: &Config,
) -> AppResult<Api> {
    if resolve_optional_openapi_input_path(common_command_arguments)?.is_some() {
        import_openapi_input_as_api_ir(common_command_arguments, codegen_config)
    } else {
        let mut api = Api::default();
        tokyo_codegen_engine::apply_codegen_config_to_api(&mut api, codegen_config)
            .map_err(engine_output_error)?;
        api.canonicalize();
        Ok(api)
    }
}

fn normalize_command_component(identifier: &str) -> String {
    identifier.to_kebab_case()
}

fn is_valid_route_identifier(identifier: &str) -> bool {
    let mut characters = identifier.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn discover_configured_routes(
    common: &CommonArgs,
    _output_directory: &Path,
    api: &Api,
) -> AppResult<Vec<DiscoveredRoute>> {
    let Some(routes_directory) = resolve_configured_routes_directory(common)? else {
        return Ok(Vec::new());
    };
    if !routes_directory.is_dir() {
        if !api.endpoints.is_empty() {
            return Ok(Vec::new());
        }
        return Err(input_error(format!(
            "configured routes directory {} does not exist",
            routes_directory.display()
        )));
    }

    fn visit(directory: &Path, files: &mut Vec<PathBuf>) -> AppResult<()> {
        let mut entries = fs::read_dir(directory)
            .map_err(|error| input_error(format!("cannot read {}: {error}", directory.display())))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                input_error(format!("cannot read {}: {error}", directory.display()))
            })?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let file_type = entry.file_type().map_err(|error| {
                input_error(format!(
                    "cannot inspect {}: {error}",
                    entry.path().display()
                ))
            })?;
            if file_type.is_symlink() {
                return Err(input_error(format!(
                    "route source paths must not be symlinks: {}",
                    entry.path().display()
                )));
            }
            if file_type.is_dir() {
                visit(&entry.path(), files)?;
            } else if file_type.is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    == Some("rs")
            {
                files.push(entry.path());
            }
        }
        Ok(())
    }

    let mut source_files = Vec::new();
    visit(&routes_directory, &mut source_files)?;
    let relative_files = source_files
        .iter()
        .map(|path| {
            path.strip_prefix(&routes_directory)
                .expect("visited child")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    let relative_set = relative_files.iter().cloned().collect::<BTreeSet<_>>();
    for relative in &relative_files {
        if relative.file_name().and_then(|name| name.to_str()) == Some("mod.rs") {
            let module_file = relative
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .with_extension("rs");
            if !module_file.as_os_str().is_empty() && relative_set.contains(&module_file) {
                return Err(input_error(format!(
                    "ambiguous route module layout: {} conflicts with {}",
                    routes_directory.join(&module_file).display(),
                    routes_directory.join(relative).display()
                )));
            }
        }
    }

    let mut reserved = [
        "achieve",
        "api",
        "auth",
        "profile",
        "env",
        "start",
        "schema",
        "completions",
        "run",
        "reset",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    for endpoint in &api.endpoints {
        if endpoint.tags.is_empty() {
            reserved.insert("default".to_string());
        } else {
            reserved.extend(
                endpoint
                    .tags
                    .iter()
                    .map(|tag| normalize_generated_command_name(tag)),
            );
        }
    }
    reserved.extend(
        api.cli
            .cli_dispatch_groups
            .iter()
            .map(|group| normalize_generated_command_name(&group.resource)),
    );

    let mut discovered = Vec::new();
    let mut normalized_paths = BTreeMap::<Vec<String>, PathBuf>::new();
    for (source_path, relative) in source_files.into_iter().zip(relative_files) {
        if relative.file_name().and_then(|name| name.to_str()) == Some("mod.rs") {
            continue;
        }
        let mut identifiers = Vec::new();
        for component in relative.parent().into_iter().flat_map(Path::components) {
            let Component::Normal(value) = component else {
                return Err(input_error(format!(
                    "invalid route path {}",
                    relative.display()
                )));
            };
            let identifier = value.to_str().ok_or_else(|| {
                input_error(format!("route path is not UTF-8: {}", relative.display()))
            })?;
            identifiers.push(identifier.to_string());
        }
        let stem = relative
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                input_error(format!("route path is not UTF-8: {}", relative.display()))
            })?;
        identifiers.push(stem.to_string());
        if let Some(invalid) = identifiers
            .iter()
            .find(|identifier| !is_valid_route_identifier(identifier))
        {
            return Err(input_error(format!(
                "invalid route identifier {invalid:?} in {}; use Rust identifiers in route paths",
                relative.display()
            )));
        }
        let command_path = identifiers
            .iter()
            .map(|identifier| normalize_command_component(identifier))
            .collect::<Vec<_>>();
        if reserved.contains(&command_path[0]) {
            return Err(input_error(format!(
                "route {} conflicts with reserved or generated top-level command {:?}",
                relative.display(),
                command_path[0]
            )));
        }
        if let Some(previous) =
            normalized_paths.insert(command_path.clone(), relative.to_path_buf())
        {
            return Err(input_error(format!(
                "duplicate normalized route command path {}: {} and {}",
                command_path.join(" "),
                previous.display(),
                relative.display()
            )));
        }
        discovered.push(DiscoveredRoute {
            command_path,
            source_path,
        });
    }
    for left in 0..discovered.len() {
        for right in 0..discovered.len() {
            if left != right
                && discovered[right]
                    .command_path
                    .starts_with(&discovered[left].command_path)
            {
                return Err(input_error(format!(
                    "route command path {} is both a command and a command group",
                    discovered[left].command_path.join(" ")
                )));
            }
        }
    }
    discovered.sort_by(|left, right| left.command_path.cmp(&right.command_path));
    Ok(discovered)
}

fn normalize_generated_command_name(value: &str) -> String {
    value.to_kebab_case()
}

#[derive(Default)]
struct RouteTree {
    route_index: Option<usize>,
    children: BTreeMap<String, RouteTree>,
}

fn render_route_registry(routes: &[DiscoveredRoute], output_directory: &Path) -> AppResult<String> {
    let mut tree = RouteTree::default();
    for (index, route) in routes.iter().enumerate() {
        let mut node = &mut tree;
        for component in &route.command_path {
            node = node.children.entry(component.clone()).or_default();
        }
        node.route_index = Some(index);
    }
    fn command_expression(name: &str, node: &RouteTree) -> String {
        if let Some(index) = node.route_index {
            return format!(
                "{{ let route = route_{index}(); route.spec().command().name({name:?}) }}"
            );
        }
        let mut expression = format!("clap::Command::new({name:?})");
        for (child_name, child) in &node.children {
            expression.push_str(&format!(
                ".subcommand({})",
                command_expression(child_name, child)
            ));
        }
        expression
    }

    let registry_directory = output_directory.join("src/tokyo");
    let mut source = String::from(
        "// Code generated by tokyo-codegen. DO NOT EDIT BY HAND.\n\
         // Route bodies remain developer-owned under the configured routes directory.\n\n",
    );
    for (index, route) in routes.iter().enumerate() {
        let module_path = relative_path_from(&registry_directory, &route.source_path)?;
        source.push_str(&format!(
            "#[path = {:?}]\nmod __route_{index};\n",
            module_path.to_string_lossy()
        ));
    }
    for index in 0..routes.len() {
        source.push_str(&format!(
            "\nfn route_{index}() -> tokyo_cli_runtime::route::Route {{\n    crate::middleware::decorate(__route_{index}::route())\n}}\n"
        ));
    }
    source.push_str("\npub fn augment(mut command: clap::Command) -> clap::Command {\n");
    for (name, node) in &tree.children {
        source.push_str(&format!(
            "    command = command.subcommand({});\n",
            command_expression(name, node)
        ));
    }
    source.push_str("    command\n}\n\n");
    source.push_str(
        "pub fn dispatch(\n    matches: &clap::ArgMatches,\n    context: &crate::cli::CommandContext<'_>,\n) -> Result<bool, crate::error::ClientError> {\n",
    );
    for (index, route) in routes.iter().enumerate() {
        let mut indent = String::from("    ");
        let mut matches_name = String::from("matches");
        for (depth, component) in route.command_path.iter().enumerate() {
            let next = format!("matches_{index}_{depth}");
            source.push_str(&format!(
                "{indent}if let Some(({component:?}, {next})) = {matches_name}.subcommand() {{\n"
            ));
            indent.push_str("    ");
            matches_name = next;
        }
        source.push_str(&format!(
            "{indent}let route = route_{index}();\n\
             {indent}let response = route.run_matches({matches_name}, context.client_optional(), context.output)?;\n\
             {indent}response.render(context.output)?;\n\
             {indent}return Ok(true);\n"
        ));
        for _ in &route.command_path {
            indent.truncate(indent.len() - 4);
            source.push_str(&format!("{indent}}}\n"));
        }
    }
    source.push_str("    Ok(false)\n}\n\n");
    source.push_str("pub fn metadata() -> serde_json::Value {\n    serde_json::json!([\n");
    for (index, route) in routes.iter().enumerate() {
        source.push_str(&format!(
            "        ({{ let route = route_{index}(); serde_json::json!({{\"command\": {:?}, \"name\": route.spec().name(), \"about\": route.spec().description(), \"arguments\": route.spec().arguments().iter().map(|argument| argument.name()).collect::<Vec<_>>()}}) }}),\n",
            route.command_path.join(".")
        ));
    }
    source.push_str("    ])\n}\n");
    Ok(source)
}

fn relative_path_from(from_directory: &Path, target: &Path) -> AppResult<PathBuf> {
    let current = std::env::current_dir()
        .map_err(|error| input_error(format!("cannot resolve current directory: {error}")))?;
    let absolute_from = if from_directory.is_absolute() {
        from_directory.to_path_buf()
    } else {
        current.join(from_directory)
    };
    let absolute_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        current.join(target)
    };
    let from_components = absolute_from.components().collect::<Vec<_>>();
    let target_components = absolute_target.components().collect::<Vec<_>>();
    let common = from_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return Err(input_error(format!(
            "cannot express route path {} relative to {}",
            target.display(),
            from_directory.display()
        )));
    }
    let mut relative = PathBuf::new();
    for _ in common..from_components.len() {
        relative.push("..");
    }
    for component in &target_components[common..] {
        relative.push(component.as_os_str());
    }
    Ok(relative)
}

fn build_desired_generated_files_by_relative_path(
    api_ir: &Api,
    routes: &[DiscoveredRoute],
    output_directory: &Path,
) -> AppResult<DesiredOutputFiles> {
    let generator_version = env!("CARGO_PKG_VERSION");
    let generated_output_plan =
        tokyo_codegen_engine::build_output_plan(api_ir, &CliEmitter, generator_version)
            .map_err(engine_output_error)?;
    let unmanaged_starter_files = tokyo_emit_cli::UNMANAGED_STARTER_FILES;
    let mut generated_files_by_relative_path = BTreeMap::new();
    let mut unmanaged_starter_files_by_relative_path = BTreeMap::new();
    for generated_file in generated_output_plan {
        validate_generated_relative_path(&generated_file.relative_path)?;
        if unmanaged_starter_files.contains(&generated_file.relative_path.as_str()) {
            unmanaged_starter_files_by_relative_path.insert(
                generated_file.relative_path,
                generated_file.contents.into_bytes(),
            );
        } else {
            generated_files_by_relative_path.insert(
                generated_file.relative_path,
                generated_file.contents.into_bytes(),
            );
        }
    }
    generated_files_by_relative_path.insert(
        "src/tokyo/routes.rs".to_string(),
        render_route_registry(routes, output_directory)?.into_bytes(),
    );

    let mut generated_manifest_file_entries: Vec<String> =
        generated_files_by_relative_path.keys().cloned().collect();
    generated_manifest_file_entries.push(MANIFEST_FILE.to_string());
    generated_manifest_file_entries.sort();
    let generated_file_hashes = generated_files_by_relative_path
        .iter()
        .map(|(relative_path, contents)| (relative_path.clone(), sha256_hex(contents)))
        .collect();
    let generated_file_manifest = Manifest {
        format_version: FORMAT_VERSION,
        files: generated_manifest_file_entries,
        hashes: generated_file_hashes,
    };
    let mut manifest_json =
        serde_json::to_string_pretty(&generated_file_manifest).map_err(json_output_error)?;
    manifest_json.push('\n');
    generated_files_by_relative_path.insert(MANIFEST_FILE.to_string(), manifest_json.into_bytes());
    Ok(DesiredOutputFiles {
        managed_files_by_relative_path: generated_files_by_relative_path,
        unmanaged_starter_files_by_relative_path,
    })
}

fn is_unmanaged_starter_file(
    unmanaged_starter_files: &[&str],
    generated_relative_path: &str,
) -> bool {
    unmanaged_starter_files.contains(&generated_relative_path)
}

fn sha256_hex(contents: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(contents);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Fails when a manifest-listed managed file was edited by hand since the
/// last generation, so regeneration never silently erases local changes.
/// Files without a recorded hash (older manifests) and missing files are
/// skipped; missing managed files are simply recreated.
fn detect_hand_edited_managed_files(
    output_directory: &Path,
    previous_generated_file_manifest: &Manifest,
    unmanaged_starter_files: &[&str],
) -> AppResult<()> {
    let mut hand_edited_relative_paths = Vec::new();
    for (relative_path, recorded_hash) in &previous_generated_file_manifest.hashes {
        if is_unmanaged_starter_file(unmanaged_starter_files, relative_path)
            || relative_path == MANIFEST_FILE
        {
            continue;
        }
        match fs::read(output_directory.join(relative_path)) {
            Ok(contents) if &sha256_hex(&contents) != recorded_hash => {
                hand_edited_relative_paths.push(relative_path.clone());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(output_error(format!(
                    "cannot read managed file {}: {error}",
                    output_directory.join(relative_path).display()
                )));
            }
        }
    }
    if hand_edited_relative_paths.is_empty() {
        return Ok(());
    }
    Err(output_error(format!(
        "managed files were edited by hand since the last generation: {}\nrevert the edits (or delete the files to have them recreated), then rerun; hand-written code belongs in developer-owned files like src/routes/**, src/middleware.rs, src/commands/guidance.rs, and src/presentation.rs",
        hand_edited_relative_paths.join(", ")
    )))
}

fn read_previous_generated_file_manifest(output_directory: &Path) -> AppResult<Manifest> {
    let mut generated_file_manifest_path = output_directory.join(MANIFEST_FILE);
    if !generated_file_manifest_path.exists() {
        let legacy_manifest_path = output_directory.join(LEGACY_MANIFEST_FILE);
        if legacy_manifest_path.exists() {
            generated_file_manifest_path = legacy_manifest_path;
        }
    }
    match fs::read_to_string(&generated_file_manifest_path) {
        Ok(generated_file_manifest_json) => {
            let generated_file_manifest: Manifest =
                serde_json::from_str(&generated_file_manifest_json).map_err(|error| {
                    output_error(format!(
                        "invalid generated-file manifest {}: {error}",
                        generated_file_manifest_path.display()
                    ))
                })?;
            if generated_file_manifest.format_version != FORMAT_VERSION {
                return Err(output_error(format!(
                    "unsupported manifest format {} in {}",
                    generated_file_manifest.format_version,
                    generated_file_manifest_path.display()
                )));
            }
            for generated_relative_path in &generated_file_manifest.files {
                validate_generated_relative_path(generated_relative_path)?;
            }
            Ok(generated_file_manifest)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(error) => Err(output_error(format!(
            "cannot read manifest {}: {error}",
            generated_file_manifest_path.display()
        ))),
    }
}

fn detect_generated_output_differences(
    output_directory: &Path,
    desired_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    previous_generated_file_manifest: &Manifest,
    unmanaged_starter_files: &[&str],
) -> AppResult<Vec<String>> {
    let mut generated_output_differences = Vec::new();
    for (generated_relative_path, desired_file_contents) in desired_files_by_relative_path {
        match fs::read(output_directory.join(generated_relative_path)) {
            Ok(existing_file_contents) if existing_file_contents == *desired_file_contents => {}
            Ok(_) => {
                generated_output_differences.push(format!("modified {generated_relative_path}"))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                generated_output_differences.push(format!("missing {generated_relative_path}"));
            }
            Err(error) => {
                return Err(output_error(format!(
                    "cannot inspect {}: {error}",
                    output_directory.join(generated_relative_path).display()
                )));
            }
        }
    }
    for previously_generated_relative_path in &previous_generated_file_manifest.files {
        if is_unmanaged_starter_file(unmanaged_starter_files, previously_generated_relative_path) {
            continue;
        }
        if !desired_files_by_relative_path.contains_key(previously_generated_relative_path)
            && output_directory
                .join(previously_generated_relative_path)
                .exists()
        {
            generated_output_differences
                .push(format!("stale {previously_generated_relative_path}"));
        }
    }
    Ok(generated_output_differences)
}

/// Resolves the IR snapshot path, preferring `.tokyo/ir.json` and falling
/// back to the legacy top-level filename for projects generated by earlier
/// releases.
fn resolve_api_snapshot_path(output_directory: &Path) -> PathBuf {
    let current = output_directory.join(SNAPSHOT_FILE);
    if current.exists() {
        return current;
    }
    let legacy = output_directory.join(tokyo_codegen_engine::LEGACY_SNAPSHOT_FILE);
    if legacy.exists() { legacy } else { current }
}

fn diff_previous_api_snapshot_with_current_api(
    output_directory: &Path,
    current_api_ir: &Api,
) -> AppResult<Option<Vec<Change>>> {
    let api_snapshot_path = resolve_api_snapshot_path(output_directory);
    match fs::read_to_string(&api_snapshot_path) {
        Ok(api_snapshot_json) => {
            let api_snapshot: Snapshot =
                serde_json::from_str(&api_snapshot_json).map_err(|error| {
                    input_error(format!(
                        "invalid IR snapshot {}: {error}",
                        api_snapshot_path.display()
                    ))
                })?;
            if api_snapshot.format_version != FORMAT_VERSION {
                return Err(input_error(format!(
                    "unsupported IR snapshot format {} in {}",
                    api_snapshot.format_version,
                    api_snapshot_path.display()
                )));
            }
            if !api_snapshot.api.has_supported_schema_version() {
                return Err(input_error(format!(
                    "unsupported IR schema version {} in {} (expected {})",
                    api_snapshot.api.schema_version,
                    api_snapshot_path.display(),
                    tokyo_ir::IR_SCHEMA_VERSION
                )));
            }
            Ok(Some(tokyo_ir::diff::diff_api_snapshots(
                &api_snapshot.api,
                current_api_ir,
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(input_error(format!(
            "cannot read IR snapshot {}: {error}",
            api_snapshot_path.display()
        ))),
    }
}

fn render_generated_source_pr_summary(
    api_snapshot_changes: &Option<Vec<Change>>,
    generated_output_differences: &[String],
    validation_status: &str,
) -> String {
    let mut summary = String::new();
    summary.push_str("# Tokyo Generated CLI Update\n\n");
    summary.push_str("## API Changes\n\n");
    match api_snapshot_changes {
        Some(changes) if changes.is_empty() => {
            summary.push_str("No API changes detected.\n\n");
        }
        Some(changes) => {
            for change in changes {
                summary.push_str("- ");
                summary.push_str(&describe_api_change(change));
                summary.push('\n');
            }
            summary.push('\n');
        }
        None => {
            summary.push_str(
                "No previous IR snapshot found; treating this as a generated CLI bootstrap.\n\n",
            );
        }
    }
    summary.push_str("## Generated Files\n\n");
    if generated_output_differences.is_empty() {
        summary.push_str("No managed generated files changed.\n\n");
    } else {
        for difference in generated_output_differences {
            summary.push_str("- ");
            summary.push_str(difference);
            summary.push('\n');
        }
        summary.push('\n');
    }
    summary.push_str("## App-Owned Files\n\n");
    summary.push_str("No app-owned files were changed by this generated-source update.\n\n");
    summary.push_str("## Validation\n\n");
    summary.push_str(validation_status);
    summary.push('\n');
    summary
}

fn emit_generated_source_pr_summary(summary_file: Option<&Path>, summary: &str) -> AppResult<()> {
    println!("{summary}");
    if let Some(summary_file) = summary_file {
        if let Some(parent) = summary_file.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|error| {
                output_error(format!(
                    "cannot create summary directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        fs::write(summary_file, summary).map_err(|error| {
            output_error(format!(
                "cannot write PR summary {}: {error}",
                summary_file.display()
            ))
        })?;
    }
    Ok(())
}

fn managed_paths_affected_by_generation(
    desired_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    previous_generated_file_manifest: &Manifest,
    unmanaged_starter_files: &[&str],
) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    paths.extend(desired_files_by_relative_path.keys().cloned());
    for previous_path in &previous_generated_file_manifest.files {
        if !is_unmanaged_starter_file(unmanaged_starter_files, previous_path) {
            paths.insert(previous_path.clone());
        }
    }
    paths
}

fn ensure_git_worktree_is_clean(output_directory: &Path) -> AppResult<()> {
    let output = run_git_capture(output_directory, &["status", "--porcelain"])?;
    if output.stdout.is_empty() {
        return Ok(());
    }
    Err(output_error(format!(
        "refusing to update branch because {} has uncommitted changes",
        output_directory.display()
    )))
}

fn stage_generated_paths(
    output_directory: &Path,
    managed_paths_to_stage: &BTreeSet<String>,
) -> AppResult<()> {
    for managed_path in managed_paths_to_stage {
        validate_generated_relative_path(managed_path)?;
        run_git(output_directory, &["add", "--", managed_path])?;
    }
    Ok(())
}

fn git_has_staged_changes(output_directory: &Path) -> AppResult<bool> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(output_directory)
        .args(["diff", "--cached", "--quiet", "--"])
        .output()
        .map_err(|error| output_error(format!("cannot run git diff: {error}")))?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(git_command_error("git diff --cached --quiet", &output)),
    }
}

fn run_git(output_directory: &Path, args: &[&str]) -> AppResult<()> {
    let output = run_git_capture(output_directory, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_command_error(
            &format!("git {}", args.join(" ")),
            &output,
        ))
    }
}

fn run_git_with_configured_identity(output_directory: &Path, args: &[&str]) -> AppResult<()> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(output_directory)
        .args([
            "-c",
            "user.name=Tokyo",
            "-c",
            "user.email=tokyo@example.invalid",
        ])
        .args(args)
        .output()
        .map_err(|error| output_error(format!("cannot run git {}: {error}", args.join(" "))))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_command_error(
            &format!("git {}", args.join(" ")),
            &output,
        ))
    }
}

fn run_git_capture(output_directory: &Path, args: &[&str]) -> AppResult<std::process::Output> {
    ProcessCommand::new("git")
        .arg("-C")
        .arg(output_directory)
        .args(args)
        .output()
        .map_err(|error| output_error(format!("cannot run git {}: {error}", args.join(" "))))
}

fn git_command_error(command: &str, output: &std::process::Output) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    output_error(format!("{command} failed: {detail}"))
}

fn write_generated_output_transactionally(
    output_directory: &Path,
    desired_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    unmanaged_starter_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    previous_generated_file_manifest: &Manifest,
    unmanaged_starter_files: &[&str],
) -> AppResult<()> {
    write_generated_output_transactionally_with_file_ops(
        output_directory,
        desired_files_by_relative_path,
        unmanaged_starter_files_by_relative_path,
        previous_generated_file_manifest,
        unmanaged_starter_files,
        &RealFileOps,
    )
}

trait FileOps {
    fn rename_path(&self, source_path: &Path, destination_path: &Path) -> std::io::Result<()>;
}

struct RealFileOps;

impl FileOps for RealFileOps {
    fn rename_path(&self, source_path: &Path, destination_path: &Path) -> std::io::Result<()> {
        fs::rename(source_path, destination_path)
    }
}

struct StagedFile {
    temporary: PathBuf,
    target: PathBuf,
    is_manifest: bool,
}

struct BackupFile {
    backup: PathBuf,
    target: PathBuf,
}

fn write_generated_output_transactionally_with_file_ops(
    output_directory: &Path,
    desired_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    unmanaged_starter_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    previous_generated_file_manifest: &Manifest,
    unmanaged_starter_files: &[&str],
    file_ops: &impl FileOps,
) -> AppResult<()> {
    fs::create_dir_all(output_directory).map_err(|error| {
        output_error(format!(
            "cannot create output directory {}: {error}",
            output_directory.display()
        ))
    })?;
    let canonical_output_directory = fs::canonicalize(output_directory).map_err(|error| {
        output_error(format!(
            "cannot resolve output directory {}: {error}",
            output_directory.display()
        ))
    })?;

    for generated_relative_path in desired_files_by_relative_path
        .keys()
        .chain(unmanaged_starter_files_by_relative_path.keys())
    {
        validate_generated_relative_path(generated_relative_path)?;
        let generated_file_parent_directory = output_directory
            .join(generated_relative_path)
            .parent()
            .unwrap_or(output_directory)
            .to_path_buf();
        fs::create_dir_all(&generated_file_parent_directory).map_err(|error| {
            output_error(format!(
                "cannot create {}: {error}",
                generated_file_parent_directory.display()
            ))
        })?;
        ensure_path_remains_inside_output_directory(
            &generated_file_parent_directory,
            &canonical_output_directory,
        )?;
    }

    let mut affected_generated_relative_paths = BTreeSet::new();
    affected_generated_relative_paths.extend(desired_files_by_relative_path.keys().cloned());
    for previously_generated_relative_path in &previous_generated_file_manifest.files {
        validate_generated_relative_path(previously_generated_relative_path)?;
        if is_unmanaged_starter_file(unmanaged_starter_files, previously_generated_relative_path) {
            continue;
        }
        affected_generated_relative_paths.insert(previously_generated_relative_path.clone());
    }

    let mut existing_managed_target_paths = Vec::new();
    for generated_relative_path in affected_generated_relative_paths {
        let managed_target_path = output_directory.join(&generated_relative_path);
        match fs::symlink_metadata(&managed_target_path) {
            Ok(managed_target_metadata) => {
                if managed_target_metadata.file_type().is_symlink()
                    || !managed_target_metadata.is_file()
                {
                    return Err(output_error(format!(
                        "refusing to replace unsafe managed path {}",
                        managed_target_path.display()
                    )));
                }
                ensure_path_remains_inside_output_directory(
                    managed_target_path.parent().unwrap_or(output_directory),
                    &canonical_output_directory,
                )?;
                existing_managed_target_paths.push(managed_target_path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(output_error(format!(
                    "cannot inspect managed file {}: {error}",
                    managed_target_path.display()
                )));
            }
        }
    }

    let transaction_directory = create_unique_output_transaction_directory(output_directory)?;
    let staged_files_directory = transaction_directory.join("staged");
    let backup_files_directory = transaction_directory.join("backups");
    if let Err(error) = fs::create_dir(&staged_files_directory)
        .and_then(|()| fs::create_dir(&backup_files_directory))
    {
        let _ = fs::remove_dir_all(&transaction_directory);
        return Err(output_error(format!(
            "cannot prepare output transaction in {}: {error}",
            transaction_directory.display()
        )));
    }

    let mut staged_generated_files = Vec::with_capacity(desired_files_by_relative_path.len());
    for (staging_index, (generated_relative_path, generated_file_contents)) in
        desired_files_by_relative_path.iter().enumerate()
    {
        match stage_generated_file_for_transaction(
            &staged_files_directory,
            output_directory,
            staging_index,
            generated_relative_path,
            generated_file_contents,
        ) {
            Ok(staged_generated_file) => staged_generated_files.push(staged_generated_file),
            Err(error) => {
                let _ = fs::remove_dir_all(&transaction_directory);
                return Err(error);
            }
        }
    }
    let mut next_staging_index = staged_generated_files.len();
    for (generated_relative_path, generated_file_contents) in
        unmanaged_starter_files_by_relative_path
    {
        let starter_target_path = output_directory.join(generated_relative_path);
        if starter_target_path.exists() {
            // Migration: a starter file that an earlier release managed (its
            // recorded hash still matches the file on disk, so the user never
            // edited it) is refreshed once to the new starter content. A
            // user-edited file is always left alone.
            let previously_managed_and_unedited = previous_generated_file_manifest
                .hashes
                .get(generated_relative_path)
                .is_some_and(|recorded_hash| {
                    fs::read(&starter_target_path)
                        .is_ok_and(|contents| &sha256_hex(&contents) == recorded_hash)
                });
            if !previously_managed_and_unedited {
                continue;
            }
        }
        match stage_generated_file_for_transaction(
            &staged_files_directory,
            output_directory,
            next_staging_index,
            generated_relative_path,
            generated_file_contents,
        ) {
            Ok(staged_generated_file) => staged_generated_files.push(staged_generated_file),
            Err(error) => {
                let _ = fs::remove_dir_all(&transaction_directory);
                return Err(error);
            }
        }
        next_staging_index += 1;
    }

    let backup_files: Vec<BackupFile> = existing_managed_target_paths
        .into_iter()
        .enumerate()
        .map(|(backup_index, managed_target_path)| BackupFile {
            backup: backup_files_directory.join(backup_index.to_string()),
            target: managed_target_path,
        })
        .collect();

    match commit_generated_output_transaction(file_ops, &backup_files, &staged_generated_files) {
        Ok(()) => {
            let _ = fs::remove_dir_all(&transaction_directory);
            Ok(())
        }
        Err((error, rollback_was_complete)) => {
            if rollback_was_complete {
                let _ = fs::remove_dir_all(&transaction_directory);
            }
            Err(error)
        }
    }
}

fn create_unique_output_transaction_directory(output_directory: &Path) -> AppResult<PathBuf> {
    for _ in 0..100 {
        let transaction_sequence_number = NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed);
        let candidate_transaction_directory = output_directory.join(format!(
            ".tokyo-transaction-{}-{transaction_sequence_number}",
            std::process::id()
        ));
        match fs::create_dir(&candidate_transaction_directory) {
            Ok(()) => return Ok(candidate_transaction_directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(output_error(format!(
                    "cannot create output transaction {}: {error}",
                    candidate_transaction_directory.display()
                )));
            }
        }
    }
    Err(output_error(format!(
        "cannot allocate a unique output transaction in {}",
        output_directory.display()
    )))
}

fn stage_generated_file_for_transaction(
    staged_files_directory: &Path,
    output_directory: &Path,
    staging_index: usize,
    generated_relative_path: &str,
    generated_file_contents: &[u8],
) -> AppResult<StagedFile> {
    validate_generated_relative_path(generated_relative_path)?;
    let final_generated_file_path = output_directory.join(generated_relative_path);
    let temporary_staged_file_path = staged_files_directory.join(staging_index.to_string());
    let mut temporary_staged_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_staged_file_path)
        .map_err(|error| {
            output_error(format!(
                "cannot stage {}: {error}",
                final_generated_file_path.display()
            ))
        })?;
    if let Err(error) = temporary_staged_file
        .write_all(generated_file_contents)
        .and_then(|()| temporary_staged_file.sync_all())
    {
        let _ = fs::remove_file(&temporary_staged_file_path);
        return Err(output_error(format!(
            "cannot stage {}: {error}",
            final_generated_file_path.display()
        )));
    }
    Ok(StagedFile {
        temporary: temporary_staged_file_path,
        target: final_generated_file_path,
        is_manifest: generated_relative_path == MANIFEST_FILE,
    })
}

fn commit_generated_output_transaction(
    file_ops: &impl FileOps,
    backup_files: &[BackupFile],
    staged_generated_files: &[StagedFile],
) -> Result<(), (anyhow::Error, bool)> {
    let mut completed_backup_files = Vec::with_capacity(backup_files.len());
    for backup_file in backup_files {
        if let Err(error) = file_ops.rename_path(&backup_file.target, &backup_file.backup) {
            let rollback_errors =
                rollback_generated_output_transaction(file_ops, &completed_backup_files, &[]);
            let rollback_was_complete = rollback_errors.is_empty();
            return Err((
                build_generated_output_transaction_error(
                    format!(
                        "cannot back up managed file {}",
                        backup_file.target.display()
                    ),
                    error,
                    rollback_errors,
                ),
                rollback_was_complete,
            ));
        }
        completed_backup_files.push(backup_file);
    }

    let mut installed_generated_files = Vec::with_capacity(staged_generated_files.len());
    for staged_generated_file in staged_generated_files
        .iter()
        .filter(|staged_generated_file| !staged_generated_file.is_manifest)
        .chain(
            staged_generated_files
                .iter()
                .filter(|staged_generated_file| staged_generated_file.is_manifest),
        )
    {
        if let Err(error) = file_ops.rename_path(
            &staged_generated_file.temporary,
            &staged_generated_file.target,
        ) {
            let rollback_errors = rollback_generated_output_transaction(
                file_ops,
                &completed_backup_files,
                &installed_generated_files,
            );
            let rollback_was_complete = rollback_errors.is_empty();
            return Err((
                build_generated_output_transaction_error(
                    format!(
                        "cannot install generated file {}",
                        staged_generated_file.target.display()
                    ),
                    error,
                    rollback_errors,
                ),
                rollback_was_complete,
            ));
        }
        installed_generated_files.push(staged_generated_file);
    }
    Ok(())
}

fn rollback_generated_output_transaction(
    file_ops: &impl FileOps,
    completed_backup_files: &[&BackupFile],
    installed_generated_files: &[&StagedFile],
) -> Vec<String> {
    let mut rollback_error_messages = Vec::new();
    for installed_generated_file in installed_generated_files.iter().rev() {
        if let Err(error) = file_ops.rename_path(
            &installed_generated_file.target,
            &installed_generated_file.temporary,
        ) {
            rollback_error_messages.push(format!(
                "cannot remove new file {} during rollback: {error}",
                installed_generated_file.target.display()
            ));
        }
    }
    for backup_file in completed_backup_files.iter().rev() {
        if let Err(error) = file_ops.rename_path(&backup_file.backup, &backup_file.target) {
            rollback_error_messages.push(format!(
                "cannot restore {} during rollback: {error}",
                backup_file.target.display()
            ));
        }
    }
    rollback_error_messages
}

fn build_generated_output_transaction_error(
    transaction_error_context: String,
    error: std::io::Error,
    rollback_errors: Vec<String>,
) -> anyhow::Error {
    let rollback_error_suffix = if rollback_errors.is_empty() {
        String::new()
    } else {
        format!("; rollback also failed: {}", rollback_errors.join("; "))
    };
    output_error(format!(
        "{transaction_error_context}: {error}{rollback_error_suffix}"
    ))
}

fn validate_generated_relative_path(generated_relative_path: &str) -> AppResult<()> {
    let generated_path = Path::new(generated_relative_path);
    if generated_relative_path.is_empty()
        || generated_path.is_absolute()
        || generated_path
            .components()
            .any(|path_component| !matches!(path_component, Component::Normal(_)))
    {
        return Err(output_error(format!(
            "refusing unsafe generated path {generated_relative_path:?}"
        )));
    }
    Ok(())
}

fn ensure_path_remains_inside_output_directory(
    path_to_validate: &Path,
    canonical_output_directory: &Path,
) -> AppResult<()> {
    let canonical_path_to_validate = fs::canonicalize(path_to_validate).map_err(|error| {
        output_error(format!(
            "cannot resolve {}: {error}",
            path_to_validate.display()
        ))
    })?;
    if !canonical_path_to_validate.starts_with(canonical_output_directory) {
        return Err(output_error(format!(
            "refusing path outside output directory: {}",
            path_to_validate.display()
        )));
    }
    Ok(())
}

fn print_human_readable_api_diff(changes: &[Change]) {
    if changes.is_empty() {
        println!("no CLI/API changes");
        return;
    }
    for change in changes {
        println!("{}", describe_api_change(change));
    }
}

fn describe_api_change(change: &Change) -> String {
    let (verb, kind, id) = match change {
        Change::CliBehaviorChanged => ("changed", "CLI behavior", "configuration".to_string()),
        Change::SdkBehaviorChanged => ("changed", "SDK behavior", "configuration".to_string()),
        Change::SchemaComponentsChanged => (
            "changed",
            "OpenAPI schema components",
            "components.schemas".to_string(),
        ),
        Change::OmissionsChanged => (
            "changed",
            "CLI omission metadata",
            "non-client operations".to_string(),
        ),
        Change::TypeAdded(id) => ("added", "type", id.to_string()),
        Change::TypeRemoved(id) => ("removed", "type", id.to_string()),
        Change::TypeChanged(id) => ("changed", "type", id.to_string()),
        Change::EndpointAdded(id) => ("added", "endpoint", id.to_string()),
        Change::EndpointRemoved(id) => ("removed", "endpoint", id.to_string()),
        Change::EndpointChanged(id) => ("changed", "endpoint", id.to_string()),
        Change::ChannelAdded(id) => ("added", "websocket channel", id.to_string()),
        Change::ChannelRemoved(id) => ("removed", "websocket channel", id.to_string()),
        Change::ChannelChanged(id) => ("changed", "websocket channel", id.to_string()),
    };
    format!("{verb} {kind}: {id}")
}

fn engine_output_error(error: tokyo_codegen_engine::Error) -> anyhow::Error {
    output_error(error.to_string())
}

fn json_output_error(error: serde_json::Error) -> anyhow::Error {
    output_error(format!("cannot serialize generated metadata: {error}"))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let sequence = NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "tokyo-output-transaction-{}-{sequence}",
                std::process::id()
            ));
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

    struct FailOneRename {
        fail_at: usize,
        calls: Cell<usize>,
    }

    impl FileOps for FailOneRename {
        fn rename_path(&self, source_path: &Path, destination_path: &Path) -> std::io::Result<()> {
            let rename_call_number = self.calls.get() + 1;
            self.calls.set(rename_call_number);
            if rename_call_number == self.fail_at {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected rename failure",
                ))
            } else {
                fs::rename(source_path, destination_path)
            }
        }
    }

    #[test]
    fn restores_all_managed_paths_when_any_commit_rename_fails() {
        for fail_at in 1..=6 {
            let temp = TestDir::new();
            let output = temp.0.join("sdk");
            fs::create_dir_all(&output).unwrap();
            fs::write(output.join("managed.txt"), "old managed").unwrap();
            fs::write(output.join("stale.txt"), "old stale").unwrap();
            fs::create_dir_all(output.join(MANIFEST_FILE).parent().unwrap()).unwrap();
            fs::write(output.join(MANIFEST_FILE), "old manifest").unwrap();
            fs::write(output.join("user-notes.txt"), "keep me").unwrap();

            let previous = Manifest {
                format_version: FORMAT_VERSION,
                files: vec![
                    MANIFEST_FILE.to_string(),
                    "managed.txt".to_string(),
                    "stale.txt".to_string(),
                ],
                hashes: BTreeMap::new(),
            };
            let desired = BTreeMap::from([
                (MANIFEST_FILE.to_string(), b"new manifest".to_vec()),
                ("managed.txt".to_string(), b"new managed".to_vec()),
                ("new.txt".to_string(), b"new file".to_vec()),
            ]);
            let file_ops = FailOneRename {
                fail_at,
                calls: Cell::new(0),
            };

            let error = write_generated_output_transactionally_with_file_ops(
                &output,
                &desired,
                &BTreeMap::new(),
                &previous,
                &[],
                &file_ops,
            )
            .expect_err("injected failure must abort output update");
            assert!(
                error.to_string().contains("injected rename failure"),
                "{error:?}"
            );
            assert_eq!(
                fs::read_to_string(output.join("managed.txt")).unwrap(),
                "old managed"
            );
            assert_eq!(
                fs::read_to_string(output.join("stale.txt")).unwrap(),
                "old stale"
            );
            assert_eq!(
                fs::read_to_string(output.join(MANIFEST_FILE)).unwrap(),
                "old manifest"
            );
            assert!(!output.join("new.txt").exists());
            assert_eq!(
                fs::read_to_string(output.join("user-notes.txt")).unwrap(),
                "keep me"
            );
            assert!(
                fs::read_dir(&output).unwrap().all(|entry| {
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".tokyo-transaction-")
                }),
                "completed rollback should remove transaction directory"
            );
        }
    }
}
