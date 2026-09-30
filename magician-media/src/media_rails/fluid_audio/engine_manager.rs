use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Serialize;
use tokio::process::{Child, Command};
use tokio::sync::{watch, Mutex};
use tracing::{info, warn};
use uuid::Uuid;

use crate::media_rails::{
    AudioEngineConfig, AudioEngineStartupPolicy, AudioModelDownloadPolicy, CachedTtsProvider,
    MEDIA_AUDIO_ENGINE_STARTED, MEDIA_AUDIO_ENGINE_STOPPED, MEDIA_AUDIO_ENGINE_UNHEALTHY,
    MEDIA_AUDIO_MODEL_LOADED, MEDIA_AUDIO_MODEL_LOADING, MEDIA_AUDIO_MODEL_UNLOADED,
    MEDIA_SYSTEM_AGENT,
};
use magician::config::MagicianMediaSettings;
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

use super::protocol::{
    FluidAudioModelDefinition, FluidAudioSidecarConfig, SidecarHealth, SidecarModelState,
    SidecarSpeechRequest, SidecarSpeechResponse, SidecarTranscriptionResponse,
    FLUID_AUDIO_DIARIZATION_ADAPTER, FLUID_AUDIO_PROTOCOL_VERSION,
    FLUID_AUDIO_RECORDING_STT_ADAPTER, FLUID_AUDIO_STREAMING_STT_ADAPTER, FLUID_AUDIO_TTS_ADAPTER,
    FLUID_AUDIO_VAD_ADAPTER,
};

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:3029";
const DEFAULT_BINARY: &str = "magician-macos-audio-engine.bin";
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(150);
const SESSION_SHUTDOWN_GRACE: Duration = Duration::from_millis(500);
const EXTERNAL_UNLOAD_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Serialize)]
pub struct FluidAudioEngineStatus {
    pub available: bool,
    pub healthy: bool,
    pub owned: bool,
    pub endpoint: String,
    pub startup_policy: AudioEngineStartupPolicy,
    pub download_policy: AudioModelDownloadPolicy,
    pub offline: bool,
    pub process_idle_secs: u64,
    pub model_idle_secs: u64,
    pub max_resident_models: usize,
    pub max_streaming_sessions: usize,
    pub process_id: Option<u32>,
    pub start_count: u64,
    pub restart_count: u64,
    pub last_started_at_ms: Option<u64>,
    pub last_error: Option<String>,
}

struct EngineState {
    child: Option<Child>,
    token: Option<String>,
    owned: bool,
    reaper_started: bool,
    shutting_down: bool,
    start_count: u64,
    restart_count: u64,
    last_started_at_ms: Option<u64>,
    last_error: Option<String>,
}

struct Inner {
    config: AudioEngineConfig,
    enabled: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    generation_tx: watch::Sender<u64>,
    endpoint: String,
    models: Vec<FluidAudioModelDefinition>,
    client: reqwest::Client,
    config_path: PathBuf,
    startup_lock: Mutex<()>,
    state: Mutex<EngineState>,
    tts_caches: StdMutex<Vec<Weak<CachedTtsProvider>>>,
    broadcaster: OnceLock<Arc<RuntimeTransportBroadcaster>>,
}

/// A generation-bound permit for one FluidAudio operation or streaming
/// session. Disabling or re-enabling the engine invalidates every outstanding
/// lease, allowing work to stop without waiting for the process-state mutex.
#[derive(Clone)]
pub struct FluidAudioEngineLease {
    generation: u64,
    enabled: Arc<AtomicBool>,
    changes: watch::Receiver<u64>,
}

impl FluidAudioEngineLease {
    pub fn ensure_current(&self) -> Result<(), String> {
        if !self.enabled.load(Ordering::Acquire) || *self.changes.borrow() != self.generation {
            Err("FluidAudio engine is disabled".to_string())
        } else {
            Ok(())
        }
    }

    pub async fn cancelled(&mut self) {
        loop {
            if self.ensure_current().is_err() {
                return;
            }
            if self.changes.changed().await.is_err() {
                return;
            }
        }
    }
}

#[derive(Clone)]
pub struct FluidAudioEngineManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for FluidAudioEngineManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FluidAudioEngineManager")
            .field("endpoint", &self.inner.endpoint)
            .field("models", &self.inner.models)
            .finish_non_exhaustive()
    }
}

impl FluidAudioEngineManager {
    pub fn from_media_settings(
        settings: &MagicianMediaSettings,
        config_path: PathBuf,
    ) -> Result<Option<Self>, String> {
        let Some(config) = settings.engines.get("fluid_audio").cloned() else {
            return Ok(None);
        };

        let mut models = settings
            .vad
            .providers
            .iter()
            .filter(|binding| {
                binding.enabled
                    && binding.engine_id == "fluid_audio"
                    && binding.adapter == FLUID_AUDIO_VAD_ADAPTER
            })
            .map(|binding| FluidAudioModelDefinition {
                id: binding.id.clone(),
                adapter: binding.adapter.clone(),
                repository: binding.model.clone(),
                variant: binding.variant.clone(),
                revision: binding.revision.clone(),
                sha256: binding.sha256.clone(),
                idle_secs: binding.idle_secs.unwrap_or(config.default_model_idle_secs),
                voice: None,
                voices: Vec::new(),
                formats: Vec::new(),
            })
            .collect::<Vec<_>>();
        models.extend(
            settings
                .recording_stt
                .providers
                .iter()
                .filter(|binding| {
                    binding.enabled
                        && binding.adapter == FLUID_AUDIO_RECORDING_STT_ADAPTER
                        && binding
                            .engine_id
                            .as_deref()
                            .is_none_or(|engine| engine == "fluid_audio")
                })
                .map(|binding| FluidAudioModelDefinition {
                    id: binding.id.clone(),
                    adapter: binding.adapter.clone(),
                    repository: binding.model.clone(),
                    variant: binding.variant.clone(),
                    revision: binding.revision.clone(),
                    sha256: binding.sha256.clone(),
                    idle_secs: binding.idle_secs.unwrap_or(config.default_model_idle_secs),
                    voice: None,
                    voices: Vec::new(),
                    formats: Vec::new(),
                }),
        );
        models.extend(
            settings
                .streaming_stt
                .providers
                .iter()
                .filter(|binding| {
                    binding.enabled
                        && binding.engine_id == "fluid_audio"
                        && binding.adapter == FLUID_AUDIO_STREAMING_STT_ADAPTER
                })
                .map(|binding| FluidAudioModelDefinition {
                    id: binding.id.clone(),
                    adapter: binding.adapter.clone(),
                    repository: binding.model.clone(),
                    variant: binding.variant.clone(),
                    revision: binding.revision.clone(),
                    sha256: binding.sha256.clone(),
                    idle_secs: binding.idle_secs.unwrap_or(config.default_model_idle_secs),
                    voice: None,
                    voices: Vec::new(),
                    formats: Vec::new(),
                }),
        );
        models.extend(
            settings
                .diarization
                .providers
                .iter()
                .filter(|binding| {
                    binding.enabled
                        && binding.engine_id == "fluid_audio"
                        && binding.adapter == FLUID_AUDIO_DIARIZATION_ADAPTER
                })
                .map(|binding| FluidAudioModelDefinition {
                    id: binding.id.clone(),
                    adapter: binding.adapter.clone(),
                    repository: binding.model.clone(),
                    variant: binding.variant.clone(),
                    revision: binding.revision.clone(),
                    sha256: binding.sha256.clone(),
                    idle_secs: binding.idle_secs.unwrap_or(config.default_model_idle_secs),
                    voice: None,
                    voices: Vec::new(),
                    formats: Vec::new(),
                }),
        );
        models.extend(
            settings
                .tts
                .providers
                .iter()
                .filter(|binding| binding.enabled && binding.adapter == FLUID_AUDIO_TTS_ADAPTER)
                .map(|binding| FluidAudioModelDefinition {
                    id: binding.id.clone(),
                    adapter: binding.adapter.clone(),
                    repository: binding.model.clone(),
                    variant: binding.variant.clone(),
                    revision: binding.revision.clone(),
                    sha256: binding.sha256.clone(),
                    idle_secs: binding.idle_secs.unwrap_or(config.default_model_idle_secs),
                    voice: binding.voice.clone(),
                    voices: binding.voices.clone(),
                    formats: binding.formats.clone(),
                }),
        );
        if models.is_empty() {
            return Ok(None);
        }

        let endpoint = config
            .endpoint
            .clone()
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
        let timeout = Duration::from_secs(config.request_timeout_secs.max(1));
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| format!("building FluidAudio HTTP client: {error}"))?;
        let enabled = Arc::new(AtomicBool::new(config.enabled));
        let (generation_tx, _) = watch::channel(0);
        Ok(Some(Self {
            inner: Arc::new(Inner {
                enabled,
                generation: Arc::new(AtomicU64::new(0)),
                generation_tx,
                config,
                endpoint,
                models,
                client,
                config_path,
                startup_lock: Mutex::new(()),
                state: Mutex::new(EngineState {
                    child: None,
                    token: None,
                    owned: false,
                    reaper_started: false,
                    shutting_down: false,
                    start_count: 0,
                    restart_count: 0,
                    last_started_at_ms: None,
                    last_error: None,
                }),
                tts_caches: StdMutex::new(Vec::new()),
                broadcaster: OnceLock::new(),
            }),
        }))
    }

    pub fn install_broadcaster(
        &self,
        broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Result<(), String> {
        self.inner
            .broadcaster
            .set(broadcaster)
            .map_err(|_| "FluidAudio event broadcaster is already installed".to_string())
    }

    pub fn endpoint(&self) -> &str {
        &self.inner.endpoint
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Acquire)
    }

    pub fn cache_gate(&self) -> (Arc<AtomicBool>, Arc<AtomicU64>) {
        (
            Arc::clone(&self.inner.enabled),
            Arc::clone(&self.inner.generation),
        )
    }

    pub fn register_tts_cache(&self, cache: Weak<CachedTtsProvider>) {
        let mut caches = self
            .inner
            .tts_caches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        caches.retain(|registered| registered.strong_count() > 0);
        caches.push(cache);
    }

    pub async fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        if enabled {
            if self.is_enabled() {
                return Ok(());
            }
            let generation = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
            self.inner.generation_tx.send_replace(generation);
            self.inner.enabled.store(true, Ordering::Release);
            let mut state = self.inner.state.lock().await;
            state.shutting_down = false;
            state.last_error = None;
            return Ok(());
        }

        self.inner.enabled.store(false, Ordering::Release);
        let generation = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.inner.generation_tx.send_replace(generation);
        self.clear_registered_tts_caches().await;

        let external_token = {
            let state = self.inner.state.lock().await;
            (!state.owned)
                .then(|| state.token.clone().or_else(|| self.external_token()))
                .flatten()
        };
        if let Err(error) = self.stop_owned_sidecar("engine_disabled", false).await {
            self.record_error(&error).await;
            return Err(error);
        }
        if let Some(token) = external_token {
            tokio::time::sleep(SESSION_SHUTDOWN_GRACE).await;
            if let Err(error) =
                tokio::time::timeout(EXTERNAL_UNLOAD_TIMEOUT, self.unload_external_models(&token))
                    .await
                    .map_err(|_| {
                        "timed out unloading FluidAudio models from external sidecar".to_string()
                    })
                    .and_then(|result| result)
            {
                self.record_error(&error).await;
                warn!(%error, "FluidAudio external-sidecar cleanup degraded");
            }
            let mut state = self.inner.state.lock().await;
            if !state.owned {
                state.token = None;
            }
        }
        Ok(())
    }

    pub fn begin_operation(&self) -> Result<FluidAudioEngineLease, String> {
        self.ensure_enabled()?;
        let generation = self.inner.generation.load(Ordering::Acquire);
        let lease = FluidAudioEngineLease {
            generation,
            enabled: Arc::clone(&self.inner.enabled),
            changes: self.inner.generation_tx.subscribe(),
        };
        lease.ensure_current()?;
        Ok(lease)
    }

    pub fn model(&self, id: &str) -> Option<FluidAudioModelDefinition> {
        self.inner
            .models
            .iter()
            .find(|model| model.id.eq_ignore_ascii_case(id))
            .cloned()
    }

    pub fn model_is_supportable(&self, id: &str) -> bool {
        let Some(model) = self.model(id) else {
            return false;
        };
        #[cfg(not(target_os = "macos"))]
        {
            let _ = model;
            false
        }
        #[cfg(target_os = "macos")]
        {
            if model.adapter == FLUID_AUDIO_RECORDING_STT_ADAPTER {
                self.macos_version_at_least("15.0")
            } else {
                self.macos_version_supported()
            }
        }
    }

    pub fn model_ids(&self) -> Vec<String> {
        self.inner
            .models
            .iter()
            .map(|model| model.id.clone())
            .collect()
    }

    pub fn configured_prewarm_ids(&self) -> Vec<String> {
        self.inner.config.prewarm.clone()
    }

    pub fn startup_prewarm_enabled(&self) -> bool {
        self.is_enabled()
            && self.inner.config.download_policy == AudioModelDownloadPolicy::Prewarm
            && !self.inner.config.prewarm.is_empty()
    }

    /// Prepare the configured interactive models without blocking service
    /// startup. The caller owns task spawning; this method waits for every
    /// selected load so failures are reported as one actionable diagnostic.
    pub async fn prewarm_configured_models(&self) -> Result<(), String> {
        self.ensure_enabled()?;
        let model_ids = self.configured_prewarm_ids();
        if model_ids.is_empty() {
            return Ok(());
        }
        let mut failures = Vec::new();
        for model_id in model_ids {
            if let Err(error) = self.load_model(&model_id).await {
                failures.push(format!("{model_id}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "FluidAudio startup prewarm failed for {} model(s): {}",
                failures.len(),
                failures.join("; ")
            ))
        }
    }

    pub async fn is_supportable(&self) -> bool {
        self.is_enabled() && self.is_host_supportable().await
    }

    pub fn can_register_providers(&self) -> bool {
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
        #[cfg(target_os = "macos")]
        {
            if !self.macos_version_supported() {
                return false;
            }
            self.inner.config.startup != AudioEngineStartupPolicy::Disabled
        }
    }

    pub async fn is_host_supportable(&self) -> bool {
        #[cfg(not(target_os = "macos"))]
        {
            return false;
        }
        #[cfg(target_os = "macos")]
        {
            if !self.macos_version_supported() {
                return false;
            }
            if self.inner.config.startup == AudioEngineStartupPolicy::Lazy
                && self.resolve_binary().is_some()
            {
                return true;
            }
            if let Some(token) = self.external_token() {
                if self.health_with_token(&token).await.is_ok() {
                    return true;
                }
            }
            false
        }
    }

    pub async fn status(&self) -> FluidAudioEngineStatus {
        let state = self.inner.state.lock().await;
        let token = state.token.clone().or_else(|| self.external_token());
        let owned = state.owned;
        let process_id = state.child.as_ref().and_then(Child::id);
        let start_count = state.start_count;
        let restart_count = state.restart_count;
        let last_started_at_ms = state.last_started_at_ms;
        let last_error = state.last_error.clone();
        drop(state);
        let enabled = self.is_enabled();
        let healthy = match (enabled, token) {
            (true, Some(token)) => self.health_with_token(&token).await.is_ok(),
            _ => false,
        };
        let available = enabled
            && (healthy
                || (self.inner.config.startup == AudioEngineStartupPolicy::Lazy
                    && self.resolve_binary().is_some()));
        FluidAudioEngineStatus {
            available,
            healthy,
            owned,
            endpoint: self.inner.endpoint.clone(),
            startup_policy: self.inner.config.startup,
            download_policy: self.inner.config.download_policy,
            offline: self.inner.config.offline,
            process_idle_secs: self.inner.config.idle_process_secs,
            model_idle_secs: self.inner.config.default_model_idle_secs,
            max_resident_models: self.inner.config.max_resident_models,
            max_streaming_sessions: self.inner.config.max_streaming_sessions,
            process_id,
            start_count,
            restart_count,
            last_started_at_ms,
            last_error,
        }
    }

    /// Returns model lifecycle state only when a sidecar is already running.
    /// Status inspection must never start the process or download a model.
    pub async fn model_states_if_running(&self) -> Result<Option<Vec<SidecarModelState>>, String> {
        if !self.is_enabled() {
            return Ok(None);
        }
        let state = self.inner.state.lock().await;
        let token = state.token.clone().or_else(|| self.external_token());
        drop(state);
        let Some(token) = token else {
            return Ok(None);
        };
        self.model_states_with_token(&token).await.map(Some)
    }

    async fn model_states_with_token(&self, token: &str) -> Result<Vec<SidecarModelState>, String> {
        let mut url = reqwest::Url::parse(&self.inner.endpoint)
            .map_err(|error| format!("parsing FluidAudio endpoint: {error}"))?;
        url.path_segments_mut()
            .map_err(|_| "FluidAudio endpoint cannot be a base URL".to_string())?
            .pop_if_empty()
            .push("models");
        let response = tokio::time::timeout(
            Duration::from_secs(self.inner.config.health_timeout_secs.max(1)),
            self.authorized(self.inner.client.get(url), token)?.send(),
        )
        .await
        .map_err(|_| "FluidAudio model-state probe timed out".to_string())?
        .map_err(|error| format!("requesting FluidAudio model states: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "FluidAudio model-state probe returned {}",
                response.status()
            ));
        }
        response
            .json::<Vec<SidecarModelState>>()
            .await
            .map_err(|error| format!("decoding FluidAudio model states: {error}"))
    }

    pub async fn ensure_ready(&self) -> Result<String, String> {
        let lease = self.begin_operation()?;
        self.ensure_ready_with_lease(&lease).await
    }

    async fn ensure_ready_with_lease(
        &self,
        lease: &FluidAudioEngineLease,
    ) -> Result<String, String> {
        lease.ensure_current()?;
        #[cfg(not(target_os = "macos"))]
        return Err("FluidAudio requires macOS 14 or newer".to_string());

        #[cfg(target_os = "macos")]
        {
            if !self.macos_version_supported() {
                return Err(format!(
                    "FluidAudio requires macOS {} or newer",
                    self.inner
                        .config
                        .minimum_macos_version
                        .as_deref()
                        .unwrap_or("14.0")
                ));
            }
            let _startup_guard = self.inner.startup_lock.lock().await;
            lease.ensure_current()?;

            let current_token = self.inner.state.lock().await.token.clone();
            if let Some(token) = current_token {
                if self.health_with_token(&token).await.is_ok() {
                    lease.ensure_current()?;
                    return Ok(token);
                }
            }
            if let Some(token) = self.external_token() {
                if self.health_with_token(&token).await.is_ok() {
                    lease.ensure_current()?;
                    let mut state = self.inner.state.lock().await;
                    lease.ensure_current()?;
                    state.token = Some(token.clone());
                    state.owned = false;
                    return Ok(token);
                }
            }
            if self.inner.config.startup != AudioEngineStartupPolicy::Lazy {
                return Err("configured external FluidAudio sidecar is unavailable".to_string());
            }

            let binary = self.resolve_binary().ok_or_else(|| {
                format!("FluidAudio sidecar binary not found; build and stage {DEFAULT_BINARY}")
            })?;
            let sidecar_config = self.sidecar_config()?;
            let config_json = serde_json::to_string(&sidecar_config)
                .map_err(|error| format!("serializing FluidAudio sidecar config: {error}"))?;
            let startup_timeout = Duration::from_secs(
                self.inner
                    .config
                    .health_timeout_secs
                    .max(1)
                    .saturating_mul(4),
            );
            let attempts = self.inner.config.max_restart_attempts.saturating_add(1);
            let mut last_error = String::new();

            for attempt in 1..=attempts {
                lease.ensure_current()?;
                let previous_child = {
                    let mut state = self.inner.state.lock().await;
                    state.token = None;
                    state.owned = false;
                    state.child.take()
                };
                if let Some(child) = previous_child {
                    Self::terminate_child(child).await?;
                }

                let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
                let mut command = Command::new(&binary);
                command
                    .env("MAGICIAN_AUDIO_ENGINE_TOKEN", &token)
                    .env("MAGICIAN_AUDIO_ENGINE_ENDPOINT", &self.inner.endpoint)
                    .env("MAGICIAN_AUDIO_ENGINE_CONFIG_JSON", &config_json)
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .kill_on_drop(true);
                let child = match command.spawn() {
                    Ok(child) => child,
                    Err(error) => {
                        last_error =
                            format!("starting FluidAudio sidecar {}: {error}", binary.display());
                        if attempt < attempts {
                            warn!(attempt, attempts, error = %last_error, "retrying FluidAudio sidecar startup");
                            continue;
                        }
                        break;
                    },
                };
                let pid = child.id();
                if let Err(error) = lease.ensure_current() {
                    let _ = Self::terminate_child(child).await;
                    return Err(error);
                }
                let start_reaper = {
                    let mut state = self.inner.state.lock().await;
                    if let Err(error) = lease.ensure_current() {
                        drop(state);
                        let _ = Self::terminate_child(child).await;
                        return Err(error);
                    }
                    state.child = Some(child);
                    state.token = Some(token.clone());
                    state.owned = true;
                    let start_reaper = !state.reaper_started;
                    state.reaper_started = true;
                    start_reaper
                };
                if start_reaper {
                    self.spawn_child_reaper();
                }

                let started = tokio::time::Instant::now();
                last_error = loop {
                    if self.health_with_token(&token).await.is_ok() {
                        lease.ensure_current()?;
                        let mut state = self.inner.state.lock().await;
                        lease.ensure_current()?;
                        if state.start_count > 0 {
                            state.restart_count = state.restart_count.saturating_add(1);
                        }
                        state.start_count = state.start_count.saturating_add(1);
                        state.last_started_at_ms = Some(unix_time_ms());
                        state.last_error = None;
                        self.emit(
                            MEDIA_AUDIO_ENGINE_STARTED,
                            serde_json::json!({
                                "engine_id": "fluid_audio",
                                "endpoint": self.inner.endpoint,
                                "owned": true,
                                "process_id": pid,
                                "start_count": state.start_count,
                                "restart_count": state.restart_count,
                                "startup_ms": started.elapsed().as_millis(),
                            }),
                        );
                        info!(?pid, attempt, endpoint = %self.inner.endpoint, "FluidAudio sidecar ready");
                        return Ok(token);
                    }
                    lease.ensure_current()?;
                    let child_status = {
                        let mut state = self.inner.state.lock().await;
                        state.child.as_mut().map(Child::try_wait)
                    };
                    if let Some(child_status) = child_status {
                        match child_status {
                            Ok(Some(status)) => {
                                break format!(
                                    "FluidAudio sidecar exited during startup with status {status}"
                                );
                            },
                            Err(error) => {
                                break format!("checking FluidAudio sidecar status: {error}");
                            },
                            Ok(None) => {},
                        }
                    }
                    if started.elapsed() >= startup_timeout {
                        break format!(
                            "FluidAudio sidecar did not become healthy within {}s",
                            startup_timeout.as_secs()
                        );
                    }
                    tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
                };

                let failed_child = {
                    let mut state = self.inner.state.lock().await;
                    state.token = None;
                    state.owned = false;
                    state.child.take()
                };
                if let Some(child) = failed_child {
                    let _ = Self::terminate_child(child).await;
                }
                if attempt < attempts {
                    warn!(attempt, attempts, error = %last_error, "retrying FluidAudio sidecar startup");
                }
            }

            let error = format!(
                "FluidAudio sidecar failed to start after {attempts} attempt(s): {last_error}"
            );
            let mut state = self.inner.state.lock().await;
            state.last_error = Some(error.clone());
            self.emit(
                MEDIA_AUDIO_ENGINE_UNHEALTHY,
                serde_json::json!({"engine_id": "fluid_audio", "error": &error}),
            );
            Err(error)
        }
    }

    pub async fn load_model(&self, model_id: &str) -> Result<String, String> {
        let lease = self.begin_operation()?;
        self.load_model_with_lease(model_id, &lease).await
    }

    pub async fn prepare_streaming_session(
        &self,
        model_id: &str,
    ) -> Result<(String, FluidAudioEngineLease), String> {
        let lease = self.begin_operation()?;
        let token = self.load_model_with_lease(model_id, &lease).await?;
        lease.ensure_current()?;
        Ok((token, lease))
    }

    async fn load_model_with_lease(
        &self,
        model_id: &str,
        lease: &FluidAudioEngineLease,
    ) -> Result<String, String> {
        lease.ensure_current()?;
        if self.model(model_id).is_none() {
            return Err(format!("unknown configured FluidAudio model: {model_id}"));
        }
        let load_started = tokio::time::Instant::now();
        let was_resident = self
            .model_states_if_running()
            .await
            .ok()
            .flatten()
            .is_some_and(|states| {
                states
                    .iter()
                    .any(|state| state.id.eq_ignore_ascii_case(model_id) && state.resident)
            });
        if !was_resident {
            self.emit(
                MEDIA_AUDIO_MODEL_LOADING,
                serde_json::json!({"engine_id": "fluid_audio", "model_id": model_id}),
            );
        }
        let token = match self.ensure_ready_with_lease(lease).await {
            Ok(token) => token,
            Err(error) => {
                self.record_error(&error).await;
                self.emit(
                    MEDIA_AUDIO_ENGINE_UNHEALTHY,
                    serde_json::json!({"engine_id": "fluid_audio", "error": &error}),
                );
                return Err(error);
            },
        };
        let mut url = reqwest::Url::parse(&self.inner.endpoint)
            .map_err(|error| format!("parsing FluidAudio endpoint: {error}"))?;
        url.path_segments_mut()
            .map_err(|_| "FluidAudio endpoint cannot be a base URL".to_string())?
            .pop_if_empty()
            .push("models")
            .push(model_id)
            .push("load");
        let response = self.authorized(self.inner.client.post(url), &token)?.send();
        let mut cancellation = lease.clone();
        let response = tokio::select! {
            response = response => response
                .map_err(|error| format!("requesting FluidAudio model load: {error}"))?,
            _ = cancellation.cancelled() => return Err("FluidAudio engine is disabled".to_string()),
        };
        lease.ensure_current()?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("FluidAudio model load failed ({status}): {body}"));
        }
        let state = response
            .json::<SidecarModelState>()
            .await
            .map_err(|error| format!("decoding FluidAudio model state: {error}"))?;
        lease.ensure_current()?;
        if state.id != model_id || !matches!(state.state.as_str(), "loaded" | "loading") {
            return Err(format!(
                "FluidAudio returned unexpected model state {} for {}",
                state.state, state.id
            ));
        }
        if !was_resident {
            self.emit(
                MEDIA_AUDIO_MODEL_LOADED,
                serde_json::json!({
                    "engine_id": "fluid_audio",
                    "model_id": state.id,
                    "resident": state.resident,
                    "active_sessions": state.active_sessions,
                    "load_ms": load_started.elapsed().as_millis(),
                }),
            );
        }
        Ok(token)
    }

    pub async fn unload_model(&self, model_id: &str) -> Result<SidecarModelState, String> {
        let lease = self.begin_operation()?;
        if self.model(model_id).is_none() {
            return Err(format!("unknown configured FluidAudio model: {model_id}"));
        }
        let state = self.inner.state.lock().await;
        let token = state.token.clone().or_else(|| self.external_token());
        drop(state);
        let Some(token) = token else {
            return Ok(SidecarModelState {
                id: model_id.to_string(),
                state: "unloaded".to_string(),
                resident: false,
                active_sessions: 0,
            });
        };
        lease.ensure_current()?;
        self.unload_model_with_token(model_id, &token).await
    }

    async fn unload_model_with_token(
        &self,
        model_id: &str,
        token: &str,
    ) -> Result<SidecarModelState, String> {
        let mut url = reqwest::Url::parse(&self.inner.endpoint)
            .map_err(|error| format!("parsing FluidAudio endpoint: {error}"))?;
        url.path_segments_mut()
            .map_err(|_| "FluidAudio endpoint cannot be a base URL".to_string())?
            .pop_if_empty()
            .push("models")
            .push(model_id)
            .push("unload");
        let response = self.authorized(self.inner.client.post(url), token)?.send();
        let response = tokio::time::timeout(
            Duration::from_secs(self.inner.config.health_timeout_secs.max(1)),
            response,
        )
        .await
        .map_err(|_| "FluidAudio model unload timed out".to_string())?
        .map_err(|error| format!("requesting FluidAudio model unload: {error}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("FluidAudio model unload failed ({status}): {body}"));
        }
        let state = response
            .json::<SidecarModelState>()
            .await
            .map_err(|error| format!("decoding FluidAudio model state: {error}"))?;
        if state.id != model_id || state.resident {
            return Err(format!(
                "FluidAudio returned unexpected model state {} for {}",
                state.state, state.id
            ));
        }
        self.emit(
            MEDIA_AUDIO_MODEL_UNLOADED,
            serde_json::json!({
                "engine_id": "fluid_audio",
                "model_id": state.id,
                "active_sessions": state.active_sessions,
            }),
        );
        Ok(state)
    }

    async fn unload_external_models(&self, token: &str) -> Result<(), String> {
        loop {
            let states = self.model_states_with_token(token).await?;
            let mut pending_busy = false;
            for state in states
                .into_iter()
                .filter(|state| state.resident && self.model(&state.id).is_some())
            {
                if state.active_sessions > 0 {
                    pending_busy = true;
                    continue;
                }
                self.unload_model_with_token(&state.id, token).await?;
            }
            if !pending_busy {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn transcribe(
        &self,
        model_id: &str,
        audio: bytes::Bytes,
        content_type: &str,
        language: Option<&str>,
    ) -> Result<SidecarTranscriptionResponse, String> {
        let lease = self.begin_operation()?;
        let model = self
            .model(model_id)
            .ok_or_else(|| format!("unknown configured FluidAudio model: {model_id}"))?;
        if model.adapter != FLUID_AUDIO_RECORDING_STT_ADAPTER {
            return Err(format!(
                "configured FluidAudio model {model_id} does not support recording STT"
            ));
        }
        if audio.is_empty() {
            return Err("FluidAudio recording STT requires non-empty audio".to_string());
        }
        if audio.len() > self.inner.config.max_request_bytes {
            return Err(format!(
                "FluidAudio recording exceeds configured {} byte limit",
                self.inner.config.max_request_bytes
            ));
        }

        let token = self.load_model_with_lease(model_id, &lease).await?;
        lease.ensure_current()?;
        let url = format!(
            "{}/v1/audio/transcriptions",
            self.inner.endpoint.trim_end_matches('/')
        );
        let mut request = self
            .authorized(self.inner.client.post(url), &token)?
            .header(CONTENT_TYPE, content_type)
            .header("x-magician-audio-model", model_id)
            .body(audio);
        if let Some(language) = language.map(str::trim).filter(|value| !value.is_empty()) {
            request = request.header("x-magician-audio-language", language);
        }
        let mut cancellation = lease.clone();
        let response = tokio::select! {
            response = request.send() => response
                .map_err(|error| format!("calling FluidAudio recording STT: {error}"))?,
            _ = cancellation.cancelled() => return Err("FluidAudio engine is disabled".to_string()),
        };
        lease.ensure_current()?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "FluidAudio recording STT failed ({status}): {}",
                body.trim()
            ));
        }
        let response = response
            .json::<SidecarTranscriptionResponse>()
            .await
            .map_err(|error| format!("decoding FluidAudio transcription: {error}"))?;
        lease.ensure_current()?;
        if !response.model_id.eq_ignore_ascii_case(model_id) {
            return Err(format!(
                "FluidAudio transcription returned model {} for requested {model_id}",
                response.model_id
            ));
        }
        Ok(response)
    }

    pub async fn synthesize_speech(
        &self,
        model_id: &str,
        request: &SidecarSpeechRequest,
    ) -> Result<SidecarSpeechResponse, String> {
        let lease = self.begin_operation()?;
        let model = self
            .model(model_id)
            .ok_or_else(|| format!("unknown configured FluidAudio model: {model_id}"))?;
        if model.adapter != FLUID_AUDIO_TTS_ADAPTER {
            return Err(format!(
                "configured FluidAudio model {model_id} does not support TTS"
            ));
        }
        if request.input.trim().is_empty() {
            return Err("FluidAudio TTS requires non-empty input".to_string());
        }

        let token = self.load_model_with_lease(model_id, &lease).await?;
        lease.ensure_current()?;
        let url = format!(
            "{}/v1/audio/speech",
            self.inner.endpoint.trim_end_matches('/')
        );
        let request = self
            .authorized(self.inner.client.post(url), &token)?
            .header("x-magician-audio-model", model_id)
            .json(request);
        let mut cancellation = lease.clone();
        let response = tokio::select! {
            response = request.send() => response
                .map_err(|error| format!("calling FluidAudio TTS: {error}"))?,
            _ = cancellation.cancelled() => return Err("FluidAudio engine is disabled".to_string()),
        };
        lease.ensure_current()?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("FluidAudio TTS failed ({status}): {}", body.trim()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.inner.config.max_request_bytes as u64)
        {
            return Err("FluidAudio TTS response exceeds the configured byte limit".to_string());
        }
        let response_model = response
            .headers()
            .get("x-magician-audio-model")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let voice = response
            .headers()
            .get("x-magician-audio-voice")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let format = response
            .headers()
            .get("x-magician-audio-format")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        if !response_model.eq_ignore_ascii_case(model_id) || voice.is_empty() || format != "wav" {
            return Err("FluidAudio TTS returned inconsistent response metadata".to_string());
        }
        let audio = response
            .bytes()
            .await
            .map_err(|error| format!("reading FluidAudio TTS audio: {error}"))?;
        if audio.is_empty() {
            return Err("FluidAudio TTS returned empty audio".to_string());
        }
        if audio.len() > self.inner.config.max_request_bytes {
            return Err("FluidAudio TTS response exceeds the configured byte limit".to_string());
        }
        lease.ensure_current()?;
        Ok(SidecarSpeechResponse {
            audio,
            model_id: response_model,
            voice,
            format,
        })
    }

    pub async fn shutdown(&self) {
        if let Err(error) = self.set_enabled(false).await {
            warn!(%error, "FluidAudio disable during shutdown degraded");
        }
        let mut state = self.inner.state.lock().await;
        state.shutting_down = true;
    }

    async fn stop_owned_sidecar(
        &self,
        reason: &'static str,
        final_shutdown: bool,
    ) -> Result<(), String> {
        let child = {
            let mut state = self.inner.state.lock().await;
            state.shutting_down = final_shutdown;
            if !state.owned {
                return Ok(());
            }
            state.token = None;
            state.owned = false;
            state.child.take()
        };
        if let Some(child) = child {
            let pid = child.id();
            info!(pid = ?pid, "stopping Magician-owned FluidAudio sidecar");
            Self::terminate_child(child).await?;
            self.emit(
                MEDIA_AUDIO_ENGINE_STOPPED,
                serde_json::json!({
                    "engine_id": "fluid_audio",
                    "owned": true,
                    "process_id": pid,
                    "reason": reason,
                }),
            );
        }
        Ok(())
    }

    async fn terminate_child(mut child: Child) -> Result<(), String> {
        if child
            .try_wait()
            .map_err(|error| format!("checking FluidAudio sidecar before termination: {error}"))?
            .is_some()
        {
            return Ok(());
        }
        match tokio::time::timeout(Duration::from_secs(3), child.kill()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                if child.try_wait().ok().flatten().is_some() {
                    Ok(())
                } else {
                    Err(format!("terminating FluidAudio sidecar: {error}"))
                }
            },
            Err(_) => Err("timed out terminating FluidAudio sidecar".to_string()),
        }
    }

    async fn clear_registered_tts_caches(&self) {
        let caches = {
            let mut registered = self
                .inner
                .tts_caches
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let caches = registered
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            registered.retain(|cache| cache.strong_count() > 0);
            caches
        };
        for cache in caches {
            cache.clear_entries().await;
        }
    }

    fn ensure_enabled(&self) -> Result<(), String> {
        if self.is_enabled() {
            Ok(())
        } else {
            Err("FluidAudio engine is disabled".to_string())
        }
    }

    fn spawn_child_reaper(&self) {
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(inner) = inner.upgrade() else {
                    break;
                };
                let mut state = inner.state.lock().await;
                if state.shutting_down {
                    break;
                }
                let exit = match state.child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => Some(Ok(status)),
                        Ok(None) => None,
                        Err(error) => Some(Err(error)),
                    },
                    None => None,
                };
                match exit {
                    Some(Ok(status)) => {
                        let pid = state.child.as_ref().and_then(Child::id);
                        info!(%status, "reaped exited Magician-owned FluidAudio sidecar");
                        state.child = None;
                        state.token = None;
                        state.owned = false;
                        inner.emit(
                            MEDIA_AUDIO_ENGINE_STOPPED,
                            serde_json::json!({
                                "engine_id": "fluid_audio",
                                "owned": true,
                                "process_id": pid,
                                "reason": "process_exit",
                                "status": status.to_string(),
                            }),
                        );
                    },
                    Some(Err(error)) => {
                        warn!(%error, "failed to poll Magician-owned FluidAudio sidecar");
                        state.child = None;
                        state.token = None;
                        state.owned = false;
                        state.last_error = Some(error.to_string());
                        inner.emit(
                            MEDIA_AUDIO_ENGINE_UNHEALTHY,
                            serde_json::json!({
                                "engine_id": "fluid_audio",
                                "error": error.to_string(),
                            }),
                        );
                    },
                    None => {},
                }
            }
        });
    }

    fn sidecar_config(&self) -> Result<FluidAudioSidecarConfig, String> {
        let cache_dir = self.resolve_cache_dir();
        Ok(FluidAudioSidecarConfig {
            protocol_version: FLUID_AUDIO_PROTOCOL_VERSION,
            model_cache_dir: cache_dir.to_string_lossy().into_owned(),
            download_policy: match self.inner.config.download_policy {
                AudioModelDownloadPolicy::Disabled => "disabled",
                AudioModelDownloadPolicy::OnDemand => "on_demand",
                AudioModelDownloadPolicy::Prewarm => "prewarm",
            }
            .to_string(),
            registry_url: self.inner.config.registry_url.clone(),
            offline: self.inner.config.offline,
            process_idle_secs: self.inner.config.idle_process_secs,
            max_resident_models: self.inner.config.max_resident_models,
            max_streaming_sessions: self.inner.config.max_streaming_sessions,
            max_request_bytes: self.inner.config.max_request_bytes,
            max_frame_bytes: self.inner.config.max_frame_bytes,
            prewarm: self.inner.config.prewarm.clone(),
            models: self.inner.models.clone(),
        })
    }

    fn resolve_cache_dir(&self) -> PathBuf {
        let configured = self
            .inner
            .config
            .model_cache_dir
            .as_deref()
            .unwrap_or("models/audio/fluidaudio");
        let path = PathBuf::from(configured);
        if path.is_absolute() {
            path
        } else {
            self.inner
                .config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(path)
        }
    }

    fn resolve_binary(&self) -> Option<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(path) = std::env::var_os("MAGICIAN_FLUID_AUDIO_ENGINE_BIN") {
            candidates.push(PathBuf::from(path));
        }
        if let Ok(executable) = std::env::current_exe() {
            if let Some(parent) = executable.parent() {
                candidates.push(parent.join(DEFAULT_BINARY));
                candidates.push(parent.join("../Resources").join(DEFAULT_BINARY));
            }
        }
        if let Ok(current_dir) = std::env::current_dir() {
            candidates.push(current_dir.join(DEFAULT_BINARY));
        }
        candidates.into_iter().find(|candidate| candidate.is_file())
    }

    fn external_token(&self) -> Option<String> {
        let env_name = self.inner.config.auth_token_env.as_deref()?;
        std::env::var(env_name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    }

    #[cfg(target_os = "macos")]
    fn macos_version_supported(&self) -> bool {
        let minimum = self
            .inner
            .config
            .minimum_macos_version
            .as_deref()
            .unwrap_or("14.0");
        self.macos_version_at_least(minimum)
    }

    #[cfg(target_os = "macos")]
    fn macos_version_at_least(&self, minimum: &str) -> bool {
        let Some(minimum) = parse_version(minimum) else {
            return false;
        };
        host_macos_version().is_some_and(|current| current >= minimum)
    }

    async fn health_with_token(&self, token: &str) -> Result<(), String> {
        let request = self.authorized(
            self.inner.client.get(format!(
                "{}/health",
                self.inner.endpoint.trim_end_matches('/')
            )),
            token,
        )?;
        let response = tokio::time::timeout(
            Duration::from_secs(self.inner.config.health_timeout_secs.max(1)),
            request.send(),
        )
        .await
        .map_err(|_| "FluidAudio health probe timed out".to_string())?
        .map_err(|error| format!("probing FluidAudio health: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("FluidAudio health returned {}", response.status()));
        }
        let health = response
            .json::<SidecarHealth>()
            .await
            .map_err(|error| format!("decoding FluidAudio health: {error}"))?;
        if health.status != "ok" || health.protocol_version != FLUID_AUDIO_PROTOCOL_VERSION {
            return Err(format!(
                "FluidAudio protocol mismatch: status={}, protocol={}",
                health.status, health.protocol_version
            ));
        }
        Ok(())
    }

    fn authorized(
        &self,
        request: reqwest::RequestBuilder,
        token: &str,
    ) -> Result<reqwest::RequestBuilder, String> {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|error| format!("building FluidAudio authorization header: {error}"))?;
        Ok(request
            .header(AUTHORIZATION, value)
            .header("x-magician-audio-protocol", FLUID_AUDIO_PROTOCOL_VERSION))
    }

    async fn record_error(&self, error: &str) {
        self.inner.state.lock().await.last_error = Some(error.to_string());
    }

    fn emit(&self, event_type: &str, payload: serde_json::Value) {
        if let Some(broadcaster) = self.inner.broadcaster.get() {
            broadcaster.emit_named(event_type, MEDIA_SYSTEM_AGENT, None, None, payload);
        }
    }
}

impl Inner {
    fn emit(&self, event_type: &str, payload: serde_json::Value) {
        if let Some(broadcaster) = self.broadcaster.get() {
            broadcaster.emit_named(event_type, MEDIA_SYSTEM_AGENT, None, None, payload);
        }
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// The host's macOS product version, read once per process.
///
/// `sw_vers` is a fork+exec — 5-20ms — and every call site sat directly on the
/// async runtime, including per-session-start. The answer cannot change while
/// the process is alive, so the first caller pays and no one else does.
/// `None` means the probe failed and every gate treats the host as unsupported,
/// exactly as a failed spawn did before.
#[cfg(target_os = "macos")]
fn host_macos_version() -> Option<(u64, u64, u64)> {
    static HOST_MACOS_VERSION: std::sync::OnceLock<Option<(u64, u64, u64)>> =
        std::sync::OnceLock::new();

    *HOST_MACOS_VERSION.get_or_init(|| {
        let output = std::process::Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        std::str::from_utf8(&output.stdout)
            .ok()
            .and_then(|value| parse_version(value.trim()))
    })
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::{parse_version, FluidAudioEngineManager};
    use crate::media_rails::{CachedTtsProvider, TtsError, TtsProvider, TtsRequest, TtsResponse};
    use async_trait::async_trait;
    use bytes::Bytes;
    use magician::config::MagicianMediaSettings;
    use std::sync::Arc;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct TestTts;

    #[async_trait]
    impl TtsProvider for TestTts {
        fn id(&self) -> &str {
            "fluid-test"
        }

        fn default_voice(&self) -> Option<&str> {
            Some("test")
        }

        fn default_model(&self) -> &str {
            "test"
        }

        async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
            Ok(TtsResponse {
                audio: Bytes::from_static(b"audio"),
                content_type: "audio/wav".to_string(),
                model: "test".to_string(),
                voice: Some("test".to_string()),
                message_id: request.message_id,
            })
        }
    }

    fn test_tts_request() -> TtsRequest {
        TtsRequest {
            text: "hello".to_string(),
            voice: None,
            rate: None,
            model: None,
            format: None,
            message_id: None,
            emotion: None,
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        }
    }

    #[test]
    fn parses_and_orders_macos_versions_numerically() {
        assert_eq!(parse_version("14"), Some((14, 0, 0)));
        assert_eq!(parse_version("14.6.1"), Some((14, 6, 1)));
        assert!(parse_version("15.0").unwrap() > parse_version("14.10").unwrap());
        assert_eq!(parse_version("14.beta"), None);
        assert_eq!(parse_version("14.0.0.1"), None);
    }

    #[test]
    fn recording_stt_bindings_are_forwarded_to_the_sidecar_model_inventory() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
    startup: lazy
    download_policy: on_demand
recording_stt:
  providers:
    - id: fluid-qwen3-asr-f32
      engine_id: fluid_audio
      adapter: fluid_audio_recording_stt
      model: FluidInference/qwen3-asr-0.6b-coreml
      variant: f32
      revision: main
      idle_secs: 42
"#,
        )
        .expect("settings");
        let manager = FluidAudioEngineManager::from_media_settings(
            &settings,
            std::path::PathBuf::from("/tmp/magician-config.yaml"),
        )
        .expect("manager")
        .expect("enabled engine");
        let model = manager.model("fluid-qwen3-asr-f32").expect("model");
        assert_eq!(model.adapter, "fluid_audio_recording_stt");
        assert_eq!(model.variant.as_deref(), Some("f32"));
        assert_eq!(model.idle_secs, 42);
    }

    #[test]
    fn configured_prewarm_policy_is_detected_without_starting_sidecar() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
    startup: lazy
    download_policy: prewarm
    prewarm: [fluid-silero-v6]
vad:
  providers:
    - id: fluid-silero-v6
      engine_id: fluid_audio
      adapter: fluid_audio_vad
      model: FluidInference/silero-vad-coreml
"#,
        )
        .expect("settings");
        let manager = FluidAudioEngineManager::from_media_settings(
            &settings,
            std::path::PathBuf::from("/tmp/magician-config.yaml"),
        )
        .expect("manager")
        .expect("enabled engine");

        assert!(manager.startup_prewarm_enabled());
        assert_eq!(manager.configured_prewarm_ids(), ["fluid-silero-v6"]);
    }

    #[test]
    fn tts_bindings_forward_configured_voice_and_format_inventory() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
    startup: lazy
    download_policy: on_demand
tts:
  providers:
    - id: fluid-kokoro-en
      adapter: fluid_audio_kokoro_tts
      model: FluidInference/kokoro-82m-coreml
      variant: 15s
      revision: main
      voice: af_heart
      voices: [af_heart, af_kore]
      format: wav
      formats: [wav]
      idle_secs: 77
"#,
        )
        .expect("settings");
        let manager = FluidAudioEngineManager::from_media_settings(
            &settings,
            std::path::PathBuf::from("/tmp/magician-config.yaml"),
        )
        .expect("manager")
        .expect("enabled engine");
        let model = manager.model("fluid-kokoro-en").expect("model");
        assert_eq!(model.adapter, "fluid_audio_kokoro_tts");
        assert_eq!(model.voice.as_deref(), Some("af_heart"));
        assert_eq!(model.voices, ["af_heart", "af_kore"]);
        assert_eq!(model.formats, ["wav"]);
        assert_eq!(model.idle_secs, 77);
    }

    #[tokio::test]
    async fn unloading_an_idle_model_does_not_start_the_sidecar() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
    startup: lazy
    download_policy: on_demand
vad:
  providers:
    - id: fluid-silero-v6
      engine_id: fluid_audio
      adapter: fluid_audio_vad
      model: FluidInference/silero-vad-coreml
"#,
        )
        .expect("settings");
        let manager = FluidAudioEngineManager::from_media_settings(
            &settings,
            std::path::PathBuf::from("/tmp/magician-config.yaml"),
        )
        .expect("manager")
        .expect("enabled engine");

        let state = manager
            .unload_model("fluid-silero-v6")
            .await
            .expect("idle unload");
        assert_eq!(state.state, "unloaded");
        assert!(!state.resident);
        assert_eq!(manager.status().await.start_count, 0);
    }

    #[tokio::test]
    async fn disabled_engine_remains_configured_but_cannot_load_models() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: false
    startup: lazy
    download_policy: prewarm
    prewarm: [fluid-silero-v6]
vad:
  providers:
    - id: fluid-silero-v6
      engine_id: fluid_audio
      adapter: fluid_audio_vad
      model: FluidInference/silero-vad-coreml
"#,
        )
        .expect("settings");
        let manager = FluidAudioEngineManager::from_media_settings(
            &settings,
            std::path::PathBuf::from("/tmp/magician-config.yaml"),
        )
        .expect("manager")
        .expect("configured engine");

        assert!(!manager.is_enabled());
        assert!(!manager.startup_prewarm_enabled());
        assert_eq!(
            manager
                .load_model("fluid-silero-v6")
                .await
                .expect_err("disabled engine must reject model loading"),
            "FluidAudio engine is disabled"
        );

        manager.set_enabled(true).await.expect("enable manager");
        assert!(manager.is_enabled());
        assert!(manager.startup_prewarm_enabled());

        manager.set_enabled(false).await.expect("disable manager");
        assert!(!manager.is_enabled());
        assert_eq!(manager.status().await.start_count, 0);
    }

    #[tokio::test]
    async fn disabling_invalidates_existing_operation_leases() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
    startup: external
    download_policy: on_demand
vad:
  providers:
    - id: fluid-silero-v6
      engine_id: fluid_audio
      adapter: fluid_audio_vad
      model: FluidInference/silero-vad-coreml
"#,
        )
        .expect("settings");
        let manager = FluidAudioEngineManager::from_media_settings(
            &settings,
            std::path::PathBuf::from("/tmp/magician-config.yaml"),
        )
        .expect("manager")
        .expect("configured engine");
        let lease = manager.begin_operation().expect("lease");

        manager.set_enabled(false).await.expect("disable manager");
        assert!(lease.ensure_current().is_err());
    }

    #[tokio::test]
    async fn disabling_clears_registered_fluid_audio_tts_caches() {
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
    startup: external
    download_policy: on_demand
vad:
  providers:
    - id: fluid-silero-v6
      engine_id: fluid_audio
      adapter: fluid_audio_vad
      model: FluidInference/silero-vad-coreml
"#,
        )
        .expect("settings");
        let manager = FluidAudioEngineManager::from_media_settings(
            &settings,
            std::path::PathBuf::from("/tmp/magician-config.yaml"),
        )
        .expect("manager")
        .expect("configured engine");
        let (enabled, generation) = manager.cache_gate();
        let cache = Arc::new(CachedTtsProvider::new_gated(
            Arc::new(TestTts),
            4,
            enabled,
            generation,
        ));
        manager.register_tts_cache(Arc::downgrade(&cache));
        cache
            .synthesize(test_tts_request())
            .await
            .expect("populate cache");
        assert_eq!(cache.cache_len().await, 1);

        manager.set_enabled(false).await.expect("disable manager");
        assert_eq!(cache.cache_len().await, 0);
    }

    #[tokio::test]
    async fn disabling_external_sidecar_unloads_idle_configured_models() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "fluid-silero-v6",
                    "state": "loaded",
                    "resident": true,
                    "active_sessions": 0
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/models/fluid-silero-v6/unload"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "fluid-silero-v6",
                "state": "unloaded",
                "resident": false,
                "active_sessions": 0
            })))
            .expect(1)
            .mount(&server)
            .await;
        let settings: MagicianMediaSettings = serde_yaml::from_str(&format!(
            r#"
engines:
  fluid_audio:
    enabled: true
    endpoint: {}
    startup: external
    download_policy: on_demand
vad:
  providers:
    - id: fluid-silero-v6
      engine_id: fluid_audio
      adapter: fluid_audio_vad
      model: FluidInference/silero-vad-coreml
"#,
            server.uri()
        ))
        .expect("settings");
        let manager = FluidAudioEngineManager::from_media_settings(
            &settings,
            std::path::PathBuf::from("/tmp/magician-config.yaml"),
        )
        .expect("manager")
        .expect("configured engine");
        {
            let mut state = manager.inner.state.lock().await;
            state.token = Some("test-token".to_string());
            state.owned = false;
        }

        manager.set_enabled(false).await.expect("disable manager");
        assert!(!manager.is_enabled());
    }

    #[tokio::test]
    async fn disable_does_not_wait_for_an_inflight_health_probe_state_lock() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(2))
                    .set_body_json(serde_json::json!({
                        "status": "ok",
                        "protocol_version": super::FLUID_AUDIO_PROTOCOL_VERSION
                    })),
            )
            .mount(&server)
            .await;
        let settings: MagicianMediaSettings = serde_yaml::from_str(&format!(
            r#"
engines:
  fluid_audio:
    enabled: true
    endpoint: {}
    startup: external
    download_policy: on_demand
    health_timeout_secs: 3
vad:
  providers:
    - id: fluid-silero-v6
      engine_id: fluid_audio
      adapter: fluid_audio_vad
      model: FluidInference/silero-vad-coreml
"#,
            server.uri()
        ))
        .expect("settings");
        let manager = Arc::new(
            FluidAudioEngineManager::from_media_settings(
                &settings,
                std::path::PathBuf::from("/tmp/magician-config.yaml"),
            )
            .expect("manager")
            .expect("configured engine"),
        );
        {
            let mut state = manager.inner.state.lock().await;
            state.token = Some("test-token".to_string());
        }
        let health_manager = Arc::clone(&manager);
        let probe = tokio::spawn(async move { health_manager.ensure_ready().await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        tokio::time::timeout(Duration::from_secs(1), manager.set_enabled(false))
            .await
            .expect("disable must not wait for the health probe")
            .expect("disable manager");
        assert!(probe.await.expect("probe task").is_err());
    }
}
