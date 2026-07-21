//! Developer-owned custom commands.
//!
//! Tokyo does not overwrite this file after the initial scaffold.

pub fn augment(command: clap::Command) -> clap::Command {
    command
}

pub fn dispatch(
    _matches: &clap::ArgMatches,
    _context: &crate::cli::CommandContext<'_>,
) -> Result<bool, crate::error::ClientError> {
    Ok(false)
}
