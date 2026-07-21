//! Error type returned by the OpenAPI importer.

use thiserror::Error;

/// Failure encountered while parsing or lowering an OpenAPI document.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ImportError {
    /// JSON input could not be decoded.
    #[error("failed to parse OpenAPI JSON: {0}")]
    ParseJson(#[from] serde_json::Error),

    /// YAML input could not be decoded.
    #[error("failed to parse OpenAPI YAML: {0}")]
    ParseYaml(#[from] yaml_serde::Error),

    /// An OpenAPI `$ref` could not be resolved.
    #[error("failed to resolve $ref: {0}")]
    Ref(#[from] oas3::spec::RefError),

    /// The document is valid OpenAPI but contains a construct this importer
    /// does not yet lower into Tokyo IR.
    #[error("unsupported: {0}")]
    Unsupported(String),
}
