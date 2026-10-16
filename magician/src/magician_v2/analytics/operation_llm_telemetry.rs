//! Scoped telemetry bridge for direct `OperationLlmRouter` callers.
//!
//! Chat and agentic execution emit `LLMResponseReceived` from their execution
//! layers. Background workers and API helpers that call the operation router
//! directly do not pass through those layers, so they must use this bridge or
//! their token usage and cost never reach the `llm_calls` lakehouse.
//!
//! # THE THREE `emit_transport_only` CALLS BELOW ARE NOT JOURNALLED
//!
//! `run_loop::phases::outbox`'s *WHAT IS NOT JOURNALLED* counts them among the
//! phase-reachable emission sites that reach a transport without a journal
//! record.
//!
//! # WHICH PHASE REACHES WHICH — corrected 2026-08-28
//!
//! This paragraph said `decide.rs` builds an [`OperationLlmTelemetryContext`]
//! per decision "so `emit_request` / `emit_failure` / `emit_usage_outcome` are
//! on the loop's own path", and the census said the same. **That is the route
//! for `emit_usage_outcome` only.** The three do not share a reaching phase,
//! and a fixer told "Decide" would thread an address into the wrong one:
//!
//! - [`Self::emit_request`] and [`Self::emit_failure`] are reached from
//!   **Apply**. `phases::apply::dispatch` calls
//!   `executor.rs::review_terminal_draft_against_opened_evidence`, which builds
//!   its **own** context and calls both. `decide.rs`'s context reaches neither.
//! - `emit_usage_outcome` is reached from **Decide** — `decide.rs`'s context
//!   travels into `schedule_agentic_decision_job` and then
//!   `decision.rs::run_adversarial_reviewer`, which calls
//!   [`Self::emit_validated_success`] / [`Self::emit_validation_failure`] — and
//!   **also from Apply**, through the same `review_terminal_draft_…` above,
//!   which calls [`Self::emit_usage_validated_success`] /
//!   [`Self::emit_usage_validation_failure`].
//! - `emit_usage_outcome` has a **third** reaching family the census names
//!   nowhere: `execution::compiled_providers`'s `analyze_image_via_openai`
//!   capability provider builds its own context in `with_runtime_context` and
//!   reaches it through [`Self::emit_native_validated_success`] /
//!   [`Self::emit_native_validation_failure`]. That is a dispatched pack — a
//!   `dyn CapabilityProvider`, reached from Apply's tool dispatch and outside
//!   the executor call graph entirely.
//!
//! That third family is the strongest single piece of evidence for obstruction
//! 2 below: eleven public methods funnel into one emitter whose callers include
//! a phase body, a pack provider, background workers and API helpers with no
//! agentic run behind them at all. There is no one address this site could
//! take.
//!
//! **The count lives THERE and is deliberately not repeated here.** This
//! paragraph used to say "forty-two … the other thirty", and by 2026-08-28 the
//! census said fifty-three and forty-one while `executor.rs` said something
//! else again. Three files carrying one number is three chances to be the stale
//! one, and a reader who believes the stale copy reads a nearly-met gate as far
//! from met. What this file owns is the **reason** these three are refused; the
//! arithmetic belongs to the census.
//!
//! One thing the census says about them is worth repeating because it was wrong
//! there until 2026-08-28: these are **not** refused for want of a phase on the
//! stack. There is one — see the section above. They were left out of the
//! 2026-08-27 conversion that routed the executor's own sites through the
//! outbox, and the reason is not the
//! `Arc<RuntimeTransportBroadcaster>` field those docs name — it is three
//! obstructions above it, in increasing order of how much they cost to remove:
//!
//! 1. **Visibility.** `outbox::journal_and_emit` is
//!    `pub(in crate::magician_v2::execution::agentic)`. This module is not in
//!    that subtree, so it cannot name the function at all.
//! 2. **No producer state.** That function needs an `&AgenticContext` (for the
//!    execution id the buffer is keyed on) and an `&ActionExecutors`. This
//!    context holds neither, and it cannot: eleven public methods funnel into
//!    `emit_usage_outcome`, and the background workers and API helpers this
//!    bridge exists for have no agentic run behind them.
//! 3. **It would change what is emitted, not only what is recorded.**
//!    `journal_and_emit` finishes with `ActionExecutors::emit_event`, which
//!    decides per event between `broadcaster.emit` — a persisted canonical
//!    runtime fact — and `broadcaster.emit_transport_only`. These three calls
//!    are unconditionally transport-only today, so routing them through it
//!    would start persisting runtime facts for every operation-router call
//!    whose mapped execution id happens to match the canonical scope.
//!
//! So journalling these needs a journal-only entry point on `outbox` that takes
//! an execution id and an address directly, plus a way for a per-decision
//! context to carry that address. Both are outside this file, which is why this
//! is a note and not a diff.

use std::sync::Arc;

use chrono::Utc;

use crate::magician_v2::query_analysis::operation_llm_router::{
    ExecutionNativeRouterResponse, SimplifiedLLMResponse,
};
use crate::magician_v2::realtime_events::{
    LlmEventCorrelation, RuntimeTransportBroadcaster, RuntimeTransportEvent,
};
use crate::magician_v2::slot_graph::extraction::LlmCallTelemetry;

#[derive(Clone)]
pub struct OperationLlmTelemetryContext {
    broadcaster: Arc<RuntimeTransportBroadcaster>,
    principal: String,
    workspace: String,
    capability: String,
}

impl std::fmt::Debug for OperationLlmTelemetryContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperationLlmTelemetryContext")
            .field("principal", &self.principal)
            .field("workspace", &self.workspace)
            .field("capability", &self.capability)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct OperationLlmCallAttribution {
    pub execution_id: Option<String>,
    pub root_execution_id: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub delegated_agent_id: Option<String>,
    pub chat_session_id: Option<String>,
    pub attempt: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct OperationLlmTelemetryScope {
    pub principal: String,
    pub workspace: String,
    pub attribution: OperationLlmCallAttribution,
}

impl OperationLlmTelemetryScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
            attribution: OperationLlmCallAttribution::default(),
        }
    }

    pub fn with_attribution(mut self, attribution: OperationLlmCallAttribution) -> Self {
        self.attribution = attribution;
        self
    }
}

impl OperationLlmTelemetryContext {
    pub fn new(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
        capability: impl Into<String>,
    ) -> Self {
        Self {
            broadcaster,
            principal: principal.into(),
            workspace: workspace.into(),
            capability: capability.into(),
        }
    }

    pub fn scope(&self) -> magicllm::LlmScope {
        magicllm::LlmScope::new(self.principal.clone(), self.workspace.clone())
    }

    /// Publish the dispatch half of a direct operation-router call. The
    /// response half is emitted by one of the outcome helpers below. Keeping
    /// this bridge here gives background workers the same request/terminal
    /// pairing as agentic and chat execution without teaching them the
    /// transport-event schema.
    pub fn emit_request(
        &self,
        operation: &str,
        attribution: &OperationLlmCallAttribution,
        input_tokens_estimate: Option<usize>,
    ) {
        let execution_id = attribution
            .execution_id
            .clone()
            .unwrap_or_else(|| format!("{}:{}:{}", self.capability, operation, ulid::Ulid::new()));
        // NOT JOURNALLED — site 1 of the three this module's header refuses.
        // Reached from the loop's **Apply** phase, via
        // `executor.rs::review_terminal_draft_against_opened_evidence`.
        self.broadcaster
            .emit_transport_only(RuntimeTransportEvent::LLMRequestSent {
                execution_id,
                principal: Some(self.principal.clone()),
                workspace: Some(self.workspace.clone()),
                plan_id: String::new(),
                step_id: None,
                step_index: None,
                capability: self.capability.clone(),
                request_summary: operation.to_string(),
                input_tokens_estimate,
                // Direct operation callers do not own the agentic budget
                // ledger. Zero means unknown here, not unlimited.
                budget_remaining: 0.0,
                timestamp: Utc::now().timestamp_millis(),
            });
    }

    /// Publish a terminal transport failure for a direct operation-router
    /// call. The error class must be a short code-owned token; provider errors
    /// can contain request material and are deliberately kept in runtime logs
    /// rather than copied into durable analytics.
    pub fn emit_failure(
        &self,
        operation: &str,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
        error_class: &str,
    ) {
        let execution_id = attribution
            .execution_id
            .clone()
            .unwrap_or_else(|| format!("{}:{}:{}", self.capability, operation, ulid::Ulid::new()));
        let mut correlation =
            direct_lineage_correlation(&self.principal, &self.workspace, &attribution);
        let error_class = normalized_validation_class(error_class);
        // NOT JOURNALLED — site 2 of the three this module's header refuses.
        // Reached from the loop's **Apply** phase, via
        // `executor.rs::review_terminal_draft_against_opened_evidence`.
        self.broadcaster
            .emit_transport_only(RuntimeTransportEvent::LLMResponseReceived {
                execution_id,
                principal: Some(self.principal.clone()),
                workspace: Some(self.workspace.clone()),
                correlation: correlation.take(),
                plan_id: String::new(),
                step_id: None,
                step_index: None,
                capability: self.capability.clone(),
                success: false,
                decision_summary: String::new(),
                cost: 0.0,
                latency_ms,
                error: Some(error_class),
                provider: String::new(),
                model: String::new(),
                usage_reported: false,
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
                task_id: attribution.task_id,
                agent_id: attribution.agent_id,
                delegated_agent_id: attribution.delegated_agent_id,
                chat_session_id: attribution.chat_session_id,
                operation: operation.to_string(),
                profile: None,
                attempt: attribution.attempt.unwrap_or(1).max(1),
                response_kind: "error".to_string(),
                started_at_ms: Utc::now()
                    .timestamp_millis()
                    .saturating_sub(i64::try_from(latency_ms).unwrap_or(i64::MAX)),
                timestamp: Utc::now().timestamp_millis(),
            });
    }

    /// Publish one successfully completed provider call to the canonical
    /// response event stream. A response without router telemetry is ignored:
    /// inventing an unrelated identity and zero-token row would distort call
    /// counts and cost analytics. Provider-reported usage may itself be absent;
    /// that is retained explicitly through `usage_reported=false`.
    pub fn emit_success(
        &self,
        fallback_operation: &str,
        response: &SimplifiedLLMResponse,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
    ) {
        let Some(telemetry) = response.telemetry.as_ref() else {
            return;
        };
        self.emit_usage_success(fallback_operation, telemetry, latency_ms, attribution);
    }

    /// Publish usage retained by an adapter response that does not expose a
    /// complete `SimplifiedLLMResponse`.
    pub fn emit_usage_success(
        &self,
        fallback_operation: &str,
        telemetry: &LlmCallTelemetry,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
    ) {
        self.emit_usage_outcome(
            fallback_operation,
            telemetry,
            latency_ms,
            attribution,
            "text".to_string(),
            None,
        );
    }

    /// Publish a transport-successful response whose caller-side contract was
    /// validated successfully. This remains distinct from provider transport
    /// success so Phase 2 analytics never infer validity from a 2xx response.
    pub fn emit_validated_success(
        &self,
        fallback_operation: &str,
        response: &SimplifiedLLMResponse,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
        validation_class: &str,
    ) {
        let Some(telemetry) = response.telemetry.as_ref() else {
            return;
        };
        self.emit_usage_validated_success(
            fallback_operation,
            telemetry,
            latency_ms,
            attribution,
            validation_class,
        );
    }

    pub fn emit_usage_validated_success(
        &self,
        fallback_operation: &str,
        telemetry: &LlmCallTelemetry,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
        validation_class: &str,
    ) {
        self.emit_usage_outcome(
            fallback_operation,
            telemetry,
            latency_ms,
            attribution,
            format!(
                "validation_success:{}",
                normalized_validation_class(validation_class)
            ),
            None,
        );
    }

    /// Publish a provider-successful response rejected by an immediate caller
    /// contract. The event deliberately keeps `success=true`: transport and
    /// validation are separate facts, while `response_kind` carries the typed
    /// validation outcome consumed by the Phase 2 activation bridge.
    pub fn emit_validation_failure(
        &self,
        fallback_operation: &str,
        response: &SimplifiedLLMResponse,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
        validation_class: &str,
        error: &str,
    ) {
        let Some(telemetry) = response.telemetry.as_ref() else {
            return;
        };
        self.emit_usage_validation_failure(
            fallback_operation,
            telemetry,
            latency_ms,
            attribution,
            validation_class,
            error,
        );
    }

    pub fn emit_usage_validation_failure(
        &self,
        fallback_operation: &str,
        telemetry: &LlmCallTelemetry,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
        validation_class: &str,
        error: &str,
    ) {
        // Parser/provider payloads can contain user text or tool arguments.
        // Persist only a typed, content-free contract error; the normalized
        // validation class in `response_kind` retains the actionable detail.
        let _ = error;
        let validation_class = normalized_validation_class(validation_class);
        self.emit_usage_outcome(
            fallback_operation,
            telemetry,
            latency_ms,
            attribution,
            format!("validation_error:{validation_class}"),
            Some(format!(
                "caller contract validation failed ({validation_class})"
            )),
        );
    }

    fn emit_usage_outcome(
        &self,
        fallback_operation: &str,
        telemetry: &LlmCallTelemetry,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
        response_kind: String,
        error: Option<String>,
    ) {
        let operation = telemetry
            .operation
            .clone()
            .unwrap_or_else(|| fallback_operation.to_string());
        let execution_id = attribution
            .execution_id
            .clone()
            // A direct caller may omit attribution while the real provider
            // receipt already carries execution lineage. Reuse it before
            // creating a background-only ID, or the ledger sees a conflict.
            .or_else(|| {
                telemetry
                    .trace_receipt
                    .as_ref()
                    .and_then(|receipt| receipt.context.execution_id.clone())
            })
            .unwrap_or_else(|| format!("{}:{}:{}", self.capability, operation, ulid::Ulid::new()));
        let has_lineage = attribution.task_id.is_some()
            || attribution.root_execution_id.is_some()
            || attribution.execution_id.is_some()
            || attribution.chat_session_id.is_some();
        let mut correlation = telemetry
            .trace_receipt
            .as_ref()
            .and_then(|receipt| {
                LlmEventCorrelation::scoped(
                    receipt,
                    self.principal.clone(),
                    self.workspace.clone(),
                    receipt.context.workload_class,
                )
            })
            .or_else(|| {
                // Older/custom adapters can report priced usage without a
                // trace receipt. Preserve the caller's durable task tree
                // lineage instead of writing an unjoinable cost row. Modern
                // adapters keep their exact receipt and workload class above.
                has_lineage.then(|| {
                    LlmEventCorrelation::direct(
                        self.principal.clone(),
                        self.workspace.clone(),
                        magicllm::LlmWorkloadClass::System,
                    )
                })
            });
        if let Some(correlation) = correlation.as_mut() {
            if correlation.task_id.is_none() {
                correlation.task_id.clone_from(&attribution.task_id);
            }
            if correlation.root_execution_id.is_none() {
                correlation
                    .root_execution_id
                    .clone_from(&attribution.root_execution_id);
            }
            if correlation.execution_id.is_none() {
                correlation
                    .execution_id
                    .clone_from(&attribution.execution_id);
            }
            if correlation.chat_session_id.is_none() {
                correlation
                    .chat_session_id
                    .clone_from(&attribution.chat_session_id);
            }
        }

        // NOT JOURNALLED — site 3 of the three this module's header refuses,
        // and the one with three separate reaching families: **Decide** through
        // `decision.rs::run_adversarial_reviewer`, **Apply** through
        // `executor.rs::review_terminal_draft_against_opened_evidence`, and a
        // dispatched capability pack through
        // `execution::compiled_providers`'s `analyze_image_via_openai`. Eleven
        // public methods funnel here; there is no one phase address to take.
        self.broadcaster
            .emit_transport_only(RuntimeTransportEvent::LLMResponseReceived {
                execution_id,
                principal: Some(self.principal.clone()),
                workspace: Some(self.workspace.clone()),
                correlation,
                plan_id: String::new(),
                step_id: None,
                step_index: None,
                capability: self.capability.clone(),
                success: true,
                decision_summary: String::new(),
                cost: telemetry.cost_usd,
                latency_ms,
                error,
                provider: telemetry.provider.clone(),
                model: telemetry.model.clone(),
                usage_reported: telemetry.usage_reported,
                input_tokens: telemetry.input_tokens,
                output_tokens: telemetry.output_tokens,
                reasoning_tokens: telemetry.reasoning_tokens,
                reasoning_summary: telemetry.reasoning_summary.clone(),
                cache_read_tokens: telemetry.cache_read_tokens,
                cache_creation_tokens: telemetry.cache_creation_tokens,
                audio_input_tokens: None,
                audio_output_tokens: None,
                audio_cached_tokens: None,
                search_calls: telemetry.search_calls,
                ttft_ms: None,
                task_id: attribution.task_id,
                agent_id: attribution.agent_id,
                delegated_agent_id: attribution.delegated_agent_id,
                chat_session_id: attribution.chat_session_id,
                operation,
                profile: telemetry.profile.clone(),
                attempt: attribution.attempt.unwrap_or(1).max(1),
                response_kind,
                started_at_ms: telemetry.started_at_ms,
                timestamp: Utc::now().timestamp_millis(),
            });
    }

    pub fn emit_native_success(
        &self,
        fallback_operation: &str,
        response: &ExecutionNativeRouterResponse,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
    ) {
        let projected = SimplifiedLLMResponse {
            telemetry: response.telemetry.clone(),
            ..SimplifiedLLMResponse::default()
        };
        self.emit_success(fallback_operation, &projected, latency_ms, attribution);
    }

    pub fn emit_native_validated_success(
        &self,
        fallback_operation: &str,
        response: &ExecutionNativeRouterResponse,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
        validation_class: &str,
    ) {
        let projected = SimplifiedLLMResponse {
            telemetry: response.telemetry.clone(),
            ..SimplifiedLLMResponse::default()
        };
        self.emit_validated_success(
            fallback_operation,
            &projected,
            latency_ms,
            attribution,
            validation_class,
        );
    }

    pub fn emit_native_validation_failure(
        &self,
        fallback_operation: &str,
        response: &ExecutionNativeRouterResponse,
        latency_ms: u64,
        attribution: OperationLlmCallAttribution,
        validation_class: &str,
        error: &str,
    ) {
        let projected = SimplifiedLLMResponse {
            telemetry: response.telemetry.clone(),
            ..SimplifiedLLMResponse::default()
        };
        self.emit_validation_failure(
            fallback_operation,
            &projected,
            latency_ms,
            attribution,
            validation_class,
            error,
        );
    }
}

fn direct_lineage_correlation(
    principal: &str,
    workspace: &str,
    attribution: &OperationLlmCallAttribution,
) -> Option<LlmEventCorrelation> {
    let has_lineage = attribution.task_id.is_some()
        || attribution.root_execution_id.is_some()
        || attribution.execution_id.is_some()
        || attribution.chat_session_id.is_some();
    has_lineage.then(|| {
        let mut correlation = LlmEventCorrelation::direct(
            principal.to_string(),
            workspace.to_string(),
            magicllm::LlmWorkloadClass::System,
        );
        correlation.task_id.clone_from(&attribution.task_id);
        correlation
            .root_execution_id
            .clone_from(&attribution.root_execution_id);
        correlation
            .execution_id
            .clone_from(&attribution.execution_id);
        correlation
            .chat_session_id
            .clone_from(&attribution.chat_session_id);
        correlation
    })
}

fn normalized_validation_class(value: &str) -> String {
    const MAX_VALIDATION_CLASS_BYTES: usize = 64;
    let value = value.trim();
    if !value.is_empty()
        && value.len() <= MAX_VALIDATION_CLASS_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        value.to_ascii_lowercase()
    } else {
        // The class is supplied by code and should be a short machine token.
        // Collapse malformed/prose input instead of transforming and retaining
        // fragments that could contain parser, prompt, or response content.
        "contract".to_string()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse;
    use crate::magician_v2::slot_graph::extraction::LlmCallTelemetry;

    #[tokio::test]
    async fn emits_scoped_priced_operation_response() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(8));
        let mut events = broadcaster.subscribe();
        let context = OperationLlmTelemetryContext::new(
            broadcaster,
            "principal-a",
            "workspace-a",
            "memory_consolidation",
        );
        let response = SimplifiedLLMResponse {
            content: "{}".to_string(),
            telemetry: Some(LlmCallTelemetry {
                usage_availability: None,
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
                usage_reported: true,
                input_tokens: 120,
                output_tokens: 30,
                reasoning_tokens: 5,
                cache_read_tokens: 20,
                cache_creation_tokens: 0,
                search_calls: 0,
                cost_usd: 0.0123,
                reasoning_summary: None,
                profile: Some("memory-small".to_string()),
                operation: Some("memory_user_promotion".to_string()),
                started_at_ms: 42,
                trace_receipt: None,
                prompt_projection_mode: None,
            }),
            ..SimplifiedLLMResponse::default()
        };

        context.emit_success(
            "fallback",
            &response,
            17,
            OperationLlmCallAttribution {
                execution_id: Some("execution-a".to_string()),
                root_execution_id: Some("execution-root".to_string()),
                task_id: Some("task-a".to_string()),
                agent_id: Some("agent-a".to_string()),
                ..OperationLlmCallAttribution::default()
            },
        );

        let event = events.recv().await.expect("telemetry event");
        let RuntimeTransportEvent::LLMResponseReceived {
            principal,
            workspace,
            capability,
            operation,
            provider,
            model,
            usage_reported,
            input_tokens,
            output_tokens,
            cost,
            task_id,
            agent_id,
            correlation,
            ..
        } = event
        else {
            panic!("unexpected event variant");
        };
        assert_eq!(principal.as_deref(), Some("principal-a"));
        assert_eq!(workspace.as_deref(), Some("workspace-a"));
        assert_eq!(capability, "memory_consolidation");
        assert_eq!(operation, "memory_user_promotion");
        assert_eq!(provider, "openai");
        assert_eq!(model, "gpt-test");
        assert!(usage_reported);
        assert_eq!(input_tokens, 120);
        assert_eq!(output_tokens, 30);
        assert_eq!(cost, 0.0123);
        assert_eq!(task_id.as_deref(), Some("task-a"));
        assert_eq!(agent_id.as_deref(), Some("agent-a"));
        let correlation = correlation.expect("lineage correlation");
        assert_eq!(correlation.task_id.as_deref(), Some("task-a"));
        assert_eq!(
            correlation.root_execution_id.as_deref(),
            Some("execution-root")
        );
        assert_eq!(correlation.execution_id.as_deref(), Some("execution-a"));

        let mut response = response;
        let mut trace = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("principal-a", "workspace-a"),
            magicllm::LlmWorkloadClass::Memory,
        );
        trace.task_id = Some("receipt-task".into());
        trace.root_execution_id = Some("receipt-root".into());
        trace.execution_id = Some("receipt-execution".into());
        let call_id = trace.llm_call_id.clone();
        response.telemetry.as_mut().unwrap().trace_receipt =
            Some(magicllm::LlmTraceReceipt::direct(trace));
        context.emit_success("fallback", &response, 17, Default::default());
        let RuntimeTransportEvent::LLMResponseReceived {
            execution_id,
            correlation: Some(correlation),
            ..
        } = events.recv().await.expect("receipt-backed response")
        else {
            panic!("receipt-backed correlation missing");
        };
        assert_eq!(execution_id, "receipt-execution");
        assert_eq!(
            correlation.execution_id.as_deref(),
            Some(execution_id.as_str())
        );
        assert_eq!(correlation.llm_call_id, call_id);
        assert_eq!(correlation.task_id.as_deref(), Some("receipt-task"));
        assert_eq!(
            correlation.root_execution_id.as_deref(),
            Some("receipt-root")
        );
    }

    #[tokio::test]
    async fn direct_operation_failure_emits_request_and_root_attributed_terminal_event() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(8));
        let mut events = broadcaster.subscribe();
        let context = OperationLlmTelemetryContext::new(
            broadcaster,
            "principal-a",
            "workspace-a",
            "artifact_synthesis",
        );
        let attribution = OperationLlmCallAttribution {
            execution_id: Some("execution-a".to_string()),
            root_execution_id: Some("execution-root".to_string()),
            task_id: Some("task-a".to_string()),
            agent_id: Some("agent-a".to_string()),
            chat_session_id: Some("chat-a".to_string()),
            ..OperationLlmCallAttribution::default()
        };

        context.emit_request("task_summary", &attribution, Some(123));
        context.emit_failure("task_summary", 17, attribution, "task_summary_router_error");

        assert!(matches!(
            events.recv().await.expect("request event"),
            RuntimeTransportEvent::LLMRequestSent {
                execution_id,
                principal: Some(principal),
                workspace: Some(workspace),
                capability,
                request_summary,
                input_tokens_estimate: Some(123),
                ..
            } if execution_id == "execution-a"
                && principal == "principal-a"
                && workspace == "workspace-a"
                && capability == "artifact_synthesis"
                && request_summary == "task_summary"
        ));
        let terminal = events.recv().await.expect("terminal event");
        let RuntimeTransportEvent::LLMResponseReceived {
            success,
            error,
            operation,
            task_id,
            agent_id,
            chat_session_id,
            correlation,
            ..
        } = terminal
        else {
            panic!("expected terminal LLM event");
        };
        assert!(!success);
        assert_eq!(error.as_deref(), Some("task_summary_router_error"));
        assert_eq!(operation, "task_summary");
        assert_eq!(task_id.as_deref(), Some("task-a"));
        assert_eq!(agent_id.as_deref(), Some("agent-a"));
        assert_eq!(chat_session_id.as_deref(), Some("chat-a"));
        let correlation = correlation.expect("root lineage correlation");
        assert_eq!(correlation.task_id.as_deref(), Some("task-a"));
        assert_eq!(
            correlation.root_execution_id.as_deref(),
            Some("execution-root")
        );
        assert_eq!(correlation.execution_id.as_deref(), Some("execution-a"));
    }

    #[test]
    fn response_without_router_telemetry_does_not_inflate_call_count() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(8));
        let mut events = broadcaster.subscribe();
        let context = OperationLlmTelemetryContext::new(
            broadcaster,
            "principal-a",
            "workspace-a",
            "memory_consolidation",
        );

        context.emit_success(
            "memory_user_promotion",
            &SimplifiedLLMResponse::content_only("{}"),
            1,
            OperationLlmCallAttribution::default(),
        );

        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn validation_outcomes_preserve_transport_success_and_are_typed() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(8));
        let mut events = broadcaster.subscribe();
        let context = OperationLlmTelemetryContext::new(
            broadcaster,
            "principal-a",
            "workspace-a",
            "query_analysis",
        );
        let response = SimplifiedLLMResponse {
            content: "{}".to_string(),
            telemetry: Some(LlmCallTelemetry {
                usage_availability: None,
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
                usage_reported: false,
                input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                search_calls: 0,
                cost_usd: 0.0,
                reasoning_summary: None,
                profile: Some("query-fast".to_string()),
                operation: Some("query_analysis".to_string()),
                started_at_ms: 42,
                trace_receipt: None,
                prompt_projection_mode: None,
            }),
            ..SimplifiedLLMResponse::default()
        };

        context.emit_validated_success(
            "query_analysis",
            &response,
            5,
            OperationLlmCallAttribution::default(),
            "json-schema",
        );
        context.emit_validation_failure(
            "query_analysis",
            &response,
            6,
            OperationLlmCallAttribution::default(),
            "not a machine category",
            "missing required field",
        );

        let first = events.recv().await.expect("validated event");
        let second = events.recv().await.expect("rejected event");
        assert!(matches!(
            first,
            RuntimeTransportEvent::LLMResponseReceived {
                success: true,
                response_kind,
                error: None,
                usage_reported: false,
                ..
            } if response_kind == "validation_success:json-schema"
        ));
        assert!(matches!(
            second,
            RuntimeTransportEvent::LLMResponseReceived {
                success: true,
                response_kind,
                error: Some(error),
                ..
            } if response_kind == "validation_error:contract"
                && error == "caller contract validation failed (contract)"
        ));
    }

    #[test]
    fn validation_class_normalization_never_persists_prose_or_oversized_input() {
        assert_eq!(normalized_validation_class("json_schema"), "json_schema");
        assert_eq!(
            normalized_validation_class("  JSON-SCHEMA  "),
            "json-schema"
        );
        assert_eq!(
            normalized_validation_class("missing field contained private user content"),
            "contract"
        );
        assert_eq!(normalized_validation_class(&"x".repeat(65)), "contract");
    }
}
