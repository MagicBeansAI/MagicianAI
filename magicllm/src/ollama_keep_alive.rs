use std::sync::{OnceLock, RwLock};

pub const DEFAULT_OLLAMA_KEEP_ALIVE: &str = "10m";

static CONFIGURED_DEFAULT: OnceLock<RwLock<Option<Option<String>>>> = OnceLock::new();

fn configured_default() -> &'static RwLock<Option<Option<String>>> {
    CONFIGURED_DEFAULT.get_or_init(|| RwLock::new(None))
}

pub fn set_default_ollama_keep_alive(value: Option<String>) {
    if let Ok(mut guard) = configured_default().write() {
        *guard = Some(value.and_then(|value| normalize_keep_alive(&value)));
    }
}

pub(crate) fn normalize_keep_alive(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn keep_alive_from_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .and_then(|value| normalize_keep_alive(&value))
    })
}

pub(crate) fn default_keep_alive() -> Option<String> {
    keep_alive_from_env(&["MAGICLLM_OLLAMA_KEEP_ALIVE", "MAGICIAN_OLLAMA_KEEP_ALIVE"]).or_else(
        || {
            configured_default()
                .read()
                .ok()
                .and_then(|guard| (*guard).clone())
                .unwrap_or_else(|| Some(DEFAULT_OLLAMA_KEEP_ALIVE.to_string()))
        },
    )
}

pub(crate) fn request_keep_alive(value: Option<&str>) -> Option<String> {
    value
        .and_then(normalize_keep_alive)
        .or_else(default_keep_alive)
}

/// Ollama accepts duration strings such as `10m`, but numeric sentinel values
/// such as `-1` (pin indefinitely) and `0` (unload immediately) must be JSON
/// numbers. Recent Ollama releases reject those sent as quoted strings.
///
/// Canonical home of the numeric-sentinel encoding: `magician-vector-index`
/// routes its embedding keep-alive through this fn rather than keeping a copy.
pub fn request_keep_alive_value(value: &str) -> serde_json::Value {
    let trimmed = value.trim();
    match trimmed.parse::<i64>() {
        Ok(seconds) => serde_json::Value::Number(seconds.into()),
        Err(_) => serde_json::Value::String(trimmed.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::request_keep_alive_value;
    use serde_json::json;

    #[test]
    fn embedding_keep_alive_serializes_numeric_sentinels_as_numbers() {
        assert_eq!(request_keep_alive_value("-1"), json!(-1));
        assert_eq!(request_keep_alive_value("0"), json!(0));
        assert_eq!(request_keep_alive_value("10m"), json!("10m"));
    }
}
