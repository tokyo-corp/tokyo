//! Atomic generated-output installation and rollback.

use crate::error::*;
use crate::manifest::*;
use crate::prelude::*;

pub(crate) static NEXT_TRANSACTION: AtomicU64 = AtomicU64::new(0);
pub(crate) fn write_generated_output_transactionally(
    output_directory: &Path,
    desired_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    unmanaged_starter_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    previous_generated_file_manifest: &Manifest,
    unmanaged_starter_files: &[&str],
) -> AppResult<()> {
    write_generated_output_transactionally_with_file_ops(
        output_directory,
        desired_files_by_relative_path,
        unmanaged_starter_files_by_relative_path,
        previous_generated_file_manifest,
        unmanaged_starter_files,
        &RealFileOps,
    )
}

pub(crate) trait FileOps {
    fn rename_path(&self, source_path: &Path, destination_path: &Path) -> std::io::Result<()>;
}

pub(crate) struct RealFileOps;

impl FileOps for RealFileOps {
    fn rename_path(&self, source_path: &Path, destination_path: &Path) -> std::io::Result<()> {
        fs::rename(source_path, destination_path)
    }
}

pub(crate) struct StagedFile {
    temporary: PathBuf,
    target: PathBuf,
    is_manifest: bool,
}

pub(crate) struct BackupFile {
    backup: PathBuf,
    target: PathBuf,
}

pub(crate) fn write_generated_output_transactionally_with_file_ops(
    output_directory: &Path,
    desired_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    unmanaged_starter_files_by_relative_path: &BTreeMap<String, Vec<u8>>,
    previous_generated_file_manifest: &Manifest,
    unmanaged_starter_files: &[&str],
    file_ops: &impl FileOps,
) -> AppResult<()> {
    fs::create_dir_all(output_directory).map_err(|error| {
        output_error(format!(
            "cannot create output directory {}: {error}",
            output_directory.display()
        ))
    })?;
    let canonical_output_directory = fs::canonicalize(output_directory).map_err(|error| {
        output_error(format!(
            "cannot resolve output directory {}: {error}",
            output_directory.display()
        ))
    })?;

    for generated_relative_path in desired_files_by_relative_path
        .keys()
        .chain(unmanaged_starter_files_by_relative_path.keys())
    {
        validate_generated_relative_path(generated_relative_path)?;
        let generated_file_parent_directory = output_directory
            .join(generated_relative_path)
            .parent()
            .unwrap_or(output_directory)
            .to_path_buf();
        fs::create_dir_all(&generated_file_parent_directory).map_err(|error| {
            output_error(format!(
                "cannot create {}: {error}",
                generated_file_parent_directory.display()
            ))
        })?;
        ensure_path_remains_inside_output_directory(
            &generated_file_parent_directory,
            &canonical_output_directory,
        )?;
    }

    let mut affected_generated_relative_paths = BTreeSet::new();
    affected_generated_relative_paths.extend(desired_files_by_relative_path.keys().cloned());
    for previously_generated_relative_path in &previous_generated_file_manifest.files {
        validate_generated_relative_path(previously_generated_relative_path)?;
        if is_unmanaged_starter_file(unmanaged_starter_files, previously_generated_relative_path) {
            continue;
        }
        affected_generated_relative_paths.insert(previously_generated_relative_path.clone());
    }

    let mut existing_managed_target_paths = Vec::new();
    for generated_relative_path in affected_generated_relative_paths {
        let managed_target_path = output_directory.join(&generated_relative_path);
        match fs::symlink_metadata(&managed_target_path) {
            Ok(managed_target_metadata) => {
                if managed_target_metadata.file_type().is_symlink()
                    || !managed_target_metadata.is_file()
                {
                    return Err(output_error(format!(
                        "refusing to replace unsafe managed path {}",
                        managed_target_path.display()
                    )));
                }
                ensure_path_remains_inside_output_directory(
                    managed_target_path.parent().unwrap_or(output_directory),
                    &canonical_output_directory,
                )?;
                existing_managed_target_paths.push(managed_target_path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(output_error(format!(
                    "cannot inspect managed file {}: {error}",
                    managed_target_path.display()
                )));
            }
        }
    }

    let transaction_directory = create_unique_output_transaction_directory(output_directory)?;
    let staged_files_directory = transaction_directory.join("staged");
    let backup_files_directory = transaction_directory.join("backups");
    if let Err(error) = fs::create_dir(&staged_files_directory)
        .and_then(|()| fs::create_dir(&backup_files_directory))
    {
        let _ = fs::remove_dir_all(&transaction_directory);
        return Err(output_error(format!(
            "cannot prepare output transaction in {}: {error}",
            transaction_directory.display()
        )));
    }

    let mut staged_generated_files = Vec::with_capacity(desired_files_by_relative_path.len());
    for (staging_index, (generated_relative_path, generated_file_contents)) in
        desired_files_by_relative_path.iter().enumerate()
    {
        match stage_generated_file_for_transaction(
            &staged_files_directory,
            output_directory,
            staging_index,
            generated_relative_path,
            generated_file_contents,
        ) {
            Ok(staged_generated_file) => staged_generated_files.push(staged_generated_file),
            Err(error) => {
                let _ = fs::remove_dir_all(&transaction_directory);
                return Err(error);
            }
        }
    }
    let mut next_staging_index = staged_generated_files.len();
    for (generated_relative_path, generated_file_contents) in
        unmanaged_starter_files_by_relative_path
    {
        let starter_target_path = output_directory.join(generated_relative_path);
        if starter_target_path.exists() {
            // Migration: a starter file that an earlier release managed (its
            // recorded hash still matches the file on disk, so the user never
            // edited it) is refreshed once to the new starter content. A
            // user-edited file is always left alone.
            let previously_managed_and_unedited = previous_generated_file_manifest
                .hashes
                .get(generated_relative_path)
                .is_some_and(|recorded_hash| {
                    fs::read(&starter_target_path)
                        .is_ok_and(|contents| &sha256_hex(&contents) == recorded_hash)
                });
            if !previously_managed_and_unedited {
                continue;
            }
        }
        match stage_generated_file_for_transaction(
            &staged_files_directory,
            output_directory,
            next_staging_index,
            generated_relative_path,
            generated_file_contents,
        ) {
            Ok(staged_generated_file) => staged_generated_files.push(staged_generated_file),
            Err(error) => {
                let _ = fs::remove_dir_all(&transaction_directory);
                return Err(error);
            }
        }
        next_staging_index += 1;
    }

    let backup_files: Vec<BackupFile> = existing_managed_target_paths
        .into_iter()
        .enumerate()
        .map(|(backup_index, managed_target_path)| BackupFile {
            backup: backup_files_directory.join(backup_index.to_string()),
            target: managed_target_path,
        })
        .collect();

    match commit_generated_output_transaction(file_ops, &backup_files, &staged_generated_files) {
        Ok(()) => {
            let _ = fs::remove_dir_all(&transaction_directory);
            Ok(())
        }
        Err((error, rollback_was_complete)) => {
            if rollback_was_complete {
                let _ = fs::remove_dir_all(&transaction_directory);
            }
            Err(error)
        }
    }
}

pub(crate) fn create_unique_output_transaction_directory(
    output_directory: &Path,
) -> AppResult<PathBuf> {
    for _ in 0..100 {
        let transaction_sequence_number = NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed);
        let candidate_transaction_directory = output_directory.join(format!(
            ".tokyo-transaction-{}-{transaction_sequence_number}",
            std::process::id()
        ));
        match fs::create_dir(&candidate_transaction_directory) {
            Ok(()) => return Ok(candidate_transaction_directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(output_error(format!(
                    "cannot create output transaction {}: {error}",
                    candidate_transaction_directory.display()
                )));
            }
        }
    }
    Err(output_error(format!(
        "cannot allocate a unique output transaction in {}",
        output_directory.display()
    )))
}

pub(crate) fn stage_generated_file_for_transaction(
    staged_files_directory: &Path,
    output_directory: &Path,
    staging_index: usize,
    generated_relative_path: &str,
    generated_file_contents: &[u8],
) -> AppResult<StagedFile> {
    validate_generated_relative_path(generated_relative_path)?;
    let final_generated_file_path = output_directory.join(generated_relative_path);
    let temporary_staged_file_path = staged_files_directory.join(staging_index.to_string());
    let mut temporary_staged_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_staged_file_path)
        .map_err(|error| {
            output_error(format!(
                "cannot stage {}: {error}",
                final_generated_file_path.display()
            ))
        })?;
    if let Err(error) = temporary_staged_file
        .write_all(generated_file_contents)
        .and_then(|()| temporary_staged_file.sync_all())
    {
        let _ = fs::remove_file(&temporary_staged_file_path);
        return Err(output_error(format!(
            "cannot stage {}: {error}",
            final_generated_file_path.display()
        )));
    }
    Ok(StagedFile {
        temporary: temporary_staged_file_path,
        target: final_generated_file_path,
        is_manifest: generated_relative_path == MANIFEST_FILE,
    })
}

pub(crate) fn commit_generated_output_transaction(
    file_ops: &impl FileOps,
    backup_files: &[BackupFile],
    staged_generated_files: &[StagedFile],
) -> Result<(), (anyhow::Error, bool)> {
    let mut completed_backup_files = Vec::with_capacity(backup_files.len());
    for backup_file in backup_files {
        if let Err(error) = file_ops.rename_path(&backup_file.target, &backup_file.backup) {
            let rollback_errors =
                rollback_generated_output_transaction(file_ops, &completed_backup_files, &[]);
            let rollback_was_complete = rollback_errors.is_empty();
            return Err((
                build_generated_output_transaction_error(
                    format!(
                        "cannot back up managed file {}",
                        backup_file.target.display()
                    ),
                    error,
                    rollback_errors,
                ),
                rollback_was_complete,
            ));
        }
        completed_backup_files.push(backup_file);
    }

    let mut installed_generated_files = Vec::with_capacity(staged_generated_files.len());
    for staged_generated_file in staged_generated_files
        .iter()
        .filter(|staged_generated_file| !staged_generated_file.is_manifest)
        .chain(
            staged_generated_files
                .iter()
                .filter(|staged_generated_file| staged_generated_file.is_manifest),
        )
    {
        if let Err(error) = file_ops.rename_path(
            &staged_generated_file.temporary,
            &staged_generated_file.target,
        ) {
            let rollback_errors = rollback_generated_output_transaction(
                file_ops,
                &completed_backup_files,
                &installed_generated_files,
            );
            let rollback_was_complete = rollback_errors.is_empty();
            return Err((
                build_generated_output_transaction_error(
                    format!(
                        "cannot install generated file {}",
                        staged_generated_file.target.display()
                    ),
                    error,
                    rollback_errors,
                ),
                rollback_was_complete,
            ));
        }
        installed_generated_files.push(staged_generated_file);
    }
    Ok(())
}

pub(crate) fn rollback_generated_output_transaction(
    file_ops: &impl FileOps,
    completed_backup_files: &[&BackupFile],
    installed_generated_files: &[&StagedFile],
) -> Vec<String> {
    let mut rollback_error_messages = Vec::new();
    for installed_generated_file in installed_generated_files.iter().rev() {
        if let Err(error) = file_ops.rename_path(
            &installed_generated_file.target,
            &installed_generated_file.temporary,
        ) {
            rollback_error_messages.push(format!(
                "cannot remove new file {} during rollback: {error}",
                installed_generated_file.target.display()
            ));
        }
    }
    for backup_file in completed_backup_files.iter().rev() {
        if let Err(error) = file_ops.rename_path(&backup_file.backup, &backup_file.target) {
            rollback_error_messages.push(format!(
                "cannot restore {} during rollback: {error}",
                backup_file.target.display()
            ));
        }
    }
    rollback_error_messages
}

pub(crate) fn build_generated_output_transaction_error(
    transaction_error_context: String,
    error: std::io::Error,
    rollback_errors: Vec<String>,
) -> anyhow::Error {
    let rollback_error_suffix = if rollback_errors.is_empty() {
        String::new()
    } else {
        format!("; rollback also failed: {}", rollback_errors.join("; "))
    };
    output_error(format!(
        "{transaction_error_context}: {error}{rollback_error_suffix}"
    ))
}

pub(crate) fn ensure_path_remains_inside_output_directory(
    path_to_validate: &Path,
    canonical_output_directory: &Path,
) -> AppResult<()> {
    let canonical_path_to_validate = fs::canonicalize(path_to_validate).map_err(|error| {
        output_error(format!(
            "cannot resolve {}: {error}",
            path_to_validate.display()
        ))
    })?;
    if !canonical_path_to_validate.starts_with(canonical_output_directory) {
        return Err(output_error(format!(
            "refusing path outside output directory: {}",
            path_to_validate.display()
        )));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let sequence = NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "tokyo-transaction-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FailOneRename {
        fail_at: usize,
        calls: Cell<usize>,
    }

    impl FileOps for FailOneRename {
        fn rename_path(&self, source_path: &Path, destination_path: &Path) -> std::io::Result<()> {
            let rename_call_number = self.calls.get() + 1;
            self.calls.set(rename_call_number);
            if rename_call_number == self.fail_at {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected rename failure",
                ))
            } else {
                fs::rename(source_path, destination_path)
            }
        }
    }

    #[test]
    fn restores_all_managed_paths_when_any_commit_rename_fails() {
        for fail_at in 1..=6 {
            let temp = TestDir::new();
            let output = temp.0.join("sdk");
            fs::create_dir_all(&output).unwrap();
            fs::write(output.join("managed.txt"), "old managed").unwrap();
            fs::write(output.join("stale.txt"), "old stale").unwrap();
            fs::create_dir_all(output.join(MANIFEST_FILE).parent().unwrap()).unwrap();
            fs::write(output.join(MANIFEST_FILE), "old manifest").unwrap();
            fs::write(output.join("user-notes.txt"), "keep me").unwrap();

            let previous = Manifest {
                format_version: FORMAT_VERSION,
                files: vec![
                    MANIFEST_FILE.to_string(),
                    "managed.txt".to_string(),
                    "stale.txt".to_string(),
                ],
                hashes: BTreeMap::new(),
            };
            let desired = BTreeMap::from([
                (MANIFEST_FILE.to_string(), b"new manifest".to_vec()),
                ("managed.txt".to_string(), b"new managed".to_vec()),
                ("new.txt".to_string(), b"new file".to_vec()),
            ]);
            let file_ops = FailOneRename {
                fail_at,
                calls: Cell::new(0),
            };

            let error = write_generated_output_transactionally_with_file_ops(
                &output,
                &desired,
                &BTreeMap::new(),
                &previous,
                &[],
                &file_ops,
            )
            .expect_err("injected failure must abort output update");
            assert!(
                error.to_string().contains("injected rename failure"),
                "{error:?}"
            );
            assert_eq!(
                fs::read_to_string(output.join("managed.txt")).unwrap(),
                "old managed"
            );
            assert_eq!(
                fs::read_to_string(output.join("stale.txt")).unwrap(),
                "old stale"
            );
            assert_eq!(
                fs::read_to_string(output.join(MANIFEST_FILE)).unwrap(),
                "old manifest"
            );
            assert!(!output.join("new.txt").exists());
            assert_eq!(
                fs::read_to_string(output.join("user-notes.txt")).unwrap(),
                "keep me"
            );
            assert!(
                fs::read_dir(&output).unwrap().all(|entry| {
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".tokyo-transaction-")
                }),
                "completed rollback should remove transaction directory"
            );
        }
    }
}
