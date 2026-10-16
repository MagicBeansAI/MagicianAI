//! Meeting/observation transcript summarizer. See the meet-bot design doc §5.D.
//!
//! Every summary goes through [`RouterSummarizer`] via [`default_summarizer()`]:
//! the `meeting_summary` operation in `magician-config.yaml` → a profile, so the
//! model (and the ollama request contract) is config-managed like every other
//! LLM call, funnelled through `magicllm`'s single ollama chokepoint
//! (`OllamaProvider`) rather than a hand-built HTTP body. The prompt pins English
//! output and preserves Hindi names/terms, and instructs against the known
//! code-switch failure modes (omission / hallucination). Tests stub the
//! [`Summarizer`] trait directly (no Ollama/HTTP).

use async_trait::async_trait;
use std::sync::{Arc, OnceLock, RwLock};

pub const DEFAULT_OLLAMA_KEEP_ALIVE: &str = "10m";

static CONFIGURED_DEFAULT_KEEP_ALIVE: OnceLock<RwLock<Option<Option<String>>>> = OnceLock::new();

const SUMMARY_SYSTEM_PROMPT: &str = "You are a meeting assistant. Summarize the \
meeting transcript provided by the user. The transcript may mix English and \
Hindi (Hinglish / code-switching). Write the summary in ENGLISH, but preserve \
names, product names, and key terms verbatim (including any in Hindi). Be \
faithful: do NOT omit any decision or action item, and do NOT invent anything \
not in the transcript. Output exactly these sections:\n\
## Summary  (3-4 sentences)\n\
## Decisions  (bullets)\n\
## Action items  (bullets, with owner + due if stated)";

#[derive(Debug, thiserror::Error)]
pub enum SummarizerError {
    #[error("ollama transport: {0}")]
    Transport(String),
    #[error("ollama returned {status}: {body}")]
    Upstream { status: u16, body: String },
    #[error("ollama response malformed: {0}")]
    Malformed(String),
    #[error("summarizer configuration: {0}")]
    Configuration(String),
}

pub fn normalize_ollama_keep_alive(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn configured_default_keep_alive() -> &'static RwLock<Option<Option<String>>> {
    CONFIGURED_DEFAULT_KEEP_ALIVE.get_or_init(|| RwLock::new(None))
}

pub fn set_default_ollama_keep_alive(value: Option<String>) {
    if let Ok(mut guard) = configured_default_keep_alive().write() {
        *guard = Some(value.and_then(|value| normalize_ollama_keep_alive(&value)));
    }
}

/// The summarizer seam the meeting session depends on. A trait (rather than a
/// concrete type) so the teardown chain — summarize → parse → memory-write — can
/// be exercised offline with a stub, no Ollama/HTTP needed.
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, transcript: &str) -> Result<String, SummarizerError>;
}

/// Router-backed summarizer: routes through `magician-config.yaml`'s
/// `meeting_summary` operation → profile (the config-managed path). This is the
/// only production summarizer; the ollama request is shaped by `magicllm`'s
/// `OllamaProvider`, not a hand-built HTTP body. Tests stub the `Summarizer`
/// trait directly.
pub struct RouterSummarizer {
    router: std::sync::Arc<
        crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter,
    >,
    telemetry: Option<
        crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext,
    >,
    execution_id: Option<String>,
}

#[async_trait]
impl Summarizer for RouterSummarizer {
    async fn summarize(&self, transcript: &str) -> Result<String, SummarizerError> {
        use crate::magician_v2::query_analysis::operation_llm_router::LLMOperation;
        // Store-managed prompt (data/magician_v2/prompts/); the compiled
        // const is the degrade-loudly fallback. The planned
        // observation-shaped summary variant is a prompt-file change.
        let system = crate::magician_v2::prompts::rendered_prompt_or(
            crate::magician_v2::prompts::names::MEETING_SUMMARY_SYSTEM,
            crate::magician_v2::prompts::versions::MEETING_SUMMARY_SYSTEM,
            std::collections::HashMap::new(),
            SUMMARY_SYSTEM_PROMPT,
        )
        .await;
        let llm_started = std::time::Instant::now();
        let response = self
            .router
            .generate_for_execution_native_tools(
                &LLMOperation::MeetingSummary,
                Some(system.as_str()),
                transcript,
                Vec::new(),
                None,
                None,
                None,
                None,
            )
            .await
            .map_err(|e| SummarizerError::Transport(e.to_string()))?;
        let summary = response
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                SummarizerError::Malformed("router returned an empty summary".to_string())
            });
        if let Some(telemetry) = self.telemetry.as_ref() {
            let attribution = crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution {
                execution_id: self.execution_id.clone(),
                ..Default::default()
            };
            let latency_ms = llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            match summary.as_ref() {
                Ok(_) => telemetry.emit_native_validated_success(
                    LLMOperation::MeetingSummary.as_str(),
                    &response,
                    latency_ms,
                    attribution,
                    "meeting_summary_nonempty",
                ),
                Err(error) => telemetry.emit_native_validation_failure(
                    LLMOperation::MeetingSummary.as_str(),
                    &response,
                    latency_ms,
                    attribution,
                    "meeting_summary_nonempty",
                    &error.to_string(),
                ),
            }
        }
        summary
    }
}

/// THE summarizer constructor every rail uses: the config-routed
/// `meeting_summary` operation. If the process-global router is unavailable,
/// calls fail explicitly instead of selecting a compiled or environment model.
pub fn default_summarizer() -> std::sync::Arc<dyn Summarizer> {
    default_summarizer_with_telemetry(None, None, None)
}

pub fn default_summarizer_with_telemetry(
    broadcaster: Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
    scope: Option<(String, String)>,
    execution_id: Option<String>,
) -> Arc<dyn Summarizer> {
    match crate::magician_v2::query_analysis::operation_llm_router::global_operation_router() {
        Some(router) => {
            let router =
                scope
                    .as_ref()
                    .map_or(router.clone(), |(principal, workspace)| {
                        Arc::new(router.with_scope_context(Some(magicllm::LlmScope::new(
                            principal, workspace,
                        ))))
                    });
            let telemetry = broadcaster.zip(scope).map(|(broadcaster, (principal, workspace))| {
                crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
                    broadcaster,
                    principal,
                    workspace,
                    "meeting_summary",
                )
            });
            Arc::new(RouterSummarizer {
                router,
                telemetry,
                execution_id,
            })
        },
        None => Arc::new(UnavailableSummarizer),
    }
}

struct UnavailableSummarizer;

#[async_trait]
impl Summarizer for UnavailableSummarizer {
    async fn summarize(&self, _transcript: &str) -> Result<String, SummarizerError> {
        Err(SummarizerError::Configuration(
            "operation router is unavailable; configure llm.router.operation_mapping.meeting_summary"
                .to_string(),
        ))
    }
}
