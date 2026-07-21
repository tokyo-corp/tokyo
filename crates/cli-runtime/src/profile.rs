use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};

/// Legacy credential scheme name for generated CLIs that use a single token.
pub const TOKEN_SCHEME: &str = "token";

/// Injectable secret storage used by profile credentials and OAuth token
/// caching. Tests can provide an in-memory implementation and never touch the
/// host keychain.
pub trait CredentialStore: Send + Sync {
    /// Loads a credential secret for the given profile and scheme.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ClientError`] if the store cannot be read.
    fn get_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
    ) -> Result<Option<String>, crate::error::ClientError>;
    /// Saves a credential secret for the given profile and scheme.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ClientError`] if the store cannot be written.
    fn save_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
        value: &str,
    ) -> Result<(), crate::error::ClientError>;
    /// Deletes a credential secret for the given profile and scheme.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ClientError`] if the store cannot be updated.
    fn delete_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
    ) -> Result<bool, crate::error::ClientError>;
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct StoreFile {
    /// Legacy layout. These values remain the `token` scheme so existing
    /// generated-CLI profiles continue to work without migration.
    #[serde(default)]
    profiles: BTreeMap<String, String>,
    #[serde(default)]
    credentials: BTreeMap<String, BTreeMap<String, String>>,
}

/// The per-CLI config directory (also used by `crate::session` for its own
/// local, non-secret files: last-response capture, created-resource tracking,
/// the request transcript).
pub fn cli_runtime_config_directory() -> Result<PathBuf, crate::error::ClientError> {
    let base_config_directory = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| {
            crate::error::ClientError::Transport(
                "could not determine a config directory (set $HOME or $XDG_CONFIG_HOME)"
                    .to_string(),
            )
        })?;
    Ok(base_config_directory.join(crate::config::runtime_config().identity.package_name))
}

fn credential_store_file_path() -> Result<PathBuf, crate::error::ClientError> {
    Ok(cli_runtime_config_directory()?.join("credentials.json"))
}

fn load_store_file() -> Result<StoreFile, crate::error::ClientError> {
    let credential_store_path = credential_store_file_path()?;
    match std::fs::read_to_string(&credential_store_path) {
        Ok(credential_store_json) => serde_json::from_str(&credential_store_json)
            .map_err(|error| crate::error::ClientError::Decode(error.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(StoreFile::default()),
        Err(error) => Err(crate::error::ClientError::Transport(error.to_string())),
    }
}

fn save_store_file(credential_store_file: &StoreFile) -> Result<(), crate::error::ClientError> {
    let credential_store_path = credential_store_file_path()?;
    if let Some(credential_store_parent_directory) = credential_store_path.parent() {
        std::fs::create_dir_all(credential_store_parent_directory)
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                credential_store_parent_directory,
                std::fs::Permissions::from_mode(0o700),
            )
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        }
    }
    let credential_store_json = serde_json::to_string_pretty(credential_store_file)
        .expect("a string-keyed map always serializes");
    let temporary_credential_store_path =
        credential_store_path.with_extension(format!("json.tmp-{}", std::process::id()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut temporary_credential_store_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary_credential_store_path)
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        temporary_credential_store_file
            .write_all(credential_store_json.as_bytes())
            .and_then(|_| temporary_credential_store_file.sync_all())
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
    }
    #[cfg(not(unix))]
    {
        let mut temporary_credential_store_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary_credential_store_path)
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        temporary_credential_store_file
            .write_all(credential_store_json.as_bytes())
            .and_then(|_| temporary_credential_store_file.sync_all())
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
    }
    std::fs::rename(&temporary_credential_store_path, &credential_store_path)
        .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
    Ok(())
}

#[derive(Default)]
struct JsonCredentialStore;

impl CredentialStore for JsonCredentialStore {
    fn get_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
    ) -> Result<Option<String>, crate::error::ClientError> {
        let credential_store_file = load_store_file()?;
        Ok(credential_store_file
            .credentials
            .get(profile)
            .and_then(|schemes| schemes.get(scheme))
            .cloned()
            .or_else(|| {
                (scheme == TOKEN_SCHEME)
                    .then(|| credential_store_file.profiles.get(profile).cloned())
                    .flatten()
            }))
    }

    fn save_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
        value: &str,
    ) -> Result<(), crate::error::ClientError> {
        let mut credential_store_file = load_store_file()?;
        if scheme == TOKEN_SCHEME {
            credential_store_file
                .profiles
                .insert(profile.to_string(), value.to_string());
        } else {
            credential_store_file
                .credentials
                .entry(profile.to_string())
                .or_default()
                .insert(scheme.to_string(), value.to_string());
        }
        save_store_file(&credential_store_file)
    }

    fn delete_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
    ) -> Result<bool, crate::error::ClientError> {
        let mut credential_store_file = load_store_file()?;
        let mut credential_was_removed = if scheme == TOKEN_SCHEME {
            credential_store_file.profiles.remove(profile).is_some()
        } else {
            credential_store_file
                .credentials
                .get_mut(profile)
                .is_some_and(|schemes| schemes.remove(scheme).is_some())
        };
        if let Some(profile_credentials_by_scheme) = credential_store_file.credentials.get(profile)
            && profile_credentials_by_scheme.is_empty()
        {
            credential_store_file.credentials.remove(profile);
        }
        save_store_file(&credential_store_file)?;
        Ok(std::mem::take(&mut credential_was_removed))
    }
}

enum KeychainError {
    Missing,
    Unavailable(String),
    Denied(String),
    Other(String),
}

fn classify_keychain(error: keyring::Error) -> KeychainError {
    match error {
        keyring::Error::NoEntry => KeychainError::Missing,
        keyring::Error::PlatformFailure(error) => KeychainError::Unavailable(error.to_string()),
        keyring::Error::NoStorageAccess(error) => KeychainError::Denied(error.to_string()),
        other => KeychainError::Other(other.to_string()),
    }
}

fn keychain_entry_for_profile_and_scheme(
    profile: &str,
    scheme: &str,
) -> Result<keyring::Entry, KeychainError> {
    let identity = crate::config::runtime_config().identity;
    let service = format!(
        "tokyo.generated.package={}.product={}",
        identity.package_name, identity.command_name
    );
    let user = format!("profile={profile};scheme={scheme}");
    keyring::Entry::new(&service, &user).map_err(classify_keychain)
}

struct KeychainFirstStore {
    fallback: JsonCredentialStore,
    fallback_active: AtomicBool,
    warning: Once,
}

impl Default for KeychainFirstStore {
    fn default() -> Self {
        Self {
            fallback: JsonCredentialStore,
            fallback_active: AtomicBool::new(false),
            warning: Once::new(),
        }
    }
}

impl KeychainFirstStore {
    fn fallback<T>(
        &self,
        operation: impl FnOnce(&JsonCredentialStore) -> Result<T, crate::error::ClientError>,
    ) -> Result<T, crate::error::ClientError> {
        self.fallback_active.store(true, Ordering::Release);
        self.warning.call_once(|| {
            eprintln!(
                "warning: native credential storage is unavailable; using owner-only JSON at {}",
                credential_store_file_path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| "<unresolved config path>".to_string())
            );
        });
        operation(&self.fallback)
    }

    fn error(error: KeychainError) -> crate::error::ClientError {
        match error {
            KeychainError::Denied(message) => crate::error::ClientError::CredentialStore(format!(
                "native credential store denied access; unlock or authorize the OS keychain: {message}"
            )),
            KeychainError::Other(message) | KeychainError::Unavailable(message) => {
                crate::error::ClientError::Transport(format!(
                    "native credential store error: {message}"
                ))
            }
            KeychainError::Missing => {
                unreachable!("missing is handled by get_credential_secret/delete_credential_secret")
            }
        }
    }
}

impl CredentialStore for KeychainFirstStore {
    fn get_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
    ) -> Result<Option<String>, crate::error::ClientError> {
        if self.fallback_active.load(Ordering::Acquire) {
            return self.fallback.get_credential_secret(profile, scheme);
        }
        let entry = match keychain_entry_for_profile_and_scheme(profile, scheme) {
            Ok(entry) => entry,
            Err(KeychainError::Unavailable(_)) => {
                return self.fallback(|store| store.get_credential_secret(profile, scheme));
            }
            Err(error) => return Err(Self::error(error)),
        };
        match entry.get_password().map_err(classify_keychain) {
            Ok(value) => Ok(Some(value)),
            Err(KeychainError::Missing) => {
                let Some(value) = self.fallback.get_credential_secret(profile, scheme)? else {
                    return Ok(None);
                };
                match entry.set_password(&value).map_err(classify_keychain) {
                    Ok(()) => {
                        // Migration succeeded: drop the plaintext fallback copy
                        // so the secret lives only in the keychain, not in both
                        // stores. Best-effort — a failed cleanup just leaves the
                        // prior (already-present) fallback entry in place.
                        let _ = self.fallback.delete_credential_secret(profile, scheme);
                        Ok(Some(value))
                    }
                    Err(KeychainError::Unavailable(_)) => {
                        self.fallback(|store| store.get_credential_secret(profile, scheme))
                    }
                    Err(error) => Err(Self::error(error)),
                }
            }
            Err(KeychainError::Unavailable(_)) => {
                self.fallback(|store| store.get_credential_secret(profile, scheme))
            }
            Err(error) => Err(Self::error(error)),
        }
    }

    fn save_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
        value: &str,
    ) -> Result<(), crate::error::ClientError> {
        if self.fallback_active.load(Ordering::Acquire) {
            return self.fallback.save_credential_secret(profile, scheme, value);
        }
        let entry = match keychain_entry_for_profile_and_scheme(profile, scheme) {
            Ok(entry) => entry,
            Err(KeychainError::Unavailable(_)) => {
                return self.fallback(|store| store.save_credential_secret(profile, scheme, value));
            }
            Err(error) => return Err(Self::error(error)),
        };
        match entry.set_password(value).map_err(classify_keychain) {
            Ok(()) => Ok(()),
            Err(KeychainError::Unavailable(_)) => {
                self.fallback(|store| store.save_credential_secret(profile, scheme, value))
            }
            Err(error) => Err(Self::error(error)),
        }
    }

    fn delete_credential_secret(
        &self,
        profile: &str,
        scheme: &str,
    ) -> Result<bool, crate::error::ClientError> {
        if self.fallback_active.load(Ordering::Acquire) {
            return self.fallback.delete_credential_secret(profile, scheme);
        }
        let entry = match keychain_entry_for_profile_and_scheme(profile, scheme) {
            Ok(entry) => entry,
            Err(KeychainError::Unavailable(_)) => {
                return self.fallback(|store| store.delete_credential_secret(profile, scheme));
            }
            Err(error) => return Err(Self::error(error)),
        };
        match entry.delete_credential().map_err(classify_keychain) {
            Ok(()) => Ok(true),
            Err(KeychainError::Missing) => Ok(false),
            Err(KeychainError::Unavailable(_)) => {
                self.fallback(|store| store.delete_credential_secret(profile, scheme))
            }
            Err(error) => Err(Self::error(error)),
        }
    }
}

/// Returns the default credential store for this platform.
#[must_use]
pub fn default_credential_store() -> Arc<dyn CredentialStore> {
    Arc::new(KeychainFirstStore::default())
}

/// Loads the legacy single-token credential for a profile.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the credential store cannot be read.
#[must_use = "loaded credentials or credential errors should be handled"]
pub fn load_legacy_token_credential(
    store: &dyn CredentialStore,
    profile: &str,
) -> Result<Option<String>, crate::error::ClientError> {
    store.get_credential_secret(profile, TOKEN_SCHEME)
}

/// Saves the legacy single-token credential for a profile.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the credential store cannot be written.
pub fn save_legacy_token_credential(
    store: &dyn CredentialStore,
    profile: &str,
    token: &str,
) -> Result<(), crate::error::ClientError> {
    store.save_credential_secret(profile, TOKEN_SCHEME, token)
}

/// Non-secret connection settings associated with the same profile name used
/// for credentials. Secrets remain in the OS keychain/credential store.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ConnectionProfile {
    /// Explicit base URL for this profile.
    pub base_url: Option<String>,
    /// Named environment selected for this profile.
    pub environment: Option<String>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct ConnectionProfilesFile {
    profiles: BTreeMap<String, ConnectionProfile>,
}

fn connection_profiles_path() -> Result<PathBuf, crate::error::ClientError> {
    Ok(cli_runtime_config_directory()?.join("profiles.json"))
}

fn load_connection_profiles() -> Result<ConnectionProfilesFile, crate::error::ClientError> {
    let connection_profiles_path = connection_profiles_path()?;
    match std::fs::read_to_string(&connection_profiles_path) {
        Ok(connection_profiles_json) => serde_json::from_str(&connection_profiles_json)
            .map_err(|error| crate::error::ClientError::Decode(error.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ConnectionProfilesFile::default())
        }
        Err(error) => Err(crate::error::ClientError::Transport(error.to_string())),
    }
}

fn save_connection_profiles(
    connection_profiles_file: &ConnectionProfilesFile,
) -> Result<(), crate::error::ClientError> {
    let connection_profiles_path = connection_profiles_path()?;
    if let Some(connection_profiles_parent_directory) = connection_profiles_path.parent() {
        std::fs::create_dir_all(connection_profiles_parent_directory)
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                connection_profiles_parent_directory,
                std::fs::Permissions::from_mode(0o700),
            )
            .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
        }
    }
    let connection_profiles_json = serde_json::to_string_pretty(connection_profiles_file)
        .expect("connection profiles always serialize");
    let temporary_connection_profiles_path =
        connection_profiles_path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(
        &temporary_connection_profiles_path,
        connection_profiles_json,
    )
    .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
    std::fs::rename(
        &temporary_connection_profiles_path,
        &connection_profiles_path,
    )
    .map_err(|error| crate::error::ClientError::Transport(error.to_string()))?;
    Ok(())
}

/// Loads non-secret connection settings for one profile.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the profile file cannot be read.
#[must_use = "profile settings or profile errors should be handled"]
pub fn connection_profile(
    profile_name: &str,
) -> Result<ConnectionProfile, crate::error::ClientError> {
    Ok(load_connection_profiles()?
        .profiles
        .get(profile_name)
        .cloned()
        .unwrap_or_default())
}

/// Lists all saved connection profiles, including the implicit default profile.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if the profile file cannot be read.
#[must_use = "profile settings or profile errors should be handled"]
pub fn list_connection_profiles()
-> Result<Vec<(String, ConnectionProfile)>, crate::error::ClientError> {
    let mut connection_profiles_by_name = load_connection_profiles()?.profiles;
    connection_profiles_by_name
        .entry("default".to_string())
        .or_default();
    Ok(connection_profiles_by_name.into_iter().collect())
}

/// Writes non-secret connection settings for one profile.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] if inputs are invalid or the profile
/// file cannot be written.
#[must_use = "updated profile settings or profile errors should be handled"]
pub fn set_connection_profile(
    profile: &str,
    base_url: Option<String>,
    environment: Option<String>,
) -> Result<ConnectionProfile, crate::error::ClientError> {
    if profile.trim().is_empty() {
        return Err(crate::error::ClientError::Decode(
            "profile name must not be empty".to_string(),
        ));
    }
    match (&base_url, &environment) {
        (Some(_), Some(_)) => {
            return Err(crate::error::ClientError::Decode(
                "--base-url and --environment cannot be set together".to_string(),
            ));
        }
        (None, None) => {
            return Err(crate::error::ClientError::Decode(
                "profile set requires --base-url or --environment".to_string(),
            ));
        }
        _ => {}
    }
    if let Some(url) = &base_url {
        validate_base_url(url)?;
    }
    if let Some(name) = &environment {
        environment_url(name)?;
    }

    let connection = ConnectionProfile {
        base_url,
        environment,
    };
    let mut profiles = load_connection_profiles()?;
    profiles
        .profiles
        .insert(profile.to_string(), connection.clone());
    save_connection_profiles(&profiles)?;
    Ok(connection)
}

fn validate_base_url(value: &str) -> Result<(), crate::error::ClientError> {
    let url = url::Url::parse(value).map_err(|error| {
        crate::error::ClientError::Decode(format!("invalid base URL {value:?}: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") || url.cannot_be_a_base() {
        return Err(crate::error::ClientError::Decode(
            "base URL must be an absolute http(s) URL".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(crate::error::ClientError::Decode(
            "base URL must not contain credentials".to_string(),
        ));
    }
    Ok(())
}

/// Returns configured named environments.
#[must_use]
pub fn environment_catalog() -> &'static [(&'static str, &'static str)] {
    crate::config::runtime_config().environments
}

/// Resolves a configured environment name to its base URL.
///
/// # Errors
///
/// Returns [`crate::error::ClientError`] when the environment is unknown.
#[must_use = "environment URL lookup errors should be handled"]
pub fn environment_url(name: &str) -> Result<String, crate::error::ClientError> {
    crate::config::runtime_config()
        .environments
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, url)| (*url).to_string())
        .ok_or_else(|| {
            let available = crate::config::runtime_config()
                .environments
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", ");
            let hint = if available.is_empty() {
                "this CLI has no named environments".to_string()
            } else {
                format!("available environments: {available}")
            };
            crate::error::ClientError::Decode(format!("unknown environment {name:?}; {hint}"))
        })
}

/// Resolve the named environment in effect for `profile`, without falling
/// back to a raw base URL: an explicit `--environment` wins, then the
/// profile's stored environment, otherwise `None`. Used to gate mock auth,
/// which must know it's targeting a specific named environment rather than
/// an arbitrary `--base-url`.
pub fn resolve_environment_name(
    profile: &str,
    explicit_environment: Option<&str>,
) -> Result<Option<String>, crate::error::ClientError> {
    if let Some(name) = explicit_environment {
        return Ok(Some(name.to_string()));
    }
    Ok(connection_profile(profile)?.environment)
}

/// The generated environment whose URL matches `url` exactly, if any.
pub fn environment_name_for_url(url: &str) -> Option<&'static str> {
    environment_catalog()
        .iter()
        .find(|(_, catalog_url)| *catalog_url == url)
        .map(|(name, _)| *name)
}

/// The named environment in effect for `profile`: the explicit/stored name,
/// falling back to reverse-matching the resolved base URL against the
/// generated environment catalog.
pub fn active_environment_name(
    profile: &str,
    explicit_environment: Option<&str>,
) -> Result<Option<String>, crate::error::ClientError> {
    if let Some(name) = resolve_environment_name(profile, explicit_environment)? {
        return Ok(Some(name));
    }
    Ok(resolve_base_url(profile, None, explicit_environment)
        .ok()
        .and_then(|resolved| environment_name_for_url(&resolved).map(str::to_string)))
}

/// Resolve connection settings in explicit-to-default order:
/// --base-url/env, --environment/env, profile URL, profile environment,
/// generated default URL.
pub fn resolve_base_url(
    profile: &str,
    explicit_base_url: Option<&str>,
    explicit_environment: Option<&str>,
) -> Result<String, crate::error::ClientError> {
    if let Some(url) = explicit_base_url {
        validate_base_url(url)?;
        return Ok(url.to_string());
    }
    if let Some(environment) = explicit_environment {
        return environment_url(environment);
    }

    let connection = connection_profile(profile)?;
    if let Some(url) = connection.base_url {
        validate_base_url(&url)?;
        return Ok(url);
    }
    if let Some(environment) = connection.environment {
        return environment_url(&environment);
    }
    if let Some(url) = crate::config::runtime_config().default_base_url {
        return Ok(url.to_string());
    }

    Err(crate::error::ClientError::MissingConfig(
        "--base-url, --environment, a configured profile, or a generated default URL",
    ))
}
