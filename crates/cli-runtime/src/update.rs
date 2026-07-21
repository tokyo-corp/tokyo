//! Background self-update: checks GitHub Releases for a newer version and
//! swaps the running binary in place. Fully inert unless the generated CLI
//! opts in via [`crate::config::RuntimeConfig::update`] — no network calls,
//! no filesystem writes, when it's `None`.

use std::io::Read as _;
use std::time::Duration;

const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const MANIFEST_TIMEOUT: Duration = Duration::from_millis(1500);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

enum UpdateError {
    Network,
    Decode,
    AssetNotFound,
    ChecksumMismatch,
    UnsupportedPlatform,
    Io,
}

#[derive(serde::Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(serde::Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// Checks for a newer release and, if found, downloads, verifies, and swaps
/// the running binary in place. Best-effort and silent: any failure along
/// the way (network, decoding, checksum, permissions) is swallowed and never
/// affects the calling command's exit code. Throttled to once per 24 hours
/// via a timestamp file in [`crate::profile::cli_runtime_config_directory`].
pub fn check_and_apply() {
    let Some(update_config) = crate::config::runtime_config().update else {
        return;
    };
    if std::env::var_os("CI").is_some() {
        return;
    }
    let env_prefix = crate::config::runtime_config().identity.env_prefix;
    if std::env::var_os(format!("{env_prefix}_NO_UPDATE_NOTIFIER")).is_some() {
        return;
    }
    if !should_check_now() {
        return;
    }
    record_check_time();
    let _ = try_update(&update_config);
}

fn should_check_now() -> bool {
    let Ok(config_directory) = crate::profile::cli_runtime_config_directory() else {
        return false;
    };
    let timestamp_path = config_directory.join("last-update-check");
    match std::fs::metadata(&timestamp_path).and_then(|metadata| metadata.modified()) {
        Ok(modified_at) => modified_at
            .elapsed()
            .is_ok_and(|elapsed| elapsed >= CHECK_INTERVAL),
        Err(_) => true,
    }
}

fn record_check_time() {
    let Ok(config_directory) = crate::profile::cli_runtime_config_directory() else {
        return;
    };
    let _ = std::fs::create_dir_all(&config_directory);
    let _ = std::fs::write(config_directory.join("last-update-check"), b"");
}

fn try_update(update_config: &crate::config::UpdateConfig) -> Result<(), UpdateError> {
    let release = fetch_latest_release(update_config.repository)?;
    let latest_version = release.tag_name.trim_start_matches('v');
    if !is_newer(latest_version, update_config.current_version) {
        return Ok(());
    }
    let target_triple = target_triple().ok_or(UpdateError::UnsupportedPlatform)?;
    let archive_extension = if cfg!(windows) { "zip" } else { "tar.gz" };
    let asset_name = format!(
        "{}-{}-{target_triple}.{archive_extension}",
        update_config.asset_prefix, release.tag_name
    );
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .ok_or(UpdateError::AssetNotFound)?;
    let checksums_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == "SHA256SUMS")
        .ok_or(UpdateError::AssetNotFound)?;

    let archive_bytes = download(&asset.browser_download_url)?;
    let checksums_bytes = download(&checksums_asset.browser_download_url)?;
    let checksums_text = String::from_utf8(checksums_bytes).map_err(|_| UpdateError::Decode)?;
    verify_checksum(&archive_bytes, &asset_name, &checksums_text)?;

    let binary_bytes = extract_binary(&archive_bytes, update_config.asset_prefix)?;
    apply_binary(&binary_bytes)?;

    eprintln!(
        "{} updated to {latest_version} — this will take effect on next run",
        crate::config::runtime_config().identity.command_name,
    );
    Ok(())
}

fn fetch_latest_release(repository: &str) -> Result<GithubRelease, UpdateError> {
    let agent = ureq::AgentBuilder::new().timeout(MANIFEST_TIMEOUT).build();
    let response = agent
        .get(&format!(
            "https://api.github.com/repos/{repository}/releases/latest"
        ))
        .set("User-Agent", "tokyo-cli-self-update")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|_| UpdateError::Network)?;
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_DOWNLOAD_BYTES)
        .read_to_end(&mut body)
        .map_err(|_| UpdateError::Network)?;
    serde_json::from_slice(&body).map_err(|_| UpdateError::Decode)
}

fn download(url: &str) -> Result<Vec<u8>, UpdateError> {
    let agent = ureq::AgentBuilder::new().timeout(DOWNLOAD_TIMEOUT).build();
    let response = agent
        .get(url)
        .set("User-Agent", "tokyo-cli-self-update")
        .call()
        .map_err(|_| UpdateError::Network)?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_DOWNLOAD_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|_| UpdateError::Network)?;
    Ok(bytes)
}

fn verify_checksum(
    archive_bytes: &[u8],
    asset_name: &str,
    checksums_text: &str,
) -> Result<(), UpdateError> {
    let expected_hex = checksums_text
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let hash = fields.next()?;
            let name = fields.next()?.trim_start_matches('*');
            (name == asset_name).then(|| hash.to_string())
        })
        .ok_or(UpdateError::ChecksumMismatch)?;

    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(archive_bytes);
    let actual_hex = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    if actual_hex.eq_ignore_ascii_case(&expected_hex) {
        Ok(())
    } else {
        Err(UpdateError::ChecksumMismatch)
    }
}

fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

fn is_newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate)
        .zip(parse_version(current))
        .is_some_and(|(candidate, current)| candidate > current)
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split(['.', '-', '+']).take(3);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(unix)]
fn extract_binary(archive_bytes: &[u8], binary_name: &str) -> Result<Vec<u8>, UpdateError> {
    let decompressed = flate2::read::GzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(decompressed);
    let entries = archive.entries().map_err(|_| UpdateError::Decode)?;
    for entry in entries {
        let mut entry = entry.map_err(|_| UpdateError::Decode)?;
        let path = entry.path().map_err(|_| UpdateError::Decode)?;
        if path.file_name().and_then(|name| name.to_str()) == Some(binary_name) {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|_| UpdateError::Decode)?;
            return Ok(bytes);
        }
    }
    Err(UpdateError::AssetNotFound)
}

#[cfg(windows)]
fn extract_binary(archive_bytes: &[u8], binary_name: &str) -> Result<Vec<u8>, UpdateError> {
    let target_name = format!("{binary_name}.exe");
    let cursor = std::io::Cursor::new(archive_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|_| UpdateError::Decode)?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|_| UpdateError::Decode)?;
        let is_target = std::path::Path::new(file.name())
            .file_name()
            .and_then(|name| name.to_str())
            == Some(target_name.as_str());
        if is_target {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|_| UpdateError::Decode)?;
            return Ok(bytes);
        }
    }
    Err(UpdateError::AssetNotFound)
}

fn apply_binary(binary_bytes: &[u8]) -> Result<(), UpdateError> {
    let command_name = crate::config::runtime_config().identity.command_name;
    let temporary_binary_path =
        std::env::temp_dir().join(format!("{command_name}-update-{}", std::process::id()));
    std::fs::write(&temporary_binary_path, binary_bytes).map_err(|_| UpdateError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            &temporary_binary_path,
            std::fs::Permissions::from_mode(0o755),
        )
        .map_err(|_| UpdateError::Io)?;
    }
    let result = self_replace::self_replace(&temporary_binary_path).map_err(|_| UpdateError::Io);
    let _ = std::fs::remove_file(&temporary_binary_path);
    result
}
