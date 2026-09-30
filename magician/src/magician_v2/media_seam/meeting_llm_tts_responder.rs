//! Concrete meeting responder: profile-routed answer + OpenAI TTS → PCM.
//!
//! On a wake phrase, drafts a short spoken reply through the configured
//! `meeting_response` operation grounded in the meeting context,
//! then synthesizes it with OpenAI TTS as 24 kHz mono PCM16 for the
//! [`AudioSink`](crate::magician_v2::media_seam::meeting_audio::AudioSink) to inject. Mirrors the validated
//! Spike-2 flow (answer → TTS → meeting mic) but with a natural OpenAI voice
//! instead of macOS `say`.

use async_trait::async_trait;

use crate::magician_v2::media_seam::responder::{MeetingResponder, ResponderError, SpokenReply};
use crate::magician_v2::media_seam::{
    OpenAiTtsProvider, StreamAudioFormat, TtsProvider, TtsRequest,
};

/// OpenAI's `pcm` TTS output is 24 kHz, 16-bit, mono, little-endian.
const OPENAI_TTS_PCM_SAMPLE_RATE: u32 = 24_000;

const RESPONDER_SYSTEM_PROMPT: &str = "You are Presto, an AI assistant in a live \
meeting. A participant addressed you directly. Using the meeting context, give a \
brief, helpful reply in 1-2 short sentences of spoken English — answer \
immediately with no preamble, no markdown, no lists, no stage directions; it \
will be read aloud. If there is no clear question, offer the single most useful \
next step.";

/// Profile-routed answer + OpenAI TTS responder.
pub struct LlmTtsResponder {
    router: Option<
        std::sync::Arc<
            crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter,
        >,
    >,
    tts: OpenAiTtsProvider,
    voice: Option<String>,
    tts_model: Option<String>,
    telemetry: Option<
        crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext,
    >,
    execution_id: Option<String>,
}

impl LlmTtsResponder {
    /// Build from the OpenAI key (TTS), the global operation router, and
    /// optional TTS voice/model environment overrides.
    pub fn from_env(openai_api_key: impl Into<String>) -> Self {
        let voice = std::env::var("MEET_BOT_TTS_VOICE").ok();
        // Low-latency TTS via MEET_BOT_TTS_MODEL=tts-1; default keeps the
        // provider's natural gpt-4o-mini-tts.
        let tts_model = std::env::var("MEET_BOT_TTS_MODEL").ok();
        Self {
            router:
                crate::magician_v2::query_analysis::operation_llm_router::global_operation_router(),
            tts: OpenAiTtsProvider::new(openai_api_key),
            voice,
            tts_model,
            telemetry: None,
            execution_id: None,
        }
    }

    pub fn with_telemetry(
        mut self,
        broadcaster: Option<
            std::sync::Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
        >,
        scope: Option<(String, String)>,
        execution_id: Option<String>,
    ) -> Self {
        if let (Some(router), Some((principal, workspace))) = (self.router.as_ref(), scope.as_ref())
        {
            self.router = Some(std::sync::Arc::new(
                router.with_scope_context(Some(magicllm::LlmScope::new(principal, workspace))),
            ));
        }
        self.telemetry = broadcaster.zip(scope).map(|(broadcaster, (principal, workspace))| {
            crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
                broadcaster,
                principal,
                workspace,
                "meeting_response",
            )
        });
        self.execution_id = execution_id;
        self
    }

    pub fn with_voice(mut self, voice: impl Into<String>) -> Self {
        self.voice = Some(voice.into());
        self
    }

    async fn answer(&self, utterance: &str, context: &str) -> Result<String, ResponderError> {
        use crate::magician_v2::query_analysis::operation_llm_router::LLMOperation;
        let router = self.router.as_ref().ok_or_else(|| {
            ResponderError::Failed(
                "operation router unavailable; configure llm.router.operation_mapping.meeting_response"
                    .to_string(),
            )
        })?;
        let user = format!(
            "Meeting context so far:\n{context}\n\nA participant just addressed you and \
             said:\n\"{utterance}\"\n\nYour spoken reply:"
        );
        let llm_started = std::time::Instant::now();
        let response = router
            .generate_for_execution_native_tools(
                &LLMOperation::MeetingResponse,
                Some(RESPONDER_SYSTEM_PROMPT),
                &user,
                Vec::new(),
                None,
                None,
                None,
                None,
            )
            .await
            .map_err(|error| ResponderError::Failed(error.to_string()))?;
        let answer = response
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .ok_or_else(|| ResponderError::Failed("meeting response was empty".to_string()));
        if let Some(telemetry) = self.telemetry.as_ref() {
            let attribution = crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution {
                execution_id: self.execution_id.clone(),
                ..Default::default()
            };
            let latency_ms = llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            match answer.as_ref() {
                Ok(_) => telemetry.emit_native_validated_success(
                    LLMOperation::MeetingResponse.as_str(),
                    &response,
                    latency_ms,
                    attribution,
                    "meeting_response_nonempty",
                ),
                Err(error) => telemetry.emit_native_validation_failure(
                    LLMOperation::MeetingResponse.as_str(),
                    &response,
                    latency_ms,
                    attribution,
                    "meeting_response_nonempty",
                    &error.to_string(),
                ),
            }
        }
        answer
    }
}

#[async_trait]
impl MeetingResponder for LlmTtsResponder {
    async fn respond(&self, utterance: &str, context: &str) -> Result<SpokenReply, ResponderError> {
        let text = self.answer(utterance, context).await?;
        if text.is_empty() {
            return Ok(SpokenReply {
                text,
                pcm: bytes::Bytes::new(),
                format: StreamAudioFormat::default(),
            });
        }
        let request = TtsRequest {
            text: text.clone(),
            voice: self.voice.clone(),
            rate: None,
            model: self.tts_model.clone(),
            format: Some("pcm".to_string()),
            message_id: None,
            emotion: None,
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        };
        let synth = self
            .tts
            .synthesize(request)
            .await
            .map_err(|e| ResponderError::Failed(format!("tts: {e}")))?;
        Ok(SpokenReply {
            text,
            pcm: synth.audio,
            format: StreamAudioFormat {
                sample_rate_hz: OPENAI_TTS_PCM_SAMPLE_RATE,
                channels: 1,
                sample_format: Default::default(),
            },
        })
    }
}
