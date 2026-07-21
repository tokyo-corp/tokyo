//! Generated-file manifests, hashes, and drift detection.

use crate::error::*;
use crate::prelude::*;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Manifest {
    pub(crate) format_version: u32,
    pub(crate) files: Vec<String>,
    /// SHA-256 of each managed file as generated. Files without a recorded
    /// hash (older manifests) skip hand-edit detection; the manifest itself
    /// is never hashed.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) hashes: BTreeMap<String, String>,
}

pub(crate) fn validate_generated_relative_path(generated_relative_path: &str) -> AppResult<()> {
    let generated_path = Path::new(generated_relative_path);
    if generated_relative_path.is_empty()
        || generated_path.is_absolute()
        || generated_path
            .components()
            .any(|path_component| !matches!(path_component, Component::Normal(_)))
    {
        return Err(output_error(format!(
            "refusing unsafe generated path {generated_relative_path:?}"
        )));
    }
    Ok(())
}

pub(crate) fn is_unmanaged_starter_file(
    unmanaged_starter_files: &[&str],
    generated_relative_path: &str,
) -> bool {
    unmanaged_starter_files.contains(&generated_relative_path)
}

pub(crate) fn sha256_hex(contents: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(contents);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Fails when a manifest-listed managed file was edited by hand since the
/// last generation, so regeneration never silently erases local changes.
/// Files without a recorded hash (older manifests) and missing files are
/// skipped; missing managed files are simply recreated.
pub(crate) fn detect_hand_edited_managed_files(
    output_directory: &Path,
    previous_generated_file_manifest: &Manifest,
    unmanaged_starter_files: &[&str],
) -> AppResult<()> {
    let mut hand_edited_relative_paths = Vec::new();
    for (relative_path, recorded_hash) in &previous_generated_file_manifest.hashes {
        if is_unmanaged_starter_file(unmanaged_starter_files, relative_path)
            || relative_path == MANIFEST_FILE
        {
            continue;
        }
        match fs::read(output_directory.join(relative_path)) {
            Ok(contents) if &sha256_hex(&contents) != recorded_hash => {
                hand_edited_relative_paths.push(relative_path.clone());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(output_error(format!(
                    "cannot read managed file {}: {error}",
                    output_directory.join(relative_path).display()
                )));
            }
        }
    }
    if hand_edited_relative_paths.is_empty() {
        return Ok(());
    }
    Err(output_error(format!(
        "managed files were edited by hand since the last generation: {}\nrevert the edits (or delete the files to have them recreated), then rerun; hand-written code belongs in developer-owned files like src/routes/**, src/middleware.rs, src/commands/guidance.rs, and src/presentation.rs",
        hand_edited_relative_paths.join(", ")
    )))
}

pub(crate) fn read_previous_generated_file_manifest(
    output_directory: &Path,
) -> AppResult<Manifest> {
    let mut generated_file_manifest_path = output_directory.join(MANIFEST_FILE);
    if !generated_file_manifest_path.exists() {
        let legacy_manifest_path = output_directory.join(LEGACY_MANIFEST_FILE);
        if legacy_manifest_path.exists() {
            generated_file_manifest_path = legacy_manifest_path;
        }
    }
    match fs::read_to_string(&generated_file_manifest_path) {
        Ok(generated_file_manifest_json) => {
            let generated_file_manifest: Manifest =
                serde_json::from_str(&generated_file_manifest_json).map_err(|error| {
                    output_error(format!(
                        "invalid generated-file manifest {}: {error}",
                        generated_file_manifest_path.display()
                    ))
                })?;
            if generated_file_manifest.format_version != FORMAT_VERSION {
                return Err(output_error(format!(
                    "unsupported manifest format {} in {}",
                    generated_file_manifest.format_version,
                    generated_file_manifest_path.display()
                )));
            }
            for generated_relative_path in &generated_file_manifest.files {
                validate_generated_relative_path(generated_relative_path)?;
            }
            Ok(generated_file_manifest)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(error) => Err(output_error(format!(
            "cannot read manifest {}: {error}",
            generated_file_manifest_path.display()
        ))),
    }
}

pub(crate) fn detect_generated_output_differences(
    output_directory: &Path,
    desired_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    previous_generated_file_manifest: &Manifest,
    unmanaged_starter_files: &[&str],
) -> AppResult<Vec<String>> {
    let mut generated_output_differences = Vec::new();
    for (generated_relative_path, desired_file_contents) in desired_files_by_relative_path {
        match fs::read(output_directory.join(generated_relative_path)) {
            Ok(existing_file_contents) if existing_file_contents == *desired_file_contents => {}
            Ok(_) => {
                generated_output_differences.push(format!("modified {generated_relative_path}"))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                generated_output_differences.push(format!("missing {generated_relative_path}"));
            }
            Err(error) => {
                return Err(output_error(format!(
                    "cannot inspect {}: {error}",
                    output_directory.join(generated_relative_path).display()
                )));
            }
        }
    }
    for previously_generated_relative_path in &previous_generated_file_manifest.files {
        if is_unmanaged_starter_file(unmanaged_starter_files, previously_generated_relative_path) {
            continue;
        }
        if !desired_files_by_relative_path.contains_key(previously_generated_relative_path)
            && output_directory
                .join(previously_generated_relative_path)
                .exists()
        {
            generated_output_differences
                .push(format!("stale {previously_generated_relative_path}"));
        }
    }
    Ok(generated_output_differences)
}
pub(crate) fn managed_paths_affected_by_generation(
    desired_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    previous_generated_file_manifest: &Manifest,
    unmanaged_starter_files: &[&str],
) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    paths.extend(desired_files_by_relative_path.keys().cloned());
    for previous_path in &previous_generated_file_manifest.files {
        if !is_unmanaged_starter_file(unmanaged_starter_files, previous_path) {
            paths.insert(previous_path.clone());
        }
    }
    paths
}
