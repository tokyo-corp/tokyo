//! Developer-owned CLI presentation.
//!
//! Tokyo does not overwrite this file after the initial scaffold. The complete
//! clap command tree — generated commands and filesystem routes alike — passes
//! through `present` before parsing, so help templates, styles, banners, about text,
//! and command ordering are yours to change here.
//!
//! Do not rename or alias generated top-level commands yet: dispatch still
//! classifies subcommands by their generated names.

pub fn present(command: clap::Command) -> clap::Command {
    command
}
