//! Process-wide spend gate: the seam between the universal LLM router and an
//! owner-set spend envelope.
//!
//! `magicllm` sits below `magician` and cannot depend on it, so the router only
//! knows this small trait. The host installs one implementation once at
//! bootstrap ([`install_spend_gate`]); with none installed (tests, other
//! embedders) the router behaves exactly as before.
//!
//! - [`SpendGate::admit`] runs before a physical provider attempt and may refuse
//!   it with [`crate::LLMError::SpendEnvelopeExhausted`].
//! - [`SpendGate::settle`] runs once per attempt that reached the provider: on
//!   a successful response with the actual cost the router computed from the
//!   active pricing table, and on a stream that ended without its final
//!   response (consumer gone, provider error after output, cancellation) with
//!   an estimate from the input size and the output produced so far.
//! - [`SpendGate::admit_embedding`] gates embedding calls the same way; they
//!   settle through [`SpendGate::settle`] with an estimated input-token count
//!   (embedding responses carry no usage) and no output.
//!
//! Both are on the hot path of every model call: implementations must do a
//! couple of atomic operations, hold no lock across an await and perform no I/O.

use std::sync::{Arc, OnceLock};

use crate::capability::LLMProviderKind;
use crate::error::LLMResult;
use crate::trace::LlmWorkloadClass;
use crate::types::{EmbeddingRequest, LLMRequest, RequestMetadata, TokenUsage};

/// What one provider attempt cost, as far as `magicllm` can tell.
#[derive(Debug, Clone, Copy)]
pub struct SpendSettlement<'a> {
    /// Workload class stamped on the request's trace context
    /// ([`LlmWorkloadClass::System`] when the request carried none).
    pub workload: LlmWorkloadClass,
    /// Scope the call was issued under (`"system"` when unattributed).
    pub principal: &'a str,
    pub workspace: &'a str,
    pub operation: &'a str,
    pub provider: &'a LLMProviderKind,
    pub model: &'a str,
    /// When the physical attempt started (epoch millis); prices the call under
    /// the rate row that was effective then.
    pub at_ms: i64,
    /// Exact cost in micro-USD when the pricing table covers the model and the
    /// provider reported usage; `None` when the model is unpriced or no usage
    /// came back, and the gate must decide how to charge it.
    pub priced_micro_usd: Option<u64>,
    /// The provider-reported usage, when there was any; for a stream that
    /// ended without its final response, or an embedding call, the router's
    /// conservative estimate.
    pub usage: Option<&'a TokenUsage>,
    /// Output tokens the request allowed (`max_output_tokens`, else a
    /// conservative default): what an unpriced call with no usage is charged
    /// against. Zero for embeddings.
    pub output_ceiling_tokens: u64,
}

/// The admission and settlement hooks of an owner-set spend envelope.
pub trait SpendGate: Send + Sync {
    /// Called before each physical provider attempt (every fallback hop).
    /// `request` already carries the effective model of that hop.
    fn admit(&self, request: &LLMRequest, provider: &LLMProviderKind) -> LLMResult<()>;

    /// Called exactly once per provider attempt that may have been billed:
    /// after each successful response, and after a stream that ended without
    /// its final response.
    fn settle(&self, settlement: &SpendSettlement<'_>);

    /// Called before each embedding call. `request` already carries the
    /// profile's model. The default applies [`SpendGate::admit`] to a
    /// generation request with the same model, operation and trace context,
    /// so a gate that does not override this still gates embeddings.
    fn admit_embedding(
        &self,
        request: &EmbeddingRequest,
        provider: &LLMProviderKind,
    ) -> LLMResult<()> {
        let proxy = LLMRequest {
            model: request.model.clone(),
            metadata: RequestMetadata {
                operation: request.metadata.operation.clone(),
                trace_context: request.metadata.trace_context.clone(),
                ..RequestMetadata::default()
            },
            ..LLMRequest::default()
        };
        self.admit(&proxy, provider)
    }
}

static INSTALLED: OnceLock<Arc<dyn SpendGate>> = OnceLock::new();

/// Install the process-wide gate. First writer wins; returns `false` when a
/// gate was already installed.
pub fn install_spend_gate(gate: Arc<dyn SpendGate>) -> bool {
    INSTALLED.set(gate).is_ok()
}

/// The process-wide gate, if the host installed one.
pub fn installed_spend_gate() -> Option<&'static Arc<dyn SpendGate>> {
    INSTALLED.get()
}

/// Output-token ceiling charged against an unpriced call that reported no
/// usage and set no `max_output_tokens`.
pub const DEFAULT_OUTPUT_CEILING_TOKENS: u64 = 8_192;

/// The workload class a request is charged under: the trace context's class,
/// or [`LlmWorkloadClass::System`] when it carries none.
///
/// Agentic decisions reach the router classed as autonomous work even when the
/// owner started the run from a chat; one that carries a chat session is the
/// owner's own request, so it is classed as interactive and never parked.
pub fn workload_of(request: &LLMRequest) -> LlmWorkloadClass {
    workload_of_metadata(&request.metadata)
}

/// [`workload_of`] over request metadata, shared by generation and embedding
/// requests.
pub fn workload_of_metadata(metadata: &RequestMetadata) -> LlmWorkloadClass {
    metadata
        .trace_context
        .as_ref()
        .map(workload_of_trace)
        .unwrap_or_default()
}

/// [`workload_of`] over a trace context. Admission and settlement both use it,
/// so a call is charged under the class it was admitted under.
pub fn workload_of_trace(context: &crate::trace::LlmTraceContext) -> LlmWorkloadClass {
    if context.workload_class == LlmWorkloadClass::AutonomousTask
        && context.chat_session_id.is_some()
    {
        return LlmWorkloadClass::InteractiveTask;
    }
    context.workload_class
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{LlmScope, LlmTraceContext};

    fn request(class: LlmWorkloadClass, chat_session: Option<&str>) -> LLMRequest {
        let mut request = LLMRequest::default();
        let mut trace = LlmTraceContext::new(LlmScope::new("test", "default"), class);
        trace.chat_session_id = chat_session.map(str::to_owned);
        request.metadata.set_trace_context(trace);
        request
    }

    #[test]
    fn an_agentic_decision_started_from_a_chat_is_interactive_not_autonomous() {
        let from_chat = request(LlmWorkloadClass::AutonomousTask, Some("chat-1"));
        assert_eq!(workload_of(&from_chat), LlmWorkloadClass::InteractiveTask);
        let on_its_own = request(LlmWorkloadClass::AutonomousTask, None);
        assert_eq!(workload_of(&on_its_own), LlmWorkloadClass::AutonomousTask);
        // Background classes keep their class even when a chat session rides along.
        let memory = request(LlmWorkloadClass::Memory, Some("chat-1"));
        assert_eq!(workload_of(&memory), LlmWorkloadClass::Memory);
        assert_eq!(
            workload_of(&LLMRequest::default()),
            LlmWorkloadClass::System
        );
    }

    #[test]
    fn the_default_embedding_admission_gates_an_embedding_like_a_generation_call() {
        #[derive(Default)]
        struct Recording(std::sync::Mutex<Vec<(LlmWorkloadClass, String, String)>>);
        impl SpendGate for Recording {
            fn admit(&self, request: &LLMRequest, _provider: &LLMProviderKind) -> LLMResult<()> {
                self.0.lock().unwrap().push((
                    workload_of(request),
                    request.model.clone(),
                    request.metadata.operation.clone(),
                ));
                Ok(())
            }

            fn settle(&self, _settlement: &SpendSettlement<'_>) {}
        }

        let gate = Recording::default();
        let mut embedding = EmbeddingRequest {
            model: "nomic-embed-text".to_string(),
            inputs: vec!["a note".to_string()],
            truncate: false,
            keep_alive: None,
            options: None,
            timeout_ms: None,
            metadata: RequestMetadata {
                operation: "memory_embed".to_string(),
                ..RequestMetadata::default()
            },
        };
        gate.admit_embedding(&embedding, &LLMProviderKind::Ollama)
            .expect("admitted");
        embedding.metadata.set_trace_context(LlmTraceContext::new(
            LlmScope::new("test", "default"),
            LlmWorkloadClass::Memory,
        ));
        gate.admit_embedding(&embedding, &LLMProviderKind::Ollama)
            .expect("admitted");
        assert_eq!(
            *gate.0.lock().unwrap(),
            vec![
                (
                    LlmWorkloadClass::System,
                    "nomic-embed-text".to_string(),
                    "memory_embed".to_string()
                ),
                (
                    LlmWorkloadClass::Memory,
                    "nomic-embed-text".to_string(),
                    "memory_embed".to_string()
                ),
            ]
        );
    }
}
