//! API snapshot diff command and human-readable change reporting.

use crate::cli::*;
use crate::config::*;
use crate::error::*;
use crate::import::*;
use crate::prelude::*;

pub(crate) fn run_api_snapshot_diff_command(diff_command_arguments: DiffArgs) -> AppResult<()> {
    let (codegen_config, output_directory) =
        load_generation_settings_and_output_directory(&diff_command_arguments.common)?;
    let current_api_ir =
        import_openapi_input_as_api_ir(&diff_command_arguments.common, &codegen_config)?;
    let api_snapshot_path = resolve_api_snapshot_path(&output_directory);
    let api_snapshot_json = fs::read_to_string(&api_snapshot_path).map_err(|error| {
        input_error(format!(
            "cannot read IR snapshot {}: {error}; run generate first",
            api_snapshot_path.display()
        ))
    })?;
    let api_snapshot: Snapshot = serde_json::from_str(&api_snapshot_json).map_err(|error| {
        input_error(format!(
            "invalid IR snapshot {}: {error}",
            api_snapshot_path.display()
        ))
    })?;
    if api_snapshot.format_version != FORMAT_VERSION {
        return Err(input_error(format!(
            "unsupported IR snapshot format {} (expected {})",
            api_snapshot.format_version, FORMAT_VERSION
        )));
    }
    if !api_snapshot.api.has_supported_schema_version() {
        return Err(input_error(format!(
            "unsupported IR schema version {} in {} (expected {})",
            api_snapshot.api.schema_version,
            api_snapshot_path.display(),
            tokyo_ir::IR_SCHEMA_VERSION
        )));
    }

    let api_snapshot_changes =
        tokyo_ir::diff::diff_api_snapshots(&api_snapshot.api, &current_api_ir);
    match diff_command_arguments.format {
        DiffFormat::Human => print_human_readable_api_diff(&api_snapshot_changes),
        DiffFormat::Json => {
            let api_changes_json =
                serde_json::to_string_pretty(&api_snapshot_changes).map_err(json_output_error)?;
            println!("{api_changes_json}");
        }
    }
    if api_snapshot_changes.is_empty() {
        Ok(())
    } else {
        Err(differences_error(format!(
            "{} CLI/API change{} detected",
            api_snapshot_changes.len(),
            if api_snapshot_changes.len() == 1 {
                ""
            } else {
                "s"
            }
        )))
    }
}
pub(crate) fn resolve_api_snapshot_path(output_directory: &Path) -> PathBuf {
    let current = output_directory.join(SNAPSHOT_FILE);
    if current.exists() {
        return current;
    }
    let legacy = output_directory.join(tokyo_codegen_engine::LEGACY_SNAPSHOT_FILE);
    if legacy.exists() { legacy } else { current }
}

pub(crate) fn diff_previous_api_snapshot_with_current_api(
    output_directory: &Path,
    current_api_ir: &Api,
) -> AppResult<Option<Vec<Change>>> {
    let api_snapshot_path = resolve_api_snapshot_path(output_directory);
    match fs::read_to_string(&api_snapshot_path) {
        Ok(api_snapshot_json) => {
            let api_snapshot: Snapshot =
                serde_json::from_str(&api_snapshot_json).map_err(|error| {
                    input_error(format!(
                        "invalid IR snapshot {}: {error}",
                        api_snapshot_path.display()
                    ))
                })?;
            if api_snapshot.format_version != FORMAT_VERSION {
                return Err(input_error(format!(
                    "unsupported IR snapshot format {} in {}",
                    api_snapshot.format_version,
                    api_snapshot_path.display()
                )));
            }
            if !api_snapshot.api.has_supported_schema_version() {
                return Err(input_error(format!(
                    "unsupported IR schema version {} in {} (expected {})",
                    api_snapshot.api.schema_version,
                    api_snapshot_path.display(),
                    tokyo_ir::IR_SCHEMA_VERSION
                )));
            }
            Ok(Some(tokyo_ir::diff::diff_api_snapshots(
                &api_snapshot.api,
                current_api_ir,
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(input_error(format!(
            "cannot read IR snapshot {}: {error}",
            api_snapshot_path.display()
        ))),
    }
}
pub(crate) fn print_human_readable_api_diff(changes: &[Change]) {
    if changes.is_empty() {
        println!("no CLI/API changes");
        return;
    }
    for change in changes {
        println!("{}", describe_api_change(change));
    }
}

pub(crate) fn describe_api_change(change: &Change) -> String {
    let (verb, kind, id) = match change {
        Change::CliBehaviorChanged => ("changed", "CLI behavior", "configuration".to_string()),
        Change::SdkBehaviorChanged => ("changed", "SDK behavior", "configuration".to_string()),
        Change::SchemaComponentsChanged => (
            "changed",
            "OpenAPI schema components",
            "components.schemas".to_string(),
        ),
        Change::OmissionsChanged => (
            "changed",
            "CLI omission metadata",
            "non-client operations".to_string(),
        ),
        Change::TypeAdded(id) => ("added", "type", id.to_string()),
        Change::TypeRemoved(id) => ("removed", "type", id.to_string()),
        Change::TypeChanged(id) => ("changed", "type", id.to_string()),
        Change::EndpointAdded(id) => ("added", "endpoint", id.to_string()),
        Change::EndpointRemoved(id) => ("removed", "endpoint", id.to_string()),
        Change::EndpointChanged(id) => ("changed", "endpoint", id.to_string()),
        Change::ChannelAdded(id) => ("added", "websocket channel", id.to_string()),
        Change::ChannelRemoved(id) => ("removed", "websocket channel", id.to_string()),
        Change::ChannelChanged(id) => ("changed", "websocket channel", id.to_string()),
    };
    format!("{verb} {kind}: {id}")
}
