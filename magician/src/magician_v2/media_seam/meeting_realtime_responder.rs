//! Meeting responder backed by the OpenAI Realtime audio rail.
//!
//! Holds a persistent backend-proxied realtime session with `turn_detection:
//! none`, so it never speaks on its own. When a wake phrase fires, `respond()`
//! injects the meeting context + the participant's utterance and requests a
//! response; the realtime model streams a spoken reply (LLM + TTS in one
//! low-latency pass) which we collect off the audio channel as 24 kHz mono
//! PCM16 for the sink.
//!
//! This replaces the slow Gemma-answer + separate-TTS path — local Gemma is now
//! summary-only. Our streaming STT still feeds the transcript / rolling summary /
//! wake gate; the realtime rail only produces the spoken reply.
//!
//! Session rotation: OpenAI closes a realtime session after ~28 min, and the
//! context window fills as turns accumulate. Before responding we rotate (close
//! + re-open + re-configure) when the session is near its age cap or its context
//! watermark — meetings longer than one upstream session keep working. Context
//! is re-injected on every turn, so a fresh session loses nothing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex;

use magicllm::pricing::compute_realtime_cost;
use magicllm::realtime::{
    AudioStreamChannel, OpenAiRealtimeProvider, RealtimeAudioControl, RealtimeProvider,
    RealtimeProviderEvent, RealtimeSessionDescriptor, OPENAI_REALTIME_DEFAULT_MODEL,
};
use magicllm::types::RealtimeUsage;

use crate::magician_v2::media_seam::responder::{MeetingResponder, ResponderError, SpokenReply};
use crate::magician_v2::media_seam::StreamAudioFormat;
use crate::magician_v2::prompts::{
    names as prompt_names, rendered_prompt_or, versions as prompt_versions,
};
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

/// The realtime rail's PCM is 24 kHz mono, little-endian 16-bit.
const REALTIME_PCM_SAMPLE_RATE: u32 = 24_000;
/// Max wall-clock to wait for the first audio frame of a reply.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(20);
/// Idle gap (after audio starts) that marks the reply finished, if no explicit
/// `ResponseDone` arrives first.
const INTER_FRAME_IDLE: Duration = Duration::from_millis(800);
/// Hard safety cap on collecting one reply.
const REPLY_DEADLINE: Duration = Duration::from_secs(45);
/// Rotate this long before the session's hard age cap.
const ROTATE_MARGIN_SECS: u64 = 60;
/// Fallback age cap when the descriptor doesn't carry one.
const DEFAULT_MAX_SESSION_SECS: u64 = 28 * 60;
/// Rotate when input tokens cross this fraction of the context window.
const CONTEXT_WATERMARK: f64 = 0.7;

/// Compiled fallback for `voice_meeting_presto_system`. Keep in sync with
/// `data/magician_v2/prompts/voice_meeting_presto_system_v1.0.0.json`.
const MEETING_PRESTO_DEFAULT_INSTRUCTIONS: &str = "You are Presto, an AI assistant participating \
in a live meeting by voice. You only speak when a participant addresses you. Keep \
replies brief and conversational — 1-2 short spoken sentences, no lists, no \
markdown. Use the meeting context you are given to answer accurately.";

fn reported_realtime_totals(
    usage: Option<RealtimeUsage>,
    coarse_input_tokens: Option<u64>,
    coarse_output_tokens: Option<u64>,
) -> (bool, u64, u64) {
    let usage_reported =
        usage.is_some() || coarse_input_tokens.is_some() || coarse_output_tokens.is_some();
    let input_tokens = usage.map_or_else(
        || coarse_input_tokens.unwrap_or_default(),
        |value| {
            value
                .audio_input_tokens
                .saturating_add(value.text_input_tokens)
                .saturating_add(value.audio_cached_input_tokens)
                .saturating_add(value.text_cached_input_tokens)
        },
    );
    let output_tokens = usage.map_or_else(
        || coarse_output_tokens.unwrap_or_default(),
        |value| {
            value
                .audio_output_tokens
                .saturating_add(value.text_output_tokens)
        },
    );
    (usage_reported, input_tokens, output_tokens)
}

/// Responder driven by a persistent (auto-rotating) OpenAI Realtime session.
pub struct RealtimeResponder {
    provider: OpenAiRealtimeProvider,
    session: Mutex<ActiveSession>,
    /// Runtime event broadcaster + scope so per-turn realtime cost/latency lands
    /// in the `llm_calls` analytics ledger (like chat + browser voice). `None` when
    /// the meeting was started without a scoped broadcaster (cost stays trace-only).
    broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    scope: Option<(String, String)>,
}

/// One live upstream realtime session + the bookkeeping that decides rotation.
struct ActiveSession {
    channel: AudioStreamChannel,
    descriptor: RealtimeSessionDescriptor,
    created_at: Instant,
    rotate_after: Duration,
    context_window: Option<u64>,
    last_input_tokens: u64,
    /// Running audio+text token totals for this upstream session, for cost.
    session_usage: RealtimeUsage,
}

impl RealtimeResponder {
    /// Open + configure a backend-proxied realtime session (push-to-talk:
    /// `turn_detection: none`). `model` falls back to the rail default; `voice`
    /// is only pinned when the caller provides one, otherwise the upstream
    /// provider/model default is used.
    pub async fn connect(
        api_key: impl Into<String>,
        model: Option<String>,
        voice: Option<String>,
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        scope: Option<(String, String)>,
    ) -> Result<Self, ResponderError> {
        let model = model.unwrap_or_else(|| OPENAI_REALTIME_DEFAULT_MODEL.to_string());

        let provider = OpenAiRealtimeProvider::new(api_key)
            .with_defaults(model, voice)
            .backend_proxied()
            .with_profile_overrides(None, Some("none".to_string()), None, None);

        let session = Self::open_session(&provider).await?;
        Ok(Self {
            provider,
            session: Mutex::new(session),
            broadcaster,
            scope,
        })
    }

    /// Mint + configure a fresh upstream session.
    async fn open_session(
        provider: &OpenAiRealtimeProvider,
    ) -> Result<ActiveSession, ResponderError> {
        let descriptor = provider
            .create_session("meet-bot", "default", "meet-bot-session", None, None)
            .await
            .map_err(|e| ResponderError::Failed(format!("realtime create_session: {e}")))?;

        let channel = provider
            .open_proxied_audio(&descriptor)
            .await
            .map_err(|e| ResponderError::Failed(format!("realtime open_proxied_audio: {e}")))?;

        let instructions = rendered_prompt_or(
            prompt_names::VOICE_MEETING_PRESTO_SYSTEM,
            prompt_versions::VOICE_MEETING_PRESTO_SYSTEM,
            std::collections::HashMap::new(),
            MEETING_PRESTO_DEFAULT_INSTRUCTIONS,
        )
        .await;
        channel
            .control_tx
            .send(RealtimeAudioControl::ConfigureSession {
                instructions,
                tools: vec![],
                input_transcription_model: None,
                update_id: None,
                defer_response_until_context: false,
            })
            .await
            .map_err(|e| ResponderError::Failed(format!("realtime configure: {e}")))?;

        let max_secs = descriptor
            .max_session_duration_secs
            .unwrap_or(DEFAULT_MAX_SESSION_SECS);
        let rotate_after = Duration::from_secs(max_secs.saturating_sub(ROTATE_MARGIN_SECS).max(60));

        Ok(ActiveSession {
            context_window: descriptor.context_window_tokens,
            descriptor,
            channel,
            created_at: Instant::now(),
            rotate_after,
            last_input_tokens: 0,
            session_usage: RealtimeUsage::default(),
        })
    }

    /// True when the active session is near its age cap or context watermark.
    fn should_rotate(session: &ActiveSession) -> bool {
        if session.created_at.elapsed() >= session.rotate_after {
            return true;
        }
        match session.context_window {
            Some(window) if window > 0 => {
                (session.last_input_tokens as f64) >= (window as f64) * CONTEXT_WATERMARK
            },
            _ => false,
        }
    }
}

#[async_trait]
impl MeetingResponder for RealtimeResponder {
    async fn respond(&self, utterance: &str, context: &str) -> Result<SpokenReply, ResponderError> {
        let mut guard = self.session.lock().await;

        // Mint logical-call identity before any per-turn provider injection.
        // The persistent realtime session is transport reuse, not logical-call
        // reuse: every addressed meeting turn must remain independently
        // attributable in the canonical ledger.
        let llm_correlation = self.scope.as_ref().map(|(principal, workspace)| {
            crate::magician_v2::realtime_events::LlmEventCorrelation::direct(
                principal.clone(),
                workspace.clone(),
                magicllm::LlmWorkloadClass::ForegroundChat,
            )
        });
        let llm_started_at_ms = chrono::Utc::now().timestamp_millis();

        // Rotate the upstream session before it ages out / fills its window.
        if Self::should_rotate(&guard) {
            tracing::info!(target: "meet_bot", "rotating realtime session (age/context)");
            let _ = self.provider.close_session(&guard.descriptor).await;
            match Self::open_session(&self.provider).await {
                Ok(fresh) => *guard = fresh,
                Err(e) => {
                    // Keep the old session and try anyway — a stale session is
                    // better than dropping the reply.
                    tracing::warn!(target: "meet_bot", error = %e, "realtime rotation failed");
                },
            }
        }

        // Discard any stale frames/events left from a prior turn.
        {
            let ch = &mut guard.channel;
            while ch.downstream_rx.try_recv().is_ok() {}
            while ch.events_rx.try_recv().is_ok() {}
        }

        let prompt = format!(
            "Meeting context so far:\n{context}\n\nA participant just addressed you and said: \
             \"{utterance}\"\n\nReply to them now, briefly, aloud."
        );

        // Inject the request. If the upstream session has closed (it idles out
        // after a stretch of no traffic), reconnect once and retry — otherwise a
        // dead channel kills every reply.
        if guard
            .channel
            .control_tx
            .send(RealtimeAudioControl::InjectSystemMessage {
                text: prompt.clone(),
                request_response: true,
            })
            .await
            .is_err()
        {
            tracing::info!(target: "meet_bot", "realtime channel closed — reconnecting");
            let _ = self.provider.close_session(&guard.descriptor).await;
            *guard = Self::open_session(&self.provider)
                .await
                .map_err(|e| ResponderError::Failed(format!("realtime reconnect: {e}")))?;
            guard
                .channel
                .control_tx
                .send(RealtimeAudioControl::InjectSystemMessage {
                    text: prompt,
                    request_response: true,
                })
                .await
                .map_err(|e| {
                    ResponderError::Failed(format!("realtime inject (post-reconnect): {e}"))
                })?;
        }

        let ch: &mut AudioStreamChannel = &mut guard.channel;
        let mut pcm: Vec<u8> = Vec::new();
        let mut reply_text = String::new();
        let mut input_tokens: Option<u64> = None;
        let mut output_tokens: Option<u64> = None;
        let mut response_usage: Option<RealtimeUsage> = None;
        let mut response_error: Option<&'static str> = None;
        let mut got_first = false;
        // Latency: wall-clock from the moment we start waiting for the reply to
        // the first audio frame (time-to-first-audio) and to response completion.
        let turn_start = std::time::Instant::now();
        let mut ttfa_ms: Option<u64> = None;
        let deadline = tokio::time::Instant::now() + REPLY_DEADLINE;

        loop {
            let idle = if got_first {
                INTER_FRAME_IDLE
            } else {
                FIRST_FRAME_TIMEOUT
            };
            tokio::select! {
                biased;
                maybe = ch.downstream_rx.recv() => match maybe {
                    Some(frame) => {
                        if !got_first {
                            ttfa_ms = Some(turn_start.elapsed().as_millis() as u64);
                        }
                        got_first = true;
                        pcm.extend_from_slice(&frame);
                    }
                    None => {
                        if pcm.is_empty() {
                            response_error = Some("realtime_audio_stream_closed");
                        }
                        break;
                    },
                },
                maybe = ch.events_rx.recv() => match maybe {
                    Some(RealtimeProviderEvent::AssistantTranscriptFinal { text, .. }) => {
                        reply_text = text;
                    }
                    Some(RealtimeProviderEvent::ResponseDone {
                        response_id: _,
                        input_tokens: it,
                        output_tokens: ot,
                        usage,
                    }) => {
                        input_tokens = it;
                        output_tokens = ot;
                        response_usage = usage;
                        while let Ok(Some(frame)) =
                            tokio::time::timeout(Duration::from_millis(300), ch.downstream_rx.recv()).await
                        {
                            pcm.extend_from_slice(&frame);
                        }
                        break;
                    }
                    Some(RealtimeProviderEvent::ResponseFailed {
                        response_id: _,
                        terminal_state,
                    }) => {
                        response_error = Some(match terminal_state {
                            magicllm::realtime::RealtimeResponseTerminalState::Cancelled =>
                                "realtime_response_cancelled",
                            magicllm::realtime::RealtimeResponseTerminalState::Incomplete =>
                                "realtime_response_incomplete",
                            magicllm::realtime::RealtimeResponseTerminalState::Failed =>
                                "realtime_response_failed",
                        });
                        break;
                    }
                    Some(RealtimeProviderEvent::NativeResumeHandleUpdated { .. })
                    | Some(RealtimeProviderEvent::SessionExpiring { .. }) => {}
                    Some(RealtimeProviderEvent::Error { message, .. }) => {
                        tracing::warn!(
                            target: "meet_bot",
                            error = %message,
                            "realtime meeting response failed"
                        );
                        response_error = Some("realtime_provider_error");
                        break;
                    }
                    Some(_) => {}
                    None => {
                        if pcm.is_empty() {
                            response_error = Some("realtime_event_stream_closed");
                        }
                        break;
                    },
                },
                _ = tokio::time::sleep(idle) => {
                    if !got_first {
                        response_error = Some("realtime_first_audio_timeout");
                    }
                    break;
                },
                _ = tokio::time::sleep_until(deadline) => {
                    response_error = Some("realtime_response_deadline");
                    break;
                },
            }
        }

        if response_error.is_none() && pcm.is_empty() {
            response_error = Some("realtime_empty_audio_response");
        }
        let response_success = response_error.is_none();

        if let Some(tokens) = input_tokens {
            guard.last_input_tokens = tokens;
        }

        // Realtime cost + latency telemetry for this turn. Cost bills the audio+text
        // tokens at the session model's realtime rates (Presto meetings run on the
        // flagship gpt-realtime-2.1, audio $32/$64 per 1M); latency reports
        // time-to-first-audio and full response wall-clock so the mini-vs-flagship
        // speed tradeoff is visible alongside cost. Latency is emitted even when the
        // provider didn't report usage (`RealtimeUsage` is Copy, so no move here).
        let response_ms = turn_start.elapsed().as_millis() as u64;
        let model = guard.descriptor.model.clone();
        let (usage_reported, reported_input_tokens, reported_output_tokens) =
            reported_realtime_totals(response_usage, input_tokens, output_tokens);
        let (turn_cost, session_cost) = if let Some(usage) = response_usage {
            guard.session_usage.add(&usage);
            (
                compute_realtime_cost(&model, &usage),
                compute_realtime_cost(&model, &guard.session_usage),
            )
        } else {
            (
                f64::NAN,
                compute_realtime_cost(&model, &guard.session_usage),
            )
        };
        let usage = response_usage.unwrap_or_default();
        tracing::info!(
            target: "voice_cost",
            model = %model,
            ttfa_ms = ttfa_ms.unwrap_or(0),
            response_ms,
            audio_in = usage.audio_input_tokens,
            audio_cached = usage.audio_cached_input_tokens,
            audio_out = usage.audio_output_tokens,
            text_in = usage.text_input_tokens,
            text_out = usage.text_output_tokens,
            turn_cost_usd = turn_cost,
            session_cost_usd = session_cost,
            "realtime meeting response cost + latency"
        );

        // Route the meeting turn into the `llm_calls` analytics ledger — same
        // schema as chat + browser/desktop voice — when a scoped broadcaster is
        // available (the meeting was joined by a scoped agent). Audio+text folded
        // into input/output/cache with the audio split preserved; cost is the
        // accurate realtime price. capability="voice.realtime" tags the row.
        if let (Some(broadcaster), Some((principal, workspace))) =
            (self.broadcaster.as_ref(), self.scope.as_ref())
        {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let to_u32 = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
            broadcaster.emit_transport_only(RuntimeTransportEvent::LLMResponseReceived {
                execution_id: format!("meeting:{principal}/{workspace}"),
                principal: Some(principal.clone()),
                workspace: Some(workspace.clone()),
                correlation: llm_correlation,
                plan_id: String::new(),
                step_id: None,
                step_index: None,
                capability: "voice.realtime".to_string(),
                success: response_success,
                decision_summary: String::new(),
                cost: turn_cost,
                latency_ms: response_ms,
                error: response_error.map(str::to_string),
                provider: "openai".to_string(),
                model: model.clone(),
                usage_reported,
                input_tokens: to_u32(reported_input_tokens),
                output_tokens: to_u32(reported_output_tokens),
                reasoning_tokens: 0,
                reasoning_summary: None,
                cache_read_tokens: to_u32(
                    usage
                        .audio_cached_input_tokens
                        .saturating_add(usage.text_cached_input_tokens),
                ),
                cache_creation_tokens: 0,
                audio_input_tokens: response_usage.map(|value| to_u32(value.audio_input_tokens)),
                audio_output_tokens: response_usage.map(|value| to_u32(value.audio_output_tokens)),
                audio_cached_tokens: response_usage
                    .map(|value| to_u32(value.audio_cached_input_tokens)),
                search_calls: 0,
                ttft_ms: ttfa_ms,
                task_id: None,
                agent_id: None,
                delegated_agent_id: None,
                chat_session_id: None,
                operation: "meeting_responder".to_string(),
                profile: None,
                attempt: 1,
                response_kind: "voice".to_string(),
                started_at_ms: llm_started_at_ms,
                timestamp: now_ms,
            });
        }

        if let Some(error) = response_error {
            return Err(ResponderError::Failed(error.to_string()));
        }

        Ok(SpokenReply {
            text: reply_text,
            pcm: bytes::Bytes::from(pcm),
            format: StreamAudioFormat {
                sample_rate_hz: REALTIME_PCM_SAMPLE_RATE,
                channels: 1,
                sample_format: Default::default(),
            },
        })
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::reported_realtime_totals;
    use magicllm::types::RealtimeUsage;

    #[test]
    fn missing_realtime_usage_is_unknown_not_reported_zero() {
        assert_eq!(reported_realtime_totals(None, None, None), (false, 0, 0));
    }

    #[test]
    fn coarse_realtime_usage_is_preserved_without_inventing_a_modality_split() {
        assert_eq!(
            reported_realtime_totals(None, Some(41), Some(7)),
            (true, 41, 7)
        );
    }

    #[test]
    fn detailed_realtime_usage_includes_cached_input_in_the_provider_total() {
        let usage = RealtimeUsage {
            text_input_tokens: 3,
            text_cached_input_tokens: 5,
            text_output_tokens: 7,
            audio_input_tokens: 11,
            audio_cached_input_tokens: 13,
            audio_output_tokens: 17,
            billed_seconds: 0.0,
        };

        assert_eq!(
            reported_realtime_totals(Some(usage), Some(999), Some(999)),
            (true, 32, 24)
        );
    }
}
