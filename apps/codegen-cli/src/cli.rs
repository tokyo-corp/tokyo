//! Command-line argument and subcommand definitions.

use crate::prelude::*;

#[derive(Parser)]
#[command(name = "tokyo", version, about = "Build route-first agent CLIs")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
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
pub(crate) struct InitArgs {
    /// Directory to initialize.
    #[arg(default_value = ".", value_name = "DIR")]
    pub(crate) directory: PathBuf,
    /// Cargo package and executable name.
    #[arg(long, value_name = "NAME")]
    pub(crate) name: String,
}

#[derive(Args)]
pub(crate) struct OpenapiArgs {
    #[command(subcommand)]
    pub(crate) command: OpenapiCommand,
}

#[derive(Subcommand)]
pub(crate) enum OpenapiCommand {
    /// Validate and vendor an OpenAPI document.
    Add(OpenapiAddArgs),
    /// Reacquire and update the configured vendored document.
    Sync(OpenapiConfigArgs),
    /// Report whether the configured source differs without writing.
    Check(OpenapiConfigArgs),
}

#[derive(Args)]
pub(crate) struct OpenapiAddArgs {
    /// HTTP(S) URL or local file path.
    #[arg(value_name = "URL|PATH")]
    pub(crate) source: String,
    /// Tokyo project configuration file.
    #[arg(short, long, default_value = DEFAULT_CONFIG, value_name = "FILE")]
    pub(crate) config: PathBuf,
}

#[derive(Args)]
pub(crate) struct OpenapiConfigArgs {
    /// Tokyo project configuration file.
    #[arg(short, long, default_value = DEFAULT_CONFIG, value_name = "FILE")]
    pub(crate) config: PathBuf,
}

#[derive(Args, Clone)]
pub(crate) struct CommonArgs {
    /// OpenAPI JSON or YAML input.
    #[arg(short, long, value_name = "FILE")]
    pub(crate) input: Option<PathBuf>,
    /// Generated output directory (overrides config).
    #[arg(short, long, value_name = "DIR")]
    pub(crate) output: Option<PathBuf>,
    /// TOML configuration file. Defaults to tokyo.toml when present.
    #[arg(short, long, value_name = "FILE")]
    pub(crate) config: Option<PathBuf>,
}

#[derive(Args)]
pub(crate) struct GenerationArgs {
    #[command(flatten)]
    pub(crate) common: CommonArgs,
}

#[derive(Args)]
pub(crate) struct DevArgs {
    #[command(flatten)]
    pub(crate) common: CommonArgs,
    /// Use a local tokyo-cli-runtime checkout instead of the published crate
    /// when building the generated CLI.
    #[arg(long, value_name = "DIR")]
    pub(crate) runtime_path: Option<PathBuf>,
}

#[derive(Args)]
pub(crate) struct UpdateBranchArgs {
    #[command(flatten)]
    pub(crate) common: CommonArgs,
    /// Branch to create or reset with generated-source updates.
    #[arg(long, default_value = "tokyo/update-generated-cli")]
    pub(crate) branch: String,
    /// Commit message for generated-source updates.
    #[arg(long, default_value = "Update Tokyo generated CLI")]
    pub(crate) message: String,
    /// Write a Markdown PR summary to this file as well as stdout.
    #[arg(long, value_name = "FILE")]
    pub(crate) summary_file: Option<PathBuf>,
    /// Type-check the generated output and include the result in the summary.
    /// The command exits nonzero when validation fails, after the branch,
    /// summary, and any requested push/PR are complete.
    #[arg(long)]
    pub(crate) validate: bool,
    /// Use a local tokyo-cli-runtime checkout instead of the published crate
    /// when validating.
    #[arg(long, value_name = "DIR")]
    pub(crate) runtime_path: Option<PathBuf>,
    /// Push the branch to this remote after committing.
    #[arg(long, value_name = "REMOTE", num_args = 0..=1, default_missing_value = "origin")]
    pub(crate) push: Option<String>,
    /// Create the GitHub pull request for the branch, or update the existing
    /// open Tokyo PR in place. Requires --push.
    #[arg(long, requires = "push")]
    pub(crate) pr: bool,
}

#[derive(Args)]
pub(crate) struct DiffArgs {
    #[command(flatten)]
    pub(crate) common: CommonArgs,
    /// Diff rendering format.
    #[arg(long, value_enum, default_value_t = DiffFormat::Human)]
    pub(crate) format: DiffFormat,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum DiffFormat {
    Human,
    Json,
}
pub(crate) fn backwards_compatible_args() -> Vec<OsString> {
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
