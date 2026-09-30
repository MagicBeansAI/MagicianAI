//! Bridge between the execution domain types and the native tool infrastructure.
//!
//! This module converts [`AgenticContext`] into [`CatalogBuildContext`] so that
//! the execution-native catalog builder can produce provider-native tool
//! specifications without depending on the full execution context.
//!
//! Phase 3 wiring layer — executor integration for native tool-call decisions.

use base64::Engine;
use magicllm::prelude::LLMMessage as RouterMessage;
use serde_json::{json, Value};

use super::native_catalog::{
    apply_catalog_context_to_tools, build_execution_native_catalog, CatalogAgentPolicy,
    CatalogBuildContext,
};
use super::native_types::ExecutionDecisionEnvelope;
use super::types::{AgenticAssistantToolCallRecord, AgenticAssistantTurnRecord, AgenticContext};
use super::Decision;
use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use crate::magician_v2::execution::builtin_action_types::is_reserved_non_pack_capability_name;
use crate::magician_v2::execution::durable_task_state::TaskStateActionEnvelope;
use crate::magician_v2::execution::multi_llm_agent_adapter::MultiLlmAgentAdapter;

fn external_policy_digest_for_context(ctx: &AgenticContext) -> Result<String, String> {
    if ctx.trust_level.is_none() {
        return Ok("trust-policy-disabled".to_string());
    }
    let bytes = if let Some(enforcer) = ctx.preloaded_trust_enforcer.as_ref() {
        serde_json::to_vec(enforcer.policies())
            .map_err(|error| format!("trust_policy_digest_serialize_failed:{error}"))?
    } else {
        let path = ctx
            .trust_policies_path
            .as_ref()
            .ok_or_else(|| "trust_policy_path_missing".to_string())?;
        std::fs::read(path).map_err(|error| format!("trust_policy_digest_read_failed:{error}"))?
    };
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

// =============================================================================
// NativeDecisionMetadata — side-channel data from native decisions
// =============================================================================

/// Metadata extracted from an [`ExecutionDecisionEnvelope`] that the executor needs.
///
/// Carries side-channel fields the executor consumes alongside the lowered
/// native decision.
#[derive(Debug, Clone)]
pub struct NativeDecisionMetadata {
    pub request_hover_discovery: Option<bool>,
    pub request_vision: Option<bool>,
    pub vision_reason: Option<String>,
    pub step_completed: Option<String>,
    pub step_failed: Option<String>,
    pub needs_plan_revision: bool,
    pub task_state_action: TaskStateActionEnvelope,
    /// Summaries of tool calls queued after the first non-Execute call in the
    /// same turn. Empty when every call folded into the in-turn Execute batch.
    pub deferred_tool_calls: Vec<super::native_types::DeferredToolCall>,
    /// Assistant-visible text/tool-call turn returned by the provider.
    pub assistant_turn: Option<AgenticAssistantTurnRecord>,
    /// Provider-issued response identifier for a stateful continuation
    /// transport (OpenAI Responses or an opted-in Gemini Interactions profile).
    /// The executor stores this on `ExecutionHistory.last_response_id` so the
    /// next outer-loop turn can send only new input. `None` for stateless
    /// transports such as Anthropic Messages and OpenAI Chat.
    pub response_id: Option<String>,
    /// Explicit prompt-transport mode selected for this turn. Propagated into
    /// the canonical iteration correlation so evals can identify a real
    /// periodic rebootstrap from telemetry instead of guessing by call order.
    pub prompt_projection_mode: Option<&'static str>,
    /// Whether the rendered request for this decision carried image content (a
    /// screenshot). The executor records it on `ExecutionHistory.last_has_images`
    /// so the next turn can DROP a chained `previous_response_id` when the
    /// image-shape flips: OpenAI scopes Responses ids per model, and a vision
    /// toggle can select a different model/profile, so re-feeding the prior id
    /// would corrupt the chain. The constructors default it to `false`;
    /// `decide_next_action` sets the real value after the call (it owns the
    /// screenshot-extraction result).
    pub has_images: bool,
    /// Token/cost telemetry for the LLM call that produced this decision.
    /// Populated by `from_envelope_and_response` from the provider's reported
    /// usage so the executor emits accurate `LLMResponseReceived` token counts
    /// (input / output / cache) instead of zeros. `None` for synthetic
    /// (non-LLM) decisions where no call was made.
    pub telemetry: Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry>,
}

impl NativeDecisionMetadata {
    /// Build a `NativeDecisionMetadata` for synthetic (non-LLM-derived)
    /// decisions. Used by the orchestrator-level auto-yield path
    /// (`decision::try_synthesize_stuck_auto_yield`) where the LLM is
    /// bypassed and the decision is computed structurally from the
    /// iteration history.
    ///
    /// `synthetic_source` identifies the synthesiser (e.g.
    /// `"stuck_auto_yield"`); `note` is a free-form human-readable
    /// reason logged into the assistant turn record for traceability.
    pub fn synthetic(synthetic_source: &str, note: &str) -> Self {
        let turn = AgenticAssistantTurnRecord {
            iteration: 0,
            operation: synthetic_source.to_string(),
            llm_trace_context: None,
            text: Some(note.to_string()),
            tool_calls: Vec::new(),
            finish_reason: Some("synthetic".to_string()),
            prompt_tokens: None,
            completion_tokens: None,
            reasoning: None,
            timestamp: chrono::Utc::now(),
        };
        Self {
            request_hover_discovery: None,
            request_vision: None,
            vision_reason: None,
            step_completed: None,
            step_failed: None,
            needs_plan_revision: false,
            task_state_action: TaskStateActionEnvelope::default(),
            deferred_tool_calls: Vec::new(),
            assistant_turn: Some(turn),
            response_id: None,
            prompt_projection_mode: None,
            has_images: false,
            telemetry: None,
        }
    }

    /// Whether the model requested vision escalation.
    pub fn needs_vision_escalation(&self) -> bool {
        self.request_vision.unwrap_or(false)
    }

    /// Vision escalation reason, if the model requested it.
    pub fn vision_escalation_reason(&self) -> Option<&str> {
        if self.needs_vision_escalation() {
            self.vision_reason.as_deref()
        } else {
            None
        }
    }

    /// Format deferred tool calls as a hint string for inclusion in execution history.
    /// Returns None if there are no deferred calls.
    pub fn format_deferred_hint(&self) -> Option<String> {
        if self.deferred_tool_calls.is_empty() {
            return None;
        }
        let summaries: Vec<&str> = self
            .deferred_tool_calls
            .iter()
            .map(|d| d.summary.as_str())
            .collect();
        Some(format!(
            "NOTE: You queued these calls after a non-action (terminal) call in the same turn, so \
             they were NOT run: {}. Re-issue them in a new turn if still needed.",
            summaries.join(", ")
        ))
    }
}

impl From<&ExecutionDecisionEnvelope> for NativeDecisionMetadata {
    fn from(env: &ExecutionDecisionEnvelope) -> Self {
        Self {
            request_hover_discovery: env.request_hover_discovery,
            request_vision: env.request_vision,
            vision_reason: env.vision_reason.clone(),
            step_completed: env.step_completed.clone(),
            step_failed: env.step_failed.clone(),
            needs_plan_revision: env.needs_plan_revision,
            task_state_action: env.task_state_action.clone(),
            deferred_tool_calls: env.deferred_tool_calls.clone(),
            assistant_turn: None,
            response_id: None,
            prompt_projection_mode: None,
            has_images: false,
            telemetry: None,
        }
    }
}

impl NativeDecisionMetadata {
    pub(crate) fn from_envelope_and_response(
        operation_name: &str,
        env: &ExecutionDecisionEnvelope,
        response: &super::native_types::ExecutionNativeResponse,
    ) -> Self {
        let mut metadata = Self::from(env);
        metadata.assistant_turn = assistant_turn_from_native_response(operation_name, response);
        metadata.response_id = response.response_id.clone();
        if let Some(telemetry) = response.telemetry.clone() {
            metadata.telemetry = Some(telemetry);
            return metadata;
        }
        // Capture the provider-reported token usage plus provider / model
        // attribution so the executor can emit fully-priced telemetry on
        // `LLMResponseReceived` (chat UI token chip, analytics `llm_calls`,
        // /llm page). Cost uses the same active pricing table (built-in base
        // + `llm_pricing.json` overlay) as every other priced call site
        // (`build_telemetry`, chat-inline); models missing from the table
        // price to 0.0. `prompt_tokens` already includes the cache read/write
        // counts — `compute_cost_at` does the subtraction, so the raw fields
        // pass through unmodified. This construction path has no upstream
        // start timestamp, so it prices at now-at-construction and stamps the
        // same instant into `started_at_ms`. This branch is test/legacy
        // compatibility only; production router responses carry exact
        // telemetry above and must never be reconstructed here.
        let provider = response.provider.clone().unwrap_or_default();
        let model = response.model.clone().unwrap_or_default();
        let provider_kind = magicllm::LLMProviderKind::from_str(&provider);
        let usage = magicllm::TokenUsage {
            prompt_tokens: response.prompt_tokens,
            completion_tokens: response.completion_tokens,
            total_tokens: None,
            reasoning_tokens: response.reasoning_tokens,
            cached_tokens: response.cached_tokens,
            cache_creation_tokens: response.cache_creation_tokens,
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        let cost_usd = magicllm::compute_cost_at(&provider_kind, &model, &usage, now_ms);
        let usage_reported = response.prompt_tokens.is_some()
            || response.completion_tokens.is_some()
            || response.reasoning_tokens.is_some()
            || response.cached_tokens.is_some()
            || response.cache_creation_tokens.is_some();
        metadata.telemetry = Some(
            crate::magician_v2::slot_graph::extraction::LlmCallTelemetry {
                provider,
                model,
                usage_reported,
                input_tokens: response.prompt_tokens.unwrap_or(0),
                output_tokens: response.completion_tokens.unwrap_or(0),
                reasoning_tokens: response.reasoning_tokens.unwrap_or(0),
                cache_read_tokens: response.cached_tokens.unwrap_or(0),
                cache_creation_tokens: response.cache_creation_tokens.unwrap_or(0),
                cost_usd,
                reasoning_summary: response
                    .reasoning_text
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string),
                profile: response.profile.clone(),
                operation: Some(operation_name.to_string()),
                started_at_ms: now_ms,
                ..Default::default()
            },
        );
        metadata
    }
}

fn assistant_turn_from_native_response(
    operation_name: &str,
    response: &super::native_types::ExecutionNativeResponse,
) -> Option<AgenticAssistantTurnRecord> {
    let text = response
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string);
    let tool_calls = response
        .tool_calls()
        .iter()
        .map(|tool_call| AgenticAssistantToolCallRecord {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments: crate::magician_v2::json_traversal::clone_json_iteratively(
                &tool_call.arguments,
            ),
        })
        .collect::<Vec<_>>();

    if text.is_none() && tool_calls.is_empty() {
        return None;
    }

    Some(AgenticAssistantTurnRecord {
        iteration: 0,
        operation: operation_name.to_string(),
        llm_trace_context: None,
        text,
        tool_calls,
        finish_reason: response.finish_reason.clone(),
        prompt_tokens: response.prompt_tokens,
        completion_tokens: response.completion_tokens,
        // Persist the model's reasoning summary (previously parsed then dropped).
        reasoning: response
            .reasoning_text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        timestamp: chrono::Utc::now(),
    })
}

// =============================================================================
// native_decision_via_adapter — execute the native path
// =============================================================================

/// Positive execution contract appended to the system prompt for autonomous
/// native-tool decisions. Tool names and parameter shapes deliberately come
/// only from the provider-native catalog; this text carries cross-tool runtime
/// semantics that schemas cannot express: every turn acts, ordered batching,
/// terminal placement, and evidence-backed completion.
pub const NATIVE_TOOL_INSTRUCTION: &str = "\n\nFor every autonomous decision, use the \
     provider-native tool-calling channel. Every response must contain at least one tool call. \
     Select one or more tools from the provided catalog and pass arguments matching each tool's \
     schema. The tool call itself is the decision.\n\n\
     You may issue multiple non-terminal tool calls when they form a natural ordered sequence. \
     They run in the order listed. Put actions before the calls that verify their effects. \
     `yield` and `need_user_input` are terminal calls: call them only after reviewing the relevant \
     results, and place nothing after them.\n\n\
     Before yielding completion, verify the objective with tool-backed evidence. If actionable \
     work remains, make the next concrete tool call. Use `yield` only when the work is complete, \
     partially complete with identified open items, or genuinely blocked.";
pub const CHAT_NATIVE_TOOL_INSTRUCTION: &str =
    "\n\nIMPORTANT: When a runtime action is needed, call exactly ONE tool per response. If the chat request can be answered directly without a tool, respond with assistant text.";

/// Execute a native execution decision using the adapter directly.
///
/// Returns the decision and side-channel metadata on success, or an error if
/// the native path fails. There is no fallback path.
pub async fn native_decision_via_adapter(
    ctx: &AgenticContext,
    adapter: &MultiLlmAgentAdapter,
    operation_name: &str,
    system_prompt: &str,
    user_prompt: &str,
    images: Option<Vec<crate::magician_v2::slot_graph::extraction::ImageData>>,
    projection_iteration: Option<usize>,
    viewport: Option<(u32, u32)>,
) -> anyhow::Result<(Decision, NativeDecisionMetadata)> {
    let native_instruction = if ctx.chat_inline {
        CHAT_NATIVE_TOOL_INSTRUCTION
    } else {
        NATIVE_TOOL_INSTRUCTION
    };
    let tool_choice_override = ctx.chat_inline.then(|| json!({"type": "auto"}));

    let catalog_ctx = build_catalog_context(ctx);
    // §4.2c layer 1: live engagement authority is read immediately before
    // policy resolution so the projected catalog narrows to CURRENT authority.
    let engagement = resolve_engagement_snapshot_input(ctx).await;
    let (tools, deferred_block) =
        build_decision_tools_and_deferred(ctx, &catalog_ctx, projection_iteration, engagement);
    let system_prompt_with_instruction = match deferred_block {
        Some(block) if !block.is_empty() => {
            format!("{}{}\n\n{}", system_prompt, native_instruction, block)
        },
        _ => format!("{}{}", system_prompt, native_instruction),
    };
    write_outer_prompt_projection_best_effort(
        ctx,
        operation_name,
        projection_iteration,
        &system_prompt_with_instruction,
        user_prompt,
        &[],
        None,
        &tools,
        images.as_deref(),
        tool_choice_override.as_ref(),
    );

    let (envelope, response) = adapter
        .call_execution_native(
            operation_name,
            &system_prompt_with_instruction,
            user_prompt,
            tools,
            images,
            tool_choice_override,
            ctx.chat_inline,
            viewport,
        )
        .await?;

    let metadata =
        NativeDecisionMetadata::from_envelope_and_response(operation_name, &envelope, &response);
    let policy_snapshot_id = ctx
        .policy_snapshot_id
        .lock()
        .ok()
        .and_then(|snapshot_id| snapshot_id.clone());
    tracing::info!(
        operation = operation_name,
        execution_id = ctx.legacy_execution_id.as_deref().unwrap_or("<none>"),
        artifact_chain_id = ctx.artifact_chain_id.as_deref().unwrap_or("<none>"),
        plan_id = ctx.plan_id.as_deref().unwrap_or("<none>"),
        step_id = ctx.step_id.as_deref().unwrap_or("<none>"),
        agent_id = ctx.agent_id.as_deref().unwrap_or("<none>"),
        policy_snapshot_id = policy_snapshot_id.as_deref().unwrap_or("<none>"),
        "Native execution decision succeeded"
    );
    Ok((envelope.decision, metadata))
}

/// Multi-message variant of `native_decision_via_adapter`.
///
/// Sends `[system, ...history_messages, user]` so the model sees its last few
/// raw decisions + outcomes as assistant/user pairs instead of relying solely
/// on the summarized history baked into the user prompt. `history_messages`
/// should already be ordered chronologically (oldest first). When `images` is
/// `Some`, falls back to the single-prompt path because the multi-message
/// adapter does not yet thread image attachments.
/// The decision prompt split at its cache marker: the part before it (goal,
/// success criteria, skills and their playbook) is the same on every turn
/// of a run; the part after it (current state, history, task state) changes.
/// A prompt without the marker (a continuation delta) is all turn prompt.
fn split_stable_prompt(user_prompt: &str) -> (Option<String>, String) {
    match magicllm::types::split_on_cache_sentinel(user_prompt) {
        (stable, Some(turn)) if !stable.trim().is_empty() => (Some(stable), turn),
        (_, Some(turn)) => (None, turn),
        (text, None) => (None, text),
    }
}

/// Put the run's stable prompt first, right after the system prompt, and the
/// conversation after it. Every provider caches by prefix; re-rendered as
/// part of the final message, after a conversation that grows each turn,
/// the ~32k-char stable part was never a prefix of the next request and was
/// billed uncached on every call (Opus 5.5 plateaued at ~60% cached). Here
/// it is inside the cached prefix from the second call on. A conversation
/// that opens with a user message takes the stable text as its first block,
/// since providers reject consecutive user messages.
fn place_stable_prompt(
    messages: &mut Vec<RouterMessage>,
    stable_prompt: Option<String>,
    mut history_messages: Vec<RouterMessage>,
) {
    if let Some(stable) = stable_prompt {
        // The marker stays at its end: providers with explicit breakpoints
        // (GPT-5.6+, Anthropic outside the rolling path) cache through it,
        // and OpenAI's routing key is digested from what precedes it —
        // system, tools and this prompt, the same for the whole run. With the
        // conversation in front of the marker the key changed on every full
        // send, each landed on a cold cache and read 0 tokens, even of the
        // unchanged system prompt.
        let text = format!("{stable}\n{}", magicllm::types::CACHE_BREAKPOINT_SENTINEL);
        let block = magicllm::prelude::ContentBlock::Text { text };
        match history_messages.first_mut() {
            Some(first) if matches!(first.role, magicllm::prelude::MessageRole::User) => {
                first.content.insert(0, block);
            },
            _ => messages.push(RouterMessage {
                role: magicllm::prelude::MessageRole::User,
                content: vec![block],
            }),
        }
    }
    messages.extend(history_messages);
}

pub async fn native_decision_via_adapter_with_history(
    ctx: &AgenticContext,
    adapter: &MultiLlmAgentAdapter,
    operation_name: &str,
    system_prompt: &str,
    user_prompt: &str,
    history_messages: Vec<RouterMessage>,
    images: Option<Vec<crate::magician_v2::slot_graph::extraction::ImageData>>,
    projection_iteration: Option<usize>,
    previous_response_id: Option<&str>,
    viewport: Option<(u32, u32)>,
) -> anyhow::Result<(Decision, NativeDecisionMetadata)> {
    if history_messages.is_empty() {
        return native_decision_via_adapter(
            ctx,
            adapter,
            operation_name,
            system_prompt,
            user_prompt,
            images,
            projection_iteration,
            viewport,
        )
        .await;
    }

    let native_instruction = if ctx.chat_inline {
        CHAT_NATIVE_TOOL_INSTRUCTION
    } else {
        NATIVE_TOOL_INSTRUCTION
    };
    let tool_choice_override = ctx.chat_inline.then(|| json!({"type": "auto"}));

    let catalog_ctx = build_catalog_context(ctx);
    // §4.2c layer 1: live engagement authority is read immediately before
    // policy resolution so the projected catalog narrows to CURRENT authority.
    let engagement = resolve_engagement_snapshot_input(ctx).await;
    let (tools, deferred_block) =
        build_decision_tools_and_deferred(ctx, &catalog_ctx, projection_iteration, engagement);
    let system_prompt_with_instruction = match deferred_block {
        Some(block) if !block.is_empty() => {
            format!("{}{}\n\n{}", system_prompt, native_instruction, block)
        },
        _ => format!("{}{}", system_prompt, native_instruction),
    };
    write_outer_prompt_projection_best_effort(
        ctx,
        operation_name,
        projection_iteration,
        &system_prompt_with_instruction,
        user_prompt,
        &history_messages,
        previous_response_id,
        &tools,
        images.as_deref(),
        tool_choice_override.as_ref(),
    );

    let (stable_prompt, turn_prompt) = split_stable_prompt(user_prompt);
    let mut messages: Vec<RouterMessage> = Vec::with_capacity(history_messages.len() + 3);
    messages.push(RouterMessage::system(&system_prompt_with_instruction));
    place_stable_prompt(&mut messages, stable_prompt, history_messages);

    // Build the final user blocks: the prompt text plus any attached images
    // (browser screenshots for vision-enabled agents). Images go AFTER the
    // text so the model has the textual context (state + ledger) before the
    // visual context. Anthropic / OpenAI / Gemini all reject consecutive
    // user messages, so we merge into the trailing `User { ToolResult... }`
    // message rather than pushing a new one.
    let mut trailing_blocks: Vec<magicllm::prelude::ContentBlock> = Vec::new();
    trailing_blocks.push(magicllm::prelude::ContentBlock::Text { text: turn_prompt });
    if let Some(image_data) = images.as_ref() {
        for image in image_data {
            match base64::engine::general_purpose::STANDARD.decode(image.base64.as_bytes()) {
                Ok(bytes) => {
                    trailing_blocks.push(magicllm::prelude::ContentBlock::Image {
                        data: bytes,
                        media_type: image.media_type.clone(),
                        caption: None,
                    });
                },
                Err(err) => {
                    // Logging-only: if a screenshot fails to decode we still
                    // want the textual decision to proceed. The model loses
                    // visual context for this turn but won't fail outright.
                    tracing::warn!(
                        target: "outer_loop_multiturn",
                        media_type = %image.media_type,
                        error = %err,
                        "Failed to base64-decode outer-loop image for ContentBlock::Image — dropping image"
                    );
                },
            }
        }
    }

    match messages.last_mut() {
        Some(last) if matches!(last.role, magicllm::prelude::MessageRole::User) => {
            for block in trailing_blocks {
                last.content.push(block);
            }
        },
        _ => {
            messages.push(RouterMessage {
                role: magicllm::prelude::MessageRole::User,
                content: trailing_blocks,
            });
        },
    }

    let (envelope, response) = adapter
        .call_execution_native_with_messages(
            operation_name,
            messages,
            tools,
            tool_choice_override,
            ctx.chat_inline,
            previous_response_id,
            viewport,
        )
        .await?;

    let metadata =
        NativeDecisionMetadata::from_envelope_and_response(operation_name, &envelope, &response);
    let policy_snapshot_id = ctx
        .policy_snapshot_id
        .lock()
        .ok()
        .and_then(|snapshot_id| snapshot_id.clone());
    tracing::info!(
        operation = operation_name,
        execution_id = ctx.legacy_execution_id.as_deref().unwrap_or("<none>"),
        agent_id = ctx.agent_id.as_deref().unwrap_or("<none>"),
        policy_snapshot_id = policy_snapshot_id.as_deref().unwrap_or("<none>"),
        previous_response_id = ?previous_response_id,
        new_response_id = ?response.response_id.as_deref(),
        "Native execution decision (with history) succeeded"
    );
    Ok((envelope.decision, metadata))
}

#[allow(clippy::too_many_arguments)]
fn write_outer_prompt_projection_best_effort(
    ctx: &AgenticContext,
    operation_name: &str,
    iteration: Option<usize>,
    system_prompt: &str,
    user_prompt: &str,
    history_messages: &[RouterMessage],
    previous_response_id: Option<&str>,
    tools: &[super::native_types::NativeExecutionTool],
    images: Option<&[crate::magician_v2::slot_graph::extraction::ImageData]>,
    tool_choice_override: Option<&Value>,
) {
    let Some(execution_id) = ctx
        .execution_id
        .as_deref()
        .or(ctx.legacy_execution_id.as_deref())
    else {
        return;
    };

    let principal = ctx.principal.as_deref().unwrap_or(DEFAULT_SCOPE_PRINCIPAL);
    let workspace = ctx.workspace.as_deref().unwrap_or(DEFAULT_SCOPE_WORKSPACE);
    let workspace_root = ArtifactV2Workspace::resolve_scoped_root(&ctx.storage_base_path);
    let workspace_layout = ArtifactV2Workspace::new(workspace_root);
    let dir = workspace_layout
        .runtime_execution_dir(principal, workspace, ctx.task_id.as_deref(), execution_id)
        .join("prompt_projections");

    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            target: "outer_prompt_projection",
            execution_id,
            path = %dir.display(),
            error = %err,
            "failed to create outer prompt projection directory"
        );
        return;
    }

    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let iteration_segment = iteration
        .map(|value| format!("iter_{value:04}"))
        .unwrap_or_else(|| "iter_unknown".to_string());
    let base_filename = format!(
        "outer_prompt_projection-{}-{}-{timestamp}",
        sanitize_projection_filename_component(operation_name),
        iteration_segment
    );
    let path = dir.join(format!("{base_filename}.md"));
    let system_path = dir.join(format!("{base_filename}-system.txt"));
    let user_path = dir.join(format!("{base_filename}-user.txt"));
    let policy_snapshot_id = ctx
        .policy_snapshot_id
        .lock()
        .ok()
        .and_then(|snapshot_id| snapshot_id.clone());
    let projection = render_outer_prompt_projection(
        operation_name,
        iteration,
        policy_snapshot_id.as_deref(),
        system_prompt,
        user_prompt,
        history_messages,
        previous_response_id,
        tools,
        images,
        tool_choice_override,
    );

    let request_written =
        write_projection_file(execution_id, "request", &path, projection.as_bytes());
    let system_written = write_projection_file(
        execution_id,
        "system",
        &system_path,
        system_prompt.as_bytes(),
    );
    let user_written =
        write_projection_file(execution_id, "user", &user_path, user_prompt.as_bytes());

    // Per-iteration projection-save log commented out: it printed three
    // full projection paths every decision and dominated the INFO stream.
    // Re-enable (or downgrade to debug!) if you need to trace projection writes.
    // tracing::info!(
    //     target: "outer_prompt_projection",
    //     execution_id,
    //     operation = operation_name,
    //     iteration = iteration.unwrap_or_default(),
    //     path = %path.display(),
    //     system_path = %system_path.display(),
    //     user_path = %user_path.display(),
    //     request_written,
    //     system_written,
    //     user_written,
    //     "saved outer prompt projection"
    // );
    let _ = (request_written, system_written, user_written);
}

fn write_projection_file(
    execution_id: &str,
    kind: &str,
    path: &std::path::Path,
    bytes: &[u8],
) -> bool {
    if let Err(err) = std::fs::write(path, bytes) {
        tracing::warn!(
            target: "outer_prompt_projection",
            execution_id,
            kind,
            path = %path.display(),
            error = %err,
            "failed to write outer prompt projection file"
        );
        return false;
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn render_outer_prompt_projection(
    operation_name: &str,
    iteration: Option<usize>,
    policy_snapshot_id: Option<&str>,
    system_prompt: &str,
    user_prompt: &str,
    history_messages: &[RouterMessage],
    previous_response_id: Option<&str>,
    tools: &[super::native_types::NativeExecutionTool],
    images: Option<&[crate::magician_v2::slot_graph::extraction::ImageData]>,
    tool_choice_override: Option<&Value>,
) -> String {
    let mut out = String::new();
    out.push_str("# Outer Prompt Projection\n\n");
    out.push_str("## Run Context\n\n");
    out.push_str("- loop: `outer`\n");
    out.push_str(&format!("- operation: `{operation_name}`\n"));
    out.push_str(&format!(
        "- policy_snapshot_id: `{}`\n",
        policy_snapshot_id.unwrap_or("unresolved")
    ));
    out.push_str(&format!(
        "- outer_iteration: `{}`\n",
        iteration
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    ));
    out.push_str(&format!(
        "- generated_at: `{}`\n",
        chrono::Utc::now().to_rfc3339()
    ));
    out.push_str(&format!(
        "- image_blocks: `{}`\n",
        images.map(|items| items.len()).unwrap_or(0)
    ));

    out.push_str("\n## Request\n\n```json\n");
    out.push_str(
        &serde_json::to_string_pretty(&projected_execution_native_request(
            operation_name,
            system_prompt,
            user_prompt,
            history_messages,
            previous_response_id,
            tools,
            images,
            tool_choice_override,
        ))
        .unwrap_or_else(|_| "{}".to_string()),
    );
    out.push_str("\n```\n");
    out
}

#[allow(clippy::too_many_arguments)]
fn projected_execution_native_request(
    operation_name: &str,
    system_prompt: &str,
    user_prompt: &str,
    history_messages: &[RouterMessage],
    previous_response_id: Option<&str>,
    tools: &[super::native_types::NativeExecutionTool],
    images: Option<&[crate::magician_v2::slot_graph::extraction::ImageData]>,
    tool_choice_override: Option<&Value>,
) -> Value {
    let mut user_content = vec![json!({
        "type": "text",
        "text": user_prompt,
    })];
    if let Some(images) = images {
        for image in images {
            user_content.push(json!({
                "type": "image",
                "media_type": image.media_type,
                "base64_chars": image.base64.len(),
                "detail": image.detail.map(image_detail_label),
                "data": "<image bytes omitted from projection>",
            }));
        }
    }

    let tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect();

    // Reconstruct the message array exactly as it is sent: system, then the
    // recent-turn assistant/user pairs (the model's own tool calls + their
    // results), then the final user turn. Earlier this projection hardcoded
    // `[system, user]` and silently dropped the history, which made it look
    // like no past context was sent even when it was.
    let mut messages = vec![json!({
        "role": "system",
        "content": [{ "type": "text", "text": system_prompt }],
    })];
    for message in history_messages {
        messages.push(projected_history_message(message));
    }
    messages.push(json!({
        "role": "user",
        "content": user_content,
    }));

    json!({
        "operation": operation_name,
        "model_override": null,
        "previous_response_id": previous_response_id,
        "history_message_count": history_messages.len(),
        "messages": messages,
        "tools": tools,
        "tool_choice": tool_choice_override.cloned(),
    })
}

/// Render a single recent-turn history message into a compact projection
/// value. Tool calls and their results are kept verbatim (results are already
/// truncated upstream); image bytes are omitted to keep the projection small.
fn projected_history_message(message: &RouterMessage) -> Value {
    use magicllm::prelude::{ContentBlock, MessageRole};

    let role = match message.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    };
    let content: Vec<Value> = message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => json!({
                "type": "tool_call",
                "id": id,
                "name": name,
                "arguments": arguments,
            }),
            ContentBlock::ToolResult {
                tool_call_id,
                content,
            } => json!({
                "type": "tool_result",
                "tool_call_id": tool_call_id,
                "content": content,
            }),
            ContentBlock::Image {
                media_type,
                data,
                caption,
            } => json!({
                "type": "image",
                "media_type": media_type,
                "bytes": data.len(),
                "caption": caption,
                "data": "<image bytes omitted from projection>",
            }),
            ContentBlock::ImageUrl { url, prompt } => json!({
                "type": "image_url",
                "url": url,
                "prompt": prompt,
            }),
            ContentBlock::Json { value } => json!({ "type": "json", "value": value }),
        })
        .collect();
    json!({ "role": role, "content": content })
}

fn image_detail_label(
    detail: crate::magician_v2::slot_graph::extraction::ImageDetail,
) -> &'static str {
    match detail {
        crate::magician_v2::slot_graph::extraction::ImageDetail::Auto => "auto",
        crate::magician_v2::slot_graph::extraction::ImageDetail::Low => "low",
        crate::magician_v2::slot_graph::extraction::ImageDetail::High => "high",
    }
}

fn sanitize_projection_filename_component(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "outer_decision".to_string()
    } else {
        sanitized
    }
}

/// Build a [`CatalogBuildContext`] from the execution's [`AgenticContext`].
///
/// This is the single entry point for mapping execution-domain state into the
/// shape expected by [`build_execution_native_catalog`].
///
/// Browser no longer has a native lane here. It is now an inner-loop pack
/// tool exposed through `direct_capabilities`, so browser primitives stay
/// inside the inner-loop catalog instead of leaking into the outer catalog.
pub fn build_catalog_context(ctx: &AgenticContext) -> CatalogBuildContext {
    // Both modes carry the agent's umbrella capabilities here. Flat-mode
    // expansion to per-primitive `<pack>__<action>` leaves is owned by
    // `build_flat_loop_tools` (via the `ToolIndex`) at the decision seam —
    // `build_decision_tools_and_deferred` — so it stays a single, index-driven
    // source of truth instead of a partial embedded-only pre-expansion here.
    // Untrusted (stranger-facing) agents receive only the safe universal
    // subset, so their tools allowlist + denied_tools is the real boundary.
    // Compare against the canonical trust level so legacy/whitespace values
    // (e.g. " Untrusted ") are normalized first.
    let is_untrusted = trust_level_is_untrusted(ctx.trust_level.as_deref());
    let app_workflow_catalog = ctx.app_disclosure_guard.is_some()
        || ctx.merged_agent_tools.iter().any(|tool| {
            tool.name == crate::magician_v2::apps::workflows::APP_COMMIT_MUTATIONS_TOOL
        });
    let direct_capabilities = extract_direct_capabilities(
        &ctx.merged_agent_tools,
        &ctx.denied_capability_names,
        is_untrusted,
        !app_workflow_catalog
            && ctx.invocation_policy.discoverability
                != crate::magician_v2::agents::AgentDiscoverability::SurfaceOnly,
    );
    // Plane launch attenuation (Task 6b): a non-empty grant allowlist narrows
    // this run's usable surface to the intersection — least privilege must
    // survive the run_task hop. Denied families are already inside
    // `ctx.denied_capability_names` by this point, so they need no second
    // application here.
    let direct_capabilities = match ctx
        .plane_allowed_capability_names
        .as_ref()
        .filter(|allowed| !allowed.is_empty())
    {
        Some(allowed) => direct_capabilities
            .into_iter()
            .filter(|capability| {
                allowed.iter().any(|name| {
                    // Match the umbrella capability name OR any of its flat
                    // `<pack>__<action>` leaves: a grant allowlist names the
                    // tools the caller saw (`duckdb__query`), while this set
                    // carries `(name, description, schema)` umbrella tuples.
                    // Exact-only matching would narrow a leaf-named grant to
                    // an empty catalog.
                    name == &capability.0
                        || name
                            .strip_prefix(&capability.0)
                            .is_some_and(|rest| rest.starts_with("__"))
                })
            })
            .collect(),
        None => direct_capabilities,
    };
    // FIX #4: the agent's OWN granted capability names — i.e. exactly what its
    // YAML `tools:` block grants (`providing_agent_id.is_none()`), BEFORE the
    // universal-backend-pack injection inside `extract_direct_capabilities`.
    // This is the boundary the coordinator gate enforces against: a delegate-
    // only engineering-manager never lists `shell`/`write_file`/`edit_file`, so
    // those never appear here even though `direct_capabilities` re-adds them for
    // a trusted agent via the universal substrate.
    let granted_capability_names: std::collections::HashSet<String> = ctx
        .merged_agent_tools
        .iter()
        .filter(|t| t.providing_agent_id.is_none())
        .map(|t| t.name.clone())
        .collect();
    let delegate_only_grant_gate = CatalogAgentPolicy::from_execution(
        ctx.agent_id.as_deref(),
        coordinator_grant_gate(ctx, &granted_capability_names),
    );
    let mut denied_tool_names = ctx.denied_capability_names.clone();
    if is_untrusted
        && !denied_tool_names
            .iter()
            .any(|name| name.trim() == "orchestrator")
    {
        // Lower-trust faces do not advertise task/execution ownership
        // controls. Runtime trust enforcement remains the independent hard
        // boundary if an injected structural call reaches dispatch.
        denied_tool_names.push("orchestrator".to_string());
    }
    CatalogBuildContext {
        allowed_action_types: ctx.allowed_action_types.clone(),
        credentials_enabled: ctx.provisioned_secret_access_enabled,
        has_delegation_targets: !ctx.delegation_targets.is_empty(),
        direct_capabilities,
        // BUG-4 / BUG-8: autonomous outer-loop path. Keeps the common metadata
        // schema and skips the chat-only Phase 2.5 promoted tools.
        is_chat_mode: false,
        available_procedure_skills: ctx.available_procedure_skills.clone(),
        denied_tool_names,
        delegate_only_grant_gate,
    }
}

/// FIX #4 — VibeDev coordinator tool gate.
///
/// Returns `Some(granted_names)` ONLY when this run is a VibeDev coding-
/// **coordinator** run that must be tool-stripped: the canonical signal
/// `ctx.coding_coordinator_run` is set (a non-plan `vibedev` build, stamped
/// server-side — see the canonical-signal plumbing) AND the agent is genuinely
/// delegate-only (it can delegate and holds NO directly-granted work/mutation
/// or coding tool of its own) AND the kill-switch is not engaged. Otherwise
/// `None` → the catalog is built exactly as today.
///
/// Keying the gate on the agent's OWN grants is self-protecting: a real
/// engineer that holds `run_coding_task`/`shell`/`write_file` is never
/// delegate-only, so its catalog is untouched even if the coordinator signal
/// ever leaked into a delegated child context.
fn coordinator_grant_gate(
    ctx: &AgenticContext,
    granted_capability_names: &std::collections::HashSet<String>,
) -> Option<std::collections::HashSet<String>> {
    if !ctx.coding_coordinator_run || !coordinator_tool_gate_enabled() {
        return None;
    }
    // Delegate-only = can delegate AND owns no work/mutation/coding tool itself.
    let holds_work_tool = granted_capability_names.iter().any(|name| {
        matches!(
            name.as_str(),
            "shell"
                | "read_file"
                | "write_file"
                | "edit_file"
                | "grep"
                | "glob"
                | "http"
                | "run_coding_task"
                | "apply_code_proposal"
                | "run_project_checks"
        )
    });
    let is_delegate_only = !ctx.delegation_targets.is_empty() && !holds_work_tool;
    if !is_delegate_only {
        return None;
    }
    Some(granted_capability_names.clone())
}

/// Kill-switch for the FIX #4 coordinator tool gate. Default ON; force OFF with
/// `MAGICIAN_VIBEDEV_COORDINATOR_TOOL_GATE=0` (also accepts `false`/`no`/`off`).
fn coordinator_tool_gate_enabled() -> bool {
    !matches!(
        std::env::var("MAGICIAN_VIBEDEV_COORDINATOR_TOOL_GATE")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

/// Resolve the exact typed invocation carried by the active owner frame.
/// Product, chat, voice, delegation and handover entrypoints may install a
/// server-authenticated override. Ordinary autonomous/task runs derive their
/// surface from durable execution state. A mismatched override is an error,
/// never a reason to fall back to a wider surface.
pub fn invocation_context_for_agentic_decision(
    ctx: &AgenticContext,
    definition: &crate::magician_v2::agents::AgentDefinition,
) -> Result<crate::magician_v2::agents::AgentInvocationContext, String> {
    if let Some(invocation) = ctx.invocation_context_override.as_ref() {
        if invocation.target_agent_id != definition.agent_id
            || Some(invocation.principal.as_str()) != ctx.principal.as_deref()
            || Some(invocation.workspace.as_str()) != ctx.workspace.as_deref()
        {
            return Err("invocation_override_owner_scope_mismatch".to_string());
        }
        return Ok(invocation.clone());
    }

    let (surface, source_kind, source_agent_id) = if !ctx.owner_stack.is_empty() {
        (
            crate::magician_v2::agents::InvocationSurface::Handover,
            crate::magician_v2::agents::InvocationSourceKind::Handover,
            ctx.owner_stack.last().cloned(),
        )
    } else if ctx.depth > 0 {
        (
            crate::magician_v2::agents::InvocationSurface::Delegation,
            crate::magician_v2::agents::InvocationSourceKind::Delegated,
            None,
        )
    } else {
        (
            crate::magician_v2::agents::InvocationSurface::Task,
            crate::magician_v2::agents::InvocationSourceKind::Autonomous,
            None,
        )
    };
    Ok(crate::magician_v2::agents::AgentInvocationContext {
        principal: ctx.principal.clone().unwrap_or_default(),
        workspace: ctx.workspace.clone().unwrap_or_default(),
        source_agent_id,
        target_agent_id: definition.agent_id.clone(),
        surface,
        feature_mode: crate::magician_v2::agents::FeatureMode::None,
        source_kind,
        chat_session_id: None,
        chat_turn_id: None,
    })
}

/// Build the decision tool set + optional deferred-tools prompt block for one
/// LLM call (Phase 3b).
///
/// - **Flat** (with a `ToolIndex` on the context): the tools array is the *hot*
///   tier (full schemas) from [`build_flat_loop_tools`], and the returned block
///   lists every other reachable leaf as a bare name for `tool_search` to load
///   on demand.
/// - **Flat without an index**: degrade to the full native catalog and no block
///   (umbrella tools still dispatch via the inner-loop fall-through in
///   `execute_action`). The index is installed by `execute_agentically`, so
///   this is a defensive path only.
/// - **Primitive** (default): the full native catalog and no block —
///   byte-for-byte unchanged.
#[derive(Debug)]
struct AutonomousDecisionProjection {
    tools: Vec<super::native_types::NativeExecutionTool>,
    deferred: Vec<crate::magician_v2::execution::flat_loop::DeferredEntry>,
    permitted_loaded_tools: Vec<String>,
    snapshot: Option<std::sync::Arc<super::EffectiveToolPolicySnapshot>>,
}

fn resolve_autonomous_decision_projection(
    ctx: &AgenticContext,
    catalog_ctx: &CatalogBuildContext,
    current_iteration: Option<usize>,
    loaded_tools: &[String],
    engagement: Option<&super::policy_snapshot::EngagementSnapshotInput>,
) -> AutonomousDecisionProjection {
    // Apps have no implicit tool_search grant. Eagerly project their admitted
    // flat leaves and execution-local query/commit schemas. The native umbrella
    // catalog would replace primitive actions with empty handoff schemas.
    let (mut tools, mut deferred) = match ctx.tool_index.as_ref() {
        Some(index) if ctx.app_disclosure_guard.is_some() => (
            crate::magician_v2::execution::flat_loop::build_eager_flat_loop_tools(
                catalog_ctx,
                index,
            ),
            Vec::new(),
        ),
        Some(index) => {
            let catalog = crate::magician_v2::execution::flat_loop::build_flat_loop_tools(
                catalog_ctx,
                index,
                loaded_tools,
            );
            (catalog.hot, catalog.deferred)
        },
        None => (build_execution_native_catalog(catalog_ctx), Vec::new()),
    };
    // The flat catalog builds control tools independently, so apply source-agent
    // schema policy after either assembly path. This is idempotent for the full
    // catalog and closes the flat-loop Relay fan-out bypass.
    apply_catalog_context_to_tools(&mut tools, catalog_ctx);

    // Governed app-result tools require the direct local chat owner to carry a
    // non-serializable policy guard through the complete model continuation.
    // A scope-wide registry is not itself evidence that this autonomous/task
    // surface can own that contract, so remove the tools before policy snapshot,
    // working-set, and tool-search construction.
    let governed_surface =
        crate::magician_v2::execution::compiled_dispatch::GovernedAppToolSurface::AutonomousTask;
    tools.retain(|tool| {
        crate::magician_v2::execution::compiled_dispatch::compiled_tool_is_exposable_on_surface(
            &tool.name,
            governed_surface,
        )
    });
    deferred.retain(|tool| {
        crate::magician_v2::execution::compiled_dispatch::compiled_tool_is_exposable_on_surface(
            &tool.name,
            governed_surface,
        )
    });

    let mut permitted_loaded_tools = loaded_tools
        .iter()
        .filter(|tool| {
            crate::magician_v2::execution::compiled_dispatch::compiled_tool_is_exposable_on_surface(
                tool,
                governed_surface,
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut resolved_snapshot = None;
    if let Some(definition) = ctx.owner_definition.as_ref() {
        use std::collections::BTreeSet;

        let deferred_tool_names = deferred
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<BTreeSet<_>>();
        let direct_reachable_tool_names = tools
            .iter()
            .map(|tool| tool.name.clone())
            .chain(deferred_tool_names.iter().cloned())
            .collect::<BTreeSet<_>>();
        let mut delegate_owned_tool_names = BTreeSet::new();
        for tool in ctx
            .merged_agent_tools
            .iter()
            .filter(|tool| tool.providing_agent_id.is_some())
            .filter(|tool| {
                crate::magician_v2::execution::compiled_dispatch::compiled_tool_is_exposable_on_surface(
                    &tool.name,
                    governed_surface,
                )
            })
        {
            delegate_owned_tool_names.insert(tool.name.clone());
            if let Some(index) = ctx.tool_index.as_ref() {
                delegate_owned_tool_names.extend(index.leaf_names_for_pack(&tool.name));
            }
        }
        let delegation_target_ids = ctx
            .delegation_targets
            .iter()
            .filter(|target| {
                target.permits_surface(crate::magician_v2::agents::InvocationSurface::Delegation)
            })
            .map(|target| target.agent_id.clone())
            .collect::<Vec<_>>();
        let handover_target_ids = ctx
            .delegation_targets
            .iter()
            .filter(|target| {
                target.permits_surface(crate::magician_v2::agents::InvocationSurface::Handover)
            })
            .map(|target| target.agent_id.clone())
            .collect::<Vec<_>>();
        let invocation = match invocation_context_for_agentic_decision(ctx, definition) {
            Ok(invocation) => invocation,
            Err(error) => {
                tracing::warn!(
                    agent_id = %definition.agent_id,
                    error = %error,
                    "[TOOL-POLICY] authenticated invocation override does not match the active owner frame; failing closed"
                );
                tools.clear();
                deferred.clear();
                permitted_loaded_tools.clear();
                return AutonomousDecisionProjection {
                    tools,
                    deferred,
                    permitted_loaded_tools,
                    snapshot: None,
                };
            },
        };
        let external_policy_digest = match external_policy_digest_for_context(ctx) {
            Ok(digest) => digest,
            Err(error) => {
                tracing::warn!(
                    agent_id = %definition.agent_id,
                    error = %error,
                    "[TOOL-POLICY] trust policy digest resolution failed closed"
                );
                tools.clear();
                deferred.clear();
                permitted_loaded_tools.clear();
                return AutonomousDecisionProjection {
                    tools,
                    deferred,
                    permitted_loaded_tools,
                    snapshot: None,
                };
            },
        };
        let implicit_tool_names = if ctx.task_id.is_some()
            && direct_reachable_tool_names
                .contains(crate::magician_v2::execution::task_state_provider::TASK_STATE_TOOL_NAME)
        {
            BTreeSet::from([
                crate::magician_v2::execution::task_state_provider::TASK_STATE_TOOL_NAME
                    .to_string(),
            ])
        } else {
            BTreeSet::new()
        };
        let spawn_sub_goal_allowed = ctx.depth < ctx.max_delegation_depth as usize
            && current_iteration
                .map(|iteration| {
                    ctx.max_iterations
                        .saturating_sub(iteration)
                        .saturating_sub(1)
                        > 0
                })
                .unwrap_or(true);
        // Transfer the just-built catalog into policy resolution. The
        // resulting snapshot is cached and must retain its authorized specs;
        // the single clone below is therefore the provider-call
        // materialization, while cloning here as well duplicated the whole
        // expanded catalog before policy had filtered it.
        let provider_tools = std::mem::take(&mut tools);
        match super::policy_snapshot::resolve_effective_tool_policy_snapshot(
            definition,
            invocation,
            super::policy_snapshot::SnapshotResolutionInput {
                provider_tools,
                runtime_tool_names: Default::default(),
                deferred_tool_names,
                direct_reachable_tool_names,
                delegate_owned_tool_names,
                delegation_target_ids,
                handover_target_ids,
                implicit_tool_names,
                effective_approval_rules: Some(ctx.approval_rules.clone()),
                external_policy_digest: Some(external_policy_digest),
                spawn_sub_goal_allowed: Some(spawn_sub_goal_allowed),
                engagement: engagement.cloned(),
            },
        ) {
            Ok(snapshot) => {
                // Snapshot cache owns the authorization proof and schemas;
                // the adapter consumes a Vec for this physical request.
                tools = snapshot.provider_specs.clone();
                deferred.retain(|entry| snapshot.deferred_tools.contains_key(&entry.name));
                permitted_loaded_tools.retain(|name| snapshot.permits_tool(name));
                tracing::debug!(
                    snapshot_id = %snapshot.snapshot_id,
                    agent_id = %snapshot.agent_id,
                    surface = %snapshot.invocation.surface.as_str(),
                    provider_tool_count = snapshot.provider_specs.len(),
                    deferred_tool_count = snapshot.deferred_tools.len(),
                    delegation_target_count = snapshot.delegation_targets.len(),
                    "[TOOL-POLICY] resolved autonomous decision snapshot"
                );
                resolved_snapshot = Some(snapshot);
            },
            Err(error) => {
                tracing::warn!(
                    agent_id = %definition.agent_id,
                    error = %error,
                    "[TOOL-POLICY] autonomous snapshot resolution failed closed"
                );
                tools.clear();
                deferred.clear();
                permitted_loaded_tools.clear();
            },
        }
    }

    AutonomousDecisionProjection {
        tools,
        deferred,
        permitted_loaded_tools,
        snapshot: resolved_snapshot,
    }
}

fn autonomous_shared_runtime_is_active(ctx: &AgenticContext) -> bool {
    ctx.surface_working_sets.is_some() && ctx.surface_plan_cache.is_some()
}

fn build_autonomous_surface_plan(
    snapshot: &std::sync::Arc<super::EffectiveToolPolicySnapshot>,
    authority_revision: &str,
    registry_revision: &str,
    loaded_names: &std::collections::BTreeSet<String>,
    deferred: &[crate::magician_v2::execution::flat_loop::DeferredEntry],
) -> crate::magician_v2::execution::flat_loop::EffectiveSurfacePlan {
    let loaded_tools = snapshot
        .provider_specs
        .iter()
        .filter(|tool| loaded_names.contains(&tool.name))
        .cloned()
        .collect::<Vec<_>>();
    let initial_hot = snapshot
        .provider_specs
        .iter()
        .filter(|tool| !loaded_names.contains(&tool.name))
        .cloned()
        .collect::<Vec<_>>();
    let provider_names = snapshot
        .provider_specs
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    crate::magician_v2::execution::flat_loop::EffectiveSurfacePlan {
        authority_revision: authority_revision.to_string(),
        registry_revision: registry_revision.to_string(),
        invocation_surface: snapshot.invocation.surface,
        feature_mode: snapshot.invocation.feature_mode,
        initial_hot,
        loaded_tools,
        deferred: deferred
            .iter()
            .filter(|entry| snapshot.deferred_tools.contains_key(&entry.name))
            .filter(|entry| !provider_names.contains(entry.name.as_str()))
            .cloned()
            .collect(),
        runtime_tool_names: snapshot.runtime_tools.keys().cloned().collect(),
        structural_tool_names: snapshot.structural_tools.keys().cloned().collect(),
        authorized_business_tool_names: snapshot
            .direct_tools
            .keys()
            .chain(snapshot.deferred_tools.keys())
            .cloned()
            .collect(),
        provider_schema_bytes: crate::magician_v2::execution::flat_loop::provider_schema_bytes(
            &snapshot.provider_specs,
        ),
        built_at_ms: 0,
    }
}

/// Resolve the engagement input for one provider decision (§4.2c layer 1).
///
/// Reads the sealed [`crate::magician_v2::engagements::EngagementAuthorityRef`]
/// the execution carries and asks the live store for CURRENT authority — the
/// snapshot narrows to the store's revision, not the carried one, so a narrow
/// that happened after launch is reflected in the very next projection (row 9).
///
/// The store read is async while the projection itself is sync
/// (`resolve_autonomous_decision_projection` and its callers up to
/// `build_decision_tools_and_deferred`), so the async decision entrypoints
/// resolve this once, immediately before building the catalog, and pass the
/// result down.
///
/// Returns:
/// - `None` — the execution carries no engagement; no narrowing applies.
/// - `Some(live)` — the current ceiling/team at the store's revision.
/// - `Some(empty)` — FAIL CLOSED: the store denied (unknown / revoked /
///   expired) or is not installed. The projection then narrows to an empty
///   ceiling and empty team — no engagement-scoped business tools, no
///   delegation targets — rather than falling back to the un-narrowed static
///   grant. Dispatch enforcement (layer 2) re-checks live authority
///   independently; this projection-level narrowing is UX, not the boundary.
async fn resolve_engagement_snapshot_input(
    ctx: &AgenticContext,
) -> Option<super::policy_snapshot::EngagementSnapshotInput> {
    // `None` for a program is the true answer, not a narrowing: this resolves an
    // ENGAGEMENT snapshot, and a program has no roster to snapshot. Dispatch
    // enforcement is unaffected — it reads the carrier directly.
    let carried = ctx.engagement_ceiling_authority()?;
    let carried = &carried;
    let principal = ctx.principal.clone().unwrap_or_default();
    let workspace = ctx.workspace.clone().unwrap_or_default();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let denial = match crate::magician_v2::engagements::global_engagement_store() {
        Some(store) => {
            match store
                .live_authority(&principal, &workspace, &carried.engagement_id, now_ms)
                .await
            {
                Ok(live) => {
                    return Some(super::policy_snapshot::EngagementSnapshotInput {
                        engagement_id: live.engagement_id,
                        // The LIVE revision, not the carried one: the snapshot
                        // records the authority it actually narrowed to.
                        authority_revision: live.authority_revision,
                        tool_ceiling: live.tool_ceiling,
                        team: live.team,
                    });
                },
                Err(denial) => denial,
            }
        },
        None => crate::magician_v2::engagements::AuthorityDenial::StoreUnavailable {
            detail: "no engagement store installed in this process".to_string(),
        },
    };
    tracing::warn!(
        engagement_id = %carried.engagement_id,
        carried_revision = carried.authority_revision,
        denial = ?denial,
        "[TOOL-POLICY] engagement live-authority resolution failed; projecting an empty engagement ceiling (fail closed)"
    );
    Some(super::policy_snapshot::EngagementSnapshotInput {
        engagement_id: carried.engagement_id.clone(),
        authority_revision: carried.authority_revision,
        tool_ceiling: Default::default(),
        team: Default::default(),
    })
}

/// The planner uses the run's exact profile overrides, task lineage and budget.
pub(crate) fn decision_rail_adapter(
    ctx: &AgenticContext,
    executors: &super::ActionExecutors,
    iteration: usize,
) -> MultiLlmAgentAdapter {
    let adapter = executors
        .native_adapter
        .with_routing_overrides(Some(super::executor::routing_overrides_for_run(ctx)));
    super::executor::llm_task_ref_for_context(ctx)
        .map(|task| {
            adapter.with_task_context(
                task.with_iteration(super::executor::agentic_iteration_id(ctx, iteration)),
            )
        })
        .unwrap_or(adapter)
}

/// The same live authority projection for native models and the Decision Engine.
pub(crate) async fn decision_rail_catalog(
    ctx: &AgenticContext,
    iteration: usize,
) -> (
    Vec<super::native_types::NativeExecutionTool>,
    Option<String>,
) {
    let catalog = build_catalog_context(ctx);
    let engagement = resolve_engagement_snapshot_input(ctx).await;
    build_decision_tools_and_deferred(ctx, &catalog, Some(iteration), engagement)
}

fn build_decision_tools_and_deferred(
    ctx: &AgenticContext,
    catalog_ctx: &CatalogBuildContext,
    current_iteration: Option<usize>,
    engagement: Option<super::policy_snapshot::EngagementSnapshotInput>,
) -> (
    Vec<super::native_types::NativeExecutionTool>,
    Option<String>,
) {
    // Production tasks restore the current owner-frame family set before
    // building the provider catalog. Detached unit harnesses that install no
    // shared runtime stores retain their explicitly supplied local set.
    let mut loaded_tools = snapshot_loaded_tools(ctx);
    if autonomous_shared_runtime_is_active(ctx) {
        let binding = ctx
            .scratch
            .autonomous_surface_binding
            .lock()
            .ok()
            .and_then(|binding| binding.clone());
        if let (Some(binding), Some(store)) = (binding, ctx.surface_working_sets.as_ref()) {
            loaded_tools = store
                .reconcile(
                    &binding.key,
                    &binding.authority_revision,
                    &binding.authorized_tool_names,
                )
                .loaded_tools;
            if let Ok(mut local) = ctx.scratch.loaded_tools.lock() {
                *local = loaded_tools.iter().cloned().collect();
            }
        }
    }

    if let Ok(mut snapshot_id) = ctx.policy_snapshot_id.lock() {
        *snapshot_id = None;
    }
    if let Ok(mut dispatch_names) = ctx.policy_dispatch_tool_names.lock() {
        *dispatch_names = None;
    }
    if let Ok(mut implicit_names) = ctx.policy_implicit_tool_names.lock() {
        *implicit_names = None;
    }

    let mut projection = resolve_autonomous_decision_projection(
        ctx,
        catalog_ctx,
        current_iteration,
        &loaded_tools,
        engagement.as_ref(),
    );

    if autonomous_shared_runtime_is_active(ctx) {
        let snapshot = projection.snapshot.clone();
        let registry_revision = ctx.scope_capability_revision.clone();
        match (snapshot, registry_revision) {
            (Some(snapshot), Some(registry_revision)) => {
                match crate::magician_v2::execution::flat_loop::autonomous_authority_revision(
                    &registry_revision,
                    &snapshot,
                ) {
                    Ok(authority_revision) => {
                        let authorized_names = snapshot
                            .direct_tools
                            .keys()
                            .chain(snapshot.deferred_tools.keys())
                            .cloned()
                            .collect::<std::collections::HashSet<_>>();
                        let binding_id = ctx
                            .execution_id
                            .as_deref()
                            .or(ctx.legacy_execution_id.as_deref())
                            .or(ctx.task_id.as_deref())
                            .map(str::to_string);
                        let Some(binding_id) = binding_id else {
                            tracing::warn!(
                                agent_id = %snapshot.agent_id,
                                "[SURFACE-RUNTIME] autonomous frame has no stable execution/task binding; failing this decision closed"
                            );
                            projection.tools.clear();
                            projection.deferred.clear();
                            projection.permitted_loaded_tools.clear();
                            if let Ok(mut dispatch_names) = ctx.policy_dispatch_tool_names.lock() {
                                *dispatch_names = Some(Default::default());
                            }
                            if let Ok(mut implicit_names) = ctx.policy_implicit_tool_names.lock() {
                                *implicit_names = Some(Default::default());
                            }
                            return (projection.tools, None);
                        };
                        let key =
                            crate::magician_v2::execution::flat_loop::SurfaceWorkingSetKey::new(
                                &snapshot.invocation.principal,
                                &snapshot.invocation.workspace,
                                &snapshot.agent_id,
                                snapshot.invocation.surface,
                                snapshot.invocation.feature_mode,
                                &binding_id,
                            );

                        // A changed authority revision clears L3 before this provider
                        // request. Rebuild once if reconciliation changed the loaded set.
                        let store = ctx
                            .surface_working_sets
                            .as_ref()
                            .expect("active shared runtime has a working-set store");
                        let reconciled =
                            store.reconcile(&key, &authority_revision, &authorized_names);
                        if reconciled.loaded_tools != loaded_tools {
                            loaded_tools = reconciled.loaded_tools;
                            if let Ok(mut local) = ctx.scratch.loaded_tools.lock() {
                                *local = loaded_tools.iter().cloned().collect();
                            }
                            projection = resolve_autonomous_decision_projection(
                                ctx,
                                catalog_ctx,
                                current_iteration,
                                &loaded_tools,
                                engagement.as_ref(),
                            );
                        }
                        if let Ok(mut binding) = ctx.scratch.autonomous_surface_binding.lock() {
                            *binding = Some(super::types::AutonomousSurfaceRuntimeBinding {
                                key: key.clone(),
                                authority_revision: authority_revision.clone(),
                                authorized_tool_names: authorized_names.clone(),
                            });
                        }

                        if let Some(final_snapshot) = projection.snapshot.as_ref() {
                            let loaded_names = loaded_tools.iter().cloned().collect();
                            let candidate = build_autonomous_surface_plan(
                                final_snapshot,
                                &authority_revision,
                                &registry_revision,
                                &loaded_names,
                                &projection.deferred,
                            );
                            let parity = candidate.parity_report();
                            let key = crate::magician_v2::execution::flat_loop::SurfacePlanKey {
                                principal: final_snapshot.invocation.principal.clone(),
                                workspace: final_snapshot.invocation.workspace.clone(),
                                agent_id: final_snapshot.agent_id.clone(),
                                surface: final_snapshot.invocation.surface,
                                feature_mode: final_snapshot.invocation.feature_mode,
                                authority_revision: authority_revision.clone(),
                            };
                            let candidate = if loaded_names.is_empty() {
                                ctx.surface_plan_cache
                                    .as_ref()
                                    .and_then(|cache| cache.get(&key))
                                    .or_else(|| {
                                        ctx.surface_plan_cache
                                            .as_ref()
                                            .map(|cache| cache.insert(key, candidate.clone()))
                                    })
                                    .expect("active shared runtime has a surface-plan cache")
                            } else {
                                std::sync::Arc::new(candidate)
                            };
                            tracing::debug!(
                                agent_id = %final_snapshot.agent_id,
                                surface = %final_snapshot.invocation.surface.as_str(),
                                authority_revision = %authority_revision,
                                exact_business_universe = parity.exact_business_universe,
                                provider_schema_bytes = candidate.provider_schema_bytes,
                                "[SURFACE-RUNTIME] autonomous plan resolved"
                            );
                            if parity.is_exact() {
                                projection.tools = candidate.provider_tools();
                            } else {
                                tracing::warn!(
                                    agent_id = %final_snapshot.agent_id,
                                    "[SURFACE-RUNTIME] autonomous shared projection mismatch; failing this decision closed"
                                );
                                projection.tools.clear();
                                projection.deferred.clear();
                                projection.permitted_loaded_tools.clear();
                            }
                        }
                    },
                    Err(error) => {
                        tracing::warn!(
                            agent_id = %snapshot.agent_id,
                            error = %error,
                            "[SURFACE-RUNTIME] autonomous authority revision failed; failing this decision closed"
                        );
                        projection.tools.clear();
                        projection.deferred.clear();
                        projection.permitted_loaded_tools.clear();
                    },
                }
            },
            (Some(snapshot), None) => {
                tracing::warn!(
                    agent_id = %snapshot.agent_id,
                    "[SURFACE-RUNTIME] scoped capability revision is unavailable; failing this decision closed"
                );
                projection.tools.clear();
                projection.deferred.clear();
                projection.permitted_loaded_tools.clear();
            },
            (None, _) => {},
        }
    }

    if let Some(snapshot) = projection.snapshot.as_ref() {
        if let Ok(mut snapshot_id) = ctx.policy_snapshot_id.lock() {
            *snapshot_id = Some(snapshot.snapshot_id.clone());
        }
        if let Ok(mut current) = ctx.policy_snapshot.lock() {
            *current = Some(std::sync::Arc::clone(snapshot));
        }
        if let Ok(mut dispatch_names) = ctx.policy_dispatch_tool_names.lock() {
            *dispatch_names = Some(snapshot.dispatch_tool_names.clone());
        }
        if let Ok(mut implicit_names) = ctx.policy_implicit_tool_names.lock() {
            *implicit_names = Some(snapshot.implicit_tools.clone());
        }
    } else {
        // No owner-scoped snapshot exists for detached/legacy execution. Keep
        // the policy slots absent so dispatch falls through to the legacy
        // allowlist checks below. `Some(empty)` means an authoritative snapshot
        // with no callable capabilities and must be reserved for an actual
        // fail-closed policy result; using it here made the documented native
        // catalog fallback advertise structural controls that dispatch then
        // rejected forever.
        if let Ok(mut dispatch_names) = ctx.policy_dispatch_tool_names.lock() {
            *dispatch_names = None;
        }
        if let Ok(mut implicit_names) = ctx.policy_implicit_tool_names.lock() {
            *implicit_names = None;
        }
    }

    let mut deferred_block = render_deferred_block(&projection.deferred);
    // Inner-loop replacement: once the agent loads a pack's tools, fold that
    // pack's guide into the prompt, but only after the snapshot confirms the
    // loaded names remain authorized for this decision boundary.
    if let Some(index) = ctx.tool_index.as_ref() {
        let guides = render_loaded_pack_guides(&projection.permitted_loaded_tools, index);
        if !guides.is_empty() {
            if !deferred_block.is_empty() {
                deferred_block.push('\n');
            }
            deferred_block.push_str(&guides);
        }
    }
    (
        projection.tools,
        (!deferred_block.is_empty()).then_some(deferred_block),
    )
}

/// Build the system-prompt guidance section for packs whose tools the agent has
/// loaded this run: each such pack's full `guide:` text, deduped by pack. This
/// is the flat-loop stand-in for the per-pack inner-loop system prompt — the
/// agent gets the pack's complete playbook (notably the browser
/// iframe/shadow/cross-origin/coordinate-drag rules) the moment it commits to
/// using that pack, not just a 400-char catalog excerpt.
fn render_loaded_pack_guides(
    loaded_tools: &[String],
    index: &crate::magician_v2::execution::flat_loop::ToolIndex,
) -> String {
    let mut seen_packs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = String::new();
    for name in loaded_tools {
        let Some(entry) = index.get(name) else {
            continue;
        };
        if !seen_packs.insert(entry.pack_name.clone()) {
            continue;
        }
        let Some(guide) = index.guide_for_pack(&entry.pack_name) else {
            continue;
        };
        if out.is_empty() {
            out.push_str(
                "## LOADED TOOL GUIDES\n\
                 Full playbooks for the tool packs you've loaded this run. Follow them — they \
                 cover the non-obvious handling (e.g. iframes, shadow DOM, cross-origin, \
                 coordinate-based drag) that the per-tool schemas don't.\n",
            );
        }
        out.push_str(&format!("\n### `{}` guide\n{}\n", entry.pack_name, guide));
    }
    out
}

/// Snapshot the deferred tools the agent has loaded via `tool_search` this
/// execution (see `AgenticContext.scratch.loaded_tools`). Returned as a plain `Vec` so
/// the lock isn't held across catalog construction.
pub fn snapshot_loaded_tools(ctx: &AgenticContext) -> Vec<String> {
    ctx.scratch
        .loaded_tools
        .lock()
        .map(|set| set.iter().cloned().collect())
        .unwrap_or_default()
}

/// Render the deferred-tools block appended to the system prompt in flat mode.
/// Returns an empty string when there is nothing deferred (no block emitted).
fn render_deferred_block(
    deferred: &[crate::magician_v2::execution::flat_loop::DeferredEntry],
) -> String {
    if deferred.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("## DEFERRED TOOLS (load with tool_search, then call directly)\n");
    out.push_str(
        "These tools are reachable but not in your active tool list yet, so you cannot call them \
         directly until you load them. To use one:\n\
         1. Call `tool_search` with `query: \"select:<name>\"` (comma-separate to load several at \
         once; or pass a keyword to search first). This returns the tool's schema AND adds it to \
         your active tools.\n\
         2. On your NEXT turn the loaded tool is a normal callable tool — just call it like any \
         other (e.g. `tool_search` or `yield`), with its arguments. Do not try to call a deferred \
         tool before loading it; it won't be available until the turn after `tool_search`.\n",
    );
    let mut rendered_hints = std::collections::HashSet::new();
    for entry in deferred {
        match &entry.search_hint {
            Some(hint) if rendered_hints.insert(hint.as_str()) => {
                out.push_str(&format!("- {} [{}] — {}\n", entry.name, entry.group, hint));
            },
            None => out.push_str(&format!("- {} [{}]\n", entry.name, entry.group)),
            Some(_) => out.push_str(&format!("- {} [{}]\n", entry.name, entry.group)),
        }
    }
    out
}

/// Names of compiled packs that are always injected into every
/// agent's catalog regardless of YAML allowlist — the universal
/// backend substrate. These are the canonical agent-backend tools
/// (defined in `embedded_pack_defs/`); listing them here makes the
/// "chat is just a face; the agent has access to everything" contract
/// concrete without adding duplicate native tool surfaces.
///
/// Categories included:
/// - **Memory** — scoped tier reads + writes (with the two-phase
///   confirm-token forget flow).
/// - **Read-only introspection** — every agent can query its own
///   environment.
/// - **Task management on existing tasks** (`stop_task`, `update_task`,
///   `delete_task`, `refine_task`) — not face-coupled; the agent can
///   manage tasks it owns from any execution context.
/// - **Workspace artifacts** (`create_dashboard`, `unpublish_dashboard`)
///   — dashboards are principal/workspace-scoped artifacts, not
///   chat-scoped. Any agent (chat-spawned or autonomous) can publish.
///
/// Explicitly NOT included (still require per-agent allowlist grants):
/// - **Face-coupled task ops** (`create_task`, `run_task`) — these
///   need a chat session for progress fan-out / live subscription.
///   Autonomous-to-autonomous work uses `delegate_to_agent` /
///   `spawn_sub_goal` instead. (`subscribe_to_task` moved to the
///   chat-runtime tools as `subscribe_to_task_for_chat`.)
/// - **Agent admin** (`create_agent`, `update_agent`, `retire_agent`,
///   `reassign_task`)
/// - **External side effects** (`notify_owner`, `create_proposal`)
/// - **Admin batch** (`evaluate_harness`, `catchup_merge`, `treasurer`,
///   `update_delegation`, `update_program_state`)
///
/// **`switch_personality`** — also belongs in this universal set
/// conceptually (the agent's identity layer should be swappable from
/// any face), but currently exists only as a `ChatRuntimeTool` with no
/// compiled-pack counterpart. Promoting it is the next follow-up slice.
pub const UNIVERSAL_BACKEND_PACKS: &[&str] = &[
    // Memory.
    "save_preference",
    "update_memory_tier",
    "search_memory",
    "forget_memory",
    "list_memory_tiers",
    // Read-only introspection.
    "list_tasks",
    "list_agents",
    "inspect_agent",
    "get_agent_details",
    "find_agents_for_capability",
    "list_artifacts",
    "list_scheduled_tasks",
    "list_episodes",
    "list_proposals",
    "get_active_executions",
    "get_execution_history",
    "read_program_state",
    "read_trace",
    "system_status",
    "time_math",
    // Task management on existing tasks (non-face-coupled).
    "stop_task",
    "update_task",
    "delete_task",
    "refine_task",
    // Workspace artifacts (principal/workspace-scoped, not chat-scoped).
    "create_dashboard",
    "unpublish_dashboard",
    // Agent identity — `switch_personality` writes to the agent-scope
    // `personality_profile` memory tier and persists across all
    // future runs. Promoted from chat-only `ChatRuntimeTool` so any
    // agent (autonomous, scheduled, chat-spawned) can swap personas.
    "switch_personality",
    // Procedure-skill activation — `activate_skill` / `deactivate_skill`
    // write to the agent-scope `active_procedure_skill` memory tier.
    // Phase 0.8c-6: routes through the same compiled-handler rail as
    // every other migrated tool (no special Decision variant).
    "activate_skill",
    "deactivate_skill",
    // Filesystem / search / meta universals — migrated from the
    // `Decision::ChatControl` envelope to the generic compiled-handler
    // rail (Phase 0.8c-10). Chat surface gets them too via the
    // universal pack mechanism — no `is_chat_mode` gate needed.
    "tool_search",
    "edit_file",
    "glob",
    "grep",
    // Provider-neutral public acquisition. These enforce configured action
    // order, scope, authority, quality, cost, and deadline. Provider-specific
    // primitives below remain direct debugging/escape hatches.
    "content_search",
    "content_read",
    // Legacy direct web primitives.
    "web_fetch",
    "web_search",
    // Provider-grounded one-shot answer lane (billed per search; counted
    // spend gate declared in the pack def).
    "web_answer",
    // Rail A built-in lanes promoted to universal-pack rail
    // (Phase 0.8c-11). `files` is the multi-action filesystem tool
    // (replaces native `file`); `http` does credentialed HTTP
    // requests; `shell` runs bash with the runtime toolbelt
    // (replaces native `bash`, with `stdin` parity). All three use
    // the existing struct-based compiled providers
    // (`FileCapabilityProvider`, `HttpCapabilityProvider`,
    // `ShellCapabilityProvider`) for execution — no GenericCompiledProvider
    // wiring needed.
    "files",
    "http",
    "shell",
    // Granular filesystem readers / writers — Phase 0.8c-11 batch 2.
    // Same `FileAction` dispatcher as the multi-action `files` pack;
    // exposed as named single-purpose tools for clearer LLM
    // ergonomics. Sandbox config comes from
    // `AgentResources.file_sandbox`.
    "read_file",
    "write_file",
];

/// Universal packs an UNTRUSTED (stranger-facing) agent may still receive.
///
/// Everything else in [`UNIVERSAL_BACKEND_PACKS`] is withheld from untrusted
/// agents (`trust_level == "untrusted"`), so their `tools` allowlist +
/// `denied_tools` is the real capability boundary (fail-closed). Without this,
/// an untrusted agent silently inherits shell/http/files/read_file/write_file/
/// web/task-mutation/cross-agent-introspection/memory-write regardless of its
/// allowlist. New universal packs default to EXCLUDED for untrusted agents —
/// add an entry here only after confirming the pack is safe for a
/// stranger-facing face.
pub const SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED: &[&str] =
    &["activate_skill", "deactivate_skill", "time_math"];

/// Whether `name` is part of the universal substrate every trusted agent
/// receives at run time regardless of its YAML allowlist.
///
/// Callers use this to tell an *ambient* capability from an allowlisted one.
/// The distinction matters wherever a delegate could displace an owner's tool:
/// a delegate that lists `read_file` is not a specialist for it, because the
/// owner runs `read_file` natively too.
pub fn is_universal_backend_pack(name: &str) -> bool {
    UNIVERSAL_BACKEND_PACKS.contains(&name)
}

/// Whether an agent's `trust_level` marks it untrusted (stranger-facing).
///
/// Untrusted agents receive only [`SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED`] from the
/// universal rail. This MUST be applied in BOTH the offered catalog
/// (`extract_direct_capabilities`) AND the dispatch ceiling
/// (`compute_dispatch_tool_scope`) so the two stay in lockstep — narrowing only
/// the catalog leaves the enforcement ceiling open, so an injected/hallucinated
/// tool_use for a withheld pack would still dispatch. The canonicalization
/// normalizes legacy/whitespace values (e.g. " Untrusted ").
pub fn trust_level_is_untrusted(trust_level: Option<&str>) -> bool {
    trust_level.is_some_and(|level| {
        crate::magician_v2::agents::types::TrustLevel(level.to_string()).is_untrusted()
    })
}

/// Convert a [`CapabilityPackDefinition`] into the
/// `(name, description, parameter_schema)` triple that
/// [`CatalogBuildContext::direct_capabilities`] expects.
fn pack_def_to_direct_capability(
    def: &crate::magician_v2::execution::capability::CapabilityPackDefinition,
) -> (String, String, Value) {
    use crate::magician_v2::execution::capability::derive_param_schema_for_emission;
    let mut properties = serde_json::Map::new();
    let mut required: Vec<Value> = Vec::new();
    for param in &def.parameters {
        properties.insert(param.name.clone(), derive_param_schema_for_emission(param));
        if param.required {
            required.push(Value::String(param.name.clone()));
        }
    }
    let schema = serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    });
    (
        def.name.clone(),
        def.description.clone().unwrap_or_default(),
        schema,
    )
}

/// Extract direct pack capabilities as `(name, description, parameter_schema)`.
///
/// Sources:
/// 1. The agent's allowlist (`tools` arg) — every non-delegate-owned,
///    non-reserved entry contributes a capability.
/// 2. The universal backend packs ([`UNIVERSAL_BACKEND_PACKS`]) —
///    injected so every agent has memory + task ops + read-only
///    introspection access regardless of its YAML allowlist. Skipped
///    when the agent already listed them (so we don't double-add).
///
/// Trust narrowing: when `is_untrusted` is set (the agent's
/// `trust_level == "untrusted"`, e.g. a stranger-facing envoy), only the
/// [`SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED`] subset is injected. The full
/// [`UNIVERSAL_BACKEND_PACKS`] set (shell/http/files/web/task-mutation/
/// cross-agent-introspection/memory-write) is withheld, so the agent's
/// `tools` allowlist + `denied_tools` becomes the real capability boundary
/// (fail-closed). Trusted agents are unchanged.
///
/// Filters out:
/// - Tools owned by delegate agents (`providing_agent_id.is_some()`)
/// - Built-in action-type tools (file, http, bash, etc.)
fn extract_direct_capabilities(
    tools: &[runtime_core::ToolInfo],
    denied_capability_names: &[String],
    is_untrusted: bool,
    include_universal_packs: bool,
) -> Vec<(String, String, Value)> {
    let is_denied = |name: &str| {
        denied_capability_names.iter().any(|denied| {
            crate::magician_v2::agents::types::tool_name_matches_block_entry(name, denied)
        })
    };
    let mut result: Vec<(String, String, Value)> = tools
        .iter()
        .filter(|t| t.providing_agent_id.is_none())
        .filter(|t| !is_reserved_non_pack_capability_name(&t.name))
        // Honor the agent's whole-tool deny set for explicit tools too
        // (defense-in-depth; merged_agent_tools is already deny-filtered).
        .filter(|t| !is_denied(&t.name))
        .map(|t| {
            let schema = parameters_to_json_schema(&t.parameters);
            (t.name.clone(), t.description.clone(), schema)
        })
        .collect();

    // Untrusted (stranger-facing) agents only receive the safe universal
    // subset; trusted agents receive the full universal pack set. This is what
    // makes the `tools` allowlist + `denied_tools` the real boundary for an
    // untrusted face (fail-closed).
    let universal_packs: &[&str] = if !include_universal_packs {
        &[]
    } else if is_untrusted {
        SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED
    } else {
        UNIVERSAL_BACKEND_PACKS
    };

    let embedded =
        crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs_ref();
    for pack_name in universal_packs {
        // Strip universal packs the agent denies (e.g. a locked-down agent that
        // lists `shell` in excluded_tools/denied_tools). Without this, universal
        // packs would be injected regardless of the deny list.
        if is_denied(pack_name) {
            continue;
        }
        if result.iter().any(|(n, _, _)| n == pack_name) {
            continue;
        }
        if let Some(def) = embedded.iter().find(|p| p.name == *pack_name) {
            result.push(pack_def_to_direct_capability(def));
        }
    }
    result
}

/// Convert a slice of [`ParameterDefinition`] into a JSON Schema object.
///
/// Produces `{"type":"object","properties":{...},"required":[...]}`. Uses
/// each parameter's canonical `schema` field as the property value when set
/// (the standard case after pack YAML → `runtime_core::ParameterDefinition`
/// conversion). Falls back to legacy primitive-type synthesis from
/// `param_type`/`enum_values`/`description`/`default_value` only when the
/// upstream populated `schema: Value::Null` (rare — happens for in-memory
/// fixtures or non-pack-derived ParameterDefinition values).
fn parameters_to_json_schema(params: &[runtime_core::ParameterDefinition]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for param in params {
        let prop = if !param.schema.is_null() {
            param.schema.clone()
        } else {
            let mut p = serde_json::Map::new();
            p.insert("type".to_string(), json!(param.param_type));
            if !param.description.is_empty() {
                p.insert("description".to_string(), json!(param.description));
            }
            if let Some(ref enum_values) = param.enum_values {
                p.insert("enum".to_string(), json!(enum_values));
            }
            if let Some(ref default) = param.default_value {
                p.insert("default".to_string(), default.clone());
            }
            Value::Object(p)
        };
        properties.insert(param.name.clone(), prop);
        if param.required {
            required.push(param.name.clone());
        }
    }

    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use runtime_core::{ParameterDefinition, ToolInfo};

    fn invocation_definition(agent_id: &str) -> crate::magician_v2::agents::AgentDefinition {
        crate::magician_v2::agents::AgentDefinition::from_yaml_str(&format!(
            r#"
agent_id: "{agent_id}"
name: "Invocation test"
persona: "Test"
kind: worker
principal: "owner"
workspace: "default"
tools: []
"#,
        ))
        .expect("invocation definition")
    }

    #[test]
    fn agentic_invocation_derives_task_and_handover_surfaces_from_owner_frame() {
        let definition = invocation_definition("worker");
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.principal = Some("owner".to_string());
        ctx.workspace = Some("default".to_string());

        let task = invocation_context_for_agentic_decision(&ctx, &definition).unwrap();
        assert_eq!(
            task.surface,
            crate::magician_v2::agents::InvocationSurface::Task
        );

        ctx.owner_stack = vec!["personal-assistant".to_string()];
        let handover = invocation_context_for_agentic_decision(&ctx, &definition).unwrap();
        assert_eq!(
            handover.surface,
            crate::magician_v2::agents::InvocationSurface::Handover
        );
        assert_eq!(
            handover.source_agent_id.as_deref(),
            Some("personal-assistant")
        );
    }

    #[test]
    fn agentic_invocation_override_is_exact_and_mismatch_fails_closed() {
        let definition = invocation_definition("brainstorm-facilitator");
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.principal = Some("owner".to_string());
        ctx.workspace = Some("default".to_string());
        ctx.invocation_context_override =
            Some(crate::magician_v2::agents::AgentInvocationContext {
                principal: "owner".to_string(),
                workspace: "default".to_string(),
                source_agent_id: None,
                target_agent_id: "brainstorm-facilitator".to_string(),
                surface: crate::magician_v2::agents::InvocationSurface::ThinkingMap,
                feature_mode: crate::magician_v2::agents::FeatureMode::Brainstorm,
                source_kind: crate::magician_v2::agents::InvocationSourceKind::ProductFeature,
                chat_session_id: Some("map-session".to_string()),
                chat_turn_id: Some("map-turn".to_string()),
            });

        let exact = invocation_context_for_agentic_decision(&ctx, &definition).unwrap();
        assert_eq!(
            exact.surface,
            crate::magician_v2::agents::InvocationSurface::ThinkingMap
        );

        ctx.workspace = Some("other".to_string());
        assert_eq!(
            invocation_context_for_agentic_decision(&ctx, &definition).unwrap_err(),
            "invocation_override_owner_scope_mismatch"
        );
    }

    #[test]
    fn same_owner_subgoal_preserves_the_parent_surface_instead_of_becoming_delegation() {
        let definition = invocation_definition("worker");
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.principal = Some("owner".to_string());
        ctx.workspace = Some("default".to_string());
        let parent = invocation_context_for_agentic_decision(&ctx, &definition).unwrap();
        assert_eq!(
            parent.surface,
            crate::magician_v2::agents::InvocationSurface::Task
        );

        // SpawnSubGoal materializes this exact parent invocation before it
        // increments depth. A deeper frame must therefore remain Task rather
        // than being misclassified as a cross-agent delegation.
        ctx.invocation_context_override = Some(parent);
        ctx.depth = 1;
        let child = invocation_context_for_agentic_decision(&ctx, &definition).unwrap();
        assert_eq!(
            child.surface,
            crate::magician_v2::agents::InvocationSurface::Task
        );
        assert_eq!(
            child.source_kind,
            crate::magician_v2::agents::InvocationSourceKind::Autonomous
        );
    }

    #[test]
    fn native_tool_instruction_is_positive_schema_authoritative_and_semantically_complete() {
        assert!(NATIVE_TOOL_INSTRUCTION.contains("provider-native tool-calling channel"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("at least one tool call"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("provided catalog"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("matching each tool's schema"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("multiple non-terminal tool calls"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("order listed"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("actions before"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("`yield`"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("`need_user_input`"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("place nothing after them"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("tool-backed evidence"));
        assert!(NATIVE_TOOL_INSTRUCTION.contains("actionable work remains"));

        // The runtime directive must not carry a competing response envelope,
        // correction language, or context-dependent hard-coded tool examples.
        assert!(!NATIVE_TOOL_INSTRUCTION.contains("action_type"));
        assert!(!NATIVE_TOOL_INSTRUCTION.contains("capability_name"));
        assert!(!NATIVE_TOOL_INSTRUCTION.contains("raw JSON"));
        assert!(!NATIVE_TOOL_INSTRUCTION.contains("shapes you see"));
        assert!(!NATIVE_TOOL_INSTRUCTION.contains("browser__open"));
        assert!(!NATIVE_TOOL_INSTRUCTION.contains("read_file"));
        assert!(!NATIVE_TOOL_INSTRUCTION.contains("goal_reached"));
        assert!(!NATIVE_TOOL_INSTRUCTION.contains("cannot_proceed"));
    }

    #[test]
    fn chat_native_tool_instruction_preserves_chat_specific_contract() {
        assert!(CHAT_NATIVE_TOOL_INSTRUCTION.contains("exactly ONE tool"));
        assert!(CHAT_NATIVE_TOOL_INSTRUCTION.contains("respond with assistant text"));
        assert!(!CHAT_NATIVE_TOOL_INSTRUCTION.contains("at least one tool call"));
    }

    fn make_custom_tool(name: &str, description: &str) -> ToolInfo {
        ToolInfo {
            name: name.to_string(),
            description: description.to_string(),
            category: "custom".to_string(),
            categories: vec!["custom".to_string()],
            parameters: vec![
                ParameterDefinition {
                    name: "query".to_string(),
                    param_type: "string".to_string(),
                    required: true,
                    description: "Search query".to_string(),
                    validation_rules: vec![],
                    default_value: None,
                    enum_values: None,
                    schema: serde_json::Value::Null,
                },
                ParameterDefinition {
                    name: "limit".to_string(),
                    param_type: "integer".to_string(),
                    required: false,
                    description: "Max results".to_string(),
                    validation_rules: vec![],
                    default_value: Some(serde_json::json!(10)),
                    enum_values: None,
                    schema: serde_json::Value::Null,
                },
            ],
            enhanced_description: None,
            keywords: vec![],
            use_cases: vec![],
            composition_category: None,
            providing_agent_id: None,
        }
    }

    fn make_delegate_tool(name: &str, agent_id: &str) -> ToolInfo {
        let mut tool = make_custom_tool(name, "delegate tool");
        tool.providing_agent_id = Some(agent_id.to_string());
        tool
    }

    fn delegation_target() -> crate::magician_v2::execution::agentic::DelegationTarget {
        crate::magician_v2::execution::agentic::DelegationTarget {
            agent_id: "worker-1".to_string(),
            name: "Worker One".to_string(),
            aliases: Vec::new(),
            description: "A test worker".to_string(),
            tools: vec![],
            allowed_invocation_surfaces: vec![
                crate::magician_v2::agents::InvocationSurface::Delegation,
                crate::magician_v2::agents::InvocationSurface::Handover,
            ],
        }
    }

    fn native_response_with_reasoning(
        reasoning: Option<&str>,
    ) -> crate::magician_v2::execution::agentic::native_types::ExecutionNativeResponse {
        let mut response = crate::magician_v2::execution::agentic::native_types::ExecutionNativeResponse::unadmitted(vec![]);
        response.text = Some("the answer".to_string());
        response.reasoning_text = reasoning.map(str::to_string);
        response.finish_reason = Some("stop".to_string());
        response.prompt_tokens = Some(100);
        response.completion_tokens = Some(50);
        response.provider = Some("openai".to_string());
        response.model = Some("gpt-5.6-sol".to_string());
        response.reasoning_tokens = Some(20);
        response
    }

    #[test]
    fn assistant_turn_persists_trimmed_reasoning_summary() {
        // The reasoning summary was previously parsed then dropped — assert it now
        // lands on the assistant-turn record (trimmed).
        let response = native_response_with_reasoning(Some("  I considered the options.  "));
        let turn = assistant_turn_from_native_response("agentic_decision", &response)
            .expect("text turn should build");
        assert_eq!(turn.reasoning.as_deref(), Some("I considered the options."));
    }

    #[test]
    fn assistant_turn_drops_empty_reasoning() {
        // Whitespace-only / absent reasoning → None (not an empty string).
        let ws = native_response_with_reasoning(Some("   "));
        assert_eq!(
            assistant_turn_from_native_response("op", &ws)
                .expect("turn")
                .reasoning,
            None
        );
        let none = native_response_with_reasoning(None);
        assert_eq!(
            assistant_turn_from_native_response("op", &none)
                .expect("turn")
                .reasoning,
            None
        );
    }

    #[test]
    fn build_catalog_context_extracts_direct_capabilities() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.merged_agent_tools = vec![
            make_custom_tool("browser", "Browser automation"),
            make_custom_tool("websearch", "Search the web"),
            make_delegate_tool("delegate_search", "agent-42"),
        ];
        let catalog_ctx = build_catalog_context(&ctx);

        // Browser is a direct inner-loop pack now; delegate-owned tools are filtered.
        // Since 0.8c, UNIVERSAL_BACKEND_PACKS (memory, task management, introspection,
        // web, filesystem, shell — 39 tools) are injected into every agent's catalog.
        let names: Vec<&str> = catalog_ctx
            .direct_capabilities
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();
        assert!(names.contains(&"browser"));
        assert!(names.contains(&"websearch"));
        // A few representative universals are always injected.
        assert!(names.contains(&"save_preference"));
        assert!(names.contains(&"list_tasks"));
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"content_search"));
        assert!(names.contains(&"content_read"));
        assert!(names.contains(&"files"));
        assert!(names.contains(&"http"));
        assert!(names.contains(&"shell"));
        // Delegate-owned tools are filtered out.
        assert!(!names.contains(&"delegate_search"));
        // custom (browser, websearch) + UNIVERSAL_BACKEND_PACKS (43, incl.
        // provider-neutral content_search/content_read and the billed
        // web_answer search lane) = 45.
        assert_eq!(names.len(), 45);
    }

    #[test]
    fn build_catalog_context_filters_all_builtin_names() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        let builtin_names = ["file", "http", "bash"];
        ctx.merged_agent_tools = builtin_names
            .iter()
            .map(|name| ToolInfo {
                name: name.to_string(),
                description: format!("{} tool", name),
                category: name.to_string(),
                categories: vec![name.to_string()],
                parameters: vec![],
                enhanced_description: None,
                keywords: vec![],
                use_cases: vec![],
                composition_category: None,
                providing_agent_id: None,
            })
            .collect();
        let catalog_ctx = build_catalog_context(&ctx);
        // 0.8c-11: built-in lane names (file/http/bash) are still filtered as
        // reserved names, but the catalog is never empty because
        // UNIVERSAL_BACKEND_PACKS (incl. the compiled replacements files/shell)
        // are always injected — intended behavior.
        let names: Vec<&str> = catalog_ctx
            .direct_capabilities
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();
        // `file` and `bash` are not universals, so they vanish entirely.
        assert!(!names.contains(&"file"));
        assert!(!names.contains(&"bash"));
        // `http` happens to also be a UNIVERSAL_BACKEND_PACK, so it's injected
        // back (as the universal, not the agent-declared lane). Its compiled
        // replacements `files`/`shell` are universals too.
        assert!(names.contains(&"http"));
        assert!(names.contains(&"files"));
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"content_search"));
        assert!(names.contains(&"content_read"));
        // Only the UNIVERSAL_BACKEND_PACKS (43, incl. the provider-neutral
        // content acquisition pair and web_answer) remain — the 3 builtin
        // lanes filtered.
        assert_eq!(names.len(), 43);
    }

    #[test]
    fn build_catalog_context_untrusted_agent_gets_only_safe_universals() {
        // SECURITY: an untrusted (stranger-facing) agent must NOT silently
        // inherit the dangerous universal packs. Its tools allowlist +
        // denied_tools is the real boundary (fail-closed). Here the agent
        // declares only a single custom tool; the catalog must add just the
        // safe universal subset, not shell/http/files/task-mutation/etc.
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.merged_agent_tools = vec![make_custom_tool("answer_question", "Answer a question")];
        ctx.trust_level =
            Some(crate::magician_v2::agents::types::TrustLevel::UNTRUSTED.to_string());
        let catalog_ctx = build_catalog_context(&ctx);
        let names: Vec<&str> = catalog_ctx
            .direct_capabilities
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();

        // The agent's own declared tool still flows through.
        assert!(names.contains(&"answer_question"));

        // Representative dangerous universals are withheld from untrusted agents.
        for withheld in [
            "shell",
            "http",
            "files",
            "read_file",
            "write_file",
            "update_task",
            "delete_task",
            "list_tasks",
            "list_agents",
            "inspect_agent",
            "get_execution_history",
            "update_memory_tier",
        ] {
            assert!(
                !names.contains(&withheld),
                "untrusted agent must NOT receive `{withheld}`"
            );
        }

        // The safe subset is still injected.
        assert!(names.contains(&"activate_skill"));
        assert!(names.contains(&"time_math"));

        // Exactly the custom tool + the safe universal subset.
        assert_eq!(names.len(), 1 + SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED.len());
    }

    #[test]
    fn build_catalog_context_mixed_case_untrusted_agent_still_fails_closed() {
        // Agent-definition validation intentionally accepts trust-level casing
        // variants. Capability narrowing must use the same canonical semantics;
        // otherwise a valid value such as `Untrusted` would inherit trusted
        // memory/files/shell universals.
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.merged_agent_tools = vec![make_custom_tool("answer_question", "Answer a question")];
        ctx.trust_level = Some("  UnTrUsTeD  ".to_string());

        let catalog_ctx = build_catalog_context(&ctx);
        let names: Vec<&str> = catalog_ctx
            .direct_capabilities
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();

        assert!(names.contains(&"answer_question"));
        assert!(names.contains(&"time_math"));
        assert!(!names.contains(&"search_memory"));
        assert!(!names.contains(&"files"));
        assert!(!names.contains(&"shell"));
        assert_eq!(names.len(), 1 + SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED.len());
    }

    #[test]
    fn build_catalog_context_trusted_agent_keeps_full_universals() {
        // A trusted (default trust_level / no trust_level set) agent is
        // UNCHANGED by the untrusted-narrowing — it still receives the full
        // universal pack set, including the dangerous ones.
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.merged_agent_tools = vec![make_custom_tool("answer_question", "Answer a question")];
        // No trust_level set => trusted (default `local`-equivalent here).
        let catalog_ctx = build_catalog_context(&ctx);
        let names: Vec<&str> = catalog_ctx
            .direct_capabilities
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"update_task"));
        assert!(names.contains(&"http"));
        // custom tool + the full UNIVERSAL_BACKEND_PACKS set.
        assert_eq!(names.len(), 1 + UNIVERSAL_BACKEND_PACKS.len());
    }

    #[test]
    fn build_catalog_context_trimmed_denies_remove_explicit_and_universal_tools() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.merged_agent_tools = vec![
            make_custom_tool("answer_question", "Answer a question"),
            make_custom_tool("shell", "Run shell"),
        ];
        ctx.denied_capability_names = vec![" shell ".to_string(), " core_utility ".to_string()];

        let catalog_ctx = build_catalog_context(&ctx);
        let names: Vec<&str> = catalog_ctx
            .direct_capabilities
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();

        assert!(names.contains(&"answer_question"));
        assert!(!names.contains(&"shell"));
        assert!(!names.contains(&"time_math"));
    }

    #[test]
    fn universal_backend_pack_contract_has_no_schema_or_safe_subset_drift() {
        let embedded_names: std::collections::HashSet<String> =
            crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs()
                .into_iter()
                .map(|pack| pack.name)
                .collect();

        for universal in UNIVERSAL_BACKEND_PACKS {
            assert!(
                embedded_names.contains(*universal),
                "universal `{universal}` must have an embedded pack definition so every offered catalogue entry has a real schema and provider"
            );
        }
        for safe in SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED {
            assert!(
                UNIVERSAL_BACKEND_PACKS.contains(safe),
                "untrusted-safe `{safe}` must remain a subset of UNIVERSAL_BACKEND_PACKS"
            );
        }
    }

    /// Several universal packs are ALSO harness tools, which is why
    /// `V2Orchestrator::visible_tools_for_definition` merges the substrate
    /// *after* its harness strip rather than before. Merging first would let
    /// the strip delete these again for any non-harness agent, leaving the
    /// planner short of tools the executor injects for every trusted agent —
    /// the planner/executor divergence that union exists to close.
    ///
    /// If this overlap ever empties, that ordering constraint has gone away and
    /// can be revisited. If it grows, re-check the ordering still holds.
    #[test]
    fn universal_packs_overlap_harness_tools_so_merge_order_matters() {
        let overlap: Vec<&str> = UNIVERSAL_BACKEND_PACKS
            .iter()
            .copied()
            .filter(|name| crate::magician_v2::harness::is_harness_tool_name(name))
            .collect();
        assert!(
            !overlap.is_empty(),
            "no universal pack is a harness tool any more — the merge-after-strip \
             ordering in `visible_tools_for_definition` can be revisited"
        );
        assert!(
            overlap.contains(&"list_agents"),
            "expected `list_agents` in the overlap, got {overlap:?}"
        );
    }

    #[test]
    fn build_catalog_context_credentials_enabled() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.provisioned_secret_access_enabled = true;
        let catalog_ctx = build_catalog_context(&ctx);
        assert!(catalog_ctx.credentials_enabled);
    }

    #[test]
    fn build_catalog_context_credentials_enabled_by_default() {
        // AgenticContext defaults provisioned_secret_access_enabled to true
        let ctx = AgenticContext::new("goal", "criteria");
        let catalog_ctx = build_catalog_context(&ctx);
        assert!(catalog_ctx.credentials_enabled);
    }

    #[test]
    fn build_catalog_context_credentials_disabled_when_set() {
        let ctx = AgenticContext::new("goal", "criteria").with_provisioned_secret_access(false);
        let catalog_ctx = build_catalog_context(&ctx);
        assert!(!catalog_ctx.credentials_enabled);
    }

    #[test]
    fn build_catalog_context_has_delegation_targets() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.delegation_targets = vec![crate::magician_v2::execution::agentic::DelegationTarget {
            agent_id: "agent-1".to_string(),
            name: "Agent One".to_string(),
            aliases: Vec::new(),
            description: "A test agent".to_string(),
            tools: vec![],
            allowed_invocation_surfaces: vec![
                crate::magician_v2::agents::InvocationSurface::Delegation,
                crate::magician_v2::agents::InvocationSurface::Handover,
            ],
        }];
        let catalog_ctx = build_catalog_context(&ctx);
        assert!(catalog_ctx.has_delegation_targets);
    }

    #[test]
    fn build_catalog_context_no_delegation_targets_by_default() {
        let ctx = AgenticContext::new("goal", "criteria");
        let catalog_ctx = build_catalog_context(&ctx);
        assert!(!catalog_ctx.has_delegation_targets);
    }

    #[test]
    fn autonomous_shared_runtime_requires_both_bounded_stores() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        assert!(!autonomous_shared_runtime_is_active(&ctx));
        ctx.surface_plan_cache =
            Some(crate::magician_v2::execution::flat_loop::SurfacePlanCache::new(8));
        assert!(!autonomous_shared_runtime_is_active(&ctx));
        ctx.surface_working_sets =
            Some(crate::magician_v2::execution::flat_loop::SurfaceWorkingSetStore::new(8));
        assert!(autonomous_shared_runtime_is_active(&ctx));
    }

    #[test]
    fn production_shared_runtime_fails_closed_without_stable_run_binding() {
        use crate::magician_v2::execution::capability::CapabilityPackDefinition;
        use crate::magician_v2::execution::flat_loop::build_tool_index;

        let packs = [
            "name: tool_search\ndescription: search tools\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: tool_search\n",
            "name: shell\ndescription: run shell\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: shell\n",
        ]
        .into_iter()
        .map(|yaml| serde_yaml::from_str::<CapabilityPackDefinition>(yaml).expect("pack"))
        .collect::<Vec<_>>();
        let definition = crate::magician_v2::agents::AgentDefinition::from_yaml_str(
            "agent_id: worker\nname: Worker\npersona: Test\ntrust_level: local\ntools:\n  - tool_search\n  - shell\n",
        )
        .expect("definition");
        let mut ctx = AgenticContext::new("query", "result");
        ctx.principal = Some("owner".to_string());
        ctx.workspace = Some("default".to_string());
        ctx.agent_id = Some("worker".to_string());
        ctx.active_owner_agent_id = Some("worker".to_string());
        ctx.owner_definition = Some(std::sync::Arc::new(definition));
        ctx.merged_agent_tools = vec![
            make_custom_tool("tool_search", "Search tools"),
            make_custom_tool("shell", "Run shell"),
        ];
        ctx.tool_index = Some(std::sync::Arc::new(build_tool_index(&packs)));
        ctx.scope_capability_revision = Some("registry-a".to_string());
        ctx.surface_plan_cache =
            Some(crate::magician_v2::execution::flat_loop::SurfacePlanCache::new(8));
        ctx.surface_working_sets =
            Some(crate::magician_v2::execution::flat_loop::SurfaceWorkingSetStore::new(8));

        let catalog_ctx = build_catalog_context(&ctx);
        let (tools, deferred) =
            build_decision_tools_and_deferred(&ctx, &catalog_ctx, Some(0), None);

        assert!(tools.is_empty());
        assert!(deferred.is_none());
        assert!(ctx
            .policy_dispatch_tool_names
            .lock()
            .expect("dispatch ceiling")
            .as_ref()
            .is_some_and(|names| names.is_empty()));
    }

    #[test]
    fn relay_identity_caps_flat_delegate_schema_without_changing_tools() {
        use crate::magician_v2::execution::capability::CapabilityPackDefinition;
        use crate::magician_v2::execution::flat_loop::build_tool_index;

        let packs: Vec<CapabilityPackDefinition> = [
            "name: tool_search\ndescription: search tools\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: tool_search\n",
            "name: shell\ndescription: run shell\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: shell\n",
        ]
        .into_iter()
        .map(|yaml| serde_yaml::from_str(yaml).expect("pack parses"))
        .collect();
        let index = std::sync::Arc::new(build_tool_index(&packs));

        let build_for_agent = |agent_id: &str| {
            let mut ctx = AgenticContext::new("goal", "criteria");
            ctx.agent_id = Some(agent_id.to_string());
            ctx.delegation_targets = vec![delegation_target()];
            ctx.merged_agent_tools = vec![make_custom_tool("shell", "Run shell")];
            ctx.tool_index = Some(index.clone());
            let catalog_ctx = build_catalog_context(&ctx);
            let (tools, _) = build_decision_tools_and_deferred(&ctx, &catalog_ctx, None, None);
            (catalog_ctx, tools)
        };

        let (relay_ctx, relay_tools) = build_for_agent("harness-sre");
        let (unrelated_ctx, unrelated_tools) = build_for_agent("cto");
        assert_eq!(relay_ctx.delegation_targets_max_items(), Some(1));
        assert_eq!(unrelated_ctx.delegation_targets_max_items(), None);

        let relay_names: Vec<&str> = relay_tools.iter().map(|tool| tool.name.as_str()).collect();
        let unrelated_names: Vec<&str> = unrelated_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(relay_names, unrelated_names, "Relay keeps every flat tool");
        assert!(relay_names.contains(&"shell"));
        assert!(relay_names.contains(&"handover_to_agent"));

        let relay_delegate = relay_tools
            .iter()
            .find(|tool| tool.name == "delegate_to_agent")
            .unwrap();
        let unrelated_delegate = unrelated_tools
            .iter()
            .find(|tool| tool.name == "delegate_to_agent")
            .unwrap();
        assert_eq!(
            relay_delegate.parameters["properties"]["delegation_targets"]["maxItems"],
            json!(1)
        );
        assert_eq!(
            relay_delegate.parameters["properties"]["delegation_targets"]["minItems"],
            json!(1)
        );
        assert!(
            unrelated_delegate.parameters["properties"]["delegation_targets"]
                .get("maxItems")
                .is_none()
        );
    }

    // === flat decision catalog (hot tier + deferred block) ===

    #[test]
    fn build_decision_tools_without_index_uses_full_catalog_no_deferred_block() {
        // No tool_index (no registry) → degrade to the full native catalog.
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.merged_agent_tools = vec![make_custom_tool("websearch", "Search the web")];
        let catalog_ctx = build_catalog_context(&ctx);
        let (tools, block) = build_decision_tools_and_deferred(&ctx, &catalog_ctx, None, None);
        assert!(block.is_none(), "no index → no deferred block");
        // Byte-for-byte the same tool set the adapter used to build itself.
        assert_eq!(
            tools.len(),
            build_execution_native_catalog(&catalog_ctx).len()
        );
        assert!(!tools.is_empty());
    }

    #[test]
    fn autonomous_catalog_excludes_guarded_app_tools_before_policy_snapshot() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.merged_agent_tools = vec![
            make_custom_tool("app_data_query", "Governed app query"),
            make_custom_tool("websearch", "Search the web"),
        ];
        let catalog_ctx = build_catalog_context(&ctx);
        let raw = build_execution_native_catalog(&catalog_ctx);
        assert!(raw.iter().any(|tool| tool.name == "app_data_query"));

        let (tools, block) = build_decision_tools_and_deferred(&ctx, &catalog_ctx, None, None);
        assert!(block.is_none());
        assert!(!tools.iter().any(|tool| tool.name == "app_data_query"));
        assert!(tools.iter().any(|tool| tool.name == "websearch"));
    }

    #[test]
    fn build_decision_tools_flat_returns_hot_tier_and_deferred_block() {
        use crate::magician_v2::execution::capability::CapabilityPackDefinition;
        use crate::magician_v2::execution::flat_loop::build_tool_index;

        fn pack(yaml: &str) -> CapabilityPackDefinition {
            serde_yaml::from_str(yaml).expect("pack parses")
        }
        // Minimal index: two hot universals + an inner-loop pack with one primitive.
        let packs = vec![
            pack("name: tool_search\ndescription: search tools\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: tool_search\n"),
            pack("name: shell\ndescription: run a shell command\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: shell\n"),
            pack("name: duckdb\ndescription: duck\nparameters: []\nnative_action_schemas:\n  query:\n    description: q\n    parameters: [sql]\n    required: [sql]\nimplementation:\n  type: primitive\n  provider_name: duckdb\n"),
        ];
        let index = std::sync::Arc::new(build_tool_index(&packs));

        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.tool_index = Some(index);
        ctx.merged_agent_tools = vec![make_custom_tool("duckdb", "DuckDB SQL")];
        let catalog_ctx = build_catalog_context(&ctx);
        let (tools, block) = build_decision_tools_and_deferred(&ctx, &catalog_ctx, None, None);

        let hot: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            hot.contains(&"yield"),
            "hot tier must include the yield terminal"
        );
        assert!(
            !hot.contains(&"goal_reached") && !hot.contains(&"cannot_proceed"),
            "flat hot tier must not expose retired LLM terminals"
        );
        assert!(
            hot.contains(&"tool_search"),
            "hot tier must include tool_search"
        );
        assert!(hot.contains(&"shell"), "hot tier must include shell");
        // Pack primitives are deferred, not hot.
        assert!(!hot.contains(&"duckdb__query"));

        let block = block.expect("flat mode emits a deferred block");
        assert!(
            block.contains("DEFERRED TOOLS"),
            "block must carry the header"
        );
        assert!(
            block.contains("duckdb__query"),
            "deferred block must list the duckdb primitive, got: {block}"
        );
    }

    #[test]
    fn autonomous_shared_task_projection_persists_family_and_clears_on_revision() {
        use crate::magician_v2::execution::capability::CapabilityPackDefinition;
        use crate::magician_v2::execution::flat_loop::{build_tool_index, WorkingSetLimits};

        let packs = [
            "name: tool_search\ndescription: search tools\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: tool_search\n",
            "name: shell\ndescription: run shell\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: shell\n",
            "name: duckdb\ndescription: query data\nparameters: []\nnative_action_schemas:\n  query:\n    description: query data\n    parameters: [sql]\n    required: [sql]\n    parameter_overrides:\n      sql: {type: string}\nimplementation:\n  type: primitive\n  provider_name: duckdb\n",
        ]
        .into_iter()
        .map(|yaml| serde_yaml::from_str::<CapabilityPackDefinition>(yaml).expect("pack"))
        .collect::<Vec<_>>();
        let index = std::sync::Arc::new(build_tool_index(&packs));
        let definition = crate::magician_v2::agents::AgentDefinition::from_yaml_str(
            "agent_id: worker\nname: Worker\npersona: Test\ntrust_level: local\ntools:\n  - tool_search\n  - shell\n  - duckdb\n",
        )
        .expect("definition");
        let mut ctx = AgenticContext::new("query", "result");
        ctx.principal = Some("owner".to_string());
        ctx.workspace = Some("default".to_string());
        ctx.agent_id = Some("worker".to_string());
        ctx.active_owner_agent_id = Some("worker".to_string());
        ctx.task_id = Some("task-a".to_string());
        ctx.execution_id = Some("execution-a".to_string());
        ctx.owner_definition = Some(std::sync::Arc::new(definition));
        ctx.merged_agent_tools = vec![
            make_custom_tool("tool_search", "Search tools"),
            make_custom_tool("shell", "Run shell"),
            make_custom_tool("duckdb", "Query data"),
        ];
        ctx.tool_index = Some(index.clone());
        ctx.scope_capability_revision = Some("registry-a".to_string());
        ctx.surface_plan_cache =
            Some(crate::magician_v2::execution::flat_loop::SurfacePlanCache::new(8));
        let working_sets = crate::magician_v2::execution::flat_loop::SurfaceWorkingSetStore::new(8);
        ctx.surface_working_sets = Some(working_sets.clone());
        let catalog_ctx = build_catalog_context(&ctx);

        let (initial_tools, deferred_block) =
            build_decision_tools_and_deferred(&ctx, &catalog_ctx, Some(0), None);
        assert!(deferred_block
            .as_deref()
            .is_some_and(|block| block.contains("duckdb__query")));
        assert!(initial_tools.iter().any(|tool| tool.name == "tool_search"));
        assert!(!initial_tools
            .iter()
            .any(|tool| tool.name == "duckdb__query"));
        let binding = ctx
            .scratch
            .autonomous_surface_binding
            .lock()
            .expect("binding lock")
            .clone()
            .expect("active binding");
        working_sets
            .select(
                &binding.key,
                &binding.authority_revision,
                index.as_ref(),
                &["duckdb__query".to_string()],
                &binding.authorized_tool_names,
                WorkingSetLimits::autonomous_compatibility(1),
            )
            .expect("load duckdb family");
        ctx.scratch
            .loaded_tools
            .lock()
            .expect("loaded lock")
            .clear();

        let (loaded_tools, _) =
            build_decision_tools_and_deferred(&ctx, &catalog_ctx, Some(1), None);
        assert!(loaded_tools.iter().any(|tool| tool.name == "duckdb__query"));

        ctx.scope_capability_revision = Some("registry-b".to_string());
        let (revised_tools, _) =
            build_decision_tools_and_deferred(&ctx, &catalog_ctx, Some(2), None);
        assert!(!revised_tools
            .iter()
            .any(|tool| tool.name == "duckdb__query"));
        let revised_binding = ctx
            .scratch
            .autonomous_surface_binding
            .lock()
            .expect("binding lock")
            .clone()
            .expect("revised binding");
        assert!(working_sets
            .reconcile(
                &revised_binding.key,
                &revised_binding.authority_revision,
                &revised_binding.authorized_tool_names,
            )
            .loaded_tools
            .is_empty());
    }

    #[test]
    fn render_deferred_block_empty_input_is_empty() {
        assert!(render_deferred_block(&[]).is_empty());
    }

    #[test]
    fn render_deferred_block_shows_a_shared_pack_hint_once() {
        let hint = Some("Editable local XLSX workbook authoring".to_string());
        let deferred = vec![
            crate::magician_v2::execution::flat_loop::DeferredEntry {
                name: "office-excel__create".to_string(),
                group: "pack",
                search_hint: hint.clone(),
            },
            crate::magician_v2::execution::flat_loop::DeferredEntry {
                name: "office-excel__set".to_string(),
                group: "pack",
                search_hint: hint,
            },
        ];

        let block = render_deferred_block(&deferred);
        assert!(block.contains("office-excel__create"));
        assert!(block.contains("office-excel__set"));
        assert_eq!(
            block
                .matches("Editable local XLSX workbook authoring")
                .count(),
            1
        );
    }

    #[test]
    fn build_catalog_context_allowed_action_types_propagated() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.allowed_action_types = Some(vec!["browser".into(), "bash".into()]);
        let catalog_ctx = build_catalog_context(&ctx);
        assert_eq!(
            catalog_ctx.allowed_action_types,
            Some(vec!["browser".to_string(), "bash".to_string()])
        );
    }

    #[test]
    fn build_catalog_context_allowed_action_types_none_by_default() {
        let ctx = AgenticContext::new("goal", "criteria");
        let catalog_ctx = build_catalog_context(&ctx);
        assert!(catalog_ctx.allowed_action_types.is_none());
    }

    #[test]
    fn parameters_to_json_schema_produces_valid_schema() {
        let tool = make_custom_tool("test", "test tool");
        let schema = parameters_to_json_schema(&tool.parameters);

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        assert_eq!(schema["properties"]["query"]["type"], "string");
        assert_eq!(schema["properties"]["query"]["description"], "Search query");
        assert_eq!(schema["properties"]["limit"]["type"], "integer");
        assert_eq!(schema["properties"]["limit"]["default"], 10);

        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(required, vec!["query"]);
    }

    #[test]
    fn parameters_to_json_schema_includes_enum_values() {
        let params = vec![ParameterDefinition {
            name: "format".to_string(),
            param_type: "string".to_string(),
            required: true,
            description: "Output format".to_string(),
            validation_rules: vec![],
            default_value: None,
            schema: serde_json::Value::Null,
            enum_values: Some(vec!["json".into(), "csv".into()]),
        }];
        let schema = parameters_to_json_schema(&params);
        let enum_vals: Vec<&str> = schema["properties"]["format"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(enum_vals, vec!["json", "csv"]);
    }
}

#[cfg(test)]
mod stable_prompt_tests {
    use super::*;
    use magicllm::prelude::{ContentBlock, MessageRole};

    fn text(role: MessageRole, t: &str) -> RouterMessage {
        RouterMessage {
            role,
            content: vec![ContentBlock::Text {
                text: t.to_string(),
            }],
        }
    }

    #[test]
    fn the_stable_prompt_leads_the_conversation_and_only_the_turn_part_trails() {
        let marker = magicllm::types::CACHE_BREAKPOINT_SENTINEL;
        let prompt = format!("## GOAL\nopen TextEdit\n{marker}\n## CURRENT STATE\nwindow 7");
        let (stable, turn) = split_stable_prompt(&prompt);
        assert_eq!(stable.as_deref(), Some("## GOAL\nopen TextEdit"));
        assert_eq!(turn, "## CURRENT STATE\nwindow 7");
        // A continuation delta has no marker: all of it is this turn's.
        assert_eq!(
            split_stable_prompt("## DELTA"),
            (None, "## DELTA".to_string())
        );

        let history = vec![
            text(MessageRole::Assistant, "call"),
            text(MessageRole::User, "result"),
        ];
        let mut messages = vec![RouterMessage::system("sys")];
        place_stable_prompt(&mut messages, stable.clone(), history);
        let roles: Vec<MessageRole> = messages.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                MessageRole::System,
                MessageRole::User,
                MessageRole::Assistant,
                MessageRole::User
            ]
        );
        let marked = format!("## GOAL\nopen TextEdit\n{marker}");
        assert!(matches!(
            &messages[1].content[0],
            ContentBlock::Text { text } if *text == marked
        ));

        // A conversation opening with a user message takes the stable text as
        // its first block rather than a second consecutive user message.
        let mut messages = vec![RouterMessage::system("sys")];
        place_stable_prompt(
            &mut messages,
            stable,
            vec![text(MessageRole::User, "result")],
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content.len(), 2);
    }
}
