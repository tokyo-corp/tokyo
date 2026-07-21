//! Renders every command-surface file in the generated CLI except the
//! static ones in `crate::templates`: `src/cli.rs` (the global `Cli`/
//! `Command`) and `src/commands/{module}.rs` (one per OpenAPI tag). Split by
//! concern rather than by output file, since a resource's `Subcommand` enum
//! and the top-level `Cli` both bottom out in per-endpoint rendering:
//!
//! - [`resources`] — resource grouping/naming and each resource's
//!   `Subcommand` enum + `dispatch`/`delete_by_id`.
//! - [`endpoint`] — one endpoint's variant, args, and dispatch arm; used by
//!   [`resources`], not called directly from outside this module.
//! - [`program`] — the top-level `Cli` struct and `run()`/`execute()`.

mod auth;
mod endpoint;
mod program;
mod resources;

pub(crate) use endpoint::request_body_mode;
pub use program::render_generated_cli_source_tokens;
pub use resources::{
    ResolvedDispatchGroup, render_resource, resolve_resource_names, resource_groups,
};
