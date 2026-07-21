use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Scenario file discovered from configured scenario search paths.
pub struct FileScenario {
    /// Scenario name, derived from the filename.
    pub name: String,
    /// Human-facing description from scenario metadata.
    pub description: String,
    /// Named environments where the scenario may run.
    pub allowed_environments: Vec<String>,
    /// Optional usage hint from scenario metadata.
    pub usage: Option<String>,
    /// Whether the scenario is marked as featured.
    pub featured: bool,
    /// Raw scenario program body.
    pub body: String,
    /// Filesystem path where the scenario was loaded from.
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed scenario program step.
pub enum Step {
    /// Execute a generated CLI command line.
    Command {
        /// Source line number.
        line: usize,
        /// Command text.
        text: String,
    },
    /// Repeat a nested block.
    Repeat {
        /// Source line number.
        line: usize,
        /// Repeat count expression.
        count: String,
        /// Nested steps.
        steps: Vec<Step>,
    },
    /// Assign a variable.
    Let {
        /// Source line number.
        line: usize,
        /// Variable name.
        name: String,
        /// Variable value expression.
        value: String,
    },
    /// Collect a value from the last command response.
    Collect {
        /// Source line number.
        line: usize,
        /// Variable name.
        name: String,
        /// JSON pointer or expression to collect.
        value: String,
    },
    /// Add a named value to structured scenario output.
    Output {
        /// Source line number.
        line: usize,
        /// Output field name.
        name: String,
        /// Output value expression.
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed scenario program.
pub struct Program {
    /// Ordered top-level steps.
    pub steps: Vec<Step>,
}

#[derive(Debug, thiserror::Error)]
/// Scenario parsing or execution failure.
pub enum Error {
    /// Scenario syntax is invalid.
    #[error("scenario line {line}: {message}")]
    Syntax {
        /// Source line number.
        line: usize,
        /// Human-readable syntax error.
        message: String,
    },
    /// Scenario expression evaluation failed.
    #[error("scenario line {line}: {message}")]
    Evaluation {
        /// Source line number.
        line: usize,
        /// Human-readable evaluation error.
        message: String,
    },
    /// A nested CLI command failed.
    #[error("scenario line {line} failed: {message}")]
    Command {
        /// Source line number.
        line: usize,
        /// Human-readable command failure.
        message: String,
    },
}

impl Program {
    /// Parses a scenario program.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when scenario syntax is invalid.
    #[must_use = "parsed programs or parse errors should be handled"]
    pub fn parse_scenario_program(scenario_file_text: &str) -> Result<Self, Error> {
        let indexed_scenario_lines = scenario_file_text.lines().enumerate().collect::<Vec<_>>();
        let mut next_line_index_to_parse = 0;
        let parsed_scenario_steps = parse_scenario_step_block(
            &indexed_scenario_lines,
            &mut next_line_index_to_parse,
            false,
        )?;
        Ok(Self {
            steps: parsed_scenario_steps,
        })
    }

    /// Executes the scenario with a command callback.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when expression evaluation fails or the callback
    /// reports a failed command.
    #[must_use = "scenario execution output or errors should be handled"]
    pub fn execute<F>(
        &self,
        scenario_set_values: &BTreeMap<String, String>,
        mut execute_cli_command_line: F,
    ) -> Result<Value, Error>
    where
        F: FnMut(usize, &str) -> Result<Option<Value>, String>,
    {
        let mut scenario_execution_state = State {
            scenario_set_values,
            scenario_variables: BTreeMap::new(),
            last_command_response: None,
            structured_output_fields: Map::new(),
        };
        execute_scenario_steps(
            &self.steps,
            &mut scenario_execution_state,
            &mut execute_cli_command_line,
        )?;
        Ok(
            if scenario_execution_state.structured_output_fields.is_empty() {
                scenario_execution_state
                    .last_command_response
                    .unwrap_or(Value::Null)
            } else {
                Value::Object(scenario_execution_state.structured_output_fields)
            },
        )
    }
}

/// Discovers scenario files from environment, config, and project locations.
#[must_use]
pub fn discover_scenario_files() -> Vec<FileScenario> {
    let mut discovered_scenarios_by_name = BTreeMap::<String, FileScenario>::new();
    let runtime_config = crate::config::runtime_config();
    let scenario_path_environment_variable_name =
        format!("{}_SCENARIO_PATH", runtime_config.identity.env_prefix);

    if let Some(configured_scenario_search_paths) =
        std::env::var_os(scenario_path_environment_variable_name)
    {
        for scenario_search_directory in std::env::split_paths(&configured_scenario_search_paths) {
            load_scenario_files_from_directory(
                &scenario_search_directory,
                &mut discovered_scenarios_by_name,
            );
        }
    }
    if let Ok(runtime_config_directory) = crate::profile::cli_runtime_config_directory() {
        load_scenario_files_from_directory(
            &runtime_config_directory.join("scenarios"),
            &mut discovered_scenarios_by_name,
        );
    }
    if let Ok(current_working_directory) = std::env::current_dir() {
        load_scenario_files_from_directory(
            &current_working_directory
                .join(format!(".{}", runtime_config.identity.command_name))
                .join("scenarios"),
            &mut discovered_scenarios_by_name,
        );
    }

    discovered_scenarios_by_name.into_values().collect()
}

/// Loads one scenario file and parses its metadata.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the file cannot be read.
#[must_use = "scenario load errors should be handled"]
pub fn load_scenario_file_from_path(
    scenario_file_path: &Path,
) -> Result<FileScenario, crate::error::ClientError> {
    let scenario_file_body = std::fs::read_to_string(scenario_file_path).map_err(|error| {
        crate::error::ClientError::Transport(format!("{}: {error}", scenario_file_path.display()))
    })?;
    Ok(parse_scenario_file_metadata(
        scenario_file_path.to_path_buf(),
        scenario_file_body,
    ))
}

fn load_scenario_files_from_directory(
    scenario_directory: &Path,
    discovered_scenarios_by_name: &mut BTreeMap<String, FileScenario>,
) {
    let Ok(directory_entries) = std::fs::read_dir(scenario_directory) else {
        return;
    };
    let mut scenario_file_paths = directory_entries
        .filter_map(Result::ok)
        .map(|directory_entry| directory_entry.path())
        .filter(|candidate_path| {
            candidate_path
                .extension()
                .is_some_and(|extension| extension == "scenario")
        })
        .collect::<Vec<_>>();
    scenario_file_paths.sort();
    for scenario_file_path in scenario_file_paths {
        let Ok(scenario_file_body) = std::fs::read_to_string(&scenario_file_path) else {
            continue;
        };
        let scenario_file_metadata =
            parse_scenario_file_metadata(scenario_file_path, scenario_file_body);
        discovered_scenarios_by_name
            .insert(scenario_file_metadata.name.clone(), scenario_file_metadata);
    }
}

fn parse_scenario_file_metadata(
    scenario_file_path: PathBuf,
    scenario_file_body: String,
) -> FileScenario {
    let scenario_name = scenario_file_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("scenario")
        .to_string();
    let mut scenario_description = String::new();
    let mut allowed_environment_names = Vec::new();
    let scenario_usage_hint = extract_scenario_usage_hint(&scenario_file_body);
    let scenario_should_be_featured = scenario_is_marked_featured(&scenario_file_body);
    for scenario_metadata_line in scenario_file_body.lines() {
        let trimmed_metadata_line = scenario_metadata_line.trim();
        if let Some(description_value) = trimmed_metadata_line.strip_prefix("# description:") {
            scenario_description = description_value.trim().to_string();
        } else if let Some(allowed_environments_value) =
            trimmed_metadata_line.strip_prefix("# allowed-environments:")
        {
            allowed_environment_names = allowed_environments_value
                .split(',')
                .map(str::trim)
                .filter(|environment_name| !environment_name.is_empty())
                .map(str::to_string)
                .collect();
        } else if !trimmed_metadata_line.is_empty() && !trimmed_metadata_line.starts_with('#') {
            break;
        }
    }
    FileScenario {
        name: scenario_name,
        description: scenario_description,
        allowed_environments: allowed_environment_names,
        usage: scenario_usage_hint,
        featured: scenario_should_be_featured,
        body: scenario_file_body,
        path: scenario_file_path,
    }
}

/// Extracts the optional usage hint from scenario metadata.
#[must_use]
pub fn extract_scenario_usage_hint(scenario_file_body: &str) -> Option<String> {
    scenario_file_body.lines().find_map(|scenario_file_line| {
        scenario_file_line
            .trim()
            .strip_prefix("# usage:")
            .map(str::trim)
            .filter(|usage_hint| !usage_hint.is_empty())
            .map(str::to_string)
    })
}

/// Returns whether scenario metadata marks the scenario as featured.
#[must_use]
pub fn scenario_is_marked_featured(scenario_file_body: &str) -> bool {
    scenario_file_body.lines().any(|scenario_file_line| {
        scenario_file_line
            .trim()
            .strip_prefix("# featured:")
            .is_some_and(|featured_flag_value| {
                featured_flag_value.trim().eq_ignore_ascii_case("true")
            })
    })
}

/// Resource command groups referenced by executable lines in a scenario.
pub fn extract_resource_names_referenced_by_scenario(scenario_file_body: &str) -> Vec<String> {
    scenario_file_body
        .lines()
        .map(str::trim)
        .filter(|trimmed_line| {
            !trimmed_line.is_empty()
                && !trimmed_line.starts_with('#')
                && !trimmed_line.starts_with('@')
        })
        .filter(|trimmed_line| !trimmed_line.starts_with("--repeat") && *trimmed_line != "--end")
        .filter_map(|scenario_command_line| {
            crate::session::split_scenario_file_line_into_cli_arguments(scenario_command_line)
                .into_iter()
                .next()
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn parse_scenario_step_block(
    indexed_scenario_lines: &[(usize, &str)],
    next_line_index_to_parse: &mut usize,
    is_nested_repeat_block: bool,
) -> Result<Vec<Step>, Error> {
    let mut parsed_scenario_steps = Vec::new();
    while let Some((zero_based_line_index, raw_scenario_line)) = indexed_scenario_lines
        .get(*next_line_index_to_parse)
        .copied()
    {
        *next_line_index_to_parse += 1;
        let one_based_line_number = zero_based_line_index + 1;
        let trimmed_scenario_line = raw_scenario_line.trim();
        if trimmed_scenario_line.is_empty() {
            continue;
        }
        if matches!(
            trimmed_scenario_line,
            "@end" | "--end" | "# end" | "# endrepeat"
        ) {
            if is_nested_repeat_block {
                return Ok(parsed_scenario_steps);
            }
            return build_scenario_syntax_error(one_based_line_number, "unexpected @end");
        }
        let repeat_count_expression = trimmed_scenario_line
            .strip_prefix("@repeat ")
            .or_else(|| trimmed_scenario_line.strip_prefix("--repeat "))
            .or_else(|| trimmed_scenario_line.strip_prefix("# repeat:"));
        if let Some(repeat_count_expression) = repeat_count_expression {
            let trimmed_repeat_count_expression = repeat_count_expression.trim();
            if trimmed_repeat_count_expression.is_empty() {
                return build_scenario_syntax_error(
                    one_based_line_number,
                    "@repeat requires a count",
                );
            }
            let repeat_body_steps =
                parse_scenario_step_block(indexed_scenario_lines, next_line_index_to_parse, true)?;
            parsed_scenario_steps.push(Step::Repeat {
                line: one_based_line_number,
                count: trimmed_repeat_count_expression.to_string(),
                steps: repeat_body_steps,
            });
            continue;
        }
        if trimmed_scenario_line.starts_with('#') {
            continue;
        }
        for (directive_prefix, directive_kind_code) in
            [("@let ", 0_u8), ("@collect ", 1_u8), ("@output ", 2_u8)]
        {
            if let Some(binding_expression) = trimmed_scenario_line.strip_prefix(directive_prefix) {
                let (binding_name, binding_value_expression) = binding_expression
                    .split_once('=')
                    .ok_or_else(|| Error::Syntax {
                    line: one_based_line_number,
                    message: format!("{directive_prefix}requires NAME=VALUE"),
                })?;
                let trimmed_binding_name = binding_name.trim();
                let trimmed_binding_value_expression = binding_value_expression.trim();
                if trimmed_binding_name.is_empty() || trimmed_binding_value_expression.is_empty() {
                    return build_scenario_syntax_error(
                        one_based_line_number,
                        "binding name and value must not be empty",
                    );
                }
                parsed_scenario_steps.push(match directive_kind_code {
                    0 => Step::Let {
                        line: one_based_line_number,
                        name: trimmed_binding_name.to_string(),
                        value: trimmed_binding_value_expression.to_string(),
                    },
                    1 => Step::Collect {
                        line: one_based_line_number,
                        name: trimmed_binding_name.to_string(),
                        value: trimmed_binding_value_expression.to_string(),
                    },
                    _ => Step::Output {
                        line: one_based_line_number,
                        name: trimmed_binding_name.to_string(),
                        value: trimmed_binding_value_expression.to_string(),
                    },
                });
                break;
            }
        }
        if !trimmed_scenario_line.starts_with("@let ")
            && !trimmed_scenario_line.starts_with("@collect ")
            && !trimmed_scenario_line.starts_with("@output ")
        {
            parsed_scenario_steps.push(Step::Command {
                line: one_based_line_number,
                text: trimmed_scenario_line.to_string(),
            });
        }
    }
    if is_nested_repeat_block {
        return build_scenario_syntax_error(
            indexed_scenario_lines
                .last()
                .map_or(1, |(zero_based_line_index, _)| zero_based_line_index + 1),
            "unterminated @repeat block (expected @end)",
        );
    }
    Ok(parsed_scenario_steps)
}

struct State<'a> {
    scenario_set_values: &'a BTreeMap<String, String>,
    scenario_variables: BTreeMap<String, Value>,
    last_command_response: Option<Value>,
    structured_output_fields: Map<String, Value>,
}

fn execute_scenario_steps<F>(
    scenario_steps_to_execute: &[Step],
    scenario_execution_state: &mut State<'_>,
    execute_cli_command_line: &mut F,
) -> Result<(), Error>
where
    F: FnMut(usize, &str) -> Result<Option<Value>, String>,
{
    for scenario_step in scenario_steps_to_execute {
        match scenario_step {
            Step::Command { line, text } => {
                let interpolated_command_text =
                    interpolate_scenario_command_text(text, scenario_execution_state);
                if let Some(command_response_json) =
                    execute_cli_command_line(*line, &interpolated_command_text).map_err(
                        |message| Error::Command {
                            line: *line,
                            message,
                        },
                    )?
                {
                    scenario_execution_state.last_command_response = Some(command_response_json);
                }
            }
            Step::Repeat { line, count, steps } => {
                let repeat_count_json_value =
                    resolve_scenario_expression_value(count, scenario_execution_state).ok_or_else(
                        || Error::Evaluation {
                            line: *line,
                            message: format!("cannot resolve repeat count {count:?}"),
                        },
                    )?;
                let repeat_iteration_count = render_scenario_value_as_argument_text(
                    &repeat_count_json_value,
                )
                .parse::<usize>()
                .map_err(|_| Error::Evaluation {
                    line: *line,
                    message: format!(
                        "repeat count must be a non-negative integer, got {repeat_count_json_value}"
                    ),
                })?;
                if repeat_iteration_count > 10_000 {
                    return Err(Error::Evaluation {
                        line: *line,
                        message: "repeat count exceeds the 10000 iteration limit".to_string(),
                    });
                }
                let previous_zero_based_loop_index_value = scenario_execution_state
                    .scenario_variables
                    .get("i")
                    .cloned();
                let previous_one_based_loop_index_value = scenario_execution_state
                    .scenario_variables
                    .get("index")
                    .cloned();
                for zero_based_loop_index in 0..repeat_iteration_count {
                    scenario_execution_state
                        .scenario_variables
                        .insert("i".to_string(), Value::from(zero_based_loop_index));
                    scenario_execution_state
                        .scenario_variables
                        .insert("index".to_string(), Value::from(zero_based_loop_index + 1));
                    execute_scenario_steps(
                        steps,
                        scenario_execution_state,
                        execute_cli_command_line,
                    )?;
                }
                restore_previous_loop_variable_value(
                    &mut scenario_execution_state.scenario_variables,
                    "i",
                    previous_zero_based_loop_index_value,
                );
                restore_previous_loop_variable_value(
                    &mut scenario_execution_state.scenario_variables,
                    "index",
                    previous_one_based_loop_index_value,
                );
            }
            Step::Let { line, name, value } => {
                let resolved_binding_value = scenario_execution_state
                    .scenario_set_values
                    .get(name)
                    .map(|set_value| parse_scenario_scalar_value(set_value))
                    .map(Ok)
                    .unwrap_or_else(|| {
                        resolve_required_scenario_expression_value(
                            *line,
                            value,
                            scenario_execution_state,
                        )
                    })?;
                scenario_execution_state
                    .scenario_variables
                    .insert(name.clone(), resolved_binding_value);
            }
            Step::Collect { line, name, value } => {
                let collected_output_value = resolve_required_scenario_expression_value(
                    *line,
                    value,
                    scenario_execution_state,
                )?;
                scenario_execution_state
                    .structured_output_fields
                    .entry(name.clone())
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
                    .expect("collect output is always initialized as an array")
                    .push(collected_output_value);
            }
            Step::Output { line, name, value } => {
                let output_field_value = resolve_required_scenario_expression_value(
                    *line,
                    value,
                    scenario_execution_state,
                )?;
                scenario_execution_state
                    .structured_output_fields
                    .insert(name.clone(), output_field_value);
            }
        }
    }
    Ok(())
}

fn resolve_required_scenario_expression_value(
    one_based_line_number: usize,
    scenario_expression: &str,
    scenario_execution_state: &State<'_>,
) -> Result<Value, Error> {
    resolve_scenario_expression_value(scenario_expression, scenario_execution_state).ok_or_else(
        || Error::Evaluation {
            line: one_based_line_number,
            message: format!("cannot resolve {scenario_expression:?}"),
        },
    )
}

fn resolve_scenario_expression_value(
    scenario_expression: &str,
    scenario_execution_state: &State<'_>,
) -> Option<Value> {
    if let Some(set_value_name) = scenario_expression.strip_prefix("@set:") {
        return scenario_execution_state
            .scenario_set_values
            .get(set_value_name)
            .map(|set_value| parse_scenario_scalar_value(set_value));
    }
    if let Some(variable_name) = scenario_expression.strip_prefix("@var:") {
        return scenario_execution_state
            .scenario_variables
            .get(variable_name)
            .cloned();
    }
    if let Some(last_response_json_pointer) = scenario_expression.strip_prefix("@last:") {
        return scenario_execution_state
            .last_command_response
            .as_ref()
            .and_then(|last_response_json| last_response_json.pointer(last_response_json_pointer))
            .cloned();
    }
    if scenario_expression.starts_with("{{") && scenario_expression.ends_with("}}") {
        let interpolated_name = &scenario_expression[2..scenario_expression.len() - 2];
        return scenario_execution_state
            .scenario_variables
            .get(interpolated_name)
            .cloned()
            .or_else(|| {
                scenario_execution_state
                    .scenario_set_values
                    .get(interpolated_name)
                    .map(|set_value| parse_scenario_scalar_value(set_value))
            });
    }
    Some(parse_scenario_scalar_value(scenario_expression))
}

fn interpolate_scenario_command_text(
    scenario_command_text: &str,
    scenario_execution_state: &State<'_>,
) -> String {
    let mut rendered_command_text = scenario_command_text.to_string();
    for (set_value_name, set_value) in scenario_execution_state.scenario_set_values {
        rendered_command_text =
            rendered_command_text.replace(&format!("{{{{{set_value_name}}}}}"), set_value);
    }
    for (variable_name, variable_value) in &scenario_execution_state.scenario_variables {
        rendered_command_text = rendered_command_text.replace(
            &format!("{{{{{variable_name}}}}}"),
            &render_scenario_value_as_argument_text(variable_value),
        );
        rendered_command_text = rendered_command_text.replace(
            &format!("@var:{variable_name}"),
            &render_scenario_value_as_argument_text(variable_value),
        );
    }
    rendered_command_text
}

fn parse_scenario_scalar_value(raw_scenario_value: &str) -> Value {
    serde_json::from_str(raw_scenario_value)
        .unwrap_or_else(|_| Value::String(raw_scenario_value.to_string()))
}

fn render_scenario_value_as_argument_text(scenario_value: &Value) -> String {
    match scenario_value {
        Value::String(string_value) => string_value.clone(),
        non_string_json_value => non_string_json_value.to_string(),
    }
}

fn restore_previous_loop_variable_value(
    scenario_variables: &mut BTreeMap<String, Value>,
    loop_variable_name: &str,
    previous_loop_variable_value: Option<Value>,
) {
    match previous_loop_variable_value {
        Some(previous_loop_variable_value) => {
            scenario_variables.insert(loop_variable_name.to_string(), previous_loop_variable_value);
        }
        None => {
            scenario_variables.remove(loop_variable_name);
        }
    }
}

fn build_scenario_syntax_error<T>(
    one_based_line_number: usize,
    error_message: impl Into<String>,
) -> Result<T, Error> {
    Err(Error::Syntax {
        line: one_based_line_number,
        message: error_message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_commands_and_builds_structured_output() {
        let program = Program::parse_scenario_program(
            r#"
@repeat {{count}}
create --field email=driver-{{i}}@test.com
@let id=@last:/id
@collect driver_ids=@var:id
@end
@output shipper=@set:shipper
"#,
        )
        .unwrap();
        let sets = BTreeMap::from([
            ("count".to_string(), "2".to_string()),
            ("shipper".to_string(), "org_1".to_string()),
        ]);
        let mut commands = Vec::new();
        let output = program
            .execute(&sets, |_, command| {
                commands.push(command.to_string());
                Ok(Some(
                    serde_json::json!({ "id": format!("driver_{}", commands.len()) }),
                ))
            })
            .unwrap();

        assert_eq!(
            commands,
            [
                "create --field email=driver-0@test.com",
                "create --field email=driver-1@test.com"
            ]
        );
        assert_eq!(
            output,
            serde_json::json!({
                "driver_ids": ["driver_1", "driver_2"],
                "shipper": "org_1"
            })
        );
    }

    #[test]
    fn rejects_unclosed_repeat() {
        assert!(Program::parse_scenario_program("@repeat 2\nitems list\n").is_err());
    }

    #[test]
    fn extracts_resource_groups_from_recipe_commands() {
        assert_eq!(
            extract_resource_names_referenced_by_scenario(
                "# comment\n@repeat 2\nshipping-drivers create\nshipping-orders list\n@end\n"
            ),
            ["shipping-drivers", "shipping-orders"]
        );
    }

    #[test]
    fn set_values_override_derived_let_bindings() {
        let program = Program::parse_scenario_program(
            "@let org_id=@last:/org_id\n@output org_id=@var:org_id\n",
        )
        .unwrap();
        let output = program
            .execute(
                &BTreeMap::from([("org_id".to_string(), "org_override".to_string())]),
                |_, _| Ok(None),
            )
            .unwrap();
        assert_eq!(output, serde_json::json!({ "org_id": "org_override" }));
    }
}
