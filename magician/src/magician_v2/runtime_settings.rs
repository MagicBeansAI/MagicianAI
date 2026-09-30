//! Shared runtime settings file helpers.
//!
//! UI-backed settings endpoints should use this module for runtime env/config
//! writes, then keep feature-specific validation and allowlists in their own
//! API handlers.

use std::collections::{HashMap, HashSet};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RuntimeSettingsPaths {
    pub runtime_root: PathBuf,
    pub config_path: PathBuf,
    pub env_target_path: PathBuf,
    pub env_target_mode: &'static str,
    pub env_development_path: PathBuf,
    pub env_path: PathBuf,
}

pub fn runtime_settings_paths() -> RuntimeSettingsPaths {
    let runtime_root = crate::magician_v2::process_storage::runtime_root();
    let env_target_mode = active_runtime_env_mode();
    let env_target_path = match env_target_mode {
        "development" => runtime_root.join(".env.development"),
        _ => runtime_root.join(".env"),
    };
    RuntimeSettingsPaths {
        config_path: crate::config::magician_config_path(),
        env_target_path,
        env_target_mode,
        env_development_path: runtime_root.join(".env.development"),
        env_path: runtime_root.join(".env"),
        runtime_root,
    }
}

pub fn active_runtime_env_mode() -> &'static str {
    if let Ok(value) = std::env::var("MAGICIAN_ENV") {
        match value.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" => return "development",
            "production" | "prod" | "release" => return "production",
            _ => {},
        }
    }
    if cfg!(debug_assertions) || std::env::var("TAURI_DEBUG").is_ok() {
        "development"
    } else {
        "production"
    }
}

pub fn read_env_file_values(path: &Path) -> HashMap<String, String> {
    if !path.is_file() {
        return HashMap::new();
    }
    match dotenvy::from_path_iter(path) {
        Ok(iter) => iter
            .filter_map(Result::ok)
            .map(|(key, value)| (key, value))
            .collect(),
        Err(_) => HashMap::new(),
    }
}

pub fn update_env_file(path: &Path, updates: &[(&str, Option<String>)]) -> std::io::Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    // Same lost-update hazard `write_top_level_yaml_block` documents: env
    // files have concurrent read-modify-write writers (the runtime env API,
    // per-skill env API, vibedev and harness settings) on a multithreaded
    // server, so the whole span holds the per-path lock.
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let mut seen = HashSet::new();
    let mut lines = Vec::new();
    for line in original.lines() {
        let key = env_line_key(line);
        if let Some(key) = key {
            if let Some((_, value)) = updates.iter().find(|(update_key, _)| *update_key == key) {
                seen.insert(key.to_string());
                if let Some(value) = value {
                    lines.push(format!("{key}={}", quote_env_value(value)));
                }
                continue;
            }
        }
        lines.push(line.to_string());
    }
    for (key, value) in updates {
        if seen.contains(*key) {
            continue;
        }
        if let Some(value) = value {
            lines.push(format!("{key}={}", quote_env_value(value)));
        }
    }
    let mut next = lines.join("\n");
    next.push('\n');
    write_text_file_atomic(path, &next, Some(0o600))
}

pub fn sync_process_env_from_file(path: &Path, keys: &[&str]) {
    let values = read_env_file_values(path);
    for key in keys {
        if let Some(value) = values.get(*key).filter(|value| !value.trim().is_empty()) {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }
}

fn env_line_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let (key, _) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(key)
}

pub fn quote_env_value(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "_-./:@%+=,".contains(ch))
    {
        return value.to_string();
    }
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

/// One lock per config file, for the life of the process.
///
/// `magician-config.yaml` has four independent writers: three top-level block
/// replacements (`media`, `vibedev_deploy`, and the settings endpoint) plus
/// `WorkspaceStorageSettingsStore::save`, which deserializes the whole config,
/// swaps its `workspace_storage` section and re-serializes. Every one of them is
/// a read-modify-write over the entire file, and none of them held a lock — so
/// two settings changes to *different sections* each read the same file and each
/// wrote their own whole version. The later write took the file, and the other
/// section silently reverted to what it was before.
///
/// Keyed by path so a test writing its own config never waits on the real one.
/// `std::sync::Mutex` because both writers are synchronous here — the async
/// caller does its load-edit-write inside `spawn_blocking` precisely so this
/// stays one kind of lock.
pub fn config_file_lock(path: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, std::sync::Arc<std::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let mut registry = LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::sync::Arc::clone(registry.entry(path.to_path_buf()).or_default())
}

pub fn write_top_level_yaml_block(path: &Path, key: &str, block: &str) -> std::io::Result<()> {
    // Guards read -> replace -> write. All three block writers come through
    // here, so locking here covers them without each caller remembering to.
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path)?;
    let next = replace_top_level_yaml_block(&original, key, block);
    write_text_file_atomic(path, &next, None)
}

pub fn replace_top_level_yaml_block(contents: &str, key: &str, block: &str) -> String {
    let lines = contents.split_inclusive('\n').collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| is_top_level_yaml_key(line, key));
    let Some(start) = start else {
        let mut out = contents.trim_end_matches('\n').to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(block);
        return out;
    };
    let mut end = lines.len();
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            end = index;
            break;
        }
    }
    let mut out = String::new();
    for line in &lines[..start] {
        out.push_str(line);
    }
    out.push_str(block);
    if end < lines.len() && !out.ends_with('\n') {
        out.push('\n');
    }
    for line in &lines[end..] {
        out.push_str(line);
    }
    out
}

fn is_top_level_yaml_key(line: &str, key: &str) -> bool {
    if line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    line.trim_end_matches(['\r', '\n'])
        .split_once(':')
        .is_some_and(|(candidate, _)| candidate.trim() == key)
}

pub fn yaml_string(value: &str) -> String {
    serde_yaml::to_string(value)
        .unwrap_or_else(|_| format!("{value:?}"))
        .trim()
        .trim_start_matches("---")
        .trim()
        .to_string()
}

pub fn write_text_file_atomic(
    path: &Path,
    contents: &str,
    unix_mode: Option<u32>,
) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target path has no parent",
        ));
    };
    std::fs::create_dir_all(parent)?;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target path has no file name",
        )
    })?;
    let temp_path = parent.join(format!(
        ".{}.tmp-{}-{}",
        file_name.to_string_lossy(),
        std::process::id(),
        chrono::Utc::now().timestamp_micros()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if let Some(mode) = unix_mode {
        options.mode(mode);
    }
    let write_result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temp_path)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            let mode = unix_mode.or_else(|| {
                std::fs::metadata(path)
                    .ok()
                    .map(|metadata| metadata.permissions().mode() & 0o777)
            });
            if let Some(mode) = mode {
                std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(mode))?;
            }
        }
        std::fs::rename(&temp_path, path)
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    write_result
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::replace_top_level_yaml_block;

    #[test]
    fn replaces_inline_top_level_yaml_block_without_duplicating_the_key() {
        let contents = "enabled: true\nmedia: {}\nfrontend:\n  host: 127.0.0.1\n";
        let replacement = "media:\n  surface_profiles:\n    default_mapping: {}";

        let updated = replace_top_level_yaml_block(contents, "media", replacement);

        assert_eq!(updated.matches("media:").count(), 1);
        assert_eq!(
            updated,
            "enabled: true\nmedia:\n  surface_profiles:\n    default_mapping: {}\nfrontend:\n  host: 127.0.0.1\n"
        );
    }
}
