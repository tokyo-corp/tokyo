//! Typed command errors and process exit-code mapping.

use crate::prelude::*;

#[derive(Debug)]
pub(crate) struct CliExitError {
    code: i32,
    message: String,
}

impl std::fmt::Display for CliExitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliExitError {}

impl CliExitError {
    fn input(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_INPUT,
            message: message.into(),
        }
    }

    fn output(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_OUTPUT,
            message: message.into(),
        }
    }

    fn differences(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_DIFFERENCES,
            message: message.into(),
        }
    }
}

pub(crate) type AppResult<T> = Result<T>;
pub(crate) fn input_error(message: impl Into<String>) -> anyhow::Error {
    CliExitError::input(message).into()
}

pub(crate) fn output_error(message: impl Into<String>) -> anyhow::Error {
    CliExitError::output(message).into()
}

pub(crate) fn differences_error(message: impl Into<String>) -> anyhow::Error {
    CliExitError::differences(message).into()
}
pub(crate) fn exit_code_for_error(error: &anyhow::Error) -> i32 {
    error
        .downcast_ref::<CliExitError>()
        .map_or(EXIT_OUTPUT, |cli_exit_error| cli_exit_error.code)
}
pub(crate) fn engine_output_error(error: tokyo_codegen_engine::Error) -> anyhow::Error {
    output_error(error.to_string())
}

pub(crate) fn json_output_error(error: serde_json::Error) -> anyhow::Error {
    output_error(format!("cannot serialize generated metadata: {error}"))
}
