//! Git subprocess helpers used by generated update branches.

use crate::error::*;
use crate::manifest::*;
use crate::prelude::*;

pub(crate) fn ensure_git_worktree_is_clean(output_directory: &Path) -> AppResult<()> {
    let output = run_git_capture(output_directory, &["status", "--porcelain"])?;
    if output.stdout.is_empty() {
        return Ok(());
    }
    Err(output_error(format!(
        "refusing to update branch because {} has uncommitted changes",
        output_directory.display()
    )))
}

pub(crate) fn stage_generated_paths(
    output_directory: &Path,
    managed_paths_to_stage: &BTreeSet<String>,
) -> AppResult<()> {
    for managed_path in managed_paths_to_stage {
        validate_generated_relative_path(managed_path)?;
        run_git(output_directory, &["add", "--", managed_path])?;
    }
    Ok(())
}

pub(crate) fn git_has_staged_changes(output_directory: &Path) -> AppResult<bool> {
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

pub(crate) fn run_git(output_directory: &Path, args: &[&str]) -> AppResult<()> {
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

pub(crate) fn run_git_with_configured_identity(
    output_directory: &Path,
    args: &[&str],
) -> AppResult<()> {
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

pub(crate) fn run_git_capture(
    output_directory: &Path,
    args: &[&str],
) -> AppResult<std::process::Output> {
    ProcessCommand::new("git")
        .arg("-C")
        .arg(output_directory)
        .args(args)
        .output()
        .map_err(|error| output_error(format!("cannot run git {}: {error}", args.join(" "))))
}

pub(crate) fn git_command_error(command: &str, output: &std::process::Output) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    output_error(format!("{command} failed: {detail}"))
}
