use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use hound::{SampleFormat, WavReader};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use magicllm::realtime::{
    AudioStreamChannel, RealtimeAudioControl, RealtimeAudioTopology, RealtimeProvider,
    RealtimeProviderError, RealtimeProviderEvent, RealtimeProviderKind, RealtimeSessionDescriptor,
    RealtimeSpeechSegment,
};

use super::{
    compose_captured_surface_audio_pipeline, resolve_surface_audio_pipeline, AudioChunk,
    AudioPipelineServices, AudioStage, AudioSurface, MediaPreferencesStore, MediaProviderRegistry,
    ResolvedAudioProfile, StreamAudioFormat, StreamSampleFormat, StreamingSttEvent,
    StreamingSttProvider, TtsProvider, TtsRequest,
};
use crate::media_rails::AudioRuntimeConfigManager;
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

const PROVIDER_ID: &str = "magician-hands-free";
const WIRE_SAMPLE_RATE_HZ: u32 = 24_000;
const CHANNEL_CAPACITY: usize = 64;
const PCM_CHUNK_MS: usize = 100;
const PCM_INITIAL_LEAD_MS: usize = 300;
const OUTPUT_EVENT_LEAD_MS: u64 = 20;

struct PendingSession {
    pipeline: Arc<dyn StreamingSttProvider>,
    tts_chain: Vec<Arc<dyn TtsProvider>>,
    profile: ResolvedAudioProfile,
}

/// Backend-owned local voice cascade. It deliberately implements the same
/// realtime provider contract as vendor sessions so browser and native hosts
/// keep one control WebSocket and one PCM transport.
pub struct CascadedHandsFreeProvider {
    services: AudioPipelineServices,
    providers: Arc<MediaProviderRegistry>,
    pending: DashMap<String, PendingSession>,
    active: DashMap<String, CancellationToken>,
    captured_profile: Option<ResolvedAudioProfile>,
}

impl CascadedHandsFreeProvider {
    pub fn new(
        runtime: Arc<AudioRuntimeConfigManager>,
        providers: Arc<MediaProviderRegistry>,
        preferences: Arc<MediaPreferencesStore>,
        broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        Self {
            services: AudioPipelineServices::new(
                runtime,
                Arc::clone(&providers),
                preferences,
                broadcaster,
            ),
            providers,
            pending: DashMap::new(),
            active: DashMap::new(),
            captured_profile: None,
        }
    }

    /// Bind immutable request-scoped audio choices captured by media-session
    /// registration. This keeps native client choices local instead of writing
    /// them into the shared Web media preferences.
    pub fn with_captured_audio_profile(mut self, profile: Option<ResolvedAudioProfile>) -> Self {
        self.captured_profile = profile;
        self
    }

    fn resolved_tts_chain(
        &self,
        profile: &ResolvedAudioProfile,
    ) -> Result<Vec<Arc<dyn TtsProvider>>, RealtimeProviderError> {
        let stage = profile
            .stages
            .get(&AudioStage::Tts)
            .filter(|stage| stage.enabled)
            .ok_or_else(|| {
                RealtimeProviderError::NotConfigured(
                    "hands-free profile has no enabled TTS stage".to_string(),
                )
            })?;
        let registered = self.providers.tts_chain();
        let chain = stage
            .providers
            .iter()
            .filter_map(|option| {
                registered
                    .iter()
                    .find(|provider| provider.id().eq_ignore_ascii_case(&option.provider_id))
                    .cloned()
            })
            .collect::<Vec<_>>();
        if chain.is_empty() {
            return Err(RealtimeProviderError::NotConfigured(
                stage
                    .degraded_reason
                    .clone()
                    .unwrap_or_else(|| "hands-free TTS providers are unavailable".to_string()),
            ));
        }
        Ok(chain)
    }
}

#[async_trait]
impl RealtimeProvider for CascadedHandsFreeProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn kind(&self) -> RealtimeProviderKind {
        RealtimeProviderKind::HandsFree
    }

    fn default_model(&self) -> &str {
        "configured-hands-free-cascade"
    }

    fn audio_topology(&self) -> RealtimeAudioTopology {
        RealtimeAudioTopology::BackendProxied
    }

    async fn create_session(
        &self,
        principal: &str,
        workspace: &str,
        voice_session_id: &str,
        _thread_id: Option<&str>,
        _preferred_voice: Option<&str>,
    ) -> Result<RealtimeSessionDescriptor, RealtimeProviderError> {
        let (pipeline, profile) = if let Some(profile) = self.captured_profile.clone() {
            compose_captured_surface_audio_pipeline(
                &self.services,
                AudioSurface::HandsFree,
                principal.to_string(),
                workspace.to_string(),
                profile,
                true,
            )
        } else {
            resolve_surface_audio_pipeline(
                &self.services,
                AudioSurface::HandsFree,
                Some((principal.to_string(), workspace.to_string())),
                true,
            )
            .await
        }
        .map_err(RealtimeProviderError::NotConfigured)?;
        let tts_chain = self.resolved_tts_chain(&profile)?;
        let upstream_id = format!("hands-free-{voice_session_id}-{}", uuid::Uuid::new_v4());
        let voice = tts_chain
            .first()
            .and_then(|provider| provider.default_voice())
            .map(str::to_string);
        self.pending.insert(
            upstream_id.clone(),
            PendingSession {
                pipeline,
                tts_chain,
                profile: profile.clone(),
            },
        );
        Ok(RealtimeSessionDescriptor {
            provider: RealtimeProviderKind::HandsFree,
            model: profile.profile_id,
            topology: RealtimeAudioTopology::BackendProxied,
            mode: magicllm::config::RealtimeVoiceMode::Assistant,
            voice,
            webrtc_url: None,
            upstream_token: None,
            upstream_provider_session_id: Some(upstream_id),
            max_session_duration_secs: None,
            native_resume_handle: None,
            transcription_model: profile
                .stages
                .get(&AudioStage::StreamingStt)
                .and_then(|stage| stage.selected.as_ref())
                .map(|provider| provider.model_id.clone()),
            transcription_fallback_model: None,
            turn_detection_mode: Some("server_vad".to_string()),
            context_window_tokens: None,
            half_duplex: None,
        })
    }

    async fn open_proxied_audio(
        &self,
        descriptor: &RealtimeSessionDescriptor,
    ) -> Result<AudioStreamChannel, RealtimeProviderError> {
        let upstream_id = descriptor
            .upstream_provider_session_id
            .as_deref()
            .ok_or_else(|| {
                RealtimeProviderError::BadRequest(
                    "hands-free descriptor has no upstream session id".to_string(),
                )
            })?;
        let (_, pending) = self.pending.remove(upstream_id).ok_or_else(|| {
            RealtimeProviderError::BadRequest(format!(
                "hands-free session `{upstream_id}` was already opened or expired"
            ))
        })?;
        let format = StreamAudioFormat {
            sample_rate_hz: WIRE_SAMPLE_RATE_HZ,
            channels: 1,
            sample_format: StreamSampleFormat::PcmS16Le,
        };
        let (stt_events_tx, stt_events_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let stt = pending
            .pipeline
            .open_session(format, stt_events_tx.clone())
            .await
            .map_err(|error| RealtimeProviderError::Upstream(error.to_string()))?;
        let (upstream_tx, upstream_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (downstream_tx, downstream_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (control_tx, control_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (events_tx, events_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let cancel = CancellationToken::new();
        self.active.insert(upstream_id.to_string(), cancel.clone());
        tokio::spawn(run_session(SessionRuntime {
            upstream_id: upstream_id.to_string(),
            format,
            pipeline: pending.pipeline,
            profile_id: pending.profile.profile_id,
            tts_chain: pending.tts_chain,
            stt,
            stt_events_tx,
            stt_events_rx,
            upstream_rx,
            downstream_tx,
            control_rx,
            events_tx,
            cancel,
        }));
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
        if let Some(id) = descriptor.upstream_provider_session_id.as_deref() {
            self.pending.remove(id);
            if let Some((_, token)) = self.active.remove(id) {
                token.cancel();
            }
        }
        Ok(())
    }
}

struct SessionRuntime {
    upstream_id: String,
    format: StreamAudioFormat,
    pipeline: Arc<dyn StreamingSttProvider>,
    profile_id: String,
    tts_chain: Vec<Arc<dyn TtsProvider>>,
    stt: Box<dyn super::StreamingSttSession>,
    stt_events_tx: mpsc::Sender<StreamingSttEvent>,
    stt_events_rx: mpsc::Receiver<StreamingSttEvent>,
    upstream_rx: mpsc::Receiver<Vec<u8>>,
    downstream_tx: mpsc::Sender<Vec<u8>>,
    control_rx: mpsc::Receiver<RealtimeAudioControl>,
    events_tx: mpsc::Sender<RealtimeProviderEvent>,
    cancel: CancellationToken,
}

async fn run_session(mut runtime: SessionRuntime) {
    let mut seq = 0_u64;
    let mut speaking = false;
    let mut synthesis: Option<CancellationToken> = None;
    debug!(
        upstream_session_id = %runtime.upstream_id,
        profile_id = %runtime.profile_id,
        "hands-free cascade opened"
    );
    let _ = runtime
        .events_tx
        .send(RealtimeProviderEvent::TransportReady)
        .await;

    loop {
        tokio::select! {
            _ = runtime.cancel.cancelled() => break,
            audio = runtime.upstream_rx.recv() => match audio {
                Some(audio) => {
                    seq = seq.saturating_add(1);
                    if let Err(error) = runtime.stt.push_audio(AudioChunk {
                        seq,
                        pcm: Bytes::from(audio),
                    }).await {
                        let _ = runtime.events_tx.send(RealtimeProviderEvent::Error {
                            message: format!("hands-free STT input failed: {error}"),
                            recoverable: true,
                        }).await;
                    }
                },
                None => break,
            },
            event = runtime.stt_events_rx.recv() => match event {
                Some(StreamingSttEvent::Partial { text, .. }) if !text.trim().is_empty() => {
                    if !speaking {
                        speaking = true;
                        cancel_synthesis(&mut synthesis);
                        let _ = runtime.events_tx.send(RealtimeProviderEvent::SpeechStarted).await;
                    }
                },
                Some(StreamingSttEvent::Final { text, .. }) if !text.trim().is_empty() => {
                    if !speaking {
                        cancel_synthesis(&mut synthesis);
                        let _ = runtime.events_tx.send(RealtimeProviderEvent::SpeechStarted).await;
                    }
                    speaking = false;
                    let item_id = format!("hands-free-turn-{}", uuid::Uuid::new_v4());
                    let _ = runtime.events_tx.send(RealtimeProviderEvent::SpeechStopped).await;
                    let _ = runtime.events_tx.send(RealtimeProviderEvent::UserTranscriptFinal {
                        text: text.trim().to_string(),
                        item_id,
                    }).await;
                },
                Some(StreamingSttEvent::Error { reason }) => {
                    let _ = runtime.events_tx.send(RealtimeProviderEvent::Error {
                        message: format!("hands-free STT failed: {reason}"),
                        recoverable: true,
                    }).await;
                },
                Some(_) => {},
                None => break,
            },
            control = runtime.control_rx.recv() => match control {
                Some(RealtimeAudioControl::SynthesizeResponse { response_id, text, segments }) => {
                    cancel_synthesis(&mut synthesis);
                    let token = runtime.cancel.child_token();
                    synthesis = Some(token.clone());
                    tokio::spawn(synthesize_response(
                        response_id,
                        text,
                        segments,
                        runtime.tts_chain.clone(),
                        runtime.downstream_tx.clone(),
                        runtime.events_tx.clone(),
                        token,
                    ));
                },
                Some(RealtimeAudioControl::InjectSystemMessage { text, request_response: true }) => {
                    cancel_synthesis(&mut synthesis);
                    let token = runtime.cancel.child_token();
                    synthesis = Some(token.clone());
                    tokio::spawn(synthesize_response(
                        format!("hands-free-system-{}", uuid::Uuid::new_v4()),
                        text.clone(),
                        vec![RealtimeSpeechSegment::plain(text)],
                        runtime.tts_chain.clone(),
                        runtime.downstream_tx.clone(),
                        runtime.events_tx.clone(),
                        token,
                    ));
                },
                Some(RealtimeAudioControl::InterruptResponse) => {
                    cancel_synthesis(&mut synthesis);
                },
                Some(RealtimeAudioControl::CommitInputAndRespond) => {
                    if let Err(error) = runtime.stt.finish().await {
                        warn!(error = %error, "hands-free STT commit failed");
                    }
                    match runtime.pipeline.open_session(runtime.format, runtime.stt_events_tx.clone()).await {
                        Ok(stt) => runtime.stt = stt,
                        Err(error) => {
                            let _ = runtime.events_tx.send(RealtimeProviderEvent::Error {
                                message: format!("hands-free STT restart failed: {error}"),
                                recoverable: false,
                            }).await;
                            break;
                        },
                    }
                },
                Some(RealtimeAudioControl::End) | None => break,
                Some(_) => {},
            },
        }
    }
    cancel_synthesis(&mut synthesis);
    let _ = runtime.stt.finish().await;
    debug!(upstream_session_id = %runtime.upstream_id, "hands-free cascade closed");
}

fn cancel_synthesis(active: &mut Option<CancellationToken>) {
    if let Some(token) = active.take() {
        token.cancel();
    }
}

async fn synthesize_response(
    response_id: String,
    text: String,
    segments: Vec<RealtimeSpeechSegment>,
    providers: Vec<Arc<dyn TtsProvider>>,
    downstream: mpsc::Sender<Vec<u8>>,
    events: mpsc::Sender<RealtimeProviderEvent>,
    cancel: CancellationToken,
) {
    let clean_segments = segments
        .into_iter()
        .filter_map(|mut segment| {
            segment.text = segment.text.trim().to_string();
            (!segment.text.is_empty()).then_some(segment)
        })
        .collect::<Vec<_>>();
    let clean_segments = if clean_segments.is_empty() && !text.trim().is_empty() {
        vec![RealtimeSpeechSegment::plain(text.trim())]
    } else {
        clean_segments
    };
    let _ = events
        .send(RealtimeProviderEvent::AssistantTranscriptFinal {
            response_id: response_id.clone(),
            text,
        })
        .await;

    let mut segments = clean_segments.into_iter();
    let mut audio = match segments.next() {
        Some(segment) => match synthesize_wav(&providers, &segment, &response_id, &cancel).await {
            Ok(audio) => Some(audio),
            Err(error) => {
                report_synthesis_failure(&events, &response_id, error, &cancel).await;
                return;
            },
        },
        None => None,
    };
    if audio.is_some() {
        let _ = events
            .send(RealtimeProviderEvent::AssistantAudioStarted {
                response_id: response_id.clone(),
            })
            .await;
        // Output-start must reach clients before the first binary frame so
        // half-duplex can gate capture without feeding the opening audio
        // chunk back into VAD.
        tokio::select! {
            _ = cancel.cancelled() => audio = None,
            _ = tokio::time::sleep(Duration::from_millis(OUTPUT_EVENT_LEAD_MS)) => {},
        }
    }

    while let Some(current_audio) = audio.take() {
        let Some(next_segment) = segments.next() else {
            let _ = stream_pcm(current_audio, &downstream, &cancel).await;
            break;
        };

        // Keep the first-audio latency of sequential synthesis, then prepare
        // one segment ahead while the current PCM is streaming. A semantic or
        // emotion boundary should not make the device run dry while another
        // network TTS request starts.
        let (stream_result, next_audio) = tokio::join!(
            stream_pcm(current_audio, &downstream, &cancel),
            synthesize_wav(&providers, &next_segment, &response_id, &cancel),
        );
        if stream_result.is_err() {
            break;
        }
        match next_audio {
            Ok(next_audio) => audio = Some(next_audio),
            Err(error) => {
                report_synthesis_failure(&events, &response_id, error, &cancel).await;
                return;
            },
        }
    }
    let interrupted = cancel.is_cancelled();
    let _ = events
        .send(RealtimeProviderEvent::AssistantAudioDone {
            response_id: response_id.clone(),
            interrupted,
        })
        .await;
    if !interrupted {
        let _ = events
            .send(RealtimeProviderEvent::ResponseDone {
                response_id: Some(response_id),
                input_tokens: None,
                output_tokens: None,
                usage: None,
            })
            .await;
    }
}

async fn report_synthesis_failure(
    events: &mpsc::Sender<RealtimeProviderEvent>,
    response_id: &str,
    error: String,
    cancel: &CancellationToken,
) {
    let interrupted = cancel.is_cancelled();
    if !interrupted {
        let _ = events
            .send(RealtimeProviderEvent::Error {
                message: error,
                recoverable: true,
            })
            .await;
    }
    let _ = events
        .send(RealtimeProviderEvent::AssistantAudioDone {
            response_id: response_id.to_string(),
            interrupted,
        })
        .await;
}

async fn synthesize_wav(
    providers: &[Arc<dyn TtsProvider>],
    segment: &RealtimeSpeechSegment,
    response_id: &str,
    cancel: &CancellationToken,
) -> Result<Vec<i16>, String> {
    let mut failures = Vec::new();
    for provider in providers {
        let request = TtsRequest {
            text: segment.text.clone(),
            voice: None,
            rate: None,
            model: None,
            format: Some("wav".to_string()),
            message_id: Some(response_id.to_string()),
            emotion: parse_speech_hint(segment.emotion.as_deref()),
            style: parse_speech_hint(segment.style.as_deref()),
            pace: parse_speech_hint(segment.pace.as_deref()),
            voice_mode: parse_speech_hint(segment.voice_mode.as_deref()),
            emphasis: segment.emphasis.clone(),
        };
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err("hands-free response interrupted".to_string()),
            response = provider.synthesize(request) => response,
        };
        match response {
            Ok(response) if response.content_type.to_ascii_lowercase().contains("wav") => {
                match decode_wav_to_pcm24k(&response.audio) {
                    Ok(audio) => return Ok(audio),
                    Err(error) => failures.push(format!("{} decode: {error}", provider.id())),
                }
            },
            Ok(response) => failures.push(format!(
                "{} returned unsupported {}",
                provider.id(),
                response.content_type
            )),
            Err(error) => failures.push(format!("{}: {error}", provider.id())),
        }
    }
    Err(format!(
        "all configured hands-free TTS providers failed: {}",
        failures.join("; ")
    ))
}

fn parse_speech_hint<T: DeserializeOwned>(value: Option<&str>) -> Option<T> {
    value
        .and_then(|value| serde_json::from_value(serde_json::Value::String(value.to_string())).ok())
}

fn decode_wav_to_pcm24k(audio: &[u8]) -> Result<Vec<i16>, String> {
    let mut reader = WavReader::new(Cursor::new(audio))
        .map_err(|error| format!("reading WAV response: {error}"))?;
    let spec = reader.spec();
    let samples = match spec.sample_format {
        SampleFormat::Int if spec.bits_per_sample <= 16 => reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decoding PCM16 WAV: {error}"))?,
        SampleFormat::Int => {
            let shift = spec.bits_per_sample.saturating_sub(16) as u32;
            reader
                .samples::<i32>()
                .map(|sample| {
                    sample.map(|value| {
                        (value >> shift).clamp(i16::MIN as i32, i16::MAX as i32) as i16
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("decoding integer WAV: {error}"))?
        },
        SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| {
                sample.map(|value| (value.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decoding float WAV: {error}"))?,
    };
    if samples.is_empty() {
        return Err("WAV response contains no audio samples".to_string());
    }
    let channels = usize::from(spec.channels.max(1));
    let mono = if channels == 1 {
        samples
    } else {
        samples
            .chunks(channels)
            .map(|frame| {
                let sum = frame.iter().map(|sample| i64::from(*sample)).sum::<i64>();
                (sum / frame.len() as i64) as i16
            })
            .collect()
    };
    Ok(resample_pcm16(
        &mono,
        spec.sample_rate.max(1),
        WIRE_SAMPLE_RATE_HZ,
    ))
}

fn resample_pcm16(input: &[i16], source_rate: u32, target_rate: u32) -> Vec<i16> {
    if input.is_empty() || source_rate == target_rate {
        return input.to_vec();
    }
    let output_len =
        ((input.len() as u64 * u64::from(target_rate)) / u64::from(source_rate)).max(1) as usize;
    (0..output_len)
        .map(|index| {
            let source = index as f64 * source_rate as f64 / target_rate as f64;
            let lower = source.floor() as usize;
            let upper = (lower + 1).min(input.len() - 1);
            let fraction = source - lower as f64;
            (input[lower] as f64 * (1.0 - fraction) + input[upper] as f64 * fraction).round() as i16
        })
        .collect()
}

async fn stream_pcm(
    samples: Vec<i16>,
    downstream: &mpsc::Sender<Vec<u8>>,
    cancel: &CancellationToken,
) -> Result<(), ()> {
    let samples_per_chunk = WIRE_SAMPLE_RATE_HZ as usize * PCM_CHUNK_MS / 1_000;
    let started_at = tokio::time::Instant::now();
    for (index, chunk) in samples.chunks(samples_per_chunk.max(1)).enumerate() {
        if cancel.is_cancelled() {
            return Err(());
        }
        let deadline_offset_ms = pcm_chunk_send_deadline_ms(index);
        if deadline_offset_ms > 0 {
            // Maintain a bounded lead over device playback instead of sleeping
            // exactly one chunk after every send. Waiting before the next send
            // keeps the lead from briefly overshooting on every packet.
            let deadline = started_at + Duration::from_millis(deadline_offset_ms);
            tokio::select! {
                _ = cancel.cancelled() => return Err(()),
                _ = tokio::time::sleep_until(deadline) => {},
            }
        }
        let mut bytes = Vec::with_capacity(chunk.len() * 2);
        for sample in chunk {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(()),
            result = downstream.send(bytes) => result.map_err(|_| ())?,
        }
    }
    Ok(())
}

fn pcm_chunk_send_deadline_ms(index: usize) -> u64 {
    (index + 1)
        .saturating_mul(PCM_CHUNK_MS)
        .saturating_sub(PCM_INITIAL_LEAD_MS) as u64
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use bytes::Bytes;
    use magicllm::realtime::{RealtimeProviderEvent, RealtimeSpeechSegment};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::{
        parse_speech_hint, pcm_chunk_send_deadline_ms, resample_pcm16, synthesize_response,
    };
    use crate::media_rails::{
        TtsEmotion, TtsError, TtsProvider, TtsRequest, TtsResponse, TtsVoiceMode,
    };

    struct LookaheadTts {
        calls: AtomicUsize,
        first_stream_finished: Arc<AtomicBool>,
        second_started_before_first_finished: Arc<AtomicBool>,
    }

    #[async_trait]
    impl TtsProvider for LookaheadTts {
        fn id(&self) -> &str {
            "lookahead-test"
        }

        fn default_voice(&self) -> Option<&str> {
            None
        }

        fn default_model(&self) -> &str {
            "lookahead-test"
        }

        async fn synthesize(&self, _request: TtsRequest) -> Result<TtsResponse, TtsError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 1 && !self.first_stream_finished.load(Ordering::SeqCst) {
                self.second_started_before_first_finished
                    .store(true, Ordering::SeqCst);
            }
            let sample_count = if call == 0 { 9_600 } else { 2_400 };
            Ok(TtsResponse {
                audio: mono_pcm16_wav(sample_count),
                content_type: "audio/wav".to_string(),
                model: "lookahead-test".to_string(),
                voice: None,
                message_id: None,
            })
        }
    }

    fn mono_pcm16_wav(sample_count: usize) -> Bytes {
        let data_len = sample_count.saturating_mul(2) as u32;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36_u32.saturating_add(data_len)).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&24_000_u32.to_le_bytes());
        wav.extend_from_slice(&48_000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for _ in 0..sample_count {
            wav.extend_from_slice(&100_i16.to_le_bytes());
        }
        Bytes::from(wav)
    }

    #[test]
    fn pcm_resampler_preserves_duration() {
        let source = vec![100_i16; 16_000];
        let output = resample_pcm16(&source, 16_000, 24_000);
        assert_eq!(output.len(), 24_000);
        assert!(output.iter().all(|sample| *sample == 100));
    }

    #[test]
    fn pcm_stream_preloads_three_chunks_then_keeps_a_bounded_lead() {
        assert_eq!(pcm_chunk_send_deadline_ms(0), 0);
        assert_eq!(pcm_chunk_send_deadline_ms(1), 0);
        assert_eq!(pcm_chunk_send_deadline_ms(2), 0);
        assert_eq!(pcm_chunk_send_deadline_ms(3), 100);
        assert_eq!(pcm_chunk_send_deadline_ms(4), 200);
    }

    #[test]
    fn speech_hint_wire_names_map_back_to_tts_types() {
        assert_eq!(
            parse_speech_hint::<TtsEmotion>(Some("urgent")),
            Some(TtsEmotion::Urgent)
        );
        assert_eq!(
            parse_speech_hint::<TtsVoiceMode>(Some("announcement")),
            Some(TtsVoiceMode::Announcement)
        );
        assert_eq!(parse_speech_hint::<TtsEmotion>(Some("unknown")), None);
    }

    #[tokio::test]
    async fn next_semantic_segment_synthesizes_while_current_audio_streams() {
        let first_stream_finished = Arc::new(AtomicBool::new(false));
        let second_started_before_first_finished = Arc::new(AtomicBool::new(false));
        let provider = Arc::new(LookaheadTts {
            calls: AtomicUsize::new(0),
            first_stream_finished: Arc::clone(&first_stream_finished),
            second_started_before_first_finished: Arc::clone(&second_started_before_first_finished),
        });
        let providers: Vec<Arc<dyn TtsProvider>> = vec![provider.clone()];
        let (downstream_tx, mut downstream_rx) = mpsc::channel(1);
        let (events_tx, mut events_rx) = mpsc::channel(16);
        let drain_finished = Arc::clone(&first_stream_finished);
        let drain = tokio::spawn(async move {
            let mut frames = 0_usize;
            while downstream_rx.recv().await.is_some() {
                frames += 1;
                if frames == 4 {
                    drain_finished.store(true, Ordering::SeqCst);
                }
            }
            frames
        });

        synthesize_response(
            "response-1".to_string(),
            "First. Second.".to_string(),
            vec![
                RealtimeSpeechSegment::plain("First."),
                RealtimeSpeechSegment::plain("Second."),
            ],
            providers,
            downstream_tx,
            events_tx,
            CancellationToken::new(),
        )
        .await;

        assert_eq!(drain.await.expect("downstream drain"), 5);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert!(second_started_before_first_finished.load(Ordering::SeqCst));
        let mut saw_started = false;
        let mut saw_done = false;
        while let Ok(event) = events_rx.try_recv() {
            saw_started |= matches!(&event, RealtimeProviderEvent::AssistantAudioStarted { .. });
            saw_done |= matches!(&event, RealtimeProviderEvent::AssistantAudioDone { .. });
        }
        assert!(saw_started);
        assert!(saw_done);
    }
}
