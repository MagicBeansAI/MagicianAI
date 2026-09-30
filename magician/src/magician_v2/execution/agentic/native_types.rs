//! Native tool-call types for the execution-native migration.
//!
//! These types decouple the execution decision transport from the legacy
//! single-wrapped-JSON `agentic_decision` schema, enabling provider-native
//! tool calls while preserving side-channel metadata and observability.

use serde::Serialize;
use serde_json::Value;

use super::Decision;
use crate::magician_v2::execution::durable_task_state::TaskStateActionEnvelope;
use crate::magician_v2::slot_graph::extraction::ImageData;

// =============================================================================
// Envelope — lowered Decision + side-channel metadata + observability
// =============================================================================

/// Carries a parsed [`Decision`] together with side-channel metadata
/// (hover/vision requests, plan-revision signals) and raw payloads for
/// shadow-comparison and observability.
#[derive(Debug, Clone)]
pub struct ExecutionDecisionEnvelope {
    /// The parsed execution decision.
    pub decision: Decision,

    // -- side-channel metadata --
    /// Request hover-discovery on the next observation cycle.
    pub request_hover_discovery: Option<bool>,
    /// Request a vision screenshot on the next observation cycle.
    pub request_vision: Option<bool>,
    /// Reason the model wants vision.
    pub vision_reason: Option<String>,
    /// Step ID that was completed by this decision.
    pub step_completed: Option<String>,
    /// Step ID that failed during this decision.
    pub step_failed: Option<String>,
    /// Whether the model believes the plan needs revision.
    pub needs_plan_revision: bool,
    /// Internal durable task-state action proposed by the outer decision LLM.
    pub task_state_action: TaskStateActionEnvelope,

    // -- deferred tool calls --
    /// Tool calls that followed the first non-Execute (terminal / unlowerable)
    /// call in the same model turn. Leading Execute calls are folded into the
    /// in-turn batch; these hints have no arguments and must be re-issued.
    pub deferred_tool_calls: Vec<DeferredToolCall>,

    // -- observability --
    /// Model's chain-of-thought / reasoning text.
    pub thinking: Option<String>,
    /// Raw provider tool-call payload, kept for shadow comparison.
    pub raw_tool_call: Option<Value>,
    /// Raw text fallback when the model didn't emit a tool call.
    pub raw_text_fallback: Option<String>,
}

/// Summary of a tool call that was deferred because the model returned
/// multiple tool calls in one response.
#[derive(Debug, Clone)]
pub struct DeferredToolCall {
    /// Tool name (e.g. "read_file", "shell", "yield").
    pub name: String,
    /// Brief summary of what the call would do.
    pub summary: String,
}

// =============================================================================
// NativeDecisionOutcome — result of parsing a native response
// =============================================================================

/// Outcome of attempting to parse an [`ExecutionNativeResponse`] into a
/// decision. The happy path is `Valid`; all other variants are recoverable
/// or loggable error states.
#[derive(Debug, Clone)]
pub enum NativeDecisionOutcome {
    /// Successfully parsed into a decision envelope.
    Valid(ExecutionDecisionEnvelope),
    /// Model returned zero tool calls (text-only response).
    ZeroToolCalls {
        text_fallback: Option<String>,
        finish_reason: Option<String>,
    },
    /// Model returned more than one tool call in a single turn.
    MultipleToolCalls {
        count: usize,
        tool_names: Vec<String>,
    },
    /// Model called a tool name we don't recognise.
    UnknownTool {
        tool_name: String,
        raw_arguments: Value,
    },
    /// Tool name was valid but the arguments failed validation.
    InvalidArguments {
        tool_name: String,
        raw_arguments: Value,
        error: String,
    },
}

// =============================================================================
// Request / Response — thin transport types for the LLM router
// =============================================================================

/// Thin request sent to the LLM router for an execution-native call.
#[derive(Debug, Clone)]
pub struct ExecutionNativeRequest {
    /// Operation identifier (e.g. `"execution_decision"`).
    pub operation: String,
    /// System prompt for the call.
    pub system_prompt: String,
    /// User prompt for the call.
    pub user_prompt: String,
    /// Optional images to include in the request.
    pub images: Option<Vec<ImageData>>,
    /// Tool specifications registered for this call.
    pub tools: Vec<NativeExecutionTool>,
    /// Optional model override (e.g. for tier routing).
    pub model_override: Option<String>,
    /// Optional provider-native tool-choice override for this call.
    pub tool_choice_override: Option<Value>,
}

/// Response from the LLM router for an execution-native call.
#[derive(Debug)]
pub struct ExecutionNativeResponse {
    /// Tool calls returned by the model.
    tool_calls: Vec<ExecutionToolCall>,
    /// Private authority produced only by local admission. Provider JSON can
    /// never manufacture an accepted/rejected state or bypass raw inspection.
    argument_admission: NativeArgumentAdmission,
    /// Free-text content returned alongside (or instead of) tool calls.
    pub text: Option<String>,
    /// Provider-emitted reasoning / chain-of-thought text. Sourced from
    /// `LLMResponse.reasoning_text` (Anthropic Extended Thinking, OpenAI
    /// Responses reasoning items, DeepSeek-R1 `reasoning_content`).
    /// `None` when the model didn't reason or the provider doesn't surface
    /// reasoning content. Used by the inner-loop trace `assistant_turn`
    /// event so debugging across providers shows what each model thought.
    pub reasoning_text: Option<String>,
    /// Opaque server-side identifier for continuation chaining (for example,
    /// OpenAI Responses or Gemini Interactions). The inner-loop threads it
    /// through typed `LLMRequest.context_reuse` on the next compatible call.
    /// It is scoped to one provider/model/endpoint/transport cohort and is
    /// cleared whenever the replay shape can no longer be proven compatible.
    pub response_id: Option<String>,
    /// Provider finish reason (e.g. `"tool_calls"`, `"stop"`).
    pub finish_reason: Option<String>,
    /// Prompt token count (when reported by the provider).
    pub prompt_tokens: Option<u32>,
    /// Completion token count (when reported by the provider).
    pub completion_tokens: Option<u32>,
    /// Cache-read tokens for prompt-caching providers (Anthropic
    /// `cache_read_input_tokens`, OpenAI Responses
    /// `prompt_tokens_details.cached_tokens`). The inner-loop emits this
    /// in `llm.succeeded` so the chat UI's per-turn cache chip can
    /// accumulate across iterations.
    pub cached_tokens: Option<u32>,
    /// Cache-write tokens — the prompt prefix the provider committed to
    /// its cache this turn.
    pub cache_creation_tokens: Option<u32>,
    /// Provider that served the call (config identifier, e.g.
    /// `"anthropic"`). Threaded from the router's resolved routing so
    /// decision telemetry can attribute + price the call. `None` only in
    /// test fixtures that never touched a provider.
    pub provider: Option<String>,
    /// Model that served the call (effective model after overrides).
    pub model: Option<String>,
    /// Reasoning tokens billed this turn (when the provider reports them). Carried
    /// so decision telemetry can surface the cost of `effort: high` as its own
    /// field instead of the prior hardcoded 0.
    pub reasoning_tokens: Option<u32>,
    /// Selected LLM profile name when it deviates from the operation default;
    /// `None` = the operation's default profile. For decision telemetry attribution.
    pub profile: Option<String>,
    /// Exact operation-router telemetry, including stable call/attempt receipt
    /// and call-start time. Product adapters must carry this field unchanged;
    /// reconstructing it after lowering loses correlation and misprices calls.
    pub telemetry: Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry>,
}

#[derive(Debug, Clone, Default)]
enum NativeArgumentAdmission {
    #[default]
    Unchecked,
    Admitted(Vec<NativeToolArgumentAdmission>),
}

#[derive(Debug, Clone)]
enum NativeToolArgumentAdmission {
    Accepted,
    Rejected(String),
}

pub enum CachedNativeToolArgumentAdmission<'a> {
    Accepted,
    Rejected(&'a str),
}

impl Clone for ExecutionNativeResponse {
    fn clone(&self) -> Self {
        let (tool_calls, argument_admission) = match &self.argument_admission {
            NativeArgumentAdmission::Admitted(admissions)
                if admissions.len() == self.tool_calls.len() =>
            {
                let tool_calls = self
                    .tool_calls
                    .iter()
                    .zip(admissions)
                    .map(|(call, admission)| ExecutionToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: match admission {
                            NativeToolArgumentAdmission::Accepted => {
                                crate::magician_v2::json_traversal::clone_json_iteratively(
                                    &call.arguments,
                                )
                            },
                            NativeToolArgumentAdmission::Rejected(_) => Value::Null,
                        },
                    })
                    .collect();
                (tool_calls, self.argument_admission.clone())
            },
            NativeArgumentAdmission::Unchecked | NativeArgumentAdmission::Admitted(_) => {
                // Cloning an unadmitted response is itself an ownership
                // boundary. Inspect every raw provider tree, retain accepted
                // values iteratively, and keep rejection authority out of JSON.
                let mut admissions = Vec::with_capacity(self.tool_calls.len());
                let tool_calls = self
                    .tool_calls
                    .iter()
                    .map(|call| {
                        let arguments = match native_tool_argument_admission_error(&call.arguments)
                        {
                            Some(error) => {
                                admissions.push(NativeToolArgumentAdmission::Rejected(error));
                                Value::Null
                            },
                            None => {
                                admissions.push(NativeToolArgumentAdmission::Accepted);
                                crate::magician_v2::json_traversal::clone_json_iteratively(
                                    &call.arguments,
                                )
                            },
                        };
                        ExecutionToolCall {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            arguments,
                        }
                    })
                    .collect();
                (tool_calls, NativeArgumentAdmission::Admitted(admissions))
            },
        };
        Self {
            tool_calls,
            argument_admission,
            text: self.text.clone(),
            reasoning_text: self.reasoning_text.clone(),
            response_id: self.response_id.clone(),
            finish_reason: self.finish_reason.clone(),
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            cached_tokens: self.cached_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            provider: self.provider.clone(),
            model: self.model.clone(),
            reasoning_tokens: self.reasoning_tokens,
            profile: self.profile.clone(),
            telemetry: self.telemetry.clone(),
        }
    }
}

pub const MAX_NATIVE_TOOL_ARGUMENT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NATIVE_TOOL_ARGUMENT_NODES: usize = 200_000;

#[cfg(any(test, feature = "test-fixtures"))]
std::thread_local! {
    static NATIVE_ARGUMENT_ADMISSION_SCANS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

pub fn native_tool_argument_admission_error(arguments: &Value) -> Option<String> {
    #[cfg(any(test, feature = "test-fixtures"))]
    NATIVE_ARGUMENT_ADMISSION_SCANS.with(|count| count.set(count.get().saturating_add(1)));
    let Some(metrics) = crate::magician_v2::json_traversal::inspect_json_bounded(
        arguments,
        MAX_NATIVE_TOOL_ARGUMENT_NODES,
    ) else {
        return Some(format!(
            "tool arguments exceed the retained node limit ({MAX_NATIVE_TOOL_ARGUMENT_NODES})"
        ));
    };
    if metrics.max_depth > crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH {
        return Some(format!(
            "tool arguments exceed the retained JSON depth ({})",
            crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH
        ));
    }
    if crate::magician_v2::json_traversal::exact_json_encoded_len(arguments)
        > MAX_NATIVE_TOOL_ARGUMENT_BYTES
    {
        return Some(format!(
            "tool arguments exceed the retained byte limit ({MAX_NATIVE_TOOL_ARGUMENT_BYTES})"
        ));
    }
    None
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn reset_native_argument_admission_scan_count() {
    NATIVE_ARGUMENT_ADMISSION_SCANS.with(|count| count.set(0));
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn native_argument_admission_scan_count() -> usize {
    NATIVE_ARGUMENT_ADMISSION_SCANS.with(std::cell::Cell::get)
}

impl ExecutionNativeResponse {
    /// Construct a response from externally supplied tool calls and admit every
    /// argument tree before returning it. Public fixtures and integrations use
    /// this boundary so they cannot manufacture or retain unchecked provider
    /// state; provider adapters use the crate-private staged constructor while
    /// they populate transport metadata.
    pub fn from_tool_calls(tool_calls: Vec<ExecutionToolCall>) -> Self {
        let mut response = Self::unadmitted(tool_calls);
        response.admit_tool_arguments();
        response
    }

    /// Construct an unadmitted response at a provider/test ownership boundary.
    /// Callers may populate the remaining public telemetry fields before
    /// invoking [`Self::admit_tool_arguments`].
    pub fn unadmitted(tool_calls: Vec<ExecutionToolCall>) -> Self {
        Self {
            tool_calls,
            argument_admission: NativeArgumentAdmission::Unchecked,
            text: None,
            reasoning_text: None,
            response_id: None,
            finish_reason: None,
            prompt_tokens: None,
            completion_tokens: None,
            cached_tokens: None,
            cache_creation_tokens: None,
            provider: None,
            model: None,
            reasoning_tokens: None,
            profile: None,
            telemetry: None,
        }
    }

    pub fn tool_calls(&self) -> &[ExecutionToolCall] {
        &self.tool_calls
    }

    pub fn cached_tool_argument_admission(
        &self,
        index: usize,
    ) -> Option<CachedNativeToolArgumentAdmission<'_>> {
        let NativeArgumentAdmission::Admitted(admissions) = &self.argument_admission else {
            return None;
        };
        match admissions.get(index)? {
            NativeToolArgumentAdmission::Accepted => {
                Some(CachedNativeToolArgumentAdmission::Accepted)
            },
            NativeToolArgumentAdmission::Rejected(reason) => {
                Some(CachedNativeToolArgumentAdmission::Rejected(reason))
            },
        }
    }

    /// Apply the retained-argument contract while this response still owns the
    /// provider values. Rejected trees are drained iteratively here so a later
    /// ordinary drop cannot overflow after lowering reports the typed error.
    pub fn admit_tool_arguments(&mut self) {
        if matches!(
            self.argument_admission,
            NativeArgumentAdmission::Admitted(_)
        ) {
            return;
        }
        let mut admissions = Vec::with_capacity(self.tool_calls.len());
        for call in &mut self.tool_calls {
            match native_tool_argument_admission_error(&call.arguments) {
                None => admissions.push(NativeToolArgumentAdmission::Accepted),
                Some(error) => {
                    let rejected = std::mem::replace(&mut call.arguments, Value::Null);
                    crate::magician_v2::json_traversal::discard_json_iteratively(rejected);
                    admissions.push(NativeToolArgumentAdmission::Rejected(error));
                },
            }
        }
        self.argument_admission = NativeArgumentAdmission::Admitted(admissions);
    }
}

impl Drop for ExecutionNativeResponse {
    fn drop(&mut self) {
        // Provider adapters admit before returning, but keep destruction safe
        // for future adapters, debug fixtures, and early-error paths that may
        // never reach lowering. Moving each owned argument out prevents
        // `serde_json::Value`'s recursive destructor from seeing adversarial
        // provider nesting.
        for call in &mut self.tool_calls {
            let arguments = std::mem::replace(&mut call.arguments, Value::Null);
            crate::magician_v2::json_traversal::discard_json_iteratively(arguments);
        }
    }
}

// =============================================================================
// Tool call + tool spec primitives
// =============================================================================

/// A single native tool call as returned by the provider.
#[derive(Debug, Clone)]
pub struct ExecutionToolCall {
    /// Provider-assigned tool-call ID.
    pub id: String,
    /// Tool name (must match a registered [`NativeExecutionTool::name`]).
    pub name: String,
    /// Raw JSON arguments.
    pub arguments: Value,
}

/// Specification for a tool registered with the provider.
#[derive(Debug, Clone, Serialize)]
pub struct NativeExecutionTool {
    /// Tool name as sent to the provider.
    pub name: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// JSON Schema describing the tool's parameters.
    pub parameters: Value,
    /// Whether this is a control-flow tool (e.g. `goal_reached`,
    /// `cannot_proceed`) rather than an action tool.
    pub is_control_tool: bool,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::Decision;

    #[test]
    fn provider_response_admission_drains_rejected_argument_tree_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| {
                let mut arguments = Value::Null;
                for _ in 0..10_000 {
                    arguments = Value::Array(vec![arguments]);
                }
                let mut response = ExecutionNativeResponse::unadmitted(vec![ExecutionToolCall {
                    id: "deep".to_string(),
                    name: "read_file".to_string(),
                    arguments,
                }]);
                response.finish_reason = Some("tool_calls".to_string());
                response.provider = Some("test".to_string());
                response.model = Some("test".to_string());
                response.admit_tool_arguments();
                assert!(matches!(
                    response.cached_tool_argument_admission(0),
                    Some(CachedNativeToolArgumentAdmission::Rejected(_))
                ));
                assert_eq!(response.tool_calls()[0].arguments, Value::Null);
                // Ordinary response destruction is now shallow and safe.
            })
            .expect("small-stack response worker")
            .join()
            .expect("provider admission must drain rejected JSON safely");
    }

    #[test]
    fn native_response_drop_is_stack_safe_before_explicit_admission() {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| {
                let mut arguments = Value::Null;
                for _ in 0..10_000 {
                    arguments = Value::Array(vec![arguments]);
                }
                let response = ExecutionNativeResponse::unadmitted(vec![ExecutionToolCall {
                    id: "deep".to_string(),
                    name: "read_file".to_string(),
                    arguments,
                }]);
                let cloned = response.clone();
                assert!(matches!(
                    cloned.cached_tool_argument_admission(0),
                    Some(CachedNativeToolArgumentAdmission::Rejected(_))
                ));
                assert_eq!(cloned.tool_calls()[0].arguments, Value::Null);
                drop(cloned);
                drop(response);
            })
            .expect("small-stack response drop worker")
            .join()
            .expect("unadmitted response drop must remain stack safe");
    }

    #[test]
    fn provider_reserved_rejection_field_cannot_bypass_raw_admission() {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| {
                let mut deep_sibling = Value::Null;
                for _ in 0..10_000 {
                    deep_sibling = Value::Array(vec![deep_sibling]);
                }
                let mut arguments = serde_json::Map::new();
                arguments.insert(
                    "__magician_argument_rejected".to_string(),
                    Value::String("provider-controlled".repeat(600_000)),
                );
                arguments.insert("deep_sibling".to_string(), deep_sibling);
                let mut response = ExecutionNativeResponse::unadmitted(vec![ExecutionToolCall {
                    id: "spoof".to_string(),
                    name: "yield".to_string(),
                    arguments: Value::Object(arguments),
                }]);

                response.admit_tool_arguments();

                let Some(CachedNativeToolArgumentAdmission::Rejected(reason)) =
                    response.cached_tool_argument_admission(0)
                else {
                    panic!("provider-controlled marker and deep sibling must be rejected");
                };
                assert!(reason.contains("depth") || reason.contains("node limit"));
                assert!(!reason.contains("provider-controlled"));
                assert_eq!(response.tool_calls()[0].arguments, Value::Null);
            })
            .expect("small-stack spoof admission worker")
            .join()
            .expect("reserved provider fields must not bypass typed admission");
    }

    #[test]
    fn envelope_preserves_side_channel_metadata() {
        let envelope = ExecutionDecisionEnvelope {
            decision: Decision::Failed {
                reason: "test".into(),
            },
            request_hover_discovery: Some(true),
            request_vision: Some(false),
            vision_reason: None,
            step_completed: Some("step-1".into()),
            step_failed: None,
            needs_plan_revision: false,
            task_state_action: TaskStateActionEnvelope::default(),
            deferred_tool_calls: vec![],
            thinking: Some("reasoning".into()),
            raw_tool_call: None,
            raw_text_fallback: None,
        };
        assert_eq!(envelope.request_hover_discovery, Some(true));
        assert_eq!(envelope.step_completed, Some("step-1".to_string()));
        assert!(!envelope.needs_plan_revision);
        assert_eq!(envelope.thinking, Some("reasoning".to_string()));
    }

    #[test]
    fn envelope_captures_raw_payloads_for_observability() {
        let raw_call = serde_json::json!({
            "name": "browser",
            "arguments": {"tool_name": "click", "element_id": 5}
        });
        let envelope = ExecutionDecisionEnvelope {
            decision: Decision::Failed { reason: "x".into() },
            request_hover_discovery: None,
            request_vision: None,
            vision_reason: None,
            step_completed: None,
            step_failed: None,
            needs_plan_revision: false,
            task_state_action: TaskStateActionEnvelope::default(),
            deferred_tool_calls: vec![],
            thinking: None,
            raw_tool_call: Some(raw_call.clone()),
            raw_text_fallback: Some("fallback text".into()),
        };
        assert_eq!(envelope.raw_tool_call, Some(raw_call));
        assert_eq!(envelope.raw_text_fallback, Some("fallback text".into()));
    }
}
