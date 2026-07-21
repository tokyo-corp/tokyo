//! Serializable intermediate representation shared by importers, emitters, and
//! the code-generation engine.
//!
//! The IR stores OpenAPI-derived API shape, CLI behavior, authentication,
//! streaming, pagination, and snapshot-diff metadata in a deterministic format.

/// Root API document and snapshot metadata.
pub mod api;
/// Authentication schemes and operation requirements.
pub mod auth;
/// CLI-specific behavior embedded in generated clients.
pub mod cli_behavior;
/// Coarse API snapshot diffing.
pub mod diff;
/// HTTP endpoint, parameter, body, and response model.
pub mod http;
/// Strongly typed identifiers used by the IR graph.
pub mod id;
/// Pagination metadata.
pub mod pagination;
/// SDK-specific behavior embedded in generated clients.
pub mod sdk_behavior;
/// Type declarations and references.
pub mod types;
/// IR invariant validation.
pub mod validation;
/// WebSocket channel model.
pub mod websocket;

pub use api::{Api, IR_SCHEMA_VERSION};
pub use cli_behavior::CliBehavior;
pub use sdk_behavior::SdkBehavior;
pub use validation::ValidationError;
