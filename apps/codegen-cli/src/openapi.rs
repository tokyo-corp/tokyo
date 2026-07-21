use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

const SNAPSHOT: &str = "openapi/upstream.json";
const LOCK: &str = "tokyo.lock";
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const FORMAT_VERSION: u32 = 1;
static NEXT_TRANSACTION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) enum Error {
    Input(String),
    Output(String),
    Differences(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(message) | Self::Output(message) | Self::Differences(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Section {
    source: Option<String>,
    snapshot: Option<String>,
    headers: BTreeMap<String, String>,
    // Accepted so `add` can migrate the legacy direct-input project shape
    // without disturbing its output setting.
    #[serde(rename = "input")]
    _input: Option<String>,
    #[serde(rename = "output")]
    _output: Option<String>,
}

#[derive(Debug, Serialize)]
struct Lock<'a> {
    format_version: u32,
    source: &'a str,
    snapshot: &'a str,
    sha256: String,
    generator_version: &'a str,
}

pub(crate) fn add(config_path: &Path, source: &str) -> Result<bool> {
    if source.trim().is_empty() {
        return Err(Error::Input("OpenAPI source must not be empty".into()));
    }
    let config_text = fs::read_to_string(config_path).map_err(|error| {
        Error::Input(format!(
            "cannot read config {}: {error}; initialize a Tokyo project first",
            config_path.display()
        ))
    })?;
    let mut config: toml::Value = toml::from_str(&config_text).map_err(|error| {
        Error::Input(format!("invalid config {}: {error}", config_path.display()))
    })?;
    let root = config.as_table_mut().ok_or_else(|| {
        Error::Input(format!(
            "invalid config {}: expected a TOML table",
            config_path.display()
        ))
    })?;
    let existing_headers = root
        .get("openapi")
        .cloned()
        .map(toml::Value::try_into::<Section>)
        .transpose()
        .map_err(|error| {
            Error::Input(format!(
                "invalid [openapi] config in {}: {error}",
                config_path.display()
            ))
        })?
        .map_or_else(BTreeMap::new, |section| section.headers);
    let section = root
        .entry("openapi")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| Error::Input("[openapi] must be a TOML table".into()))?;
    section.insert("source".into(), toml::Value::String(source.to_string()));
    section.insert("snapshot".into(), toml::Value::String(SNAPSHOT.into()));
    section.remove("input");
    if !existing_headers.is_empty() && !section.contains_key("headers") {
        section.insert(
            "headers".into(),
            toml::Value::try_from(existing_headers)
                .map_err(|error| Error::Output(format!("cannot serialize headers: {error}")))?,
        );
    }

    let headers = section_headers(section)?;
    let acquired = acquire(source, config_directory(config_path), &headers)?;
    let canonical = canonicalize_and_validate(&acquired)?;
    let config_bytes = toml::to_string_pretty(&config)
        .map_err(|error| Error::Output(format!("cannot serialize config: {error}")))?
        .into_bytes();
    let lock = render_lock(source, SNAPSHOT, &canonical)?;
    let root = config_directory(config_path);
    let config_relative = config_path
        .file_name()
        .map(PathBuf::from)
        .ok_or_else(|| Error::Output("config path must name a file".into()))?;
    let files = BTreeMap::from([
        (config_relative, config_bytes),
        (PathBuf::from(SNAPSHOT), canonical),
        (PathBuf::from(LOCK), lock),
    ]);
    let changed = files_differ(root, &files)?;
    if changed {
        write_transaction(root, &files)?;
    }
    Ok(changed)
}

pub(crate) fn sync(config_path: &Path) -> Result<bool> {
    let (section, root) = read_section(config_path)?;
    let source = required(&section.source, "[openapi].source")?;
    let snapshot = required(&section.snapshot, "[openapi].snapshot")?;
    validate_relative(snapshot)?;
    let acquired = acquire(source, root, &section.headers)?;
    let canonical = canonicalize_and_validate(&acquired)?;
    let lock = render_lock(source, snapshot, &canonical)?;
    let files = BTreeMap::from([
        (PathBuf::from(snapshot), canonical),
        (PathBuf::from(LOCK), lock),
    ]);
    let changed = files_differ(root, &files)?;
    if changed {
        write_transaction(root, &files)?;
    }
    Ok(changed)
}

pub(crate) fn check(config_path: &Path) -> Result<()> {
    let (section, root) = read_section(config_path)?;
    let source = required(&section.source, "[openapi].source")?;
    let snapshot = required(&section.snapshot, "[openapi].snapshot")?;
    validate_relative(snapshot)?;
    let acquired = acquire(source, root, &section.headers)?;
    let canonical = canonicalize_and_validate(&acquired)?;
    let expected_lock = render_lock(source, snapshot, &canonical)?;
    let snapshot_path = root.join(snapshot);
    let current = fs::read(&snapshot_path).map_err(|error| {
        Error::Input(format!(
            "cannot read vendored OpenAPI snapshot {}: {error}; run `tokyo openapi sync`",
            snapshot_path.display()
        ))
    })?;
    let lock_path = root.join(LOCK);
    let current_lock = fs::read(&lock_path).map_err(|error| {
        Error::Input(format!(
            "cannot read OpenAPI lock {}: {error}; run `tokyo openapi sync`",
            lock_path.display()
        ))
    })?;
    if current == canonical && current_lock == expected_lock {
        return Ok(());
    }
    Err(Error::Differences(
        "OpenAPI source, vendored snapshot, or lock differs; run `tokyo openapi sync`".to_string(),
    ))
}

fn config_directory(config_path: &Path) -> &Path {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn required<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str> {
    value
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Input(format!("missing {name}; run `tokyo openapi add URL|PATH`")))
}

fn read_section(config_path: &Path) -> Result<(Section, &Path)> {
    let text = fs::read_to_string(config_path).map_err(|error| {
        Error::Input(format!(
            "cannot read config {}: {error}",
            config_path.display()
        ))
    })?;
    let mut root: toml::Value = toml::from_str(&text).map_err(|error| {
        Error::Input(format!("invalid config {}: {error}", config_path.display()))
    })?;
    let section = root
        .as_table_mut()
        .and_then(|table| table.remove("openapi"))
        .ok_or_else(|| {
            Error::Input("OpenAPI is not configured; run `tokyo openapi add URL|PATH`".into())
        })?
        .try_into()
        .map_err(|error| {
            Error::Input(format!(
                "invalid [openapi] config in {}: {error}",
                config_path.display()
            ))
        })?;
    Ok((section, config_directory(config_path)))
}

fn section_headers(
    section: &toml::map::Map<String, toml::Value>,
) -> Result<BTreeMap<String, String>> {
    // `add` currently preserves existing header mappings. Deserialize once
    // more so acquisition and config validation use exactly the same rules.
    let value = toml::Value::Table(section.clone());
    let parsed: Section = value
        .try_into()
        .map_err(|error| Error::Input(format!("invalid [openapi] config: {error}")))?;
    Ok(parsed.headers)
}

fn acquire(source: &str, root: &Path, headers: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    if source.starts_with("http://") || source.starts_with("https://") {
        acquire_http(source, headers)
    } else {
        let path = Path::new(source);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let metadata = fs::metadata(&path).map_err(|error| {
            Error::Input(format!(
                "cannot read OpenAPI source {}: {error}",
                path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(Error::Input(format!(
                "OpenAPI source {} is not a file",
                path.display()
            )));
        }
        if metadata.len() > MAX_RESPONSE_BYTES {
            return Err(Error::Input(format!(
                "OpenAPI source {} exceeds {} byte limit",
                path.display(),
                MAX_RESPONSE_BYTES
            )));
        }
        fs::read(&path).map_err(|error| {
            Error::Input(format!(
                "cannot read OpenAPI source {}: {error}",
                path.display()
            ))
        })
    }
}

fn acquire_http(source: &str, headers: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| Error::Input(format!("cannot configure OpenAPI HTTP client: {error}")))?;
    let mut request = client.get(source);
    for (name, env_name) in headers {
        if name.trim().is_empty() || env_name.trim().is_empty() {
            return Err(Error::Input(
                "[openapi].headers names and environment variable names must not be empty".into(),
            ));
        }
        let value = std::env::var(env_name).map_err(|_| {
            Error::Input(format!(
                "missing environment variable {env_name:?} for OpenAPI request header {name:?}"
            ))
        })?;
        request = request.header(name, value);
    }
    let response = request
        .send()
        .map_err(|error| Error::Input(format!("cannot fetch OpenAPI source {source}: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        let mut detail = String::new();
        let _ = response.take(1025).read_to_string(&mut detail);
        return Err(Error::Input(format!(
            "OpenAPI source {source} returned HTTP {status}{}",
            if detail.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", detail.trim())
            }
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return Err(Error::Input(format!(
            "OpenAPI response from {source} exceeds {MAX_RESPONSE_BYTES} byte limit"
        )));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.is_empty()
        && !content_type.contains("json")
        && !content_type.contains("yaml")
        && !content_type.starts_with("text/")
        && !content_type.contains("octet-stream")
    {
        return Err(Error::Input(format!(
            "OpenAPI source {source} returned unsupported content type {content_type:?}"
        )));
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            Error::Input(format!(
                "cannot read OpenAPI response from {source}: {error}"
            ))
        })?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(Error::Input(format!(
            "OpenAPI response from {source} exceeds {MAX_RESPONSE_BYTES} byte limit"
        )));
    }
    Ok(bytes)
}

fn canonicalize_and_validate(bytes: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| Error::Input(format!("OpenAPI source is not UTF-8: {error}")))?;
    tokyo_codegen_engine::import_openapi_text(
        text,
        tokyo_codegen_engine::InputFormat::Auto,
        &tokyo_codegen_engine::Config::default(),
    )
    .map_err(|error| Error::Input(format!("cannot import OpenAPI source: {error}")))?;
    let value: serde_json::Value = if text.trim_start().starts_with(['{', '[']) {
        serde_json::from_str(text)
            .or_else(|_| yaml_serde::from_str(text))
            .map_err(|error| Error::Input(format!("cannot parse OpenAPI source: {error}")))?
    } else {
        yaml_serde::from_str(text)
            .or_else(|_| serde_json::from_str(text))
            .map_err(|error| Error::Input(format!("cannot parse OpenAPI source: {error}")))?
    };
    let mut output = serde_json::to_vec_pretty(&value)
        .map_err(|error| Error::Output(format!("cannot canonicalize OpenAPI source: {error}")))?;
    output.push(b'\n');
    Ok(output)
}

fn render_lock(source: &str, snapshot: &str, canonical: &[u8]) -> Result<Vec<u8>> {
    let lock = Lock {
        format_version: FORMAT_VERSION,
        source,
        snapshot,
        sha256: format!("{:x}", sha2::Sha256::digest(canonical)),
        generator_version: env!("CARGO_PKG_VERSION"),
    };
    toml::to_string_pretty(&lock)
        .map(String::into_bytes)
        .map_err(|error| Error::Output(format!("cannot serialize {LOCK}: {error}")))
}

fn validate_relative(path: &str) -> Result<()> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::Input(format!(
            "invalid [openapi].snapshot {path:?}; expected a safe project-relative path"
        )));
    }
    Ok(())
}

fn files_differ(root: &Path, files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<bool> {
    for (relative, desired) in files {
        validate_relative(relative.to_string_lossy().as_ref())?;
        match fs::read(root.join(relative)) {
            Ok(current) if current == *desired => {}
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => {
                return Err(Error::Output(format!(
                    "cannot inspect {}: {error}",
                    root.join(relative).display()
                )));
            }
        }
    }
    Ok(false)
}

fn write_transaction(root: &Path, files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
    // Preflight every target before creating temporary files.
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| Error::Output(format!("cannot resolve {}: {error}", root.display())))?;
    for relative in files.keys() {
        validate_relative(relative.to_string_lossy().as_ref())?;
        let target = root.join(relative);
        if let Ok(metadata) = fs::symlink_metadata(&target)
            && (metadata.file_type().is_symlink() || !metadata.is_file())
        {
            return Err(Error::Output(format!(
                "refusing to replace unsafe path {}",
                target.display()
            )));
        }
        let mut ancestor = target.parent();
        while let Some(path) = ancestor {
            if path == root {
                break;
            }
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(Error::Output(format!(
                        "refusing unsafe parent path {}",
                        path.display()
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(Error::Output(format!(
                        "cannot inspect {}: {error}",
                        path.display()
                    )));
                }
            }
            ancestor = path.parent();
        }
    }
    for relative in files.keys() {
        if let Some(parent) = root.join(relative).parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::Output(format!("cannot create {}: {error}", parent.display()))
            })?;
            let canonical_parent = fs::canonicalize(parent).map_err(|error| {
                Error::Output(format!("cannot resolve {}: {error}", parent.display()))
            })?;
            if !canonical_parent.starts_with(&canonical_root) {
                return Err(Error::Output(format!(
                    "refusing path outside project: {}",
                    parent.display()
                )));
            }
        }
    }

    let id = NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed);
    let transaction = root.join(format!(".tokyo-openapi-{}-{id}", std::process::id()));
    fs::create_dir(&transaction).map_err(|error| {
        Error::Output(format!(
            "cannot create OpenAPI transaction {}: {error}",
            transaction.display()
        ))
    })?;
    let result = (|| {
        let mut staged = Vec::new();
        for (index, (relative, contents)) in files.iter().enumerate() {
            let temp = transaction.join(format!("staged-{index}"));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|error| {
                    Error::Output(format!("cannot stage {}: {error}", relative.display()))
                })?;
            file.write_all(contents)
                .and_then(|()| file.sync_all())
                .map_err(|error| {
                    Error::Output(format!("cannot stage {}: {error}", relative.display()))
                })?;
            staged.push((temp, root.join(relative)));
        }
        let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (index, (_, target)) in staged.iter().enumerate() {
            if target.exists() {
                let backup = transaction.join(format!("backup-{index}"));
                if let Err(error) = fs::rename(target, &backup) {
                    let mut rollback_errors = Vec::new();
                    for (backup, target) in backups.iter().rev() {
                        if let Err(rollback_error) = fs::rename(backup, target) {
                            rollback_errors.push(format!(
                                "cannot restore {}: {rollback_error}",
                                target.display()
                            ));
                        }
                    }
                    return Err(Error::Output(format!(
                        "cannot back up {}: {error}{}",
                        target.display(),
                        if rollback_errors.is_empty() {
                            String::new()
                        } else {
                            format!("; rollback failed: {}", rollback_errors.join("; "))
                        }
                    )));
                }
                backups.push((backup, target.clone()));
            }
        }
        let mut installed: Vec<PathBuf> = Vec::new();
        for (temp, target) in &staged {
            if let Err(error) = fs::rename(temp, target) {
                let mut rollback_errors = Vec::new();
                for target in installed.iter().rev() {
                    if let Err(rollback_error) = fs::remove_file(target) {
                        rollback_errors.push(format!(
                            "cannot remove {}: {rollback_error}",
                            target.display()
                        ));
                    }
                }
                for (backup, target) in backups.iter().rev() {
                    if let Err(rollback_error) = fs::rename(backup, target) {
                        rollback_errors.push(format!(
                            "cannot restore {}: {rollback_error}",
                            target.display()
                        ));
                    }
                }
                return Err(Error::Output(format!(
                    "cannot install {}: {error}{}",
                    target.display(),
                    if rollback_errors.is_empty() {
                        "; transaction rolled back".to_string()
                    } else {
                        format!("; rollback failed: {}", rollback_errors.join("; "))
                    }
                )));
            }
            installed.push(target.clone());
        }
        Ok(())
    })();
    if result.is_ok() {
        let _ = fs::remove_dir_all(&transaction);
    }
    result
}
