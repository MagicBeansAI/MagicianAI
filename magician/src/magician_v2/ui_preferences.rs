use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error};
use crate::magician_v2::resource_authority::scoped_authority::is_safe_scope_id;

const UI_PREFERENCES_DIR: &str = "ui";
const UI_PREFERENCES_FILE: &str = "preferences.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiPreferences {
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Which permission posture the composer's Do mode carries: prompt before
    /// each in-scope file edit (`ask`), or perform them without a prompt
    /// (`accept_in_scope`).
    ///
    /// Scoped per principal+workspace rather than per browser, so relaxing the
    /// gate is a deliberate decision about one workspace instead of a setting
    /// that silently follows the operator onto every machine they sign into.
    #[serde(default = "default_composer_permission_mode")]
    pub composer_permission_mode: String,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            composer_permission_mode: default_composer_permission_mode(),
        }
    }
}

impl UiPreferences {
    pub fn normalize(mut self) -> Self {
        self.theme = normalized_theme_id(&self.theme).unwrap_or_else(default_theme);
        self.composer_permission_mode =
            normalized_composer_permission_mode(&self.composer_permission_mode)
                .unwrap_or_else(default_composer_permission_mode);
        self
    }

    /// Whether this scope has opted out of the per-edit permission prompt.
    pub fn accepts_in_scope_edits(&self) -> bool {
        self.composer_permission_mode == COMPOSER_PERMISSION_ACCEPT_IN_SCOPE
    }
}

pub const COMPOSER_PERMISSION_ASK: &str = "ask";
pub const COMPOSER_PERMISSION_ACCEPT_IN_SCOPE: &str = "accept_in_scope";

fn default_theme() -> String {
    "longhand".to_string()
}

fn default_composer_permission_mode() -> String {
    COMPOSER_PERMISSION_ASK.to_string()
}

/// Anything unrecognised becomes `ask`.
///
/// This normalises a *permission* value, so the failure direction is not
/// arbitrary: a corrupted or future-versioned file must fall back to the
/// posture that still prompts, never to the one that stops asking.
fn normalized_composer_permission_mode(value: &str) -> Option<String> {
    match value.trim() {
        COMPOSER_PERMISSION_ASK => Some(COMPOSER_PERMISSION_ASK.to_string()),
        COMPOSER_PERMISSION_ACCEPT_IN_SCOPE => {
            Some(COMPOSER_PERMISSION_ACCEPT_IN_SCOPE.to_string())
        },
        _ => None,
    }
}

fn normalized_theme_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 80 {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }
    Some(trimmed.to_string())
}

#[derive(Debug, Clone)]
pub struct UiPreferencesStore {
    workspace_layout: ArtifactV2Workspace,
}

impl UiPreferencesStore {
    pub fn new<P: AsRef<Path>>(storage_root: P) -> Self {
        Self::with_workspace_layout(ArtifactV2Workspace::new(storage_root))
    }

    pub fn with_workspace_layout(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub async fn load(&self, principal: &str, workspace: &str) -> std::io::Result<UiPreferences> {
        let path = match self.preferences_path(principal, workspace) {
            Some(path) => path,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unsafe principal/workspace scope",
                ));
            },
        };
        let bytes = match self.workspace_layout.read_path(&path).await {
            Ok(bytes) => bytes,
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(UiPreferences::default());
            },
            Err(error) => return Err(artifact_v2_error_to_io(error)),
        };
        let preferences = serde_json::from_slice::<UiPreferences>(&bytes)
            .map(UiPreferences::normalize)
            .unwrap_or_default();
        Ok(preferences)
    }

    pub async fn exists(&self, principal: &str, workspace: &str) -> std::io::Result<bool> {
        let path = match self.preferences_path(principal, workspace) {
            Some(path) => path,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unsafe principal/workspace scope",
                ));
            },
        };
        self.workspace_layout
            .exists_path(&path)
            .await
            .map_err(artifact_v2_error_to_io)
    }

    pub async fn save(
        &self,
        principal: &str,
        workspace: &str,
        preferences: UiPreferences,
    ) -> std::io::Result<UiPreferences> {
        let path = match self.preferences_path(principal, workspace) {
            Some(path) => path,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unsafe principal/workspace scope",
                ));
            },
        };
        let preferences = preferences.normalize();
        self.workspace_layout
            .write_json_atomic_path(&path, &preferences)
            .await
            .map_err(artifact_v2_error_to_io)?;
        Ok(preferences)
    }

    fn preferences_path(&self, principal: &str, workspace: &str) -> Option<PathBuf> {
        if !is_safe_scope_id(principal) || !is_safe_scope_id(workspace) {
            return None;
        }
        Some(
            self.workspace_layout
                .scope_root(principal, workspace)
                .join(UI_PREFERENCES_DIR)
                .join(UI_PREFERENCES_FILE),
        )
    }
}

fn artifact_v2_error_to_io(error: ArtifactV2Error) -> std::io::Error {
    match error {
        ArtifactV2Error::Io(error) => error,
        other => std::io::Error::other(other),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn ui_preferences_normalizes_theme_id() {
        assert_eq!(
            UiPreferences {
                theme: " jarvis-light ".to_string(),
                ..UiPreferences::default()
            }
            .normalize()
            .theme,
            "jarvis-light"
        );
        assert_eq!(
            UiPreferences {
                theme: "../bad".to_string(),
                ..UiPreferences::default()
            }
            .normalize()
            .theme,
            "longhand"
        );
    }

    #[test]
    fn composer_permission_mode_defaults_to_asking() {
        let defaults = UiPreferences::default();
        assert_eq!(defaults.composer_permission_mode, COMPOSER_PERMISSION_ASK);
        assert!(!defaults.accepts_in_scope_edits());
    }

    #[test]
    fn composer_permission_mode_round_trips_accept() {
        let value = UiPreferences {
            composer_permission_mode: format!("  {COMPOSER_PERMISSION_ACCEPT_IN_SCOPE}  "),
            ..UiPreferences::default()
        }
        .normalize();
        assert_eq!(
            value.composer_permission_mode,
            COMPOSER_PERMISSION_ACCEPT_IN_SCOPE
        );
        assert!(value.accepts_in_scope_edits());
    }

    #[test]
    fn an_unrecognised_permission_mode_falls_back_to_asking_not_accepting() {
        // The failure direction matters: this normalises a permission, so a
        // corrupt or future-versioned file must land on the posture that still
        // prompts. Falling back to accept would silently disarm the gate.
        for raw in [
            "",
            "accept",
            "yes",
            "true",
            "ACCEPT_IN_SCOPE",
            "../bad",
            "plan",
        ] {
            let value = UiPreferences {
                composer_permission_mode: raw.to_string(),
                ..UiPreferences::default()
            }
            .normalize();
            assert_eq!(
                value.composer_permission_mode, COMPOSER_PERMISSION_ASK,
                "{raw:?} must normalise to ask"
            );
            assert!(!value.accepts_in_scope_edits(), "{raw:?} must not accept");
        }
    }

    #[test]
    fn a_preferences_file_written_before_this_field_existed_still_asks() {
        let legacy: UiPreferences =
            serde_json::from_str(r#"{"theme":"longhand"}"#).expect("legacy preferences");
        assert_eq!(legacy.composer_permission_mode, COMPOSER_PERMISSION_ASK);
        assert!(!legacy.accepts_in_scope_edits());
    }
}
