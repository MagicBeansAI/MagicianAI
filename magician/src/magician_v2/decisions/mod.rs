//! Shared classification runner. Callers retain text generation and mutation.
pub(crate) mod mixed;
pub mod observation;
pub mod reference;
pub mod runner;
pub mod telemetry;

#[cfg(test)]
pub(crate) mod test_engine;
pub(crate) mod text;

/// Scoped policy for satellite classification owners; unknown policy preserves
/// each caller's own unresolved-item behavior.
pub async fn policy_lookup(operation: &str, scope: &magicllm::LlmScope) -> PolicyLookup {
    crate::magician_v2::decision_host::classification::ready_policy(
        operation,
        &scope.principal,
        &scope.workspace,
    )
    .await
}
pub use crate::magician_v2::decision_host::classification::{
    Participation, PolicyFailure, PolicyLookup,
};

/// Preserve the router's authoritative scoped flow attribution for satellites.
pub fn trace_context(
    router: &crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter,
    scope: magicllm::LlmScope,
) -> magicllm::LlmTraceContext {
    router.classification_trace_context(scope)
}
