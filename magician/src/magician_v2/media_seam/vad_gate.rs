use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;

use super::providers::{
    AudioChunk, MediaProviderRegistry, StreamAudioFormat, StreamSampleFormat, StreamingSttEvent,
    StreamingSttProvider, StreamingSttSession, SttError, VadEvent, VadProvider, VadSession,
    VadSessionConfig,
};
use super::{
    AudioRuntimeConfigManager, AudioStage, AudioSurface, DiarizedStreamingSttProvider,
    FallbackStreamingSttProvider, MediaPreferences, MediaPreferencesStore, ResolvedAudioProfile,
    ResolvedAudioStage, StreamingSttFallbackState, StreamingSttFallbackTransition, VadMode,
    MEDIA_SYSTEM_AGENT,
};

pub const MEDIA_AUDIO_PROFILE_RESOLVED: &str = "media.audio.profile.resolved";
pub const MEDIA_AUDIO_PROFILE_DEGRADED: &str = "media.audio.profile.degraded";
pub const MEDIA_AUDIO_VAD_SPEECH_STARTED: &str = "media.audio.vad.speech_started";
pub const MEDIA_AUDIO_VAD_SPEECH_ENDED: &str = "media.audio.vad.speech_ended";
pub const MEDIA_AUDIO_PROVIDER_FALLBACK: &str = "media.audio.provider.fallback";
pub const MEDIA_AUDIO_FRAMES_DROPPED: &str = "media.audio.frames.dropped";

const COMMAND_CAPACITY: usize = 64;
const EVENT_CAPACITY: usize = 64;
const MAX_REPLAY_WINDOW_MS: u64 = 30_000;

/// Process services needed by capture paths that predate the media profile API.
/// Keeping this as one immutable boot-installed bundle avoids threading new
/// dependencies through every compiled meeting/screen capability signature.
#[derive(Clone)]
pub struct AudioPipelineServices {
    runtime: Arc<AudioRuntimeConfigManager>,
    providers: Arc<MediaProviderRegistry>,
    preferences: Arc<MediaPreferencesStore>,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
}

impl AudioPipelineServices {
    pub fn new(
        runtime: Arc<AudioRuntimeConfigManager>,
        providers: Arc<MediaProviderRegistry>,
        preferences: Arc<MediaPreferencesStore>,
        broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        Self {
            runtime,
            providers,
            preferences,
            broadcaster,
        }
    }
}

static AUDIO_PIPELINE_SERVICES: OnceLock<AudioPipelineServices> = OnceLock::new();

pub fn install_audio_pipeline_services(
    services: AudioPipelineServices,
) -> Result<(), &'static str> {
    AUDIO_PIPELINE_SERVICES
        .set(services)
        .map_err(|_| "audio pipeline services are already installed")
}

/// Resolve an installed capture surface from the canonical profile and stage
/// options. Production callers fail visibly if boot services are unavailable.
pub async fn resolve_installed_surface_audio_pipeline(
    surface: AudioSurface,
    scope: Option<(String, String)>,
    explicit_profile: Option<&str>,
    explicit_stage_options: &BTreeMap<AudioStage, String>,
    attach_diarization: bool,
) -> Result<(Arc<dyn StreamingSttProvider>, ResolvedAudioProfile), String> {
    let Some(services) = AUDIO_PIPELINE_SERVICES.get() else {
        return Err("audio pipeline services are not installed".to_string());
    };
    resolve_surface_audio_pipeline_with_overrides(
        services,
        surface,
        scope,
        explicit_profile,
        explicit_stage_options,
        attach_diarization,
    )
    .await
}

/// Resolve only the configured streaming-STT chain for a surface. Push-to-talk
/// owns its turn boundary explicitly, so applying a profile's VAD wrapper here
/// would let provider end-of-utterance events finalize a turn before release.
pub async fn resolve_installed_surface_streaming_stt_pipeline(
    surface: AudioSurface,
    scope: Option<(String, String)>,
    correlation_id: Option<String>,
) -> Result<(Arc<dyn StreamingSttProvider>, ResolvedAudioProfile), String> {
    let Some(services) = AUDIO_PIPELINE_SERVICES.get() else {
        return Err("audio pipeline services are not installed".to_string());
    };
    let (principal, workspace, resolved) =
        resolve_surface_profile(services, surface, scope, None, &BTreeMap::new()).await?;
    let mut streaming_chain = resolved
        .stages
        .get(&AudioStage::StreamingStt)
        .filter(|stage| stage.enabled)
        .map(|stage| ordered_streaming_providers(stage, &services.providers.streaming_stt_chain()))
        .unwrap_or_default();
    if streaming_chain.is_empty() {
        return Err(format!(
            "{surface} has no available configured streaming STT provider"
        ));
    }

    let telemetry = GateTelemetry::new(
        surface,
        principal,
        workspace,
        streaming_chain[0].id().to_string(),
        &resolved,
        Some(Arc::clone(&services.broadcaster)),
        correlation_id,
    );
    let pipeline = compose_streaming_stt_chain(&mut streaming_chain, &telemetry)?;
    telemetry.profile_resolved();
    Ok((pipeline, resolved))
}

/// Resolve a surface strictly from its configured provider chain.
pub async fn resolve_surface_audio_pipeline(
    services: &AudioPipelineServices,
    surface: AudioSurface,
    scope: Option<(String, String)>,
    attach_diarization: bool,
) -> Result<(Arc<dyn StreamingSttProvider>, ResolvedAudioProfile), String> {
    resolve_surface_audio_pipeline_with_overrides(
        services,
        surface,
        scope,
        None,
        &BTreeMap::new(),
        attach_diarization,
    )
    .await
}

async fn resolve_surface_audio_pipeline_with_overrides(
    services: &AudioPipelineServices,
    surface: AudioSurface,
    scope: Option<(String, String)>,
    explicit_profile: Option<&str>,
    explicit_stage_options: &BTreeMap<AudioStage, String>,
    attach_diarization: bool,
) -> Result<(Arc<dyn StreamingSttProvider>, ResolvedAudioProfile), String> {
    let (principal, workspace, resolved) = resolve_surface_profile(
        services,
        surface,
        scope,
        explicit_profile,
        explicit_stage_options,
    )
    .await?;
    compose_captured_surface_audio_pipeline(
        services,
        surface,
        principal,
        workspace,
        resolved,
        attach_diarization,
    )
}

/// Build a provider pipeline from the immutable profile captured when a media
/// session registered. Runtime preference changes must not alter an active
/// session; only a new registration resolves a new profile.
pub fn compose_captured_surface_audio_pipeline(
    services: &AudioPipelineServices,
    surface: AudioSurface,
    principal: String,
    workspace: String,
    resolved: ResolvedAudioProfile,
    attach_diarization: bool,
) -> Result<(Arc<dyn StreamingSttProvider>, ResolvedAudioProfile), String> {
    if resolved.surface != surface {
        return Err(format!(
            "captured {} audio profile cannot serve {surface}",
            resolved.surface
        ));
    }
    let streaming_chain = resolved
        .stages
        .get(&AudioStage::StreamingStt)
        .filter(|stage| stage.enabled)
        .map(|stage| ordered_streaming_providers(stage, &services.providers.streaming_stt_chain()))
        .unwrap_or_default();
    if streaming_chain.is_empty() {
        return Err(format!(
            "{surface} has no available configured streaming STT provider"
        ));
    }
    let pipeline = compose_resolved_audio_pipeline(
        services,
        surface,
        principal,
        workspace,
        resolved.clone(),
        streaming_chain,
        attach_diarization,
    )?;
    Ok((pipeline, resolved))
}

async fn resolve_surface_profile(
    services: &AudioPipelineServices,
    surface: AudioSurface,
    scope: Option<(String, String)>,
    explicit_profile: Option<&str>,
    explicit_stage_options: &BTreeMap<AudioStage, String>,
) -> Result<(String, String, ResolvedAudioProfile), String> {
    let (principal, workspace) = resolved_scope(surface, scope);
    let preferences = match services
        .preferences
        .load(&principal, &workspace, &services.runtime)
        .await
    {
        Ok(preferences) => preferences,
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            return Err(format!(
                "loading scoped {surface} audio preferences: {error}"
            ));
        },
        Err(error) => {
            warn!(
                surface = %surface,
                principal,
                workspace,
                error = %error,
                "failed to load scoped audio preferences; using configured defaults"
            );
            emit(
                &services.broadcaster,
                MEDIA_AUDIO_PROFILE_DEGRADED,
                &principal,
                &workspace,
                json!({
                    "surface": surface,
                    "reason": "scoped_preferences_unavailable",
                    "detail": error.to_string(),
                }),
            );
            MediaPreferences::default()
        },
    };
    let resolved = services
        .runtime
        .resolve(
            surface,
            &preferences,
            explicit_profile,
            explicit_stage_options,
        )
        .map_err(|error| format!("resolving {surface} audio profile: {error}"))?;
    Ok((principal, workspace, resolved))
}

fn compose_resolved_audio_pipeline(
    services: &AudioPipelineServices,
    surface: AudioSurface,
    principal: String,
    workspace: String,
    resolved: ResolvedAudioProfile,
    mut streaming_chain: Vec<Arc<dyn StreamingSttProvider>>,
    attach_diarization: bool,
) -> Result<Arc<dyn StreamingSttProvider>, String> {
    let telemetry = GateTelemetry::new(
        surface,
        principal,
        workspace,
        streaming_chain
            .first()
            .map_or("unavailable", |provider| provider.id())
            .to_string(),
        &resolved,
        Some(Arc::clone(&services.broadcaster)),
        None,
    );
    let inner = compose_streaming_stt_chain(&mut streaming_chain, &telemetry)?;

    let diarization_stage = if attach_diarization && !inner.capabilities().speaker_attribution {
        resolved
            .stages
            .get(&AudioStage::Diarization)
            .filter(|stage| stage.enabled)
    } else {
        None
    };
    let inner = if let Some(diarization_stage) = diarization_stage {
        let diarization = ordered_diarization_providers(
            diarization_stage,
            &services.providers.diarization_chain(),
        );
        if diarization.is_empty() {
            warn!(
                surface = %surface,
                profile_id = resolved.profile_id,
                "diarization is configured but unavailable; continuing without speaker labels"
            );
            inner
        } else {
            Arc::new(DiarizedStreamingSttProvider::new(inner, diarization))
                as Arc<dyn StreamingSttProvider>
        }
    } else {
        inner
    };

    telemetry.profile_resolved();

    let Some(vad_stage) = resolved.stages.get(&AudioStage::Vad) else {
        return Ok(inner);
    };
    if !vad_stage.enabled {
        return Ok(inner);
    }

    let registered = services.providers.vad_chain();
    let vad_chain = vad_stage
        .providers
        .iter()
        .filter_map(|option| {
            registered
                .iter()
                .find(|provider| provider.id().eq_ignore_ascii_case(&option.provider_id))
                .cloned()
        })
        .collect::<Vec<_>>();
    if vad_chain.is_empty() {
        let reason = vad_stage
            .degraded_reason
            .clone()
            .unwrap_or_else(|| "no configured VAD provider is available".to_string());
        telemetry.degraded(&reason, None);
        if vad_stage.required {
            return Err(format!("required {surface} VAD unavailable: {reason}"));
        }
        return Ok(inner);
    }

    Ok(Arc::new(VadGatedStreamingSttProvider::new(
        inner,
        vad_chain,
        vad_config(vad_stage),
        vad_stage.required,
        telemetry,
    )))
}

fn compose_streaming_stt_chain(
    streaming_chain: &mut Vec<Arc<dyn StreamingSttProvider>>,
    telemetry: &GateTelemetry,
) -> Result<Arc<dyn StreamingSttProvider>, String> {
    match streaming_chain.len() {
        0 => Err(format!(
            "{} streaming STT chain is empty",
            telemetry.surface
        )),
        1 => Ok(streaming_chain.remove(0)),
        _ => {
            let telemetry = telemetry.clone();
            let observer = Arc::new(move |transition: StreamingSttFallbackTransition| {
                telemetry.streaming_fallback(&transition);
            });
            Ok(Arc::new(FallbackStreamingSttProvider::with_observer(
                std::mem::take(streaming_chain),
                observer,
            )))
        },
    }
}

fn ordered_streaming_providers(
    stage: &ResolvedAudioStage,
    registered: &[Arc<dyn StreamingSttProvider>],
) -> Vec<Arc<dyn StreamingSttProvider>> {
    stage
        .providers
        .iter()
        .filter_map(|option| {
            registered
                .iter()
                .find(|provider| provider.id().eq_ignore_ascii_case(&option.provider_id))
                .cloned()
        })
        .collect()
}

fn ordered_diarization_providers(
    stage: &ResolvedAudioStage,
    registered: &[Arc<dyn super::providers::DiarizationProvider>],
) -> Vec<Arc<dyn super::providers::DiarizationProvider>> {
    stage
        .providers
        .iter()
        .filter_map(|option| {
            registered
                .iter()
                .find(|provider| provider.id().eq_ignore_ascii_case(&option.provider_id))
                .cloned()
        })
        .collect()
}

fn resolved_scope(surface: AudioSurface, scope: Option<(String, String)>) -> (String, String) {
    let (scope_principal, scope_workspace) = scope.unwrap_or_else(|| {
        (
            DEFAULT_SCOPE_PRINCIPAL.to_string(),
            DEFAULT_SCOPE_WORKSPACE.to_string(),
        )
    });
    if surface != AudioSurface::Meeting {
        return (scope_principal, scope_workspace);
    }
    let principal = nonempty_env("MEET_BOT_PRINCIPAL").unwrap_or(scope_principal);
    let workspace = nonempty_env("MEET_BOT_WORKSPACE").unwrap_or(scope_workspace);
    (principal, workspace)
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn vad_config(stage: &ResolvedAudioStage) -> VadSessionConfig {
    let defaults = VadSessionConfig::default();
    VadSessionConfig {
        threshold: stage.threshold.unwrap_or(defaults.threshold),
        min_speech_ms: stage.min_speech_ms.unwrap_or(defaults.min_speech_ms),
        min_silence_ms: stage.min_silence_ms.unwrap_or(defaults.min_silence_ms),
        pre_roll_ms: stage.pre_roll_ms.unwrap_or(defaults.pre_roll_ms),
        hangover_ms: stage.hangover_ms.unwrap_or(defaults.hangover_ms),
        max_utterance_ms: stage.max_utterance_ms.unwrap_or(defaults.max_utterance_ms),
        gate_only: stage.mode != Some(VadMode::TurnAuthority),
    }
}

#[derive(Clone)]
struct GateTelemetry {
    surface: AudioSurface,
    principal: String,
    workspace: String,
    profile_id: String,
    revision: String,
    stt_provider: String,
    correlation_id: Option<String>,
    broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
}

impl GateTelemetry {
    fn new(
        surface: AudioSurface,
        principal: String,
        workspace: String,
        stt_provider: String,
        profile: &ResolvedAudioProfile,
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        correlation_id: Option<String>,
    ) -> Self {
        Self {
            surface,
            principal,
            workspace,
            profile_id: profile.profile_id.clone(),
            revision: profile.revision.clone(),
            stt_provider,
            correlation_id,
            broadcaster,
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn test(surface: AudioSurface, stt_provider: impl Into<String>) -> Self {
        Self {
            surface,
            principal: "test-principal".to_string(),
            workspace: "test-workspace".to_string(),
            profile_id: "test-profile".to_string(),
            revision: "test-revision".to_string(),
            stt_provider: stt_provider.into(),
            correlation_id: None,
            broadcaster: None,
        }
    }

    fn profile_resolved(&self) {
        self.emit(
            MEDIA_AUDIO_PROFILE_RESOLVED,
            json!({
                "profile_id": self.profile_id,
                "revision": self.revision,
                "stt_provider": self.stt_provider,
            }),
        );
    }

    fn degraded(&self, reason: &str, provider: Option<&str>) {
        warn!(
            surface = %self.surface,
            profile_id = self.profile_id,
            vad_provider = provider,
            reason,
            "audio VAD gate degraded"
        );
        self.emit(
            MEDIA_AUDIO_PROFILE_DEGRADED,
            json!({
                "profile_id": self.profile_id,
                "revision": self.revision,
                "stt_provider": self.stt_provider,
                "vad_provider": provider,
                "reason": reason,
            }),
        );
    }

    fn fallback(&self, provider: &str, reason: &str, next_provider: Option<&str>) {
        self.emit(
            MEDIA_AUDIO_PROVIDER_FALLBACK,
            json!({
                "stage": "vad",
                "from_provider": provider,
                "to_provider": next_provider,
                "reason": reason,
            }),
        );
    }

    fn streaming_fallback(&self, transition: &StreamingSttFallbackTransition) {
        let state = match transition.state {
            StreamingSttFallbackState::ProviderFailed => "provider_failed",
            StreamingSttFallbackState::ProviderActivated => "provider_activated",
            StreamingSttFallbackState::Exhausted => "fallback_exhausted",
        };
        self.emit(
            MEDIA_AUDIO_PROVIDER_FALLBACK,
            json!({
                "stage": "streaming_stt",
                "state": state,
                "stream_session_id": transition.session_id,
                "from_provider": transition.from_provider,
                "to_provider": transition.to_provider,
                "generation": transition.generation,
                "replayed_chunks": transition.replayed_chunks,
                "error_class": transition.error_class,
            }),
        );
    }

    fn boundary(&self, event_type: &str, stream_id: &str, provider: &str, at_ms: u64) {
        self.emit(
            event_type,
            json!({
                "stream_id": stream_id,
                "vad_provider": provider,
                "at_ms": at_ms,
            }),
        );
    }

    fn dropped(&self, stream_id: &str, reason: &str, chunks: u64, duration_ms: u64) {
        if chunks == 0 {
            return;
        }
        self.emit(
            MEDIA_AUDIO_FRAMES_DROPPED,
            json!({
                "stream_id": stream_id,
                "reason": reason,
                "chunks": chunks,
                "duration_ms": duration_ms,
            }),
        );
    }

    fn emit(&self, event_type: &str, mut payload: serde_json::Value) {
        let Some(broadcaster) = self.broadcaster.as_ref() else {
            return;
        };
        if let Some(object) = payload.as_object_mut() {
            object.insert("surface".to_string(), json!(self.surface));
            if let Some(correlation_id) = self.correlation_id.as_deref() {
                object.insert("voice_session_id".to_string(), json!(correlation_id));
            }
        }
        emit(
            broadcaster,
            event_type,
            &self.principal,
            &self.workspace,
            payload,
        );
    }
}

fn emit(
    broadcaster: &RuntimeTransportBroadcaster,
    event_type: &str,
    principal: &str,
    workspace: &str,
    payload: serde_json::Value,
) {
    broadcaster.emit_named(
        event_type,
        MEDIA_SYSTEM_AGENT,
        Some(principal),
        Some(workspace),
        payload,
    );
}

struct VadGatedStreamingSttProvider {
    inner: Arc<dyn StreamingSttProvider>,
    vad_chain: Vec<Arc<dyn VadProvider>>,
    config: VadSessionConfig,
    required: bool,
    telemetry: GateTelemetry,
}

impl VadGatedStreamingSttProvider {
    fn new(
        inner: Arc<dyn StreamingSttProvider>,
        vad_chain: Vec<Arc<dyn VadProvider>>,
        config: VadSessionConfig,
        required: bool,
        telemetry: GateTelemetry,
    ) -> Self {
        Self {
            inner,
            vad_chain,
            config,
            required,
            telemetry,
        }
    }
}

#[async_trait]
impl StreamingSttProvider for VadGatedStreamingSttProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn label(&self) -> Option<&str> {
        self.inner.label()
    }

    fn default_model(&self) -> &str {
        self.inner.default_model()
    }

    fn capabilities(&self) -> super::providers::StreamingSttCapabilities {
        self.inner.capabilities()
    }

    async fn open_session(
        &self,
        format: StreamAudioFormat,
        events: mpsc::Sender<StreamingSttEvent>,
    ) -> Result<Box<dyn StreamingSttSession>, SttError> {
        validate_format(format)?;
        let initial_stt = self.inner.open_session(format, events.clone()).await?;
        let (vad_events_tx, vad_events_rx) = mpsc::channel(EVENT_CAPACITY);
        let mut remaining = VecDeque::from(self.vad_chain.clone());
        let mut failures = Vec::new();
        let mut opened = None;

        while let Some(provider) = remaining.pop_front() {
            match provider
                .open_session(format, self.config.clone(), vad_events_tx.clone())
                .await
            {
                Ok(session) => {
                    opened = Some((provider, session));
                    break;
                },
                Err(error) => {
                    let reason = error.to_string();
                    let next = remaining.front().map(|item| item.id());
                    self.telemetry.fallback(provider.id(), &reason, next);
                    failures.push(format!("{}: {reason}", provider.id()));
                },
            }
        }

        let Some((vad_provider, vad_session)) = opened else {
            let reason = format!(
                "all configured VAD providers failed: {}",
                failures.join("; ")
            );
            self.telemetry.degraded(&reason, None);
            if self.required {
                let _ = initial_stt.finish().await;
                return Err(SttError::NotConfigured(reason));
            }
            return Ok(initial_stt);
        };

        let stream_id = format!("audio-{}", uuid::Uuid::new_v4().simple());
        info!(
            surface = %self.telemetry.surface,
            profile_id = self.telemetry.profile_id,
            vad_provider = vad_provider.id(),
            stt_provider = self.inner.id(),
            stream_id,
            "VAD-gated streaming STT session opened"
        );
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (finish_tx, finish_rx) = mpsc::channel(8);
        let finish_worker = tokio::spawn(run_stt_finalizer(finish_rx));
        tokio::spawn(run_gate_worker(GateWorker {
            format,
            config: self.config.clone(),
            required: self.required,
            inner: Arc::clone(&self.inner),
            stt_events: events,
            current_stt: Some(initial_stt),
            vad_provider,
            vad_session,
            vad_events_tx,
            vad_events_rx,
            remaining_vad: remaining,
            commands: receiver,
            telemetry: self.telemetry.clone(),
            stream_id,
            speaking: false,
            fail_open: false,
            cursor_ms: 0,
            replay: VecDeque::new(),
            finish_tx: Some(finish_tx),
            finish_worker: Some(finish_worker),
            pending_finish: None,
            last_seq: None,
            evicted_chunks: 0,
            evicted_ms: 0,
        }));
        Ok(Box::new(VadGatedStreamingSttSession { commands }))
    }
}

fn validate_format(format: StreamAudioFormat) -> Result<(), SttError> {
    if format.sample_rate_hz == 0 || format.channels == 0 {
        return Err(SttError::BadRequest(
            "streaming audio format requires a positive sample rate and channel count".to_string(),
        ));
    }
    Ok(())
}

struct VadGatedStreamingSttSession {
    commands: mpsc::Sender<GateCommand>,
}

enum GateCommand {
    Push(AudioChunk, oneshot::Sender<Result<(), SttError>>),
    Finish(oneshot::Sender<Result<(), SttError>>),
}

#[async_trait]
impl StreamingSttSession for VadGatedStreamingSttSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(GateCommand::Push(chunk, reply))
            .await
            .map_err(|_| SttError::Transport("VAD gate worker stopped".to_string()))?;
        result
            .await
            .map_err(|_| SttError::Transport("VAD gate worker dropped a push".to_string()))?
    }

    async fn finish(&self) -> Result<(), SttError> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(GateCommand::Finish(reply))
            .await
            .map_err(|_| SttError::Transport("VAD gate worker stopped".to_string()))?;
        result
            .await
            .map_err(|_| SttError::Transport("VAD gate worker dropped finish".to_string()))?
    }
}

#[derive(Clone)]
struct TimedChunk {
    chunk: AudioChunk,
    start_ms: u64,
    end_ms: u64,
}

struct GateWorker {
    format: StreamAudioFormat,
    config: VadSessionConfig,
    required: bool,
    inner: Arc<dyn StreamingSttProvider>,
    stt_events: mpsc::Sender<StreamingSttEvent>,
    current_stt: Option<Box<dyn StreamingSttSession>>,
    vad_provider: Arc<dyn VadProvider>,
    vad_session: Box<dyn VadSession>,
    vad_events_tx: mpsc::Sender<VadEvent>,
    vad_events_rx: mpsc::Receiver<VadEvent>,
    remaining_vad: VecDeque<Arc<dyn VadProvider>>,
    commands: mpsc::Receiver<GateCommand>,
    telemetry: GateTelemetry,
    stream_id: String,
    speaking: bool,
    fail_open: bool,
    cursor_ms: u64,
    replay: VecDeque<TimedChunk>,
    finish_tx: Option<mpsc::Sender<SttFinishRequest>>,
    finish_worker: Option<tokio::task::JoinHandle<Result<(), SttError>>>,
    /// Completion barrier for the previous segmented STT session. Some local
    /// engines permit only one session per model; opening the next utterance
    /// before `finish()` has released the prior engine lease deterministically
    /// fails with `model has active sessions`.
    pending_finish: Option<oneshot::Receiver<Result<(), String>>>,
    last_seq: Option<u64>,
    evicted_chunks: u64,
    evicted_ms: u64,
}

async fn run_gate_worker(mut worker: GateWorker) {
    while let Some(command) = worker.commands.recv().await {
        match command {
            GateCommand::Push(chunk, reply) => {
                let result = worker.push(chunk).await;
                let stop = result.is_err() && worker.required;
                let _ = reply.send(result);
                if stop {
                    let _ = worker.finish().await;
                    return;
                }
            },
            GateCommand::Finish(reply) => {
                let _ = reply.send(worker.finish().await);
                return;
            },
        }
    }
    let _ = worker.finish().await;
}

impl GateWorker {
    async fn push(&mut self, chunk: AudioChunk) -> Result<(), SttError> {
        self.record_sequence_gap(chunk.seq);
        let duration_ms = chunk_duration_ms(self.format, chunk.pcm.len());
        let timed = TimedChunk {
            chunk: chunk.clone(),
            start_ms: self.cursor_ms,
            end_ms: self.cursor_ms.saturating_add(duration_ms),
        };
        self.cursor_ms = timed.end_ms;

        if self.fail_open {
            return self.forward(chunk).await;
        }

        if self.speaking {
            self.forward(chunk.clone()).await?;
        } else {
            self.replay.push_back(timed);
            self.trim_replay();
        }

        if let Err(error) = self.vad_session.push_audio(chunk).await {
            self.handle_vad_failure(error.to_string()).await?;
        }
        self.drain_vad_events().await
    }

    fn record_sequence_gap(&mut self, seq: u64) {
        if let Some(previous) = self.last_seq {
            if seq > previous.saturating_add(1) {
                self.telemetry.dropped(
                    &self.stream_id,
                    "capture_sequence_gap",
                    seq - previous - 1,
                    0,
                );
            }
        }
        self.last_seq = Some(seq);
    }

    fn trim_replay(&mut self) {
        let retention_ms = self
            .config
            .pre_roll_ms
            .saturating_add(self.config.min_speech_ms)
            .saturating_add(1_000)
            .clamp(1_000, MAX_REPLAY_WINDOW_MS);
        let cutoff = self.cursor_ms.saturating_sub(retention_ms);
        while self
            .replay
            .front()
            .is_some_and(|chunk| chunk.end_ms <= cutoff)
        {
            if let Some(chunk) = self.replay.pop_front() {
                self.evicted_chunks += 1;
                self.evicted_ms += chunk.end_ms.saturating_sub(chunk.start_ms);
            }
        }
    }

    async fn drain_vad_events(&mut self) -> Result<(), SttError> {
        while let Ok(event) = self.vad_events_rx.try_recv() {
            self.handle_vad_event(event).await?;
        }
        Ok(())
    }

    async fn handle_vad_event(&mut self, event: VadEvent) -> Result<(), SttError> {
        match event {
            VadEvent::Probability { .. } => {},
            VadEvent::SpeechStarted { at_ms } => {
                if self.speaking || self.fail_open {
                    return Ok(());
                }
                self.speaking = true;
                self.telemetry.boundary(
                    MEDIA_AUDIO_VAD_SPEECH_STARTED,
                    &self.stream_id,
                    self.vad_provider.id(),
                    at_ms,
                );
                let cutoff = at_ms.saturating_sub(self.config.pre_roll_ms);
                while self
                    .replay
                    .front()
                    .is_some_and(|chunk| chunk.end_ms <= cutoff)
                {
                    if let Some(chunk) = self.replay.pop_front() {
                        self.evicted_chunks += 1;
                        self.evicted_ms += chunk.end_ms.saturating_sub(chunk.start_ms);
                    }
                }
                while let Some(chunk) = self.replay.pop_front() {
                    self.forward(chunk.chunk).await?;
                }
                self.flush_eviction_telemetry();
            },
            VadEvent::SpeechEnded { at_ms } => {
                if !self.speaking || self.fail_open {
                    return Ok(());
                }
                self.speaking = false;
                self.telemetry.boundary(
                    MEDIA_AUDIO_VAD_SPEECH_ENDED,
                    &self.stream_id,
                    self.vad_provider.id(),
                    at_ms,
                );
                self.finalize_current().await?;
            },
        }
        Ok(())
    }

    async fn forward(&mut self, chunk: AudioChunk) -> Result<(), SttError> {
        self.ensure_current().await?;
        let session = self.current_stt.as_ref().ok_or_else(|| {
            SttError::Transport("VAD gate lost its active STT session".to_string())
        })?;
        session.push_audio(chunk).await?;
        Ok(())
    }

    async fn ensure_current(&mut self) -> Result<(), SttError> {
        if self.current_stt.is_none() {
            self.await_pending_finish().await?;
            self.current_stt = Some(
                self.inner
                    .open_session(self.format, self.stt_events.clone())
                    .await?,
            );
        }
        Ok(())
    }

    async fn finalize_current(&mut self) -> Result<(), SttError> {
        let Some(session) = self.current_stt.take() else {
            return Ok(());
        };
        self.await_pending_finish().await?;
        let (completed_tx, completed_rx) = oneshot::channel();
        self.finish_tx
            .as_ref()
            .ok_or_else(|| SttError::Transport("STT finalizer already stopped".to_string()))?
            .send(SttFinishRequest {
                session,
                completed: completed_tx,
            })
            .await
            .map_err(|_| SttError::Transport("STT finalizer stopped".to_string()))?;
        self.pending_finish = Some(completed_rx);
        Ok(())
    }

    async fn await_pending_finish(&mut self) -> Result<(), SttError> {
        let Some(completed) = self.pending_finish.take() else {
            return Ok(());
        };
        completed
            .await
            .map_err(|_| SttError::Transport("STT finalizer completion dropped".to_string()))?
            .map_err(SttError::Transport)
    }

    async fn handle_vad_failure(&mut self, reason: String) -> Result<(), SttError> {
        let failed_provider = self.vad_provider.id().to_string();
        let _ = self.vad_session.finish().await;
        self.drain_vad_events().await?;
        if self.speaking {
            self.speaking = false;
            self.finalize_current().await?;
        }
        while self.vad_events_rx.try_recv().is_ok() {}

        while let Some(provider) = self.remaining_vad.pop_front() {
            let next_after = self.remaining_vad.front().map(|item| item.id());
            match provider
                .open_session(self.format, self.config.clone(), self.vad_events_tx.clone())
                .await
            {
                Ok(session) => {
                    self.telemetry
                        .fallback(&failed_provider, &reason, Some(provider.id()));
                    self.vad_provider = provider;
                    self.vad_session = session;
                    return Ok(());
                },
                Err(error) => {
                    self.telemetry
                        .fallback(provider.id(), &error.to_string(), next_after)
                },
            }
        }

        self.telemetry.degraded(
            &format!("runtime VAD failure: {reason}"),
            Some(&failed_provider),
        );
        if self.required {
            return Err(SttError::Transport(format!(
                "required VAD provider {failed_provider} failed: {reason}"
            )));
        }
        self.fail_open = true;
        while let Some(chunk) = self.replay.pop_front() {
            self.forward(chunk.chunk).await?;
        }
        self.flush_eviction_telemetry();
        Ok(())
    }

    fn flush_eviction_telemetry(&mut self) {
        self.telemetry.dropped(
            &self.stream_id,
            "vad_replay_window_eviction",
            self.evicted_chunks,
            self.evicted_ms,
        );
        self.evicted_chunks = 0;
        self.evicted_ms = 0;
    }

    async fn finish(&mut self) -> Result<(), SttError> {
        let mut first_error = None;
        if !self.fail_open {
            if let Err(error) = self.vad_session.finish().await {
                self.telemetry.degraded(
                    &format!("VAD finish failed: {error}"),
                    Some(self.vad_provider.id()),
                );
                if self.required {
                    first_error = Some(SttError::Transport(error.to_string()));
                }
            }
            if let Err(error) = self.drain_vad_events().await {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        self.replay.clear();
        self.flush_eviction_telemetry();
        if let Err(error) = self.finalize_current().await {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        if let Err(error) = self.await_pending_finish().await {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        self.finish_tx.take();
        let finalizer_result = match self.finish_worker.take() {
            Some(worker) => match worker.await {
                Ok(result) => result,
                Err(error) => Err(SttError::Transport(format!(
                    "joining segmented STT finalizer: {error}"
                ))),
            },
            None => Ok(()),
        };
        if let Err(error) = finalizer_result {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

struct SttFinishRequest {
    session: Box<dyn StreamingSttSession>,
    completed: oneshot::Sender<Result<(), String>>,
}

async fn run_stt_finalizer(mut sessions: mpsc::Receiver<SttFinishRequest>) -> Result<(), SttError> {
    while let Some(request) = sessions.recv().await {
        let result = request.session.finish().await;
        let completion = result.as_ref().map(|_| ()).map_err(ToString::to_string);
        let _ = request.completed.send(completion);
        result?;
    }
    Ok(())
}

fn chunk_duration_ms(format: StreamAudioFormat, bytes: usize) -> u64 {
    let sample_bytes = match format.sample_format {
        StreamSampleFormat::PcmS16Le => 2_u64,
        StreamSampleFormat::PcmF32Le => 4_u64,
    };
    let bytes_per_second = u64::from(format.sample_rate_hz)
        .saturating_mul(u64::from(format.channels))
        .saturating_mul(sample_bytes);
    if bytes == 0 || bytes_per_second == 0 {
        return 0;
    }
    (bytes as u64)
        .saturating_mul(1_000)
        .saturating_add(bytes_per_second - 1)
        / bytes_per_second
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::Mutex as StdMutex;

    use bytes::Bytes;

    use super::*;
    use crate::magician_v2::media_seam::providers::{
        StreamingSttCapabilities, VadCapabilities, VadError,
    };

    #[derive(Default)]
    struct RecordingSttState {
        sessions: Vec<Vec<u64>>,
        finishes: usize,
        active_sessions: usize,
    }

    struct RecordingSttProvider {
        state: Arc<StdMutex<RecordingSttState>>,
    }

    #[async_trait]
    impl StreamingSttProvider for RecordingSttProvider {
        fn id(&self) -> &str {
            "recording-stt"
        }

        fn capabilities(&self) -> StreamingSttCapabilities {
            StreamingSttCapabilities {
                end_of_utterance: true,
                ..StreamingSttCapabilities::default()
            }
        }

        async fn open_session(
            &self,
            _format: StreamAudioFormat,
            _events: mpsc::Sender<StreamingSttEvent>,
        ) -> Result<Box<dyn StreamingSttSession>, SttError> {
            let index = {
                let mut state = self.state.lock().expect("state");
                if state.active_sessions > 0 {
                    return Err(SttError::Transport(
                        "model has active sessions: recording-stt".to_string(),
                    ));
                }
                state.active_sessions += 1;
                let index = state.sessions.len();
                state.sessions.push(Vec::new());
                index
            };
            Ok(Box::new(RecordingSttSession {
                state: Arc::clone(&self.state),
                index,
            }))
        }
    }

    struct RecordingSttSession {
        state: Arc<StdMutex<RecordingSttState>>,
        index: usize,
    }

    #[async_trait]
    impl StreamingSttSession for RecordingSttSession {
        async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
            self.state.lock().expect("state").sessions[self.index].push(chunk.seq);
            Ok(())
        }

        async fn finish(&self) -> Result<(), SttError> {
            // Keep teardown observably asynchronous so a missing reopen
            // barrier deterministically reproduces the single-session engine
            // failure instead of depending on scheduler luck.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let mut state = self.state.lock().expect("state");
            state.finishes += 1;
            state.active_sessions = state.active_sessions.saturating_sub(1);
            Ok(())
        }
    }

    struct ScriptedVadProvider {
        start_seq: Option<u64>,
        end_seq: Option<u64>,
        fail_open: bool,
        fail_at_seq: Option<u64>,
    }

    #[async_trait]
    impl VadProvider for ScriptedVadProvider {
        fn id(&self) -> &str {
            "scripted-vad"
        }

        fn capabilities(&self) -> VadCapabilities {
            VadCapabilities {
                probability_events: true,
                configurable_threshold: true,
            }
        }

        async fn open_session(
            &self,
            _format: StreamAudioFormat,
            _config: VadSessionConfig,
            events: mpsc::Sender<VadEvent>,
        ) -> Result<Box<dyn VadSession>, VadError> {
            if self.fail_open {
                return Err(VadError::Unavailable("scripted open failure".to_string()));
            }
            Ok(Box::new(ScriptedVadSession {
                start_seq: self.start_seq,
                end_seq: self.end_seq,
                fail_at_seq: self.fail_at_seq,
                events,
            }))
        }
    }

    struct ScriptedVadSession {
        start_seq: Option<u64>,
        end_seq: Option<u64>,
        fail_at_seq: Option<u64>,
        events: mpsc::Sender<VadEvent>,
    }

    #[async_trait]
    impl VadSession for ScriptedVadSession {
        async fn push_audio(&self, chunk: AudioChunk) -> Result<(), VadError> {
            if self.fail_at_seq == Some(chunk.seq) {
                return Err(VadError::Session("scripted runtime failure".to_string()));
            }
            if self.start_seq == Some(chunk.seq) {
                self.events
                    .send(VadEvent::SpeechStarted {
                        at_ms: chunk.seq * 100,
                    })
                    .await
                    .map_err(|_| VadError::Session("event receiver closed".to_string()))?;
            }
            if self.end_seq == Some(chunk.seq) {
                self.events
                    .send(VadEvent::SpeechEnded {
                        at_ms: chunk.seq * 100 + 100,
                    })
                    .await
                    .map_err(|_| VadError::Session("event receiver closed".to_string()))?;
            }
            Ok(())
        }

        async fn finish(&self) -> Result<(), VadError> {
            Ok(())
        }
    }

    fn chunk(seq: u64) -> AudioChunk {
        AudioChunk {
            seq,
            pcm: Bytes::from(vec![0_u8; 3_200]),
        }
    }

    fn gate(
        vad: Arc<dyn VadProvider>,
        required: bool,
    ) -> (
        Arc<StdMutex<RecordingSttState>>,
        VadGatedStreamingSttProvider,
    ) {
        let state = Arc::new(StdMutex::new(RecordingSttState::default()));
        let inner: Arc<dyn StreamingSttProvider> = Arc::new(RecordingSttProvider {
            state: Arc::clone(&state),
        });
        let mut config = VadSessionConfig::default();
        config.pre_roll_ms = 100;
        config.min_speech_ms = 100;
        (
            state,
            VadGatedStreamingSttProvider::new(
                inner,
                vec![vad],
                config,
                required,
                GateTelemetry::test(AudioSurface::Listening, "recording-stt"),
            ),
        )
    }

    #[tokio::test]
    async fn silence_never_reaches_stt() {
        let vad = Arc::new(ScriptedVadProvider {
            start_seq: None,
            end_seq: None,
            fail_open: false,
            fail_at_seq: None,
        });
        let (state, provider) = gate(vad, false);
        let (events, _receiver) = mpsc::channel(4);
        let session = provider
            .open_session(StreamAudioFormat::default(), events)
            .await
            .expect("open");
        for seq in 0..8 {
            session.push_audio(chunk(seq)).await.expect("push");
        }
        session.finish().await.expect("finish");

        let state = state.lock().expect("state");
        assert_eq!(state.sessions, vec![Vec::<u64>::new()]);
        assert_eq!(state.finishes, 1);
    }

    #[tokio::test]
    async fn speech_is_bounded_and_includes_pre_roll() {
        let vad = Arc::new(ScriptedVadProvider {
            start_seq: Some(2),
            end_seq: Some(4),
            fail_open: false,
            fail_at_seq: None,
        });
        let (state, provider) = gate(vad, false);
        let (events, _receiver) = mpsc::channel(4);
        let session = provider
            .open_session(StreamAudioFormat::default(), events)
            .await
            .expect("open");
        for seq in 0..6 {
            session.push_audio(chunk(seq)).await.expect("push");
        }
        session.finish().await.expect("finish");

        let state = state.lock().expect("state");
        assert_eq!(state.sessions[0], vec![1, 2, 3, 4]);
        assert_eq!(state.finishes, 1);
    }

    #[tokio::test]
    async fn separate_utterances_use_separate_ordered_stt_sessions() {
        struct TwoTurnVadProvider;
        struct TwoTurnVadSession {
            events: mpsc::Sender<VadEvent>,
        }

        #[async_trait]
        impl VadProvider for TwoTurnVadProvider {
            fn id(&self) -> &str {
                "two-turn-vad"
            }

            async fn open_session(
                &self,
                _format: StreamAudioFormat,
                _config: VadSessionConfig,
                events: mpsc::Sender<VadEvent>,
            ) -> Result<Box<dyn VadSession>, VadError> {
                Ok(Box::new(TwoTurnVadSession { events }))
            }
        }

        #[async_trait]
        impl VadSession for TwoTurnVadSession {
            async fn push_audio(&self, chunk: AudioChunk) -> Result<(), VadError> {
                let event = match chunk.seq {
                    1 | 4 => Some(VadEvent::SpeechStarted {
                        at_ms: chunk.seq * 100,
                    }),
                    2 | 5 => Some(VadEvent::SpeechEnded {
                        at_ms: chunk.seq * 100 + 100,
                    }),
                    _ => None,
                };
                if let Some(event) = event {
                    self.events
                        .send(event)
                        .await
                        .map_err(|_| VadError::Session("event receiver closed".to_string()))?;
                }
                Ok(())
            }

            async fn finish(&self) -> Result<(), VadError> {
                Ok(())
            }
        }

        let (state, provider) = gate(Arc::new(TwoTurnVadProvider), false);
        let (events, _receiver) = mpsc::channel(4);
        let session = provider
            .open_session(StreamAudioFormat::default(), events)
            .await
            .expect("open");
        for seq in 0..6 {
            session.push_audio(chunk(seq)).await.expect("push");
        }
        session.finish().await.expect("finish");

        let state = state.lock().expect("state");
        assert_eq!(state.sessions, vec![vec![0, 1, 2], vec![3, 4, 5]]);
        assert_eq!(state.finishes, 2);
        assert_eq!(state.active_sessions, 0);
    }

    #[tokio::test]
    async fn optional_open_failure_fails_open_without_losing_audio() {
        let vad = Arc::new(ScriptedVadProvider {
            start_seq: None,
            end_seq: None,
            fail_open: true,
            fail_at_seq: None,
        });
        let (state, provider) = gate(vad, false);
        let (events, _receiver) = mpsc::channel(4);
        let session = provider
            .open_session(StreamAudioFormat::default(), events)
            .await
            .expect("optional VAD must fail open");
        session.push_audio(chunk(0)).await.expect("push");
        session.push_audio(chunk(1)).await.expect("push");
        session.finish().await.expect("finish");

        let state = state.lock().expect("state");
        assert_eq!(state.sessions[0], vec![0, 1]);
    }

    #[tokio::test]
    async fn optional_runtime_failure_replays_buffer_then_stays_open() {
        let vad = Arc::new(ScriptedVadProvider {
            start_seq: None,
            end_seq: None,
            fail_open: false,
            fail_at_seq: Some(2),
        });
        let (state, provider) = gate(vad, false);
        let (events, _receiver) = mpsc::channel(4);
        let session = provider
            .open_session(StreamAudioFormat::default(), events)
            .await
            .expect("open");
        for seq in 0..4 {
            session.push_audio(chunk(seq)).await.expect("push");
        }
        session.finish().await.expect("finish");

        let state = state.lock().expect("state");
        assert_eq!(state.sessions[0], vec![0, 1, 2, 3]);
    }

    #[tokio::test]
    async fn required_open_failure_rejects_the_session() {
        let vad = Arc::new(ScriptedVadProvider {
            start_seq: None,
            end_seq: None,
            fail_open: true,
            fail_at_seq: None,
        });
        let (state, provider) = gate(vad, true);
        let (events, _receiver) = mpsc::channel(4);
        let result = provider
            .open_session(StreamAudioFormat::default(), events)
            .await;
        assert!(result.is_err());
        assert_eq!(state.lock().expect("state").finishes, 1);
    }

    #[test]
    fn chunk_duration_handles_pcm16_and_float() {
        assert_eq!(chunk_duration_ms(StreamAudioFormat::default(), 3_200), 100);
        assert_eq!(
            chunk_duration_ms(
                StreamAudioFormat {
                    sample_rate_hz: 48_000,
                    channels: 2,
                    sample_format: StreamSampleFormat::PcmF32Le,
                },
                38_400,
            ),
            100
        );
    }
}
