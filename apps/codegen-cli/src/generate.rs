//! Generate and check command orchestration.

use crate::cli::*;
use crate::config::*;
use crate::emit::*;
use crate::error::*;
use crate::import::*;
use crate::manifest::*;
use crate::routes::*;
use crate::transaction::*;

pub(crate) fn run_generate_or_check_command(
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
