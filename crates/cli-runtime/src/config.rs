use std::sync::OnceLock;

use crate::oauth::OAuthProvider;

/// Stable identity used for config paths and keychain service names.
#[derive(Clone, Copy, Debug)]
pub struct ProductIdentity {
    /// Generated Cargo package name.
    pub package_name: &'static str,
    /// Installed command name.
    pub command_name: &'static str,
    /// Environment variable prefix used by the generated CLI.
    pub env_prefix: &'static str,
}

/// API-specific values compiled into a generated CLI.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfig {
    /// Product identity used for paths, keychain names, and messages.
    pub identity: ProductIdentity,
    /// Default API base URL.
    pub default_base_url: Option<&'static str>,
    /// Named environments accepted by the generated CLI.
    pub environments: &'static [(&'static str, &'static str)],
    /// OAuth providers embedded in the generated CLI.
    pub oauth_providers: &'static [OAuthProvider],
    /// Scenario programs embedded in the generated CLI.
    pub scenarios: &'static [CliScenario],
    /// Self-update source, when this CLI opts into it. `None` keeps
    /// [`crate::update::check_and_apply`] fully inert (no network calls).
    pub update: Option<UpdateConfig>,
}

/// Where to find newer released versions of this CLI, and how to recognize
/// the current one.
#[derive(Clone, Copy, Debug)]
pub struct UpdateConfig {
    /// `owner/repo` on GitHub whose Releases publish this CLI's binaries.
    pub repository: &'static str,
    /// Release asset filename prefix, e.g. `"tokyo"` for
    /// `tokyo-v0.1.2-x86_64-unknown-linux-gnu.tar.gz`.
    pub asset_prefix: &'static str,
    /// This build's version, e.g. `env!("CARGO_PKG_VERSION")`.
    pub current_version: &'static str,
}

#[derive(Clone, Copy, Debug)]
/// Scenario program embedded in generated CLI configuration.
pub struct CliScenario {
    /// Scenario command name.
    pub name: &'static str,
    /// Human-facing scenario description.
    pub description: &'static str,
    /// Scenario program body.
    pub body: &'static str,
    /// Named environments where the scenario may run.
    pub allowed_environments: &'static [&'static str],
}

static CONFIG: OnceLock<RuntimeConfig> = OnceLock::new();

const DEFAULT_CONFIG: RuntimeConfig = RuntimeConfig {
    identity: ProductIdentity {
        package_name: "tokyo-generated-cli",
        command_name: "generated-cli",
        env_prefix: "GENERATED_CLI",
    },
    default_base_url: None,
    environments: &[],
    oauth_providers: &[],
    scenarios: &[],
    update: None,
};

/// Install the generated CLI's runtime configuration.
///
/// Calling this more than once with the same process is a programming error:
/// one process represents exactly one generated CLI.
pub fn configure_generated_cli_runtime(config: RuntimeConfig) {
    CONFIG
        .set(config)
        .expect("Tokyo CLI runtime was configured more than once");
}

pub(crate) fn runtime_config() -> &'static RuntimeConfig {
    CONFIG.get().unwrap_or(&DEFAULT_CONFIG)
}

/// Returns all scenarios embedded into the generated CLI.
#[must_use]
pub fn configured_cli_scenarios() -> &'static [CliScenario] {
    runtime_config().scenarios
}
