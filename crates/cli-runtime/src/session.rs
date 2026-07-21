use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

fn path_inside_current_cli_session_directory(
    session_file_name: &str,
) -> Result<PathBuf, crate::error::ClientError> {
    Ok(crate::profile::cli_runtime_config_directory()?.join(session_file_name))
}

/// Creates the session directory owner-only. Session files (`last.json`,
/// `transcript.jsonl`) can contain response bodies and request URLs, so the
/// directory must be `0700` even on the first unauthenticated command — before
/// any credential write would otherwise be the first thing to tighten it.
fn create_private_session_directory(directory: &std::path::Path) {
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
    }
}

/// Persists the most recent successful JSON response so a later argument can
/// reference it as `@last:/json/pointer/syntax` instead of the caller
/// re-parsing and re-typing IDs between commands.
pub fn record_json_response_as_last_response(bytes: &[u8]) {
    let Ok(response_json_value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return;
    };
    let Ok(last_response_file_path) = path_inside_current_cli_session_directory("last.json") else {
        return;
    };
    if let Some(parent_directory) = last_response_file_path.parent() {
        create_private_session_directory(parent_directory);
    }
    let _ = std::fs::write(last_response_file_path, response_json_value.to_string());
}

/// Resolves every `@last:/pointer` argument against the last recorded
/// response, before clap ever sees the arguments. An argument that isn't a
/// `@last:` reference, or a pointer that doesn't resolve, is passed through
/// unchanged so clap's own error reporting explains the failure.
pub fn resolve_last_response_references_in_cli_arguments(
    cli_arguments: Vec<String>,
) -> Vec<String> {
    let mut cached_last_response_json_value: Option<serde_json::Value> = None;
    let mut has_attempted_to_load_last_response = false;
    cli_arguments
        .into_iter()
        .map(|cli_argument| {
            let (argument_prefix_before_reference, embedded_reference) =
                split_embedded_argument_reference(&cli_argument, "@last:");
            let Some(json_pointer) = embedded_reference else {
                return cli_argument;
            };
            if !has_attempted_to_load_last_response {
                has_attempted_to_load_last_response = true;
                cached_last_response_json_value = load_last_recorded_json_response();
            }
            match cached_last_response_json_value
                .as_ref()
                .and_then(|last_response_json_value| last_response_json_value.pointer(json_pointer))
            {
                Some(serde_json::Value::String(referenced_string_value)) => {
                    format!("{argument_prefix_before_reference}{referenced_string_value}")
                }
                Some(referenced_json_value) => {
                    format!("{argument_prefix_before_reference}{referenced_json_value}")
                }
                None => cli_argument,
            }
        })
        .collect()
}

/// Returns the most recent successful JSON response, if one was recorded.
pub fn load_last_recorded_json_response() -> Option<serde_json::Value> {
    path_inside_current_cli_session_directory("last.json")
        .ok()
        .and_then(|last_response_file_path| std::fs::read_to_string(last_response_file_path).ok())
        .and_then(|last_response_file_text| serde_json::from_str(&last_response_file_text).ok())
}

/// Resolves `@set:KEY` arguments from values supplied to `run --set`.
/// This intentionally runs before [`resolve_last_refs`], allowing a supplied
/// value to itself be an `@last:/pointer` reference. A reference may be the
/// whole argument or the value side of an argument such as `id=@set:ID`.
pub fn resolve_scenario_set_variable_references_in_cli_arguments(
    cli_arguments: Vec<String>,
    set_variable_values_by_name: &BTreeMap<String, String>,
) -> Vec<String> {
    cli_arguments
        .into_iter()
        .map(|cli_argument| {
            let (argument_prefix_before_reference, embedded_reference) =
                split_embedded_argument_reference(&cli_argument, "@set:");
            embedded_reference
                .and_then(|set_variable_name| set_variable_values_by_name.get(set_variable_name))
                .map(|set_variable_value| {
                    format!("{argument_prefix_before_reference}{set_variable_value}")
                })
                .unwrap_or(cli_argument)
        })
        .collect()
}

/// Resolves `@self:KEY` against safe identity fields projected by the active
/// CLI auth provider. Like `@set:`, references may be whole arguments or the
/// value side of `field=@self:KEY`.
pub fn resolve_authenticated_identity_references_in_cli_arguments(
    cli_arguments: Vec<String>,
    authenticated_identity_json_value: &serde_json::Value,
) -> Vec<String> {
    cli_arguments
        .into_iter()
        .map(|cli_argument| {
            let (argument_prefix_before_reference, embedded_reference) =
                split_embedded_argument_reference(&cli_argument, "@self:");
            embedded_reference
                .and_then(|identity_field_name| {
                    authenticated_identity_json_value.get(identity_field_name)
                })
                .map(|identity_field_value| match identity_field_value {
                    serde_json::Value::String(identity_string_value) => {
                        format!("{argument_prefix_before_reference}{identity_string_value}")
                    }
                    identity_json_value => {
                        format!("{argument_prefix_before_reference}{identity_json_value}")
                    }
                })
                .unwrap_or(cli_argument)
        })
        .collect()
}

fn split_embedded_argument_reference<'a>(
    cli_argument: &'a str,
    reference_marker: &str,
) -> (&'a str, Option<&'a str>) {
    if let Some(reference_value) = cli_argument.strip_prefix(reference_marker) {
        return ("", Some(reference_value));
    }
    match cli_argument.split_once('=') {
        Some((argument_name_before_equals, argument_value_after_equals)) => (
            &cli_argument[..=argument_name_before_equals.len()],
            argument_value_after_equals.strip_prefix(reference_marker),
        ),
        None => ("", None),
    }
}

/// One resource created via a `POST` command this session, recorded so
/// `reset` can undo it later.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CreatedEntry {
    /// Resource name passed to the generated delete command.
    pub resource: String,
    /// Resource identifier captured from the creation response.
    pub id: serde_json::Value,
}

/// Appends one entry to this session's created-resource log.
pub fn append_created_resource_to_session_reset_log(
    resource_name: &str,
    resource_identifier: &serde_json::Value,
) {
    let Ok(created_resources_log_path) = path_inside_current_cli_session_directory("created.jsonl")
    else {
        return;
    };
    if let Some(parent_directory) = created_resources_log_path.parent() {
        create_private_session_directory(parent_directory);
    }
    let Ok(mut created_resources_log_file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(created_resources_log_path)
    else {
        return;
    };
    let created_resource_log_entry =
        serde_json::json!({ "resource": resource_name, "id": resource_identifier });
    let _ = writeln!(created_resources_log_file, "{created_resource_log_entry}");
}

/// Reads and clears this session's created-resource log. `reset` consumes it
/// exactly once, so re-running `reset` with nothing new created is a no-op
/// rather than re-deleting the same IDs.
pub fn take_created_resources_from_session_reset_log() -> Vec<CreatedEntry> {
    let Ok(created_resources_log_path) = path_inside_current_cli_session_directory("created.jsonl")
    else {
        return Vec::new();
    };
    let Ok(created_resources_log_text) = std::fs::read_to_string(&created_resources_log_path)
    else {
        return Vec::new();
    };
    let _ = std::fs::remove_file(&created_resources_log_path);
    created_resources_log_text
        .lines()
        .filter_map(|created_resource_log_line| {
            serde_json::from_str(created_resource_log_line).ok()
        })
        .collect()
}

/// Appends one line describing a completed request to this session's
/// transcript: a plain record of what actually happened, so an agent (or a
/// human) can review a multi-step run afterward instead of re-deriving it
/// from scrollback.
pub fn append_completed_request_to_session_transcript(
    http_method: &str,
    request_url_or_path: &str,
    request_outcome: &str,
    request_duration_ms: u128,
) {
    let Ok(transcript_path) = path_inside_current_cli_session_directory("transcript.jsonl") else {
        return;
    };
    if let Some(parent_directory) = transcript_path.parent() {
        create_private_session_directory(parent_directory);
    }
    let Ok(mut transcript_file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(transcript_path)
    else {
        return;
    };
    let unix_timestamp_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let transcript_entry = serde_json::json!({
        "timestamp": unix_timestamp_seconds,
        "method": http_method,
        "path": request_url_or_path,
        "outcome": request_outcome,
        "duration_ms": request_duration_ms,
    });
    let _ = writeln!(transcript_file, "{transcript_entry}");
}

/// Splits one scenario-file line into argv-style tokens: whitespace
/// separated, with single/double-quoted segments kept intact. Enough for
/// pasting real CLI invocations into a file — not a full POSIX shell grammar.
pub fn split_scenario_file_line_into_cli_arguments(line: &str) -> Vec<String> {
    let mut parsed_cli_arguments = Vec::new();
    let mut current_argument_text = String::new();
    let mut is_inside_argument = false;
    let mut active_quote_character: Option<char> = None;
    let mut remaining_line_characters = line.chars().peekable();
    while let Some(current_character) = remaining_line_characters.next() {
        match active_quote_character {
            Some(quote_character) => {
                if current_character == '\\'
                    && remaining_line_characters.peek() == Some(&quote_character)
                {
                    current_argument_text.push(remaining_line_characters.next().expect("peeked"));
                } else if current_character == quote_character {
                    active_quote_character = None;
                } else {
                    current_argument_text.push(current_character);
                }
            }
            None => match current_character {
                '\'' | '"' => {
                    active_quote_character = Some(current_character);
                    is_inside_argument = true;
                }
                whitespace_character if whitespace_character.is_whitespace() => {
                    if is_inside_argument {
                        parsed_cli_arguments.push(std::mem::take(&mut current_argument_text));
                        is_inside_argument = false;
                    }
                }
                non_whitespace_character => {
                    current_argument_text.push(non_whitespace_character);
                    is_inside_argument = true;
                }
            },
        }
    }
    if is_inside_argument {
        parsed_cli_arguments.push(current_argument_text);
    }
    parsed_cli_arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn session_directory_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = std::env::temp_dir().join(format!("tokyo-session-perms-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let directory = base.join("nested");
        create_private_session_directory(&directory);
        let mode = std::fs::metadata(&directory)
            .expect("session directory should exist")
            .permissions()
            .mode()
            & 0o777;
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(mode, 0o700, "session dir must be owner-only, got {mode:o}");
    }

    #[test]
    fn set_refs_support_field_values_and_can_feed_last_resolution() {
        let values = BTreeMap::from([
            ("ID".to_string(), "123".to_string()),
            ("PREVIOUS".to_string(), "@last:/id".to_string()),
        ]);
        assert_eq!(
            resolve_scenario_set_variable_references_in_cli_arguments(
                vec![
                    "cmd".to_string(),
                    "@set:ID".to_string(),
                    "id=@set:ID".to_string(),
                    "prefix-@set:ID".to_string(),
                    "@set:MISSING".to_string(),
                    "@set:PREVIOUS".to_string(),
                ],
                &values,
            ),
            vec![
                "cmd",
                "123",
                "id=123",
                "prefix-@set:ID",
                "@set:MISSING",
                "@last:/id"
            ]
        );
    }

    #[test]
    fn self_refs_support_whole_arguments_and_field_values() {
        let identity = serde_json::json!({
            "org_id": "org_123",
            "org_role": "owner"
        });
        assert_eq!(
            resolve_authenticated_identity_references_in_cli_arguments(
                vec![
                    "@self:org_id".to_string(),
                    "coordinator_org_id=@self:org_id".to_string(),
                    "@self:missing".to_string(),
                ],
                &identity,
            ),
            ["org_123", "coordinator_org_id=org_123", "@self:missing"]
        );
    }
}
