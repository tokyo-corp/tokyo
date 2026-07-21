//! Shared CLI defaults, generated-file paths, and format versions.

pub(crate) const EXIT_DIFFERENCES: i32 = 1;
pub(crate) const EXIT_INPUT: i32 = 2;
pub(crate) const EXIT_OUTPUT: i32 = 3;
pub(crate) const DEFAULT_INPUT: &str = "examples/petstore.yaml";
pub(crate) const DEFAULT_OUTPUT: &str = "generated";
pub(crate) const DEFAULT_CONFIG: &str = "tokyo.toml";
pub(crate) const SNAPSHOT_FILE: &str = tokyo_codegen_engine::SNAPSHOT_FILE;
pub(crate) const MANIFEST_FILE: &str = ".tokyo/manifest.json";
/// Paths written by earlier releases, read as fallbacks so existing projects
/// migrate to `.tokyo/` on their next generation.
pub(crate) const LEGACY_MANIFEST_FILE: &str = ".tokyo-manifest.json";
pub(crate) const FORMAT_VERSION: u32 = 1;
