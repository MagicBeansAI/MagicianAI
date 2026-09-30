//! Shared desktop-side resolution for backend runtime-owned paths.
//!
//! Features must consume this module instead of independently spelling the
//! live-root fallback or environment precedence. That keeps a configured root
//! change coherent across container, env-editor, and host-native capabilities.

use std::path::PathBuf;

pub const ROOT_ENV_KEYS: [&str; 2] = ["MAGICIAN_ROOT_DIR", "MAGICIAN_STORAGE_PATH"];
pub const DESKTOP_ROOT_ENV_KEY: &str = "MAGICIAN_DESKTOP_RUNTIME_ROOT";
pub const DEFAULT_ROOT_DIR_NAME: &str = "MagicianNotes";
#[cfg(feature = "native-wake")]
pub const VOSK_MODEL_DIR_NAME: &str = "vosk-model";

/// Runtime root used by the desktop's backend-adjacent capabilities.
///
/// Explicit environment configuration always wins. The home-directory default
/// is the project-wide local-install convention; the Application Support data
/// directory is only a last resort for environments without a home directory.
pub fn runtime_root_dir() -> PathBuf {
    for key in ROOT_ENV_KEYS {
        if let Ok(root) = std::env::var(key) {
            let trimmed = root.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
    }

    if let Ok(root) = std::env::var(DESKTOP_ROOT_ENV_KEY) {
        let trimmed = root.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    dirs::home_dir()
        .map(|home| home.join(DEFAULT_ROOT_DIR_NAME))
        .unwrap_or_else(crate::config::data_dir)
}

#[cfg(feature = "native-wake")]
pub fn vosk_model_dir() -> PathBuf {
    runtime_root_dir().join(VOSK_MODEL_DIR_NAME)
}
