//! Transactional Tokyo project initialization.

use crate::cli::*;
use crate::emit::*;
use crate::error::*;
use crate::manifest::*;
use crate::prelude::*;
use crate::routes::*;

pub(crate) fn run_init_command(arguments: InitArgs) -> AppResult<()> {
    validate_project_name(&arguments.name)?;
    let files = materialize_project_files(&arguments.name, &arguments.directory)?;
    install_scaffold(&arguments.directory, &files)?;
    println!(
        "initialized Tokyo project {} in {}",
        arguments.name,
        arguments.directory.display()
    );
    Ok(())
}

pub(crate) fn materialize_project_files(
    name: &str,
    output_directory: &Path,
) -> AppResult<BTreeMap<String, Vec<u8>>> {
    let mut codegen_config = Config {
        package: Some(name.to_string()),
        cli_name: Some(name.to_string()),
        ..Config::default()
    };
    codegen_config.output = None;
    let mut api = Api::default();
    tokyo_codegen_engine::apply_codegen_config_to_api(&mut api, &codegen_config)
        .map_err(engine_output_error)?;
    api.canonicalize();

    let routes = [DiscoveredRoute {
        command_path: vec!["index".to_string()],
        source_path: output_directory.join("src/routes/index.rs"),
    }];
    let desired = build_desired_generated_files_by_relative_path(&api, &routes, output_directory)?;
    let mut files = scaffold_files(name);
    files.extend(desired.unmanaged_starter_files_by_relative_path);
    files.extend(desired.managed_files_by_relative_path);
    Ok(files)
}

pub(crate) fn validate_project_name(name: &str) -> AppResult<()> {
    let valid = !name.is_empty()
        && !name.starts_with(|character: char| character.is_ascii_digit())
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(input_error(format!(
            "invalid project name {name:?}; use ASCII letters, digits, '-' or '_', starting with a letter"
        )))
    }
}

pub(crate) fn scaffold_files(name: &str) -> BTreeMap<String, Vec<u8>> {
    let files = [
        (".gitignore", "/target\n/.tokyo/bin\n"),
        (
            "tokyo.toml",
            &format!("[project]\nname = {name:?}\nroutes = \"src/routes\"\n"),
        ),
        ("src/routes/mod.rs", "pub mod index;\n"),
        (
            "src/routes/index.rs",
            "use tokyo_cli_runtime::prelude::*;\n\n/// Defines the default local route.\npub fn route() -> Route {\n    Route::new(RouteSpec::new(\"index\").about(\"Print a greeting\"), |_| {\n        Ok(RouteResponse::text(\"Hello from Tokyo\"))\n    })\n}\n",
        ),
    ];
    let files: BTreeMap<String, Vec<u8>> = files
        .into_iter()
        .map(|(path, contents)| (path.to_string(), contents.as_bytes().to_vec()))
        .collect();
    files
}

pub(crate) fn install_scaffold(root: &Path, files: &BTreeMap<String, Vec<u8>>) -> AppResult<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(output_error(format!(
                "refusing to initialize unsafe project directory {}",
                root.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(output_error(format!(
                "cannot inspect project directory {}: {error}",
                root.display()
            )));
        }
    }

    for relative_path in files.keys() {
        validate_generated_relative_path(relative_path)?;
        let target = root.join(relative_path);
        if fs::symlink_metadata(&target).is_ok() {
            return Err(output_error(format!(
                "refusing to overwrite existing path {}",
                target.display()
            )));
        }
        let mut parent = target.parent();
        while let Some(path) = parent {
            if path == root {
                break;
            }
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(output_error(format!(
                        "refusing unsafe scaffold path {}",
                        path.display()
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(output_error(format!(
                        "cannot inspect scaffold path {}: {error}",
                        path.display()
                    )));
                }
            }
            parent = path.parent();
        }
    }

    let mut created_directories = Vec::new();
    let mut created_files = Vec::new();
    let result = (|| -> AppResult<()> {
        if !root.exists() {
            fs::create_dir(root).map_err(|error| {
                output_error(format!("cannot create {}: {error}", root.display()))
            })?;
            created_directories.push(root.to_path_buf());
        }
        for (relative_path, contents) in files {
            let target = root.join(relative_path);
            let mut missing_parents = Vec::new();
            let mut parent = target.parent();
            while let Some(path) = parent {
                if path.exists() {
                    break;
                }
                missing_parents.push(path.to_path_buf());
                parent = path.parent();
            }
            for directory in missing_parents.into_iter().rev() {
                fs::create_dir(&directory).map_err(|error| {
                    output_error(format!("cannot create {}: {error}", directory.display()))
                })?;
                created_directories.push(directory);
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|error| {
                    output_error(format!("cannot create {}: {error}", target.display()))
                })?;
            created_files.push(target.clone());
            file.write_all(contents).map_err(|error| {
                output_error(format!("cannot write {}: {error}", target.display()))
            })?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        for path in created_files.iter().rev() {
            let _ = fs::remove_file(path);
        }
        for path in created_directories.iter().rev() {
            let _ = fs::remove_dir(path);
        }
        return Err(error);
    }
    Ok(())
}
