//! OpenAI Realtime API provider.
//!
//! In `DirectPeerToPeer` topology, the provider mints ephemeral browser
//! tokens for WebRTC audio between browser and OpenAI. The orchestrator hands
//! the frontend the URL + token from each minted descriptor, and the browser
//! opens an `RTCPeerConnection` straight to OpenAI.
//!
//! In `BackendProxied` topology, Magician opens an OpenAI Realtime WebSocket
//! with the real API key server-side, forwards host-native PCM/control frames,
//! and translates provider audio/transcript/tool events back onto the Magician
//! voice-control WebSocket.
//!
//! Why DirectPeerToPeer for OpenAI:
//!   - The WebRTC path has the lowest possible latency
//!     (browser-to-OpenAI direct, no backend hop). For a
//!     conversational surface this is the difference between
//!     "natural" and "noticeable lag".
//!   - The ephemeral `ek_…` client_secret keeps the real API key
//!     server-side — short TTL (~60 s), single-use, safe to surface
//!     to the browser.
//!
//! Why BackendProxied for host-native PTT:
//!   - Global hotkeys and the mascot tray do not always have a browser page
//!     open, so host mic/speaker audio needs a backend-owned realtime stream.
//!
//! OpenAI's GA Realtime endpoint is `/v1/realtime/client_secrets`.
//! The deprecated `/v1/realtime/sessions` beta endpoint returns
//! `beta_api_shape_disabled` as of 2026-05.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures_util::{Sink, SinkExt, StreamExt};
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::AUTHORIZATION, HeaderValue},
        Message,
    },
};
use tracing::{debug, error, info, warn};

use crate::realtime::provider::RealtimeProvider;
use crate::realtime::types::{
    AudioStreamChannel, RealtimeAudioControl, RealtimeAudioTopology, RealtimeProviderError,
    RealtimeProviderEvent, RealtimeProviderKind, RealtimeSessionDescriptor,
};
use crate::types::LLMToolSpec;

/// OpenAI's GA Realtime endpoint for minting ephemeral browser tokens.
pub const OPENAI_REALTIME_DEFAULT_BASE_URL: &str =
    "https://api.openai.com/v1/realtime/client_secrets";

/// OpenAI's Realtime WebSocket endpoint for server-to-server audio.
/// This is used by the backend-proxied native tray PTT path.
pub const OPENAI_REALTIME_DEFAULT_WEBSOCKET_URL: &str = "wss://api.openai.com/v1/realtime";

/// Default Realtime model. `gpt-realtime-2.1` is OpenAI's most capable Realtime
/// voice model (July 2026) — a same-priced, higher-quality drop-in for the prior
/// `gpt-realtime-2` (better alphanumeric recognition, silence/noise handling, and
/// interruption behavior). This shared default tracks it for consistency; override
/// via the `magician-config.yaml > realtime_voice.profiles.*.model` profile field.
///
/// Valid alternatives:
///   - `gpt-realtime-2.1` (default; July 2026, 128 K ctx, flagship reasoning)
///   - `gpt-realtime-2.1-mini` (distilled reasoning; ~3x cheaper audio, faster)
///   - `gpt-realtime-2` (prior flagship; May 2026, same price as 2.1)
///   - `gpt-realtime` (older GA pointer)
///   - `gpt-realtime-translate` (speech-to-speech translation)
///   - `gpt-realtime-whisper` (streaming STT only)
pub const OPENAI_REALTIME_DEFAULT_MODEL: &str = "gpt-realtime-2.1";

/// OpenAI Realtime fallback voice for callers that instantiate the provider
/// directly instead of through a `realtime_voice` config profile.
/// Config-driven profiles should leave `voice` unset to let the upstream
/// provider choose its default, or set `realtime_voice.profiles.*.voice` to
/// pin a specific voice.
pub const OPENAI_REALTIME_DEFAULT_VOICE: &str = "marin";

/// OpenAI Realtime's hard upper bound on a single upstream session
/// (seconds). The orchestrator schedules a proactive rotation about
/// 60 s before this fires so the user never hits an abrupt close.
/// Empirically ~30 min on `gpt-realtime` at conversational density;
/// we stay conservative.
const OPENAI_REALTIME_MAX_SESSION_SECS: u64 = 28 * 60;

/// Parse the OpenAI Realtime `response.done` `usage` object into a modality-split
/// [`RealtimeUsage`]. The API reports totals in `input_token_details`/
/// `output_token_details` (each with `text_tokens`/`audio_tokens`) and cached
/// counts under `input_token_details.cached_tokens_details`; we store the UNCACHED
/// input (total − cached) plus the cached counts separately so each token is
/// billed once at its correct rate. Incomplete, non-integer or internally
/// inconsistent detail returns `None`; callers retain coarse totals without
/// inventing a billable modality split.
fn parse_realtime_usage(usage: &Value) -> Option<crate::types::RealtimeUsage> {
    let input_total = usage.get("input_tokens")?.as_u64()?;
    let output_total = usage.get("output_tokens")?.as_u64()?;
    let input = usage.get("input_token_details")?;
    let output = usage.get("output_token_details")?;
    let cached = input.get("cached_tokens_details")?;
    let input_text = input.get("text_tokens")?.as_u64()?;
    let input_audio = input.get("audio_tokens")?.as_u64()?;
    let cached_text = cached.get("text_tokens")?.as_u64()?;
    let cached_audio = cached.get("audio_tokens")?.as_u64()?;
    let output_text = output.get("text_tokens")?.as_u64()?;
    let output_audio = output.get("audio_tokens")?.as_u64()?;
    if input_text.checked_add(input_audio)? != input_total
        || output_text.checked_add(output_audio)? != output_total
        || cached_text > input_text
        || cached_audio > input_audio
    {
        return None;
    }

    Some(crate::types::RealtimeUsage {
        text_input_tokens: input_text.saturating_sub(cached_text),
        text_cached_input_tokens: cached_text,
        text_output_tokens: output_text,
        audio_input_tokens: input_audio.saturating_sub(cached_audio),
        audio_cached_input_tokens: cached_audio,
        audio_output_tokens: output_audio,
        billed_seconds: 0.0,
    })
}
const AUDIO_CHANNEL_CAPACITY: usize = 64;
const CONTROL_CHANNEL_CAPACITY: usize = 16;
const EVENT_CHANNEL_CAPACITY: usize = 64;
const OPENAI_REALTIME_PCM_RATE: u32 = 24_000;
const OPENAI_REALTIME_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
pub const GROK_VOICE_DEFAULT_MODEL: &str = "grok-voice-latest";
pub const GROK_VOICE_DEFAULT_VOICE: &str = "eve";
pub const GROK_VOICE_DEFAULT_WEBSOCKET_URL: &str = "wss://api.x.ai/v1/realtime";
/// Grok's published cap is 120 minutes. Rotate before that cutoff.
pub const GROK_VOICE_MAX_SESSION_SECS: u64 = 110 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RealtimeWireDialect {
    OpenAi,
    Grok,
}

fn input_transcription_is_disabled(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "none" | "off" | "disabled" | "local"
    )
}

#[derive(Debug, Clone)]
pub struct OpenAiRealtimeProvider {
    client: Arc<Client>,
    api_key: String,
    base_url: String,
    default_model: String,
    default_voice: Option<String>,
    topology: RealtimeAudioTopology,
    /// Provider-level overrides plumbed through from the
    /// `RealtimeVoiceProfile`. Surfaced on each minted
    /// [`RealtimeSessionDescriptor`] so the frontend can install
    /// them without re-fetching config.
    transcription_model: Option<String>,
    transcription_fallback_model: Option<String>,
    turn_detection_mode: Option<String>,
    context_window_tokens: Option<u64>,
    wire: RealtimeWireDialect,
    max_session_duration_override: Option<u64>,
}

impl OpenAiRealtimeProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(
            Arc::new(default_http_client()),
            api_key,
            OPENAI_REALTIME_DEFAULT_BASE_URL,
        )
    }

    pub fn with_client(
        client: Arc<Client>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            api_key: api_key.into(),
            base_url: base_url.into(),
            default_model: OPENAI_REALTIME_DEFAULT_MODEL.to_string(),
            default_voice: Some(OPENAI_REALTIME_DEFAULT_VOICE.to_string()),
            topology: RealtimeAudioTopology::DirectPeerToPeer,
            transcription_model: None,
            transcription_fallback_model: None,
            turn_detection_mode: None,
            context_window_tokens: None,
            wire: RealtimeWireDialect::OpenAi,
            max_session_duration_override: None,
        }
    }

    pub fn with_defaults(mut self, model: impl Into<String>, voice: Option<String>) -> Self {
        self.default_model = model.into();
        self.default_voice = voice;
        self
    }

    pub fn backend_proxied(mut self) -> Self {
        self.topology = RealtimeAudioTopology::BackendProxied;
        self
    }

    /// Backend-proxied Grok speech-to-speech. Reuses the OpenAI Realtime
    /// socket reader; session.update uses Grok's flatter payload.
    pub fn grok_voice(mut self) -> Self {
        self.wire = RealtimeWireDialect::Grok;
        self.topology = RealtimeAudioTopology::BackendProxied;
        self
    }

    pub fn with_max_session_duration_secs(mut self, secs: Option<u64>) -> Self {
        self.max_session_duration_override = secs.filter(|secs| *secs > 0);
        self
    }

    pub fn with_profile_overrides(
        mut self,
        transcription_model: Option<String>,
        transcription_fallback_model: Option<String>,
        turn_detection_mode: Option<String>,
        context_window_tokens: Option<u64>,
    ) -> Self {
        self.transcription_model = transcription_model;
        self.transcription_fallback_model = transcription_fallback_model;
        self.turn_detection_mode = turn_detection_mode;
        self.context_window_tokens = context_window_tokens;
        self
    }

    fn session_cap_secs(&self) -> u64 {
        self.max_session_duration_override.unwrap_or(match self.wire {
            RealtimeWireDialect::OpenAi => OPENAI_REALTIME_MAX_SESSION_SECS,
            RealtimeWireDialect::Grok => GROK_VOICE_MAX_SESSION_SECS,
        })
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn default_voice_id(&self) -> Option<&str> {
        self.default_voice.as_deref()
    }
}

// GA request body shape for `/v1/realtime/client_secrets`. Model and optional
// voice live under `session.audio.output.voice` — the flat `{model, voice}`
// shape was the beta-only convention and is rejected by the GA endpoint.
#[derive(Serialize)]
struct CreateClientSecretRequest<'a> {
    session: SessionConfig<'a>,
}

#[derive(Serialize)]
struct SessionConfig<'a> {
    #[serde(rename = "type")]
    session_type: &'a str,
    model: &'a str,
    audio: AudioConfig<'a>,
}

#[derive(Serialize)]
struct AudioConfig<'a> {
    output: AudioOutputConfig<'a>,
}

#[derive(Serialize)]
struct AudioOutputConfig<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    voice: Option<&'a str>,
}

#[async_trait]
impl RealtimeProvider for OpenAiRealtimeProvider {
    fn id(&self) -> &str {
        match self.wire {
            RealtimeWireDialect::OpenAi => "openai-realtime",
            RealtimeWireDialect::Grok => "grok-voice",
        }
    }

    fn kind(&self) -> RealtimeProviderKind {
        match self.wire {
            RealtimeWireDialect::OpenAi => RealtimeProviderKind::OpenAi,
            RealtimeWireDialect::Grok => RealtimeProviderKind::Grok,
        }
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn audio_topology(&self) -> RealtimeAudioTopology {
        self.topology
    }

    async fn create_session(
        &self,
        _principal: &str,
        _workspace: &str,
        voice_session_id: &str,
        _thread_id: Option<&str>,
        preferred_voice: Option<&str>,
    ) -> Result<RealtimeSessionDescriptor, RealtimeProviderError> {
        let voice_catalog = match self.wire {
            RealtimeWireDialect::OpenAi => "openai_realtime",
            RealtimeWireDialect::Grok => "grok_voice",
        };
        let session_voice = preferred_voice
            .and_then(|value| crate::realtime::canonical_realtime_voice(voice_catalog, value))
            .or_else(|| self.default_voice.clone());
        if matches!(self.topology, RealtimeAudioTopology::BackendProxied) {
            return Ok(RealtimeSessionDescriptor {
                provider: self.kind(),
                model: self.default_model.clone(),
                topology: RealtimeAudioTopology::BackendProxied,
                mode: crate::config::RealtimeVoiceMode::Assistant,
                voice: session_voice,
                webrtc_url: None,
                upstream_token: None,
                upstream_provider_session_id: Some(format!("openai-ws-{voice_session_id}")),
                max_session_duration_secs: Some(self.session_cap_secs()),
                native_resume_handle: None,
                transcription_model: self.transcription_model.clone(),
                transcription_fallback_model: self.transcription_fallback_model.clone(),
                turn_detection_mode: self.turn_detection_mode.clone(),
                context_window_tokens: self.context_window_tokens,
                half_duplex: None,
            });
        }

        let body = CreateClientSecretRequest {
            session: SessionConfig {
                session_type: "realtime",
                model: &self.default_model,
                audio: AudioConfig {
                    output: AudioOutputConfig {
                        voice: session_voice.as_deref(),
                    },
                },
            },
        };
        info!(
            "[REALTIME-VOICE] bootstrapping client_secret url={} model={} voice={}",
            self.base_url,
            self.default_model,
            session_voice.as_deref().unwrap_or("<provider-default>")
        );
        let upstream = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            // Token mint is a single round-trip; 30 s is generous
            // even on slow networks. Without this the actix worker
            // would hang on an unresponsive OpenAI endpoint until
            // the OS-level TCP timeout (minutes), blocking the
            // upstream-session bootstrap path and any caller behind
            // it.
            .timeout(Duration::from_secs(30))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                error!("[REALTIME-VOICE] transport failure: {e}");
                RealtimeProviderError::Upstream(e.to_string())
            })?;
        let status = upstream.status();
        if !status.is_success() {
            let body = upstream.text().await.unwrap_or_default();
            error!(
                "[REALTIME-VOICE] upstream rejected client_secret status={} body={}",
                status, body
            );
            return Err(RealtimeProviderError::Upstream(format!(
                "openai realtime client_secret failed: {status}: {body}"
            )));
        }
        info!(
            "[REALTIME-VOICE] upstream client_secret accepted status={} model={}",
            status, self.default_model
        );
        // GA response shape: top-level `value` field with `ek_…`
        // prefix is the ephemeral token. Beta returned the value
        // nested under `client_secret.value`. We try the new shape
        // first then fall back to the old one for compatibility
        // with OpenAI-compatible local servers that still ship the
        // beta contract.
        let response: Value = upstream
            .json()
            .await
            .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
        let upstream_token = response
            .get("value")
            .and_then(|v| v.as_str())
            .or_else(|| {
                response
                    .get("client_secret")
                    .and_then(|cs| cs.get("value"))
                    .and_then(|v| v.as_str())
            })
            .map(str::to_owned);
        let session_id_from_response = response
            .get("session")
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        Ok(RealtimeSessionDescriptor {
            provider: RealtimeProviderKind::OpenAi,
            model: self.default_model.clone(),
            topology: RealtimeAudioTopology::DirectPeerToPeer,
            mode: crate::config::RealtimeVoiceMode::Assistant,
            voice: session_voice,
            webrtc_url: Some(format!(
                "https://api.openai.com/v1/realtime/calls?model={}",
                self.default_model
            )),
            upstream_token,
            upstream_provider_session_id: session_id_from_response,
            max_session_duration_secs: Some(OPENAI_REALTIME_MAX_SESSION_SECS),
            native_resume_handle: None,
            transcription_model: self.transcription_model.clone(),
            transcription_fallback_model: self.transcription_fallback_model.clone(),
            turn_detection_mode: self.turn_detection_mode.clone(),
            context_window_tokens: self.context_window_tokens,
            half_duplex: None,
        })
    }

    fn supports_native_resume(&self) -> bool {
        // OpenAI Realtime today has no native resume — the client
        // owns context replay. Flip to `true` if a future GA brings
        // a `session.resume` flow.
        false
    }

    async fn open_proxied_audio(
        &self,
        descriptor: &RealtimeSessionDescriptor,
    ) -> Result<AudioStreamChannel, RealtimeProviderError> {
        if !matches!(self.topology, RealtimeAudioTopology::BackendProxied) {
            return Err(RealtimeProviderError::UnsupportedTopology(format!(
                "provider `{}` is not configured for backend-proxied OpenAI Realtime",
                self.id()
            )));
        }

        let (upstream_tx, upstream_rx) = mpsc::channel::<Vec<u8>>(AUDIO_CHANNEL_CAPACITY);
        let (downstream_tx, downstream_rx) = mpsc::channel::<Vec<u8>>(AUDIO_CHANNEL_CAPACITY);
        let (control_tx, control_rx) =
            mpsc::channel::<RealtimeAudioControl>(CONTROL_CHANNEL_CAPACITY);
        let (events_tx, events_rx) = mpsc::channel::<RealtimeProviderEvent>(EVENT_CHANNEL_CAPACITY);

        let requested_transcription_model = descriptor
            .transcription_model
            .clone()
            .or_else(|| self.transcription_model.clone());
        let configured_transcription_model = if self.wire == RealtimeWireDialect::Grok {
            String::new()
        } else {
            match requested_transcription_model {
            Some(model) if input_transcription_is_disabled(&model) => descriptor
                .transcription_fallback_model
                .clone()
                .or_else(|| self.transcription_fallback_model.clone())
                .filter(|model| !model.trim().is_empty())
                .ok_or_else(|| {
                    RealtimeProviderError::NotConfigured(
                        "local realtime transcription requires transcription_fallback_model"
                            .to_string(),
                    )
                })?,
            Some(model) if !model.trim().is_empty() => model,
            _ => {
                return Err(RealtimeProviderError::NotConfigured(
                    "OpenAI realtime transcription_model is not configured".to_string(),
                ));
            },
            }
        };
        let task = OpenAiBackendRealtimeTask {
            api_key: self.api_key.clone(),
            websocket_url: self.base_url.clone(),
            model: descriptor.model.clone(),
            voice: descriptor
                .voice
                .clone()
                .or_else(|| self.default_voice.clone()),
            // A `local` profile request is activated only after Magician has
            // opened the call-scoped STT stream. Until ConfigureSession carries
            // that explicit override, retain Whisper as the fail-safe default.
            transcription_model: configured_transcription_model,
            turn_detection_mode: descriptor
                .turn_detection_mode
                .clone()
                .or_else(|| self.turn_detection_mode.clone())
                .unwrap_or_else(|| "none".to_string()),
            wire: self.wire,
        };
        tokio::spawn(async move {
            // Match Gemini: a dropped upstream socket is a transport close the
            // control actor can rotate, not a fatal call-ending Error. Ending
            // cleanly via `End` returns Ok and sends nothing.
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
        });

        Ok(AudioStreamChannel {
            upstream_tx,
            downstream_rx,
            control_tx,
            events_rx,
        })
    }

    fn max_session_duration_secs(&self) -> Option<u64> {
        Some(self.session_cap_secs())
    }

    fn compaction_token_watermark(&self) -> f32 {
        // 70 % of the context window — empirically the point where
        // generation latency starts climbing and tool-call accuracy
        // degrades on `gpt-realtime`. Tune downward (e.g. 0.6) via
        // the magician-config profile for chattier domains.
        0.7
    }
}

struct OpenAiBackendRealtimeTask {
    api_key: String,
    websocket_url: String,
    model: String,
    voice: Option<String>,
    transcription_model: String,
    turn_detection_mode: String,
    wire: RealtimeWireDialect,
}

/// Provider-authoritative guard for OpenAI's stateful `response.cancel` command.
///
/// OpenAI rejects cancellation when no response exists. Native wake/engage and
/// server VAD can race response creation, and several independent barge-in
/// signals can target the same response. Track the lifecycle observed on the
/// provider socket so a no-op or duplicate interrupt never becomes a visible
/// protocol error.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct OpenAiResponseCancelGate {
    active: bool,
    cancel_pending: bool,
}

impl OpenAiResponseCancelGate {
    fn observe_server_event(&mut self, event_type: &str) {
        match event_type {
            "response.created" => {
                self.active = true;
                self.cancel_pending = false;
            },
            "response.done" => {
                self.active = false;
                self.cancel_pending = false;
            },
            _ => {},
        }
    }

    fn admit_cancel(&mut self) -> bool {
        if !self.active || self.cancel_pending {
            return false;
        }
        self.cancel_pending = true;
        true
    }
}

impl OpenAiBackendRealtimeTask {
    async fn run(
        self,
        mut upstream_rx: mpsc::Receiver<Vec<u8>>,
        downstream_tx: mpsc::Sender<Vec<u8>>,
        mut control_rx: mpsc::Receiver<RealtimeAudioControl>,
        events_tx: mpsc::Sender<RealtimeProviderEvent>,
    ) -> Result<(), RealtimeProviderError> {
        let url = openai_realtime_ws_url(&self.websocket_url, &self.model);
        info!(
            "[REALTIME-VOICE] opening backend-proxied OpenAI WebSocket url={} model={} voice={}",
            url,
            self.model,
            self.voice.as_deref().unwrap_or("<provider-default>")
        );
        let mut request = url
            .into_client_request()
            .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?,
        );
        let (ws, _) = tokio::time::timeout(OPENAI_REALTIME_CONNECT_TIMEOUT, connect_async(request))
            .await
            .map_err(|_| {
                RealtimeProviderError::Upstream(format!(
                    "OpenAI Realtime websocket handshake timed out after {}ms",
                    OPENAI_REALTIME_CONNECT_TIMEOUT.as_millis()
                ))
            })?
            .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
        info!(
            model = %self.model,
            "[REALTIME-VOICE] backend-proxied OpenAI WebSocket connected"
        );
        let _ = events_tx.send(RealtimeProviderEvent::TransportReady).await;
        let (mut write, mut read) = ws.split();
        let mut pending_configure_ids = VecDeque::<Option<String>>::new();
        let mut transcripts = OpenAiTranscriptState::default();
        // Magician still has to render instructions/tools before the full
        // ConfigureSession. Send a minimal session.update now so OpenAI does
        // not idle-close the socket during that window, and so VAD/PCM are
        // live as soon as the client starts sending audio. Queue a None ack
        // so this bootstrap `session.updated` cannot steal a later
        // ConfigureSession event_id.
        write
            .send(Message::Text(self.bootstrap_session_payload().to_string()))
            .await
            .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
        pending_configure_ids.push_back(None);
        let mut defer_response_until_context = false;
        let mut active_turn_context_item_id: Option<String> = None;
        let mut response_cancel_gate = OpenAiResponseCancelGate::default();

        loop {
            tokio::select! {
                biased;
                maybe_control = control_rx.recv() => {
                    let Some(control) = maybe_control else { break; };
                    match control {
                        RealtimeAudioControl::ConfigureSession {
                            instructions,
                            tools,
                            input_transcription_model,
                            update_id,
                            defer_response_until_context: defer_response,
                        } => {
                            let payload = self.configure_session_payload(
                                &instructions,
                                &tools,
                                input_transcription_model.as_deref(),
                                update_id.as_deref(),
                                defer_response,
                            );
                            write.send(Message::Text(payload.to_string())).await
                                .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            pending_configure_ids.push_back(update_id);
                            defer_response_until_context = defer_response;
                        },
                        RealtimeAudioControl::ClearInput => {
                            write.send(Message::Text(json!({
                                "type": "input_audio_buffer.clear"
                            }).to_string())).await
                                .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                        },
                        RealtimeAudioControl::InterruptResponse => {
                            if response_cancel_gate.admit_cancel() {
                                write.send(Message::Text(json!({
                                    "type": "response.cancel"
                                }).to_string())).await
                                    .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            } else {
                                debug!(
                                    active = response_cancel_gate.active,
                                    cancel_pending = response_cancel_gate.cancel_pending,
                                    "[REALTIME-VOICE] skipped response.cancel without an interruptible response"
                                );
                            }
                        },
                        RealtimeAudioControl::CommitInputAndRespond => {
                            // Audio frames and controls arrive on separate
                            // channels. Flush any mic chunks already queued
                            // before committing the input buffer, otherwise a
                            // release control can overtake the tail of the
                            // user's utterance under scheduler pressure.
                            while let Ok(frame) = upstream_rx.try_recv() {
                                send_audio_append(&mut write, frame).await?;
                            }
                            write.send(Message::Text(json!({
                                "type": "input_audio_buffer.commit"
                            }).to_string())).await
                                .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            if !defer_response_until_context {
                                write.send(Message::Text(json!({
                                    "type": "response.create"
                                }).to_string())).await
                                    .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            }
                        },
                        RealtimeAudioControl::ToolResult { call_id, output } => {
                            write.send(Message::Text(json!({
                                "type": "conversation.item.create",
                                "item": {
                                    "type": "function_call_output",
                                    "call_id": call_id,
                                    "output": output,
                                }
                            }).to_string())).await
                                .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            write.send(Message::Text(json!({
                                "type": "response.create"
                            }).to_string())).await
                                .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                        },
                        RealtimeAudioControl::InjectInitialHistory { text } => {
                            write.send(Message::Text(json!({
                                "type": "conversation.item.create",
                                "item": {
                                    "type": "message",
                                    "role": "system",
                                    "content": [{ "type": "input_text", "text": text }],
                                }
                            }).to_string())).await
                                .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                        },
                        RealtimeAudioControl::InjectToolExchange {
                            call_id,
                            tool_name,
                            arguments,
                            projected_result,
                        } => {
                            for payload in openai_realtime_tool_exchange_payloads(
                                &call_id,
                                &tool_name,
                                &arguments,
                                &projected_result,
                            ) {
                                write.send(Message::Text(payload.to_string())).await
                                    .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            }
                        },
                        RealtimeAudioControl::InjectSystemMessage { text, request_response } => {
                            write.send(Message::Text(json!({
                                "type": "conversation.item.create",
                                "item": {
                                    "type": "message",
                                    "role": "system",
                                    "content": [{ "type": "input_text", "text": text }],
                                }
                            }).to_string())).await
                                .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            if request_response {
                                write.send(Message::Text(json!({
                                    "type": "response.create"
                                }).to_string())).await
                                    .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            }
                        },
                        RealtimeAudioControl::RespondWithTurnContext {
                            context_item_id,
                            context,
                        } => {
                            let has_context = context
                                .as_deref()
                                .is_some_and(|value| !value.trim().is_empty());
                            if response_cancel_gate.admit_cancel() {
                                write
                                    .send(Message::Text(
                                        json!({ "type": "response.cancel" }).to_string(),
                                    ))
                                    .await
                                    .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            }
                            for payload in turn_context_response_payloads(
                                active_turn_context_item_id.as_deref(),
                                &context_item_id,
                                context.as_deref(),
                            ) {
                                write.send(Message::Text(payload.to_string())).await
                                    .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                            }
                            active_turn_context_item_id = has_context.then_some(context_item_id);
                        },
                        RealtimeAudioControl::SynthesizeResponse { .. } => {
                            // Magician's cascaded TTS control is consumed only by
                            // the local hands-free provider.
                        },
                        RealtimeAudioControl::End => {
                            let _ = write.send(Message::Close(None)).await;
                            break;
                        },
                    }
                },
                maybe_frame = upstream_rx.recv() => {
                    let Some(frame) = maybe_frame else { break; };
                    send_audio_append(&mut write, frame).await?;
                },
                maybe_msg = read.next() => {
                    let Some(message) = maybe_msg else {
                        return Err(RealtimeProviderError::Upstream(
                            "OpenAI Realtime connection ended without a close frame".to_string(),
                        ));
                    };
                    match message {
                        Ok(Message::Text(text)) => {
                            let event_type = serde_json::from_str::<Value>(&text)
                                .ok()
                                .and_then(|event| event.get("type").and_then(Value::as_str).map(str::to_string))
                                .unwrap_or_default();
                            response_cancel_gate.observe_server_event(&event_type);
                            handle_openai_realtime_server_event(
                                &text,
                                &downstream_tx,
                                &events_tx,
                                &mut pending_configure_ids,
                                &mut transcripts,
                            ).await?;
                            if event_type == "response.done" {
                                if let Some(context_item_id) = active_turn_context_item_id.take() {
                                    write.send(Message::Text(json!({
                                        "type": "conversation.item.delete",
                                        "item_id": context_item_id,
                                    }).to_string())).await
                                        .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
                                }
                            }
                        },
                        Ok(Message::Binary(_bytes)) => {
                            // OpenAI Realtime WebSocket sends audio as Base64
                            // JSON deltas. Ignore unexpected binary frames.
                        },
                        Ok(Message::Close(frame)) => {
                            return Err(RealtimeProviderError::Upstream(format!(
                                "OpenAI Realtime connection closed{}",
                                frame
                                    .map(|frame| format!(" ({}: {})", frame.code, frame.reason))
                                    .unwrap_or_default()
                            )));
                        },
                        Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {},
                        Err(e) => return Err(RealtimeProviderError::Upstream(e.to_string())),
                    }
                },
                else => break,
            }
        }
        Ok(())
    }

    fn bootstrap_session_payload(&self) -> Value {
        if self.wire == RealtimeWireDialect::Grok {
            return grok_session_update(None, self.voice.as_deref(), &self.turn_detection_mode, None);
        }
        let turn_detection = if self.turn_detection_mode.trim().eq_ignore_ascii_case("none") {
            Value::Null
        } else {
            json!({
                "type": self.turn_detection_mode,
                "create_response": true,
            })
        };
        let transcription = if input_transcription_is_disabled(&self.transcription_model) {
            Value::Null
        } else {
            json!({ "model": self.transcription_model })
        };
        json!({
            "type": "session.update",
            "session": {
                "type": "realtime",
                "output_modalities": ["audio"],
                "audio": {
                    "input": {
                        "format": {
                            "type": "audio/pcm",
                            "rate": OPENAI_REALTIME_PCM_RATE,
                        },
                        "transcription": transcription,
                        "turn_detection": turn_detection,
                    },
                    "output": {
                        "format": {
                            "type": "audio/pcm",
                            "rate": OPENAI_REALTIME_PCM_RATE,
                        }
                    },
                },
            },
        })
    }

    fn configure_session_payload(
        &self,
        instructions: &str,
        tools: &[LLMToolSpec],
        input_transcription_model: Option<&str>,
        update_id: Option<&str>,
        defer_response_until_context: bool,
    ) -> Value {
        if self.wire == RealtimeWireDialect::Grok {
            let _ = (input_transcription_model, defer_response_until_context);
            let mut payload = grok_session_update(
                Some(instructions),
                self.voice.as_deref(),
                &self.turn_detection_mode,
                Some(tools),
            );
            if let Some(update_id) = update_id {
                payload["event_id"] = json!(update_id);
            }
            return payload;
        }
        let turn_detection = if self.turn_detection_mode.trim().eq_ignore_ascii_case("none") {
            Value::Null
        } else {
            json!({
                "type": self.turn_detection_mode,
                "create_response": !defer_response_until_context,
            })
        };
        let transcription_model = input_transcription_model.unwrap_or(&self.transcription_model);
        let transcription = if input_transcription_is_disabled(transcription_model) {
            Value::Null
        } else {
            json!({ "model": transcription_model })
        };
        let mut payload = json!({
            "type": "session.update",
            "session": {
                "type": "realtime",
                "instructions": instructions,
                "output_modalities": ["audio"],
                "audio": {
                    "input": {
                        "format": {
                            "type": "audio/pcm",
                            "rate": OPENAI_REALTIME_PCM_RATE,
                        },
                        "transcription": transcription,
                        "turn_detection": turn_detection,
                    },
                    "output": {
                        "format": {
                            "type": "audio/pcm",
                            "rate": OPENAI_REALTIME_PCM_RATE,
                        }
                    },
                },
                "tools": tools.iter().map(openai_realtime_tool_spec).collect::<Vec<_>>(),
            },
        });
        if let Some(voice) = self
            .voice
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            payload["session"]["audio"]["output"]["voice"] = json!(voice);
        }
        if let Some(update_id) = update_id {
            payload["event_id"] = json!(update_id);
        }
        payload
    }
}

/// Build the response-gate sequence independently of transport I/O so the
/// replace-before-create invariant and timeout/failure fail-open path remain
/// deterministic under regression tests.
fn grok_session_update(
    instructions: Option<&str>,
    voice: Option<&str>,
    turn_detection_mode: &str,
    tools: Option<&[LLMToolSpec]>,
) -> Value {
    let turn_detection = if turn_detection_mode.trim().eq_ignore_ascii_case("none") {
        Value::Null
    } else {
        json!({ "type": "server_vad" })
    };
    let mut session = json!({
        "turn_detection": turn_detection,
        "audio": {
            "input": {
                "format": { "type": "audio/pcm", "rate": OPENAI_REALTIME_PCM_RATE }
            },
            "output": {
                "format": { "type": "audio/pcm", "rate": OPENAI_REALTIME_PCM_RATE }
            }
        }
    });
    if let Some(instructions) = instructions {
        session["instructions"] = json!(instructions);
    }
    if let Some(voice) = voice.filter(|value| !value.trim().is_empty()) {
        session["voice"] = json!(voice);
    }
    if let Some(tools) = tools {
        session["tools"] = json!(tools.iter().map(openai_realtime_tool_spec).collect::<Vec<_>>());
    }
    json!({
        "type": "session.update",
        "session": session,
    })
}

fn turn_context_response_payloads(
    previous_context_item_id: Option<&str>,
    context_item_id: &str,
    context: Option<&str>,
) -> Vec<Value> {
    let mut payloads = Vec::with_capacity(3);
    if let Some(previous) = previous_context_item_id {
        payloads.push(json!({
            "type": "conversation.item.delete",
            "item_id": previous,
        }));
    }
    if let Some(context) = context.filter(|value| !value.trim().is_empty()) {
        payloads.push(json!({
            "type": "conversation.item.create",
            "item": {
                "id": context_item_id,
                "type": "message",
                "role": "system",
                "content": [{ "type": "input_text", "text": context }],
            }
        }));
    }
    payloads.push(json!({ "type": "response.create" }));
    payloads
}

fn openai_realtime_ws_url(base: &str, model: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.contains('?') {
        format!("{base}&model={model}")
    } else {
        format!("{base}?model={model}")
    }
}

fn openai_realtime_tool_spec(tool: &LLMToolSpec) -> Value {
    json!({
        "type": "function",
        "name": tool.name.clone(),
        "description": tool.description.clone(),
        "parameters": tool.parameters.clone(),
    })
}

async fn send_audio_append<W>(write: &mut W, frame: Vec<u8>) -> Result<(), RealtimeProviderError>
where
    W: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let payload = json!({
        "type": "input_audio_buffer.append",
        "audio": BASE64_STANDARD.encode(frame),
    });
    write
        .send(Message::Text(payload.to_string()))
        .await
        .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
    Ok(())
}

#[derive(Default)]
struct OpenAiTranscriptState {
    assistant_by_response: HashMap<String, String>,
    user_by_item: HashMap<String, String>,
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

async fn handle_openai_realtime_server_event(
    text: &str,
    downstream_tx: &mpsc::Sender<Vec<u8>>,
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    pending_configure_ids: &mut VecDeque<Option<String>>,
    transcripts: &mut OpenAiTranscriptState,
) -> Result<(), RealtimeProviderError> {
    let event: Value = serde_json::from_str(text)
        .map_err(|e| RealtimeProviderError::Upstream(format!("invalid realtime event: {e}")))?;
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type {
        "session.updated" => {
            let update_id = pending_configure_ids.pop_front().flatten();
            let _ = events_tx
                .send(RealtimeProviderEvent::SessionConfigured { update_id })
                .await;
        },
        "input_audio_buffer.speech_started" => {
            let _ = events_tx.send(RealtimeProviderEvent::SpeechStarted).await;
        },
        "input_audio_buffer.speech_stopped" => {
            let _ = events_tx.send(RealtimeProviderEvent::SpeechStopped).await;
        },
        "conversation.item.input_audio_transcription.delta"
        | "conversation.item.input_audio.transcription.delta" => {
            let item_id = event
                .get("item_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("openai-user")
                .to_string();
            let delta = event
                .get("delta")
                .and_then(Value::as_str)
                .or_else(|| event.get("transcript").and_then(Value::as_str))
                .unwrap_or_default();
            if !delta.is_empty() {
                let snapshot = {
                    let buffer = transcripts.user_by_item.entry(item_id.clone()).or_default();
                    absorb_transcript_fragment(buffer, delta);
                    buffer.trim().to_string()
                };
                if !snapshot.is_empty() {
                    let _ = events_tx
                        .send(RealtimeProviderEvent::UserTranscriptPartial {
                            text: snapshot,
                            item_id,
                        })
                        .await;
                }
            }
        },
        "conversation.item.input_audio_transcription.completed"
        | "conversation.item.input_audio.transcription.completed" => {
            let item_id = event
                .get("item_id")
                .and_then(Value::as_str)
                .unwrap_or("openai-user")
                .to_string();
            transcripts.user_by_item.remove(&item_id);
            let text = event
                .get("transcript")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if !text.is_empty() {
                let _ = events_tx
                    .send(RealtimeProviderEvent::UserTranscriptFinal { text, item_id })
                    .await;
            }
        },
        "response.audio.delta" | "response.output_audio.delta" => {
            if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                match BASE64_STANDARD.decode(delta) {
                    Ok(bytes) if !bytes.is_empty() => {
                        let _ = downstream_tx.send(bytes).await;
                    },
                    Ok(_) => {},
                    Err(err) => {
                        let _ = events_tx
                            .send(RealtimeProviderEvent::Error {
                                message: format!("OpenAI audio delta decode failed: {err}"),
                                recoverable: true,
                            })
                            .await;
                    },
                }
            }
        },
        "response.audio_transcript.delta"
        | "response.output_audio_transcript.delta"
        | "response.output_audio.transcript.delta" => {
            let response_id = event
                .get("response_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let text = event
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !response_id.is_empty() && !text.is_empty() {
                let snapshot = {
                    let buffer = transcripts
                        .assistant_by_response
                        .entry(response_id.clone())
                        .or_default();
                    absorb_transcript_fragment(buffer, &text);
                    buffer.clone()
                };
                let _ = events_tx
                    .send(RealtimeProviderEvent::AssistantTranscriptDelta {
                        response_id,
                        text: snapshot,
                    })
                    .await;
            }
        },
        "response.audio_transcript.done"
        | "response.output_audio_transcript.done"
        | "response.output_audio.transcript.done" => {
            let response_id = event
                .get("response_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            transcripts.assistant_by_response.remove(&response_id);
            let text = event
                .get("transcript")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if !response_id.is_empty() && !text.is_empty() {
                let _ = events_tx
                    .send(RealtimeProviderEvent::AssistantTranscriptFinal { response_id, text })
                    .await;
            }
        },
        "response.function_call_arguments.done" => {
            let response_id = event
                .get("response_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let call_id = event
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let name = event
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let arguments_json = event
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}")
                .to_string();
            if !call_id.is_empty() && !name.is_empty() {
                let _ = events_tx
                    .send(RealtimeProviderEvent::FunctionCall {
                        response_id,
                        call_id,
                        name,
                        arguments_json,
                    })
                    .await;
            }
        },
        "response.done" => {
            let response = event.get("response");
            let response_id = response
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let status = response
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str);
            if status != Some("completed") {
                let terminal_state = match status {
                    Some("cancelled") => crate::realtime::RealtimeResponseTerminalState::Cancelled,
                    Some("incomplete") => {
                        crate::realtime::RealtimeResponseTerminalState::Incomplete
                    },
                    _ => crate::realtime::RealtimeResponseTerminalState::Failed,
                };
                let _ = events_tx
                    .send(RealtimeProviderEvent::ResponseFailed {
                        response_id,
                        terminal_state,
                    })
                    .await;
                return Ok(());
            }
            let usage = response.and_then(|v| v.get("usage"));
            let input_tokens = usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(Value::as_u64);
            let output_tokens = usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64);
            let _ = events_tx
                .send(RealtimeProviderEvent::ResponseDone {
                    response_id,
                    input_tokens,
                    output_tokens,
                    usage: usage.and_then(parse_realtime_usage),
                })
                .await;
        },
        "error" => {
            let message = event
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("OpenAI Realtime error")
                .to_string();
            if is_benign_inactive_response_cancel_error(&message) {
                debug!(
                    error = %message,
                    "[REALTIME-VOICE] ignored stale response.cancel acknowledgement"
                );
                return Ok(());
            }
            warn!(
                error = %message,
                "[REALTIME-VOICE] OpenAI realtime error"
            );
            let _ = events_tx
                .send(RealtimeProviderEvent::Error {
                    message,
                    recoverable: true,
                })
                .await;
        },
        _ => {
            debug!("[REALTIME-VOICE] unhandled OpenAI realtime event: {event_type}");
        },
    }
    Ok(())
}

fn is_benign_inactive_response_cancel_error(message: &str) -> bool {
    let normalized = message.trim().to_ascii_lowercase();
    normalized.contains("cancel") && normalized.contains("no active response")
}

fn default_http_client() -> Client {
    Client::builder()
        .build()
        .expect("failed to build OpenAI realtime HTTP client")
}

fn openai_realtime_tool_exchange_payloads(
    call_id: &str,
    tool_name: &str,
    arguments: &Value,
    projected_result: &Value,
) -> [Value; 2] {
    let replay_call_id = openai_realtime_replay_call_id(call_id);
    let json_string = |value: &Value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    [
        json!({
            "type": "conversation.item.create",
            "item": {
                "type": "function_call",
                "call_id": replay_call_id.clone(),
                "name": tool_name,
                "arguments": json_string(arguments),
            }
        }),
        json!({
            "type": "conversation.item.create",
            "item": {
                "type": "function_call_output",
                "call_id": replay_call_id,
                "output": json_string(projected_result),
            }
        }),
    ]
}

/// OpenAI Realtime caps client-created historical `item.call_id` values at 32
/// ASCII bytes. Durable chat transcripts can legitimately contain longer IDs
/// from Responses and other providers, so preserve the canonical ID in storage
/// and derive a deterministic, collision-resistant alias only for this paired
/// replay wire shape. Live tool results keep the provider-issued ID unchanged.
const OPENAI_REALTIME_REPLAY_CALL_ID_MAX_BYTES: usize = 32;

fn openai_realtime_replay_call_id(canonical_call_id: &str) -> String {
    if !canonical_call_id.is_empty()
        && canonical_call_id.is_ascii()
        && canonical_call_id.len() <= OPENAI_REALTIME_REPLAY_CALL_ID_MAX_BYTES
    {
        return canonical_call_id.to_string();
    }
    let digest = blake3::hash(canonical_call_id.as_bytes())
        .to_hex()
        .to_string();
    format!("resume_{}", &digest[..25])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_cancel_requires_an_active_uncancelled_provider_response() {
        let mut gate = OpenAiResponseCancelGate::default();

        assert!(!gate.admit_cancel());
        gate.observe_server_event("response.created");
        assert!(gate.admit_cancel());
        assert!(!gate.admit_cancel(), "duplicate cancel must be coalesced");
        gate.observe_server_event("response.done");
        assert!(!gate.admit_cancel());

        gate.observe_server_event("response.created");
        assert!(
            gate.admit_cancel(),
            "the next response is independently interruptible"
        );
    }

    #[test]
    fn inactive_response_cancel_error_is_benign_but_other_errors_are_not() {
        assert!(is_benign_inactive_response_cancel_error(
            "Cancellation failed: no active response found"
        ));
        assert!(!is_benign_inactive_response_cancel_error(
            "Realtime authentication failed"
        ));
    }

    #[test]
    fn resume_tool_exchange_is_balanced_and_json_encoded_exactly_once() {
        let arguments = json!({ "query": "birthday" });
        let result = json!({
            "schema_version": 1,
            "outcome": { "status": "succeeded" },
            "data": { "records": [{ "value": "May 8" }] }
        });
        let payloads = openai_realtime_tool_exchange_payloads(
            "call-memory-1",
            "search_memory",
            &arguments,
            &result,
        );

        assert_eq!(payloads[0]["item"]["type"], "function_call");
        assert_eq!(payloads[0]["item"]["call_id"], "call-memory-1");
        assert_eq!(payloads[1]["item"]["type"], "function_call_output");
        assert_eq!(payloads[1]["item"]["call_id"], "call-memory-1");
        let decoded_arguments: Value = serde_json::from_str(
            payloads[0]["item"]["arguments"]
                .as_str()
                .expect("arguments string"),
        )
        .expect("arguments encoded once");
        let decoded_result: Value = serde_json::from_str(
            payloads[1]["item"]["output"]
                .as_str()
                .expect("output string"),
        )
        .expect("projection encoded once");
        assert_eq!(decoded_arguments, arguments);
        assert_eq!(decoded_result, result);
    }

    #[test]
    fn resume_tool_exchange_aliases_only_overlong_call_ids_at_the_openai_boundary() {
        let canonical = format!("call_{}", "x".repeat(55));
        assert_eq!(canonical.len(), 60);
        let payloads = openai_realtime_tool_exchange_payloads(
            &canonical,
            "search_memory",
            &json!({ "query": "birthday" }),
            &json!({ "status": "ok" }),
        );
        let replay_id = payloads[0]["item"]["call_id"]
            .as_str()
            .expect("replay call id");

        assert_ne!(replay_id, canonical);
        assert!(replay_id.is_ascii());
        assert!(replay_id.len() <= OPENAI_REALTIME_REPLAY_CALL_ID_MAX_BYTES);
        assert_eq!(payloads[1]["item"]["call_id"], replay_id);
        assert_eq!(
            openai_realtime_replay_call_id(&canonical),
            openai_realtime_replay_call_id(&canonical)
        );
        assert_ne!(
            openai_realtime_replay_call_id(&canonical),
            openai_realtime_replay_call_id(&format!("{canonical}-different"))
        );
        assert_eq!(openai_realtime_replay_call_id("call-short"), "call-short");
    }

    #[test]
    fn parse_realtime_usage_splits_text_audio_and_subtracts_cached() {
        // input: text 300 (100 cached), audio 1000 (400 cached); output text 50, audio 500.
        let usage = json!({
            "input_tokens": 1300,
            "output_tokens": 550,
            "input_token_details": {
                "text_tokens": 300,
                "audio_tokens": 1000,
                "cached_tokens_details": { "text_tokens": 100, "audio_tokens": 400 }
            },
            "output_token_details": { "text_tokens": 50, "audio_tokens": 500 }
        });
        let u = parse_realtime_usage(&usage).expect("complete usage");
        assert_eq!(u.text_input_tokens, 200); // 300 total − 100 cached
        assert_eq!(u.text_cached_input_tokens, 100);
        assert_eq!(u.text_output_tokens, 50);
        assert_eq!(u.audio_input_tokens, 600); // 1000 total − 400 cached
        assert_eq!(u.audio_cached_input_tokens, 400);
        assert_eq!(u.audio_output_tokens, 500);
    }

    #[test]
    fn parse_realtime_usage_rejects_missing_or_inconsistent_details() {
        assert_eq!(parse_realtime_usage(&json!({})), None);
        let cached_exceeds_modality = json!({
            "input_tokens": 10,
            "output_tokens": 0,
            "input_token_details": {
                "text_tokens": 0,
                "audio_tokens": 10,
                "cached_tokens_details": { "text_tokens": 0, "audio_tokens": 50 }
            },
            "output_token_details": { "text_tokens": 0, "audio_tokens": 0 }
        });
        assert_eq!(parse_realtime_usage(&cached_exceeds_modality), None);
        let totals_disagree = json!({
            "input_tokens": 11,
            "output_tokens": 1,
            "input_token_details": {
                "text_tokens": 0,
                "audio_tokens": 10,
                "cached_tokens_details": { "text_tokens": 0, "audio_tokens": 0 }
            },
            "output_token_details": { "text_tokens": 0, "audio_tokens": 0 }
        });
        assert_eq!(parse_realtime_usage(&totals_disagree), None);
    }

    #[tokio::test]
    async fn backend_response_done_preserves_non_success_terminal_state() {
        for (status, expected) in [
            (
                Some("incomplete"),
                crate::realtime::RealtimeResponseTerminalState::Incomplete,
            ),
            (
                Some("cancelled"),
                crate::realtime::RealtimeResponseTerminalState::Cancelled,
            ),
            (
                Some("unexpected"),
                crate::realtime::RealtimeResponseTerminalState::Failed,
            ),
            (None, crate::realtime::RealtimeResponseTerminalState::Failed),
        ] {
            let (audio_tx, _audio_rx) = mpsc::channel(1);
            let (events_tx, mut events_rx) = mpsc::channel(1);
            let mut pending = VecDeque::new();
            let mut transcripts = OpenAiTranscriptState::default();
            let mut response = json!({ "usage": { "input_tokens": 10 } });
            if let Some(status) = status {
                response["status"] = json!(status);
            }
            handle_openai_realtime_server_event(
                &json!({ "type": "response.done", "response": response }).to_string(),
                &audio_tx,
                &events_tx,
                &mut pending,
                &mut transcripts,
            )
            .await
            .expect("handle terminal response");

            assert_eq!(
                events_rx.recv().await,
                Some(RealtimeProviderEvent::ResponseFailed {
                    response_id: None,
                    terminal_state: expected,
                })
            );
        }
    }

    fn client_secret_payload(voice: Option<&str>) -> Value {
        serde_json::to_value(CreateClientSecretRequest {
            session: SessionConfig {
                session_type: "realtime",
                model: "gpt-realtime-2",
                audio: AudioConfig {
                    output: AudioOutputConfig { voice },
                },
            },
        })
        .expect("serialize client secret payload")
    }

    #[test]
    fn client_secret_payload_omits_voice_when_profile_does_not_pin_one() {
        let payload = client_secret_payload(None);

        assert!(payload["session"]["audio"]["output"]
            .as_object()
            .expect("output object")
            .get("voice")
            .is_none());
    }

    #[test]
    fn client_secret_payload_preserves_configured_voice() {
        let payload = client_secret_payload(Some("shimmer"));

        assert_eq!(payload["session"]["audio"]["output"]["voice"], "shimmer");
    }

    #[test]
    fn grok_session_update_uses_a_flat_voice_and_pcm() {
        let task = OpenAiBackendRealtimeTask {
            api_key: "test".to_string(),
            websocket_url: GROK_VOICE_DEFAULT_WEBSOCKET_URL.to_string(),
            model: GROK_VOICE_DEFAULT_MODEL.to_string(),
            voice: Some("eve".to_string()),
            transcription_model: String::new(),
            turn_detection_mode: "none".to_string(),
            wire: RealtimeWireDialect::Grok,
        };

        let payload = task.configure_session_payload("Be concise.", &[], None, None, true);

        assert_eq!(payload["session"]["voice"], "eve");
        assert_eq!(payload["session"]["instructions"], "Be concise.");
        assert!(payload["session"]["turn_detection"].is_null());
        assert_eq!(payload["session"]["audio"]["input"]["format"]["rate"], 24000);
        assert!(payload["session"].get("type").is_none());
        assert!(payload["session"]["audio"]["input"].get("transcription").is_none());
    }

    #[test]
    fn backend_session_update_omits_voice_when_profile_does_not_pin_one() {
        let task = OpenAiBackendRealtimeTask {
            api_key: "test".to_string(),
            websocket_url: OPENAI_REALTIME_DEFAULT_WEBSOCKET_URL.to_string(),
            model: "gpt-realtime-2".to_string(),
            voice: None,
            transcription_model: "whisper-1".to_string(),
            turn_detection_mode: "none".to_string(),
            wire: RealtimeWireDialect::OpenAi,
        };

        let payload = task.configure_session_payload("Be concise.", &[], None, None, false);

        assert!(payload["session"]["audio"]["output"]
            .as_object()
            .expect("output object")
            .get("voice")
            .is_none());
    }

    #[test]
    fn backend_session_update_preserves_configured_voice() {
        let task = OpenAiBackendRealtimeTask {
            api_key: "test".to_string(),
            websocket_url: OPENAI_REALTIME_DEFAULT_WEBSOCKET_URL.to_string(),
            model: "gpt-realtime-2".to_string(),
            voice: Some("marin".to_string()),
            transcription_model: "whisper-1".to_string(),
            turn_detection_mode: "none".to_string(),
            wire: RealtimeWireDialect::OpenAi,
        };

        let payload = task.configure_session_payload("Be concise.", &[], None, None, false);

        assert_eq!(payload["session"]["audio"]["output"]["voice"], "marin");
    }

    #[test]
    fn realtime_turn_context_backend_session_update_defers_vad_response() {
        let task = OpenAiBackendRealtimeTask {
            api_key: "test".to_string(),
            websocket_url: OPENAI_REALTIME_DEFAULT_WEBSOCKET_URL.to_string(),
            model: "gpt-realtime-2".to_string(),
            voice: None,
            transcription_model: "whisper-1".to_string(),
            turn_detection_mode: "server_vad".to_string(),
            wire: RealtimeWireDialect::OpenAi,
        };

        let deferred = task.configure_session_payload("Be concise.", &[], None, None, true);
        let established = task.configure_session_payload("Be concise.", &[], None, None, false);

        assert_eq!(
            deferred["session"]["audio"]["input"]["turn_detection"]["create_response"],
            false
        );
        assert_eq!(
            established["session"]["audio"]["input"]["turn_detection"]["create_response"],
            true
        );
    }

    #[test]
    fn backend_session_update_can_disable_vendor_input_transcription() {
        let task = OpenAiBackendRealtimeTask {
            api_key: "test".to_string(),
            websocket_url: OPENAI_REALTIME_DEFAULT_WEBSOCKET_URL.to_string(),
            model: "gpt-realtime-2".to_string(),
            voice: None,
            transcription_model: "whisper-1".to_string(),
            turn_detection_mode: "none".to_string(),
            wire: RealtimeWireDialect::OpenAi,
        };

        let payload =
            task.configure_session_payload("Be concise.", &[], Some("local"), None, false);

        assert!(payload["session"]["audio"]["input"]["transcription"].is_null());
    }

    #[test]
    fn backend_session_update_can_restore_configured_vendor_input_transcription() {
        let task = OpenAiBackendRealtimeTask {
            api_key: "test".to_string(),
            websocket_url: OPENAI_REALTIME_DEFAULT_WEBSOCKET_URL.to_string(),
            model: "gpt-realtime-2".to_string(),
            voice: None,
            transcription_model: "profile-fallback-stt".to_string(),
            turn_detection_mode: "none".to_string(),
            wire: RealtimeWireDialect::OpenAi,
        };

        let payload = task.configure_session_payload(
            "Be concise.",
            &[],
            Some("profile-fallback-stt"),
            Some("restore-1"),
            false,
        );

        assert_eq!(
            payload["session"]["audio"]["input"]["transcription"]["model"],
            "profile-fallback-stt"
        );
        assert_eq!(payload["event_id"], "restore-1");
    }

    #[test]
    fn realtime_turn_context_replaces_prior_item_and_creates_exactly_one_response() {
        let payloads = turn_context_response_payloads(
            Some("voice-context-old"),
            "voice-context-new",
            Some("Relevant memory"),
        );

        assert_eq!(payloads.len(), 3);
        assert_eq!(payloads[0]["type"], "conversation.item.delete");
        assert_eq!(payloads[0]["item_id"], "voice-context-old");
        assert_eq!(payloads[1]["type"], "conversation.item.create");
        assert_eq!(payloads[1]["item"]["id"], "voice-context-new");
        assert_eq!(payloads[1]["item"]["role"], "system");
        assert_eq!(payloads[2]["type"], "response.create");
        assert_eq!(
            payloads
                .iter()
                .filter(|payload| payload["type"] == "response.create")
                .count(),
            1
        );

        let fail_open = turn_context_response_payloads(None, "unused", None);
        assert_eq!(fail_open, vec![json!({ "type": "response.create" })]);
    }

    #[tokio::test]
    async fn session_updated_acknowledges_configurations_in_wire_order() {
        let (audio_tx, _audio_rx) = mpsc::channel(2);
        let (events_tx, mut events_rx) = mpsc::channel(2);
        let mut pending = VecDeque::from([None, Some("catalog-1".to_string())]);
        let mut transcripts = OpenAiTranscriptState::default();

        handle_openai_realtime_server_event(
            r#"{"type":"session.updated"}"#,
            &audio_tx,
            &events_tx,
            &mut pending,
            &mut transcripts,
        )
        .await
        .expect("initial ack");
        handle_openai_realtime_server_event(
            r#"{"type":"session.updated"}"#,
            &audio_tx,
            &events_tx,
            &mut pending,
            &mut transcripts,
        )
        .await
        .expect("catalog ack");

        assert_eq!(
            events_rx.recv().await,
            Some(RealtimeProviderEvent::SessionConfigured { update_id: None })
        );
        assert_eq!(
            events_rx.recv().await,
            Some(RealtimeProviderEvent::SessionConfigured {
                update_id: Some("catalog-1".to_string())
            })
        );
        assert!(pending.is_empty());
    }
}
