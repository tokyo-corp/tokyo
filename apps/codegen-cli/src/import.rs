//! OpenAPI input resolution and IR import.

use crate::cli::*;
use crate::config::*;
use crate::error::*;
use crate::prelude::*;

pub(crate) fn import_openapi_input_as_api_ir(
    common_command_arguments: &CommonArgs,
    codegen_config: &Config,
) -> AppResult<Api> {
    let openapi_input_path = resolve_openapi_input_path(common_command_arguments)?;
    let openapi_input_text = fs::read_to_string(&openapi_input_path).map_err(|error| {
        input_error(format!(
            "cannot read OpenAPI input {}: {error}",
            openapi_input_path.display()
        ))
    })?;
    tokyo_codegen_engine::import_openapi_text(
        &openapi_input_text,
        InputFormat::Auto,
        codegen_config,
    )
    .map_err(|error| {
        let import_error_message = match error {
            tokyo_codegen_engine::Error::Import(message) => {
                format!(
                    "cannot import OpenAPI input {}: {message}",
                    openapi_input_path.display()
                )
            }
            other => other.to_string(),
        };
        input_error(import_error_message)
    })
}

pub(crate) fn import_generation_api(
    common_command_arguments: &CommonArgs,
    codegen_config: &Config,
) -> AppResult<Api> {
    if resolve_optional_openapi_input_path(common_command_arguments)?.is_some() {
        import_openapi_input_as_api_ir(common_command_arguments, codegen_config)
    } else {
        let mut api = Api::default();
        tokyo_codegen_engine::apply_codegen_config_to_api(&mut api, codegen_config)
            .map_err(engine_output_error)?;
        api.canonicalize();
        Ok(api)
    }
}
