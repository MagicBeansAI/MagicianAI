//! Gemini Live API realtime provider.
//!
//! Gemini Live is WebSocket-only for the backend path we use here:
//! Magician keeps the API key server-side, receives 24 kHz mono PCM16
//! from the browser/native voice client, forwards it to Gemini with an
//! explicit `audio/pcm;rate=24000` MIME type, and forwards Gemini's
//! 24 kHz PCM16 output straight back over the existing backend-proxied
//! audio channel.
//!
//! One provider serves every Gemini Live model family, and the wire contract
//! differs by family (see [`GeminiLiveModelContract`]). The 3.8 models keep
//! talking while a `NON_BLOCKING` function call runs and report whether the
//! interaction is still open through `serverContent.interactionStatus`;
//! `turnComplete` there only closes one utterance. 3.1 Flash Live blocks on
//! every tool call and has no interaction status. This is not the GPT-Live-1
//! mouth/brain protocol: Gemini reasons inside the audio model, and Magician
//! is the brain only because the voice catalog advertises `delegate_to_chat`.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use dashmap::DashMap;
use futures_util::{Sink, SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};
use ulid::Ulid;

use crate::{
    config::RealtimeVoiceMode,
    providers::gemini::sanitize_gemini_schema,
    realtime::{
        provider::RealtimeProvider,
        types::{
            AudioStreamChannel, RealtimeAudioControl, RealtimeAudioTopology, RealtimeProviderError,
            RealtimeProviderEvent, RealtimeProviderKind, RealtimeSessionDescriptor,
        },
    },
    types::LLMToolSpec,
};

pub const GEMINI_LIVE_PROVIDER_ID: &str = "gemini-live";
/// Google's GA default for low-latency voice agents. The 3.1 Flash Live
/// preview stays a configurable profile; it is no longer the code fallback.
pub const GEMINI_LIVE_DEFAULT_MODEL: &str = "gemini-3.8-live";
pub const GEMINI_LIVE_DEFAULT_WEBSOCKET_URL: &str = "wss://generativelanguage.googleapis.com/ws/\
                                                     google.ai.generativelanguage.v1beta.\
                                                     GenerativeService.BidiGenerateContent";
pub const GEMINI_LIVE_DEFAULT_VOICE: &str = "Kore";

const GEMINI_LIVE_MAX_CONNECTION_SECS: u64 = 9 * 60;
const GEMINI_LIVE_CONTEXT_WINDOW_TOKENS: u64 = 128_000;
const GEMINI_LIVE_PCM_RATE: u32 = 24_000;
const AUDIO_CHANNEL_CAPACITY: usize = 64;
const CONTROL_CHANNEL_CAPACITY: usize = 16;
const EVENT_CHANNEL_CAPACITY: usize = 64;
const PENDING_AFTER_SETUP_MAX_BYTES: usize = 1024 * 1024;
const GEMINI_UPSTREAM_SESSION_PREFIX: &str = "gemini-live:";

/// Published wire-contract differences between Gemini Live model families.
///
/// Derived from the model id, never from a profile flag, so an operator cannot
/// pair a model with a payload shape Google closes the socket on (1007
/// "Unknown name"). Anything this table does not recognise gets the 3.1
/// Flash Live contract, which every family accepts as the conservative
/// baseline except that a model requiring `NON_BLOCKING` would refuse
/// blocking declarations — add a row before shipping such a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeminiLiveModelContract {
    /// Function declarations carry `behavior: NON_BLOCKING` and function
    /// responses carry `scheduling`; the model keeps speaking while the tool
    /// runs. 3.8 Live defaults to this, 3.8 Live Extended Thinking accepts
    /// nothing else, and 3.1 Flash Live rejects it.
    pub async_function_calling: bool,
    /// `generationConfig.thinkingConfig.thinkingLevel` is accepted. Only the
    /// extended-thinking model; 3.8 Live rejects the field outright.
    pub thinking_level: bool,
    /// `serverContent.interactionStatus` (`IN_PROGRESS` / `IDLE`) is the idle
    /// signal and `turnComplete` only closes one utterance. Measured on the
    /// wire 2026-09-17: the extended-thinking model sends it on every
    /// `turnComplete`; plain 3.8 Live sent none even with a non-blocking call
    /// open (it waited for the result instead of speaking first). The parser
    /// honours the field whenever it appears, so this flag only says what to
    /// expect.
    pub interaction_status: bool,
}

impl GeminiLiveModelContract {
    /// 3.1 Flash Live and the translate models: blocking tools, no thinking
    /// knob, `turnComplete` means idle.
    pub const LEGACY: Self = Self {
        async_function_calling: false,
        thinking_level: false,
        interaction_status: false,
    };
    /// `gemini-3.8-live`: interleaved reasoning without a level knob (its
    /// `thoughtsTokenCount` is billed all the same), no interaction status
    /// observed.
    pub const LIVE_3_8: Self = Self {
        async_function_calling: true,
        thinking_level: false,
        interaction_status: false,
    };
    /// `gemini-3.8-live-extended-thinking`.
    pub const LIVE_3_8_EXTENDED_THINKING: Self = Self {
        async_function_calling: true,
        thinking_level: true,
        interaction_status: true,
    };
}

/// Resolve the wire contract for a Gemini Live model id. Accepts the bare id
/// or the `models/` resource path, any case.
pub fn gemini_live_model_contract(model: &str) -> GeminiLiveModelContract {
    let id = model
        .trim()
        .strip_prefix("models/")
        .unwrap_or(model.trim())
        .to_ascii_lowercase();
    if id.starts_with("gemini-3.8-live-extended-thinking") {
        GeminiLiveModelContract::LIVE_3_8_EXTENDED_THINKING
    } else if id.starts_with("gemini-3.8-live") {
        GeminiLiveModelContract::LIVE_3_8
    } else {
        GeminiLiveModelContract::LEGACY
    }
}

/// Background reasoning depth for extended-thinking Live models. Google does
/// not accept `MINIMAL` on the Live API, so it is not representable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiThinkingLevel {
    Low,
    Medium,
    High,
}

impl GeminiThinkingLevel {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    fn wire(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }
}

/// When a non-blocking function result should reach the user's ears.
/// `WhenIdle` lets the model finish its "let me check…" before it speaks the
/// answer, which is why it is the default: a result that lands mid-sentence
/// would otherwise clip the acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GeminiToolResultScheduling {
    #[default]
    WhenIdle,
    Interrupt,
    Silent,
}

impl GeminiToolResultScheduling {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "when_idle" => Some(Self::WhenIdle),
            "interrupt" => Some(Self::Interrupt),
            "silent" => Some(Self::Silent),
            _ => None,
        }
    }

    fn wire(self) -> &'static str {
        match self {
            Self::WhenIdle => "WHEN_IDLE",
            Self::Interrupt => "INTERRUPT",
            Self::Silent => "SILENT",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeminiLiveProvider {
    api_key: String,
    websocket_url: String,
    default_model: String,
    default_voice: Option<String>,
    turn_detection_mode: Option<String>,
    context_window_tokens: Option<u64>,
    max_session_duration_secs: Option<u64>,
    mode: RealtimeVoiceMode,
    translation_target_language: Option<String>,
    translation_echo_target_language: bool,
    thinking_level: Option<GeminiThinkingLevel>,
    tool_result_scheduling: GeminiToolResultScheduling,
    resume_handles_by_voice_session: Arc<DashMap<String, String>>,
    /// The names of the function calls still in flight, per voice session.
    /// Per voice session, not per connection: a Gemini Live session cannot
    /// change its tool catalog, so loading a deferred tool reconnects it, and
    /// the result of the call that triggered the reconnect is delivered to the
    /// new connection — which must still answer with the function's name.
    pending_tool_names_by_voice_session: Arc<DashMap<String, Arc<Mutex<HashMap<String, String>>>>>,
    sessions: Arc<DashMap<String, mpsc::Sender<RealtimeAudioControl>>>,
}

impl GeminiLiveProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            websocket_url: GEMINI_LIVE_DEFAULT_WEBSOCKET_URL.to_string(),
            default_model: GEMINI_LIVE_DEFAULT_MODEL.to_string(),
            default_voice: Some(GEMINI_LIVE_DEFAULT_VOICE.to_string()),
            turn_detection_mode: None,
            context_window_tokens: Some(GEMINI_LIVE_CONTEXT_WINDOW_TOKENS),
            max_session_duration_secs: Some(GEMINI_LIVE_MAX_CONNECTION_SECS),
            mode: RealtimeVoiceMode::Assistant,
            translation_target_language: None,
            translation_echo_target_language: false,
            thinking_level: None,
            tool_result_scheduling: GeminiToolResultScheduling::default(),
            resume_handles_by_voice_session: Arc::new(DashMap::new()),
            pending_tool_names_by_voice_session: Arc::new(DashMap::new()),
            sessions: Arc::new(DashMap::new()),
        }
    }

    /// The in-flight function-call names for a voice session, shared by every
    /// connection that serves it. See the field.
    fn pending_tool_names_for(
        &self,
        voice_session_id: &str,
    ) -> Arc<Mutex<HashMap<String, String>>> {
        Arc::clone(
            self.pending_tool_names_by_voice_session
                .entry(voice_session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(HashMap::new())))
                .value(),
        )
    }

    /// Reasoning depth and non-blocking result delivery, already validated by
    /// the factory against [`gemini_live_model_contract`]. The provider trusts
    /// the pairing and still omits `thinkingConfig` on models whose contract
    /// has no level knob, so a stale pairing degrades to the model's default
    /// instead of an upstream close.
    pub fn with_live_options(
        mut self,
        thinking_level: Option<GeminiThinkingLevel>,
        tool_result_scheduling: GeminiToolResultScheduling,
    ) -> Self {
        self.thinking_level = thinking_level;
        self.tool_result_scheduling = tool_result_scheduling;
        self
    }

    pub fn with_websocket_url(mut self, websocket_url: impl Into<String>) -> Self {
        self.websocket_url = websocket_url.into();
        self
    }

    pub fn with_defaults(
        mut self,
        model: impl Into<String>,
        voice: Option<impl Into<String>>,
    ) -> Self {
        self.default_model = model.into();
        self.default_voice = voice.map(Into::into);
        self
    }

    pub fn with_profile_overrides(
        mut self,
        turn_detection_mode: Option<String>,
        context_window_tokens: Option<u64>,
        max_session_duration_secs: Option<u64>,
    ) -> Self {
        self.turn_detection_mode = turn_detection_mode;
        self.context_window_tokens = context_window_tokens.or(self.context_window_tokens);
        self.max_session_duration_secs =
            max_session_duration_secs.or(self.max_session_duration_secs);
        self
    }

    pub fn with_mode(
        mut self,
        mode: RealtimeVoiceMode,
        translation_target_language: Option<String>,
        translation_echo_target_language: bool,
    ) -> Self {
        self.mode = mode;
        self.translation_target_language = translation_target_language;
        self.translation_echo_target_language = translation_echo_target_language;
        self
    }
}

#[async_trait]
impl RealtimeProvider for GeminiLiveProvider {
    fn id(&self) -> &str {
        GEMINI_LIVE_PROVIDER_ID
    }

    fn kind(&self) -> RealtimeProviderKind {
        RealtimeProviderKind::Gemini
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn audio_topology(&self) -> RealtimeAudioTopology {
        RealtimeAudioTopology::BackendProxied
    }

    async fn create_session(
        &self,
        _principal: &str,
        _workspace: &str,
        voice_session_id: &str,
        _thread_id: Option<&str>,
        preferred_voice: Option<&str>,
    ) -> Result<RealtimeSessionDescriptor, RealtimeProviderError> {
        Ok(RealtimeSessionDescriptor {
            provider: RealtimeProviderKind::Gemini,
            model: self.default_model.clone(),
            topology: RealtimeAudioTopology::BackendProxied,
            mode: self.mode,
            voice: preferred_voice
                .and_then(|value| crate::realtime::canonical_realtime_voice("gemini_live", value))
                .or_else(|| self.default_voice.clone()),
            webrtc_url: None,
            upstream_token: None,
            upstream_provider_session_id: Some(gemini_upstream_session_id(voice_session_id)),
            max_session_duration_secs: self.max_session_duration_secs,
            native_resume_handle: self
                .resume_handles_by_voice_session
                .get(voice_session_id)
                .map(|entry| entry.value().clone()),
            transcription_model: None,
            transcription_fallback_model: None,
            turn_detection_mode: self.turn_detection_mode.clone(),
            context_window_tokens: self.context_window_tokens,
            half_duplex: None,
        })
    }

    fn supports_native_resume(&self) -> bool {
        true
    }

    fn max_session_duration_secs(&self) -> Option<u64> {
        self.max_session_duration_secs
    }

    async fn open_proxied_audio(
        &self,
        descriptor: &RealtimeSessionDescriptor,
    ) -> Result<AudioStreamChannel, RealtimeProviderError> {
        let upstream_session_id = descriptor
            .upstream_provider_session_id
            .clone()
            .unwrap_or_else(|| format!("gemini-live:unknown:{}", Ulid::new()));
        let voice_session_id = gemini_voice_session_id_from_upstream(&upstream_session_id)
            .unwrap_or_else(|| "unknown".to_string());

        let (upstream_tx, upstream_rx) = mpsc::channel::<Vec<u8>>(AUDIO_CHANNEL_CAPACITY);
        let (downstream_tx, downstream_rx) = mpsc::channel::<Vec<u8>>(AUDIO_CHANNEL_CAPACITY);
        let (control_tx, control_rx) =
            mpsc::channel::<RealtimeAudioControl>(CONTROL_CHANNEL_CAPACITY);
        let (events_tx, events_rx) = mpsc::channel::<RealtimeProviderEvent>(EVENT_CHANNEL_CAPACITY);

        self.sessions
            .insert(upstream_session_id.clone(), control_tx.clone());

        let task = GeminiLiveTask {
            api_key: self.api_key.clone(),
            websocket_url: self.websocket_url.clone(),
            contract: gemini_live_model_contract(&descriptor.model),
            model: descriptor.model.clone(),
            voice: descriptor
                .voice
                .clone()
                .or_else(|| self.default_voice.clone()),
            turn_detection_mode: descriptor
                .turn_detection_mode
                .clone()
                .or_else(|| self.turn_detection_mode.clone())
                .unwrap_or_else(|| "server_vad".to_string()),
            native_resume_handle: descriptor.native_resume_handle.clone(),
            upstream_session_id: upstream_session_id.clone(),
            pending_tool_names: self.pending_tool_names_for(&voice_session_id),
            voice_session_id,
            resume_handles_by_voice_session: Arc::clone(&self.resume_handles_by_voice_session),
            sessions: Arc::clone(&self.sessions),
            mode: self.mode,
            translation_target_language: self.translation_target_language.clone(),
            translation_echo_target_language: self.translation_echo_target_language,
            thinking_level: self.thinking_level,
            tool_result_scheduling: self.tool_result_scheduling,
        };
        let cleanup_sessions = Arc::clone(&self.sessions);
        let cleanup_session_id = upstream_session_id.clone();
        tokio::spawn(async move {
            if let Err(error) = task
                .run(upstream_rx, downstream_tx, control_rx, events_tx.clone())
                .await
            {
                let _ = events_tx
                    .send(RealtimeProviderEvent::TransportClosed {
                        message: error.to_string(),
                    })
                    .await;
            }
            cleanup_sessions.remove(&cleanup_session_id);
        });

        Ok(AudioStreamChannel {
            upstream_tx,
            downstream_rx,
            control_tx,
            events_rx,
        })
    }

    async fn close_session(
        &self,
        descriptor: &RealtimeSessionDescriptor,
    ) -> Result<(), RealtimeProviderError> {
        let Some(session_id) = descriptor.upstream_provider_session_id.as_ref() else {
            return Ok(());
        };
        if let Some((_, tx)) = self.sessions.remove(session_id) {
            let _ = tx.send(RealtimeAudioControl::End).await;
        }
        Ok(())
    }

    fn compaction_token_watermark(&self) -> f32 {
        0.7
    }
}

struct GeminiLiveTask {
    api_key: String,
    websocket_url: String,
    model: String,
    /// Wire contract for `model`; fixed for the life of the socket.
    contract: GeminiLiveModelContract,
    voice: Option<String>,
    turn_detection_mode: String,
    native_resume_handle: Option<String>,
    upstream_session_id: String,
    voice_session_id: String,
    pending_tool_names: Arc<Mutex<HashMap<String, String>>>,
    resume_handles_by_voice_session: Arc<DashMap<String, String>>,
    sessions: Arc<DashMap<String, mpsc::Sender<RealtimeAudioControl>>>,
    mode: RealtimeVoiceMode,
    translation_target_language: Option<String>,
    translation_echo_target_language: bool,
    thinking_level: Option<GeminiThinkingLevel>,
    tool_result_scheduling: GeminiToolResultScheduling,
}

#[derive(Debug)]
enum PendingAfterSetup {
    Audio(Vec<u8>),
    Control(RealtimeAudioControl),
}

impl PendingAfterSetup {
    fn audio_len(&self) -> usize {
        match self {
            Self::Audio(frame) => frame.len(),
            Self::Control(_) => 0,
        }
    }
}

impl GeminiLiveTask {
    async fn run(
        self,
        mut upstream_rx: mpsc::Receiver<Vec<u8>>,
        downstream_tx: mpsc::Sender<Vec<u8>>,
        mut control_rx: mpsc::Receiver<RealtimeAudioControl>,
        events_tx: mpsc::Sender<RealtimeProviderEvent>,
    ) -> Result<(), RealtimeProviderError> {
        let url = gemini_live_ws_url(&self.websocket_url, &self.api_key);
        info!(
            "[REALTIME-VOICE] opening backend-proxied Gemini Live WebSocket url={} model={} \
             voice={}",
            redact_gemini_key(&url),
            self.model,
            self.voice.as_deref().unwrap_or("<provider-default>")
        );
        let (ws, _) = connect_async(url)
            .await
            .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
        let _ = events_tx.send(RealtimeProviderEvent::TransportReady).await;
        let (mut write, mut read) = ws.split();

        let mut setup_sent = false;
        let mut setup_complete = false;
        let manual_activity = self.turn_detection_mode.trim().eq_ignore_ascii_case("none");
        let pending_tool_names = Arc::clone(&self.pending_tool_names);
        let mut pending_after_setup = VecDeque::new();
        let mut pending_after_setup_bytes = 0usize;
        let mut event_state = GeminiLiveEventState::default();

        loop {
            tokio::select! {
                biased;
                maybe_control = control_rx.recv() => {
                    let Some(control) = maybe_control else { break; };
                    match control {
                        RealtimeAudioControl::ConfigureSession {
                            instructions,
                            tools,
                            input_transcription_model: _,
                            update_id,
                            defer_response_until_context: _,
                        } => {
                            if setup_sent {
                                if let Some(update_id) = update_id {
                                    let _ = events_tx
                                        .send(RealtimeProviderEvent::SessionConfigurationUnsupported {
                                            update_id,
                                        })
                                        .await;
                                }
                                continue;
                            }
                            let payload = self.configure_session_payload(&instructions, &tools, manual_activity);
                            write.send(Message::Text(payload.to_string())).await
                                .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            setup_sent = true;
                        },
                        RealtimeAudioControl::End => {
                            let _ = write.send(Message::Close(None)).await;
                            break;
                        },
                        control if !setup_complete => {
                            queue_pending_after_setup(
                                &mut pending_after_setup,
                                &mut pending_after_setup_bytes,
                                PendingAfterSetup::Control(control),
                            );
                        },
                        control => {
                            self.send_control(
                                &mut write,
                                control,
                                manual_activity,
                                &mut upstream_rx,
                                &events_tx,
                                &pending_tool_names,
                            ).await?;
                        },
                    }
                },
                maybe_frame = upstream_rx.recv() => {
                    let Some(frame) = maybe_frame else { break; };
                    if setup_complete {
                        send_gemini_audio(&mut write, frame).await?;
                    } else {
                        queue_pending_after_setup(
                            &mut pending_after_setup,
                            &mut pending_after_setup_bytes,
                            PendingAfterSetup::Audio(frame),
                        );
                    }
                },
                maybe_msg = read.next() => {
                    let Some(message) = maybe_msg else {
                        return Err(RealtimeProviderError::Upstream(
                            "Gemini Live connection ended without a close frame".to_string(),
                        ));
                    };
                    let outcome = match message {
                        Ok(Message::Text(text)) => Some(
                            handle_gemini_live_server_event(
                                &text,
                                &downstream_tx,
                                &events_tx,
                                &mut event_state,
                                &self.resume_handles_by_voice_session,
                                &self.voice_session_id,
                                &pending_tool_names,
                            )
                            .await?,
                        ),
                        Ok(Message::Binary(bytes)) => Some(
                            handle_gemini_live_server_binary_event(
                                &bytes,
                                &downstream_tx,
                                &events_tx,
                                &mut event_state,
                                &self.resume_handles_by_voice_session,
                                &self.voice_session_id,
                                &pending_tool_names,
                            )
                            .await?,
                        ),
                        Ok(Message::Close(frame)) => {
                            return Err(RealtimeProviderError::Upstream(format!(
                                "Gemini Live connection closed{}",
                                frame
                                    .map(|frame| format!(" ({}: {})", frame.code, frame.reason))
                                    .unwrap_or_default()
                            )));
                        },
                        Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => None,
                        Err(e) => return Err(RealtimeProviderError::Upstream(e.to_string())),
                    };
                    if outcome.is_some_and(|outcome| outcome.setup_complete) && !setup_complete {
                        setup_complete = true;
                        let _ = events_tx
                            .send(RealtimeProviderEvent::SessionConfigured { update_id: None })
                            .await;
                        flush_pending_after_setup(
                            &self,
                            &mut write,
                            &mut pending_after_setup,
                            &mut pending_after_setup_bytes,
                            manual_activity,
                            &mut upstream_rx,
                            &events_tx,
                            &pending_tool_names,
                        )
                        .await?;
                    }
                },
                else => break,
            }
        }

        self.sessions.remove(&self.upstream_session_id);
        Ok(())
    }

    fn configure_session_payload(
        &self,
        instructions: &str,
        tools: &[LLMToolSpec],
        manual_activity: bool,
    ) -> Value {
        if self.mode == RealtimeVoiceMode::Translation {
            let target_language = self
                .translation_target_language
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("en");
            // Google's Live Translate *guide* nests transcription inside
            // generationConfig. The v1beta WebSocket rejects that with 1007
            // (`Unknown name "inputAudioTranscription" at 'setup.generation_config'`).
            // Transcription belongs on `setup`; `translationConfig` stays in
            // generationConfig. Same split as assistant Live.
            let mut setup = json!({
                "model": gemini_model_path(&self.model),
                "generationConfig": {
                    "responseModalities": ["AUDIO"],
                    "translationConfig": {
                        "targetLanguageCode": target_language,
                        "echoTargetLanguage": self.translation_echo_target_language
                    }
                },
                "inputAudioTranscription": {},
                "outputAudioTranscription": {},
                "sessionResumption": {}
            });
            if let Some(handle) = self
                .native_resume_handle
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                setup["sessionResumption"] = json!({ "handle": handle });
            }
            return json!({ "setup": setup });
        }

        let mut setup = json!({
            "model": gemini_model_path(&self.model),
            "generationConfig": {
                "responseModalities": ["AUDIO"]
            },
            "systemInstruction": {
                "parts": [{ "text": instructions }]
            },
            "inputAudioTranscription": {},
            "outputAudioTranscription": {},
            "realtimeInputConfig": {
                "automaticActivityDetection": {
                    "disabled": manual_activity
                }
            },
            "sessionResumption": {},
            "contextWindowCompression": {
                "slidingWindow": {}
            },
            "historyConfig": {
                "initialHistoryInClientContent": true
            }
        });

        if let Some(voice) = self.voice.as_deref().filter(|v| !v.trim().is_empty()) {
            setup["generationConfig"]["speechConfig"] = json!({
                "voiceConfig": {
                    "prebuiltVoiceConfig": {
                        "voiceName": voice
                    }
                }
            });
        }

        if let Some(handle) = self
            .native_resume_handle
            .as_deref()
            .filter(|v| !v.trim().is_empty())
        {
            setup["sessionResumption"] = json!({ "handle": handle });
        }

        // Only the extended-thinking model accepts a level; 3.8 Live rejects
        // the field, and 3.1 never had it. Omitting it on the extended model
        // leaves Google's default depth in charge.
        if let Some(level) = self.thinking_level.filter(|_| self.contract.thinking_level) {
            setup["generationConfig"]["thinkingConfig"] = json!({
                "thinkingLevel": level.wire()
            });
        }

        let mapped_tools = gemini_live_tools(tools, self.contract.async_function_calling);
        if !mapped_tools.is_empty() {
            setup["tools"] = Value::Array(mapped_tools);
        }

        json!({ "setup": setup })
    }

    async fn send_control<W>(
        &self,
        write: &mut W,
        control: RealtimeAudioControl,
        manual_activity: bool,
        upstream_rx: &mut mpsc::Receiver<Vec<u8>>,
        events_tx: &mpsc::Sender<RealtimeProviderEvent>,
        pending_tool_names: &Arc<Mutex<HashMap<String, String>>>,
    ) -> Result<(), RealtimeProviderError>
    where
        W: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    {
        match control {
            RealtimeAudioControl::ConfigureSession { .. } => {},
            RealtimeAudioControl::ClearInput | RealtimeAudioControl::InterruptResponse => {
                if manual_activity {
                    send_json(write, json!({ "realtimeInput": { "activityStart": {} } })).await?;
                    let _ = events_tx.send(RealtimeProviderEvent::SpeechStarted).await;
                }
            },
            RealtimeAudioControl::CommitInputAndRespond => {
                while let Ok(frame) = upstream_rx.try_recv() {
                    send_gemini_audio(write, frame).await?;
                }
                if manual_activity {
                    send_json(write, json!({ "realtimeInput": { "activityEnd": {} } })).await?;
                } else {
                    send_json(
                        write,
                        json!({ "realtimeInput": { "audioStreamEnd": true } }),
                    )
                    .await?;
                }
                let _ = events_tx.send(RealtimeProviderEvent::SpeechStopped).await;
            },
            RealtimeAudioControl::ToolResult { call_id, output } => {
                let name = pending_tool_names
                    .lock()
                    .remove(&call_id)
                    .unwrap_or_else(|| call_id.clone());
                let scheduling = self
                    .contract
                    .async_function_calling
                    .then_some(self.tool_result_scheduling);
                send_json(
                    write,
                    gemini_tool_response_payload(&call_id, &name, &output, scheduling),
                )
                .await?;
            },
            RealtimeAudioControl::InjectInitialHistory { text } => {
                if self.mode == RealtimeVoiceMode::Assistant {
                    send_json(write, gemini_initial_history_payload(&text)).await?;
                }
            },
            RealtimeAudioControl::InjectToolExchange {
                call_id,
                tool_name,
                arguments: _,
                projected_result: _,
            } => {
                // Gemini Live has no silent native history insertion for a
                // balanced historical function-call/result pair after setup.
                // Never downgrade projected evidence to an ordinary client
                // text turn: that changes provider role semantics, loses the
                // protocol pairing invariant, and lets data-looking text be
                // interpreted as a fresh user instruction. Magician filters
                // this control by provider; keep the transport fail-closed as
                // defense in depth for any future caller.
                warn!(
                    call_id,
                    tool_name, "Gemini Live ignored unsupported historical tool-exchange replay"
                );
            },
            RealtimeAudioControl::InjectSystemMessage {
                text,
                request_response,
            } => {
                if self.mode == RealtimeVoiceMode::Translation {
                    warn!(
                        "[REALTIME-VOICE] ignoring assistant message injection in Gemini Live \
                         translation mode"
                    );
                } else if request_response {
                    send_json(write, json!({ "realtimeInput": { "text": text } })).await?;
                } else {
                    warn!(
                        "[REALTIME-VOICE] Gemini Live does not support silent runtime text \
                         injection; dropping injected text"
                    );
                }
            },
            RealtimeAudioControl::RespondWithTurnContext { .. } => {
                warn!(
                    "[REALTIME-VOICE] Gemini Live received a turn-context response gate even \
                     though the provider does not advertise support"
                );
            },
            RealtimeAudioControl::SynthesizeResponse { .. } => {},
            RealtimeAudioControl::End => {
                let _ = write.send(Message::Close(None)).await;
            },
        }
        Ok(())
    }
}

async fn flush_pending_after_setup<W>(
    task: &GeminiLiveTask,
    write: &mut W,
    pending_after_setup: &mut VecDeque<PendingAfterSetup>,
    pending_after_setup_bytes: &mut usize,
    manual_activity: bool,
    upstream_rx: &mut mpsc::Receiver<Vec<u8>>,
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    pending_tool_names: &Arc<Mutex<HashMap<String, String>>>,
) -> Result<(), RealtimeProviderError>
where
    W: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    while let Some(frame) = pending_after_setup.pop_front() {
        *pending_after_setup_bytes = (*pending_after_setup_bytes).saturating_sub(frame.audio_len());
        match frame {
            PendingAfterSetup::Audio(frame) => send_gemini_audio(write, frame).await?,
            PendingAfterSetup::Control(control) => {
                task.send_control(
                    write,
                    control,
                    manual_activity,
                    upstream_rx,
                    events_tx,
                    pending_tool_names,
                )
                .await?;
            },
        }
    }
    Ok(())
}

fn queue_pending_after_setup(
    pending_after_setup: &mut VecDeque<PendingAfterSetup>,
    pending_after_setup_bytes: &mut usize,
    frame: PendingAfterSetup,
) {
    *pending_after_setup_bytes += frame.audio_len();
    pending_after_setup.push_back(frame);
    while *pending_after_setup_bytes > PENDING_AFTER_SETUP_MAX_BYTES {
        let Some(dropped) = pending_after_setup.pop_front() else {
            break;
        };
        *pending_after_setup_bytes =
            (*pending_after_setup_bytes).saturating_sub(dropped.audio_len());
        warn!(
            pending_audio_bytes = *pending_after_setup_bytes,
            "[REALTIME-VOICE] trimmed Gemini Live pre-setup frame buffer"
        );
    }
}

async fn send_json<W>(write: &mut W, value: Value) -> Result<(), RealtimeProviderError>
where
    W: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    write
        .send(Message::Text(value.to_string()))
        .await
        .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
    Ok(())
}

async fn send_gemini_audio<W>(write: &mut W, frame: Vec<u8>) -> Result<(), RealtimeProviderError>
where
    W: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    send_json(write, gemini_audio_payload(frame)).await
}

fn gemini_audio_payload(frame: Vec<u8>) -> Value {
    json!({
        "realtimeInput": {
            "audio": {
                "data": BASE64_STANDARD.encode(frame),
                "mimeType": format!("audio/pcm;rate={GEMINI_LIVE_PCM_RATE}")
            }
        }
    })
}

fn gemini_initial_history_payload(text: &str) -> Value {
    json!({
        "clientContent": {
            "turns": [{
                "role": "user",
                "parts": [{ "text": text }]
            }],
            "turnComplete": false
        }
    })
}

/// How many in-flight function calls one voice session may await. A call
/// whose result never arrives must not accumulate: this map holds the turn's
/// outstanding calls, not the session's history.
const MAX_PENDING_TOOL_NAMES: usize = 32;

/// Record the function name a call id is awaiting, bounded. At the ceiling the
/// oldest id is dropped — the newest call is the one still in flight.
fn remember_pending_tool_name(
    pending: &Arc<Mutex<HashMap<String, String>>>,
    call_id: String,
    name: String,
) {
    let mut pending = pending.lock();
    if pending.len() >= MAX_PENDING_TOOL_NAMES && !pending.contains_key(&call_id) {
        if let Some(oldest) = pending.keys().next().cloned() {
            pending.remove(&oldest);
        }
    }
    pending.insert(call_id, name);
}

/// One `toolResponse` for a finished function call. `scheduling` is the
/// non-blocking delivery hint and is only sent on models whose contract has
/// async function calling: 3.1 Flash Live does not know the field.
fn gemini_tool_response_payload(
    call_id: &str,
    name: &str,
    output: &str,
    scheduling: Option<GeminiToolResultScheduling>,
) -> Value {
    let mut response = json!({ "output": output });
    if let Some(scheduling) = scheduling {
        response["scheduling"] = Value::String(scheduling.wire().to_string());
    }
    json!({
        "toolResponse": {
            "functionResponses": [{
                "id": call_id,
                "name": name,
                "response": response
            }]
        }
    })
}

#[derive(Default)]
struct GeminiLiveEventState {
    response_seq: u64,
    user_seq: u64,
    current_response_id: Option<String>,
    current_user_item_id: Option<String>,
    user_transcript: String,
    assistant_transcript: String,
    user_final_emitted: bool,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    /// Per-modality split for realtime cost. `None` until Gemini sends a
    /// `usageMetadata` detail array; see `parse_realtime_usage`.
    realtime_usage: Option<crate::types::RealtimeUsage>,
    response_done_emitted: bool,
    audio_started: bool,
    /// Last `serverContent.interactionStatus` seen. Spans generations — a
    /// non-blocking tool call leaves the interaction `IN_PROGRESS` across the
    /// utterance boundary — so `reset_turn` leaves it alone. `None` until the
    /// model sends the field (3.1 never does).
    interaction_in_progress: Option<bool>,
}

impl GeminiLiveEventState {
    fn response_id(&mut self) -> String {
        match self.current_response_id.as_ref() {
            Some(id) => id.clone(),
            None => {
                self.response_seq = self.response_seq.saturating_add(1);
                let id = format!("gemini-response-{}", self.response_seq);
                self.current_response_id = Some(id.clone());
                id
            },
        }
    }

    fn next_user_item_id(&mut self) -> String {
        self.user_seq = self.user_seq.saturating_add(1);
        format!("gemini-user-{}", self.user_seq)
    }

    fn user_item_id(&mut self) -> String {
        if let Some(id) = self.current_user_item_id.as_ref() {
            return id.clone();
        }
        let id = self.next_user_item_id();
        self.current_user_item_id = Some(id.clone());
        id
    }

    fn reset_turn(&mut self) {
        self.current_response_id = None;
        self.current_user_item_id = None;
        self.user_transcript.clear();
        self.assistant_transcript.clear();
        self.user_final_emitted = false;
        self.input_tokens = None;
        self.output_tokens = None;
        self.realtime_usage = None;
        self.response_done_emitted = false;
        self.audio_started = false;
    }
}

/// Sum a Gemini `usageMetadata` per-modality detail array into `(text, audio)`.
///
/// Live reports these as `[{modality, tokenCount}]`. Field casing differs
/// across Google's surfaces (REST/WebSocket JSON is camelCase, the Python SDK
/// exposes snake_case), so both spellings are accepted rather than silently
/// yielding zero. Modalities other than TEXT/AUDIO (e.g. IMAGE, VIDEO) are not
/// billable on a voice turn and are skipped.
fn modality_token_counts(usage: &Value, camel: &str, snake: &str) -> Option<(u64, u64)> {
    let entries = usage.get(camel).or_else(|| usage.get(snake))?.as_array()?;
    let mut text = 0_u64;
    let mut audio = 0_u64;
    for entry in entries {
        let count = entry
            .get("tokenCount")
            .or_else(|| entry.get("token_count"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        match entry
            .get("modality")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_uppercase()
            .as_str()
        {
            "AUDIO" => audio = audio.saturating_add(count),
            "TEXT" => text = text.saturating_add(count),
            _ => {},
        }
    }
    Some((text, audio))
}

/// Build a [`RealtimeUsage`] from Gemini Live's `usageMetadata`.
///
/// Voice cost is dominated by the audio/text split — audio input bills at 4x
/// text input on `gemini-3.1-flash-live`, audio output at ~2.7x text output —
/// so a split has to come from the provider, never from the coarse totals.
/// When neither detail array is present this returns `None` and the caller
/// keeps the totals without a fabricated split, matching the OpenAI parser.
///
/// Cached buckets are zero because Google publishes no cached-input tier for
/// the Live models; `pricing.rs` prices cached at the standard rate to match,
/// so nothing is under-billed by that choice.
///
/// Extended thinking reports its reasoning as `thoughtsTokenCount`, billed at
/// the text-output rate and absent from the per-modality response details
/// (the same split `generateContent` makes between `candidatesTokenCount` and
/// thoughts). It is folded into text output here; if Google ever counted it
/// in both places the turn would bill high, which is the error this codebase
/// prefers over a silent $0.
fn parse_realtime_usage(usage: &Value) -> Option<crate::types::RealtimeUsage> {
    let prompt = modality_token_counts(usage, "promptTokensDetails", "prompt_tokens_details");
    let response = modality_token_counts(usage, "responseTokensDetails", "response_tokens_details");
    let thoughts = usage
        .get("thoughtsTokenCount")
        .or_else(|| usage.get("thoughts_token_count"))
        .and_then(Value::as_u64);
    if prompt.is_none() && response.is_none() && thoughts.is_none() {
        return None;
    }
    let (text_input, audio_input) = prompt.unwrap_or((0, 0));
    let (text_output, audio_output) = response.unwrap_or((0, 0));
    Some(crate::types::RealtimeUsage {
        text_input_tokens: text_input,
        text_cached_input_tokens: 0,
        text_output_tokens: text_output.saturating_add(thoughts.unwrap_or(0)),
        audio_input_tokens: audio_input,
        audio_cached_input_tokens: 0,
        audio_output_tokens: audio_output,
        billed_seconds: 0.0,
    })
}

#[derive(Debug, Default)]
struct GeminiLiveServerEventOutcome {
    setup_complete: bool,
}

async fn handle_gemini_live_server_event(
    text: &str,
    downstream_tx: &mpsc::Sender<Vec<u8>>,
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    state: &mut GeminiLiveEventState,
    resume_handles_by_voice_session: &DashMap<String, String>,
    voice_session_id: &str,
    pending_tool_names: &Arc<Mutex<HashMap<String, String>>>,
) -> Result<GeminiLiveServerEventOutcome, RealtimeProviderError> {
    let event: Value = serde_json::from_str(text)
        .map_err(|e| RealtimeProviderError::Upstream(format!("invalid Gemini Live event: {e}")))?;
    let mut outcome = GeminiLiveServerEventOutcome::default();

    if event.get("setupComplete").is_some() {
        outcome.setup_complete = true;
    }

    if let Some(error) = event.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .unwrap_or("Gemini Live error")
            .to_string();
        let _ = events_tx
            .send(RealtimeProviderEvent::Error {
                message,
                recoverable: true,
            })
            .await;
    }

    if let Some(update) = event.get("sessionResumptionUpdate") {
        let resumable = update
            .get("resumable")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if resumable {
            if let Some(handle) = update
                .get("newHandle")
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
            {
                resume_handles_by_voice_session
                    .insert(voice_session_id.to_string(), handle.to_string());
                let _ = events_tx
                    .send(RealtimeProviderEvent::NativeResumeHandleUpdated {
                        handle: handle.to_string(),
                    })
                    .await;
            }
        } else {
            // The conversation is over: its in-flight calls are meaningless.
            resume_handles_by_voice_session.remove(voice_session_id);
            pending_tool_names.lock().clear();
        }
    }

    if let Some(go_away) = event.get("goAway") {
        let time_left_secs = go_away
            .get("timeLeft")
            .and_then(Value::as_str)
            .and_then(parse_google_duration_secs);
        let _ = events_tx
            .send(RealtimeProviderEvent::SessionExpiring { time_left_secs })
            .await;
    }

    if let Some(usage) = event.get("usageMetadata") {
        state.input_tokens = usage
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .or(state.input_tokens);
        state.output_tokens = usage
            .get("responseTokenCount")
            .and_then(Value::as_u64)
            .or(state.output_tokens);
        if state.input_tokens.is_none() {
            state.input_tokens = usage.get("totalTokenCount").and_then(Value::as_u64);
        }
        state.realtime_usage = parse_realtime_usage(usage).or(state.realtime_usage);
    }

    if let Some(tool_call) = event.get("toolCall") {
        if let Some(calls) = tool_call.get("functionCalls").and_then(Value::as_array) {
            // Tool calls belong to the same logical model response as the
            // transcript/audio that may follow in this event. Allocate (or
            // reuse) that response identity before emitting any call so a
            // guided-flow fence can reject only the interrupted generation.
            let response_id = state.response_id();
            for call in calls {
                let name = call
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let provider_id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("gemini-call-{}", Ulid::new()));
                remember_pending_tool_name(pending_tool_names, provider_id.clone(), name.clone());
                let arguments_json = call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
                    .to_string();
                info!(
                    name = %name,
                    call_id = %provider_id,
                    "[REALTIME-VOICE] Gemini Live function call"
                );
                let _ = events_tx
                    .send(RealtimeProviderEvent::FunctionCall {
                        response_id: Some(response_id.clone()),
                        call_id: provider_id,
                        name,
                        arguments_json,
                    })
                    .await;
            }
        }
    }

    if let Some(server_content) = event.get("serverContent") {
        handle_server_content(server_content, downstream_tx, events_tx, state).await;
    }

    if let Some(data) = event.get("data").and_then(Value::as_str) {
        decode_and_send_audio(data, downstream_tx, events_tx, state).await;
    }
    if let Some(text) = event.get("text").and_then(Value::as_str) {
        emit_assistant_delta(text, events_tx, state).await;
    }

    Ok(outcome)
}

async fn handle_gemini_live_server_binary_event(
    bytes: &[u8],
    downstream_tx: &mpsc::Sender<Vec<u8>>,
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    state: &mut GeminiLiveEventState,
    resume_handles_by_voice_session: &DashMap<String, String>,
    voice_session_id: &str,
    pending_tool_names: &Arc<Mutex<HashMap<String, String>>>,
) -> Result<GeminiLiveServerEventOutcome, RealtimeProviderError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        RealtimeProviderError::Upstream(format!("invalid UTF-8 Gemini Live binary event: {error}"))
    })?;
    handle_gemini_live_server_event(
        text,
        downstream_tx,
        events_tx,
        state,
        resume_handles_by_voice_session,
        voice_session_id,
        pending_tool_names,
    )
    .await
}

fn absorb_transcript_fragment(buffer: &mut String, incoming: &str) {
    if incoming.is_empty() {
        return;
    }
    if buffer.is_empty() || incoming.starts_with(buffer.as_str()) {
        buffer.clear();
        buffer.push_str(incoming);
    } else if !buffer.starts_with(incoming) {
        buffer.push_str(incoming);
    }
}

fn transcription_object<'a>(
    server_content: &'a Value,
    camel: &str,
    snake: &str,
) -> Option<&'a Value> {
    server_content
        .get(camel)
        .or_else(|| server_content.get(snake))
}

async fn handle_server_content(
    server_content: &Value,
    downstream_tx: &mpsc::Sender<Vec<u8>>,
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    state: &mut GeminiLiveEventState,
) {
    if let Some(input) =
        transcription_object(server_content, "inputTranscription", "input_transcription")
    {
        if let Some(text) = input
            .get("text")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            absorb_transcript_fragment(&mut state.user_transcript, text);
            emit_user_partial(events_tx, state).await;
        }
        if input
            .get("finished")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            emit_user_final_once(events_tx, state).await;
        }
    }

    if let Some(output) = transcription_object(
        server_content,
        "outputTranscription",
        "output_transcription",
    )
    .and_then(|v| v.get("text"))
    .and_then(Value::as_str)
    .filter(|v| !v.is_empty())
    {
        emit_assistant_delta(output, events_tx, state).await;
    }

    if let Some(parts) = server_content
        .get("modelTurn")
        .or_else(|| server_content.get("model_turn"))
        .and_then(|turn| turn.get("parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            if let Some(data) = part
                .get("inlineData")
                .or_else(|| part.get("inline_data"))
                .and_then(|inline| inline.get("data"))
                .and_then(Value::as_str)
            {
                decode_and_send_audio(data, downstream_tx, events_tx, state).await;
            }
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                emit_assistant_delta(text, events_tx, state).await;
            }
        }
    }

    if server_content
        .get("interrupted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if state.audio_started {
            let response_id = state.response_id();
            let _ = events_tx
                .send(RealtimeProviderEvent::AssistantAudioDone {
                    response_id,
                    interrupted: true,
                })
                .await;
            state.audio_started = false;
        }
        let _ = events_tx.send(RealtimeProviderEvent::SpeechStarted).await;
    }

    // Each `turnComplete` closes one generation and ships that generation's
    // own `usageMetadata`, on every model family. On the extended-thinking
    // model that is an utterance boundary, not idleness: the model may have
    // said "let me check…" and still be waiting on a non-blocking tool.
    // `interactionStatus` below carries the idle signal; flushing per
    // generation keeps each generation's usage billed exactly once.
    //
    // `generationComplete` arrives in its own message one step BEFORE the
    // `turnComplete` message that carries the usage (measured on 3.1, 3.8 and
    // 3.8 extended thinking, 2026-09-17). Closing the response there would
    // ship `usage: None` and the usage that follows would be dropped — every
    // Gemini voice turn priced at $0 — so the response only closes early when
    // its usage is already known.
    if server_content
        .get("turnComplete")
        .or_else(|| server_content.get("turn_complete"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        flush_turn(events_tx, state).await;
    } else if server_content
        .get("generationComplete")
        .or_else(|| server_content.get("generation_complete"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && (state.realtime_usage.is_some() || state.input_tokens.is_some())
    {
        emit_response_done_once(events_tx, state).await;
    }

    if let Some(in_progress) = server_content
        .get("interactionStatus")
        .or_else(|| server_content.get("interaction_status"))
        .and_then(Value::as_str)
        .and_then(parse_interaction_status)
    {
        if state.interaction_in_progress != Some(in_progress) {
            state.interaction_in_progress = Some(in_progress);
            let _ = events_tx
                .send(RealtimeProviderEvent::InteractionStatus { in_progress })
                .await;
        }
    }
}

/// `IN_PROGRESS` → still reasoning or waiting on a tool; `IDLE` → the user's
/// request is answered. Any other spelling is unknown rather than guessed.
fn parse_interaction_status(status: &str) -> Option<bool> {
    match status.trim().to_ascii_uppercase().as_str() {
        "IN_PROGRESS" => Some(true),
        "IDLE" => Some(false),
        _ => None,
    }
}

async fn emit_user_partial(
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    state: &mut GeminiLiveEventState,
) {
    if state.user_final_emitted {
        return;
    }
    let text = state.user_transcript.trim().to_string();
    if text.is_empty() {
        return;
    }
    let item_id = state.user_item_id();
    let _ = events_tx
        .send(RealtimeProviderEvent::UserTranscriptPartial { text, item_id })
        .await;
}

async fn emit_user_final_once(
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    state: &mut GeminiLiveEventState,
) {
    if state.user_final_emitted {
        return;
    }
    let text = state.user_transcript.trim().to_string();
    if text.is_empty() {
        return;
    }
    state.user_final_emitted = true;
    let item_id = state.user_item_id();
    let _ = events_tx
        .send(RealtimeProviderEvent::UserTranscriptFinal { text, item_id })
        .await;
}

async fn emit_assistant_delta(
    text: &str,
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    state: &mut GeminiLiveEventState,
) {
    if text.is_empty() {
        return;
    }
    emit_user_final_once(events_tx, state).await;
    absorb_transcript_fragment(&mut state.assistant_transcript, text);
    let response_id = state.response_id();
    let _ = events_tx
        .send(RealtimeProviderEvent::AssistantTranscriptDelta {
            response_id,
            text: state.assistant_transcript.clone(),
        })
        .await;
}

async fn flush_turn(
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    state: &mut GeminiLiveEventState,
) {
    if state.audio_started {
        let response_id = state.response_id();
        let _ = events_tx
            .send(RealtimeProviderEvent::AssistantAudioDone {
                response_id,
                interrupted: false,
            })
            .await;
        state.audio_started = false;
    }
    emit_user_final_once(events_tx, state).await;

    let assistant_text = state.assistant_transcript.trim().to_string();
    if !assistant_text.is_empty() {
        let response_id = state.response_id();
        let _ = events_tx
            .send(RealtimeProviderEvent::AssistantTranscriptFinal {
                response_id,
                text: assistant_text,
            })
            .await;
    }

    emit_response_done_once(events_tx, state).await;
    state.reset_turn();
}

async fn emit_response_done_once(
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    state: &mut GeminiLiveEventState,
) {
    if state.response_done_emitted {
        return;
    }
    state.response_done_emitted = true;
    let response_id = state.response_id();
    let _ = events_tx
        .send(RealtimeProviderEvent::ResponseDone {
            response_id: Some(response_id),
            input_tokens: state.input_tokens,
            output_tokens: state.output_tokens,
            // Gemini Live DOES surface a text/audio split, via
            // `usageMetadata.{prompt,response}TokensDetails`. It used to be
            // dropped here, so every Gemini voice turn reached the orchestrator
            // with `usage: None` and was recorded as NaN cost.
            usage: state.realtime_usage,
        })
        .await;
}

async fn decode_and_send_audio(
    data: &str,
    downstream_tx: &mpsc::Sender<Vec<u8>>,
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    state: &mut GeminiLiveEventState,
) {
    match BASE64_STANDARD.decode(data) {
        Ok(bytes) if !bytes.is_empty() => {
            if !state.audio_started {
                emit_user_final_once(events_tx, state).await;
                state.audio_started = true;
                let response_id = state.response_id();
                let _ = events_tx
                    .send(RealtimeProviderEvent::AssistantAudioStarted { response_id })
                    .await;
            }
            let _ = downstream_tx.send(bytes).await;
        },
        Ok(_) => {},
        Err(err) => {
            let _ = events_tx
                .send(RealtimeProviderEvent::Error {
                    message: format!("Gemini Live audio decode failed: {err}"),
                    recoverable: true,
                })
                .await;
        },
    }
}

fn gemini_live_ws_url(base: &str, api_key: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.contains('?') {
        format!("{base}&key={api_key}")
    } else {
        format!("{base}?key={api_key}")
    }
}

fn gemini_model_path(model: &str) -> String {
    let model = model.trim();
    if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{model}")
    }
}

fn gemini_upstream_session_id(voice_session_id: &str) -> String {
    format!(
        "{GEMINI_UPSTREAM_SESSION_PREFIX}{voice_session_id}:{}",
        Ulid::new()
    )
}

fn gemini_voice_session_id_from_upstream(upstream_session_id: &str) -> Option<String> {
    upstream_session_id
        .strip_prefix(GEMINI_UPSTREAM_SESSION_PREFIX)
        .and_then(|rest| rest.rsplit_once(':'))
        .map(|(voice_session_id, _)| voice_session_id.to_string())
}

fn redact_gemini_key(url: &str) -> String {
    let Some((head, _)) = url.split_once("key=") else {
        return url.to_string();
    };
    format!("{head}key=***")
}

/// Map Magician's tool catalog to Gemini `functionDeclarations`.
///
/// `non_blocking` marks every declaration `behavior: NON_BLOCKING` so the
/// model keeps talking while Magician runs the tool. It is declared even on
/// 3.8 Live, where it is already the default, so the wire says what the
/// provider relies on; the extended-thinking model accepts nothing else; 3.1
/// Flash Live rejects the field, so it stays off there.
fn gemini_live_tools(tools: &[LLMToolSpec], non_blocking: bool) -> Vec<Value> {
    if tools.is_empty() {
        return Vec::new();
    }
    let declarations = tools
        .iter()
        .map(|tool| {
            let mut parameters = tool.parameters.clone();
            sanitize_gemini_schema(&mut parameters);
            let mut declaration = json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": parameters,
            });
            if non_blocking {
                declaration["behavior"] = Value::String("NON_BLOCKING".to_string());
            }
            declaration
        })
        .collect::<Vec<_>>();
    vec![json!({ "functionDeclarations": declarations })]
}

fn parse_google_duration_secs(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    let seconds = trimmed.strip_suffix('s')?.parse::<f64>().ok()?;
    if seconds.is_finite() && seconds >= 0.0 {
        Some(seconds.ceil() as u64)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;

    /// A PTT assistant task for `model`, with the contract the factory would
    /// derive. Override fields with struct-update syntax.
    fn assistant_task(model: &str) -> GeminiLiveTask {
        GeminiLiveTask {
            api_key: "test".to_string(),
            websocket_url: GEMINI_LIVE_DEFAULT_WEBSOCKET_URL.to_string(),
            model: model.to_string(),
            contract: gemini_live_model_contract(model),
            voice: Some("Kore".to_string()),
            turn_detection_mode: "none".to_string(),
            native_resume_handle: None,
            upstream_session_id: "gemini-live:vsid:01H".to_string(),
            voice_session_id: "vsid".to_string(),
            pending_tool_names: Arc::new(Mutex::new(HashMap::new())),
            resume_handles_by_voice_session: Arc::new(DashMap::new()),
            sessions: Arc::new(DashMap::new()),
            mode: RealtimeVoiceMode::Assistant,
            translation_target_language: None,
            translation_echo_target_language: false,
            thinking_level: None,
            tool_result_scheduling: GeminiToolResultScheduling::default(),
        }
    }

    fn lookup_tool() -> LLMToolSpec {
        LLMToolSpec {
            name: "lookup".to_string(),
            description: "Look something up".to_string(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    fn drain_events(
        events_rx: &mut mpsc::Receiver<RealtimeProviderEvent>,
    ) -> Vec<RealtimeProviderEvent> {
        let mut seen = Vec::new();
        while let Ok(event) = events_rx.try_recv() {
            seen.push(event);
        }
        seen
    }

    #[test]
    fn model_contract_follows_the_published_family_table() {
        for legacy in [
            "gemini-3.1-flash-live-preview",
            "gemini-3.5-live-translate-preview",
            "gemini-2.5-flash-native-audio-preview-09-2025",
            "",
        ] {
            assert_eq!(
                gemini_live_model_contract(legacy),
                GeminiLiveModelContract::LEGACY,
                "{legacy:?} must keep blocking tools and no thinking knob"
            );
        }
        assert_eq!(
            gemini_live_model_contract("gemini-3.8-live"),
            GeminiLiveModelContract::LIVE_3_8
        );
        assert_eq!(
            gemini_live_model_contract("models/Gemini-3.8-Live"),
            GeminiLiveModelContract::LIVE_3_8,
            "resource path and case must not change the contract"
        );
        assert_eq!(
            gemini_live_model_contract("gemini-3.8-live-extended-thinking"),
            GeminiLiveModelContract::LIVE_3_8_EXTENDED_THINKING
        );
        assert!(!GeminiLiveModelContract::LIVE_3_8.thinking_level);
        assert!(!GeminiLiveModelContract::LIVE_3_8.interaction_status);
        assert!(GeminiLiveModelContract::LIVE_3_8_EXTENDED_THINKING.async_function_calling);
        assert!(GeminiLiveModelContract::LIVE_3_8_EXTENDED_THINKING.interaction_status);
    }

    #[test]
    fn default_model_is_the_ga_live_model_with_async_tools() {
        assert_eq!(GEMINI_LIVE_DEFAULT_MODEL, "gemini-3.8-live");
        assert!(gemini_live_model_contract(GEMINI_LIVE_DEFAULT_MODEL).async_function_calling);
    }

    #[test]
    fn thinking_level_and_scheduling_parse_case_insensitively_and_reject_unknowns() {
        assert_eq!(
            GeminiThinkingLevel::parse(" Medium "),
            Some(GeminiThinkingLevel::Medium)
        );
        assert_eq!(
            GeminiThinkingLevel::parse("HIGH"),
            Some(GeminiThinkingLevel::High)
        );
        assert_eq!(
            GeminiThinkingLevel::parse("minimal"),
            None,
            "Live rejects MINIMAL"
        );
        assert_eq!(
            GeminiToolResultScheduling::parse("Interrupt"),
            Some(GeminiToolResultScheduling::Interrupt)
        );
        assert_eq!(
            GeminiToolResultScheduling::parse("when_idle"),
            Some(GeminiToolResultScheduling::WhenIdle)
        );
        assert_eq!(GeminiToolResultScheduling::parse("later"), None);
        assert_eq!(
            GeminiToolResultScheduling::default(),
            GeminiToolResultScheduling::WhenIdle
        );
    }

    #[test]
    fn extended_thinking_setup_carries_thinking_level_and_non_blocking_tools() {
        let task = GeminiLiveTask {
            thinking_level: Some(GeminiThinkingLevel::Medium),
            ..assistant_task("gemini-3.8-live-extended-thinking")
        };
        let payload = task.configure_session_payload("Be concise.", &[lookup_tool()], true);
        let setup = &payload["setup"];
        assert_eq!(setup["model"], "models/gemini-3.8-live-extended-thinking");
        assert_eq!(
            setup["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "MEDIUM"
        );
        assert_eq!(
            setup["tools"][0]["functionDeclarations"][0]["behavior"], "NON_BLOCKING",
            "the extended-thinking model accepts only non-blocking functions"
        );
        // Everything the 3.1 payload had is still there.
        assert_eq!(setup["inputAudioTranscription"], json!({}));
        assert_eq!(
            setup["contextWindowCompression"]["slidingWindow"],
            json!({})
        );
        assert_eq!(setup["sessionResumption"], json!({}));
    }

    #[test]
    fn extended_thinking_without_a_level_leaves_google_default_in_charge() {
        let task = assistant_task("gemini-3.8-live-extended-thinking");
        let payload = task.configure_session_payload("Be concise.", &[], true);
        assert!(payload["setup"]["generationConfig"]
            .get("thinkingConfig")
            .is_none());
    }

    #[test]
    fn live_3_8_setup_declares_non_blocking_tools_and_never_a_thinking_level() {
        // A stale pairing must degrade to the model default, not a 1007 close.
        let task = GeminiLiveTask {
            thinking_level: Some(GeminiThinkingLevel::High),
            ..assistant_task("gemini-3.8-live")
        };
        let payload = task.configure_session_payload("Be concise.", &[lookup_tool()], true);
        let setup = &payload["setup"];
        assert!(setup["generationConfig"].get("thinkingConfig").is_none());
        assert_eq!(
            setup["tools"][0]["functionDeclarations"][0]["behavior"],
            "NON_BLOCKING"
        );
    }

    #[test]
    fn flash_live_3_1_setup_is_unchanged_by_the_new_options() {
        let task = GeminiLiveTask {
            thinking_level: Some(GeminiThinkingLevel::Low),
            tool_result_scheduling: GeminiToolResultScheduling::Interrupt,
            ..assistant_task("gemini-3.1-flash-live-preview")
        };
        let payload = task.configure_session_payload("Be concise.", &[lookup_tool()], true);
        let setup = &payload["setup"];
        assert!(setup["generationConfig"].get("thinkingConfig").is_none());
        assert!(
            setup["tools"][0]["functionDeclarations"][0]
                .get("behavior")
                .is_none(),
            "3.1 Flash Live rejects async declarations"
        );
    }

    #[test]
    fn tool_response_carries_scheduling_only_for_async_contracts() {
        let blocking = gemini_tool_response_payload("call-1", "lookup", "{\"ok\":true}", None);
        let response = &blocking["toolResponse"]["functionResponses"][0];
        assert_eq!(response["id"], "call-1");
        assert_eq!(response["name"], "lookup");
        assert_eq!(response["response"]["output"], "{\"ok\":true}");
        assert!(response["response"].get("scheduling").is_none());

        let non_blocking = gemini_tool_response_payload(
            "call-2",
            "lookup",
            "{}",
            Some(GeminiToolResultScheduling::WhenIdle),
        );
        assert_eq!(
            non_blocking["toolResponse"]["functionResponses"][0]["response"]["scheduling"],
            "WHEN_IDLE"
        );
        let interrupt = gemini_tool_response_payload(
            "call-3",
            "lookup",
            "{}",
            Some(GeminiToolResultScheduling::Interrupt),
        );
        assert_eq!(
            interrupt["toolResponse"]["functionResponses"][0]["response"]["scheduling"],
            "INTERRUPT"
        );
    }

    #[tokio::test]
    async fn interaction_status_spans_the_utterance_boundary_of_a_non_blocking_call() {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (events_tx, mut events_rx) = mpsc::channel(32);
        let mut state = GeminiLiveEventState::default();
        let handles = DashMap::new();
        let pending_tool_names = Arc::new(Mutex::new(HashMap::new()));

        // Utterance 1: "let me check…" plus the non-blocking call; the
        // generation is complete but the interaction is not.
        let acknowledgement = json!({
            "toolCall": {
                "functionCalls": [{ "id": "call-1", "name": "lookup", "args": {} }]
            },
            "serverContent": {
                "outputTranscription": { "text": "Let me check that." },
                "turnComplete": true,
                "interactionStatus": "IN_PROGRESS"
            },
            "usageMetadata": {
                "promptTokenCount": 10,
                "responseTokenCount": 3
            }
        });
        handle_gemini_live_server_event(
            &acknowledgement.to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();
        let first = drain_events(&mut events_rx);
        let done_index = first
            .iter()
            .position(|e| matches!(e, RealtimeProviderEvent::ResponseDone { .. }))
            .expect("the acknowledgement generation is flushed with its own usage");
        let status_index = first
            .iter()
            .position(|e| e == &RealtimeProviderEvent::InteractionStatus { in_progress: true })
            .expect("the interaction stays open across the utterance boundary");
        assert!(
            done_index < status_index,
            "consumers see the generation close before they learn it is not idle"
        );
        assert!(first.iter().any(|e| matches!(
            e,
            RealtimeProviderEvent::AssistantTranscriptFinal { text, .. } if text == "Let me check that."
        )));
        assert_eq!(state.interaction_in_progress, Some(true));

        // Still in progress on the next message: no duplicate status event.
        let quiet = json!({
            "serverContent": { "interactionStatus": "IN_PROGRESS" }
        });
        handle_gemini_live_server_event(
            &quiet.to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();
        assert!(drain_events(&mut events_rx).is_empty());

        // Utterance 2: the answer after the tool result; the interaction is
        // idle again and the answer is its own generation.
        let answer = json!({
            "serverContent": {
                "outputTranscription": { "text": "It is at three." },
                "turnComplete": true,
                "interactionStatus": "IDLE"
            },
            "usageMetadata": {
                "promptTokenCount": 40,
                "responseTokenCount": 8
            }
        });
        handle_gemini_live_server_event(
            &answer.to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();
        let second = drain_events(&mut events_rx);
        assert!(second.iter().any(|e| matches!(
            e,
            RealtimeProviderEvent::ResponseDone { response_id: Some(id), input_tokens: Some(40), .. }
                if id == "gemini-response-2"
        )));
        assert!(second
            .iter()
            .any(|e| e == &RealtimeProviderEvent::InteractionStatus { in_progress: false }));
        assert_eq!(state.interaction_in_progress, Some(false));
    }

    #[tokio::test]
    async fn legacy_models_never_emit_interaction_status() {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (events_tx, mut events_rx) = mpsc::channel(16);
        let mut state = GeminiLiveEventState::default();
        let handles = DashMap::new();
        let pending_tool_names = Arc::new(Mutex::new(HashMap::new()));
        let event = json!({
            "serverContent": {
                "outputTranscription": { "text": "Done." },
                "turnComplete": true
            }
        });
        handle_gemini_live_server_event(
            &event.to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();
        let seen = drain_events(&mut events_rx);
        assert!(seen
            .iter()
            .any(|e| matches!(e, RealtimeProviderEvent::ResponseDone { .. })));
        assert!(!seen
            .iter()
            .any(|e| matches!(e, RealtimeProviderEvent::InteractionStatus { .. })));
        assert_eq!(state.interaction_in_progress, None);
    }

    #[test]
    fn unknown_interaction_status_is_not_guessed() {
        assert_eq!(parse_interaction_status("in_progress"), Some(true));
        assert_eq!(parse_interaction_status(" idle "), Some(false));
        assert_eq!(parse_interaction_status("THINKING"), None);
    }

    #[test]
    fn thinking_tokens_bill_as_text_output() {
        let usage = json!({
            "promptTokenCount": 120,
            "responseTokenCount": 30,
            "thoughtsTokenCount": 500,
            "promptTokensDetails": [
                { "modality": "AUDIO", "tokenCount": 100 },
                { "modality": "TEXT", "tokenCount": 20 }
            ],
            "responseTokensDetails": [
                { "modality": "AUDIO", "tokenCount": 25 },
                { "modality": "TEXT", "tokenCount": 5 }
            ]
        });
        let parsed = parse_realtime_usage(&usage).expect("details present");
        assert_eq!(parsed.audio_input_tokens, 100);
        assert_eq!(parsed.text_input_tokens, 20);
        assert_eq!(parsed.audio_output_tokens, 25);
        assert_eq!(parsed.text_output_tokens, 505, "5 text + 500 thoughts");

        // Thoughts alone still produce a billable split rather than `None`.
        let thoughts_only = json!({ "thoughtsTokenCount": 7 });
        assert_eq!(
            parse_realtime_usage(&thoughts_only).map(|u| u.text_output_tokens),
            Some(7)
        );
        assert!(parse_realtime_usage(&json!({ "promptTokenCount": 1 })).is_none());
    }

    #[test]
    fn gemini_live_ws_url_appends_key() {
        assert_eq!(
            gemini_live_ws_url("wss://example.test/live", "abc123"),
            "wss://example.test/live?key=abc123"
        );
        assert_eq!(
            gemini_live_ws_url("wss://example.test/live?x=1", "abc123"),
            "wss://example.test/live?x=1&key=abc123"
        );
    }

    #[test]
    fn configure_payload_enables_audio_transcripts_resumption_tools_and_manual_activity() {
        let task = GeminiLiveTask {
            native_resume_handle: Some("resume-1".to_string()),
            ..assistant_task("gemini-3.1-flash-live-preview")
        };
        let payload = task.configure_session_payload(
            "Be concise.",
            &[LLMToolSpec {
                name: "lookup".to_string(),
                description: "Look something up".to_string(),
                parameters: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "q": { "type": ["string", "null"], "format": "uri" }
                    }
                }),
            }],
            true,
        );
        let setup = &payload["setup"];
        assert_eq!(setup["model"], "models/gemini-3.1-flash-live-preview");
        assert_eq!(setup["generationConfig"]["responseModalities"][0], "AUDIO");
        assert_eq!(setup["inputAudioTranscription"], json!({}));
        assert_eq!(setup["outputAudioTranscription"], json!({}));
        assert_eq!(
            setup["realtimeInputConfig"]["automaticActivityDetection"]["disabled"],
            true
        );
        assert_eq!(setup["sessionResumption"]["handle"], "resume-1");
        assert_eq!(
            setup["historyConfig"]["initialHistoryInClientContent"],
            true
        );
        assert_eq!(
            setup["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
                ["voiceName"],
            "Kore"
        );
        assert_eq!(
            setup["tools"][0]["functionDeclarations"][0]["name"],
            "lookup"
        );
        assert!(setup["tools"][0]["functionDeclarations"][0]["parameters"]
            .get("additionalProperties")
            .is_none());
        assert_eq!(
            setup["tools"][0]["functionDeclarations"][0]["parameters"]["properties"]["q"]
                ["nullable"],
            true
        );
        assert!(
            setup["tools"][0]["functionDeclarations"][0]["parameters"]["properties"]["q"]
                .get("format")
                .is_none()
        );
    }

    #[test]
    fn translation_payload_uses_generation_translation_config_without_assistant_tools() {
        let task = GeminiLiveTask {
            voice: None,
            turn_detection_mode: "server_vad".to_string(),
            mode: RealtimeVoiceMode::Translation,
            translation_target_language: Some("hi".to_string()),
            translation_echo_target_language: true,
            ..assistant_task("gemini-3.5-live-translate-preview")
        };
        let payload = task.configure_session_payload("ignored", &[], false);
        let setup = &payload["setup"];
        assert_eq!(setup["model"], "models/gemini-3.5-live-translate-preview");
        assert_eq!(
            setup["generationConfig"]["translationConfig"]["targetLanguageCode"],
            "hi"
        );
        assert_eq!(
            setup["generationConfig"]["translationConfig"]["echoTargetLanguage"],
            true
        );
        assert!(setup.get("systemInstruction").is_none());
        assert!(setup.get("tools").is_none());
        assert_eq!(setup["inputAudioTranscription"], json!({}));
        assert_eq!(setup["outputAudioTranscription"], json!({}));
        assert!(setup["generationConfig"]
            .get("inputAudioTranscription")
            .is_none());
        assert!(setup["generationConfig"]
            .get("outputAudioTranscription")
            .is_none());
    }

    #[test]
    fn audio_payload_keeps_browser_24khz_pcm() {
        let frame = vec![1u8, 2, 3, 4, 5, 6];
        let payload = gemini_audio_payload(frame.clone());
        let audio = &payload["realtimeInput"]["audio"];
        assert_eq!(audio["mimeType"], "audio/pcm;rate=24000");
        let encoded = audio["data"].as_str().unwrap();
        assert_eq!(BASE64_STANDARD.decode(encoded).unwrap(), frame);
    }

    #[test]
    fn initial_history_uses_client_content_without_requesting_turn_completion() {
        let payload = gemini_initial_history_payload("Prior context");
        assert_eq!(
            payload["clientContent"]["turns"][0]["parts"][0]["text"],
            "Prior context"
        );
        assert_eq!(payload["clientContent"]["turnComplete"], false);
    }

    #[tokio::test]
    async fn server_event_decodes_transcripts_audio_tool_resume_and_goaway() {
        let (audio_tx, mut audio_rx) = mpsc::channel(4);
        let (events_tx, mut events_rx) = mpsc::channel(16);
        let mut state = GeminiLiveEventState::default();
        let handles = DashMap::new();
        let pending_tool_names = Arc::new(Mutex::new(HashMap::new()));
        let audio = BASE64_STANDARD.encode([1u8, 2, 3, 4]);
        let event = json!({
            "setupComplete": {},
            "sessionResumptionUpdate": {
                "resumable": true,
                "newHandle": "resume-2"
            },
            "goAway": {
                "timeLeft": "30.5s"
            },
            "usageMetadata": {
                "promptTokenCount": 10,
                "responseTokenCount": 5
            },
            "toolCall": {
                "functionCalls": [{
                    "id": "call-1",
                    "name": "lookup",
                    "args": { "q": "coffee" }
                }]
            },
            "serverContent": {
                "inputTranscription": { "text": "hello" },
                "outputTranscription": { "text": "hi" },
                "modelTurn": {
                    "parts": [{
                        "inlineData": {
                            "data": audio,
                            "mimeType": "audio/pcm;rate=24000"
                        }
                    }]
                },
                "turnComplete": true
            }
        });

        let outcome = handle_gemini_live_server_event(
            &event.to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();
        assert!(outcome.setup_complete);
        assert_eq!(audio_rx.recv().await.unwrap(), vec![1, 2, 3, 4]);
        assert_eq!(handles.get("voice-1").unwrap().value(), "resume-2");

        let mut seen_resume = false;
        let mut seen_expiring = false;
        let mut seen_tool = false;
        let mut seen_done = false;
        while let Ok(event) = events_rx.try_recv() {
            match event {
                RealtimeProviderEvent::NativeResumeHandleUpdated { handle } => {
                    seen_resume = handle == "resume-2";
                },
                RealtimeProviderEvent::SessionExpiring { time_left_secs } => {
                    seen_expiring = time_left_secs == Some(31);
                },
                RealtimeProviderEvent::FunctionCall {
                    response_id,
                    call_id,
                    name,
                    arguments_json,
                } => {
                    seen_tool = response_id.as_deref() == Some("gemini-response-1")
                        && call_id == "call-1"
                        && name == "lookup"
                        && arguments_json.contains("coffee");
                },
                RealtimeProviderEvent::ResponseDone {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    seen_done = input_tokens == Some(10) && output_tokens == Some(5);
                },
                _ => {},
            }
        }
        assert!(seen_resume);
        assert!(seen_expiring);
        assert!(seen_tool);
        assert!(seen_done);
    }

    #[tokio::test]
    async fn input_and_output_transcription_stream_before_turn_complete() {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (events_tx, mut events_rx) = mpsc::channel(16);
        let mut state = GeminiLiveEventState::default();
        let handles = DashMap::new();
        let pending_tool_names = Arc::new(Mutex::new(HashMap::new()));

        for payload in [
            json!({ "serverContent": { "inputTranscription": { "text": "hel" } } }),
            json!({ "serverContent": { "inputTranscription": { "text": "lo" } } }),
            json!({ "serverContent": { "outputTranscription": { "text": "hi" } } }),
            json!({ "serverContent": { "outputTranscription": { "text": " there" } } }),
            json!({ "serverContent": { "turnComplete": true } }),
        ] {
            handle_gemini_live_server_event(
                &payload.to_string(),
                &audio_tx,
                &events_tx,
                &mut state,
                &handles,
                "voice-1",
                &pending_tool_names,
            )
            .await
            .unwrap();
        }

        let mut events = Vec::new();
        while let Ok(event) = events_rx.try_recv() {
            events.push(event);
        }
        assert!(matches!(
            events.as_slice(),
            [
                RealtimeProviderEvent::UserTranscriptPartial {
                    text: user_a,
                    item_id: user_id_a,
                },
                RealtimeProviderEvent::UserTranscriptPartial {
                    text: user_b,
                    item_id: user_id_b,
                },
                RealtimeProviderEvent::UserTranscriptFinal {
                    text: user_final,
                    item_id: user_id_final,
                },
                RealtimeProviderEvent::AssistantTranscriptDelta {
                    response_id: delta_id_a,
                    text: assistant_a,
                },
                RealtimeProviderEvent::AssistantTranscriptDelta {
                    response_id: delta_id_b,
                    text: assistant_b,
                },
                RealtimeProviderEvent::AssistantTranscriptFinal {
                    response_id: final_id,
                    text: assistant_final,
                },
                RealtimeProviderEvent::ResponseDone { .. },
            ] if user_a == "hel"
                && user_b == "hello"
                && user_final == "hello"
                && user_id_a == "gemini-user-1"
                && user_id_b == "gemini-user-1"
                && user_id_final == "gemini-user-1"
                && assistant_a == "hi"
                && assistant_b == "hi there"
                && assistant_final == "hi there"
                && delta_id_a == "gemini-response-1"
                && delta_id_b == "gemini-response-1"
                && final_id == "gemini-response-1"
        ));
    }

    #[tokio::test]
    async fn binary_server_event_marks_setup_complete() {
        let (audio_tx, _audio_rx) = mpsc::channel(1);
        let (events_tx, _events_rx) = mpsc::channel(1);
        let mut state = GeminiLiveEventState::default();
        let handles = DashMap::new();
        let pending_tool_names = Arc::new(Mutex::new(HashMap::new()));

        let outcome = handle_gemini_live_server_binary_event(
            br#"{ "setupComplete": {} }"#,
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();

        assert!(outcome.setup_complete);
    }

    #[tokio::test]
    async fn malformed_binary_server_event_fails_closed() {
        let (audio_tx, _audio_rx) = mpsc::channel(1);
        let (events_tx, _events_rx) = mpsc::channel(1);
        let mut state = GeminiLiveEventState::default();
        let handles = DashMap::new();
        let pending_tool_names = Arc::new(Mutex::new(HashMap::new()));

        let error = handle_gemini_live_server_binary_event(
            &[0xff, 0xfe],
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("invalid UTF-8 Gemini Live binary event"));
    }

    /// Real wire order (3.1, 3.8, 3.8 extended thinking): `generationComplete`
    /// alone, then `turnComplete` carrying `usageMetadata`. The response must
    /// close on the second message so its usage travels with it.
    #[tokio::test]
    async fn response_done_waits_for_the_turn_complete_that_carries_usage() {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let mut state = GeminiLiveEventState::default();
        let handles = DashMap::new();
        let pending_tool_names = Arc::new(Mutex::new(HashMap::new()));

        let generation_event = json!({
            "serverContent": { "generationComplete": true }
        });
        handle_gemini_live_server_event(
            &generation_event.to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();
        assert!(
            drain_events(&mut events_rx)
                .iter()
                .all(|event| !matches!(event, RealtimeProviderEvent::ResponseDone { .. })),
            "a generation without usage yet must not close the response"
        );

        let turn_event = json!({
            "usageMetadata": {
                "promptTokenCount": 706,
                "responseTokenCount": 78,
                "promptTokensDetails": [
                    { "modality": "TEXT", "tokenCount": 417 },
                    { "modality": "AUDIO", "tokenCount": 222 }
                ],
                "responseTokensDetails": [{ "modality": "AUDIO", "tokenCount": 78 }],
                "thoughtsTokenCount": 65
            },
            "serverContent": { "turnComplete": true }
        });
        handle_gemini_live_server_event(
            &turn_event.to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();

        let done_events = drain_events(&mut events_rx)
            .into_iter()
            .filter(|event| matches!(event, RealtimeProviderEvent::ResponseDone { .. }))
            .collect::<Vec<_>>();
        assert_eq!(done_events.len(), 1);
        assert_eq!(
            done_events[0],
            RealtimeProviderEvent::ResponseDone {
                response_id: Some("gemini-response-1".to_string()),
                input_tokens: Some(706),
                output_tokens: Some(78),
                usage: Some(crate::types::RealtimeUsage {
                    text_input_tokens: 417,
                    text_cached_input_tokens: 0,
                    text_output_tokens: 65,
                    audio_input_tokens: 222,
                    audio_cached_input_tokens: 0,
                    audio_output_tokens: 78,
                    billed_seconds: 0.0,
                }),
            }
        );
    }

    /// When usage does ride with `generationComplete`, the response closes
    /// there and the later `turnComplete` does not close it twice.
    #[tokio::test]
    async fn response_done_closes_early_once_usage_is_known() {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let mut state = GeminiLiveEventState::default();
        let handles = DashMap::new();
        let pending_tool_names = Arc::new(Mutex::new(HashMap::new()));

        let generation_event = json!({
            "usageMetadata": {
                "promptTokenCount": 7,
                "responseTokenCount": 3
            },
            "serverContent": {
                "generationComplete": true
            }
        });
        handle_gemini_live_server_event(
            &generation_event.to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();
        assert_eq!(
            drain_events(&mut events_rx)
                .iter()
                .filter(|event| matches!(event, RealtimeProviderEvent::ResponseDone { .. }))
                .count(),
            1
        );

        let turn_event = json!({
            "serverContent": {
                "turnComplete": true
            }
        });
        handle_gemini_live_server_event(
            &turn_event.to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();
        assert!(
            drain_events(&mut events_rx)
                .iter()
                .all(|event| !matches!(event, RealtimeProviderEvent::ResponseDone { .. })),
            "the turn that follows an already-closed generation must not close it again"
        );
    }

    #[tokio::test]
    async fn server_event_clears_resume_handle_when_session_is_not_resumable() {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (events_tx, _events_rx) = mpsc::channel(8);
        let mut state = GeminiLiveEventState::default();
        let handles = DashMap::new();
        handles.insert("voice-1".to_string(), "old-handle".to_string());
        let pending_tool_names = Arc::new(Mutex::new(HashMap::new()));

        let event = json!({
            "sessionResumptionUpdate": {
                "resumable": false
            }
        });
        handle_gemini_live_server_event(
            &event.to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();

        assert!(handles.get("voice-1").is_none());
    }

    /// A Gemini Live session cannot change its tool catalog, so loading a
    /// deferred tool reconnects the session — and the result of the call that
    /// triggered the reconnect is delivered to the NEW connection. The names
    /// of the calls still in flight must therefore outlive the connection,
    /// exactly as the resume handle does. They did not: every catalog-driven
    /// reconnect answered with the call id in place of the function name, and
    /// the model, resumed by its handle and still waiting on the call, said "a
    /// system error occurred" — 3 of 3 runs on the voice conformance lane.
    #[test]
    fn in_flight_tool_call_names_outlive_a_reconnect() {
        let provider = GeminiLiveProvider::new("key");
        provider
            .pending_tool_names_for("voice-1")
            .lock()
            .insert("call-1".to_string(), "content_search".to_string());

        // The reconnect builds a fresh session task for the same voice session.
        let after_reconnect = provider.pending_tool_names_for("voice-1");
        assert_eq!(
            after_reconnect.lock().remove("call-1").as_deref(),
            Some("content_search"),
            "the response must name the function Gemini is waiting on"
        );
        assert!(
            provider.pending_tool_names_for("voice-2").lock().is_empty(),
            "another call is another conversation"
        );
    }

    /// A call whose result never arrives must not accumulate: the map holds
    /// the turn's in-flight calls, not the session's history.
    #[test]
    fn undelivered_tool_calls_stay_bounded() {
        let names = Arc::new(Mutex::new(HashMap::new()));
        for index in 0..(MAX_PENDING_TOOL_NAMES * 2) {
            remember_pending_tool_name(
                &names,
                format!("call-{index}"),
                "content_search".to_string(),
            );
        }
        assert!(names.lock().len() <= MAX_PENDING_TOOL_NAMES);
        let last = format!("call-{}", MAX_PENDING_TOOL_NAMES * 2 - 1);
        assert!(
            names.lock().contains_key(&last),
            "the newest call is the one still in flight"
        );
    }

    /// The conversation being over is the one moment the in-flight calls are
    /// meaningless — the same moment the resume handle is dropped.
    #[tokio::test]
    async fn a_session_that_cannot_resume_forgets_its_in_flight_calls() {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (events_tx, _events_rx) = mpsc::channel(8);
        let mut state = GeminiLiveEventState::default();
        let handles = DashMap::new();
        handles.insert("voice-1".to_string(), "old-handle".to_string());
        let provider = GeminiLiveProvider::new("key");
        let pending_tool_names = provider.pending_tool_names_for("voice-1");
        pending_tool_names
            .lock()
            .insert("call-1".to_string(), "content_search".to_string());

        handle_gemini_live_server_event(
            &json!({ "sessionResumptionUpdate": { "resumable": false } }).to_string(),
            &audio_tx,
            &events_tx,
            &mut state,
            &handles,
            "voice-1",
            &pending_tool_names,
        )
        .await
        .unwrap();

        assert!(pending_tool_names.lock().is_empty());
    }
}
