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

mod yaml_edit;

pub use yaml_edit::{
    apply_yaml_changes, has_top_level_key, set_yaml_path, yaml_value_changes, YamlChange,
};

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
    // Same lost-update hazard `config_file_lock` documents: env
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
/// `magician-config.yaml` has many independent settings writers (media,
/// vibedev deploy, harness pause, engine switches, decision mode, budgets,
/// workspace storage, privacy, critical delivery, ...). Each is a
/// read-modify-write over the whole file, and before this lock two changes to
/// *different sections* could each read the same file and each write their
/// own version: the later write took the file, and the other section silently
/// reverted. Writers now change only their own lines ([`write_yaml_section`]),
/// and still do the read-edit-write under this lock.
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

/// Writes what changed between `before` and `after` — two serializations of
/// the value at `section` — into the YAML file at `path`, changing only the
/// lines of the values that differ. Comments, unknown keys, key order and
/// untouched values stay byte for byte; equal values leave the file alone.
///
/// Every runtime-config settings writer comes through here (or through
/// [`apply_yaml_section`] when it already holds the file lock), so locking
/// here covers them without each caller remembering to.
pub fn write_yaml_section<T: serde::Serialize + ?Sized>(
    path: &Path,
    section: &[&str],
    before: &T,
    after: &T,
) -> std::io::Result<()> {
    write_yaml_section_seeded(path, section, before, after, None)
}

/// [`write_yaml_section`], first writing `seed` — a whole top-level block
/// carrying explanatory comments — when the file has no `section` block yet.
pub fn write_yaml_section_seeded<T: serde::Serialize + ?Sized>(
    path: &Path,
    section: &[&str],
    before: &T,
    after: &T,
    seed: Option<&str>,
) -> std::io::Result<()> {
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path)?;
    let next = apply_yaml_section_seeded(&original, section, before, after, seed)?;
    if next == original {
        return Ok(());
    }
    write_text_file_atomic(path, &next, None)
}

/// The text-only half of [`write_yaml_section`], for writers that hold the
/// config lock themselves or publish the file another way.
pub fn apply_yaml_section<T: serde::Serialize + ?Sized>(
    contents: &str,
    section: &[&str],
    before: &T,
    after: &T,
) -> std::io::Result<String> {
    apply_yaml_section_seeded(contents, section, before, after, None)
}

pub fn apply_yaml_section_seeded<T: serde::Serialize + ?Sized>(
    contents: &str,
    section: &[&str],
    before: &T,
    after: &T,
    seed: Option<&str>,
) -> std::io::Result<String> {
    let before = serde_yaml::to_value(before).map_err(std::io::Error::other)?;
    let after = serde_yaml::to_value(after).map_err(std::io::Error::other)?;
    apply_yaml_changes(contents, section, &before, &after, seed)
}

/// Replaces (or appends) a whole top-level block. Settings writers use
/// [`write_yaml_section`] instead; this remains for seeding a block that does
/// not exist yet and for building test fixtures.
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
    use super::{replace_top_level_yaml_block, set_yaml_path, write_yaml_section};

    /// One scalar under a top-level block, `value` already YAML.
    fn set_nested_yaml_scalar(contents: &str, top: &str, key: &str, value: Option<&str>) -> String {
        let value = value.map(|text| serde_yaml::from_str::<serde_yaml::Value>(text).unwrap());
        set_yaml_path(contents, &[top, key], value.as_ref()).unwrap()
    }

    const CONFIG: &str = "# top comment\nexecution:\n  # which engine thinks a run\n  harness_engine: magician  # owner note\n  harness_model: default\n  harness_auth:\n    # login first\n    mode: login_first\nchat:\n  harness_engine: magician\n";

    #[test]
    fn setting_a_nested_scalar_keeps_every_comment() {
        let updated = set_nested_yaml_scalar(CONFIG, "execution", "harness_engine", Some("pi"));
        assert_eq!(
            updated,
            CONFIG.replace(
                "harness_engine: magician  # owner note",
                "harness_engine: pi  # owner note"
            )
        );
    }

    #[test]
    fn a_missing_nested_key_is_added_at_the_end_of_its_block() {
        let updated = set_nested_yaml_scalar(CONFIG, "execution", "pi_profile", Some("openai"));
        assert!(
            updated.contains("    mode: login_first\n  pi_profile: openai\nchat:\n"),
            "{updated}"
        );
        assert!(updated.contains("# which engine thinks a run"));
    }

    #[test]
    fn removing_a_nested_key_drops_only_its_lines() {
        let with_profile =
            set_nested_yaml_scalar(CONFIG, "execution", "pi_profile", Some("openai"));
        assert_eq!(
            set_nested_yaml_scalar(&with_profile, "execution", "pi_profile", None),
            CONFIG
        );
        assert_eq!(
            set_nested_yaml_scalar(CONFIG, "execution", "pi_profile", None),
            CONFIG
        );
    }

    #[test]
    fn only_the_named_block_changes() {
        let updated = set_nested_yaml_scalar(CONFIG, "chat", "harness_engine", Some("claude_code"));
        assert!(updated.contains("  harness_engine: magician  # owner note\n"));
        assert!(updated.ends_with("chat:\n  harness_engine: claude_code\n"));
    }

    #[test]
    fn a_missing_block_is_appended() {
        let updated =
            set_nested_yaml_scalar("enabled: true\n", "chat", "harness_model", Some("default"));
        assert_eq!(
            updated,
            "enabled: true\n\nchat:\n  harness_model: default\n"
        );
    }

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

    #[derive(serde::Serialize, Clone)]
    struct Harness {
        paused: bool,
        shared_program_docs: Vec<String>,
    }

    #[test]
    fn a_section_write_changes_only_the_changed_lines_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("magician-config.yaml");
        let original = "harness:\n  # owner: pause on weekends\n  paused: false  # flip me\n  experimental_knob: 7\n  shared_program_docs:\n  - facts.md\nchat:\n  harness_engine: magician\n";
        std::fs::write(&path, original).unwrap();
        let before = Harness {
            paused: false,
            shared_program_docs: vec!["facts.md".to_string()],
        };
        write_yaml_section(&path, &["harness"], &before, &before).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let after = Harness {
            paused: true,
            ..before.clone()
        };
        write_yaml_section(&path, &["harness"], &before, &after).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original.replace("paused: false  # flip me", "paused: true  # flip me")
        );
    }
}
