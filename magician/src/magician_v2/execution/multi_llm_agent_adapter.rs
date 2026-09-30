// Multimodal LLM adapter for execution agents that properly supports vision and reasoning
//
// This adapter wraps the operation-aware LLM router and routes agent requests to appropriate operations:
// - Vision/Page Understanding → uses unified multimodal models (GPT-5, Sonnet 4.5, etc.)
// - Validation → uses reasoning-capable models for complex validation
//
// Modern unified models like GPT-5 and Sonnet 4.5 natively support both text and vision,
// eliminating the need for separate vision-specific models.
//
// Unlike MultiLlmExtractionAdapter which is hardcoded to SlotExtraction,
// this adapter intelligently selects the right operation based on the request.

use crate::magician_v2::{
    execution::agentic::native_adapter::{
        execute_native_messages_request_with_chain, execute_native_request,
    },
    execution::agentic::native_lowering::lower_native_response,
    execution::agentic::native_types::{
        ExecutionDecisionEnvelope, ExecutionNativeRequest, ExecutionNativeResponse,
        NativeDecisionOutcome, NativeExecutionTool,
    },
    execution::durable_task_state::TaskStateActionEnvelope,
    query_analysis::operation_llm_router::{
        LLMOperation, OperationLlmRouter, OperationRoutingOverrides,
    },
    slot_graph::extraction::{LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService},
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use magicllm::prelude::LLMMessage as RouterMessage;
use serde_json::Value;
#[cfg(any(test, debug_assertions))]
use std::collections::VecDeque;
use std::sync::Arc;
#[cfg(any(test, debug_assertions))]
use std::sync::Mutex;
use tracing::{debug, error, warn};

/// A scripted model, for tests that exercise runtime plumbing without a
/// provider: one FIFO of native responses, shared by every executor set the
/// orchestrator builds for a run — so a run that pauses and resumes continues
/// its script instead of starting it over — re-emitting the last response
/// once drained so a loop terminates whatever its iteration count.
#[cfg(any(test, debug_assertions))]
pub struct TestNativeResponseQueue {
    responses: Mutex<VecDeque<ExecutionNativeResponse>>,
    last_response: Mutex<Option<ExecutionNativeResponse>>,
    /// What the run actually asked the model, in order. A scripted model is
    /// otherwise write-only, and some runtime behaviour is only observable in
    /// the prompt — a refusal reaches the model as its next tool result and is
    /// persisted nowhere else, so a test that wants to prove the run was *told*
    /// has to read what it was told.
    prompts: Mutex<Vec<String>>,
}

#[cfg(any(test, debug_assertions))]
impl TestNativeResponseQueue {
    /// A scripted response is still one model call: give it the stable call
    /// identity a routed call carries, so a tool call it makes composes an
    /// effect id and the stateless driver can dispatch outward work
    /// (`gate_pending_effect` refuses an unkeyable outward dispatch). A script
    /// that stamped its own telemetry keeps it.
    fn stamp_call_identity(response: &mut ExecutionNativeResponse) {
        if response
            .telemetry
            .as_ref()
            .is_some_and(|telemetry| telemetry.trace_receipt.is_some())
        {
            return;
        }
        let context = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("test", "test"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        );
        let mut telemetry = response.telemetry.take().unwrap_or_default();
        telemetry.provider = "scripted".to_string();
        telemetry.model = "scripted".to_string();
        telemetry.operation = Some("agentic_decision".to_string());
        telemetry.trace_receipt = Some(magicllm::LlmTraceReceipt::direct(context));
        response.telemetry = Some(telemetry);
    }

    pub fn new(responses: Vec<ExecutionNativeResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            last_response: Mutex::new(None),
            prompts: Mutex::new(Vec::new()),
        }
    }

    /// Every prompt the run has asked this queue for a decision on.
    pub fn prompts(&self) -> Vec<String> {
        self.prompts
            .lock()
            .expect("test native response queue poisoned")
            .clone()
    }

    fn record_prompt(&self, prompt: String) {
        self.prompts
            .lock()
            .expect("test native response queue poisoned")
            .push(prompt);
    }

    fn next_response(&self) -> ExecutionNativeResponse {
        let mut responses = self
            .responses
            .lock()
            .expect("test native response queue poisoned");
        if let Some(mut response) = responses.pop_front() {
            response.admit_tool_arguments();
            Self::stamp_call_identity(&mut response);
            *self
                .last_response
                .lock()
                .expect("test native response queue poisoned") = Some(response.clone());
            return response;
        }
        let mut response = self
            .last_response
            .lock()
            .expect("test native response queue poisoned")
            .clone()
            .unwrap_or_else(|| {
                let mut response = ExecutionNativeResponse::unadmitted(Vec::new());
                response.text = Some("test native response queue is empty".to_string());
                response.finish_reason = Some("test_empty_queue".to_string());
                // Test sentinel — no provider served this response.
                response
            });
        // A re-emitted response is a new call: a repeated identity would make
        // a repeated tool call read as a replay of the first.
        if let Some(telemetry) = response.telemetry.as_mut() {
            telemetry.trace_receipt = None;
        }
        Self::stamp_call_identity(&mut response);
        response
    }
}

/// Multimodal LLM adapter for execution agents with proper operation routing
pub struct MultiLlmAgentAdapter {
    service: Arc<OperationLlmRouter>,
    protected_app: bool,
    #[cfg(any(test, debug_assertions))]
    test_native_responses: Option<Arc<TestNativeResponseQueue>>,
}

/// The provider returned a real response, but the execution-native contract
/// rejected its decision shape. Keeping the exact response telemetry in the
/// error chain lets observability distinguish contract quality from transport
/// reliability without storing response content.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct NativeDecisionResponseError {
    message: String,
    validation_error_class: String,
    telemetry: Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry>,
}

fn text_terminal_envelope(text: String) -> ExecutionDecisionEnvelope {
    ExecutionDecisionEnvelope {
        decision: crate::magician_v2::execution::agentic::Decision::Completed {
            evidence: Some(text.clone()),
            artifacts: Vec::new(),
        },
        request_hover_discovery: None,
        request_vision: None,
        vision_reason: None,
        step_completed: None,
        step_failed: None,
        needs_plan_revision: false,
        task_state_action: TaskStateActionEnvelope::default(),
        deferred_tool_calls: Vec::new(),
        thinking: None,
        raw_tool_call: None,
        raw_text_fallback: Some(text),
    }
}

impl MultiLlmAgentAdapter {
    fn invalid_native_response(
        response: &ExecutionNativeResponse,
        validation_error_class: &'static str,
        message: String,
    ) -> anyhow::Error {
        anyhow::Error::new(NativeDecisionResponseError {
            message,
            validation_error_class: validation_error_class.to_string(),
            telemetry: response.telemetry.clone(),
        })
    }

    /// Recover a response-contract failure after arbitrary `anyhow::Context`
    /// layers. `None` means the failure happened before a provider response or
    /// outside execution-native lowering.
    pub fn validation_failure_from_error(
        error: &anyhow::Error,
    ) -> Option<(
        String,
        Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry>,
    )> {
        error.chain().find_map(|source| {
            source
                .downcast_ref::<NativeDecisionResponseError>()
                .map(|failure| {
                    (
                        failure.validation_error_class.clone(),
                        failure.telemetry.clone(),
                    )
                })
        })
    }

    pub fn new(service: Arc<OperationLlmRouter>) -> Self {
        Self {
            service,
            protected_app: false,
            #[cfg(any(test, debug_assertions))]
            test_native_responses: None,
        }
    }

    /// Clone the adapter with execution/scope lineage installed on every
    /// operation-router request. The scripted debug/test lane is retained.
    pub fn with_task_context(&self, task_ref: magicllm::dispatch::TaskRef) -> Self {
        Self {
            service: Arc::new(self.service.with_task_context(Some(task_ref))),
            protected_app: self.protected_app,
            #[cfg(any(test, debug_assertions))]
            test_native_responses: self.test_native_responses.clone(),
        }
    }

    /// Clone the adapter with the current effective owner/execution routing.
    /// The scripted debug/test response queue is intentionally shared so
    /// provider-boundary routing refreshes do not consume or replace fixtures.
    pub fn with_routing_overrides(&self, overrides: Option<OperationRoutingOverrides>) -> Self {
        Self {
            service: Arc::new(self.service.with_routing_overrides(overrides)),
            protected_app: self.protected_app,
            #[cfg(any(test, debug_assertions))]
            test_native_responses: self.test_native_responses.clone(),
        }
    }

    /// Clone the adapter with one runtime-only app disclosure fence. The
    /// underlying router revalidates the fence again at physical dispatch;
    /// scripted debug/test responses remain shared and provider-free.
    pub fn with_disclosure_guard(&self, guard: Option<magicllm::LlmDisclosureGuard>) -> Self {
        let protected_app = guard.is_some();
        Self {
            service: Arc::new(self.service.with_disclosure_guard(guard)),
            protected_app,
            #[cfg(any(test, debug_assertions))]
            test_native_responses: self.test_native_responses.clone(),
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn new_with_test_native_responses(responses: Vec<ExecutionNativeResponse>) -> Self {
        Self::new_with_test_native_queue(Arc::new(TestNativeResponseQueue::new(responses)))
    }

    /// The same, over a queue the caller keeps: several executor sets (a
    /// run's start and each of its resumes) then consume one script.
    #[cfg(any(test, debug_assertions))]
    pub fn new_with_test_native_queue(queue: Arc<TestNativeResponseQueue>) -> Self {
        Self {
            service: Arc::new(OperationLlmRouter::new(None)),
            protected_app: false,
            test_native_responses: Some(queue),
        }
    }

    /// Determine which LLM operation to use based on the request
    fn select_operation(request: &LlmFunctionCallRequest) -> LLMOperation {
        let schema = request.function_schema.to_lowercase();
        let user_prompt = request.user_prompt.to_lowercase();
        let system_prompt = request.system_prompt.to_lowercase();

        // Placeholder resolution requests (runtime parameter resolution)
        if system_prompt.contains("parameter resolution")
            || user_prompt.contains("parameters to resolve")
        {
            debug!("Selecting placeholder_resolution operation for parameter resolution request");
            return LLMOperation::PlaceholderResolution;
        }

        // Agentic decision requests (observe-decide-execute loop)
        // Matches schema with decision enum and action_type
        if schema.contains("\"decision\"") && schema.contains("\"action_type\"") {
            if system_prompt.contains("[retry with extended thinking]") {
                debug!("Selecting agentic_decision_retry operation for agentic retry");
                return LLMOperation::Other("agentic_decision_retry".to_string());
            }
            debug!("Selecting agentic_decision operation for agentic loop decision");
            return LLMOperation::Other("agentic_decision".to_string());
        }

        // NOTE: Overlay detection routing removed - overlay detection is now integrated
        // into the agentic decision loop. The LLM detects overlays directly from the
        // screenshot, with DOM-detected overlays shown as hints in the prompt.

        // Page understanding vision requests (screenshot analysis)
        // Matches schema with page_stage and elements for visual page analysis
        if schema.contains("page_stage") && schema.contains("elements") {
            debug!("Selecting page_understanding_vision operation for page analysis request");
            return LLMOperation::Other("page_understanding_vision".to_string());
        }

        // Generic fallback - use a general agent operation
        debug!("Using generic agent operation for request");
        LLMOperation::Other("agent_general".to_string())
    }

    fn request_model_override(request: &LlmFunctionCallRequest) -> Option<&str> {
        let model = request.model.trim();
        (!model.is_empty()).then_some(model)
    }

    fn strip_json_response_hint(prompt: &str) -> &str {
        const HINTS: &[&str] = &[
            "Respond with ONLY valid JSON matching the expected schema.",
            "Respond with ONLY valid JSON.",
        ];
        let trimmed = prompt.trim_end();
        for hint in HINTS {
            if let Some(stripped) = trimmed.strip_suffix(hint) {
                return stripped.trim_end();
            }
        }
        trimmed
    }

    fn operation_uses_native_tool_schema(
        &self,
        _operation: &LLMOperation,
        request: &LlmFunctionCallRequest,
    ) -> bool {
        !request.function_schema.trim().is_empty()
    }

    /// Compose prompt for the LLM based on the request
    fn compose_prompt(request: &LlmFunctionCallRequest) -> String {
        format!(
            "{system}\n\nTask: Generate a JSON response matching this schema.\n\n\
             Schema:\n{schema}\n\n\
             Instructions:\n{user}\n\n\
             Respond with ONLY valid JSON matching the schema. No markdown, no code blocks, no prose.",
            system = request.system_prompt.trim(),
            schema = request.function_schema.trim(),
            user = request.user_prompt.trim()
        )
    }

    /// Execute an agentic decision using the native tool-call path.
    ///
    /// Returns `Ok(envelope)` on success or `Err` on failure. There is no
    /// legacy fallback — native tool calling is the only execution path.
    pub async fn call_execution_native(
        &self,
        operation_name: &str,
        system_prompt: &str,
        user_prompt: &str,
        tools: Vec<NativeExecutionTool>,
        images: Option<Vec<crate::magician_v2::slot_graph::extraction::ImageData>>,
        tool_choice_override: Option<Value>,
        allow_text_terminal: bool,
        viewport: Option<(u32, u32)>,
    ) -> Result<(ExecutionDecisionEnvelope, ExecutionNativeResponse)> {
        // Tools are built by the caller (the decision seam) — flat mode passes
        // the hot tier, inner-loop mode passes the full native catalog.
        if tools.is_empty() {
            error!(
                operation = operation_name,
                "Execution-native catalog is empty; cannot proceed"
            );
            return Err(anyhow::anyhow!(
                "Execution-native catalog is empty for operation '{}'",
                operation_name
            ));
        }

        // Build native request
        let request = ExecutionNativeRequest {
            operation: operation_name.to_string(),
            system_prompt: system_prompt.to_string(),
            user_prompt: user_prompt.to_string(),
            images,
            tools,
            model_override: None,
            tool_choice_override,
        };

        #[cfg(any(test, debug_assertions))]
        let mut response = if let Some(queue) = self.test_native_responses.as_ref() {
            queue.record_prompt(format!(
                "{}\n{}",
                request.system_prompt, request.user_prompt
            ));
            queue.next_response()
        } else {
            execute_native_request(&self.service, &request, viewport).await?
        };
        #[cfg(not(any(test, debug_assertions)))]
        let mut response = execute_native_request(&self.service, &request, viewport).await?;

        // Lower the response
        response.admit_tool_arguments();
        match lower_native_response(&response) {
            NativeDecisionOutcome::Valid(envelope) => Ok((envelope, response)),
            NativeDecisionOutcome::ZeroToolCalls { text_fallback, .. } => {
                if allow_text_terminal {
                    if let Some(text) = text_fallback
                        .clone()
                        .map(|text| text.trim().to_string())
                        .filter(|text| !text.is_empty())
                    {
                        let envelope = text_terminal_envelope(text);
                        return Ok((envelope, response));
                    }
                }
                if self.protected_app {
                    error!(
                        operation = operation_name,
                        validation_error_class = "zero_tool_calls",
                        "Protected app execution-native response was invalid"
                    );
                } else {
                    error!(
                        operation = operation_name,
                        text_fallback = ?text_fallback
                            .as_deref()
                            .map(|text| text.chars().take(200).collect::<String>()),
                        "Execution-native model returned zero tool calls"
                    );
                }
                Err(Self::invalid_native_response(
                    &response,
                    "zero_tool_calls",
                    if self.protected_app {
                        "Protected app workflow returned an invalid execution-native response"
                            .to_owned()
                    } else {
                        format!(
                            "Execution-native response contained zero tool calls for '{}'",
                            operation_name
                        )
                    },
                ))
            },
            NativeDecisionOutcome::MultipleToolCalls { count, tool_names } => {
                if self.protected_app {
                    error!(
                        operation = operation_name,
                        count,
                        validation_error_class = "multiple_tool_calls",
                        "Protected app execution-native response was invalid"
                    );
                } else {
                    error!(operation = operation_name, count, tool_names = ?tool_names, "Execution-native returned unexpected multiple tool calls");
                }
                Err(Self::invalid_native_response(
                    &response,
                    "multiple_tool_calls",
                    if self.protected_app {
                        "Protected app workflow returned an invalid execution-native response"
                            .to_owned()
                    } else {
                        format!(
                            "Execution-native response contained {} tool calls: {:?}",
                            count, tool_names
                        )
                    },
                ))
            },
            NativeDecisionOutcome::UnknownTool { tool_name, .. } => {
                Err(Self::invalid_native_response(
                    &response,
                    "unknown_tool",
                    if self.protected_app {
                        "Protected app workflow returned an invalid execution-native response"
                            .to_owned()
                    } else {
                        format!("Execution-native: unknown tool '{}'", tool_name)
                    },
                ))
            },
            NativeDecisionOutcome::InvalidArguments {
                tool_name, error, ..
            } => Err(Self::invalid_native_response(
                &response,
                "invalid_tool_arguments",
                if self.protected_app {
                    "Protected app workflow returned an invalid execution-native response"
                        .to_owned()
                } else {
                    format!(
                        "Execution-native: invalid arguments for '{}': {}",
                        tool_name, error
                    )
                },
            )),
        }
    }

    /// Multi-message variant of `call_execution_native`. Sends a pre-built
    /// message list (system + last-N raw turns as `Assistant`/`User` pairs +
    /// final user prompt) so the outer-loop LLM sees its own decisions and
    /// their outcomes verbatim across iterations instead of relying solely on
    /// the summarized history baked into the user prompt.
    pub async fn call_execution_native_with_messages(
        &self,
        operation_name: &str,
        messages: Vec<RouterMessage>,
        tools: Vec<NativeExecutionTool>,
        tool_choice_override: Option<Value>,
        allow_text_terminal: bool,
        previous_response_id: Option<&str>,
        viewport: Option<(u32, u32)>,
    ) -> Result<(ExecutionDecisionEnvelope, ExecutionNativeResponse)> {
        // Tools are built by the caller (the decision seam) — flat mode passes
        // the hot tier, inner-loop mode passes the full native catalog.
        if tools.is_empty() {
            error!(
                operation = operation_name,
                "Execution-native catalog is empty; cannot proceed"
            );
            return Err(anyhow::anyhow!(
                "Execution-native catalog is empty for operation '{}'",
                operation_name
            ));
        }

        #[cfg(any(test, debug_assertions))]
        let mut response = if let Some(queue) = self.test_native_responses.as_ref() {
            queue.record_prompt(
                messages
                    .iter()
                    .map(|message| format!("{:?}", message))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            queue.next_response()
        } else {
            execute_native_messages_request_with_chain(
                &self.service,
                operation_name,
                messages,
                &tools,
                None,
                tool_choice_override,
                previous_response_id,
                viewport,
            )
            .await?
        };
        #[cfg(not(any(test, debug_assertions)))]
        let mut response = execute_native_messages_request_with_chain(
            &self.service,
            operation_name,
            messages,
            &tools,
            None,
            tool_choice_override,
            previous_response_id,
            viewport,
        )
        .await?;

        response.admit_tool_arguments();
        match lower_native_response(&response) {
            NativeDecisionOutcome::Valid(envelope) => Ok((envelope, response)),
            NativeDecisionOutcome::ZeroToolCalls { text_fallback, .. } => {
                if allow_text_terminal {
                    if let Some(text) = text_fallback
                        .clone()
                        .map(|text| text.trim().to_string())
                        .filter(|text| !text.is_empty())
                    {
                        let envelope = text_terminal_envelope(text);
                        return Ok((envelope, response));
                    }
                }
                if self.protected_app {
                    error!(
                        operation = operation_name,
                        validation_error_class = "zero_tool_calls",
                        "Protected app execution-native response was invalid"
                    );
                } else {
                    error!(
                        operation = operation_name,
                        text_fallback = ?text_fallback
                            .as_deref()
                            .map(|text| text.chars().take(200).collect::<String>()),
                        "Execution-native (messages) model returned zero tool calls"
                    );
                }
                Err(Self::invalid_native_response(
                    &response,
                    "zero_tool_calls",
                    if self.protected_app {
                        "Protected app workflow returned an invalid execution-native response"
                            .to_owned()
                    } else {
                        format!(
                        "Execution-native (messages) response contained zero tool calls for '{}'",
                        operation_name
                    )
                    },
                ))
            },
            NativeDecisionOutcome::MultipleToolCalls { count, tool_names } => {
                if self.protected_app {
                    error!(
                        operation = operation_name,
                        count,
                        validation_error_class = "multiple_tool_calls",
                        "Protected app execution-native response was invalid"
                    );
                } else {
                    error!(operation = operation_name, count, tool_names = ?tool_names, "Execution-native (messages) returned unexpected multiple tool calls");
                }
                Err(Self::invalid_native_response(
                    &response,
                    "multiple_tool_calls",
                    if self.protected_app {
                        "Protected app workflow returned an invalid execution-native response"
                            .to_owned()
                    } else {
                        format!(
                            "Execution-native (messages) response contained {} tool calls: {:?}",
                            count, tool_names
                        )
                    },
                ))
            },
            NativeDecisionOutcome::UnknownTool { tool_name, .. } => {
                Err(Self::invalid_native_response(
                    &response,
                    "unknown_tool",
                    if self.protected_app {
                        "Protected app workflow returned an invalid execution-native response"
                            .to_owned()
                    } else {
                        format!("Execution-native (messages): unknown tool '{}'", tool_name)
                    },
                ))
            },
            NativeDecisionOutcome::InvalidArguments {
                tool_name, error, ..
            } => Err(Self::invalid_native_response(
                &response,
                "invalid_tool_arguments",
                if self.protected_app {
                    "Protected app workflow returned an invalid execution-native response"
                        .to_owned()
                } else {
                    format!(
                        "Execution-native (messages): invalid arguments for '{}': {}",
                        tool_name, error
                    )
                },
            )),
        }
    }

    /// Get a reference to the underlying router.
    pub fn router(&self) -> &Arc<OperationLlmRouter> {
        &self.service
    }

    /// Compose prompt for multimodal requests (text + images)
    #[allow(dead_code)]
    fn compose_multimodal_prompt(request: &LlmFunctionCallRequest) -> String {
        format!(
            "{system}\n\nTask: Analyze the provided image(s) and generate a JSON response matching this schema.\n\n\
             Schema:\n{schema}\n\n\
             Instructions:\n{user}\n\n\
             Respond with ONLY valid JSON matching the schema. No markdown, no code blocks, no prose.",
            system = request.system_prompt.trim(),
            schema = request.function_schema.trim(),
            user = request.user_prompt.trim()
        )
    }

    /// Generate with multimodal (text + images) input
    async fn generate_with_images(
        &self,
        operation: &LLMOperation,
        request: &LlmFunctionCallRequest,
        images: &[crate::magician_v2::slot_graph::extraction::ImageData],
    ) -> Result<LlmFunctionCallResponse> {
        let model_override = Self::request_model_override(request);
        debug!(
            "Generating multimodal response with {} image(s) for operation: {:?}",
            images.len(),
            operation
        );

        // Try primary operation
        match self
            .service
            .generate_multimodal_with_model(operation, request, images, model_override)
            .await
        {
            Ok(response) => {
                debug!("Multimodal agent LLM request successful");
                Ok(LlmFunctionCallResponse {
                    raw_arguments: response.content,
                    telemetry: response.telemetry.clone(),
                })
            },
            Err(e) => {
                // Fallback chain (same as text-only)
                warn!(
                    "Primary multimodal operation {:?} failed: {}. Trying fallback...",
                    operation, e
                );

                let fallback_ops = vec![
                    LLMOperation::AtomicComposition,
                    LLMOperation::QueryAnalysis,
                    LLMOperation::SlotExtraction,
                ];

                for fallback_op in fallback_ops {
                    debug!("Trying fallback operation: {:?}", fallback_op);
                    if let Ok(response) = self
                        .service
                        .generate_multimodal_with_model(
                            &fallback_op,
                            request,
                            images,
                            model_override,
                        )
                        .await
                    {
                        debug!("Fallback operation {:?} succeeded", fallback_op);
                        return Ok(LlmFunctionCallResponse {
                            raw_arguments: response.content,
                            telemetry: response.telemetry.clone(),
                        });
                    }
                }

                Err(e).with_context(|| {
                    format!(
                        "Multimodal agent LLM request failed for operation {:?} and all fallbacks",
                        operation
                    )
                })
            },
        }
    }

    /// Generate with text-only input
    async fn generate_text_only(
        &self,
        operation: &LLMOperation,
        request: &LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse> {
        let model_override = Self::request_model_override(request);

        if self.operation_uses_native_tool_schema(operation, request) {
            match self
                .service
                .generate_for_operation_with_system_and_tool_schema(
                    operation,
                    Some(Self::strip_json_response_hint(&request.system_prompt)),
                    Self::strip_json_response_hint(&request.user_prompt),
                    Some(&request.function_schema),
                    model_override,
                )
                .await
            {
                Ok(response) => {
                    debug!("Text-only native tool-schema request successful");
                    return Ok(LlmFunctionCallResponse {
                        raw_arguments: response.content,
                        telemetry: response.telemetry.clone(),
                    });
                },
                Err(e) => {
                    error!(
                        operation = ?operation,
                        error = %e,
                        fallback = "prompt_embedded_schema_path",
                        "Native tool-schema request failed; falling back to prompt-embedded schema path"
                    );
                },
            }
        }

        // For text-completion requests (empty schema), send system+user directly
        // without the JSON wrapper that compose_prompt adds
        let prompt = if request.function_schema.trim().is_empty() {
            format!(
                "{}\n\n{}",
                request.system_prompt.trim(),
                request.user_prompt.trim()
            )
        } else {
            Self::compose_prompt(request)
        };

        // Try to generate with the selected operation
        match self
            .service
            .generate_for_operation_with_model(operation, &prompt, model_override)
            .await
        {
            Ok(response) => {
                debug!("Text-only agent LLM request successful");
                Ok(LlmFunctionCallResponse {
                    raw_arguments: response.content,
                    telemetry: response.telemetry.clone(),
                })
            },
            Err(e) => {
                // If the specific operation fails (e.g., not configured), try fallback operations
                warn!(
                    "Primary operation {:?} failed: {}. Trying fallback...",
                    operation, e
                );

                // Try with a more generic operation as fallback
                let fallback_ops = vec![
                    LLMOperation::AtomicComposition,
                    LLMOperation::QueryAnalysis,
                    LLMOperation::SlotExtraction,
                ];

                for fallback_op in fallback_ops {
                    debug!("Trying fallback operation: {:?}", fallback_op);
                    match self
                        .service
                        .generate_for_operation_with_model(&fallback_op, &prompt, model_override)
                        .await
                    {
                        Ok(response) => {
                            debug!("Fallback operation {:?} succeeded", fallback_op);
                            return Ok(LlmFunctionCallResponse {
                                raw_arguments: response.content,
                                telemetry: response.telemetry.clone(),
                            });
                        },
                        Err(fallback_err) => {
                            debug!(
                                "Fallback operation {:?} failed: {}",
                                fallback_op, fallback_err
                            );
                            continue;
                        },
                    }
                }

                // All operations failed
                Err(e).with_context(|| {
                    format!(
                        "Agent LLM request failed for operation {:?} and all fallbacks",
                        operation
                    )
                })
            },
        }
    }
}

#[async_trait]
impl LlmService for MultiLlmAgentAdapter {
    async fn call_function(
        &self,
        request: LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse> {
        let operation = Self::select_operation(&request);

        debug!(
            "Executing agent LLM request with operation: {:?}, multimodal: {}",
            operation,
            request.images.is_some()
        );

        // Determine if this is a multimodal request
        if let Some(images) = &request.images {
            // Use multimodal path
            return self
                .generate_with_images(&operation, &request, images)
                .await;
        }

        // Text-only path
        self.generate_text_only(&operation, &request).await
    }

    fn provider_name(&self) -> String {
        // Try to get provider from a common operation
        self.service
            .get_config_for_operation(&LLMOperation::AtomicComposition)
            .or_else(|_| {
                self.service
                    .get_config_for_operation(&LLMOperation::QueryAnalysis)
            })
            .or_else(|_| {
                self.service
                    .get_config_for_operation(&LLMOperation::SlotExtraction)
            })
            .map(|config| config.provider.to_string())
            .unwrap_or_else(|_| "multi-llm".to_string())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use magicllm::prelude::{LLMProfile, LLMProviderKind, LLMRouterConfig};

    #[test]
    fn native_validation_error_keeps_exact_telemetry_through_context_layers() {
        let context = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::AutonomousTask,
        );
        let receipt = magicllm::LlmTraceReceipt::queued(context, "job-1", 2);
        let telemetry = crate::magician_v2::slot_graph::extraction::LlmCallTelemetry {
            provider: "openai".to_string(),
            model: "gpt-5.6-terra".to_string(),
            started_at_ms: 123,
            trace_receipt: Some(receipt.clone()),
            ..Default::default()
        };
        let mut response = ExecutionNativeResponse::unadmitted(Vec::new());
        response.text = Some("not a tool call".to_string());
        response.finish_reason = Some("stop".to_string());
        response.prompt_tokens = Some(10);
        response.completion_tokens = Some(2);
        response.provider = Some("openai".to_string());
        response.model = Some("gpt-5.6-terra".to_string());
        response.profile = Some("fast".to_string());
        response.telemetry = Some(telemetry);
        let error = MultiLlmAgentAdapter::invalid_native_response(
            &response,
            "zero_tool_calls",
            "invalid decision".to_string(),
        );
        let wrapped = Err::<(), _>(error)
            .context("decision failed")
            .expect_err("wrapped validation error");

        let (class, recovered) = MultiLlmAgentAdapter::validation_failure_from_error(&wrapped)
            .expect("typed response validation failure");
        assert_eq!(class, "zero_tool_calls");
        let recovered = recovered.expect("exact response telemetry");
        assert_eq!(recovered.started_at_ms, 123);
        assert_eq!(recovered.trace_receipt, Some(receipt));
    }

    fn adapter_for_tests() -> MultiLlmAgentAdapter {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "agent".to_string();
        config.profiles.insert(
            "agent".to_string(),
            LLMProfile {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                api_key_env: Some("__TEST_OPENAI_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: Some(true),
                supports_computer_use: None,
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );

        let router = Arc::new(OperationLlmRouter::new(Some(config)));

        MultiLlmAgentAdapter::new(router)
    }

    #[test]
    fn call_time_routing_clone_preserves_scripted_response_queue() {
        let adapter = MultiLlmAgentAdapter::new_with_test_native_responses(Vec::new());
        let original_queue = adapter
            .test_native_responses
            .as_ref()
            .expect("scripted queue");
        let routed = adapter.with_routing_overrides(Some(OperationRoutingOverrides {
            operations: std::collections::BTreeMap::from([(
                "agentic_decision".to_string(),
                crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingEndpoint::new(
                    "openai",
                    "gpt-6-luna",
                )
                .expect("route endpoint"),
            )]),
            ..Default::default()
        }));

        assert!(Arc::ptr_eq(
            original_queue,
            routed
                .test_native_responses
                .as_ref()
                .expect("routed scripted queue")
        ));
        assert_eq!(
            routed
                .service
                .provider_for_operation(&LLMOperation::Other("agentic_decision".to_string()))
                .as_deref(),
            Some("openai")
        );
    }

    #[test]
    fn test_operation_selection_vision() {
        let request = LlmFunctionCallRequest {
            system_prompt: "Analyze page".to_string(),
            user_prompt: "Analyze this screenshot".to_string(),
            function_schema: r#"{"page_stage": string, "elements": array}"#.to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: None,
        };

        let operation = MultiLlmAgentAdapter::select_operation(&request);
        assert!(
            matches!(operation, LLMOperation::Other(ref s) if s == "page_understanding_vision")
        );
    }

    #[test]
    fn test_operation_selection_agentic_decision_without_image() {
        let request = LlmFunctionCallRequest {
            system_prompt: "You are an agentic executor".to_string(),
            user_prompt: "Decide next action".to_string(),
            function_schema: r#"{"decision": "execute", "action_type": "browser"}"#.to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: None,
        };

        let operation = MultiLlmAgentAdapter::select_operation(&request);
        assert!(matches!(operation, LLMOperation::Other(ref s) if s == "agentic_decision"));
    }

    #[test]
    fn test_operation_selection_agentic_decision_vision() {
        use crate::magician_v2::slot_graph::extraction::ImageData;

        // Vision mode: images attached
        let image = ImageData::new(
            "fake_base64_image_data".to_string(),
            "image/png".to_string(),
        );
        let request = LlmFunctionCallRequest {
            system_prompt: "You are an agentic executor".to_string(),
            user_prompt: "Decide next action".to_string(),
            function_schema: r#"{"decision": "execute", "action_type": "browser"}"#.to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: Some(vec![image]),
        };

        let operation = MultiLlmAgentAdapter::select_operation(&request);
        assert!(matches!(operation, LLMOperation::Other(ref s) if s == "agentic_decision"));
    }

    #[test]
    fn operation_does_not_use_native_tool_schema_when_schema_empty() {
        let adapter = adapter_for_tests();
        let request = LlmFunctionCallRequest {
            system_prompt: "Plan the interaction".to_string(),
            user_prompt: "Pick the best channel".to_string(),
            function_schema: String::new(),
            model: String::new(),
            temperature: 0.0,
            images: None,
        };

        assert!(!adapter.operation_uses_native_tool_schema(
            &LLMOperation::Other("agent_general".to_string()),
            &request
        ));
    }

    #[test]
    fn operation_uses_native_tool_schema_when_schema_present() {
        let adapter = adapter_for_tests();
        let request = LlmFunctionCallRequest {
            system_prompt: "Choose a tool".to_string(),
            user_prompt: "Return tool arguments".to_string(),
            function_schema: r#"{
              "name": "browser",
              "parameters": { "type": "object", "properties": {} }
            }"#
            .to_string(),
            model: String::new(),
            temperature: 0.0,
            images: Some(vec![]),
        };

        assert!(adapter.operation_uses_native_tool_schema(
            &LLMOperation::Other("agent_general".to_string()),
            &request
        ));
    }

    #[test]
    fn llm_routing_uses_agent_model_override_when_present() {
        let request = LlmFunctionCallRequest {
            system_prompt: "system".to_string(),
            user_prompt: "user".to_string(),
            function_schema: "{}".to_string(),
            model: "gpt-5.6-terra".to_string(),
            temperature: 0.1,
            images: None,
        };
        assert_eq!(
            MultiLlmAgentAdapter::request_model_override(&request),
            Some("gpt-5.6-terra")
        );
    }

    #[test]
    fn llm_routing_fallback_uses_global_model_when_agent_override_absent() {
        let request = LlmFunctionCallRequest {
            system_prompt: "system".to_string(),
            user_prompt: "user".to_string(),
            function_schema: "{}".to_string(),
            model: "   ".to_string(),
            temperature: 0.1,
            images: None,
        };
        assert_eq!(MultiLlmAgentAdapter::request_model_override(&request), None);
    }

    #[test]
    fn test_prompt_composition() {
        let request = LlmFunctionCallRequest {
            system_prompt: "You are a validation agent".to_string(),
            user_prompt: "Validate this action".to_string(),
            function_schema: r#"{"result": boolean}"#.to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: None,
        };

        let prompt = MultiLlmAgentAdapter::compose_prompt(&request);
        assert!(prompt.contains("You are a validation agent"));
        assert!(prompt.contains("Validate this action"));
        assert!(prompt.contains(r#"{"result": boolean}"#));
        assert!(prompt.contains("valid JSON"));
    }

    #[test]
    fn test_multimodal_prompt_composition() {
        let request = LlmFunctionCallRequest {
            system_prompt: "You are a vision analysis agent".to_string(),
            user_prompt: "Analyze this screenshot".to_string(),
            function_schema: r#"{"page_stage": string, "elements": array}"#.to_string(),
            model: "gpt-5".to_string(),
            temperature: 0.1,
            images: Some(vec![]),
        };

        let prompt = MultiLlmAgentAdapter::compose_multimodal_prompt(&request);
        assert!(prompt.contains("You are a vision analysis agent"));
        assert!(prompt.contains("Analyze this screenshot"));
        assert!(prompt.contains(r#"{"page_stage": string, "elements": array}"#));
        assert!(prompt.contains("Analyze the provided image(s)"));
        assert!(prompt.contains("valid JSON"));
    }

    #[test]
    fn test_image_data_creation() {
        use crate::magician_v2::slot_graph::extraction::{ImageData, ImageDetail};

        let image = ImageData::new("base64data".to_string(), "image/png".to_string());
        assert_eq!(image.base64, "base64data");
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.detail, Some(ImageDetail::Auto));

        let image_high = ImageData::with_detail(
            "base64data".to_string(),
            "image/jpeg".to_string(),
            ImageDetail::High,
        );
        assert_eq!(image_high.base64, "base64data");
        assert_eq!(image_high.media_type, "image/jpeg");
        assert_eq!(image_high.detail, Some(ImageDetail::High));
    }

    #[test]
    fn test_image_detail_enum() {
        use crate::magician_v2::slot_graph::extraction::ImageDetail;

        assert_eq!(ImageDetail::default(), ImageDetail::Auto);
        assert_eq!(ImageDetail::Auto, ImageDetail::Auto);
        assert_ne!(ImageDetail::Auto, ImageDetail::High);
        assert_ne!(ImageDetail::Low, ImageDetail::High);
    }

    #[test]
    fn test_multimodal_request_detection() {
        use crate::magician_v2::slot_graph::extraction::ImageData;

        // Text-only request
        let text_request = LlmFunctionCallRequest {
            system_prompt: "System".to_string(),
            user_prompt: "User".to_string(),
            function_schema: "{}".to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: None,
        };
        assert!(text_request.images.is_none());

        // Multimodal request with one image
        let image = ImageData::new("base64".to_string(), "image/png".to_string());
        let multimodal_request = LlmFunctionCallRequest {
            system_prompt: "System".to_string(),
            user_prompt: "User".to_string(),
            function_schema: "{}".to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: Some(vec![image]),
        };
        assert!(multimodal_request.images.is_some());
        assert_eq!(multimodal_request.images.as_ref().unwrap().len(), 1);

        // Multimodal request with multiple images
        let image1 = ImageData::new("base64_1".to_string(), "image/png".to_string());
        let image2 = ImageData::new("base64_2".to_string(), "image/jpeg".to_string());
        let multi_image_request = LlmFunctionCallRequest {
            system_prompt: "System".to_string(),
            user_prompt: "User".to_string(),
            function_schema: "{}".to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: Some(vec![image1, image2]),
        };
        assert_eq!(multi_image_request.images.as_ref().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_multimodal_routing() {
        use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
        use crate::magician_v2::slot_graph::extraction::ImageData;

        // Create a minimal disabled router (won't actually call LLM)
        let service = Arc::new(OperationLlmRouter::new(None));
        let _adapter = MultiLlmAgentAdapter::new(service);

        // Create a multimodal request with proper schema for vision analysis
        let image = ImageData::new("iVBORw0KGgo=".to_string(), "image/png".to_string());
        let request = LlmFunctionCallRequest {
            system_prompt: "Analyze page".to_string(),
            user_prompt: "What do you see?".to_string(),
            function_schema: r#"{"page_stage": string, "elements": array}"#.to_string(),
            model: "gpt-5".to_string(),
            temperature: 0.1,
            images: Some(vec![image]),
        };

        // Verify operation selection works for page understanding vision
        let operation = MultiLlmAgentAdapter::select_operation(&request);
        assert!(
            matches!(operation, LLMOperation::Other(ref s) if s == "page_understanding_vision")
        );

        // Note: We can't test actual LLM call without API keys,
        // but we've verified the routing logic and data structures
    }

    #[test]
    fn test_base64_image_formats() {
        use crate::magician_v2::slot_graph::extraction::{ImageData, ImageDetail};

        // Test PNG format
        let png_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let png_image = ImageData::with_detail(
            png_data.to_string(),
            "image/png".to_string(),
            ImageDetail::High,
        );
        assert_eq!(png_image.media_type, "image/png");
        assert!(!png_image.base64.is_empty());

        // Test JPEG format
        let jpeg_image = ImageData::new("/9j/4AAQSkZJRg==".to_string(), "image/jpeg".to_string());
        assert_eq!(jpeg_image.media_type, "image/jpeg");

        // Test WebP format
        let webp_image = ImageData::new("UklGRg==".to_string(), "image/webp".to_string());
        assert_eq!(webp_image.media_type, "image/webp");
    }

    #[test]
    fn test_image_detail_levels() {
        use crate::magician_v2::slot_graph::extraction::{ImageData, ImageDetail};

        let auto_image = ImageData::new("base64".to_string(), "image/png".to_string());
        assert_eq!(auto_image.detail, Some(ImageDetail::Auto));

        let low_image = ImageData::with_detail(
            "base64".to_string(),
            "image/png".to_string(),
            ImageDetail::Low,
        );
        assert_eq!(low_image.detail, Some(ImageDetail::Low));

        let high_image = ImageData::with_detail(
            "base64".to_string(),
            "image/png".to_string(),
            ImageDetail::High,
        );
        assert_eq!(high_image.detail, Some(ImageDetail::High));
    }

    #[test]
    fn test_empty_images_array() {
        // Test that empty images array is treated as multimodal
        let request = LlmFunctionCallRequest {
            system_prompt: "System".to_string(),
            user_prompt: "User".to_string(),
            function_schema: "{}".to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: Some(vec![]),
        };

        assert!(request.images.is_some());
        assert_eq!(request.images.as_ref().unwrap().len(), 0);
    }
}
