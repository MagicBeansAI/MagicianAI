use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error};
use crate::magician_v2::resource_authority::scoped_authority::is_safe_scope_id;

use super::{AudioRuntimeConfigManager, AudioStage, AudioSurface};

const MEDIA_PREFERENCES_DIR: &str = "media";
const MEDIA_PREFERENCES_FILE: &str = "preferences.json";
pub const MEDIA_PREFERENCES_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaPreferences {
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub auto_speak: bool,
    #[serde(default = "default_voice_mode")]
    pub voice_mode: String,
    /// Require an explicit "Hey <primary-agent name or alias>" address before
    /// open-mic voice transcripts become conversation turns.
    #[serde(default = "default_require_voice_prefix")]
    pub require_voice_prefix: bool,
    /// Optional scoped profile choice per workflow. Missing means use the
    /// configured global default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub surface_profiles: BTreeMap<AudioSurface, String>,
    /// Optional per-stage option IDs per workflow. Values are validated against
    /// the backend-advertised catalog by the preferences API before persistence.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub surface_stage_options: BTreeMap<AudioSurface, BTreeMap<AudioStage, String>>,
    /// Speakable voice id per realtime / Live profile (`voice_realtime_*`).
    /// Empty means the profile YAML / provider default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub realtime_voices: BTreeMap<String, String>,
}

impl Default for MediaPreferences {
    fn default() -> Self {
        Self {
            schema_version: MEDIA_PREFERENCES_SCHEMA_VERSION,
            auto_speak: false,
            voice_mode: default_voice_mode(),
            require_voice_prefix: default_require_voice_prefix(),
            surface_profiles: BTreeMap::new(),
            surface_stage_options: BTreeMap::new(),
            realtime_voices: BTreeMap::new(),
        }
    }
}

impl MediaPreferences {
    pub fn normalize(mut self) -> Self {
        self.schema_version = MEDIA_PREFERENCES_SCHEMA_VERSION;
        self.voice_mode =
            normalized_voice_mode(&self.voice_mode).unwrap_or_else(default_voice_mode);
        self.surface_profiles = self
            .surface_profiles
            .into_iter()
            .filter_map(|(surface, value)| {
                normalized_provider_id(&value).map(|value| (surface, value))
            })
            .collect();
        self.surface_stage_options = self
            .surface_stage_options
            .into_iter()
            .filter_map(|(surface, options)| {
                let options = options
                    .into_iter()
                    .filter_map(|(stage, value)| {
                        normalized_provider_id(&value).map(|value| (stage, value))
                    })
                    .collect::<BTreeMap<_, _>>();
                (!options.is_empty()).then_some((surface, options))
            })
            .collect();
        self.realtime_voices = self
            .realtime_voices
            .into_iter()
            .filter_map(|(profile_id, voice)| {
                let profile_id = normalized_provider_id(&profile_id)?;
                let voice = normalized_provider_id(&voice)?;
                Some((profile_id, voice))
            })
            .collect();
        self
    }
}

fn current_schema_version() -> u32 {
    MEDIA_PREFERENCES_SCHEMA_VERSION
}

fn default_voice_mode() -> String {
    // Dictation by default. This is the canonical cross-surface default (Settings
    // + desktop tray mirror it); a profile with no saved preference must not
    // default to Live voice / an armed mic.
    "recording".to_string()
}

fn default_require_voice_prefix() -> bool {
    true
}

fn normalized_provider_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

fn normalized_voice_mode(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "realtime" | "live" | "call" => Some("realtime".to_string()),
        "hands_free" | "handsfree" => Some("hands_free".to_string()),
        "recording" | "dictate" | "dictation" => Some("recording".to_string()),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct PersistedMediaPreferences {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    auto_speak: bool,
    #[serde(default = "default_voice_mode")]
    voice_mode: String,
    #[serde(default = "default_require_voice_prefix")]
    require_voice_prefix: bool,
    #[serde(default)]
    surface_profiles: BTreeMap<AudioSurface, String>,
    #[serde(default)]
    surface_stage_options: BTreeMap<AudioSurface, BTreeMap<AudioStage, String>>,
    #[serde(default)]
    realtime_voices: BTreeMap<String, String>,
    #[serde(default)]
    recording_stt_provider: Option<String>,
    #[serde(default)]
    recording_tts_provider: Option<String>,
    #[serde(default)]
    observe_stt_mode: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LegacyMediaPreferenceHints {
    pub recording_stt_provider: Option<String>,
    pub recording_tts_provider: Option<String>,
    pub observe_stt_mode: Option<String>,
}

/// One lock per scope's preferences file, for the life of the process.
///
/// The PUT handler is a read-modify-write: load the whole record, apply only the
/// fields the request names, write the whole record back. `MediaPreferences` has
/// seven independently settable fields, so two requests changing *different* ones
/// — a voice-mode change and an auto-speak toggle — each load the same base and
/// each write their own version. The later write takes the record whole and the
/// other setting silently reverts.
///
/// Keyed by scope, held by the handler across load → mutate → save.
/// `tokio::sync::Mutex` because that span awaits. One process owns a data root
/// (decided 2026-08-12), so an in-process lock is the whole fix.
///
/// Note `ui_preferences.rs` deliberately has no equivalent: `UiPreferences` has
/// exactly one field, so last-writer-wins is the behaviour, not a bug.
pub fn media_preferences_lock(
    principal: &str,
    workspace: &str,
) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<(String, String), std::sync::Arc<tokio::sync::Mutex<()>>>,
        >,
    > = std::sync::OnceLock::new();
    let mut registry = LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::sync::Arc::clone(
        registry
            .entry((principal.to_string(), workspace.to_string()))
            .or_default(),
    )
}

#[derive(Debug, Clone)]
pub struct MediaPreferencesStore {
    workspace_layout: ArtifactV2Workspace,
}

impl MediaPreferencesStore {
    pub fn new<P: AsRef<Path>>(storage_root: P) -> Self {
        Self::with_workspace_layout(ArtifactV2Workspace::new(storage_root))
    }

    pub fn with_workspace_layout(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub async fn load(
        &self,
        principal: &str,
        workspace: &str,
        runtime: &AudioRuntimeConfigManager,
    ) -> std::io::Result<MediaPreferences> {
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
                return Ok(MediaPreferences::default());
            },
            Err(error) => return Err(artifact_v2_error_to_io(error)),
        };
        let document =
            serde_json::from_slice::<PersistedMediaPreferences>(&bytes).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid media preferences JSON: {error}"),
                )
            })?;
        if document.schema_version > MEDIA_PREFERENCES_SCHEMA_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "media preferences schema {} is newer than supported schema {}",
                    document.schema_version, MEDIA_PREFERENCES_SCHEMA_VERSION
                ),
            ));
        }

        let migration_required = document.schema_version != MEDIA_PREFERENCES_SCHEMA_VERSION
            || document.recording_stt_provider.is_some()
            || document.recording_tts_provider.is_some()
            || document.observe_stt_mode.is_some();
        let hints = LegacyMediaPreferenceHints {
            recording_stt_provider: document.recording_stt_provider,
            recording_tts_provider: document.recording_tts_provider,
            observe_stt_mode: document.observe_stt_mode,
        };
        let canonical = MediaPreferences {
            schema_version: MEDIA_PREFERENCES_SCHEMA_VERSION,
            auto_speak: document.auto_speak,
            voice_mode: document.voice_mode,
            require_voice_prefix: document.require_voice_prefix,
            surface_profiles: document.surface_profiles,
            surface_stage_options: document.surface_stage_options,
            realtime_voices: document.realtime_voices,
        }
        .normalize();
        let preferences = runtime.migrate_legacy_preferences(canonical.clone(), hints);
        if migration_required || preferences != canonical {
            return self.save(principal, workspace, preferences).await;
        }
        Ok(preferences)
    }

    pub async fn save(
        &self,
        principal: &str,
        workspace: &str,
        preferences: MediaPreferences,
    ) -> std::io::Result<MediaPreferences> {
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
                .join(MEDIA_PREFERENCES_DIR)
                .join(MEDIA_PREFERENCES_FILE),
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
    use std::sync::Arc;

    use super::*;
    use crate::config::MagicianMediaSettings;
    use crate::magician_v2::media_seam::{
        AudioProviderBindingConfig, AudioProviderCatalogConfig, AudioStageProfileConfig,
        AudioSurfaceProfileConfig, AudioSurfaceProfilesConfig, MediaProviderRegistry,
        TurnBoundaryAuthority,
    };

    fn runtime() -> AudioRuntimeConfigManager {
        let mut settings = MagicianMediaSettings::default();
        settings.streaming_stt = AudioProviderCatalogConfig {
            providers: vec![AudioProviderBindingConfig {
                id: "local-streaming".to_string(),
                label: None,
                engine_id: "macos_system".to_string(),
                adapter: "macos_speech_streaming".to_string(),
                model: "system_default".to_string(),
                variant: None,
                revision: None,
                sha256: None,
                idle_secs: None,
                language_codes: Vec::new(),
                capabilities: Default::default(),
                enabled: true,
            }],
        };
        let profile = AudioSurfaceProfileConfig {
            surface: AudioSurface::Meeting,
            turn_boundary: TurnBoundaryAuthority::SttEou,
            vad: AudioStageProfileConfig::default(),
            recording_stt: AudioStageProfileConfig::default(),
            streaming_stt: AudioStageProfileConfig::enabled_with(vec![
                "local-streaming".to_string()
            ]),
            diarization: AudioStageProfileConfig::default(),
            tts: AudioStageProfileConfig::default(),
        };
        settings.surface_profiles = AudioSurfaceProfilesConfig {
            default_mapping: [(AudioSurface::Meeting, "meeting-default".to_string())]
                .into_iter()
                .collect(),
            profiles: [("meeting-default".to_string(), profile)]
                .into_iter()
                .collect(),
        };
        AudioRuntimeConfigManager::in_memory(settings, Arc::new(MediaProviderRegistry::new()))
            .expect("runtime")
    }

    #[test]
    fn canonical_preferences_deserialize_with_empty_surface_overrides() {
        let preferences: MediaPreferences = serde_json::from_value(serde_json::json!({
            "schema_version": 3,
            "auto_speak": false,
            "voice_mode": "recording",
            "require_voice_prefix": true
        }))
        .expect("canonical preferences");

        assert_eq!(preferences.schema_version, MEDIA_PREFERENCES_SCHEMA_VERSION);
        assert_eq!(preferences.voice_mode, "recording");
        assert!(preferences.require_voice_prefix);
        assert!(preferences.surface_profiles.is_empty());
        assert!(preferences.surface_stage_options.is_empty());
    }

    #[test]
    fn hands_free_voice_mode_is_preserved() {
        let preferences = MediaPreferences {
            voice_mode: "handsfree".to_string(),
            ..MediaPreferences::default()
        }
        .normalize();
        assert_eq!(preferences.voice_mode, "hands_free");
    }

    #[test]
    fn surface_preferences_round_trip_and_normalize_values() {
        let preferences = MediaPreferences {
            surface_profiles: [(AudioSurface::Meeting, " meeting-local ".to_string())]
                .into_iter()
                .collect(),
            surface_stage_options: [(
                AudioSurface::Meeting,
                [(AudioStage::StreamingStt, " local-streaming ".to_string())]
                    .into_iter()
                    .collect(),
            )]
            .into_iter()
            .collect(),
            ..MediaPreferences::default()
        }
        .normalize();
        let bytes = serde_json::to_vec(&preferences).expect("serialize");
        let restored: MediaPreferences = serde_json::from_slice(&bytes).expect("deserialize");

        assert_eq!(
            restored.surface_profiles.get(&AudioSurface::Meeting),
            Some(&"meeting-local".to_string())
        );
        assert_eq!(
            restored.surface_stage_options[&AudioSurface::Meeting][&AudioStage::StreamingStt],
            "local-streaming"
        );
    }

    #[tokio::test]
    async fn legacy_preferences_are_migrated_and_rewritten_as_schema_v3() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = MediaPreferencesStore::new(temp.path());
        let path = store
            .preferences_path("anonymous", "default")
            .expect("preference path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "recording_stt_provider": "auto",
                "recording_tts_provider": "auto",
                "observe_stt_mode": "local",
                "auto_speak": true,
                "voice_mode": "handsfree"
            }))
            .expect("legacy json"),
        )
        .expect("write legacy preferences");

        let preferences = store
            .load("anonymous", "default", &runtime())
            .await
            .expect("migrated preferences");
        assert!(preferences.auto_speak);
        assert_eq!(preferences.voice_mode, "hands_free");
        assert!(preferences.require_voice_prefix);
        assert_eq!(
            preferences.surface_stage_options[&AudioSurface::Meeting][&AudioStage::StreamingStt],
            "local-streaming"
        );

        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("read canonical"))
                .expect("canonical json");
        assert_eq!(saved["schema_version"], MEDIA_PREFERENCES_SCHEMA_VERSION);
        assert!(saved.get("observe_stt_mode").is_none());
        assert!(saved.get("recording_stt_provider").is_none());
        assert!(saved.get("recording_tts_provider").is_none());
    }

    #[tokio::test]
    async fn corrupt_preferences_fail_closed_without_overwrite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = MediaPreferencesStore::new(temp.path());
        let path = store
            .preferences_path("anonymous", "default")
            .expect("preference path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"{not-json").expect("write corrupt preferences");

        let error = store
            .load("anonymous", "default", &runtime())
            .await
            .expect_err("corrupt document must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(path).expect("unchanged"), b"{not-json");
    }
}
