//! Compatibility starter for projects generated before filesystem routes.

pub const CUSTOM_RS: &str = r#"//! Legacy custom-command compatibility hook.
//!
//! New commands belong in `src/routes/**`. Tokyo keeps this user-owned hook
//! so older generated projects continue to compile and existing custom
//! commands survive regeneration.

pub fn augment(command: clap::Command) -> clap::Command {
    command
}

pub fn dispatch(
    _matches: &clap::ArgMatches,
    _context: &crate::cli::CommandContext<'_>,
) -> Result<bool, crate::error::ClientError> {
    Ok(false)
}
"#;
