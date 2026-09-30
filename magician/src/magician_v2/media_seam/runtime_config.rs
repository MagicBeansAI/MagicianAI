use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::config::MagicianMediaSettings;
use crate::magician_v2::runtime_settings::write_top_level_yaml_block;

use super::audio_surface::{
    AudioEngineConfig, AudioEngineStartupPolicy, AudioModelDownloadPolicy, AudioProfileSource,
    AudioProviderBindingConfig, AudioStage, AudioStageCapabilities, AudioStageOption,
    AudioStageProfileConfig, AudioSurface, AudioSurfaceProfileConfig, ProviderAvailability,
    ResolvedAudioProfile, ResolvedAudioStage, TurnBoundaryAuthority, VadMode,
};
use super::preferences::{LegacyMediaPreferenceHints, MediaPreferences};
use super::providers::{
    MediaProviderRegistry, MACOS_SPEECH_DEFAULT_MODEL, MACOS_SPEECH_PROVIDER_ID,
    MACOS_TTS_DEFAULT_MODEL, MACOS_TTS_PROVIDER_ID,
};
pub use crate::magician_v2::media_seam::runtime_validation::validate_media_settings;

const MIGRATED_DICTATION_PROFILE_ID: &str = "migrated-dictation-v1";
const MIGRATED_MEETING_PROFILE_ID: &str = "migrated-meeting-v1";
const MIGRATED_LISTENING_PROFILE_ID: &str = "migrated-listening-v1";

#[derive(Debug, Clone, Serialize)]
pub struct AudioEngineStatus {
    pub engine_id: String,
    pub label: String,
    pub enabled: bool,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub healthy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_policy: Option<AudioEngineStartupPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_policy: Option<AudioModelDownloadPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offline: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_idle_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_idle_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_resident_models: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_streaming_sessions: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resident_models: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_sessions: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_started_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub can_manage_models: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioModelLifecycleState {
    DownloadRequired,
    Loading,
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioModelRuntimeStatus {
    pub option_id: String,
    pub stage: AudioStage,
    pub provider_id: String,
    pub engine_id: String,
    pub model_id: String,
    pub state: AudioModelLifecycleState,
    pub resident: bool,
    pub active_sessions: usize,
    pub can_load: bool,
    pub can_unload: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioSettingsResponse {
    pub revision: String,
    pub default_profiles: BTreeMap<AudioSurface, String>,
    pub profiles: BTreeMap<String, AudioSurfaceProfileConfig>,
    pub stages: BTreeMap<AudioStage, Vec<AudioStageOption>>,
    pub engines: BTreeMap<String, AudioEngineStatus>,
    pub models: BTreeMap<String, AudioModelRuntimeStatus>,
    pub requires_session_restart: bool,
}

#[derive(Debug, Clone)]
pub struct AudioRuntimeSnapshot {
    pub revision: String,
    pub source_settings: MagicianMediaSettings,
    pub default_profiles: BTreeMap<AudioSurface, String>,
    pub profiles: BTreeMap<String, AudioSurfaceProfileConfig>,
    pub catalog: BTreeMap<AudioStage, Vec<AudioStageOption>>,
    pub engines: BTreeMap<String, AudioEngineStatus>,
}

impl AudioRuntimeSnapshot {
    pub fn settings_response(&self) -> AudioSettingsResponse {
        AudioSettingsResponse {
            revision: self.revision.clone(),
            default_profiles: self.default_profiles.clone(),
            profiles: self.profiles.clone(),
            stages: self.catalog.clone(),
            engines: self.engines.clone(),
            models: build_model_runtime_statuses(&self.catalog),
            requires_session_restart: true,
        }
    }
}

fn build_model_runtime_statuses(
    catalog: &BTreeMap<AudioStage, Vec<AudioStageOption>>,
) -> BTreeMap<String, AudioModelRuntimeStatus> {
    catalog
        .iter()
        .flat_map(|(stage, options)| options.iter().map(move |option| (*stage, option)))
        .map(|(stage, option)| {
            let (state, message) = match option.availability {
                ProviderAvailability::Available if option.engine_id == "fluid_audio" => (
                    AudioModelLifecycleState::DownloadRequired,
                    Some("Downloads and loads on first use".to_string()),
                ),
                ProviderAvailability::Available => (AudioModelLifecycleState::Ready, None),
                ProviderAvailability::Unavailable | ProviderAvailability::Disabled => (
                    AudioModelLifecycleState::Unavailable,
                    option.unavailable_reason.clone(),
                ),
            };
            (
                option.option_id.clone(),
                AudioModelRuntimeStatus {
                    option_id: option.option_id.clone(),
                    stage,
                    provider_id: option.provider_id.clone(),
                    engine_id: option.engine_id.clone(),
                    model_id: option.model_id.clone(),
                    state,
                    resident: option.engine_id != "fluid_audio"
                        && option.availability == ProviderAvailability::Available,
                    active_sessions: 0,
                    can_load: option.engine_id == "fluid_audio"
                        && option.availability == ProviderAvailability::Available,
                    can_unload: false,
                    message,
                },
            )
        })
        .collect()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AudioStageSettingsPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub providers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AudioProfileSettingsPatch {
    #[serde(default)]
    pub turn_boundary: Option<TurnBoundaryAuthority>,
    #[serde(default)]
    pub stages: BTreeMap<AudioStage, AudioStageSettingsPatch>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AudioEngineSettingsPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AudioSettingsPatch {
    pub expected_revision: String,
    #[serde(default)]
    pub engines: BTreeMap<String, AudioEngineSettingsPatch>,
    #[serde(default)]
    pub default_profiles: BTreeMap<AudioSurface, String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, AudioProfileSettingsPatch>,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioConfigError {
    #[error("audio settings revision conflict: expected {expected}, current {current}")]
    RevisionConflict { expected: String, current: String },
    #[error("live media configuration changed outside the audio settings manager")]
    ExternalConfigDrift,
    #[error("invalid audio configuration: {0}")]
    Invalid(String),
    #[error("audio settings persistence failed: {0}")]
    Persistence(String),
    #[error("audio settings lock is unavailable")]
    LockPoisoned,
}

#[derive(Clone)]
pub struct AudioRuntimeConfigManager {
    current: Arc<RwLock<Arc<AudioRuntimeSnapshot>>>,
    mutation_lock: Arc<Mutex<()>>,
    providers: Arc<MediaProviderRegistry>,
    runtime_engine_availability: Arc<RwLock<BTreeMap<String, EngineRuntimeAvailability>>>,
    config_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct EngineRuntimeAvailability {
    available: bool,
    reason: Option<String>,
}

impl std::fmt::Debug for AudioRuntimeConfigManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AudioRuntimeConfigManager")
            .field("config_path", &self.config_path)
            .field("revision", &self.snapshot().revision)
            .finish()
    }
}

impl AudioRuntimeConfigManager {
    pub fn new(
        settings: MagicianMediaSettings,
        providers: Arc<MediaProviderRegistry>,
        config_path: PathBuf,
    ) -> Result<Self, AudioConfigError> {
        Self::build(settings, providers, Some(config_path))
    }

    pub fn in_memory(
        settings: MagicianMediaSettings,
        providers: Arc<MediaProviderRegistry>,
    ) -> Result<Self, AudioConfigError> {
        Self::build(settings, providers, None)
    }

    fn build(
        settings: MagicianMediaSettings,
        providers: Arc<MediaProviderRegistry>,
        config_path: Option<PathBuf>,
    ) -> Result<Self, AudioConfigError> {
        let (settings, migrated) = prepare_settings(settings, providers.as_ref());
        let snapshot = Arc::new(build_snapshot(settings, providers.as_ref())?);
        if migrated {
            if let Some(path) = config_path.as_deref() {
                persist_media_settings(path, &snapshot.source_settings)?;
            }
        }
        Ok(Self {
            current: Arc::new(RwLock::new(snapshot)),
            mutation_lock: Arc::new(Mutex::new(())),
            providers,
            runtime_engine_availability: Arc::new(RwLock::new(BTreeMap::new())),
            config_path,
        })
    }

    pub fn snapshot(&self) -> Arc<AudioRuntimeSnapshot> {
        self.current
            .read()
            .map(|snapshot| Arc::clone(&snapshot))
            .unwrap_or_else(|poisoned| Arc::clone(&poisoned.into_inner()))
    }

    pub fn settings_response(&self) -> AudioSettingsResponse {
        self.snapshot().settings_response()
    }

    /// Apply host/process availability without persisting it into
    /// magician-config.yaml. This keeps configured adapters registered across
    /// an off/on transition while preventing resolution from selecting an
    /// engine whose local binary or external sidecar is unavailable.
    pub fn set_engine_runtime_availability(
        &self,
        engine_id: impl Into<String>,
        available: bool,
        reason: Option<String>,
    ) -> Result<Arc<AudioRuntimeSnapshot>, AudioConfigError> {
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| AudioConfigError::LockPoisoned)?;
        let engine_id = engine_id.into();
        let current = self.snapshot();
        if !current.source_settings.engines.contains_key(&engine_id) {
            return Err(AudioConfigError::Invalid(format!(
                "unknown audio engine {engine_id}"
            )));
        }
        let mut overrides = self
            .runtime_engine_availability
            .read()
            .map_err(|_| AudioConfigError::LockPoisoned)?
            .clone();
        overrides.insert(engine_id, EngineRuntimeAvailability { available, reason });
        let mut next = build_snapshot(current.source_settings.clone(), self.providers.as_ref())?;
        apply_runtime_engine_availability(&mut next, &overrides);
        let next = Arc::new(next);
        *self
            .runtime_engine_availability
            .write()
            .map_err(|_| AudioConfigError::LockPoisoned)? = overrides;
        *self
            .current
            .write()
            .map_err(|_| AudioConfigError::LockPoisoned)? = Arc::clone(&next);
        Ok(next)
    }

    pub fn resolve(
        &self,
        surface: AudioSurface,
        preferences: &MediaPreferences,
        explicit_profile: Option<&str>,
        explicit_stage_options: &BTreeMap<AudioStage, String>,
    ) -> Result<ResolvedAudioProfile, AudioConfigError> {
        resolve_snapshot(
            self.snapshot().as_ref(),
            surface,
            preferences,
            explicit_profile,
            explicit_stage_options,
        )
    }

    pub fn validate_preferences(
        &self,
        preferences: &MediaPreferences,
    ) -> Result<(), AudioConfigError> {
        let snapshot = self.snapshot();
        for (surface, profile_id) in &preferences.surface_profiles {
            let Some(profile_id) = normalized_override(Some(profile_id)) else {
                continue;
            };
            let profile = snapshot.profiles.get(profile_id).ok_or_else(|| {
                AudioConfigError::Invalid(format!(
                    "surface {surface} references unknown profile {profile_id}"
                ))
            })?;
            if profile.surface != *surface {
                return Err(AudioConfigError::Invalid(format!(
                    "profile {profile_id} belongs to {}, not {surface}",
                    profile.surface
                )));
            }
        }
        for (surface, stage_options) in &preferences.surface_stage_options {
            let profile_id = preferences
                .surface_profiles
                .get(surface)
                .and_then(|value| normalized_override(Some(value)))
                .or_else(|| snapshot.default_profiles.get(surface).map(String::as_str))
                .ok_or_else(|| {
                    AudioConfigError::Invalid(format!("no default profile for {surface}"))
                })?;
            let profile = snapshot.profiles.get(profile_id).ok_or_else(|| {
                AudioConfigError::Invalid(format!("unknown audio profile {profile_id}"))
            })?;
            for (stage, option_id) in stage_options {
                validate_stage_override(snapshot.as_ref(), profile, *stage, option_id)?;
            }
        }
        Ok(())
    }

    pub fn sanitize_preferences(&self, mut preferences: MediaPreferences) -> MediaPreferences {
        let snapshot = self.snapshot();
        preferences.surface_profiles.retain(|surface, profile_id| {
            normalized_override(Some(profile_id)).is_some_and(|profile_id| {
                validate_profile_surface(snapshot.as_ref(), *surface, profile_id).is_ok()
            })
        });

        let selected_profiles = preferences.surface_profiles.clone();
        preferences
            .surface_stage_options
            .retain(|surface, options| {
                let profile_id = selected_profiles
                    .get(surface)
                    .and_then(|value| normalized_override(Some(value)))
                    .or_else(|| snapshot.default_profiles.get(surface).map(String::as_str));
                let Some(profile) = profile_id.and_then(|id| snapshot.profiles.get(id)) else {
                    return false;
                };
                options.retain(|stage, option_id| {
                    validate_stage_override(snapshot.as_ref(), profile, *stage, option_id).is_ok()
                });
                !options.is_empty()
            });
        preferences
    }

    pub fn migrate_legacy_preferences(
        &self,
        mut preferences: MediaPreferences,
        hints: LegacyMediaPreferenceHints,
    ) -> MediaPreferences {
        let snapshot = self.snapshot();
        migrate_legacy_stage_provider(
            snapshot.as_ref(),
            &mut preferences,
            AudioSurface::Dictation,
            AudioStage::RecordingStt,
            hints.recording_stt_provider.as_deref(),
        );
        migrate_legacy_stage_provider(
            snapshot.as_ref(),
            &mut preferences,
            AudioSurface::Dictation,
            AudioStage::Tts,
            hints.recording_tts_provider.as_deref(),
        );
        let observe_engine = match hints.observe_stt_mode.as_deref().map(str::trim) {
            Some("local") => Some("macos_system"),
            Some("cloud") => Some("online"),
            _ => None,
        };
        if let Some(engine_id) = observe_engine {
            for surface in [AudioSurface::Meeting, AudioSurface::Listening] {
                migrate_legacy_stage_engine(
                    snapshot.as_ref(),
                    &mut preferences,
                    surface,
                    AudioStage::StreamingStt,
                    engine_id,
                );
            }
        }
        self.sanitize_preferences(preferences.normalize())
    }

    pub fn update(
        &self,
        patch: AudioSettingsPatch,
    ) -> Result<Arc<AudioRuntimeSnapshot>, AudioConfigError> {
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| AudioConfigError::LockPoisoned)?;
        let current = self.snapshot();
        if patch.expected_revision != current.revision {
            return Err(AudioConfigError::RevisionConflict {
                expected: patch.expected_revision,
                current: current.revision.clone(),
            });
        }
        if let Some(path) = self.config_path.as_deref() {
            ensure_live_media_revision(path, &current.revision)?;
        }

        let mut candidate = current.source_settings.clone();
        apply_settings_patch(&mut candidate, patch)?;
        let mut next = build_snapshot(candidate, self.providers.as_ref())?;
        let runtime_availability = self
            .runtime_engine_availability
            .read()
            .map_err(|_| AudioConfigError::LockPoisoned)?;
        apply_runtime_engine_availability(&mut next, &runtime_availability);
        let next = Arc::new(next);

        let mut target = self
            .current
            .write()
            .map_err(|_| AudioConfigError::LockPoisoned)?;
        if let Some(path) = self.config_path.as_deref() {
            persist_media_settings(path, &next.source_settings)?;
        }
        *target = Arc::clone(&next);
        Ok(next)
    }
}

fn apply_runtime_engine_availability(
    snapshot: &mut AudioRuntimeSnapshot,
    availability: &BTreeMap<String, EngineRuntimeAvailability>,
) {
    for (engine_id, runtime) in availability {
        let configured_enabled = snapshot
            .source_settings
            .engines
            .get(engine_id)
            .is_some_and(|engine| engine.enabled);
        if let Some(engine) = snapshot.engines.get_mut(engine_id) {
            engine.available = configured_enabled && runtime.available;
        }
        if runtime.available || !configured_enabled {
            continue;
        }
        for option in snapshot.catalog.values_mut().flatten() {
            if option.engine_id == *engine_id
                && option.availability == ProviderAvailability::Available
            {
                option.availability = ProviderAvailability::Unavailable;
                option.unavailable_reason = runtime.reason.clone().or_else(|| {
                    Some(format!(
                        "audio engine {engine_id} is unavailable on this host"
                    ))
                });
            }
        }
    }
}

pub(crate) fn build_snapshot(
    settings: MagicianMediaSettings,
    providers: &MediaProviderRegistry,
) -> Result<AudioRuntimeSnapshot, AudioConfigError> {
    validate_provider_catalogs(&settings)?;
    let catalog = build_catalog(&settings, providers);
    let (profiles, default_profiles) = build_profiles(&settings)?;
    validate_profiles(&profiles, &default_profiles, &catalog)?;
    let engines = build_engine_statuses(&settings, &catalog);
    Ok(AudioRuntimeSnapshot {
        revision: media_revision(&settings)?,
        source_settings: settings,
        default_profiles,
        profiles,
        catalog,
        engines,
    })
}

pub(crate) fn prepare_settings(
    mut settings: MagicianMediaSettings,
    providers: &MediaProviderRegistry,
) -> (MagicianMediaSettings, bool) {
    let catalog = build_catalog(&settings, providers);
    let mut migrated = false;
    for (surface, profile_id, profile) in generated_migration_profiles(&catalog) {
        if settings
            .surface_profiles
            .default_mapping
            .contains_key(&surface)
        {
            continue;
        }
        settings
            .surface_profiles
            .profiles
            .entry(profile_id.clone())
            .or_insert(profile);
        settings
            .surface_profiles
            .default_mapping
            .insert(surface, profile_id);
        migrated = true;
    }
    (settings, migrated)
}

fn build_catalog(
    settings: &MagicianMediaSettings,
    providers: &MediaProviderRegistry,
) -> BTreeMap<AudioStage, Vec<AudioStageOption>> {
    let mut catalog = AudioStage::ALL
        .into_iter()
        .map(|stage| (stage, Vec::new()))
        .collect::<BTreeMap<_, _>>();

    for provider in providers.stt_chain() {
        let configured = settings
            .recording_stt
            .providers
            .iter()
            .find(|item| item.id.eq_ignore_ascii_case(provider.id()));
        push_option(
            &mut catalog,
            AudioStageOption {
                option_id: provider.id().to_string(),
                stage: AudioStage::RecordingStt,
                provider_id: provider.id().to_string(),
                engine_id: configured
                    .map(|item| engine_for_adapter(&item.adapter))
                    .unwrap_or_else(|| engine_for_provider(provider.id())),
                model_id: provider.default_model().to_string(),
                variant: None,
                label: provider
                    .label()
                    .map(str::to_string)
                    .unwrap_or_else(|| provider.id().to_string()),
                capabilities: AudioStageCapabilities {
                    recording: true,
                    languages: configured
                        .map(|item| item.language_codes.clone())
                        .unwrap_or_default(),
                    ..AudioStageCapabilities::default()
                },
                availability: ProviderAvailability::Available,
                unavailable_reason: None,
            },
        );
    }
    add_recording_config_options(&mut catalog, settings, providers);
    add_system_recording_option(&mut catalog, providers);

    for provider in providers.streaming_stt_chain() {
        let Some(configured) = settings
            .streaming_stt
            .providers
            .iter()
            .find(|item| item.id.eq_ignore_ascii_case(provider.id()))
        else {
            continue;
        };
        let capabilities = provider.capabilities();
        let mut features = configured.capabilities.clone();
        push_option(
            &mut catalog,
            AudioStageOption {
                option_id: configured.id.clone(),
                stage: AudioStage::StreamingStt,
                provider_id: configured.id.clone(),
                engine_id: configured.engine_id.clone(),
                model_id: configured.model.clone(),
                variant: configured.variant.clone(),
                label: configured
                    .label
                    .clone()
                    .or_else(|| provider.label().map(str::to_string))
                    .unwrap_or_else(|| configured.id.clone()),
                capabilities: AudioStageCapabilities {
                    streaming: true,
                    word_timestamps: capabilities.word_timestamps
                        || features.remove("word_timestamps"),
                    end_of_utterance: capabilities.end_of_utterance
                        || features.remove("end_of_utterance"),
                    speaker_attribution: capabilities.speaker_attribution
                        || features.remove("speaker_attribution"),
                    languages: configured.language_codes.clone(),
                    features,
                    ..Default::default()
                },
                availability: configured_availability(
                    binding_enabled(configured, &settings.engines),
                    true,
                ),
                unavailable_reason: unavailable_reason(
                    binding_enabled(configured, &settings.engines),
                    true,
                ),
            },
        );
    }
    add_binding_options(
        &mut catalog,
        AudioStage::StreamingStt,
        &settings.streaming_stt.providers,
        &settings.engines,
    );

    for provider in providers.vad_chain() {
        let Some(configured) = settings
            .vad
            .providers
            .iter()
            .find(|item| item.id.eq_ignore_ascii_case(provider.id()))
        else {
            continue;
        };
        let capabilities = provider.capabilities();
        let mut features = configured.capabilities.clone();
        if capabilities.probability_events {
            features.insert("probability_events".to_string());
        }
        if capabilities.configurable_threshold {
            features.insert("configurable_threshold".to_string());
        }
        push_option(
            &mut catalog,
            AudioStageOption {
                option_id: configured.id.clone(),
                stage: AudioStage::Vad,
                provider_id: configured.id.clone(),
                engine_id: configured.engine_id.clone(),
                model_id: configured.model.clone(),
                variant: configured.variant.clone(),
                label: configured
                    .label
                    .clone()
                    .or_else(|| provider.label().map(str::to_string))
                    .unwrap_or_else(|| configured.id.clone()),
                capabilities: AudioStageCapabilities {
                    languages: configured.language_codes.clone(),
                    features,
                    ..Default::default()
                },
                availability: configured_availability(
                    binding_enabled(configured, &settings.engines),
                    true,
                ),
                unavailable_reason: unavailable_reason(
                    binding_enabled(configured, &settings.engines),
                    true,
                ),
            },
        );
    }
    add_binding_options(
        &mut catalog,
        AudioStage::Vad,
        &settings.vad.providers,
        &settings.engines,
    );

    for provider in providers.diarization_chain() {
        let Some(configured) = settings
            .diarization
            .providers
            .iter()
            .find(|item| item.id.eq_ignore_ascii_case(provider.id()))
        else {
            continue;
        };
        let capabilities = provider.capabilities();
        let mut features = configured.capabilities.clone();
        if capabilities.online_revisions {
            features.insert("online_revisions".to_string());
        }
        push_option(
            &mut catalog,
            AudioStageOption {
                option_id: configured.id.clone(),
                stage: AudioStage::Diarization,
                provider_id: configured.id.clone(),
                engine_id: configured.engine_id.clone(),
                model_id: configured.model.clone(),
                variant: configured.variant.clone(),
                label: configured
                    .label
                    .clone()
                    .or_else(|| provider.label().map(str::to_string))
                    .unwrap_or_else(|| configured.id.clone()),
                capabilities: AudioStageCapabilities {
                    speaker_attribution: true,
                    languages: configured.language_codes.clone(),
                    features,
                    ..Default::default()
                },
                availability: configured_availability(
                    binding_enabled(configured, &settings.engines),
                    true,
                ),
                unavailable_reason: unavailable_reason(
                    binding_enabled(configured, &settings.engines),
                    true,
                ),
            },
        );
    }
    add_binding_options(
        &mut catalog,
        AudioStage::Diarization,
        &settings.diarization.providers,
        &settings.engines,
    );

    for provider in providers.tts_chain() {
        let configured = settings
            .tts
            .providers
            .iter()
            .find(|item| item.id.eq_ignore_ascii_case(provider.id()));
        let mut features = configured
            .map(|item| item.capabilities.clone())
            .unwrap_or_default();
        push_option(
            &mut catalog,
            AudioStageOption {
                option_id: provider.id().to_string(),
                stage: AudioStage::Tts,
                provider_id: provider.id().to_string(),
                engine_id: configured
                    .map(|item| engine_for_adapter(&item.adapter))
                    .unwrap_or_else(|| engine_for_provider(provider.id())),
                model_id: provider.default_model().to_string(),
                variant: provider.default_voice().map(str::to_string),
                label: provider
                    .label()
                    .map(str::to_string)
                    .unwrap_or_else(|| provider.id().to_string()),
                capabilities: AudioStageCapabilities {
                    streaming: provider.supports_streaming(),
                    voice_cloning: features.remove("voice_cloning"),
                    voices: configured
                        .filter(|item| !item.voices.is_empty())
                        .map(|item| item.voices.clone())
                        .unwrap_or_else(|| provider.supported_voices()),
                    formats: configured
                        .filter(|item| !item.formats.is_empty())
                        .map(|item| item.formats.clone())
                        .unwrap_or_else(|| provider.supported_formats()),
                    languages: configured
                        .map(|item| item.language_codes.clone())
                        .unwrap_or_default(),
                    features,
                    ..AudioStageCapabilities::default()
                },
                availability: ProviderAvailability::Available,
                unavailable_reason: None,
            },
        );
    }
    add_tts_config_options(&mut catalog, settings, providers);
    add_system_tts_option(&mut catalog, providers);
    apply_engine_availability(&mut catalog, &settings.engines);
    catalog
}

fn apply_engine_availability(
    catalog: &mut BTreeMap<AudioStage, Vec<AudioStageOption>>,
    engines: &BTreeMap<String, AudioEngineConfig>,
) {
    for option in catalog.values_mut().flatten() {
        if engines
            .get(&option.engine_id)
            .is_some_and(|engine| !engine.enabled)
        {
            option.availability = ProviderAvailability::Disabled;
            option.unavailable_reason =
                Some(format!("audio engine {} is disabled", option.engine_id));
        }
    }
}

fn add_recording_config_options(
    catalog: &mut BTreeMap<AudioStage, Vec<AudioStageOption>>,
    settings: &MagicianMediaSettings,
    providers: &MediaProviderRegistry,
) {
    let available = providers
        .stt_chain()
        .into_iter()
        .map(|provider| provider.id().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    for provider in &settings.recording_stt.providers {
        let is_available = available.contains(&provider.id.to_ascii_lowercase());
        push_option(
            catalog,
            AudioStageOption {
                option_id: provider.id.clone(),
                stage: AudioStage::RecordingStt,
                provider_id: provider.id.clone(),
                engine_id: provider
                    .engine_id
                    .clone()
                    .unwrap_or_else(|| engine_for_adapter(&provider.adapter)),
                model_id: provider.model.clone(),
                variant: provider.variant.clone(),
                label: provider
                    .label
                    .clone()
                    .unwrap_or_else(|| provider.id.clone()),
                capabilities: AudioStageCapabilities {
                    recording: true,
                    languages: provider.language_codes.clone(),
                    ..AudioStageCapabilities::default()
                },
                availability: configured_availability(provider.enabled, is_available),
                unavailable_reason: unavailable_reason(provider.enabled, is_available),
            },
        );
    }
}

fn add_tts_config_options(
    catalog: &mut BTreeMap<AudioStage, Vec<AudioStageOption>>,
    settings: &MagicianMediaSettings,
    providers: &MediaProviderRegistry,
) {
    let available = providers
        .tts_chain()
        .into_iter()
        .map(|provider| provider.id().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    for provider in &settings.tts.providers {
        let is_available = available.contains(&provider.id.to_ascii_lowercase());
        let mut features = provider.capabilities.clone();
        push_option(
            catalog,
            AudioStageOption {
                option_id: provider.id.clone(),
                stage: AudioStage::Tts,
                provider_id: provider.id.clone(),
                engine_id: engine_for_adapter(&provider.adapter),
                model_id: provider.model.clone(),
                variant: provider.voice.clone(),
                label: provider
                    .label
                    .clone()
                    .unwrap_or_else(|| provider.id.clone()),
                capabilities: AudioStageCapabilities {
                    streaming: features.remove("streaming"),
                    voice_cloning: features.remove("voice_cloning"),
                    voices: provider.voices.clone(),
                    formats: provider.formats.clone(),
                    languages: provider.language_codes.clone(),
                    features,
                    ..AudioStageCapabilities::default()
                },
                availability: configured_availability(provider.enabled, is_available),
                unavailable_reason: unavailable_reason(provider.enabled, is_available),
            },
        );
    }
}

fn add_binding_options(
    catalog: &mut BTreeMap<AudioStage, Vec<AudioStageOption>>,
    stage: AudioStage,
    bindings: &[AudioProviderBindingConfig],
    engines: &BTreeMap<String, AudioEngineConfig>,
) {
    for binding in bindings {
        let already_available = catalog
            .get(&stage)
            .and_then(|options| {
                options.iter().find(|option| {
                    option.provider_id.eq_ignore_ascii_case(binding.id.as_str())
                        && option.availability == ProviderAvailability::Available
                })
            })
            .is_some();
        let mut features = binding.capabilities.clone();
        let capabilities = AudioStageCapabilities {
            streaming: stage == AudioStage::StreamingStt,
            recording: stage == AudioStage::RecordingStt,
            end_of_utterance: features.remove("end_of_utterance"),
            word_timestamps: features.remove("word_timestamps"),
            speaker_attribution: features.remove("speaker_attribution"),
            voice_cloning: features.remove("voice_cloning"),
            voices: Vec::new(),
            formats: Vec::new(),
            languages: binding.language_codes.clone(),
            features,
        };
        push_option(
            catalog,
            AudioStageOption {
                option_id: binding.id.clone(),
                stage,
                provider_id: binding.id.clone(),
                engine_id: binding.engine_id.clone(),
                model_id: binding.model.clone(),
                variant: binding.variant.clone(),
                label: binding.label.clone().unwrap_or_else(|| binding.id.clone()),
                capabilities,
                availability: configured_availability(
                    binding_enabled(binding, engines),
                    already_available,
                ),
                unavailable_reason: unavailable_reason(
                    binding_enabled(binding, engines),
                    already_available,
                ),
            },
        );
    }
}

fn binding_enabled(
    binding: &AudioProviderBindingConfig,
    engines: &BTreeMap<String, AudioEngineConfig>,
) -> bool {
    binding.enabled
        && engines
            .get(&binding.engine_id)
            .map(|engine| engine.enabled)
            .unwrap_or(true)
}

fn add_system_recording_option(
    catalog: &mut BTreeMap<AudioStage, Vec<AudioStageOption>>,
    providers: &MediaProviderRegistry,
) {
    let available = providers
        .stt_chain()
        .iter()
        .any(|provider| provider.id() == MACOS_SPEECH_PROVIDER_ID);
    push_option(
        catalog,
        AudioStageOption {
            option_id: MACOS_SPEECH_PROVIDER_ID.to_string(),
            stage: AudioStage::RecordingStt,
            provider_id: MACOS_SPEECH_PROVIDER_ID.to_string(),
            engine_id: "macos_system".to_string(),
            model_id: MACOS_SPEECH_DEFAULT_MODEL.to_string(),
            variant: None,
            label: "macOS Speech".to_string(),
            capabilities: AudioStageCapabilities {
                recording: true,
                ..AudioStageCapabilities::default()
            },
            availability: configured_availability(true, available),
            unavailable_reason: unavailable_reason(true, available),
        },
    );
}

fn add_system_tts_option(
    catalog: &mut BTreeMap<AudioStage, Vec<AudioStageOption>>,
    providers: &MediaProviderRegistry,
) {
    let system_provider = providers
        .tts_chain()
        .iter()
        .find(|provider| provider.id() == MACOS_TTS_PROVIDER_ID)
        .cloned();
    let available = system_provider.is_some();
    push_option(
        catalog,
        AudioStageOption {
            option_id: MACOS_TTS_PROVIDER_ID.to_string(),
            stage: AudioStage::Tts,
            provider_id: MACOS_TTS_PROVIDER_ID.to_string(),
            engine_id: "macos_system".to_string(),
            model_id: MACOS_TTS_DEFAULT_MODEL.to_string(),
            variant: None,
            label: "macOS system voice".to_string(),
            capabilities: AudioStageCapabilities {
                voices: system_provider
                    .as_ref()
                    .map(|provider| provider.supported_voices())
                    .unwrap_or_default(),
                formats: system_provider
                    .as_ref()
                    .map(|provider| provider.supported_formats())
                    .unwrap_or_else(|| vec!["wav".to_string()]),
                ..AudioStageCapabilities::default()
            },
            availability: configured_availability(true, available),
            unavailable_reason: unavailable_reason(true, available),
        },
    );
}

fn push_option(
    catalog: &mut BTreeMap<AudioStage, Vec<AudioStageOption>>,
    option: AudioStageOption,
) {
    let options = catalog.entry(option.stage).or_default();
    if let Some(existing) = options.iter_mut().find(|existing| {
        existing
            .provider_id
            .eq_ignore_ascii_case(option.provider_id.as_str())
    }) {
        if option.availability == ProviderAvailability::Available
            || existing.availability != ProviderAvailability::Available
        {
            *existing = option;
        }
    } else {
        options.push(option);
    }
}

fn configured_availability(enabled: bool, available: bool) -> ProviderAvailability {
    if !enabled {
        ProviderAvailability::Disabled
    } else if available {
        ProviderAvailability::Available
    } else {
        ProviderAvailability::Unavailable
    }
}

fn unavailable_reason(enabled: bool, available: bool) -> Option<String> {
    if !enabled {
        Some("disabled by configuration".to_string())
    } else if !available {
        Some("provider could not be instantiated on this host".to_string())
    } else {
        None
    }
}

fn build_profiles(
    settings: &MagicianMediaSettings,
) -> Result<
    (
        BTreeMap<String, AudioSurfaceProfileConfig>,
        BTreeMap<AudioSurface, String>,
    ),
    AudioConfigError,
> {
    let profiles = settings.surface_profiles.profiles.clone();
    let defaults = settings.surface_profiles.default_mapping.clone();
    if profiles.is_empty() {
        return Err(AudioConfigError::Invalid(
            "no audio surface profiles are configured".to_string(),
        ));
    }
    Ok((profiles, defaults))
}

fn generated_migration_profiles(
    catalog: &BTreeMap<AudioStage, Vec<AudioStageOption>>,
) -> Vec<(AudioSurface, String, AudioSurfaceProfileConfig)> {
    let recording = provider_ids(catalog, AudioStage::RecordingStt, |_| true);
    let tts = provider_ids(catalog, AudioStage::Tts, |_| true);
    let streaming = provider_ids(catalog, AudioStage::StreamingStt, |_| true);

    vec![
        (
            AudioSurface::Dictation,
            MIGRATED_DICTATION_PROFILE_ID.to_string(),
            AudioSurfaceProfileConfig {
                surface: AudioSurface::Dictation,
                turn_boundary: TurnBoundaryAuthority::PushToTalk,
                vad: AudioStageProfileConfig::default(),
                recording_stt: AudioStageProfileConfig::enabled_with(recording),
                streaming_stt: AudioStageProfileConfig::default(),
                diarization: AudioStageProfileConfig::default(),
                tts: AudioStageProfileConfig::enabled_with(tts),
            },
        ),
        (
            AudioSurface::Meeting,
            MIGRATED_MEETING_PROFILE_ID.to_string(),
            AudioSurfaceProfileConfig {
                surface: AudioSurface::Meeting,
                turn_boundary: TurnBoundaryAuthority::SttEou,
                vad: AudioStageProfileConfig::default(),
                recording_stt: AudioStageProfileConfig::default(),
                streaming_stt: AudioStageProfileConfig::enabled_with(streaming.clone()),
                diarization: AudioStageProfileConfig::default(),
                tts: AudioStageProfileConfig::default(),
            },
        ),
        (
            AudioSurface::Listening,
            MIGRATED_LISTENING_PROFILE_ID.to_string(),
            AudioSurfaceProfileConfig {
                surface: AudioSurface::Listening,
                turn_boundary: TurnBoundaryAuthority::SttEou,
                vad: AudioStageProfileConfig::default(),
                recording_stt: AudioStageProfileConfig::default(),
                streaming_stt: AudioStageProfileConfig::enabled_with(streaming),
                diarization: AudioStageProfileConfig::default(),
                tts: AudioStageProfileConfig::default(),
            },
        ),
    ]
}

fn provider_ids<F>(
    catalog: &BTreeMap<AudioStage, Vec<AudioStageOption>>,
    stage: AudioStage,
    predicate: F,
) -> Vec<String>
where
    F: Fn(&AudioStageOption) -> bool,
{
    catalog
        .get(&stage)
        .into_iter()
        .flatten()
        .filter(|option| option.availability != ProviderAvailability::Disabled)
        .filter(|option| predicate(option))
        .map(|option| option.provider_id.clone())
        .collect()
}

fn validate_provider_catalogs(settings: &MagicianMediaSettings) -> Result<(), AudioConfigError> {
    validate_binding_list("vad", &settings.vad.providers, &settings.engines)?;
    validate_binding_list(
        "streaming_stt",
        &settings.streaming_stt.providers,
        &settings.engines,
    )?;
    validate_binding_list(
        "diarization",
        &settings.diarization.providers,
        &settings.engines,
    )?;
    validate_provider_ids(
        "recording_stt",
        settings
            .recording_stt
            .providers
            .iter()
            .map(|provider| provider.id.as_str()),
    )?;
    validate_provider_ids(
        "tts",
        settings
            .tts
            .providers
            .iter()
            .map(|provider| provider.id.as_str()),
    )?;
    validate_tts_bindings(settings)?;
    validate_engine_configs(settings)?;
    Ok(())
}

fn validate_tts_bindings(settings: &MagicianMediaSettings) -> Result<(), AudioConfigError> {
    for provider in &settings.tts.providers {
        for (field, values) in [("voices", &provider.voices), ("formats", &provider.formats)] {
            let mut seen = HashSet::new();
            if values.iter().any(|value| {
                let normalized = value.trim().to_ascii_lowercase();
                normalized.is_empty() || !seen.insert(normalized)
            }) {
                return Err(AudioConfigError::Invalid(format!(
                    "media.tts.providers.{} has empty or duplicate {field}",
                    provider.id
                )));
            }
        }
        if let Some(voice) = provider.voice.as_deref() {
            if !provider.voices.is_empty()
                && !provider.voices.iter().any(|candidate| candidate == voice)
            {
                return Err(AudioConfigError::Invalid(format!(
                    "media.tts.providers.{} default voice is absent from voices",
                    provider.id
                )));
            }
        }
        if let Some(format) = provider.format.as_deref() {
            if !provider.formats.is_empty()
                && !provider
                    .formats
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(format))
            {
                return Err(AudioConfigError::Invalid(format!(
                    "media.tts.providers.{} default format is absent from formats",
                    provider.id
                )));
            }
        }
        if provider.adapter == crate::magician_v2::media_seam::FLUID_AUDIO_TTS_ADAPTER {
            if provider.model != "FluidInference/kokoro-82m-coreml"
                || provider
                    .variant
                    .as_deref()
                    .is_some_and(|variant| !matches!(variant, "5s" | "15s"))
                || provider
                    .revision
                    .as_deref()
                    .is_some_and(|revision| revision != "main")
                || provider.sha256.is_some()
                || provider.voice.as_deref().is_none_or(str::is_empty)
                || provider.formats != ["wav"]
            {
                return Err(AudioConfigError::Invalid(format!(
                    "media.tts.providers.{} is not a supported FluidAudio Kokoro binding",
                    provider.id
                )));
            }
        }
    }
    Ok(())
}

fn validate_engine_configs(settings: &MagicianMediaSettings) -> Result<(), AudioConfigError> {
    for provider in &settings.recording_stt.providers {
        if let Some(engine_id) = provider.engine_id.as_deref() {
            if engine_id.trim().is_empty() {
                return Err(AudioConfigError::Invalid(format!(
                    "media.recording_stt.providers.{} has an empty engine_id",
                    provider.id
                )));
            }
            if !settings.engines.is_empty() && !settings.engines.contains_key(engine_id) {
                return Err(AudioConfigError::Invalid(format!(
                    "media.recording_stt.providers.{} references unknown engine {engine_id}",
                    provider.id
                )));
            }
        }
    }

    let Some(engine) = settings.engines.get("fluid_audio") else {
        return Ok(());
    };
    if engine.max_resident_models == 0
        || engine.max_streaming_sessions == 0
        || engine.max_request_bytes == 0
        || engine.max_frame_bytes == 0
        || engine.request_timeout_secs == 0
        || engine.health_timeout_secs == 0
    {
        return Err(AudioConfigError::Invalid(
            "media.engines.fluid_audio resource limits and timeouts must be positive".to_string(),
        ));
    }
    if let Some(version) = engine.minimum_macos_version.as_deref() {
        let parts = version.split('.').collect::<Vec<_>>();
        if parts.is_empty()
            || parts.len() > 3
            || parts
                .iter()
                .any(|part| part.is_empty() || part.parse::<u64>().is_err())
        {
            return Err(AudioConfigError::Invalid(
                "media.engines.fluid_audio.minimum_macos_version must be a numeric dotted version"
                    .to_string(),
            ));
        }
    }
    if let Some(endpoint) = engine.endpoint.as_deref() {
        let parsed = reqwest::Url::parse(endpoint).map_err(|error| {
            AudioConfigError::Invalid(format!(
                "media.engines.fluid_audio.endpoint is invalid: {error}"
            ))
        })?;
        let loopback = parsed
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.is_loopback())
            .unwrap_or_else(|| parsed.host_str() == Some("localhost"));
        if parsed.scheme() != "http"
            || !loopback
            || parsed.port().is_none()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(AudioConfigError::Invalid(
                "media.engines.fluid_audio.endpoint must be an explicit loopback HTTP port"
                    .to_string(),
            ));
        }
    }
    if engine.startup == super::AudioEngineStartupPolicy::External
        && engine
            .auth_token_env
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err(AudioConfigError::Invalid(
            "media.engines.fluid_audio.auth_token_env is required for external startup".to_string(),
        ));
    }
    let fluid_stage_ids = settings
        .vad
        .providers
        .iter()
        .chain(settings.streaming_stt.providers.iter())
        .chain(settings.diarization.providers.iter())
        .filter(|binding| binding.engine_id == "fluid_audio")
        .map(|binding| binding.id.as_str());
    let fluid_recording_ids = settings
        .recording_stt
        .providers
        .iter()
        .filter(|binding| {
            binding.engine_id.as_deref() == Some("fluid_audio")
                || binding
                    .adapter
                    .eq_ignore_ascii_case("fluid_audio_recording_stt")
        })
        .map(|binding| binding.id.as_str());
    let fluid_tts_ids = settings
        .tts
        .providers
        .iter()
        .filter(|binding| {
            binding.adapter == crate::magician_v2::media_seam::FLUID_AUDIO_TTS_ADAPTER
        })
        .map(|binding| binding.id.as_str());
    let mut known = HashSet::new();
    for id in fluid_stage_ids
        .chain(fluid_recording_ids)
        .chain(fluid_tts_ids)
    {
        if !known.insert(id.to_ascii_lowercase()) {
            return Err(AudioConfigError::Invalid(format!(
                "FluidAudio model id {id} is duplicated across stage catalogs"
            )));
        }
    }
    if let Some(unknown) = engine
        .prewarm
        .iter()
        .find(|model_id| !known.contains(&model_id.to_ascii_lowercase()))
    {
        return Err(AudioConfigError::Invalid(format!(
            "media.engines.fluid_audio.prewarm references unknown FluidAudio model {unknown}"
        )));
    }
    Ok(())
}

fn validate_binding_list(
    stage: &str,
    providers: &[AudioProviderBindingConfig],
    engines: &BTreeMap<String, AudioEngineConfig>,
) -> Result<(), AudioConfigError> {
    validate_provider_ids(stage, providers.iter().map(|provider| provider.id.as_str()))?;
    for provider in providers {
        if provider.engine_id.trim().is_empty()
            || provider.adapter.trim().is_empty()
            || provider.model.trim().is_empty()
        {
            return Err(AudioConfigError::Invalid(format!(
                "media.{stage}.providers.{} requires engine_id, adapter, and model",
                provider.id
            )));
        }
        if !engines.is_empty() && !engines.contains_key(&provider.engine_id) {
            return Err(AudioConfigError::Invalid(format!(
                "media.{stage}.providers.{} references unknown engine {}",
                provider.id, provider.engine_id
            )));
        }
    }
    Ok(())
}

fn validate_provider_ids<'a>(
    stage: &str,
    ids: impl Iterator<Item = &'a str>,
) -> Result<(), AudioConfigError> {
    let mut seen = HashSet::new();
    for id in ids {
        let normalized = id.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(AudioConfigError::Invalid(format!(
                "media.{stage} provider id cannot be empty"
            )));
        }
        if !seen.insert(normalized) {
            return Err(AudioConfigError::Invalid(format!(
                "media.{stage} contains duplicate provider id {id}"
            )));
        }
    }
    Ok(())
}

fn validate_profiles(
    profiles: &BTreeMap<String, AudioSurfaceProfileConfig>,
    defaults: &BTreeMap<AudioSurface, String>,
    catalog: &BTreeMap<AudioStage, Vec<AudioStageOption>>,
) -> Result<(), AudioConfigError> {
    for (surface, profile_id) in defaults {
        let profile = profiles.get(profile_id).ok_or_else(|| {
            AudioConfigError::Invalid(format!(
                "default profile {profile_id} for {surface} does not exist"
            ))
        })?;
        if profile.surface != *surface {
            return Err(AudioConfigError::Invalid(format!(
                "default profile {profile_id} belongs to {}, not {surface}",
                profile.surface
            )));
        }
    }
    for (profile_id, profile) in profiles {
        if profile_id.trim().is_empty() {
            return Err(AudioConfigError::Invalid(
                "audio profile id cannot be empty".to_string(),
            ));
        }
        validate_profile(profile_id, profile, catalog)?;
    }
    Ok(())
}

fn validate_profile(
    profile_id: &str,
    profile: &AudioSurfaceProfileConfig,
    catalog: &BTreeMap<AudioStage, Vec<AudioStageOption>>,
) -> Result<(), AudioConfigError> {
    match profile.surface {
        AudioSurface::Dictation if profile.streaming_stt.enabled => {
            return Err(AudioConfigError::Invalid(format!(
                "profile {profile_id}: Dictation cannot enable streaming_stt"
            )));
        },
        AudioSurface::Meeting | AudioSurface::Listening | AudioSurface::HandsFree
            if profile.recording_stt.enabled =>
        {
            return Err(AudioConfigError::Invalid(format!(
                "profile {profile_id}: {} cannot enable recording_stt",
                profile.surface
            )));
        },
        _ => {},
    }
    if profile.turn_boundary == TurnBoundaryAuthority::Vad {
        if !profile.vad.enabled || profile.vad.mode != Some(VadMode::TurnAuthority) {
            return Err(AudioConfigError::Invalid(format!(
                "profile {profile_id}: VAD turn boundary requires enabled vad.mode=turn_authority"
            )));
        }
    } else if profile.vad.mode == Some(VadMode::TurnAuthority) {
        return Err(AudioConfigError::Invalid(format!(
            "profile {profile_id}: vad.mode=turn_authority requires turn_boundary=vad"
        )));
    }
    if profile.surface == AudioSurface::HandsFree
        && (!profile.vad.enabled || !profile.streaming_stt.enabled || !profile.tts.enabled)
    {
        return Err(AudioConfigError::Invalid(format!(
            "profile {profile_id}: Hands-free requires VAD, streaming STT, and TTS"
        )));
    }
    for stage in AudioStage::ALL {
        let config = profile.stage(stage);
        if !profile.surface.supports_stage(stage)
            && (config.enabled || config.required || !config.providers.is_empty())
        {
            return Err(AudioConfigError::Invalid(format!(
                "profile {profile_id}: {} does not support stage {stage}",
                profile.surface
            )));
        }
        if config.required && !config.enabled {
            return Err(AudioConfigError::Invalid(format!(
                "profile {profile_id}: required stage {stage} is disabled"
            )));
        }
        if config.enabled && config.providers.is_empty() {
            return Err(AudioConfigError::Invalid(format!(
                "profile {profile_id}: enabled stage {stage} has no provider chain"
            )));
        }
        let mut seen = HashSet::new();
        for provider_id in &config.providers {
            if !seen.insert(provider_id.to_ascii_lowercase()) {
                return Err(AudioConfigError::Invalid(format!(
                    "profile {profile_id}: stage {stage} repeats provider {provider_id}"
                )));
            }
            if find_option(catalog, stage, provider_id).is_none() {
                return Err(AudioConfigError::Invalid(format!(
                    "profile {profile_id}: stage {stage} references unknown provider {provider_id}"
                )));
            }
        }
        if let Some(threshold) = config.threshold {
            if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
                return Err(AudioConfigError::Invalid(format!(
                    "profile {profile_id}: stage {stage} threshold must be between 0 and 1"
                )));
            }
        }
        if stage == AudioStage::Vad {
            validate_vad_stage(profile_id, config)?;
        } else if config.mode.is_some()
            || config.threshold.is_some()
            || config.min_speech_ms.is_some()
            || config.min_silence_ms.is_some()
            || config.pre_roll_ms.is_some()
            || config.hangover_ms.is_some()
            || config.max_utterance_ms.is_some()
        {
            return Err(AudioConfigError::Invalid(format!(
                "profile {profile_id}: VAD timing and mode fields are invalid on stage {stage}"
            )));
        }
    }
    Ok(())
}

fn validate_vad_stage(
    profile_id: &str,
    config: &AudioStageProfileConfig,
) -> Result<(), AudioConfigError> {
    if config.enabled && config.mode.is_none() {
        return Err(AudioConfigError::Invalid(format!(
            "profile {profile_id}: enabled VAD requires an explicit mode"
        )));
    }
    let min_speech_ms = config.min_speech_ms.unwrap_or(250);
    let min_silence_ms = config.min_silence_ms.unwrap_or(500);
    let pre_roll_ms = config.pre_roll_ms.unwrap_or(400);
    let hangover_ms = config.hangover_ms.unwrap_or(600);
    let max_utterance_ms = config.max_utterance_ms.unwrap_or(120_000);
    if min_speech_ms == 0 || min_silence_ms == 0 || max_utterance_ms == 0 {
        return Err(AudioConfigError::Invalid(format!(
            "profile {profile_id}: VAD minimum speech, minimum silence, and maximum utterance must be positive"
        )));
    }
    const MAX_VAD_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;
    if [
        min_speech_ms,
        min_silence_ms,
        pre_roll_ms,
        hangover_ms,
        max_utterance_ms,
    ]
    .into_iter()
    .any(|value| value > MAX_VAD_DURATION_MS)
    {
        return Err(AudioConfigError::Invalid(format!(
            "profile {profile_id}: VAD timing values cannot exceed 24 hours"
        )));
    }
    if min_speech_ms > max_utterance_ms {
        return Err(AudioConfigError::Invalid(format!(
            "profile {profile_id}: VAD minimum speech cannot exceed maximum utterance"
        )));
    }
    Ok(())
}

fn resolve_snapshot(
    snapshot: &AudioRuntimeSnapshot,
    surface: AudioSurface,
    preferences: &MediaPreferences,
    explicit_profile: Option<&str>,
    explicit_stage_options: &BTreeMap<AudioStage, String>,
) -> Result<ResolvedAudioProfile, AudioConfigError> {
    let mut degradations = Vec::new();
    let (profile_id, source) = select_profile(
        snapshot,
        surface,
        preferences,
        explicit_profile,
        &mut degradations,
    )?;
    let mut profile = snapshot
        .profiles
        .get(&profile_id)
        .cloned()
        .ok_or_else(|| AudioConfigError::Invalid(format!("unknown profile {profile_id}")))?;

    // An explicit request profile is an isolation boundary. In particular,
    // native clients keep profile choices device-local and must not inherit a
    // provider override previously saved by another surface such as Web.
    if source != AudioProfileSource::ExplicitRequest {
        if let Some(overrides) = preferences.surface_stage_options.get(&surface) {
            for (stage, option_id) in overrides {
                if let Err(error) = apply_stage_override(snapshot, &mut profile, *stage, option_id)
                {
                    degradations.push(format!("ignored stale scoped {stage} option: {error}"));
                }
            }
        }
    }
    for (stage, option_id) in explicit_stage_options {
        apply_stage_override(snapshot, &mut profile, *stage, option_id)?;
    }

    let mut stages = BTreeMap::new();
    for stage in AudioStage::ALL {
        let config = profile.stage(stage);
        let mut options = Vec::new();
        for provider_id in &config.providers {
            if let Some(option) = find_option(&snapshot.catalog, stage, provider_id) {
                options.push(option.clone());
            }
        }
        let selected = options
            .iter()
            .find(|option| option.availability == ProviderAvailability::Available)
            .cloned();
        let degraded_reason = if config.enabled && selected.is_none() {
            Some(format!("no available {stage} provider"))
        } else if config.enabled
            && options
                .first()
                .is_some_and(|option| option.availability != ProviderAvailability::Available)
        {
            Some(format!("{stage} fell back from the configured primary"))
        } else {
            None
        };
        if let Some(reason) = degraded_reason.as_ref() {
            degradations.push(reason.clone());
        }
        stages.insert(
            stage,
            ResolvedAudioStage {
                stage,
                enabled: config.enabled,
                required: config.required,
                mode: config.mode,
                providers: options,
                selected,
                degraded_reason,
                threshold: config.threshold,
                min_speech_ms: config.min_speech_ms,
                min_silence_ms: config.min_silence_ms,
                pre_roll_ms: config.pre_roll_ms,
                hangover_ms: config.hangover_ms,
                max_utterance_ms: config.max_utterance_ms,
            },
        );
    }
    Ok(ResolvedAudioProfile {
        surface,
        profile_id,
        revision: snapshot.revision.clone(),
        source,
        turn_boundary: profile.turn_boundary,
        stages,
        degradations,
    })
}

fn select_profile(
    snapshot: &AudioRuntimeSnapshot,
    surface: AudioSurface,
    preferences: &MediaPreferences,
    explicit_profile: Option<&str>,
    degradations: &mut Vec<String>,
) -> Result<(String, AudioProfileSource), AudioConfigError> {
    if let Some(profile_id) = normalized_override(explicit_profile) {
        validate_profile_surface(snapshot, surface, profile_id)?;
        return Ok((profile_id.to_string(), AudioProfileSource::ExplicitRequest));
    }
    if let Some(profile_id) = preferences
        .surface_profiles
        .get(&surface)
        .and_then(|value| normalized_override(Some(value)))
    {
        match validate_profile_surface(snapshot, surface, profile_id) {
            Ok(()) => {
                return Ok((profile_id.to_string(), AudioProfileSource::ScopedPreference));
            },
            Err(error) => degradations.push(format!("ignored stale scoped profile: {error}")),
        }
    }
    let profile_id = snapshot.default_profiles.get(&surface).ok_or_else(|| {
        AudioConfigError::Invalid(format!("no default profile is configured for {surface}"))
    })?;
    Ok((profile_id.clone(), AudioProfileSource::ConfiguredDefault))
}

fn validate_profile_surface(
    snapshot: &AudioRuntimeSnapshot,
    surface: AudioSurface,
    profile_id: &str,
) -> Result<(), AudioConfigError> {
    let profile = snapshot
        .profiles
        .get(profile_id)
        .ok_or_else(|| AudioConfigError::Invalid(format!("unknown audio profile {profile_id}")))?;
    if profile.surface != surface {
        return Err(AudioConfigError::Invalid(format!(
            "profile {profile_id} belongs to {}, not {surface}",
            profile.surface
        )));
    }
    Ok(())
}

fn migrate_legacy_stage_provider(
    snapshot: &AudioRuntimeSnapshot,
    preferences: &mut MediaPreferences,
    surface: AudioSurface,
    stage: AudioStage,
    provider_id: Option<&str>,
) {
    if preferences
        .surface_stage_options
        .get(&surface)
        .is_some_and(|options| options.contains_key(&stage))
    {
        return;
    }
    let Some(provider_id) = provider_id.and_then(|value| normalized_override(Some(value))) else {
        return;
    };
    let Some(profile) = selected_profile(snapshot, preferences, surface) else {
        return;
    };
    let Some(option) = find_option(&snapshot.catalog, stage, provider_id) else {
        return;
    };
    if !profile
        .stage(stage)
        .providers
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&option.provider_id))
    {
        return;
    }
    preferences
        .surface_stage_options
        .entry(surface)
        .or_default()
        .insert(stage, option.option_id.clone());
}

fn migrate_legacy_stage_engine(
    snapshot: &AudioRuntimeSnapshot,
    preferences: &mut MediaPreferences,
    surface: AudioSurface,
    stage: AudioStage,
    engine_id: &str,
) {
    if preferences
        .surface_stage_options
        .get(&surface)
        .is_some_and(|options| options.contains_key(&stage))
    {
        return;
    }
    let Some(profile) = selected_profile(snapshot, preferences, surface) else {
        return;
    };
    let option = profile
        .stage(stage)
        .providers
        .iter()
        .find_map(|provider_id| {
            find_option(&snapshot.catalog, stage, provider_id)
                .filter(|option| option.engine_id.eq_ignore_ascii_case(engine_id))
        });
    if let Some(option) = option {
        preferences
            .surface_stage_options
            .entry(surface)
            .or_default()
            .insert(stage, option.option_id.clone());
    }
}

fn selected_profile<'a>(
    snapshot: &'a AudioRuntimeSnapshot,
    preferences: &MediaPreferences,
    surface: AudioSurface,
) -> Option<&'a AudioSurfaceProfileConfig> {
    preferences
        .surface_profiles
        .get(&surface)
        .and_then(|profile_id| snapshot.profiles.get(profile_id))
        .filter(|profile| profile.surface == surface)
        .or_else(|| {
            snapshot
                .default_profiles
                .get(&surface)
                .and_then(|profile_id| snapshot.profiles.get(profile_id))
        })
}

fn validate_stage_override(
    snapshot: &AudioRuntimeSnapshot,
    profile: &AudioSurfaceProfileConfig,
    stage: AudioStage,
    option_id: &str,
) -> Result<(), AudioConfigError> {
    validate_surface_stage(profile.surface, stage)?;
    let option_id = option_id.trim();
    if is_auto_stage_option(option_id) {
        return Ok(());
    }
    if option_id.eq_ignore_ascii_case("off") {
        if profile.stage(stage).required {
            return Err(AudioConfigError::Invalid(format!(
                "stage {stage} is required for {}",
                profile.surface
            )));
        }
        return Ok(());
    }
    find_option(&snapshot.catalog, stage, option_id)
        .ok_or_else(|| AudioConfigError::Invalid(format!("unknown {stage} option {option_id}")))?;
    Ok(())
}

fn apply_stage_override(
    snapshot: &AudioRuntimeSnapshot,
    profile: &mut AudioSurfaceProfileConfig,
    stage: AudioStage,
    option_id: &str,
) -> Result<(), AudioConfigError> {
    validate_surface_stage(profile.surface, stage)?;
    let option_id = option_id.trim();
    if is_auto_stage_option(option_id) {
        return Ok(());
    }
    let surface = profile.surface;
    let stage_config = profile.stage_mut(stage);
    if option_id.eq_ignore_ascii_case("off") {
        if stage_config.required {
            return Err(AudioConfigError::Invalid(format!(
                "stage {stage} is required for {}",
                surface
            )));
        }
        stage_config.enabled = false;
        return Ok(());
    }
    let option = find_option(&snapshot.catalog, stage, option_id)
        .ok_or_else(|| AudioConfigError::Invalid(format!("unknown {stage} option {option_id}")))?;
    stage_config.enabled = true;
    move_provider_to_head(&mut stage_config.providers, &option.provider_id);
    Ok(())
}

fn is_auto_stage_option(option_id: &str) -> bool {
    option_id.is_empty()
        || option_id.eq_ignore_ascii_case("auto")
        || option_id.eq_ignore_ascii_case("default")
}

fn validate_surface_stage(
    surface: AudioSurface,
    stage: AudioStage,
) -> Result<(), AudioConfigError> {
    if surface.supports_stage(stage) {
        Ok(())
    } else {
        Err(AudioConfigError::Invalid(format!(
            "surface {surface} does not support stage {stage}"
        )))
    }
}

fn move_provider_to_head(providers: &mut Vec<String>, provider_id: &str) {
    let Some(provider_id) = normalized_override(Some(provider_id)) else {
        return;
    };
    providers.retain(|candidate| !candidate.eq_ignore_ascii_case(provider_id));
    providers.insert(0, provider_id.to_string());
}

fn normalized_override(value: Option<&str>) -> Option<&str> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| !value.eq_ignore_ascii_case("auto"))
        .filter(|value| !value.eq_ignore_ascii_case("default"))
}

fn find_option<'a>(
    catalog: &'a BTreeMap<AudioStage, Vec<AudioStageOption>>,
    stage: AudioStage,
    option_id: &str,
) -> Option<&'a AudioStageOption> {
    catalog.get(&stage)?.iter().find(|option| {
        option.option_id.eq_ignore_ascii_case(option_id)
            || option.provider_id.eq_ignore_ascii_case(option_id)
    })
}

fn build_engine_statuses(
    settings: &MagicianMediaSettings,
    catalog: &BTreeMap<AudioStage, Vec<AudioStageOption>>,
) -> BTreeMap<String, AudioEngineStatus> {
    let mut configs = settings.engines.clone();
    for option in catalog.values().flatten() {
        configs
            .entry(option.engine_id.clone())
            .or_insert_with(AudioEngineConfig::default);
    }
    configs
        .into_iter()
        .map(|(engine_id, config)| {
            let available = config.enabled
                && catalog.values().flatten().any(|option| {
                    option.engine_id == engine_id
                        && option.availability == ProviderAvailability::Available
                });
            let label = config.label.unwrap_or_else(|| engine_id.replace('_', " "));
            (
                engine_id.clone(),
                AudioEngineStatus {
                    engine_id,
                    label,
                    enabled: config.enabled,
                    available,
                    healthy: None,
                    owned: None,
                    endpoint: config.endpoint,
                    startup_policy: None,
                    download_policy: None,
                    offline: None,
                    process_idle_secs: None,
                    model_idle_secs: None,
                    max_resident_models: None,
                    max_streaming_sessions: None,
                    resident_models: None,
                    active_sessions: None,
                    process_id: None,
                    start_count: None,
                    restart_count: None,
                    last_started_at_ms: None,
                    last_error: None,
                    can_manage_models: false,
                },
            )
        })
        .collect()
}

fn engine_for_provider(provider_id: &str) -> String {
    let id = provider_id.to_ascii_lowercase();
    if id.starts_with("macos") || id.starts_with("apple") {
        "macos_system".to_string()
    } else if id.starts_with("fluid") {
        "fluid_audio".to_string()
    } else if id.starts_with("noop") {
        "test".to_string()
    } else {
        "online".to_string()
    }
}

fn engine_for_adapter(adapter: &str) -> String {
    let adapter = adapter.to_ascii_lowercase();
    if adapter.starts_with("macos") || adapter.starts_with("apple") {
        "macos_system".to_string()
    } else if adapter.starts_with("fluid_audio") {
        "fluid_audio".to_string()
    } else {
        "online".to_string()
    }
}

fn apply_settings_patch(
    settings: &mut MagicianMediaSettings,
    patch: AudioSettingsPatch,
) -> Result<(), AudioConfigError> {
    for (engine_id, engine_patch) in patch.engines {
        let engine = settings.engines.get_mut(&engine_id).ok_or_else(|| {
            AudioConfigError::Invalid(format!("unknown audio engine {engine_id}"))
        })?;
        if let Some(enabled) = engine_patch.enabled {
            engine.enabled = enabled;
        }
    }
    for (surface, profile_id) in patch.default_profiles {
        if profile_id.trim().is_empty() {
            return Err(AudioConfigError::Invalid(format!(
                "default profile for {surface} cannot be empty"
            )));
        }
        settings
            .surface_profiles
            .default_mapping
            .insert(surface, profile_id);
    }
    for (profile_id, profile_patch) in patch.profiles {
        let profile = settings
            .surface_profiles
            .profiles
            .get_mut(&profile_id)
            .ok_or_else(|| {
                AudioConfigError::Invalid(format!("unknown audio profile {profile_id}"))
            })?;
        if let Some(turn_boundary) = profile_patch.turn_boundary {
            profile.turn_boundary = turn_boundary;
        }
        for (stage, stage_patch) in profile_patch.stages {
            let stage_config = profile.stage_mut(stage);
            if let Some(enabled) = stage_patch.enabled {
                stage_config.enabled = enabled;
            }
            if let Some(providers) = stage_patch.providers {
                stage_config.providers = providers;
            }
        }
    }
    Ok(())
}

fn ensure_live_media_revision(path: &Path, expected: &str) -> Result<(), AudioConfigError> {
    let config = crate::config::load_magician_config_from_path(path)
        .map_err(|error| AudioConfigError::Persistence(error.to_string()))?;
    let revision = media_revision(&config.media)?;
    if revision != expected {
        return Err(AudioConfigError::ExternalConfigDrift);
    }
    Ok(())
}

fn persist_media_settings(
    path: &Path,
    settings: &MagicianMediaSettings,
) -> Result<(), AudioConfigError> {
    #[derive(Serialize)]
    struct MediaBlock<'a> {
        media: &'a MagicianMediaSettings,
    }
    let block = serde_yaml::to_string(&MediaBlock { media: settings })
        .map_err(|error| AudioConfigError::Persistence(error.to_string()))?;
    write_top_level_yaml_block(path, "media", block.trim_end())
        .map_err(|error| AudioConfigError::Persistence(error.to_string()))
}

fn media_revision(settings: &MagicianMediaSettings) -> Result<String, AudioConfigError> {
    let bytes = serde_json::to_vec(settings)
        .map_err(|error| AudioConfigError::Invalid(error.to_string()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::media_seam::{
        AudioEngineStartupPolicy, AudioProviderCatalogConfig, AudioSurfaceProfilesConfig, SttError,
        SttProvider, SttRequest, SttResponse,
    };
    use async_trait::async_trait;

    struct TestStt(&'static str);

    #[async_trait]
    impl SttProvider for TestStt {
        fn id(&self) -> &str {
            self.0
        }

        fn default_model(&self) -> &str {
            "test-model"
        }

        async fn transcribe(&self, _request: SttRequest) -> Result<SttResponse, SttError> {
            unreachable!("runtime config tests do not transcribe")
        }
    }

    fn binding(id: &str, engine_id: &str, capabilities: &[&str]) -> AudioProviderBindingConfig {
        AudioProviderBindingConfig {
            id: id.to_string(),
            label: Some(id.to_string()),
            engine_id: engine_id.to_string(),
            adapter: format!("{id}_adapter"),
            model: format!("{id}_model"),
            variant: None,
            revision: None,
            sha256: None,
            idle_secs: None,
            language_codes: vec!["en".to_string()],
            capabilities: capabilities.iter().map(|value| value.to_string()).collect(),
            enabled: true,
        }
    }

    fn meeting_profile(providers: &[&str]) -> AudioSurfaceProfileConfig {
        AudioSurfaceProfileConfig {
            surface: AudioSurface::Meeting,
            turn_boundary: TurnBoundaryAuthority::SttEou,
            vad: AudioStageProfileConfig::default(),
            recording_stt: AudioStageProfileConfig::default(),
            streaming_stt: AudioStageProfileConfig::enabled_with(
                providers.iter().map(|value| value.to_string()).collect(),
            ),
            diarization: AudioStageProfileConfig::default(),
            tts: AudioStageProfileConfig::default(),
        }
    }

    fn settings_with_profiles() -> MagicianMediaSettings {
        let mut settings = MagicianMediaSettings::default();
        settings.streaming_stt = AudioProviderCatalogConfig {
            providers: vec![
                binding("local-stt", "macos_system", &[]),
                binding("cloud-stt", "online", &["end_of_utterance"]),
            ],
        };
        settings.surface_profiles = AudioSurfaceProfilesConfig {
            default_mapping: [(AudioSurface::Meeting, "meeting-default".to_string())]
                .into_iter()
                .collect(),
            profiles: [
                (
                    "meeting-default".to_string(),
                    meeting_profile(&["cloud-stt", "local-stt"]),
                ),
                (
                    "meeting-local".to_string(),
                    meeting_profile(&["local-stt", "cloud-stt"]),
                ),
            ]
            .into_iter()
            .collect(),
        };
        settings
    }

    fn seed_persisted_test_config(path: &Path) {
        let mut document: serde_yaml::Value =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("template YAML");
        let root = document.as_mapping_mut().expect("template root");
        let runtime = root
            .get_mut(&serde_yaml::Value::String("runtime".to_string()))
            .and_then(serde_yaml::Value::as_mapping_mut)
            .expect("runtime mapping");
        let ollama = runtime
            .get_mut(&serde_yaml::Value::String("ollama".to_string()))
            .and_then(serde_yaml::Value::as_mapping_mut)
            .expect("ollama mapping");
        ollama.insert(
            serde_yaml::Value::String("prewarm".to_string()),
            serde_yaml::Value::Bool(false),
        );
        root.insert(
            serde_yaml::Value::String("media".to_string()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        std::fs::write(
            path,
            serde_yaml::to_string(&document).expect("test config YAML"),
        )
        .expect("seed config");
    }

    #[test]
    fn resolver_precedence_is_explicit_then_scoped_then_configured_default() {
        let manager = AudioRuntimeConfigManager::in_memory(
            settings_with_profiles(),
            Arc::new(MediaProviderRegistry::new()),
        )
        .expect("runtime");
        let mut preferences = MediaPreferences::default();
        preferences
            .surface_profiles
            .insert(AudioSurface::Meeting, "meeting-local".to_string());

        let scoped = manager
            .resolve(AudioSurface::Meeting, &preferences, None, &BTreeMap::new())
            .expect("scoped resolution");
        assert_eq!(scoped.profile_id, "meeting-local");
        assert_eq!(scoped.source, AudioProfileSource::ScopedPreference);

        let explicit = manager
            .resolve(
                AudioSurface::Meeting,
                &preferences,
                Some("meeting-default"),
                &BTreeMap::new(),
            )
            .expect("explicit resolution");
        assert_eq!(explicit.profile_id, "meeting-default");
        assert_eq!(explicit.source, AudioProfileSource::ExplicitRequest);

        preferences.surface_profiles.clear();
        let configured = manager
            .resolve(AudioSurface::Meeting, &preferences, None, &BTreeMap::new())
            .expect("configured resolution");
        assert_eq!(configured.profile_id, "meeting-default");
        assert_eq!(configured.source, AudioProfileSource::ConfiguredDefault);
    }

    #[test]
    fn explicit_profile_does_not_inherit_scoped_stage_overrides() {
        let manager = AudioRuntimeConfigManager::in_memory(
            settings_with_profiles(),
            Arc::new(MediaProviderRegistry::new()),
        )
        .expect("runtime");
        let mut preferences = MediaPreferences::default();
        preferences.surface_stage_options.insert(
            AudioSurface::Meeting,
            [(AudioStage::StreamingStt, "cloud-stt".to_string())]
                .into_iter()
                .collect(),
        );

        let explicit = manager
            .resolve(
                AudioSurface::Meeting,
                &preferences,
                Some("meeting-local"),
                &BTreeMap::new(),
            )
            .expect("explicit profile");
        assert_eq!(
            explicit.stages[&AudioStage::StreamingStt].providers[0].provider_id,
            "local-stt"
        );

        let request_override = [(AudioStage::StreamingStt, "cloud-stt".to_string())]
            .into_iter()
            .collect();
        let explicit = manager
            .resolve(
                AudioSurface::Meeting,
                &preferences,
                Some("meeting-local"),
                &request_override,
            )
            .expect("explicit stage override");
        assert_eq!(
            explicit.stages[&AudioStage::StreamingStt].providers[0].provider_id,
            "cloud-stt"
        );
    }

    #[test]
    fn legacy_recording_preference_migrates_to_a_dictation_stage_option() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
recording_stt:
  providers:
    - id: openai
      adapter: openai_transcriptions
      model: cloud
    - id: fluid-local
      engine_id: fluid_audio
      adapter: fluid_audio_recording_stt
      model: FluidInference/qwen3-asr-0.6b-coreml
      variant: f32
surface_profiles:
  default_mapping:
    dictation: dictation-fluid
  profiles:
    dictation-fluid:
      surface: dictation
      turn_boundary: push_to_talk
      recording_stt:
        enabled: true
        providers: [fluid-local, openai]
"#,
        )
        .expect("media settings");
        let providers = Arc::new(
            MediaProviderRegistry::new()
                .with_stt(Arc::new(TestStt("openai")))
                .with_stt_fallback(Arc::new(TestStt("fluid-local"))),
        );
        let manager = AudioRuntimeConfigManager::in_memory(settings, providers).expect("runtime");
        let preferences = manager.migrate_legacy_preferences(
            MediaPreferences::default(),
            LegacyMediaPreferenceHints {
                recording_stt_provider: Some("openai".to_string()),
                ..LegacyMediaPreferenceHints::default()
            },
        );
        assert_eq!(
            preferences.surface_stage_options[&AudioSurface::Dictation][&AudioStage::RecordingStt],
            "openai"
        );

        let resolved = manager
            .resolve(
                AudioSurface::Dictation,
                &preferences,
                None,
                &BTreeMap::new(),
            )
            .expect("migrated default");
        assert_eq!(
            resolved.stages[&AudioStage::RecordingStt]
                .selected
                .as_ref()
                .expect("selected")
                .provider_id,
            "openai"
        );
    }

    #[test]
    fn legacy_observe_choice_migrates_to_surface_stage_options() {
        let manager = AudioRuntimeConfigManager::in_memory(
            settings_with_profiles(),
            Arc::new(MediaProviderRegistry::new()),
        )
        .expect("runtime");
        let preferences = manager.migrate_legacy_preferences(
            MediaPreferences::default(),
            LegacyMediaPreferenceHints {
                observe_stt_mode: Some("local".to_string()),
                ..LegacyMediaPreferenceHints::default()
            },
        );
        let resolved = manager
            .resolve(AudioSurface::Meeting, &preferences, None, &BTreeMap::new())
            .expect("resolution");
        let streaming = resolved
            .stages
            .get(&AudioStage::StreamingStt)
            .expect("streaming stage");
        assert_eq!(streaming.providers[0].provider_id, "local-stt");
    }

    #[test]
    fn one_time_profile_migration_preserves_provider_catalog_order() {
        let mut settings = MagicianMediaSettings::default();
        settings.streaming_stt = AudioProviderCatalogConfig {
            providers: vec![
                binding("local-stt", "macos_system", &[]),
                binding(
                    "cloud-diarize",
                    "online",
                    &["end_of_utterance", "speaker_attribution"],
                ),
            ],
        };
        let manager =
            AudioRuntimeConfigManager::in_memory(settings, Arc::new(MediaProviderRegistry::new()))
                .expect("runtime");
        assert_eq!(
            manager
                .snapshot()
                .default_profiles
                .get(&AudioSurface::Meeting)
                .map(String::as_str),
            Some(MIGRATED_MEETING_PROFILE_ID)
        );
        let resolved = manager
            .resolve(
                AudioSurface::Meeting,
                &MediaPreferences::default(),
                None,
                &BTreeMap::new(),
            )
            .expect("resolution");
        let ids = resolved.stages[&AudioStage::StreamingStt]
            .providers
            .iter()
            .map(|option| option.provider_id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["local-stt", "cloud-diarize"]);
    }

    #[test]
    fn explicit_stage_option_is_prepended_without_dropping_fallbacks() {
        let manager = AudioRuntimeConfigManager::in_memory(
            settings_with_profiles(),
            Arc::new(MediaProviderRegistry::new()),
        )
        .expect("runtime");
        let overrides = [(AudioStage::StreamingStt, "local-stt".to_string())]
            .into_iter()
            .collect();
        let resolved = manager
            .resolve(
                AudioSurface::Meeting,
                &MediaPreferences::default(),
                None,
                &overrides,
            )
            .expect("resolution");
        let ids = resolved.stages[&AudioStage::StreamingStt]
            .providers
            .iter()
            .map(|option| option.provider_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["local-stt", "cloud-stt"]);
    }

    #[test]
    fn stale_scoped_profile_falls_back_with_visible_degradation() {
        let manager = AudioRuntimeConfigManager::in_memory(
            settings_with_profiles(),
            Arc::new(MediaProviderRegistry::new()),
        )
        .expect("runtime");
        let mut preferences = MediaPreferences::default();
        preferences
            .surface_profiles
            .insert(AudioSurface::Meeting, "removed-profile".to_string());
        let resolved = manager
            .resolve(AudioSurface::Meeting, &preferences, None, &BTreeMap::new())
            .expect("fallback resolution");
        assert_eq!(resolved.profile_id, "meeting-default");
        assert!(resolved
            .degradations
            .iter()
            .any(|reason| reason.contains("stale scoped profile")));
    }

    #[test]
    fn preference_sanitation_removes_stale_values_without_touching_valid_ones() {
        let manager = AudioRuntimeConfigManager::in_memory(
            settings_with_profiles(),
            Arc::new(MediaProviderRegistry::new()),
        )
        .expect("runtime");
        let mut preferences = MediaPreferences::default();
        preferences
            .surface_profiles
            .insert(AudioSurface::Meeting, "removed-profile".to_string());
        preferences.surface_stage_options.insert(
            AudioSurface::Meeting,
            [(AudioStage::StreamingStt, "local-stt".to_string())]
                .into_iter()
                .collect(),
        );
        preferences.surface_stage_options.insert(
            AudioSurface::Dictation,
            [(AudioStage::StreamingStt, "off".to_string())]
                .into_iter()
                .collect(),
        );

        let sanitized = manager.sanitize_preferences(preferences);

        assert!(sanitized.surface_profiles.is_empty());
        assert_eq!(
            sanitized.surface_stage_options[&AudioSurface::Meeting][&AudioStage::StreamingStt],
            "local-stt"
        );
        assert!(!sanitized
            .surface_stage_options
            .contains_key(&AudioSurface::Dictation));
    }

    #[test]
    fn vad_turn_authority_requires_an_enabled_authoritative_vad_stage() {
        let mut settings = settings_with_profiles();
        let profile = settings
            .surface_profiles
            .profiles
            .get_mut("meeting-default")
            .expect("profile");
        profile.turn_boundary = TurnBoundaryAuthority::Vad;
        let error =
            AudioRuntimeConfigManager::in_memory(settings, Arc::new(MediaProviderRegistry::new()))
                .expect_err("invalid VAD authority");
        assert!(error.to_string().contains("vad.mode=turn_authority"));
    }

    #[test]
    fn enabled_vad_requires_a_mode_and_sane_timing() {
        let mut settings = settings_with_profiles();
        settings.vad = AudioProviderCatalogConfig {
            providers: vec![binding("test-vad", "test", &[])],
        };
        let profile = settings
            .surface_profiles
            .profiles
            .get_mut("meeting-default")
            .expect("profile");
        profile.vad = AudioStageProfileConfig {
            enabled: true,
            providers: vec!["test-vad".to_string()],
            ..AudioStageProfileConfig::default()
        };
        let error = AudioRuntimeConfigManager::in_memory(
            settings.clone(),
            Arc::new(MediaProviderRegistry::new()),
        )
        .expect_err("mode is required");
        assert!(error.to_string().contains("explicit mode"));

        let profile = settings
            .surface_profiles
            .profiles
            .get_mut("meeting-default")
            .expect("profile");
        profile.vad.mode = Some(VadMode::GateOnly);
        profile.vad.min_speech_ms = Some(2_000);
        profile.vad.max_utterance_ms = Some(1_000);
        let error =
            AudioRuntimeConfigManager::in_memory(settings, Arc::new(MediaProviderRegistry::new()))
                .expect_err("minimum speech exceeds utterance");
        assert!(error.to_string().contains("minimum speech"));
    }

    #[test]
    fn update_is_revision_checked_after_profile_migration() {
        let legacy_revision =
            media_revision(&MagicianMediaSettings::default()).expect("legacy revision");
        let manager = AudioRuntimeConfigManager::in_memory(
            MagicianMediaSettings::default(),
            Arc::new(MediaProviderRegistry::new()),
        )
        .expect("runtime");
        let revision = manager.snapshot().revision.clone();
        assert_ne!(legacy_revision, revision);
        let error = manager
            .update(AudioSettingsPatch {
                expected_revision: legacy_revision,
                engines: BTreeMap::new(),
                default_profiles: BTreeMap::new(),
                profiles: BTreeMap::new(),
            })
            .expect_err("pre-migration revision is stale");
        assert!(matches!(error, AudioConfigError::RevisionConflict { .. }));

        let next = manager
            .update(AudioSettingsPatch {
                expected_revision: revision,
                engines: BTreeMap::new(),
                default_profiles: BTreeMap::new(),
                profiles: BTreeMap::new(),
            })
            .expect("post-migration revision is current");
        assert!(!next.source_settings.surface_profiles.profiles.is_empty());
    }

    #[test]
    fn engine_toggle_removes_and_restores_fluid_audio_profile_options() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
  online:
    enabled: true
recording_stt:
  providers:
    - id: fluid-local
      engine_id: fluid_audio
      adapter: fluid_audio_recording_stt
      model: local-model
    - id: online-stt
      adapter: openai_transcriptions
      model: online-model
surface_profiles:
  default_mapping:
    dictation: dictation-test
  profiles:
    dictation-test:
      surface: dictation
      turn_boundary: push_to_talk
      recording_stt:
        enabled: true
        required: true
        providers: [fluid-local, online-stt]
"#,
        )
        .expect("settings");
        let providers = Arc::new(
            MediaProviderRegistry::new()
                .with_stt(Arc::new(TestStt("fluid-local")))
                .with_stt_fallback(Arc::new(TestStt("online-stt"))),
        );
        let manager = AudioRuntimeConfigManager::in_memory(settings, providers).expect("runtime");

        let initial = manager
            .resolve(
                AudioSurface::Dictation,
                &MediaPreferences::default(),
                None,
                &BTreeMap::new(),
            )
            .expect("initial resolution");
        assert_eq!(
            initial
                .stages
                .get(&AudioStage::RecordingStt)
                .and_then(|stage| stage.selected.as_ref())
                .map(|option| option.provider_id.as_str()),
            Some("fluid-local")
        );

        let disabled = manager
            .update(AudioSettingsPatch {
                expected_revision: manager.snapshot().revision.clone(),
                engines: [(
                    "fluid_audio".to_string(),
                    AudioEngineSettingsPatch {
                        enabled: Some(false),
                    },
                )]
                .into_iter()
                .collect(),
                default_profiles: BTreeMap::new(),
                profiles: BTreeMap::new(),
            })
            .expect("disable FluidAudio");
        assert!(!disabled.engines["fluid_audio"].enabled);
        assert_eq!(
            disabled.catalog[&AudioStage::RecordingStt]
                .iter()
                .find(|option| option.provider_id == "fluid-local")
                .map(|option| option.availability),
            Some(ProviderAvailability::Disabled)
        );
        let fallback = manager
            .resolve(
                AudioSurface::Dictation,
                &MediaPreferences::default(),
                None,
                &BTreeMap::new(),
            )
            .expect("fallback resolution");
        assert_eq!(
            fallback
                .stages
                .get(&AudioStage::RecordingStt)
                .and_then(|stage| stage.selected.as_ref())
                .map(|option| option.provider_id.as_str()),
            Some("online-stt")
        );

        let enabled = manager
            .update(AudioSettingsPatch {
                expected_revision: disabled.revision.clone(),
                engines: [(
                    "fluid_audio".to_string(),
                    AudioEngineSettingsPatch {
                        enabled: Some(true),
                    },
                )]
                .into_iter()
                .collect(),
                default_profiles: BTreeMap::new(),
                profiles: BTreeMap::new(),
            })
            .expect("enable FluidAudio");
        assert!(enabled.engines["fluid_audio"].enabled);
        assert_eq!(
            enabled.catalog[&AudioStage::RecordingStt]
                .iter()
                .find(|option| option.provider_id == "fluid-local")
                .map(|option| option.availability),
            Some(ProviderAvailability::Available)
        );
    }

    #[test]
    fn runtime_engine_availability_changes_resolution_without_persisting_config() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
recording_stt:
  providers:
    - id: fluid-local
      engine_id: fluid_audio
      adapter: fluid_audio_recording_stt
      model: local-model
surface_profiles:
  default_mapping:
    dictation: dictation-test
  profiles:
    dictation-test:
      surface: dictation
      turn_boundary: push_to_talk
      recording_stt:
        enabled: true
        required: true
        providers: [fluid-local]
"#,
        )
        .expect("settings");
        let providers =
            Arc::new(MediaProviderRegistry::new().with_stt(Arc::new(TestStt("fluid-local"))));
        let manager = AudioRuntimeConfigManager::in_memory(settings, providers).expect("runtime");
        let revision = manager.snapshot().revision.clone();

        let unavailable = manager
            .set_engine_runtime_availability(
                "fluid_audio",
                false,
                Some("sidecar unavailable".to_string()),
            )
            .expect("mark unavailable");
        assert_eq!(unavailable.revision, revision);
        assert!(unavailable.source_settings.engines["fluid_audio"].enabled);
        assert_eq!(
            unavailable.catalog[&AudioStage::RecordingStt][0].availability,
            ProviderAvailability::Unavailable
        );

        let restored = manager
            .set_engine_runtime_availability("fluid_audio", true, None)
            .expect("restore availability");
        assert_eq!(restored.revision, revision);
        assert_eq!(
            restored.catalog[&AudioStage::RecordingStt][0].availability,
            ProviderAvailability::Available
        );
    }

    #[test]
    fn startup_persists_missing_surface_defaults_before_serving() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("magician-config.yaml");
        seed_persisted_test_config(&path);

        let manager = AudioRuntimeConfigManager::new(
            MagicianMediaSettings::default(),
            Arc::new(MediaProviderRegistry::new()),
            path.clone(),
        )
        .expect("migrated runtime");
        assert_eq!(
            manager
                .snapshot()
                .default_profiles
                .get(&AudioSurface::Dictation)
                .map(String::as_str),
            Some(MIGRATED_DICTATION_PROFILE_ID)
        );

        let persisted =
            crate::config::load_magician_config_from_path(&path).expect("persisted config");
        assert_eq!(
            persisted
                .media
                .surface_profiles
                .default_mapping
                .get(&AudioSurface::Listening)
                .map(String::as_str),
            Some(MIGRATED_LISTENING_PROFILE_ID)
        );
    }

    #[test]
    fn persisted_update_and_runtime_snapshot_share_the_returned_revision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("magician-config.yaml");
        seed_persisted_test_config(&path);
        let config = crate::config::load_magician_config_from_path(&path).expect("load config");
        let manager = AudioRuntimeConfigManager::new(
            config.media,
            Arc::new(MediaProviderRegistry::new()),
            path.clone(),
        )
        .expect("runtime");
        let revision = manager.snapshot().revision.clone();

        let next = manager
            .update(AudioSettingsPatch {
                expected_revision: revision,
                engines: BTreeMap::new(),
                default_profiles: BTreeMap::new(),
                profiles: BTreeMap::new(),
            })
            .expect("persisted update");
        let persisted =
            crate::config::load_magician_config_from_path(&path).expect("reload config");
        assert_eq!(
            media_revision(&persisted.media).expect("revision"),
            next.revision
        );
        assert_eq!(manager.snapshot().revision, next.revision);
    }

    #[test]
    fn rejected_or_unwritable_update_changes_neither_file_nor_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("magician-config.yaml");
        seed_persisted_test_config(&path);
        let config = crate::config::load_magician_config_from_path(&path).expect("load config");
        let manager = AudioRuntimeConfigManager::new(
            config.media,
            Arc::new(MediaProviderRegistry::new()),
            path.clone(),
        )
        .expect("runtime");
        let revision = manager.snapshot().revision.clone();
        let before = std::fs::read(&path).expect("read config");

        let error = manager
            .update(AudioSettingsPatch {
                expected_revision: revision.clone(),
                engines: BTreeMap::new(),
                default_profiles: BTreeMap::new(),
                profiles: [(
                    "missing-profile".to_string(),
                    AudioProfileSettingsPatch::default(),
                )]
                .into_iter()
                .collect(),
            })
            .expect_err("invalid patch");
        assert!(matches!(error, AudioConfigError::Invalid(_)));
        assert_eq!(std::fs::read(&path).expect("read config"), before);
        assert_eq!(manager.snapshot().revision, revision);

        std::fs::remove_file(&path).expect("remove config");
        let error = manager
            .update(AudioSettingsPatch {
                expected_revision: revision.clone(),
                engines: BTreeMap::new(),
                default_profiles: BTreeMap::new(),
                profiles: BTreeMap::new(),
            })
            .expect_err("missing config blocks persistence");
        assert!(matches!(error, AudioConfigError::Persistence(_)));
        assert_eq!(manager.snapshot().revision, revision);
    }

    #[test]
    fn overrides_reject_stages_that_do_not_apply_to_the_surface() {
        let manager = AudioRuntimeConfigManager::in_memory(
            MagicianMediaSettings::default(),
            Arc::new(MediaProviderRegistry::new()),
        )
        .expect("runtime");
        let overrides = [(AudioStage::StreamingStt, "off".to_string())]
            .into_iter()
            .collect();
        let error = manager
            .resolve(
                AudioSurface::Dictation,
                &MediaPreferences::default(),
                None,
                &overrides,
            )
            .expect_err("dictation streaming stage is invalid");
        assert!(error
            .to_string()
            .contains("dictation does not support stage streaming_stt"));
    }

    #[test]
    fn fluid_audio_engine_rejects_unsafe_origins_and_invalid_resource_limits() {
        let mut settings = MagicianMediaSettings::default();
        let mut engine = AudioEngineConfig {
            endpoint: Some("http://127.0.0.1:3029/api".to_string()),
            ..AudioEngineConfig::default()
        };
        settings
            .engines
            .insert("fluid_audio".to_string(), engine.clone());
        let error = validate_media_settings(&settings).expect_err("path must be rejected");
        assert!(error.to_string().contains("explicit loopback HTTP port"));

        engine.endpoint = Some("http://example.com:3029".to_string());
        settings
            .engines
            .insert("fluid_audio".to_string(), engine.clone());
        let error = validate_media_settings(&settings).expect_err("remote host must be rejected");
        assert!(error.to_string().contains("explicit loopback HTTP port"));

        engine.endpoint = Some("http://localhost:3029".to_string());
        engine.max_streaming_sessions = 0;
        settings.engines.insert("fluid_audio".to_string(), engine);
        let error = validate_media_settings(&settings).expect_err("zero limit must be rejected");
        assert!(error.to_string().contains("must be positive"));
    }

    #[test]
    fn fluid_audio_kokoro_binding_requires_consistent_voice_and_wav_inventory() {
        let mut settings = MagicianMediaSettings::default();
        settings.tts = serde_yaml::from_str(
            r#"
providers:
  - id: fluid-kokoro-en
    adapter: fluid_audio_kokoro_tts
    model: FluidInference/kokoro-82m-coreml
    variant: 15s
    revision: main
    voice: af_heart
    voices: [af_kore]
    format: wav
    formats: [wav]
"#,
        )
        .expect("tts settings");
        let error = validate_media_settings(&settings).expect_err("missing default voice");
        assert!(error.to_string().contains("default voice is absent"));

        settings.tts.providers[0].voices = vec!["af_heart".to_string()];
        settings.tts.providers[0].formats = vec!["mp3".to_string()];
        let error = validate_media_settings(&settings).expect_err("non-wav Kokoro output");
        assert!(error.to_string().contains("default format is absent"));

        settings.tts.providers[0].formats = vec!["wav".to_string()];
        validate_media_settings(&settings).expect("valid Kokoro binding");
    }

    #[test]
    fn fluid_audio_engine_rejects_invalid_version_external_auth_and_prewarm_ids() {
        let mut settings = MagicianMediaSettings::default();
        let mut engine = AudioEngineConfig {
            minimum_macos_version: Some("14.beta".to_string()),
            ..AudioEngineConfig::default()
        };
        settings
            .engines
            .insert("fluid_audio".to_string(), engine.clone());
        let error = validate_media_settings(&settings).expect_err("version must be numeric");
        assert!(error.to_string().contains("numeric dotted version"));

        engine.minimum_macos_version = Some("14.0".to_string());
        engine.startup = AudioEngineStartupPolicy::External;
        settings
            .engines
            .insert("fluid_audio".to_string(), engine.clone());
        let error = validate_media_settings(&settings).expect_err("external auth is required");
        assert!(error.to_string().contains("auth_token_env is required"));

        engine.startup = AudioEngineStartupPolicy::Lazy;
        engine.prewarm = vec!["missing-model".to_string()];
        settings.engines.insert("fluid_audio".to_string(), engine);
        let error = validate_media_settings(&settings).expect_err("prewarm ID must be known");
        assert!(error.to_string().contains("unknown FluidAudio model"));
    }

    #[test]
    fn fluid_audio_recording_models_can_prewarm_but_cannot_reuse_vad_ids() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
    startup: lazy
    prewarm: [fluid-qwen]
recording_stt:
  providers:
    - id: fluid-qwen
      engine_id: fluid_audio
      adapter: fluid_audio_recording_stt
      model: FluidInference/qwen3-asr-0.6b-coreml
"#,
        )
        .expect("settings");
        validate_media_settings(&settings).expect("recording model should be prewarmable");

        let duplicate: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
    startup: lazy
vad:
  providers:
    - id: shared-model
      engine_id: fluid_audio
      adapter: fluid_audio_vad
      model: FluidInference/silero-vad-coreml
recording_stt:
  providers:
    - id: shared-model
      engine_id: fluid_audio
      adapter: fluid_audio_recording_stt
      model: FluidInference/qwen3-asr-0.6b-coreml
"#,
        )
        .expect("duplicate settings");
        let error = validate_media_settings(&duplicate).expect_err("duplicate must fail");
        assert!(error
            .to_string()
            .contains("duplicated across stage catalogs"));
    }

    #[test]
    fn fluid_audio_models_from_every_stage_can_prewarm() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
    startup: lazy
    prewarm: [fluid-vad, fluid-streaming, fluid-diarization, fluid-recording, fluid-tts]
vad:
  providers:
    - id: fluid-vad
      engine_id: fluid_audio
      adapter: fluid_audio_vad
      model: FluidInference/silero-vad-coreml
streaming_stt:
  providers:
    - id: fluid-streaming
      engine_id: fluid_audio
      adapter: fluid_audio_streaming_eou_stt
      model: FluidInference/parakeet-realtime-eou-120m-coreml
diarization:
  providers:
    - id: fluid-diarization
      engine_id: fluid_audio
      adapter: fluid_audio_streaming_sortformer
      model: FluidInference/diar-streaming-sortformer-coreml
recording_stt:
  providers:
    - id: fluid-recording
      engine_id: fluid_audio
      adapter: fluid_audio_recording_stt
      model: FluidInference/qwen3-asr-0.6b-coreml
tts:
  providers:
    - id: fluid-tts
      adapter: fluid_audio_kokoro_tts
      model: FluidInference/kokoro-82m-coreml
      variant: 15s
      revision: main
      voice: af_heart
      voices: [af_heart]
      format: wav
      formats: [wav]
"#,
        )
        .expect("settings");

        validate_media_settings(&settings)
            .expect("every FluidAudio-backed stage should be prewarmable");
    }

    #[test]
    fn settings_response_advertises_backend_owned_model_lifecycle_state() {
        let options = vec![
            AudioStageOption {
                option_id: "fluid-vad:fluid-vad-model".to_string(),
                stage: AudioStage::Vad,
                provider_id: "fluid-vad".to_string(),
                engine_id: "fluid_audio".to_string(),
                model_id: "fluid-vad-model".to_string(),
                variant: None,
                label: "Fluid VAD".to_string(),
                capabilities: AudioStageCapabilities::default(),
                availability: ProviderAvailability::Available,
                unavailable_reason: None,
            },
            AudioStageOption {
                option_id: "cloud-vad:cloud-model".to_string(),
                stage: AudioStage::Vad,
                provider_id: "cloud-vad".to_string(),
                engine_id: "online".to_string(),
                model_id: "cloud-model".to_string(),
                variant: None,
                label: "Cloud VAD".to_string(),
                capabilities: AudioStageCapabilities::default(),
                availability: ProviderAvailability::Available,
                unavailable_reason: None,
            },
        ];
        let statuses =
            build_model_runtime_statuses(&[(AudioStage::Vad, options)].into_iter().collect());

        assert_eq!(
            statuses["fluid-vad:fluid-vad-model"].state,
            AudioModelLifecycleState::DownloadRequired
        );
        assert!(!statuses["fluid-vad:fluid-vad-model"].resident);
        assert_eq!(
            statuses["cloud-vad:cloud-model"].state,
            AudioModelLifecycleState::Ready
        );
    }
}
