use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const DESKTOP_ENV_FILE_OVERRIDE: &str = "MAGICIAN_DESKTOP_ENV_FILE";
const ADDED_BY_MARKER: &str = "# Added by Magican Desktop";

#[derive(Debug, Clone, Serialize)]
pub struct EnvSnapshot {
    pub mode: String,
    pub path: String,
    pub exists: bool,
    pub entries: Vec<EnvEntry>,
    pub restart_required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvEntry {
    pub key: String,
    pub label: String,
    pub category: String,
    pub description: String,
    pub known: bool,
    pub secret: bool,
    pub present: bool,
    pub empty: bool,
    pub value_preview: String,
    pub requires_restart: bool,
    pub config_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvValueResponse {
    pub key: String,
    pub present: bool,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaveEnvValueRequest {
    pub key: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone)]
struct KnownEnvKey {
    key: &'static str,
    label: &'static str,
    category: &'static str,
    description: &'static str,
    secret: bool,
    requires_restart: bool,
}

#[tauri::command]
pub fn get_environment_snapshot() -> Result<EnvSnapshot, String> {
    refuse_engine_owned_env_file()?;
    environment_snapshot()
}

#[tauri::command]
pub fn reveal_environment_value(key: String) -> Result<EnvValueResponse, String> {
    validate_env_key(&key)?;
    refuse_engine_owned_env_file()?;
    let (_, path) = active_env_file();
    let content = read_env_file_lossy(&path)?;
    let values = parse_env_values(&content);
    Ok(EnvValueResponse {
        key: key.clone(),
        present: values.contains_key(&key),
        value: values.get(&key).cloned().unwrap_or_default(),
    })
}

#[tauri::command]
pub fn save_environment_value(request: SaveEnvValueRequest) -> Result<EnvSnapshot, String> {
    validate_env_key(&request.key)?;
    refuse_engine_owned_env_file()?;
    let (_, path) = active_env_file();
    let content = read_env_file_lossy(&path)?;
    let next = update_env_content(&content, &request.key, request.value.as_deref());
    write_env_file_atomically(&path, &next)?;
    environment_snapshot()
}

fn environment_snapshot() -> Result<EnvSnapshot, String> {
    let (mode, path) = active_env_file();
    let content = read_env_file_lossy(&path)?;
    let values = parse_env_values(&content);
    let mut keys: BTreeSet<String> = known_env_keys()
        .iter()
        .map(|known| known.key.to_string())
        .collect();
    keys.extend(values.keys().cloned());

    let known_by_key: BTreeMap<&'static str, KnownEnvKey> = known_env_keys()
        .into_iter()
        .map(|known| (known.key, known))
        .collect();
    let mut entries = Vec::new();
    for key in keys {
        let known = known_by_key.get(key.as_str());
        let value = values.get(&key).cloned().unwrap_or_default();
        let present = values.contains_key(&key);
        let secret = known
            .map(|known| known.secret)
            .unwrap_or_else(|| looks_like_secret_key(&key));
        entries.push(EnvEntry {
            label: known
                .map(|known| known.label)
                .unwrap_or(key.as_str())
                .to_string(),
            category: known
                .map(|known| known.category)
                .unwrap_or("Advanced")
                .to_string(),
            description: known
                .map(|known| known.description)
                .unwrap_or("Custom environment variable.")
                .to_string(),
            known: known.is_some(),
            secret,
            present,
            empty: present && value.is_empty(),
            value_preview: value_preview(&value, secret, present),
            requires_restart: known.map(|known| known.requires_restart).unwrap_or(true),
            config_path: config_path_for_env_key(&key).map(str::to_string),
            key,
        });
    }

    entries.sort_by(|left, right| {
        let left_known_rank = known_rank(&left.key).unwrap_or(usize::MAX);
        let right_known_rank = known_rank(&right.key).unwrap_or(usize::MAX);
        left_known_rank
            .cmp(&right_known_rank)
            .then_with(|| left.category.cmp(&right.category))
            .then_with(|| left.key.cmp(&right.key))
    });

    Ok(EnvSnapshot {
        mode,
        path: path.display().to_string(),
        exists: path.is_file(),
        entries,
        restart_required: true,
    })
}

fn env_file_override_active() -> bool {
    std::env::var(DESKTOP_ENV_FILE_OVERRIDE)
        .ok()
        .is_some_and(|path| !path.trim().is_empty())
}

fn refuse_engine_owned_env_file() -> Result<(), String> {
    if env_file_override_active() {
        return Ok(());
    }
    crate::engine_roots::refuse_engine_owned_filesystem("read or write")
}

fn active_env_file() -> (String, PathBuf) {
    if let Ok(path) = std::env::var(DESKTOP_ENV_FILE_OVERRIDE) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return ("override".to_string(), PathBuf::from(trimmed));
        }
    }

    let debug_mode = cfg!(debug_assertions) || std::env::var("TAURI_DEBUG").is_ok();
    let filename = if debug_mode {
        ".env.development"
    } else {
        ".env"
    };
    let mode = if debug_mode {
        "development"
    } else {
        "production"
    }
    .to_string();
    // Use the shared desktop runtime-root resolver so the env editor reads and
    // writes the same file as every other backend-adjacent desktop capability.
    let base_dir = crate::runtime_paths::runtime_root_dir();
    (mode, base_dir.join(filename))
}

fn read_env_file_lossy(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("failed to read {}: {}", path.display(), error)),
    }
}

fn write_env_file_atomically(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {}", parent.display(), error))?;
    }
    let tmp_path = path.with_extension("env.tmp");
    std::fs::write(&tmp_path, content)
        .map_err(|error| format!("failed to write {}: {}", tmp_path.display(), error))?;
    std::fs::rename(&tmp_path, path)
        .map_err(|error| format!("failed to replace {}: {}", path.display(), error))?;
    Ok(())
}

fn parse_env_values(content: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in content.lines() {
        if let Some((key, raw_value)) = parse_env_assignment(line) {
            values.insert(key.to_string(), parse_env_value(raw_value));
        }
    }
    values
}

fn update_env_content(content: &str, key: &str, value: Option<&str>) -> String {
    let mut lines = Vec::new();
    let mut updated = false;
    for line in content.lines() {
        if parse_env_assignment(line).is_some_and(|(candidate, _)| candidate == key) {
            if let Some(value) = value {
                if !updated {
                    lines.push(render_env_assignment(key, value));
                    updated = true;
                }
            }
            continue;
        }
        lines.push(line.to_string());
    }

    if let Some(value) = value {
        if !updated {
            if !lines.is_empty() && lines.last().is_some_and(|line| !line.trim().is_empty()) {
                lines.push(String::new());
            }
            if !lines.iter().any(|line| line.trim() == ADDED_BY_MARKER) {
                lines.push(ADDED_BY_MARKER.to_string());
            }
            lines.push(render_env_assignment(key, value));
        }
    }

    let mut next = lines.join("\n");
    if !next.is_empty() {
        next.push('\n');
    }
    next
}

fn parse_env_assignment(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let (key, value) = trimmed.split_once('=')?;
    let key = key.trim();
    if is_valid_env_key(key) {
        Some((key, value.trim_start()))
    } else {
        None
    }
}

fn parse_env_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        if bytes.first() == Some(&b'\'') && bytes.last() == Some(&b'\'') {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
        if bytes.first() == Some(&b'"') && bytes.last() == Some(&b'"') {
            return unescape_double_quoted(&trimmed[1..trimmed.len() - 1]);
        }
    }
    trimmed.to_string()
}

fn unescape_double_quoted(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('"') => output.push('"'),
            Some('\\') => output.push('\\'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            },
            None => output.push('\\'),
        }
    }
    output
}

fn render_env_assignment(key: &str, value: &str) -> String {
    if value.is_empty() {
        return format!("{key}=");
    }
    if value.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '_' | '-' | '.' | '/' | ':' | '@' | '%' | '+' | '=' | ','
            )
    }) {
        return format!("{key}={value}");
    }
    format!("{key}=\"{}\"", escape_double_quoted(value))
}

fn escape_double_quoted(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn value_preview(value: &str, secret: bool, present: bool) -> String {
    if !present {
        return "Not set".to_string();
    }
    if value.is_empty() {
        return "Empty".to_string();
    }
    if !secret {
        return value.to_string();
    }
    let suffix: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("••••{suffix}")
}

fn validate_env_key(key: &str) -> Result<(), String> {
    if is_valid_env_key(key) {
        Ok(())
    } else {
        Err("environment variable names must start with a letter or underscore and contain only letters, numbers, and underscores".to_string())
    }
}

fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {},
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn looks_like_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("KEY")
        || upper.contains("TOKEN")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("CREDENTIAL")
        || upper.contains("COOKIE")
        || upper.contains("PRIVATE")
}

fn known_rank(key: &str) -> Option<usize> {
    known_env_keys().iter().position(|known| known.key == key)
}

fn config_path_for_env_key(key: &str) -> Option<&'static str> {
    match key {
        "OPENAI_API_KEY" | "ANTHROPIC_API_KEY" | "GEMINI_API_KEY" | "GOOGLE_API_KEY"
        | "DEEPSEEK_API_KEY" | "OPENROUTER_API_KEY" | "SARVAM_API_KEY" => {
            Some("llm.router.profiles.*.api_key_env")
        },
        "MINIMAX_API_KEY" => Some("media.tts.providers.*.api_key_env"),
        "MAGICIAN_MINIMAX_GROUP_ID" => Some("media.tts.providers.*.group_id_env"),
        "MAGICIAN_CONFIG_PATH" => Some("magician-config.yaml path override"),
        "MAGICIAN_STORAGE_PATH" | "MAGICIAN_ROOT_DIR" => Some("storage_path"),
        "MAGICIAN_PORT" => Some("network.magician_port"),
        "MAGICUTOR_PORT" => Some("network.magicutor_port"),
        "MAGICIAN_OWNER_KAPSO_IDENTITIES"
        | "MAGICIAN_OWNER_TELEGRAM_IDENTITIES"
        | "MAGICIAN_OWNER_AGENTMAIL_IDENTITIES" => Some("envoy.owner_identity_envs"),
        "MAGICIAN_OLLAMA_KEEP_ALIVE"
        | "MAGICLLM_OLLAMA_KEEP_ALIVE"
        | "MAGICIAN_OLLAMA_EMBEDDING_KEEP_ALIVE"
        | "MAGICIAN_MEMORY_EMBEDDING_KEEP_ALIVE"
        | "MAGICIAN_MEMORY_OLLAMA_KEEP_ALIVE"
        | "MEET_BOT_OLLAMA_KEEP_ALIVE" => Some("runtime.ollama.keep_alive"),
        "MEET_BOT_TTS_VOICE" => Some("media.tts.providers.*.voice"),
        "MEET_BOT_TTS_MODEL" => Some("media.tts.providers.*.model"),
        _ => None,
    }
}

fn known_env_keys() -> Vec<KnownEnvKey> {
    vec![
        KnownEnvKey {
            key: "OPENAI_API_KEY",
            label: "OpenAI API Key",
            category: "Provider Keys",
            description: "Used by OpenAI chat, realtime, STT, TTS, and analysis profiles.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "ANTHROPIC_API_KEY",
            label: "Anthropic API Key",
            category: "Provider Keys",
            description: "Used by Anthropic Claude profiles and Claude-backed skills.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "GEMINI_API_KEY",
            label: "Gemini API Key",
            category: "Provider Keys",
            description: "Used by Gemini chat, live voice, image, and video providers.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "GOOGLE_API_KEY",
            label: "Google API Key",
            category: "Provider Keys",
            description: "Fallback key used by Google/Gemini-backed tools.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MINIMAX_API_KEY",
            label: "MiniMax API Key",
            category: "Provider Keys",
            description: "Used by MiniMax TTS and MiniMax media skills.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_MINIMAX_GROUP_ID",
            label: "MiniMax Group ID",
            category: "Provider Keys",
            description: "MiniMax account group identifier required by MiniMax audio APIs.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "VEO31_API_KEY",
            label: "Veo API Key",
            category: "Provider Keys",
            description: "Optional direct key for the Veo 3.1 video generation capability.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "DEEPSEEK_API_KEY",
            label: "DeepSeek API Key",
            category: "Provider Keys",
            description: "Used by DeepSeek LLM profiles when configured.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "SARVAM_API_KEY",
            label: "Sarvam API Key",
            category: "Provider Keys",
            description: "Used by Sarvam (Indic-language) LLM profiles when configured.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "OPENROUTER_API_KEY",
            label: "OpenRouter API Key",
            category: "Provider Keys",
            description: "Used by OpenRouter LLM profiles when configured.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "EXA_API_KEY",
            label: "Exa API Key",
            category: "Provider Keys",
            description: "Used by web research and search skills.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "TAVILY_API_KEY",
            label: "Tavily API Key",
            category: "Provider Keys",
            description: "Used by web research and search skills.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "CLOAKBROWSER_LICENSE_KEY",
            label: "CloakBrowser License Key",
            category: "Provider Keys",
            description:
                "Unlocks the current CloakBrowser binary; free keys permit one concurrent session.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_ENV",
            label: "Backend environment",
            category: "Runtime",
            description: "Runtime environment label for backend services.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_RUNTIME_MODE",
            label: "Runtime Mode",
            category: "Runtime",
            description: "Runtime mode passed to the backend by the supervisor.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_PORT",
            label: "Magician backend port",
            category: "Runtime",
            description: "Override port for the Magician backend.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICUTOR_PORT",
            label: "Magicutor Port",
            category: "Runtime",
            description: "Override port for Magicutor.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_HOST_GATEWAY_URL",
            label: "Host Gateway URL",
            category: "Runtime",
            description: "Loopback URL runtime services use to reach desktop-native services.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_DESKTOP_MANAGE_RUNTIME",
            label: "Desktop Manages Runtime",
            category: "Runtime",
            description: "Set to 0/false to let Makefile or another supervisor own services.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_CONFIG_PATH",
            label: "Magician backend config path",
            category: "Runtime",
            description: "Optional path to magician-config.yaml.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_STORAGE_PATH",
            label: "Storage Path",
            category: "Runtime",
            description: "Optional root for artifact and storage data.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_ROOT_DIR",
            label: "Runtime Root Directory",
            category: "Runtime",
            description: "Optional runtime root override used before the default storage path.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_OWNER_KAPSO_IDENTITIES",
            label: "Kapso Owner Identities",
            category: "Access",
            description:
                "Private comma-separated Kapso/WhatsApp sender identities referenced by envoy.owner_identity_envs.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_OWNER_TELEGRAM_IDENTITIES",
            label: "Telegram Owner Identities",
            category: "Access",
            description:
                "Private comma-separated Telegram numeric chat ids referenced by envoy.owner_identity_envs.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_OWNER_AGENTMAIL_IDENTITIES",
            label: "AgentMail Owner Identities",
            category: "Access",
            description:
                "Private comma-separated AgentMail owner email identities referenced by envoy.owner_identity_envs.",
            secret: true,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_OLLAMA_KEEP_ALIVE",
            label: "Ollama Keep Alive",
            category: "Runtime",
            description: "Override runtime.ollama.keep_alive for local Ollama model residency.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICLLM_OLLAMA_KEEP_ALIVE",
            label: "MagicLLM Ollama Keep Alive",
            category: "Runtime",
            description: "Lower-level Ollama provider override; prefer runtime.ollama.keep_alive or MAGICIAN_OLLAMA_KEEP_ALIVE.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_OLLAMA_EMBEDDING_KEEP_ALIVE",
            label: "Ollama Embedding Keep Alive",
            category: "Runtime",
            description: "Embedding-specific Ollama keep_alive override for vector and memory index calls.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_MEMORY_EMBEDDING_KEEP_ALIVE",
            label: "Memory Embedding Keep Alive",
            category: "Runtime",
            description: "Legacy memory-index embedding keep_alive override; prefer runtime.ollama.keep_alive.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_MEMORY_OLLAMA_KEEP_ALIVE",
            label: "Memory Ollama Keep Alive",
            category: "Runtime",
            description: "Legacy memory Ollama keep_alive override; prefer runtime.ollama.keep_alive.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MEET_BOT_STT",
            label: "Meeting STT Mode",
            category: "Meetings",
            description: "Meeting transcription mode, such as local or cloud.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MEET_BOT_STT_SEGMENT_SECS",
            label: "Meeting STT Segment Seconds",
            category: "Meetings",
            description: "Chunk duration for meeting STT capture.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MEET_LISTEN_AUTO_STOP_SECS",
            label: "Passive Listen Auto Stop",
            category: "Meetings",
            description: "Idle auto-stop duration for passive meeting listening.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MEET_LISTEN_MAX_SECS",
            label: "Passive Listen Max Duration",
            category: "Meetings",
            description: "Maximum duration for passive meeting listening.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MEET_BOT_THREAD",
            label: "Meeting Thread",
            category: "Meetings",
            description: "Default chat thread used by meeting transcripts and responses.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MEET_BOT_TTS_VOICE",
            label: "Meeting TTS Voice",
            category: "Meetings",
            description: "Optional voice override for meeting responses.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MEET_BOT_TTS_MODEL",
            label: "Meeting TTS Model",
            category: "Meetings",
            description: "Optional TTS model override for meeting responses.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MEET_BOT_OLLAMA_KEEP_ALIVE",
            label: "Meeting Ollama Keep Alive",
            category: "Meetings",
            description: "Meeting fallback summarizer keep_alive override; prefer runtime.ollama.keep_alive.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_MACOS_SPEECH_HELPER_BIN",
            label: "macOS Speech Helper",
            category: "Host Helpers",
            description: "Override path for the macOS speech helper binary.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_MACOS_MEET_AUDIO_BIN",
            label: "macOS Meeting Audio Helper",
            category: "Host Helpers",
            description: "Override path for the macOS meeting audio helper binary.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_MACOS_PRESENCE_HOST_BIN",
            label: "macOS Presence Host",
            category: "Host Helpers",
            description: "Override path for the macOS presence host binary.",
            secret: false,
            requires_restart: true,
        },
        KnownEnvKey {
            key: "MAGICIAN_VOSK_MODEL_DIR",
            label: "Vosk Model Directory",
            category: "Host Helpers",
            description: "Override path for the local wake-word Vosk model.",
            secret: false,
            requires_restart: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_values_handles_quotes_and_export() {
        let values = parse_env_values("export FOO=\"bar baz\"\nPLAIN=abc\n# SKIP=no\n");
        assert_eq!(values.get("FOO"), Some(&"bar baz".to_string()));
        assert_eq!(values.get("PLAIN"), Some(&"abc".to_string()));
        assert!(!values.contains_key("SKIP"));
    }

    #[test]
    fn update_env_content_replaces_and_removes_duplicates() {
        let next = update_env_content("FOO=old\nBAR=1\nFOO=older\n", "FOO", Some("new value"));
        assert_eq!(next, "FOO=\"new value\"\nBAR=1\n");
    }

    #[test]
    fn update_env_content_appends_new_key_with_marker() {
        let next = update_env_content("FOO=1\n", "BAR", Some("2"));
        assert_eq!(next, "FOO=1\n\n# Added by Magican Desktop\nBAR=2\n");
    }

    #[test]
    fn update_env_content_deletes_key() {
        let next = update_env_content("FOO=1\nBAR=2\n", "FOO", None);
        assert_eq!(next, "BAR=2\n");
    }

    #[test]
    fn cloakbrowser_license_is_a_known_env_only_secret() {
        let key = known_env_keys()
            .into_iter()
            .find(|known| known.key == "CLOAKBROWSER_LICENSE_KEY")
            .expect("CloakBrowser license key should be editable from Desktop Settings");
        assert!(key.secret);
        assert!(key.requires_restart);
        assert_eq!(config_path_for_env_key(key.key), None);
    }
}
