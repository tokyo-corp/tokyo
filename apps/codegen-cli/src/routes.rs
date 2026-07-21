//! Filesystem route discovery and generated route registry rendering.

use crate::cli::*;
use crate::config::*;
use crate::error::*;
use crate::prelude::*;

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredRoute {
    pub(crate) command_path: Vec<String>,
    pub(crate) source_path: PathBuf,
}
pub(crate) fn normalize_command_component(identifier: &str) -> String {
    identifier.to_kebab_case()
}

pub(crate) fn is_valid_route_identifier(identifier: &str) -> bool {
    let mut characters = identifier.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

pub(crate) fn discover_configured_routes(
    common: &CommonArgs,
    _output_directory: &Path,
    api: &Api,
) -> AppResult<Vec<DiscoveredRoute>> {
    let Some(routes_directory) = resolve_configured_routes_directory(common)? else {
        return Ok(Vec::new());
    };
    if !routes_directory.is_dir() {
        if !api.endpoints.is_empty() {
            return Ok(Vec::new());
        }
        return Err(input_error(format!(
            "configured routes directory {} does not exist",
            routes_directory.display()
        )));
    }

    fn visit(directory: &Path, files: &mut Vec<PathBuf>) -> AppResult<()> {
        let mut entries = fs::read_dir(directory)
            .map_err(|error| input_error(format!("cannot read {}: {error}", directory.display())))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                input_error(format!("cannot read {}: {error}", directory.display()))
            })?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let file_type = entry.file_type().map_err(|error| {
                input_error(format!(
                    "cannot inspect {}: {error}",
                    entry.path().display()
                ))
            })?;
            if file_type.is_symlink() {
                return Err(input_error(format!(
                    "route source paths must not be symlinks: {}",
                    entry.path().display()
                )));
            }
            if file_type.is_dir() {
                visit(&entry.path(), files)?;
            } else if file_type.is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    == Some("rs")
            {
                files.push(entry.path());
            }
        }
        Ok(())
    }

    let mut source_files = Vec::new();
    visit(&routes_directory, &mut source_files)?;
    let relative_files = source_files
        .iter()
        .map(|path| {
            path.strip_prefix(&routes_directory)
                .expect("visited child")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    let relative_set = relative_files.iter().cloned().collect::<BTreeSet<_>>();
    for relative in &relative_files {
        if relative.file_name().and_then(|name| name.to_str()) == Some("mod.rs") {
            let module_file = relative
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .with_extension("rs");
            if !module_file.as_os_str().is_empty() && relative_set.contains(&module_file) {
                return Err(input_error(format!(
                    "ambiguous route module layout: {} conflicts with {}",
                    routes_directory.join(&module_file).display(),
                    routes_directory.join(relative).display()
                )));
            }
        }
    }

    let mut reserved = [
        "achieve",
        "api",
        "auth",
        "profile",
        "env",
        "start",
        "schema",
        "completions",
        "run",
        "reset",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    for endpoint in &api.endpoints {
        if endpoint.tags.is_empty() {
            reserved.insert("default".to_string());
        } else {
            reserved.extend(
                endpoint
                    .tags
                    .iter()
                    .map(|tag| normalize_generated_command_name(tag)),
            );
        }
    }
    reserved.extend(
        api.cli
            .cli_dispatch_groups
            .iter()
            .map(|group| normalize_generated_command_name(&group.resource)),
    );

    let mut discovered = Vec::new();
    let mut normalized_paths = BTreeMap::<Vec<String>, PathBuf>::new();
    for (source_path, relative) in source_files.into_iter().zip(relative_files) {
        if relative.file_name().and_then(|name| name.to_str()) == Some("mod.rs") {
            continue;
        }
        let mut identifiers = Vec::new();
        for component in relative.parent().into_iter().flat_map(Path::components) {
            let Component::Normal(value) = component else {
                return Err(input_error(format!(
                    "invalid route path {}",
                    relative.display()
                )));
            };
            let identifier = value.to_str().ok_or_else(|| {
                input_error(format!("route path is not UTF-8: {}", relative.display()))
            })?;
            identifiers.push(identifier.to_string());
        }
        let stem = relative
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                input_error(format!("route path is not UTF-8: {}", relative.display()))
            })?;
        identifiers.push(stem.to_string());
        if let Some(invalid) = identifiers
            .iter()
            .find(|identifier| !is_valid_route_identifier(identifier))
        {
            return Err(input_error(format!(
                "invalid route identifier {invalid:?} in {}; use Rust identifiers in route paths",
                relative.display()
            )));
        }
        let command_path = identifiers
            .iter()
            .map(|identifier| normalize_command_component(identifier))
            .collect::<Vec<_>>();
        if reserved.contains(&command_path[0]) {
            return Err(input_error(format!(
                "route {} conflicts with reserved or generated top-level command {:?}",
                relative.display(),
                command_path[0]
            )));
        }
        if let Some(previous) =
            normalized_paths.insert(command_path.clone(), relative.to_path_buf())
        {
            return Err(input_error(format!(
                "duplicate normalized route command path {}: {} and {}",
                command_path.join(" "),
                previous.display(),
                relative.display()
            )));
        }
        discovered.push(DiscoveredRoute {
            command_path,
            source_path,
        });
    }
    for left in 0..discovered.len() {
        for right in 0..discovered.len() {
            if left != right
                && discovered[right]
                    .command_path
                    .starts_with(&discovered[left].command_path)
            {
                return Err(input_error(format!(
                    "route command path {} is both a command and a command group",
                    discovered[left].command_path.join(" ")
                )));
            }
        }
    }
    discovered.sort_by(|left, right| left.command_path.cmp(&right.command_path));
    Ok(discovered)
}

pub(crate) fn normalize_generated_command_name(value: &str) -> String {
    value.to_kebab_case()
}

#[derive(Default)]
pub(crate) struct RouteTree {
    pub(crate) route_index: Option<usize>,
    pub(crate) children: BTreeMap<String, RouteTree>,
}

pub(crate) fn render_route_registry(
    routes: &[DiscoveredRoute],
    output_directory: &Path,
) -> AppResult<String> {
    let mut tree = RouteTree::default();
    for (index, route) in routes.iter().enumerate() {
        let mut node = &mut tree;
        for component in &route.command_path {
            node = node.children.entry(component.clone()).or_default();
        }
        node.route_index = Some(index);
    }
    fn command_expression(name: &str, node: &RouteTree) -> String {
        if let Some(index) = node.route_index {
            return format!(
                "{{ let route = route_{index}(); route.spec().command().name({name:?}) }}"
            );
        }
        let mut expression = format!("clap::Command::new({name:?})");
        for (child_name, child) in &node.children {
            expression.push_str(&format!(
                ".subcommand({})",
                command_expression(child_name, child)
            ));
        }
        expression
    }

    let registry_directory = output_directory.join(".tokyo/src/tokyo");
    let mut source = String::from(
        "// Code generated by tokyo-codegen. DO NOT EDIT BY HAND.\n\
         // Route bodies remain developer-owned under the configured routes directory.\n\n",
    );
    for (index, route) in routes.iter().enumerate() {
        let module_path = relative_path_from(&registry_directory, &route.source_path)?;
        source.push_str(&format!(
            "#[path = {:?}]\nmod __route_{index};\n",
            module_path.to_string_lossy()
        ));
    }
    for index in 0..routes.len() {
        source.push_str(&format!(
            "\nfn route_{index}() -> tokyo_cli_runtime::route::Route {{\n    crate::middleware::decorate(__route_{index}::route())\n}}\n"
        ));
    }
    source.push_str("\npub fn augment(mut command: clap::Command) -> clap::Command {\n");
    for (name, node) in &tree.children {
        source.push_str(&format!(
            "    command = command.subcommand({});\n",
            command_expression(name, node)
        ));
    }
    source.push_str("    command\n}\n\n");
    source.push_str(
        "pub fn dispatch(\n    matches: &clap::ArgMatches,\n    context: &crate::cli::CommandContext<'_>,\n) -> Result<bool, crate::error::ClientError> {\n",
    );
    for (index, route) in routes.iter().enumerate() {
        let mut indent = String::from("    ");
        let mut matches_name = String::from("matches");
        for (depth, component) in route.command_path.iter().enumerate() {
            let next = format!("matches_{index}_{depth}");
            source.push_str(&format!(
                "{indent}if let Some(({component:?}, {next})) = {matches_name}.subcommand() {{\n"
            ));
            indent.push_str("    ");
            matches_name = next;
        }
        source.push_str(&format!(
            "{indent}let route = route_{index}();\n\
             {indent}let response = route.run_matches({matches_name}, context.client_optional(), context.output)?;\n\
             {indent}response.render(context.output)?;\n\
             {indent}return Ok(true);\n"
        ));
        for _ in &route.command_path {
            indent.truncate(indent.len() - 4);
            source.push_str(&format!("{indent}}}\n"));
        }
    }
    source.push_str("    Ok(false)\n}\n\n");
    source.push_str("pub fn metadata() -> serde_json::Value {\n    serde_json::json!([\n");
    for (index, route) in routes.iter().enumerate() {
        source.push_str(&format!(
            "        ({{ let route = route_{index}(); serde_json::json!({{\"command\": {:?}, \"name\": route.spec().name(), \"about\": route.spec().description(), \"arguments\": route.spec().arguments().iter().map(|argument| argument.name()).collect::<Vec<_>>()}}) }}),\n",
            route.command_path.join(".")
        ));
    }
    source.push_str("    ])\n}\n");
    Ok(source)
}

pub(crate) fn relative_path_from(from_directory: &Path, target: &Path) -> AppResult<PathBuf> {
    let current = std::env::current_dir()
        .map_err(|error| input_error(format!("cannot resolve current directory: {error}")))?;
    let absolute_from = if from_directory.is_absolute() {
        from_directory.to_path_buf()
    } else {
        current.join(from_directory)
    };
    let absolute_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        current.join(target)
    };
    let from_components = absolute_from.components().collect::<Vec<_>>();
    let target_components = absolute_target.components().collect::<Vec<_>>();
    let common = from_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return Err(input_error(format!(
            "cannot express route path {} relative to {}",
            target.display(),
            from_directory.display()
        )));
    }
    let mut relative = PathBuf::new();
    for _ in common..from_components.len() {
        relative.push("..");
    }
    for component in &target_components[common..] {
        relative.push(component.as_os_str());
    }
    Ok(relative)
}
