//! Live Thinking Map — production [`InterpreterLlm`] adapter (Phase 3 wiring).
//!
//! Bridges the dormant [`super::interpreter`] to the real
//! [`OperationLlmRouter`]. The interpreter only needs `complete(system, user)
//! -> String`; this adapter relays that to the router under the dedicated
//! `thinking_map_interpret` operation, which `magician-config.yaml`'s
//! `llm.router.operation_mapping` maps to the `op-thinking-map-interpret`
//! profile (an isolated OpenAI GPT-5.6 Terra structured-JSON profile — NOT a
//! shared memory/chat profile).
//!
//! The interpreter already instructs the model to return `{"operations":[...]}`
//! and tolerantly parses fences/prose, so plain text output is acceptable; this
//! adapter does no schema plumbing.
//!
//! ## Metrics
//! [`OperationLlmRouter`] only *computes* usage/cost into the response — direct
//! callers (background workers, API helpers) that bypass the chat/execution
//! layers must emit it themselves via [`OperationLlmTelemetryContext`] or their
//! token usage and cost never reach the shared `llm_calls` lakehouse / `/llm`
//! dashboard. This adapter therefore times each call and emits an
//! `LLMResponseReceived` event under the `thinking_map_interpret` operation
//! label — the same pipeline chat turns feed. When no broadcaster is available
//! (e.g. unit tests) the call still runs, unmetered.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;

use super::interpreter::InterpreterLlm;
use magician::magician_v2::analytics::operation_llm_telemetry::{
    OperationLlmCallAttribution, OperationLlmTelemetryContext,
};
use magician::magician_v2::query_analysis::operation_llm_router::{
    LLMOperation, OperationLlmRouter,
};
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

/// The operation name mapped in `llm.router.operation_mapping` →
/// `op-thinking-map-interpret`. Routed via [`LLMOperation::Other`] so no enum
/// edit is needed; also the capability/operation label under which usage is
/// metered.
const THINKING_MAP_INTERPRET_OP: &str = "thinking_map_interpret";

/// Production [`InterpreterLlm`] backed by the shared [`OperationLlmRouter`],
/// with usage/cost telemetry emitted to the `llm_calls` lakehouse.
pub struct RouterInterpreterLlm {
    router: Arc<OperationLlmRouter>,
    /// Scoped telemetry bridge (`principal`/`workspace`, capability
    /// `thinking_map_interpret`). `None` only when no broadcaster is available
    /// (unit tests / pre-startup) — the interpret call still runs, unmetered.
    telemetry: Option<OperationLlmTelemetryContext>,
}

impl RouterInterpreterLlm {
    /// Build the adapter. `broadcaster` is `Some` in normal operation (the actix
    /// app registers one), enabling telemetry emission; `None` disables it.
    pub fn new(
        router: Arc<OperationLlmRouter>,
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        let principal = principal.into();
        let workspace = workspace.into();
        let router = Arc::new(router.with_scope_context(Some(magicllm::LlmScope::new(
            principal.clone(),
            workspace.clone(),
        ))));
        let telemetry = broadcaster.map(|broadcaster| {
            OperationLlmTelemetryContext::new(
                broadcaster,
                principal,
                workspace,
                THINKING_MAP_INTERPRET_OP,
            )
        });
        Self { router, telemetry }
    }
}

#[async_trait]
impl InterpreterLlm for RouterInterpreterLlm {
    async fn complete(&self, system: &str, user: &str) -> anyhow::Result<String> {
        let op = LLMOperation::Other(THINKING_MAP_INTERPRET_OP.to_string());
        let started = Instant::now();
        let resp = self
            .router
            .generate_for_operation_with_system(&op, Some(system), user)
            .await?;
        let latency_ms = started.elapsed().as_millis() as u64;
        // Emit usage/cost to the shared `llm_calls` lakehouse (same pipeline as
        // chat). A response without usage telemetry is ignored by `emit_success`.
        if let Some(telemetry) = &self.telemetry {
            telemetry.emit_success(
                THINKING_MAP_INTERPRET_OP,
                &resp,
                latency_ms,
                OperationLlmCallAttribution::default(),
            );
        }
        Ok(resp.content)
    }
}
