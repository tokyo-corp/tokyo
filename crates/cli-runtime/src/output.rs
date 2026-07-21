use std::io::IsTerminal as _;
use std::io::Write as _;

thread_local! {
    static CAPTURES: std::cell::RefCell<Vec<Vec<serde_json::Value>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
/// User-selected output format.
pub enum OutputFormat {
    /// A human-readable table (default when stdout is a terminal).
    Text,
    /// Pretty-printed JSON (default when stdout is piped).
    Json,
    /// Compact JSON, no whitespace — smaller payload for piping to another tool.
    JsonRaw,
}

/// Resolved output settings for one command invocation.
pub struct OutputOptions {
    /// Selected serialization/presentation mode.
    pub format: OutputFormat,
    /// Comma-separated column selection; honored in `Text` mode only. Machine
    /// output (`Json`/`JsonRaw`) always returns the complete record — column
    /// selection is a human-presentation concern, filtering the rest is `jq`'s
    /// job, not the CLI's (mirrors doctl's `--output json` vs `--format`).
    pub fields: Option<String>,
}

impl OutputOptions {
    /// TTY-adaptive by default: an explicit `-o` always wins; otherwise a
    /// terminal gets a table and a pipe gets JSON, with no flag required.
    #[must_use]
    pub fn resolve_requested_output_options(
        requested_output_format: Option<OutputFormat>,
        requested_table_fields: Option<String>,
    ) -> Self {
        let resolved_output_format = requested_output_format.unwrap_or_else(|| {
            if std::io::stdout().is_terminal() {
                OutputFormat::Text
            } else {
                OutputFormat::Json
            }
        });
        Self {
            format: resolved_output_format,
            fields: requested_table_fields,
        }
    }
}

/// Prints a typed response according to the requested output options.
pub fn print_serialized_response<T: serde::Serialize>(
    response_value: &T,
    output_options: &OutputOptions,
) {
    let response_json_value =
        serde_json::to_value(response_value).expect("a decoded response always re-serializes");
    if capture_response_value_for_active_scenario(&response_json_value) {
        return;
    }
    match output_options.format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&response_json_value).unwrap()
        ),
        OutputFormat::JsonRaw => {
            println!("{}", serde_json::to_string(&response_json_value).unwrap())
        }
        OutputFormat::Text => {
            print_json_value_as_table(&response_json_value, output_options.fields.as_deref())
        }
    }
}

/// Prints a raw response body as UTF-8 text.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the bytes are not valid UTF-8.
pub fn print_response_body_as_utf8_text(bytes: &[u8]) -> Result<(), crate::error::ClientError> {
    let response_text = std::str::from_utf8(bytes)
        .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
    if capture_response_value_for_active_scenario(&serde_json::Value::String(
        response_text.to_string(),
    )) {
        return Ok(());
    }
    print!("{response_text}");
    if !response_text.ends_with('\n') {
        println!();
    }
    Ok(())
}

/// Writes raw response bytes to stdout.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if stdout cannot be written.
pub fn print_response_body_as_binary_bytes(bytes: &[u8]) -> Result<(), crate::error::ClientError> {
    let mut locked_stdout = std::io::stdout().lock();
    locked_stdout
        .write_all(bytes)
        .and_then(|()| locked_stdout.flush())
        .map_err(|error| crate::error::ClientError::Transport(error.to_string()))
}

/// Prints one streaming text chunk.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if stdout cannot be written.
pub fn print_stream_chunk_as_utf8_text(bytes: &[u8]) -> Result<(), crate::error::ClientError> {
    print_response_body_as_binary_bytes(bytes)
}

/// Prints one stream item as a JSON line.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the item cannot be serialized or
/// stdout cannot be written.
pub fn print_stream_item_as_json_line<T: serde::Serialize>(
    stream_item_value: &T,
) -> Result<(), crate::error::ClientError> {
    let stream_item_json_value = serde_json::to_value(stream_item_value)
        .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
    if capture_response_value_for_active_scenario(&stream_item_json_value) {
        return Ok(());
    }
    let mut locked_stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut locked_stdout, &stream_item_json_value)
        .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
    locked_stdout
        .write_all(b"\n")
        .and_then(|()| locked_stdout.flush())
        .map_err(|error| crate::error::ClientError::Transport(error.to_string()))
}

/// Starts suppressing normal command output and collecting serialized values.
/// Captures form a stack so a scenario can safely invoke another scenario.
pub fn begin_capturing_command_output_for_scenario() {
    CAPTURES.with(|captures| captures.borrow_mut().push(Vec::new()));
}

/// Ends the innermost capture and returns everything commands attempted to
/// print while it was active.
pub fn end_capturing_command_output_for_scenario() -> Vec<serde_json::Value> {
    CAPTURES.with(|captures| captures.borrow_mut().pop().unwrap_or_default())
}

fn capture_response_value_for_active_scenario(response_json_value: &serde_json::Value) -> bool {
    CAPTURES.with(|captures| {
        let mut active_capture_stack = captures.borrow_mut();
        let Some(active_capture_buffer) = active_capture_stack.last_mut() else {
            return false;
        };
        active_capture_buffer.push(response_json_value.clone());
        true
    })
}

/// Prints an untyped response body using content type and output settings.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if response decoding or stdout writing
/// fails.
pub fn print_untyped_response_body_according_to_content_type(
    bytes: &[u8],
    content_type: Option<&str>,
    output_options: &OutputOptions,
) -> Result<(), crate::error::ClientError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let response_media_type = content_type
        .and_then(|content_type_header_value| content_type_header_value.split(';').next())
        .unwrap_or_default()
        .trim();
    if response_media_type == "application/json" || response_media_type.ends_with("+json") {
        let response_json_value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
        print_serialized_response(&response_json_value, output_options);
        Ok(())
    } else if response_media_type.starts_with("text/") {
        print_response_body_as_utf8_text(bytes)
    } else {
        print_response_body_as_binary_bytes(bytes)
    }
}

fn print_json_value_as_table(
    json_value_to_print: &serde_json::Value,
    selected_fields: Option<&str>,
) {
    let table_rows: Vec<&serde_json::Map<String, serde_json::Value>> = match json_value_to_print {
        serde_json::Value::Array(array_items) => array_items
            .iter()
            .filter_map(|array_item| array_item.as_object())
            .collect(),
        serde_json::Value::Object(json_object) => vec![json_object],
        serde_json::Value::Null => return,
        scalar_json_value => {
            println!("{scalar_json_value}");
            return;
        }
    };
    if table_rows.is_empty() {
        return;
    }

    let table_columns: Vec<String> = match selected_fields {
        Some(selected_field_list) => selected_field_list
            .split(',')
            .map(|selected_field| selected_field.trim().to_string())
            .collect(),
        None => table_rows[0].keys().cloned().collect(),
    };
    let table_column_widths: Vec<usize> = table_columns
        .iter()
        .map(|table_column| {
            table_rows
                .iter()
                .map(|table_row| table_cell_text(table_row, table_column).len())
                .chain(std::iter::once(table_column.len()))
                .max()
                .unwrap_or(0)
        })
        .collect();

    let header_cells: Vec<String> = table_columns
        .iter()
        .zip(&table_column_widths)
        .map(|(table_column, table_column_width)| format!("{table_column:<table_column_width$}"))
        .collect();
    println!("{}", header_cells.join("  ").trim_end());
    for table_row in &table_rows {
        let row_cells: Vec<String> = table_columns
            .iter()
            .zip(&table_column_widths)
            .map(|(table_column, table_column_width)| {
                format!(
                    "{:<table_column_width$}",
                    table_cell_text(table_row, table_column)
                )
            })
            .collect();
        println!("{}", row_cells.join("  ").trim_end());
    }
}

fn table_cell_text(
    table_row: &serde_json::Map<String, serde_json::Value>,
    table_column: &str,
) -> String {
    match table_row.get(table_column) {
        Some(serde_json::Value::String(string_cell_value)) => string_cell_value.clone(),
        Some(non_string_cell_value) => non_string_cell_value.to_string(),
        None => String::new(),
    }
}
