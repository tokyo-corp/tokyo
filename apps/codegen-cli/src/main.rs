//! `tokyo` project and code-generation command-line front end.
//!
//! The binary owns argument parsing and command dispatch. Cohesive command,
//! configuration, emission, and filesystem behavior lives in sibling modules.

use clap::Parser;

mod api_diff;
mod cli;
mod config;
mod constants;
mod dev;
mod emit;
mod error;
mod generate;
mod git;
mod import;
mod init;
mod manifest;
mod openapi;
mod prelude;
mod routes;
mod transaction;
mod update_branch;

use api_diff::*;
use cli::*;
use dev::*;
use error::*;
use generate::*;
use init::*;
use update_branch::*;

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

const RUNTIME_CONFIG: tokyo_cli_runtime::RuntimeConfig = tokyo_cli_runtime::RuntimeConfig {
    identity: tokyo_cli_runtime::ProductIdentity {
        package_name: "tokyo-cli",
        command_name: "tokyo",
        env_prefix: "TOKYO",
    },
    default_base_url: None,
    environments: &[],
    oauth_providers: &[],
    scenarios: &[],
    update: Some(tokyo_cli_runtime::UpdateConfig {
        repository: "tokyo-corp/tokyo",
        asset_prefix: "tokyo",
        current_version: env!("CARGO_PKG_VERSION"),
    }),
};

fn main() {
    tokyo_cli_runtime::configure_generated_cli_runtime(RUNTIME_CONFIG);
    tokyo_cli_runtime::update::check_and_apply();
    let normalized_command_line_arguments = backwards_compatible_args();
    let parsed_cli_arguments = match Cli::try_parse_from(normalized_command_line_arguments) {
        Ok(parsed_cli_arguments) => parsed_cli_arguments,
        Err(error) => error.exit(),
    };
    let command_result = match parsed_cli_arguments.command {
        Command::Init(arguments) => run_init_command(arguments),
        Command::Generate(arguments) => run_generate_or_check_command(arguments, false),
        Command::Check(arguments) => run_generate_or_check_command(arguments, true),
        Command::Dev(arguments) => run_dev_command(arguments),
        Command::UpdateBranch(arguments) => run_update_branch_command(arguments),
        Command::Diff(arguments) => run_api_snapshot_diff_command(arguments),
        Command::Openapi(arguments) => run_openapi_command(arguments),
    };
    if let Err(error) = command_result {
        let exit_code = exit_code_for_error(&error);
        eprintln!("error: {error:#}");
        std::process::exit(exit_code);
    }
}
