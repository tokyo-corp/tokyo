//! Small generated-project templates. Shared transport, auth, profile, output,
//! and session behavior lives in the published `tokyo-cli-runtime`
//! crate; only API-specific configuration and binary wiring remain here.

mod commands_mod;
mod config;
mod custom;
mod guidance;
mod main;
mod middleware;
mod presentation;
mod project_files;
mod skills;

pub use commands_mod::COMMANDS_MOD_RS;
pub use config::render_generated_cli_runtime_config_source_file;
pub use custom::CUSTOM_RS;
pub use guidance::GUIDANCE_RS;
pub use main::MAIN_RS;
pub use middleware::MIDDLEWARE_RS;
pub use presentation::PRESENTATION_RS;
pub use project_files::{
    render_generated_cli_cargo_manifest_source_file, render_generated_cli_readme_source_file,
};
pub use skills::project_skill_starter_files;
