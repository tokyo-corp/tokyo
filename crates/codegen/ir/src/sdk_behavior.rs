use serde::{Deserialize, Serialize};

/// Configuration consumed by generated SDK targets.
///
/// Mirrors [`crate::cli_behavior::CliBehavior`] for the SDK output family:
/// target-facing knobs applied by the engine from user configuration, carried
/// in the IR so SDK emitters see one input value.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SdkBehavior {
    /// Generated package name (e.g. an npm package name).
    pub package_name: Option<String>,
    /// Exported client type name (e.g. `Stripe` for `new Stripe(...)`).
    pub client_name: Option<String>,
    /// Default API base URL baked into the generated client.
    pub base_url: Option<String>,
}
