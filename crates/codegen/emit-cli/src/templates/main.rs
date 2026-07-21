//! The generated CLI's `src/main.rs`: just module wiring.

pub const MAIN_RS: &str = r#"// Declared types/methods not exercised by any endpoint in this particular API
// are expected, not a bug: this is a generated SDK-CLI surface, not a single
// application binary that only ever uses what it needs.
#![allow(dead_code)]

mod cli;
mod commands;
mod middleware;
mod presentation;
mod tokyo;

pub use tokyo_cli_runtime::{client, error, oauth, output, profile, session};

fn main() -> std::process::ExitCode {
    tokyo_cli_runtime::configure_generated_cli_runtime(tokyo::config::CONFIG);
    cli::run()
}
"#;
