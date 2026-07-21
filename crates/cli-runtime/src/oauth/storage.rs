//! Managed OAuth token records, provider binding, and refresh serialization.

use std::time::Duration;

use super::oauth_provider_for_scheme;
use crate::error::ClientError;
use crate::profile::CredentialStore;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct StoredOAuthToken {
    pub(super) access_token: String,
    pub(super) refresh_token: Option<String>,
    pub(super) expires_at: Option<u64>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct BoundStoredOAuthToken {
    binding: String,
    token: StoredOAuthToken,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum StoredOAuthTokenOnDisk {
    Bound(BoundStoredOAuthToken),
    Legacy(StoredOAuthToken),
}

pub(super) fn save_oauth_token_record(
    store: &dyn CredentialStore,
    profile: &str,
    scheme: &str,
    token: StoredOAuthToken,
) -> Result<(), ClientError> {
    let metadata = serde_json::to_string(&BoundStoredOAuthToken {
        binding: oauth_provider_binding(scheme),
        token: token.clone(),
    })
    .expect("OAuth token metadata always serializes");
    // Commit complete rotation state first. Readers prefer this record, so a
    // crash cannot pair a new access token with stale refresh metadata.
    store.save_credential_secret(profile, &oauth_metadata_cache_key(scheme), &metadata)?;
    store.save_credential_secret(profile, scheme, &token.access_token)
}

pub(super) fn decode_bound_oauth_token_record(
    scheme: &str,
    raw: &str,
) -> Result<StoredOAuthToken, ClientError> {
    let record: StoredOAuthTokenOnDisk =
        serde_json::from_str(raw).map_err(|error| ClientError::Decode(error.to_string()))?;
    match record {
        StoredOAuthTokenOnDisk::Legacy(token) => Ok(token),
        StoredOAuthTokenOnDisk::Bound(bound) => {
            let expected = oauth_provider_binding(scheme);
            if bound.binding != expected {
                return Err(ClientError::MissingCredential(format!(
                    "stored credential {scheme:?} was issued for different provider settings; rerun `auth login --scheme {scheme}`"
                )));
            }
            Ok(bound.token)
        }
    }
}

fn oauth_provider_binding(scheme: &str) -> String {
    use sha2::Digest as _;
    let identity = crate::config::runtime_config().identity;
    let provider = oauth_provider_for_scheme(scheme);
    let material = provider.map_or_else(
        || {
            format!(
                "{}|{}|{scheme}|manual",
                identity.package_name, identity.command_name
            )
        },
        |provider| {
            format!(
                "{}|{}|{}|{}|{:?}|{:?}|{:?}",
                identity.package_name,
                identity.command_name,
                scheme,
                provider.client_id,
                provider.endpoints,
                provider.scopes,
                provider.audience,
            )
        },
    );
    format!("{:x}", sha2::Sha256::digest(material.as_bytes()))
}

pub(super) fn oauth_metadata_cache_key(scheme: &str) -> String {
    format!("__tokyo_oauth__{scheme}")
}

pub(super) struct OAuthRefreshLock {
    path: Option<std::path::PathBuf>,
}

impl OAuthRefreshLock {
    pub(super) fn acquire(profile: &str, scheme: &str) -> Result<Self, ClientError> {
        if cfg!(test) {
            return Ok(Self { path: None });
        }
        use sha2::Digest as _;
        let digest = format!(
            "{:x}",
            sha2::Sha256::digest(format!("{profile}\0{scheme}").as_bytes())
        );
        let directory = crate::profile::cli_runtime_config_directory()?.join("locks");
        std::fs::create_dir_all(&directory)
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| ClientError::Transport(error.to_string()))?;
        }
        let path = directory.join(format!("oauth-refresh-{}.lock", &digest[..24]));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write as _;
                    writeln!(file, "{}", std::process::id()).ok();
                    return Ok(Self { path: Some(path) });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(120));
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(ClientError::CredentialStore(format!(
                            "timed out waiting for OAuth refresh lock for profile {profile:?}, scheme {scheme:?}"
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    return Err(ClientError::Transport(format!(
                        "could not create OAuth refresh lock: {error}"
                    )));
                }
            }
        }
    }
}

impl Drop for OAuthRefreshLock {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}
