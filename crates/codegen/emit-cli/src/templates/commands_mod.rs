//! Compatibility module plus developer-owned agent guidance.

pub const COMMANDS_MOD_RS: &str = r#"//! Developer-owned compatibility and guidance module.
//!
//! Tokyo does not overwrite this file after the initial scaffold. Generated
//! API commands live in `crate::tokyo::commands`; new handwritten commands
//! live as filesystem routes under `src/routes/**`.

pub mod custom;
pub mod guidance;
"#;
