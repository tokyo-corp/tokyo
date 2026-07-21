//! Project configuration loading and path resolution.

use crate::cli::*;
use crate::error::*;
use crate::prelude::*;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProjectSection {
    pub(crate) name: Option<String>,
    pub(crate) routes: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OpenapiSection {
    pub(crate) source: Option<String>,
    pub(crate) snapshot: Option<String>,
    #[serde(default)]
    #[serde(rename = "headers")]
    pub(crate) _headers: BTreeMap<String, String>,
    /// Legacy direct input support. New projects use `snapshot`.
    pub(crate) input: Option<String>,
    pub(crate) output: Option<String>,
}

pub(crate) struct ProjectConfig {
    pub(crate) project: Option<ProjectSection>,
    pub(crate) openapi: Option<OpenapiSection>,
    pub(crate) codegen: Config,
}
pub(crate) fn load_generation_settings_and_output_directory(
    common_command_arguments: &CommonArgs,
) -> AppResult<(Config, PathBuf)> {
    let config_path = configured_or_default_config_path(common_command_arguments);
    let parsed_config = if let Some(ref config_path) = config_path {
        read_project_config(config_path)?
    } else {
        ProjectConfig {
            project: None,
            openapi: None,
            codegen: Config::default(),
        }
    };
    let is_route_project = parsed_config
        .project
        .as_ref()
        .and_then(|project| project.routes.as_ref())
        .is_some();
    let has_configured_openapi_output = parsed_config
        .openapi
        .as_ref()
        .and_then(|openapi| openapi.output.as_ref())
        .is_some();
    let mut codegen_config = parsed_config.codegen;
    if codegen_config.package.is_none() {
        codegen_config.package = parsed_config
            .project
            .as_ref()
            .and_then(|project| project.name.clone());
    }
    if codegen_config.cli_name.is_none() {
        codegen_config.cli_name = parsed_config
            .project
            .as_ref()
            .and_then(|project| project.name.clone());
    }
    if let Some(config_path) = config_path.as_deref() {
        resolve_scenario_files(&mut codegen_config, config_path)?;
    }
    let configured_output = codegen_config.output.clone();
    let output_directory = common_command_arguments
        .output
        .clone()
        .or_else(|| {
            parsed_config
                .openapi
                .as_ref()
                .and_then(|openapi| openapi.output.as_deref())
                .map(|output| path_relative_to_config(config_path.as_deref(), output))
        })
        .or_else(|| configured_output.map(PathBuf::from))
        .unwrap_or_else(|| {
            if is_route_project && !has_configured_openapi_output {
                config_path
                    .as_deref()
                    .and_then(Path::parent)
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf()
            } else {
                PathBuf::from(DEFAULT_OUTPUT)
            }
        });
    Ok((codegen_config, output_directory))
}

pub(crate) fn configured_or_default_config_path(
    common_command_arguments: &CommonArgs,
) -> Option<PathBuf> {
    if let Some(path) = &common_command_arguments.config {
        return Some(path.clone());
    }
    let mut directory = std::env::current_dir().ok()?;
    loop {
        let candidate = directory.join(DEFAULT_CONFIG);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !directory.pop() {
            return None;
        }
    }
}

pub(crate) fn read_project_config(path: &Path) -> AppResult<ProjectConfig> {
    let config_toml = fs::read_to_string(path)
        .map_err(|error| input_error(format!("cannot read config {}: {error}", path.display())))?;
    let mut root: toml::Value = toml::from_str(&config_toml)
        .map_err(|error| input_error(format!("invalid config {}: {error}", path.display())))?;
    let table = root.as_table_mut().ok_or_else(|| {
        input_error(format!(
            "invalid config {}: expected a TOML table",
            path.display()
        ))
    })?;
    let project = table
        .remove("project")
        .map(toml::Value::try_into)
        .transpose()
        .map_err(|error| {
            input_error(format!(
                "invalid [project] config in {}: {error}",
                path.display()
            ))
        })?;
    let openapi = table
        .remove("openapi")
        .map(toml::Value::try_into)
        .transpose()
        .map_err(|error| {
            input_error(format!(
                "invalid [openapi] config in {}: {error}",
                path.display()
            ))
        })?;
    let codegen = root.try_into().map_err(|error| {
        input_error(format!("invalid legacy config {}: {error}", path.display()))
    })?;
    if let Some(ProjectSection {
        routes: Some(routes),
        ..
    }) = &project
        && (Path::new(routes).is_absolute()
            || Path::new(routes)
                .components()
                .any(|component| !matches!(component, Component::Normal(_))))
    {
        return Err(input_error(format!(
            "invalid [project].routes in {}: expected a relative path without '..'",
            path.display()
        )));
    }
    Ok(ProjectConfig {
        project,
        openapi,
        codegen,
    })
}

pub(crate) fn path_relative_to_config(
    config_path: Option<&Path>,
    configured_path: &str,
) -> PathBuf {
    config_path
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .join(configured_path)
}

pub(crate) fn resolve_openapi_input_path(
    common_command_arguments: &CommonArgs,
) -> AppResult<PathBuf> {
    resolve_optional_openapi_input_path(common_command_arguments)?
        .ok_or_else(|| input_error("this command requires an [openapi].input or --input"))
}

pub(crate) fn resolve_optional_openapi_input_path(
    common_command_arguments: &CommonArgs,
) -> AppResult<Option<PathBuf>> {
    if let Some(input) = &common_command_arguments.input {
        return Ok(Some(input.clone()));
    }
    let config_path = configured_or_default_config_path(common_command_arguments);
    if let Some(config_path) = &config_path {
        let parsed = read_project_config(config_path)?;
        if let Some(openapi) = parsed.openapi {
            if let Some(snapshot) = openapi.snapshot {
                return Ok(Some(path_relative_to_config(Some(config_path), &snapshot)));
            }
            if let Some(input) = openapi.input {
                return Ok(Some(path_relative_to_config(Some(config_path), &input)));
            }
            if openapi.source.is_some() {
                return Err(input_error(format!(
                    "[openapi].source in {} requires [openapi].snapshot; run `tokyo openapi sync` after repairing the config",
                    config_path.display()
                )));
            }
        }
        if parsed
            .project
            .as_ref()
            .and_then(|project| project.routes.as_ref())
            .is_some()
        {
            return Ok(None);
        }
    }
    Ok(Some(PathBuf::from(DEFAULT_INPUT)))
}

pub(crate) fn resolve_configured_routes_directory(
    common_command_arguments: &CommonArgs,
) -> AppResult<Option<PathBuf>> {
    let Some(config_path) = configured_or_default_config_path(common_command_arguments) else {
        return Ok(None);
    };
    let parsed = read_project_config(&config_path)?;
    Ok(parsed
        .project
        .and_then(|project| project.routes)
        .map(|routes| path_relative_to_config(Some(&config_path), &routes)))
}

pub(crate) fn resolve_scenario_files(
    codegen_config: &mut Config,
    config_path: &Path,
) -> AppResult<()> {
    let config_parent_directory = config_path.parent().unwrap_or_else(|| Path::new("."));
    for configured_cli_scenario in &mut codegen_config.cli_scenarios {
        let Some(relative_scenario_file_path) = configured_cli_scenario.file.as_deref() else {
            continue;
        };
        if configured_cli_scenario.body.is_some() {
            return Err(input_error(format!(
                "cli_scenarios {:?} must set exactly one of body or file",
                configured_cli_scenario.name
            )));
        }
        let scenario_file_path = config_parent_directory.join(relative_scenario_file_path);
        configured_cli_scenario.body =
            Some(fs::read_to_string(&scenario_file_path).map_err(|error| {
                input_error(format!(
                    "cannot read cli_scenarios {:?} file {}: {error}",
                    configured_cli_scenario.name,
                    scenario_file_path.display()
                ))
            })?);
        configured_cli_scenario.file = None;
    }
    Ok(())
}
