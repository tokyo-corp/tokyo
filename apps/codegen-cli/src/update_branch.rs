//! Generated-source branch, validation, and pull-request workflow.

use crate::api_diff::*;
use crate::cli::*;
use crate::config::*;
use crate::emit::*;
use crate::error::*;
use crate::git::*;
use crate::import::*;
use crate::manifest::*;
use crate::prelude::*;
use crate::routes::*;
use crate::transaction::*;

pub(crate) fn run_update_branch_command(
    update_branch_arguments: UpdateBranchArgs,
) -> AppResult<()> {
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
pub(crate) fn validate_generated_output(
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
pub(crate) fn create_or_update_generated_source_pull_request(
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
pub(crate) fn render_generated_source_pr_summary(
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

pub(crate) fn emit_generated_source_pr_summary(
    summary_file: Option<&Path>,
    summary: &str,
) -> AppResult<()> {
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
