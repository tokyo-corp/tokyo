//! Generated output-plan assembly for CLI projects.

use crate::error::*;
use crate::manifest::*;
use crate::prelude::*;
use crate::routes::*;

pub(crate) struct DesiredOutputFiles {
    pub(crate) managed_files_by_relative_path: BTreeMap<String, Vec<u8>>,
    pub(crate) unmanaged_starter_files_by_relative_path: BTreeMap<String, Vec<u8>>,
}
pub(crate) struct CliEmitter;

impl Emitter for CliEmitter {
    type Error = Infallible;

    fn emit_target_files(
        &self,
        api: &Api,
    ) -> Result<Vec<tokyo_codegen_engine::GeneratedFile>, Self::Error> {
        Ok(tokyo_emit_cli::emit_generated_cli_project_files(api))
    }
}
pub(crate) fn build_desired_generated_files_by_relative_path(
    api_ir: &Api,
    routes: &[DiscoveredRoute],
    output_directory: &Path,
) -> AppResult<DesiredOutputFiles> {
    let generator_version = env!("CARGO_PKG_VERSION");
    let generated_output_plan =
        tokyo_codegen_engine::build_output_plan(api_ir, &CliEmitter, generator_version)
            .map_err(engine_output_error)?;
    let unmanaged_starter_files = tokyo_emit_cli::UNMANAGED_STARTER_FILES;
    let mut generated_files_by_relative_path = BTreeMap::new();
    let mut unmanaged_starter_files_by_relative_path = BTreeMap::new();
    for generated_file in generated_output_plan {
        validate_generated_relative_path(&generated_file.relative_path)?;
        if unmanaged_starter_files.contains(&generated_file.relative_path.as_str()) {
            unmanaged_starter_files_by_relative_path.insert(
                generated_file.relative_path,
                generated_file.contents.into_bytes(),
            );
        } else {
            generated_files_by_relative_path.insert(
                generated_file.relative_path,
                generated_file.contents.into_bytes(),
            );
        }
    }
    generated_files_by_relative_path.insert(
        ".tokyo/src/tokyo/routes.rs".to_string(),
        render_route_registry(routes, output_directory)?.into_bytes(),
    );

    let mut generated_manifest_file_entries: Vec<String> =
        generated_files_by_relative_path.keys().cloned().collect();
    generated_manifest_file_entries.push(MANIFEST_FILE.to_string());
    generated_manifest_file_entries.sort();
    let generated_file_hashes = generated_files_by_relative_path
        .iter()
        .map(|(relative_path, contents)| (relative_path.clone(), sha256_hex(contents)))
        .collect();
    let generated_file_manifest = Manifest {
        format_version: FORMAT_VERSION,
        files: generated_manifest_file_entries,
        hashes: generated_file_hashes,
    };
    let mut manifest_json =
        serde_json::to_string_pretty(&generated_file_manifest).map_err(json_output_error)?;
    manifest_json.push('\n');
    generated_files_by_relative_path.insert(MANIFEST_FILE.to_string(), manifest_json.into_bytes());
    Ok(DesiredOutputFiles {
        managed_files_by_relative_path: generated_files_by_relative_path,
        unmanaged_starter_files_by_relative_path,
    })
}
