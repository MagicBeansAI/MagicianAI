//! Cost metering for text-to-speech and speech-to-text calls.
//!
//! Every real vendor call (never a cache hit) writes one `llm_calls` row,
//! like a model call: capability `voice.tts` / `voice.stt`, response kind
//! `voice_tts` / `voice_stt` / `voice_stt_streaming`, the vendor and model,
//! and a cost priced here from the pricing file's `media_rates`
//! (`magicllm::compute_media_cost_at`). The analytics bridge records that
//! figure as an estimate fingerprinted to the media row; the event carries no
//! characters or seconds, so these rows cannot be repriced later.
//!
//! What each call is billed on:
//! - TTS: the request's characters, and the output audio's length (exact for
//!   WAV and raw PCM, estimated from the first frame's bitrate for MP3).
//! - STT upload: the vendor's reported duration, else the upload's own length
//!   when it is WAV, raw PCM or MP3. A compressed upload (webm, ogg, m4a, ...)
//!   has no measured length, so a per-minute model records cost unknown.
//! - Streaming STT: the PCM pushed into the session, one row per session.
//!
//! Rows need a tenant scope: call sites wrap the call in
//! [`with_media_meter_scope`]. An unscoped call is not metered (logged at
//! debug), and neither is anything before [`install_media_meter`] runs.
//!
//! Local engines (Fluid, macOS speech) meter as vendor `local`: a computed
//! $0 row, so volume and latency show beside the paid calls.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::magician_v2::media_seam::providers_stt::{SttError, SttProvider, SttRequest, SttResponse, SttStreamEvent};
use crate::magician_v2::media_seam::streaming_stt_types::{
    AudioChunk, StreamAudioFormat, StreamSampleFormat, StreamingSttCapabilities, StreamingSttEvent,
    StreamingSttProvider, StreamingSttSession,
};
use crate::magician_v2::media_seam::tts_types::{
    TtsCacheStats, TtsError, TtsProvider, TtsRequest, TtsResponse,
};
use crate::magician_v2::realtime_events::{
    LlmEventCorrelation, RuntimeTransportBroadcaster, RuntimeTransportEvent,
};

/// The tenant a media call bills to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaMeterScope {
    pub principal: String,
    pub workspace: String,
    pub chat_session_id: Option<String>,
}

impl MediaMeterScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
            chat_session_id: None,
        }
    }

    pub fn with_chat_session(mut self, chat_session_id: Option<String>) -> Self {
        self.chat_session_id = chat_session_id;
        self
    }
}

tokio::task_local! {
    static MEDIA_METER_SCOPE: MediaMeterScope;
}

/// Run `fut` with its media calls billed to `scope`.
pub async fn with_media_meter_scope<F: std::future::Future>(scope: MediaMeterScope, fut: F) -> F::Output {
    MEDIA_METER_SCOPE.scope(scope, fut).await
}

fn current_scope() -> Option<MediaMeterScope> {
    MEDIA_METER_SCOPE.try_with(Clone::clone).ok()
}

static BROADCASTER: OnceLock<Arc<RuntimeTransportBroadcaster>> = OnceLock::new();

/// Connect metering to the runtime event bus, once at boot. Calls before this
/// are not metered.
pub fn install_media_meter(broadcaster: Arc<RuntimeTransportBroadcaster>) {
    let _ = BROADCASTER.set(broadcaster);
}

/// The vendor a TTS/STT adapter bills through, as the pricing file names it:
/// `local` for an on-machine engine, `None` for an adapter we cannot attribute.
pub fn media_vendor_for_adapter(adapter: &str) -> Option<&'static str> {
    Some(match adapter {
        "openai_speech" | "openai_transcriptions" | "openai_streaming_stt" | "openai_diarize_streaming_stt" => "openai",
        "minimax_t2a_v2" => "minimax",
        "gemini_tts" | "gemini_generate_content" | "gemini_transcribe" | "gemini_live_transcribe" => "gemini",
        "grok_tts" | "grok_stt" => "xai",
        "sarvam_tts" | "sarvam_stt" => "sarvam",
        "google_cloud_speech_v2" => "google_cloud_speech",
        "fluid_audio_kokoro_tts" | "fluid_audio_recording_stt" | "fluid_audio_streaming_eou_stt" | "macos_tts"
        | "macos_speech" | "macos_speech_streaming" => "local",
        _ => return None,
    })
}

/// One finished media call, ready to price and record.
#[derive(Debug, Clone)]
struct MediaCall<'a> {
    kind: magicllm::MediaKind,
    vendor: &'a str,
    model: &'a str,
    usage: magicllm::MediaUsage,
    latency_ms: u64,
    started_at_ms: i64,
    error: Option<String>,
}

fn record(call: MediaCall<'_>, scope: Option<MediaMeterScope>) {
    let Some(broadcaster) = BROADCASTER.get() else {
        return;
    };
    let Some(scope) = scope else {
        tracing::debug!(
            target: "voice_cost",
            kind = call.kind.as_str(),
            vendor = call.vendor,
            model = call.model,
            "media call without a meter scope; not recorded"
        );
        return;
    };
    let provider = magicllm::LLMProviderKind::from_str(call.vendor);
    let cost = magicllm::compute_media_cost_at(&provider, call.model, call.kind, &call.usage, call.started_at_ms);
    let priced = magicllm::media_pricing_version_at(&provider, call.model, call.kind, call.started_at_ms).is_some();
    tracing::info!(
        target: "voice_cost",
        kind = call.kind.as_str(),
        vendor = call.vendor,
        model = call.model,
        input_chars = call.usage.input_chars,
        audio_seconds = call.usage.audio_seconds,
        cost_usd = cost,
        priced,
        "media call cost"
    );
    let mut correlation = LlmEventCorrelation::direct(
        scope.principal.clone(),
        scope.workspace.clone(),
        magicllm::LlmWorkloadClass::ForegroundChat,
    );
    correlation.chat_session_id.clone_from(&scope.chat_session_id);
    let capability = match call.kind {
        magicllm::MediaKind::Tts => "voice.tts",
        magicllm::MediaKind::Stt | magicllm::MediaKind::SttStreaming => "voice.stt",
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    broadcaster.emit_transport_only(RuntimeTransportEvent::LLMResponseReceived {
        execution_id: format!("media:{}:{}", call.kind.as_str(), ulid::Ulid::new()),
        principal: Some(scope.principal),
        workspace: Some(scope.workspace),
        correlation: Some(correlation),
        plan_id: String::new(),
        step_id: None,
        step_index: None,
        capability: capability.to_string(),
        success: call.error.is_none(),
        decision_summary: String::new(),
        // Unpriced or unmeasured calls carry 0.0 with `usage_reported` telling
        // the bridge it is unknown, never a recorded zero.
        cost: cost.unwrap_or(0.0),
        latency_ms: call.latency_ms,
        error: call.error,
        provider: call.vendor.to_string(),
        model: call.model.to_string(),
        // No row is a pricing miss, which the bridge reports itself; a row
        // whose billed unit went unmeasured is usage the call did not report.
        usage_reported: cost.is_some() || !priced,
        input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: 0,
        reasoning_summary: None,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        audio_input_tokens: None,
        audio_output_tokens: None,
        audio_cached_tokens: None,
        search_calls: 0,
        ttft_ms: None,
        task_id: None,
        agent_id: None,
        delegated_agent_id: None,
        chat_session_id: scope.chat_session_id,
        operation: format!("voice_{}", call.kind.as_str()),
        profile: None,
        attempt: 0,
        response_kind: format!("voice_{}", call.kind.as_str()),
        started_at_ms: call.started_at_ms,
        timestamp: now_ms,
    });
}

// ---------------------------------------------------------------------------
// Audio length
// ---------------------------------------------------------------------------

/// Length in seconds of an audio payload, when its format says so exactly
/// (WAV, raw PCM with a `rate=` parameter) or closely enough (CBR MP3 from
/// its first frame). `None` for formats that need decoding.
pub fn audio_duration_seconds(audio: &[u8], content_type: &str) -> Option<f64> {
    if audio.starts_with(b"RIFF") && audio.get(8..12) == Some(b"WAVE") {
        return wav_duration_seconds(audio);
    }
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("pcm") || content_type.contains("l16") {
        let rate = content_type
            .split(';')
            .filter_map(|part| part.trim().strip_prefix("rate="))
            .find_map(|rate| rate.trim().parse::<u32>().ok())?;
        return pcm_seconds(audio.len() as u64, rate, 1, 2);
    }
    if content_type.contains("mpeg") || content_type.contains("mp3") || audio.starts_with(b"ID3") {
        return mp3_duration_seconds(audio);
    }
    None
}

fn pcm_seconds(bytes: u64, sample_rate_hz: u32, channels: u16, bytes_per_sample: u16) -> Option<f64> {
    let per_second = u64::from(sample_rate_hz) * u64::from(channels) * u64::from(bytes_per_sample);
    (per_second > 0).then(|| bytes as f64 / per_second as f64)
}

fn wav_duration_seconds(audio: &[u8]) -> Option<f64> {
    let le32 = |at: usize| audio.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let mut at = 12;
    let mut byte_rate = None;
    while at + 8 <= audio.len() {
        let id = &audio[at..at + 4];
        let size = le32(at + 4)? as usize;
        if id == b"fmt " {
            byte_rate = le32(at + 16);
        } else if id == b"data" {
            let rate = byte_rate.filter(|rate| *rate > 0)?;
            // A streamed WAV may carry a placeholder size; trust the bytes.
            let data = size.min(audio.len().saturating_sub(at + 8));
            return Some(data as f64 / f64::from(rate));
        }
        at = at.checked_add(8 + size + (size & 1))?;
    }
    None
}

/// MP3 length from the first frame's bitrate, assuming a constant bitrate
/// (what TTS vendors emit). VBR files are estimated, not measured.
fn mp3_duration_seconds(audio: &[u8]) -> Option<f64> {
    let mut at = 0;
    if audio.starts_with(b"ID3") && audio.len() >= 10 {
        let size = audio[6..10].iter().fold(0usize, |acc, b| (acc << 7) | usize::from(b & 0x7f));
        at = 10 + size;
    }
    while at + 4 <= audio.len() {
        if audio[at] == 0xff && audio[at + 1] & 0xe0 == 0xe0 {
            let version = (audio[at + 1] >> 3) & 0b11; // 3 = MPEG-1, 2 = MPEG-2, 0 = MPEG-2.5
            let layer = (audio[at + 1] >> 1) & 0b11; // 1 = Layer III
            let index = usize::from(audio[at + 2] >> 4);
            if layer == 1 && version != 1 && (1..15).contains(&index) {
                const MPEG1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
                const MPEG2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
                let kbps = if version == 3 { MPEG1[index] } else { MPEG2[index] };
                let bytes = (audio.len() - at) as f64;
                return Some(bytes * 8.0 / (f64::from(kbps) * 1000.0));
            }
        }
        at += 1;
    }
    None
}

fn duration_from_extras(response: &SttResponse) -> Option<f64> {
    response
        .extras
        .as_ref()?
        .get("duration_seconds")?
        .as_f64()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
}

fn started() -> (Instant, i64) {
    (Instant::now(), chrono::Utc::now().timestamp_millis())
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// Text-to-speech
// ---------------------------------------------------------------------------

/// A vendor TTS adapter that records each synthesis. Goes inside the response
/// cache, so a cache hit never reaches it.
pub struct MeteredTtsProvider {
    inner: Arc<dyn TtsProvider>,
    vendor: &'static str,
}

impl MeteredTtsProvider {
    pub fn wrap(inner: Arc<dyn TtsProvider>, vendor: &'static str) -> Arc<dyn TtsProvider> {
        Arc::new(Self { inner, vendor })
    }
}

#[async_trait]
impl TtsProvider for MeteredTtsProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn label(&self) -> Option<&str> {
        self.inner.label()
    }
    fn default_voice(&self) -> Option<&str> {
        self.inner.default_voice()
    }
    fn default_model(&self) -> &str {
        self.inner.default_model()
    }
    fn default_format(&self) -> Option<&str> {
        self.inner.default_format()
    }
    fn supported_voices(&self) -> Vec<String> {
        self.inner.supported_voices()
    }
    fn supported_formats(&self) -> Vec<String> {
        self.inner.supported_formats()
    }
    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        let scope = current_scope();
        let chars = request.text.chars().count() as u64;
        let requested_model = request.model.clone().unwrap_or_else(|| self.inner.default_model().to_string());
        let (start, started_at_ms) = started();
        let result = self.inner.synthesize(request).await;
        let (model, audio_seconds, error) = match &result {
            Ok(response) => (
                response.model.clone(),
                audio_duration_seconds(&response.audio, &response.content_type),
                None,
            ),
            Err(error) => (requested_model, None, Some(error.to_string())),
        };
        record(
            MediaCall {
                kind: magicllm::MediaKind::Tts,
                vendor: self.vendor,
                model: &model,
                usage: magicllm::MediaUsage {
                    input_chars: Some(chars),
                    audio_seconds,
                    requests: 1,
                },
                latency_ms: elapsed_ms(start),
                started_at_ms,
                error,
            },
            scope,
        );
        result
    }

    async fn clear_cache(&self) {
        self.inner.clear_cache().await;
    }
    async fn cache_len(&self) -> usize {
        self.inner.cache_len().await
    }
    fn cache_stats(&self) -> TtsCacheStats {
        self.inner.cache_stats()
    }
}

// ---------------------------------------------------------------------------
// Speech-to-text uploads
// ---------------------------------------------------------------------------

/// A vendor STT adapter that records each transcription.
pub struct MeteredSttProvider {
    inner: Arc<dyn SttProvider>,
    vendor: &'static str,
}

impl MeteredSttProvider {
    pub fn wrap(inner: Arc<dyn SttProvider>, vendor: &'static str) -> Arc<dyn SttProvider> {
        Arc::new(Self { inner, vendor })
    }

    fn record(&self, request_audio_seconds: Option<f64>, requested_model: String, start: (Instant, i64), result: &Result<SttResponse, SttError>, scope: Option<MediaMeterScope>) {
        let (model, audio_seconds, error) = match result {
            Ok(response) => (
                response.model.clone(),
                duration_from_extras(response).or(request_audio_seconds),
                None,
            ),
            // Nothing heard is still billed audio.
            Err(SttError::NoSpeech) => (requested_model, request_audio_seconds, None),
            Err(error) => (requested_model, request_audio_seconds, Some(error.to_string())),
        };
        record(
            MediaCall {
                kind: magicllm::MediaKind::Stt,
                vendor: self.vendor,
                model: &model,
                usage: magicllm::MediaUsage {
                    input_chars: None,
                    audio_seconds,
                    requests: 1,
                },
                latency_ms: elapsed_ms(start.0),
                started_at_ms: start.1,
                error,
            },
            scope,
        );
    }

    fn measure(&self, request: &SttRequest) -> (Option<f64>, String) {
        (
            audio_duration_seconds(&request.audio, &request.content_type),
            request.model.clone().unwrap_or_else(|| self.inner.default_model().to_string()),
        )
    }
}

#[async_trait]
impl SttProvider for MeteredSttProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn label(&self) -> Option<&str> {
        self.inner.label()
    }
    fn default_model(&self) -> &str {
        self.inner.default_model()
    }
    fn review_configuration_hash(&self) -> Option<String> {
        self.inner.review_configuration_hash()
    }

    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, SttError> {
        let scope = current_scope();
        let (seconds, model) = self.measure(&request);
        let start = started();
        let result = self.inner.transcribe(request).await;
        self.record(seconds, model, start, &result, scope);
        result
    }

    async fn transcribe_bounded_review(
        &self,
        request: SttRequest,
        maximum_response_bytes: u64,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<SttResponse, SttError> {
        let scope = current_scope();
        let (seconds, model) = self.measure(&request);
        let start = started();
        let result = self
            .inner
            .transcribe_bounded_review(request, maximum_response_bytes, cancel)
            .await;
        // A provider without a review boundary never called its vendor.
        if !matches!(result, Err(SttError::NotConfigured(_))) {
            self.record(seconds, model, start, &result, scope);
        }
        result
    }

    async fn transcribe_stream(
        &self,
        request: SttRequest,
        events: mpsc::Sender<SttStreamEvent>,
    ) -> Result<SttResponse, SttError> {
        let scope = current_scope();
        let (seconds, model) = self.measure(&request);
        let start = started();
        let result = self.inner.transcribe_stream(request, events).await;
        self.record(seconds, model, start, &result, scope);
        result
    }
}

// ---------------------------------------------------------------------------
// Streaming speech-to-text
// ---------------------------------------------------------------------------

/// A vendor streaming STT adapter whose sessions record the audio pushed into
/// them, one row per session (on `finish`, or when dropped unfinished).
pub struct MeteredStreamingSttProvider {
    inner: Arc<dyn StreamingSttProvider>,
    vendor: &'static str,
}

impl MeteredStreamingSttProvider {
    pub fn wrap(inner: Arc<dyn StreamingSttProvider>, vendor: &'static str) -> Arc<dyn StreamingSttProvider> {
        Arc::new(Self { inner, vendor })
    }
}

#[async_trait]
impl StreamingSttProvider for MeteredStreamingSttProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn label(&self) -> Option<&str> {
        self.inner.label()
    }
    fn default_model(&self) -> &str {
        self.inner.default_model()
    }
    fn capabilities(&self) -> StreamingSttCapabilities {
        self.inner.capabilities()
    }

    async fn open_session(
        &self,
        format: StreamAudioFormat,
        events: mpsc::Sender<StreamingSttEvent>,
    ) -> Result<Box<dyn StreamingSttSession>, SttError> {
        let session = self.inner.open_session(format, events).await?;
        Ok(Box::new(MeteredStreamingSession {
            inner: session,
            vendor: self.vendor,
            model: self.inner.default_model().to_string(),
            format,
            pcm_bytes: AtomicU64::new(0),
            recorded: AtomicBool::new(false),
            scope: current_scope(),
            start: started(),
        }))
    }
}

struct MeteredStreamingSession {
    inner: Box<dyn StreamingSttSession>,
    vendor: &'static str,
    model: String,
    format: StreamAudioFormat,
    pcm_bytes: AtomicU64,
    recorded: AtomicBool,
    scope: Option<MediaMeterScope>,
    start: (Instant, i64),
}

impl MeteredStreamingSession {
    fn record_once(&self, error: Option<String>) {
        if self.recorded.swap(true, Ordering::SeqCst) {
            return;
        }
        let bytes_per_sample = match self.format.sample_format {
            StreamSampleFormat::PcmS16Le => 2,
            StreamSampleFormat::PcmF32Le => 4,
        };
        let seconds = pcm_seconds(
            self.pcm_bytes.load(Ordering::SeqCst),
            self.format.sample_rate_hz,
            self.format.channels.max(1),
            bytes_per_sample,
        );
        record(
            MediaCall {
                kind: magicllm::MediaKind::SttStreaming,
                vendor: self.vendor,
                model: &self.model,
                usage: magicllm::MediaUsage {
                    input_chars: None,
                    audio_seconds: seconds,
                    requests: 1,
                },
                latency_ms: elapsed_ms(self.start.0),
                started_at_ms: self.start.1,
                error,
            },
            self.scope.clone(),
        );
    }
}

#[async_trait]
impl StreamingSttSession for MeteredStreamingSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
        let bytes = chunk.pcm.len() as u64;
        let result = self.inner.push_audio(chunk).await;
        if result.is_ok() {
            self.pcm_bytes.fetch_add(bytes, Ordering::SeqCst);
        }
        result
    }

    async fn finish(&self) -> Result<(), SttError> {
        let result = self.inner.finish().await;
        self.record_once(result.as_ref().err().map(ToString::to_string));
        result
    }
}

impl Drop for MeteredStreamingSession {
    fn drop(&mut self) {
        // A session torn down without `finish` still streamed billed audio.
        if self.pcm_bytes.load(Ordering::SeqCst) > 0 {
            self.record_once(None);
        }
    }
}

// ---------------------------------------------------------------------------
// Binding a chain to its tenant
// ---------------------------------------------------------------------------

/// Bind each TTS provider in `chain` to `scope`, for a session that resolves
/// its chain once and synthesizes from spawned tasks (hands-free, meetings).
pub fn scope_tts_chain(chain: Vec<Arc<dyn TtsProvider>>, scope: &MediaMeterScope) -> Vec<Arc<dyn TtsProvider>> {
    chain
        .into_iter()
        .map(|inner| Arc::new(ScopedTtsProvider { inner, scope: scope.clone() }) as Arc<dyn TtsProvider>)
        .collect()
}

/// Bind a streaming STT provider to `scope`: its sessions bill there.
pub fn scope_streaming_stt(inner: Arc<dyn StreamingSttProvider>, scope: MediaMeterScope) -> Arc<dyn StreamingSttProvider> {
    Arc::new(ScopedStreamingSttProvider { inner, scope })
}

/// Bind each STT provider in `chain` to `scope`.
pub fn scope_stt_chain(chain: Vec<Arc<dyn SttProvider>>, scope: &MediaMeterScope) -> Vec<Arc<dyn SttProvider>> {
    chain
        .into_iter()
        .map(|inner| Arc::new(ScopedSttProvider { inner, scope: scope.clone() }) as Arc<dyn SttProvider>)
        .collect()
}

struct ScopedTtsProvider {
    inner: Arc<dyn TtsProvider>,
    scope: MediaMeterScope,
}

#[async_trait]
impl TtsProvider for ScopedTtsProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn label(&self) -> Option<&str> {
        self.inner.label()
    }
    fn default_voice(&self) -> Option<&str> {
        self.inner.default_voice()
    }
    fn default_model(&self) -> &str {
        self.inner.default_model()
    }
    fn default_format(&self) -> Option<&str> {
        self.inner.default_format()
    }
    fn supported_voices(&self) -> Vec<String> {
        self.inner.supported_voices()
    }
    fn supported_formats(&self) -> Vec<String> {
        self.inner.supported_formats()
    }
    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }
    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        with_media_meter_scope(self.scope.clone(), self.inner.synthesize(request)).await
    }
    async fn clear_cache(&self) {
        self.inner.clear_cache().await;
    }
    async fn cache_len(&self) -> usize {
        self.inner.cache_len().await
    }
    fn cache_stats(&self) -> TtsCacheStats {
        self.inner.cache_stats()
    }
}

struct ScopedSttProvider {
    inner: Arc<dyn SttProvider>,
    scope: MediaMeterScope,
}

#[async_trait]
impl SttProvider for ScopedSttProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn label(&self) -> Option<&str> {
        self.inner.label()
    }
    fn default_model(&self) -> &str {
        self.inner.default_model()
    }
    fn review_configuration_hash(&self) -> Option<String> {
        self.inner.review_configuration_hash()
    }
    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, SttError> {
        with_media_meter_scope(self.scope.clone(), self.inner.transcribe(request)).await
    }
    async fn transcribe_bounded_review(
        &self,
        request: SttRequest,
        maximum_response_bytes: u64,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<SttResponse, SttError> {
        with_media_meter_scope(
            self.scope.clone(),
            self.inner.transcribe_bounded_review(request, maximum_response_bytes, cancel),
        )
        .await
    }
    async fn transcribe_stream(
        &self,
        request: SttRequest,
        events: mpsc::Sender<SttStreamEvent>,
    ) -> Result<SttResponse, SttError> {
        with_media_meter_scope(self.scope.clone(), self.inner.transcribe_stream(request, events)).await
    }
}

struct ScopedStreamingSttProvider {
    inner: Arc<dyn StreamingSttProvider>,
    scope: MediaMeterScope,
}

#[async_trait]
impl StreamingSttProvider for ScopedStreamingSttProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn label(&self) -> Option<&str> {
        self.inner.label()
    }
    fn default_model(&self) -> &str {
        self.inner.default_model()
    }
    fn capabilities(&self) -> StreamingSttCapabilities {
        self.inner.capabilities()
    }
    async fn open_session(
        &self,
        format: StreamAudioFormat,
        events: mpsc::Sender<StreamingSttEvent>,
    ) -> Result<Box<dyn StreamingSttSession>, SttError> {
        // The session's pushes and finish run in scope too: a fallback chain
        // reopens its next provider from inside `push_audio`.
        let inner = with_media_meter_scope(self.scope.clone(), self.inner.open_session(format, events)).await?;
        Ok(Box::new(ScopedStreamingSession {
            inner,
            scope: self.scope.clone(),
        }))
    }
}

struct ScopedStreamingSession {
    inner: Box<dyn StreamingSttSession>,
    scope: MediaMeterScope,
}

#[async_trait]
impl StreamingSttSession for ScopedStreamingSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
        with_media_meter_scope(self.scope.clone(), self.inner.push_audio(chunk)).await
    }
    async fn finish(&self) -> Result<(), SttError> {
        with_media_meter_scope(self.scope.clone(), self.inner.finish()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(seconds: u32, rate: u32) -> Vec<u8> {
        let data = vec![0u8; (rate * 2 * seconds) as usize];
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn wav_length_comes_from_its_header() {
        let seconds = audio_duration_seconds(&wav(3, 24_000), "audio/wav").unwrap();
        assert!((seconds - 3.0).abs() < 1e-9, "{seconds}");
    }

    #[test]
    fn raw_pcm_needs_its_rate() {
        let pcm = vec![0u8; 48_000];
        let seconds = audio_duration_seconds(&pcm, "audio/pcm;rate=24000").unwrap();
        assert!((seconds - 1.0).abs() < 1e-9);
        assert_eq!(audio_duration_seconds(&pcm, "audio/pcm"), None);
    }

    #[test]
    fn cbr_mp3_length_comes_from_the_first_frame_bitrate() {
        // MPEG-1 Layer III, 128 kbps: 0xFF 0xFB 0x90 ...; 16,000 bytes = 1 s.
        let mut mp3 = vec![0u8; 16_000];
        mp3[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
        let seconds = audio_duration_seconds(&mp3, "audio/mpeg").unwrap();
        assert!((seconds - 1.0).abs() < 1e-9, "{seconds}");
    }

    #[test]
    fn compressed_containers_have_no_measured_length() {
        assert_eq!(audio_duration_seconds(b"\x1a\x45\xdf\xa3webm", "audio/webm"), None);
        assert_eq!(audio_duration_seconds(b"OggS....", "audio/ogg"), None);
    }

    struct FakeTts;

    #[async_trait]
    impl TtsProvider for FakeTts {
        fn id(&self) -> &str {
            "fake"
        }
        fn default_voice(&self) -> Option<&str> {
            None
        }
        fn default_model(&self) -> &str {
            "bulbul:v4-flash"
        }
        async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
            Ok(TtsResponse {
                audio: bytes::Bytes::from(wav(2, 24_000)),
                content_type: "audio/wav".into(),
                model: request.model.unwrap_or_else(|| "bulbul:v4-flash".into()),
                voice: None,
                message_id: None,
            })
        }
    }

    /// One metered call inside a tenant scope emits one priced `voice_tts`
    /// event for that tenant; the same call outside a scope emits nothing.
    #[tokio::test]
    async fn a_scoped_tts_call_emits_one_priced_event() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        install_media_meter(Arc::clone(&broadcaster));
        let broadcaster = BROADCASTER.get().expect("meter installed");
        let mut events = broadcaster.subscribe();
        let tts = MeteredTtsProvider::wrap(Arc::new(FakeTts), "sarvam");
        let request = TtsRequest {
            text: "नमस्ते".repeat(100), // 600 characters, not bytes
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
        };
        tts.synthesize(request.clone()).await.expect("unscoped synth");
        with_media_meter_scope(MediaMeterScope::new("owner", "work"), tts.synthesize(request))
            .await
            .expect("scoped synth");
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .expect("an event")
            .expect("received");
        let RuntimeTransportEvent::LLMResponseReceived {
            principal, workspace, provider, model, response_kind, capability, cost, usage_reported, ..
        } = event
        else {
            panic!("expected an LLM response event");
        };
        assert_eq!((principal.as_deref(), workspace.as_deref()), (Some("owner"), Some("work")));
        assert_eq!((provider.as_str(), model.as_str()), ("sarvam", "bulbul:v4-flash"));
        assert_eq!((response_kind.as_str(), capability.as_str()), ("voice_tts", "voice.tts"));
        assert!(usage_reported);
        // The seed's bulbul:v4-flash row bills characters: 600 of them.
        let expected = magicllm::compute_media_cost_at(
            &magicllm::LLMProviderKind::Sarvam,
            "bulbul:v4-flash",
            magicllm::MediaKind::Tts,
            &magicllm::MediaUsage { input_chars: Some(600), audio_seconds: Some(2.0), requests: 1 },
            chrono::Utc::now().timestamp_millis(),
        )
        .expect("seed prices bulbul:v4-flash");
        assert!(expected > 0.0 && (cost - expected).abs() < 1e-12, "{cost} vs {expected}");
        // The unscoped call before it emitted nothing.
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn local_engines_meter_as_local() {
        assert_eq!(media_vendor_for_adapter("fluid_audio_kokoro_tts"), Some("local"));
        assert_eq!(media_vendor_for_adapter("macos_speech_streaming"), Some("local"));
        assert_eq!(media_vendor_for_adapter("some_new_adapter"), None);
        assert_eq!(media_vendor_for_adapter("sarvam_tts"), Some("sarvam"));
        assert_eq!(media_vendor_for_adapter("grok_stt"), Some("xai"));
    }
}
