//! Native tool-call lowering layer for the execution-native migration.
//!
//! Takes an [`ExecutionNativeResponse`] and validates it (zero/multi tool-call
//! policy), then lowers a single [`ExecutionToolCall`] into an
//! [`ExecutionDecisionEnvelope`].
//!
//! # Why some LLM tools live here instead of as packs
//!
//! By Phase 0.8c-11 the codebase has two clean tool layers:
//!
//! - **Work tools** — anything the LLM can ask the runtime to *do*.
//!   They all share a single lowering path (`lower_pack_execute`
//!   below builds an `ExecutableAction::Pack` envelope) and a single
//!   dispatch entry point (`CapabilityRegistry::execute` resolves
//!   the provider). The registry then branches by the pack's
//!   `ImplementationType`:
//!
//!   - **Compiled** — Rust-native providers (struct-based like
//!     `FileCapabilityProvider`, or generic-handler-based like
//!     `GenericCompiledProvider` reading from
//!     `compiled_handlers/<name>::handle`). Signature:
//!     `async fn handle(resources, args) -> Result<Value, ...>`.
//!   - **Composite** — pack-defined as a sequence of calls to other
//!     registered capabilities. Walked by `PackCapabilityProvider::execute_composite`.
//!   - **Command** — direct subprocess via `Command::new(program).arg(...)`.
//!     Argv built from declarative `CommandArgMapping`s. Walked by
//!     `PackCapabilityProvider::execute_command`.
//!   - **Primitive** — nested LLM loop with the pack's
//!     `native_action_schemas` as the inner-LLM's catalog. Browser is
//!     the canonical example; gmail / csvkit / metabase explore use
//!     this too. Dispatched by the outer executor when it sees
//!     `ImplementationType::Primitive` in the registry.
//!
//!   "Compiled" and the latter three are sometimes called the
//!   "compiled" vs "interpreted" buckets respectively. From the LLM's
//!   perspective they're interchangeable — same catalog, same tool
//!   call shape, same `Value`-returning contract. The difference is
//!   only in how the runtime executes them.
//!
//! - **Control decisions** (lowering arms below): the LLM tool call is
//!   *not* a request to do work — it's a signal to the orchestrator
//!   that alters execution flow. `yield` terminates the loop with a
//!   structured outcome; `need_user_input` pauses for user input;
//!   `spawn_sub_goal` spawns a child execution; `delegate_to_agent` /
//!   `handover_to_agent` transfer to another agent. None of these
//!   produce a `Value` the LLM consumes — they produce `Decision`
//!   variants the executor *interprets*.
//!
//! The work-tool rail's return type (`Result<Value, ...>`) can't
//! express "alter execution flow" without a richer contract (a
//! `HandlerOutcome` enum that distinguishes value-returning handlers
//! from control-returning handlers). Until/unless that refactor lands,
//! control decisions stay on this lowering path — the architectural
//! split is real, not gratuitous duplication.

use std::collections::HashMap;

use serde_json::{Map, Value};
use tracing::info;

use super::native_catalog::{
    DECISION_METADATA_FIELD, LEGACY_DECISION_METADATA_FIELDS, MAX_DECISION_RATIONALE_CHARS,
};
use super::native_types::{
    native_tool_argument_admission_error, CachedNativeToolArgumentAdmission,
    ExecutionDecisionEnvelope, ExecutionNativeResponse, ExecutionToolCall, NativeDecisionOutcome,
};
use super::Decision;
use crate::magician_v2::execution::actions::{DelegationTargetRequest, ExecutableAction};
use crate::magician_v2::execution::agentic::types::{
    Artifact, ChoiceOption, FormQuestion, UserInputType,
};
use crate::magician_v2::execution::capability::ImplementationType;
use crate::magician_v2::execution::durable_task_state::{
    parse_task_state_action_value, TaskStateActionEnvelope,
};
use crate::magician_v2::execution::verified_executor::{ActionCandidate, CandidateBatch};
use crate::magician_v2::json_traversal::clone_json_iteratively;

// =============================================================================
// Constants
// =============================================================================

/// Default budget for spawn_sub_goal when the LLM omits budget_iterations.
const DEFAULT_SUB_GOAL_BUDGET: usize = 75;

/// Maximum allowed budget for spawn_sub_goal.
const MAX_SUB_GOAL_BUDGET: usize = 400;

fn rejected_raw_arguments(error: &str) -> Value {
    serde_json::json!({
        "_omitted": true,
        "reason": error,
    })
}

// =============================================================================
// Common metadata
// =============================================================================

/// Side-channel metadata extracted from tool-call arguments.
struct CommonMetadata {
    request_hover_discovery: Option<bool>,
    request_vision: Option<bool>,
    vision_reason: Option<String>,
    step_completed: Option<String>,
    step_failed: Option<String>,
    needs_plan_revision: bool,
    task_state_action: TaskStateActionEnvelope,
    thinking: Option<String>,
}

fn normalize_decision_rationale(value: Option<&str>) -> Option<String> {
    let value = value.map(str::trim).filter(|value| !value.is_empty())?;
    if value.chars().count() <= MAX_DECISION_RATIONALE_CHARS {
        return Some(value.to_string());
    }

    let mut bounded: String = value
        .chars()
        .take(MAX_DECISION_RATIONALE_CHARS.saturating_sub(1))
        .collect();
    bounded.push('…');
    Some(bounded)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskStateActionPolicy {
    Optional,
    Disabled,
}

fn extract_common_metadata(
    args: &Value,
    task_state_action_policy: TaskStateActionPolicy,
) -> Result<CommonMetadata, String> {
    let decision_metadata = match args.get(DECISION_METADATA_FIELD) {
        None => None,
        Some(Value::Object(metadata)) => Some(metadata),
        Some(_) => {
            return Err(format!(
                "`{DECISION_METADATA_FIELD}` must be an object when present"
            ));
        },
    };

    let task_state_action = match task_state_action_policy {
        TaskStateActionPolicy::Optional => match args.get("task_state_action") {
            Some(value) => parse_task_state_action_value(Some(value))?,
            None => TaskStateActionEnvelope::default(),
        },
        TaskStateActionPolicy::Disabled => TaskStateActionEnvelope::default(),
    };

    let request_hover_discovery =
        decision_metadata_value(args, decision_metadata, "request_hover_discovery")?
            .and_then(Value::as_bool);
    let request_vision = decision_metadata_value(args, decision_metadata, "request_vision")?
        .and_then(Value::as_bool);
    let vision_reason = decision_metadata_value(args, decision_metadata, "vision_reason")?
        .and_then(Value::as_str)
        .map(String::from);
    let step_completed = decision_metadata_value(args, decision_metadata, "step_completed")?
        .and_then(Value::as_str)
        .map(String::from);
    let step_failed = decision_metadata_value(args, decision_metadata, "step_failed")?
        .and_then(Value::as_str)
        .map(String::from);
    let needs_plan_revision =
        decision_metadata_value(args, decision_metadata, "needs_plan_revision")?
            .and_then(Value::as_bool)
            .unwrap_or(false);

    Ok(CommonMetadata {
        thinking: normalize_decision_rationale(args.get("thinking").and_then(Value::as_str)),
        request_hover_discovery,
        request_vision,
        vision_reason,
        step_completed,
        step_failed,
        needs_plan_revision,
        task_state_action,
    })
}

/// Resolve one sparse decision signal from the compact sidecar, with a flat
/// legacy fallback for persisted history and older provider responses. Equal
/// duplicate values are harmless; conflicting values fail closed so execution
/// never has to guess which plan/vision signal the model intended.
fn decision_metadata_value<'a>(
    args: &'a Value,
    decision_metadata: Option<&'a Map<String, Value>>,
    field: &str,
) -> Result<Option<&'a Value>, String> {
    debug_assert!(LEGACY_DECISION_METADATA_FIELDS.contains(&field));
    let nested = decision_metadata.and_then(|metadata| metadata.get(field));
    let legacy = args.get(field);
    if let (Some(nested), Some(legacy)) = (nested, legacy) {
        if nested != legacy {
            return Err(format!(
                "conflicting `{field}` values in `{DECISION_METADATA_FIELD}` and legacy flat metadata"
            ));
        }
    }
    Ok(nested.or(legacy))
}

// =============================================================================
// Response-level validation
// =============================================================================

/// Validates and lowers the response to a decision envelope.
///
/// - 0 tool calls → `NativeDecisionOutcome::ZeroToolCalls`
/// - 1 tool call → lowers via [`lower_native_tool_call`], attaching raw text
/// - 2+ tool calls → folds consecutive leading execute calls in order; once a
///   terminal/unlowerable call is encountered, preserves it and later calls as
///   deferred hints rather than executing anything after the terminal
pub fn lower_native_response(response: &ExecutionNativeResponse) -> NativeDecisionOutcome {
    match response.tool_calls().len() {
        0 => NativeDecisionOutcome::ZeroToolCalls {
            text_fallback: response.text.as_deref().map(str::to_owned),
            finish_reason: response.finish_reason.clone(),
        },
        n => {
            // Lower the first tool call. When the model emits several calls in
            // one turn (e.g. act → verify-read → act), fold the consecutive
            // leading Execute calls into the SAME candidate batch so the
            // executor runs them in order this turn (multi-tool-per-turn).
            // Folding lowers from the original tool calls, which retain their
            // arguments — `DeferredToolCall` is a lossy name+summary and cannot
            // be re-executed. Folding stops at the first non-Execute /
            // unlowerable call; those remain deferred hints for the next turn.
            // If the first call is itself terminal (yield/delegate/…), nothing
            // folds and every extra call is deferred, as before.
            let mut env = match lower_response_tool_call(response, 0) {
                NativeDecisionOutcome::Valid(env) => env,
                other => return other,
            };
            env.raw_text_fallback = response.text.as_deref().map(str::to_owned);

            if n > 1 {
                let mut deferred: Vec<super::native_types::DeferredToolCall> = Vec::new();
                if let Decision::Execute { candidates, .. } = &mut env.decision {
                    let mut still_folding = true;
                    for (index, tc) in response.tool_calls()[1..].iter().enumerate() {
                        if still_folding {
                            if let NativeDecisionOutcome::Valid(follow_env) =
                                lower_response_tool_call(response, index + 1)
                            {
                                if let Decision::Execute {
                                    candidates: more, ..
                                } = follow_env.decision
                                {
                                    candidates.candidates.extend(more.candidates);
                                    continue;
                                }
                            }
                            // Non-Execute or unlowerable → stop folding here;
                            // this call and everything after it is deferred.
                            still_folding = false;
                        }
                        deferred.push(super::native_types::DeferredToolCall {
                            name: tc.name.clone(),
                            summary: summarize_tool_call(tc),
                        });
                    }
                } else {
                    // First call is terminal — preserve original defer-hint behavior.
                    deferred = response.tool_calls()[1..]
                        .iter()
                        .map(|tc| super::native_types::DeferredToolCall {
                            name: tc.name.clone(),
                            summary: summarize_tool_call(tc),
                        })
                        .collect();
                }

                let folded = match &env.decision {
                    Decision::Execute { candidates, .. } => candidates.candidates.len(),
                    _ => 1,
                };
                if folded > 1 || !deferred.is_empty() {
                    let deferred_names: Vec<&str> =
                        deferred.iter().map(|d| d.name.as_str()).collect();
                    info!(
                        "Execution-native: model returned {} tool calls; folded {} into the \
                         in-turn batch (first='{}'), deferred {:?}",
                        n,
                        folded,
                        response.tool_calls()[0].name,
                        deferred_names
                    );
                }
                env.deferred_tool_calls = deferred;
            }

            NativeDecisionOutcome::Valid(env)
        },
    }
}

/// Lower a response-owned call using its private admission authority. Provider
/// adapters admit once at response construction; fixtures or future public
/// callers that provide an unchecked response still receive a full safe scan.
fn lower_response_tool_call(
    response: &ExecutionNativeResponse,
    index: usize,
) -> NativeDecisionOutcome {
    let tool_call = &response.tool_calls()[index];
    match response.cached_tool_argument_admission(index) {
        Some(CachedNativeToolArgumentAdmission::Accepted) => {
            lower_native_tool_call_after_admission(tool_call, TaskStateActionPolicy::Optional, None)
        },
        Some(CachedNativeToolArgumentAdmission::Rejected(error)) => {
            lower_native_tool_call_after_admission(
                tool_call,
                TaskStateActionPolicy::Optional,
                Some(error),
            )
        },
        None => lower_native_tool_call_with_policy(tool_call, TaskStateActionPolicy::Optional),
    }
}

/// Build a brief human-readable summary of a deferred tool call.
fn summarize_tool_call(tc: &ExecutionToolCall) -> String {
    match tc.name.as_str() {
        "browser" => {
            // Browser is an inner-loop macro tool; arguments are session-level
            // options such as connection_mode, not primitive browser commands.
            let mode = tc
                .arguments
                .get("connection_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("cdp");
            format!("browser(connection_mode={mode})")
        },
        "file" => {
            let op = tc
                .arguments
                .get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let path = tc
                .arguments
                .get("path")
                .or_else(|| tc.arguments.get("source"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("file({}, {})", op, path)
        },
        "http" => {
            let method = tc
                .arguments
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let url = tc
                .arguments
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let url_trunc: String = url.chars().take(60).collect();
            format!("http({} {})", method, url_trunc)
        },
        "bash" => {
            let cmd = tc
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let cmd_trunc: String = cmd.chars().take(60).collect();
            format!("bash({})", cmd_trunc)
        },
        "duckdb" => {
            let sql = tc
                .arguments
                .get("sql")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let sql_trunc: String = sql.chars().take(60).collect();
            format!("duckdb({})", sql_trunc)
        },
        name => format!("{}(…)", name),
    }
}

// =============================================================================
// Single tool-call lowering
// =============================================================================

/// Lowers a single native tool call into a [`NativeDecisionOutcome`].
///
/// Extracts common metadata first, then dispatches to the appropriate lowering
/// function based on tool name.
pub fn lower_native_tool_call(tool_call: &ExecutionToolCall) -> NativeDecisionOutcome {
    lower_native_tool_call_with_policy(tool_call, TaskStateActionPolicy::Optional)
}

/// Lower an inner-loop terminal control call.
///
/// Inner loops never see or emit the outer-loop `task_state_action`
/// contract, so this path deliberately ignores that metadata entirely.
pub fn lower_primitive_terminal_tool_call(tool_call: &ExecutionToolCall) -> NativeDecisionOutcome {
    lower_native_tool_call_with_policy(tool_call, TaskStateActionPolicy::Disabled)
}

fn lower_native_tool_call_with_policy(
    tool_call: &ExecutionToolCall,
    task_state_action_policy: TaskStateActionPolicy,
) -> NativeDecisionOutcome {
    let admission_error = native_tool_argument_admission_error(&tool_call.arguments);
    lower_native_tool_call_after_admission(
        tool_call,
        task_state_action_policy,
        admission_error.as_deref(),
    )
}

fn lower_native_tool_call_after_admission(
    tool_call: &ExecutionToolCall,
    task_state_action_policy: TaskStateActionPolicy,
    admission_error: Option<&str>,
) -> NativeDecisionOutcome {
    if let Some(error) = admission_error {
        return NativeDecisionOutcome::InvalidArguments {
            tool_name: tool_call.name.clone(),
            raw_arguments: rejected_raw_arguments(error),
            error: error.to_string(),
        };
    }

    let metadata = match extract_common_metadata(&tool_call.arguments, task_state_action_policy) {
        Ok(metadata) => metadata,
        Err(error) => {
            return NativeDecisionOutcome::InvalidArguments {
                tool_name: tool_call.name.clone(),
                raw_arguments: clone_json_iteratively(&tool_call.arguments),
                error,
            };
        },
    };

    let decision_result = match tool_call.name.as_str() {
        // Phase 0.8c-12: `yield` is the sole LLM terminal; the legacy
        // `goal_reached` / `cannot_proceed` LLM tools are no longer in
        // the catalog (outer or inner). The lowering aliases below
        // remain as a backward-compat safety net — they convert
        // legacy-named tool calls into `Decision::Yield` with an
        // appropriate `YieldDecision` payload, so persisted-history
        // replay and test fixtures that still use the old names keep
        // working.
        "goal_reached" => lower_goal_reached_alias(&tool_call.arguments),
        "cannot_proceed" => lower_cannot_proceed_alias(&tool_call.arguments),
        "need_user_input" => lower_need_user_input(&tool_call.arguments),
        "yield" => lower_yield(&tool_call.arguments),
        "delegate_to_agent" => lower_delegate_to_agent(&tool_call.arguments),
        "handover_to_agent" => lower_handover_to_agent(&tool_call.arguments),
        "spawn_sub_goal" => lower_spawn_sub_goal(&tool_call.arguments),
        // Universal-substrate compiled packs (memory, read-only
        // introspection, non-face-coupled task ops, workspace
        // artifacts). These used to be routed through ChatControl, but
        // that meant they no-op'd in the autonomous executor's
        // ChatControl arm. Now they fall through to `lower_pack_execute`
        // and dispatch through the standard `ExecutableAction::Pack`
        // path (which both chat and autonomous handle correctly via
        // the compiled provider). The chat fast-path
        // (`try_dispatch_compiled_pack`) still picks them up first in
        // chat mode without needing this lowering arm.
        //
        // Names listed here intentionally fall through to the pack
        // execute path via `lower_pack_execute` — they MUST be in
        // `UNIVERSAL_BACKEND_PACKS` (`native_integration.rs`) for the
        // catalog to surface them universally.
        // Rail A is fully retired (Phase 0.8c-11): every former
        // native lane (`file`, `http`, `bash`, `read_file`,
        // `write_file`) is now a compiled pack that falls through
        // to `lower_pack_execute` below. Same for Rail B (`edit_file`,
        // `glob`, `grep`, `tool_search`, `web_fetch`, `web_search`).
        // Everything else flows through the pack catalog. Inner-loop packs
        // (`browser`, `duckdb`, etc.) are recognised inside
        // `lower_pack_execute` and routed to their own dispatchers
        // (`execute_browser_action`, …) — no per-name match arm needed
        // here. When the dispatch swap lands ([P2-1/2/3] in the skills
        // migration plan), this catch-all will check for an installed
        // skill of the same name first and route to skill activation.
        other => {
            // Yutori emits bare native action names (`scroll`, `left_click`,
            // `drag`, …). Route them to the browser pack leaf (`browser__<action>`)
            // here at lowering so the action-allowlist gate, flat classification,
            // and dispatch all treat them as browser actions (the shim translates
            // the action at dispatch). Without this the bare name fails the
            // allowlist as a non-browser pack ("ActionNotAllowed").
            let routed: std::borrow::Cow<'_, str> =
                if crate::magician_v2::execution::primitive_dispatch::browser::yutori_translator::is_yutori_action(other) {
                    std::borrow::Cow::Owned(format!(
                        "{}__{other}",
                        crate::magician_v2::execution::primitive_dispatch::dispatch::BROWSER_PACK_NAME
                    ))
                } else {
                    std::borrow::Cow::Borrowed(other)
                };
            lower_pack_execute(
                &routed,
                &tool_call.arguments,
                Some(tool_call.id.as_str()),
                metadata.thinking.as_deref(),
            )
        },
    };

    match decision_result {
        Ok(decision) => {
            let mut raw_tool_call = Map::with_capacity(3);
            raw_tool_call.insert("id".to_string(), Value::String(tool_call.id.clone()));
            raw_tool_call.insert("name".to_string(), Value::String(tool_call.name.clone()));
            raw_tool_call.insert(
                "arguments".to_string(),
                clone_json_iteratively(&tool_call.arguments),
            );
            NativeDecisionOutcome::Valid(ExecutionDecisionEnvelope {
                decision,
                request_hover_discovery: metadata.request_hover_discovery,
                request_vision: metadata.request_vision,
                vision_reason: metadata.vision_reason,
                step_completed: metadata.step_completed,
                step_failed: metadata.step_failed,
                needs_plan_revision: metadata.needs_plan_revision,
                task_state_action: metadata.task_state_action,
                deferred_tool_calls: vec![],
                thinking: metadata.thinking,
                raw_tool_call: Some(Value::Object(raw_tool_call)),
                raw_text_fallback: None,
            })
        },
        Err(error) => NativeDecisionOutcome::InvalidArguments {
            tool_name: tool_call.name.clone(),
            raw_arguments: clone_json_iteratively(&tool_call.arguments),
            error,
        },
    }
}

// =============================================================================
// Control tool lowering
// =============================================================================

/// Lower the unified `yield` tool call into a `Decision::Yield`,
/// preserving the structured payload end-to-end. See
/// `docs/plans/2026-05-27-yield-decision-migration.md`.
///
/// The argument object deserialises directly into `YieldDecision` via
/// serde with `summary` as the only required field. Unknown fields are
/// tolerated (forward compatibility). The executor's `Decision::Yield`
/// handler runs `dispose_yield` and dispatches to the corresponding
/// outcome path with the full payload available to downstream
/// consumers (memory recording, event emission, orchestrator-side
/// stuck detection).
fn lower_yield(args: &Value) -> Result<Decision, String> {
    use crate::magician_v2::execution::agentic::yield_decision::YieldDecision;
    let payload: YieldDecision = serde_json::from_value(clone_json_iteratively(args))
        .map_err(|err| format!("yield arguments did not deserialise: {err}"))?;
    if payload.summary.trim().is_empty() {
        return Err("yield requires a non-empty `summary` field".to_string());
    }
    Ok(Decision::Yield { payload })
}

/// Backward-compat alias: lower a legacy `goal_reached(evidence, artifacts?)`
/// tool call to `Decision::Yield` with the evidence packed into
/// `summary` + `completed[0]` and a `Done` self-classification.
/// `dispose_yield` then resolves the shape to the `Completed`
/// disposition. The catalog no longer exposes `goal_reached` (see
/// Phase 0.8c-12) but persisted history and test fixtures may still
/// carry the old name.
fn lower_goal_reached_alias(args: &Value) -> Result<Decision, String> {
    use crate::magician_v2::execution::agentic::yield_decision::{
        YieldDecision, YieldSelfClassification,
    };
    let evidence = args
        .get("evidence")
        .and_then(|v| v.as_str())
        .map(String::from);
    let artifacts: Vec<Artifact> = args
        .get("artifacts")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| serde_json::from_value(clone_json_iteratively(a)).ok())
                .collect()
        })
        .unwrap_or_default();

    let summary = evidence
        .clone()
        .unwrap_or_else(|| "Goal reached.".to_string());
    let completed = match evidence.as_deref() {
        Some(text) if !text.trim().is_empty() => vec![text.to_string()],
        _ => Vec::new(),
    };
    Ok(Decision::Yield {
        payload: YieldDecision {
            summary,
            completed,
            open: Vec::new(),
            blockers: Vec::new(),
            artifacts,
            next_step_hint: None,
            self_classification: Some(YieldSelfClassification::Done),
            ..Default::default()
        },
    })
}

/// Backward-compat alias: lower a legacy `cannot_proceed(reason)`
/// tool call to `Decision::Yield` with the reason as both `summary`
/// and a single `Other` blocker, plus a `Blocked` self-classification.
/// `dispose_yield` resolves the empty-completed + non-empty-blocker
/// shape to the `Failed` disposition. Same backward-compat rationale
/// as `lower_goal_reached_alias`.
fn lower_cannot_proceed_alias(args: &Value) -> Result<Decision, String> {
    use crate::magician_v2::execution::agentic::yield_decision::{
        YieldBlocker, YieldBlockerKind, YieldDecision, YieldSelfClassification,
    };
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "cannot_proceed requires 'reason' field".to_string())?;
    Ok(Decision::Yield {
        payload: YieldDecision {
            summary: reason.clone(),
            completed: Vec::new(),
            open: Vec::new(),
            blockers: vec![YieldBlocker {
                kind: YieldBlockerKind::Other,
                description: reason,
            }],
            artifacts: Vec::new(),
            next_step_hint: None,
            self_classification: Some(YieldSelfClassification::Blocked),
            ..Default::default()
        },
    })
}

/// Build a legacy-style `evidence` string from a structured
/// `YieldDecision`. Currently consumed only by the unit tests below —
/// kept available for Phase 2.5 of the yield-decision migration, when
/// `AgenticOutcome` variants learn to carry an evidence string built
/// from the structured payload so the memory layer (tactical pattern T3 partial-
/// success classifier, which reads `COMPLETED: ... BLOCKED: ...`
/// markers) keeps working as yields flow into recording. Now live: the flat
/// `Decision::Yield` evidence gate (`executor.rs`) builds the evidence string
/// from it before running `goal_reached_rejection_reason`.
pub fn yield_evidence_string(
    payload: &crate::magician_v2::execution::agentic::yield_decision::YieldDecision,
) -> String {
    let mut buf = String::new();
    buf.push_str(payload.summary.trim());
    if !payload.completed.is_empty() {
        buf.push_str("\n\nCOMPLETED:\n");
        for item in &payload.completed {
            buf.push_str("- ");
            buf.push_str(item.trim());
            buf.push('\n');
        }
    }
    if !payload.open.is_empty() {
        buf.push_str("\nBLOCKED:\n");
        for item in &payload.open {
            buf.push_str("- ");
            buf.push_str(item.trim());
            buf.push('\n');
        }
    }
    if !payload.blockers.is_empty() {
        buf.push_str("\nblockers: ");
        let descs: Vec<String> = payload
            .blockers
            .iter()
            .map(|b| format!("{kind:?}: {desc}", kind = b.kind, desc = b.description))
            .collect();
        buf.push_str(&descs.join("; "));
    }
    if let Some(hint) = payload
        .next_step_hint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        buf.push_str("\n\nnext: ");
        buf.push_str(hint);
    }
    buf
}

fn lower_need_user_input(args: &Value) -> Result<Decision, String> {
    let question = args
        .get("question")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "need_user_input requires 'question' field".to_string())?;

    let input_type_str = args
        .get("input_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "need_user_input requires 'input_type' field".to_string())?;

    let hint = args.get("hint").and_then(|v| v.as_str()).map(String::from);

    // Parse options array (used for choice/multi_choice but also stored on Decision)
    let raw_options: Option<Vec<ChoiceOption>> =
        args.get("options").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    let id = o.get("id").and_then(|v| v.as_str())?.to_string();
                    let label = o.get("label").and_then(|v| v.as_str())?.to_string();
                    let description = o
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    Some(ChoiceOption {
                        id,
                        label,
                        description,
                    })
                })
                .collect()
        });

    let input_type = match input_type_str.to_lowercase().as_str() {
        "text" => UserInputType::Text {
            placeholder: None,
            multiline: false,
        },
        "password" => UserInputType::Password { placeholder: None },
        "otp" => UserInputType::Otp { placeholder: None },
        "choice" => {
            let opts = raw_options.clone().unwrap_or_default();
            UserInputType::Choice {
                options: opts,
                allow_other: false,
            }
        },
        "multi_choice" => {
            let opts = raw_options.clone().unwrap_or_default();
            UserInputType::MultiChoice {
                options: opts.clone(),
                min_selections: 0,
                max_selections: opts.len(), // default: all options selectable
            }
        },
        "confirmation" => UserInputType::Confirmation {
            confirm_label: None,
            deny_label: None,
            destructive: false,
        },
        "external_action" => UserInputType::ExternalAction {
            instructions: "Please complete the required action and click 'Done' when finished."
                .to_string(),
            done_label: None,
        },
        "file_path" => UserInputType::FilePath {
            filter: None,
            multiple: false,
        },
        "guidance" => UserInputType::Guidance {
            context: None,
            suggestions: None,
        },
        "form" => {
            let questions = parse_form_questions(args.get("questions"))?;
            if questions.is_empty() {
                return Err("form input_type requires a non-empty questions array".to_string());
            }
            UserInputType::Form { questions }
        },
        other => {
            return Err(format!("Unknown input_type: {}", other));
        },
    };

    let input_type = if let Some(questions) = args.get("questions") {
        if input_type_str != "form" {
            let parsed = parse_form_questions(Some(questions))?;
            if parsed.len() >= 2 {
                UserInputType::Form { questions: parsed }
            } else {
                input_type
            }
        } else {
            input_type
        }
    } else {
        input_type
    };

    Ok(Decision::NeedUserInput {
        question,
        input_type,
        hint,
        options: raw_options,
    })
}

fn parse_form_questions(value: Option<&Value>) -> Result<Vec<FormQuestion>, String> {
    let Some(arr) = value.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    if arr.len() > 3 {
        return Err("form questions are capped at 3".to_string());
    }
    let mut out = Vec::new();
    for entry in arr {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "form question missing id".to_string())?
            .to_string();
        let prompt = entry
            .get("prompt")
            .or_else(|| entry.get("question"))
            .and_then(Value::as_str)
            .ok_or_else(|| "form question missing prompt".to_string())?
            .to_string();
        let input_type = entry
            .get("input_type")
            .and_then(Value::as_str)
            .unwrap_or("text")
            .to_string();
        if !matches!(
            input_type.as_str(),
            "text" | "password" | "otp" | "choice" | "multi_choice"
        ) {
            return Err(format!(
                "form question {id} has unsupported input_type {input_type}"
            ));
        }
        let options = entry
            .get("options")
            .and_then(Value::as_array)
            .map(|opts| {
                opts.iter()
                    .filter_map(|option| {
                        let id = option.get("id").and_then(Value::as_str)?.to_string();
                        let label = option.get("label").and_then(Value::as_str)?.to_string();
                        Some(ChoiceOption {
                            id,
                            label,
                            description: option
                                .get("description")
                                .and_then(Value::as_str)
                                .map(String::from),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(FormQuestion {
            id,
            prompt,
            input_type,
            options,
        });
    }
    Ok(out)
}

fn lower_spawn_sub_goal(args: &Value) -> Result<Decision, String> {
    let goal = args
        .get("goal")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| "spawn_sub_goal requires non-empty 'goal' field".to_string())?;

    let unblocks = args
        .get("unblocks")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| {
            "spawn_sub_goal requires non-empty 'unblocks' field naming the specific parent step this sub-goal resolves. Without it, sub-goals tend to be paraphrases of the parent rather than decomposition.".to_string()
        })?;

    let budget_iterations = args
        .get("budget_iterations")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_SUB_GOAL_BUDGET)
        .clamp(1, MAX_SUB_GOAL_BUDGET);

    Ok(Decision::SpawnSubGoal {
        goal,
        unblocks,
        budget_iterations,
    })
}

fn lower_handover_to_agent(args: &Value) -> Result<Decision, String> {
    let target_agent_id = args
        .get("target_agent_id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "handover_to_agent requires 'target_agent_id' field".to_string())?;

    let context = args
        .get("context")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "handover_to_agent requires 'context' field".to_string())?;

    let preserve_live_execution_context = args
        .get("preserve_live_execution_context")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok(Decision::HandoverToAgent {
        target_agent_id,
        context,
        preserve_live_execution_context,
    })
}

fn lower_delegate_to_agent(args: &Value) -> Result<Decision, String> {
    let targets_val = args
        .get("delegation_targets")
        .ok_or_else(|| "delegate_to_agent requires 'delegation_targets' field".to_string())?;

    let targets: Vec<DelegationTargetRequest> =
        serde_json::from_value(clone_json_iteratively(targets_val))
            .map_err(|e| format!("Failed to parse delegation_targets: {}", e))?;

    Ok(Decision::DelegateToAgent { targets })
}

// =============================================================================
// Executable tool lowering
// =============================================================================

/// Build an `Execute` decision with optional credential context that
/// flows through `ActionCandidate::with_credential_id` /
/// `with_credential_token` to `prepare_action_with_secrets` at
/// dispatch time. Used by `lower_pack_execute` to lift
/// `credential_id` / `credential_token` from compiled-pack args onto
/// the candidate so credentialed packs (e.g. `http` against
/// authenticated endpoints) get auth resolved via the secret broker.
fn execute_decision_with_credentials(
    action: ExecutableAction,
    thinking: impl Into<String>,
    credential_id: Option<String>,
    credential_token: Option<String>,
    tool_call_id: Option<&str>,
) -> Decision {
    let thinking = thinking.into();
    let mut candidate = ActionCandidate::new(1, 0.9, action)
        .with_reasoning(thinking.clone())
        .with_criticality_hint("medium");
    if let Some(id) = credential_id {
        candidate = candidate.with_credential_id(id);
    }
    if let Some(token) = credential_token {
        candidate = candidate.with_credential_token(token);
    }
    // Carry the provider tool-call id so the live conversation can pair the
    // resulting tool result back to this exact assistant tool call.
    if let Some(id) = tool_call_id.filter(|s| !s.is_empty()) {
        candidate = candidate.with_tool_call_id(id);
    }
    Decision::Execute {
        candidates: CandidateBatch::new(vec![candidate], thinking.clone()),
        thinking,
    }
}

/// Metadata field names that should be stripped from pack tool args
/// before forwarding as capability parameters.
const METADATA_FIELD_NAMES: &[&str] = &[
    "thinking",
    DECISION_METADATA_FIELD,
    "request_hover_discovery",
    "request_vision",
    "vision_reason",
    "step_completed",
    "step_failed",
    "needs_plan_revision",
    "task_state_action",
];

fn lower_pack_execute(
    capability_name: &str,
    args: &Value,
    tool_call_id: Option<&str>,
    decision_rationale: Option<&str>,
) -> Result<Decision, String> {
    let mut resolved_params = HashMap::new();

    if let Some(obj) = args.as_object() {
        if let Some(params) = obj.get("parameters").and_then(Value::as_object) {
            for (key, val) in params {
                resolved_params.insert(key.clone(), clone_json_iteratively(val));
            }
        }
        for (key, val) in obj {
            if key != "parameters" && !METADATA_FIELD_NAMES.contains(&key.as_str()) {
                resolved_params.insert(key.clone(), clone_json_iteratively(val));
            }
        }
    }

    // Lift `credential_id` / `credential_token` from compiled-pack
    // args onto the `ActionCandidate` (via the wrapper below). This
    // re-establishes the credentialed-HTTP affordance lost when the
    // native `http` lane retired in Phase 0.8c-11 — the secret
    // broker reads these off the candidate at
    // `prepare_action_with_secrets` time to resolve the actual token.
    // Pop them out of `resolved_params` so the pack action body
    // doesn't see the references; they're handled at the candidate
    // layer.
    let credential_id = resolved_params
        .remove("credential_id")
        .and_then(|v| v.as_str().map(str::to_string));
    let credential_token = resolved_params
        .remove("credential_token")
        .and_then(|v| v.as_str().map(str::to_string));

    let decision_rationale = decision_rationale
        .map(str::to_string)
        .unwrap_or_else(|| format!("Native pack tool call: {capability_name}"));

    Ok(execute_decision_with_credentials(
        ExecutableAction::Pack {
            capability_name: capability_name.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: capability_name.to_string(),
            },
            resolved_params,
        },
        decision_rationale,
        credential_id,
        credential_token,
        tool_call_id,
    ))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    fn native_response_for_test(
        tool_calls: Vec<ExecutionToolCall>,
        text: Option<&str>,
        finish_reason: Option<&str>,
    ) -> ExecutionNativeResponse {
        let mut response = ExecutionNativeResponse::unadmitted(tool_calls);
        response.text = text.map(str::to_string);
        response.finish_reason = finish_reason.map(str::to_string);
        response
    }

    #[test]
    fn admitted_response_is_not_rescanned_during_production_lowering() {
        use crate::magician_v2::execution::agentic::native_types::{
            native_argument_admission_scan_count, reset_native_argument_admission_scan_count,
        };

        let mut response = native_response_for_test(
            vec![ExecutionToolCall {
                id: "one-pass".to_string(),
                name: "shell".to_string(),
                arguments: json!({"command": "pwd"}),
            }],
            None,
            Some("tool_calls"),
        );
        reset_native_argument_admission_scan_count();

        response.admit_tool_arguments();
        assert_eq!(native_argument_admission_scan_count(), 1);
        response.admit_tool_arguments();
        assert_eq!(
            native_argument_admission_scan_count(),
            1,
            "typed response admission is idempotent"
        );

        assert!(matches!(
            lower_native_response(&response),
            NativeDecisionOutcome::Valid(_)
        ));
        assert_eq!(
            native_argument_admission_scan_count(),
            1,
            "response lowering consumes private admission authority"
        );
    }

    #[test]
    fn deep_native_tool_arguments_fail_before_retained_clones_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| {
                let mut arguments = Value::Null;
                for _ in 0..10_000 {
                    arguments = Value::Array(vec![arguments]);
                }
                let call = ExecutionToolCall {
                    id: "deep-call".to_string(),
                    name: "read_file".to_string(),
                    arguments,
                };
                match lower_native_tool_call(&call) {
                    NativeDecisionOutcome::InvalidArguments {
                        raw_arguments,
                        error,
                        ..
                    } => {
                        assert_eq!(raw_arguments["_omitted"], true);
                        assert!(error.contains("depth"));
                    },
                    other => panic!("deep arguments must fail admission: {other:?}"),
                }
                let ExecutionToolCall { arguments, .. } = call;
                crate::magician_v2::json_traversal::discard_json_iteratively(arguments);
            })
            .expect("small-stack lowering worker")
            .join()
            .expect("deep argument rejection must remain stack safe");
    }

    #[test]
    fn wide_native_tool_arguments_fail_at_the_node_boundary() {
        let call = ExecutionToolCall {
            id: "wide-call".to_string(),
            name: "read_file".to_string(),
            arguments: Value::Array(
                (0..=crate::magician_v2::execution::agentic::native_types::MAX_NATIVE_TOOL_ARGUMENT_NODES)
                    .map(|_| Value::Null)
                    .collect(),
            ),
        };
        match lower_native_tool_call(&call) {
            NativeDecisionOutcome::InvalidArguments {
                raw_arguments,
                error,
                ..
            } => {
                assert_eq!(raw_arguments["_omitted"], true);
                assert!(error.contains("node limit"));
            },
            other => panic!("wide arguments must fail admission: {other:?}"),
        }
    }

    #[test]
    fn public_tool_call_lowering_does_not_trust_provider_rejection_fields() {
        let call = ExecutionToolCall {
            id: "reserved-field".to_string(),
            name: "yield".to_string(),
            arguments: json!({
                "__magician_argument_rejected": "x".repeat(
                    crate::magician_v2::execution::agentic::native_types::MAX_NATIVE_TOOL_ARGUMENT_BYTES
                ),
                "summary": "provider attempted to bypass admission",
            }),
        };

        match lower_native_tool_call(&call) {
            NativeDecisionOutcome::InvalidArguments { error, .. } => {
                assert!(error.contains("byte limit"));
            },
            other => panic!("public lowering must independently admit raw calls: {other:?}"),
        }
    }

    fn with_task_state_action(mut args: Value) -> Value {
        if let Some(obj) = args.as_object_mut() {
            obj.insert(
                "task_state_action".to_string(),
                json!({
                    "action": "none",
                    "reason": "No durable task-state change needed for this unit test."
                }),
            );
        }
        args
    }

    // === Response-level validation ===

    #[test]
    fn rejects_zero_tool_calls() {
        let response =
            native_response_for_test(vec![], Some("I'll click the button"), Some("stop"));
        match lower_native_response(&response) {
            NativeDecisionOutcome::ZeroToolCalls { text_fallback, .. } => {
                assert_eq!(text_fallback, Some("I'll click the button".into()));
            },
            other => panic!("Expected ZeroToolCalls, got {:?}", other),
        }
    }

    #[test]
    fn terminal_first_defers_following_calls() {
        let response = native_response_for_test(
            vec![
                ExecutionToolCall {
                    id: "1".into(),
                    name: "yield".into(),
                    arguments: with_task_state_action(json!({"summary": "blocked"})),
                },
                ExecutionToolCall {
                    id: "2".into(),
                    name: "yield".into(),
                    arguments: with_task_state_action(json!({"summary": "extra"})),
                },
            ],
            None,
            Some("tool_calls"),
        );
        // Should process the first tool call, not reject
        match lower_native_response(&response) {
            NativeDecisionOutcome::Valid(env) => {
                assert!(matches!(env.decision, Decision::Yield { .. }));
                assert_eq!(env.deferred_tool_calls.len(), 1);
                assert_eq!(env.deferred_tool_calls[0].name, "yield");
            },
            other => panic!("Expected Valid from first tool call, got {:?}", other),
        }
    }

    #[test]
    fn accepts_single_valid_tool_call() {
        let response = native_response_for_test(
            vec![ExecutionToolCall {
                id: "1".into(),
                name: "yield".into(),
                arguments: with_task_state_action(json!({"summary": "blocked"})),
            }],
            Some("some text"),
            Some("tool_calls"),
        );
        match lower_native_response(&response) {
            NativeDecisionOutcome::Valid(env) => {
                assert!(matches!(env.decision, Decision::Yield { .. }));
                assert_eq!(env.raw_text_fallback, Some("some text".into()));
            },
            _ => panic!("Expected Valid"),
        }
    }

    // === Common metadata extraction ===

    #[test]
    fn decision_metadata_sidecar_preserves_execution_signals() {
        let tc = ExecutionToolCall {
            id: "tc1".into(),
            name: "yield".into(),
            arguments: json!({
                "summary": "blocked",
                "thinking": "I tried everything",
                "decision_metadata": {
                    "request_hover_discovery": true,
                    "request_vision": true,
                    "vision_reason": "need to see the page",
                    "step_completed": "step-1",
                    "step_failed": "step-2",
                    "needs_plan_revision": true
                },
                "task_state_action": {
                    "action": "patch",
                    "reason": "The current durable task step is blocked.",
                    "confidence": 0.8,
                    "patch": {"ops": []}
                }
            }),
        };
        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::Valid(env) => {
                assert_eq!(env.step_failed, Some("step-2".into()));
                assert_eq!(env.step_completed, Some("step-1".into()));
                assert!(env.needs_plan_revision);
                assert_eq!(env.thinking, Some("I tried everything".into()));
                assert_eq!(env.request_hover_discovery, Some(true));
                assert_eq!(env.request_vision, Some(true));
                assert_eq!(env.vision_reason, Some("need to see the page".into()));
                assert_eq!(env.task_state_action.action.as_str(), "patch");
                assert_eq!(
                    env.task_state_action.reason,
                    "The current durable task step is blocked."
                );
            },
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn decision_metadata_legacy_flat_signals_remain_compatible() {
        let tc = ExecutionToolCall {
            id: "tc-legacy".into(),
            name: "yield".into(),
            arguments: json!({
                "summary": "blocked",
                "request_hover_discovery": true,
                "request_vision": true,
                "vision_reason": "legacy vision reason",
                "step_completed": "step-legacy-complete",
                "step_failed": "step-legacy-failed",
                "needs_plan_revision": true
            }),
        };

        let NativeDecisionOutcome::Valid(env) = lower_native_tool_call(&tc) else {
            panic!("expected legacy flat metadata to remain valid");
        };
        assert_eq!(env.request_hover_discovery, Some(true));
        assert_eq!(env.request_vision, Some(true));
        assert_eq!(env.vision_reason.as_deref(), Some("legacy vision reason"));
        assert_eq!(env.step_completed.as_deref(), Some("step-legacy-complete"));
        assert_eq!(env.step_failed.as_deref(), Some("step-legacy-failed"));
        assert!(env.needs_plan_revision);
    }

    #[test]
    fn decision_metadata_equal_nested_and_legacy_values_are_accepted() {
        let tc = ExecutionToolCall {
            id: "tc-equal".into(),
            name: "yield".into(),
            arguments: json!({
                "summary": "done",
                "decision_metadata": {"step_completed": "step-7"},
                "step_completed": "step-7"
            }),
        };

        let NativeDecisionOutcome::Valid(env) = lower_native_tool_call(&tc) else {
            panic!("equal nested and legacy metadata should be accepted");
        };
        assert_eq!(env.step_completed.as_deref(), Some("step-7"));
    }

    #[test]
    fn decision_metadata_conflicting_nested_and_legacy_values_fail_closed() {
        let tc = ExecutionToolCall {
            id: "tc-conflict".into(),
            name: "yield".into(),
            arguments: json!({
                "summary": "done",
                "decision_metadata": {"step_completed": "step-7"},
                "step_completed": "step-8"
            }),
        };

        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::InvalidArguments { error, .. } => {
                assert!(error.contains("conflicting `step_completed`"));
            },
            other => panic!("expected conflict rejection, got {other:?}"),
        }
    }

    #[test]
    fn decision_metadata_rejects_non_object_sidecar() {
        let tc = ExecutionToolCall {
            id: "tc-invalid-sidecar".into(),
            name: "yield".into(),
            arguments: json!({
                "summary": "done",
                "decision_metadata": true
            }),
        };

        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::InvalidArguments { error, .. } => {
                assert!(error.contains("`decision_metadata` must be an object"));
            },
            other => panic!("expected invalid sidecar rejection, got {other:?}"),
        }
    }

    #[test]
    fn outer_native_lowering_defaults_missing_task_state_action_to_none() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "yield".into(),
            arguments: json!({"summary": "blocked"}),
        };

        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::Valid(env) => {
                assert!(matches!(env.decision, Decision::Yield { .. }));
                assert_eq!(env.task_state_action.action.as_str(), "none");
                assert_eq!(
                    env.task_state_action.reason,
                    "Durable task state unchanged."
                );
            },
            other => panic!("Expected Valid, got {:?}", other),
        }
    }

    #[test]
    fn outer_native_lowering_rejects_malformed_present_task_state_action() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "yield".into(),
            arguments: json!({
                "summary": "blocked",
                "task_state_action": {"action": "patch"}
            }),
        };

        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::InvalidArguments { error, .. } => {
                assert!(error.contains("invalid task_state_action envelope"));
            },
            other => panic!("Expected InvalidArguments, got {:?}", other),
        }
    }

    #[test]
    fn primitive_terminal_lowering_does_not_require_task_state_action() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "yield".into(),
            arguments: json!({"summary": "blocked"}),
        };

        match lower_primitive_terminal_tool_call(&tc) {
            NativeDecisionOutcome::Valid(env) => {
                assert!(matches!(env.decision, Decision::Yield { .. }));
                assert_eq!(env.task_state_action.action.as_str(), "none");
            },
            other => panic!("Expected Valid, got {:?}", other),
        }
    }

    // === Control tools ===
    //
    // Phase 0.8c-12: the LLM-facing `goal_reached` / `cannot_proceed`
    // terminal tools are retired. `yield` is the unified terminal —
    // see `lower_yield_*` tests below for coverage.

    #[test]
    fn retired_goal_reached_alias_still_lowers_persisted_history() {
        use crate::magician_v2::execution::agentic::yield_decision::YieldSelfClassification;

        let tc = ExecutionToolCall {
            id: "persisted-goal-reached".into(),
            name: "goal_reached".into(),
            arguments: json!({"evidence": "Legacy run finished"}),
        };
        let NativeDecisionOutcome::Valid(env) = lower_native_tool_call(&tc) else {
            panic!("persisted goal_reached call must remain compatible");
        };
        let Decision::Yield { payload } = env.decision else {
            panic!("legacy completion must lower to Decision::Yield");
        };
        assert_eq!(payload.summary, "Legacy run finished");
        assert_eq!(payload.completed, vec!["Legacy run finished"]);
        assert!(matches!(
            payload.self_classification,
            Some(YieldSelfClassification::Done)
        ));
    }

    #[test]
    fn retired_cannot_proceed_alias_still_lowers_persisted_history() {
        use crate::magician_v2::execution::agentic::yield_decision::{
            YieldBlockerKind, YieldSelfClassification,
        };

        let tc = ExecutionToolCall {
            id: "persisted-cannot-proceed".into(),
            name: "cannot_proceed".into(),
            arguments: json!({"reason": "Legacy dependency unavailable"}),
        };
        let NativeDecisionOutcome::Valid(env) = lower_native_tool_call(&tc) else {
            panic!("persisted cannot_proceed call must remain compatible");
        };
        let Decision::Yield { payload } = env.decision else {
            panic!("legacy failure must lower to Decision::Yield");
        };
        assert_eq!(payload.summary, "Legacy dependency unavailable");
        assert_eq!(payload.blockers.len(), 1);
        assert!(matches!(payload.blockers[0].kind, YieldBlockerKind::Other));
        assert!(matches!(
            payload.self_classification,
            Some(YieldSelfClassification::Blocked)
        ));
    }

    #[test]
    fn lower_need_user_input_with_options() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "need_user_input".into(),
            arguments: with_task_state_action(json!({
                "question": "Which account?",
                "input_type": "choice",
                "options": [
                    {"id": "a", "label": "Account A"},
                    {"id": "b", "label": "Account B", "description": "Secondary"}
                ]
            })),
        };
        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::Valid(env) => {
                if let Decision::NeedUserInput {
                    question,
                    input_type,
                    options,
                    ..
                } = &env.decision
                {
                    assert_eq!(question, "Which account?");
                    assert!(matches!(input_type, UserInputType::Choice { .. }));
                    assert_eq!(options.as_ref().unwrap().len(), 2);
                } else {
                    panic!("Wrong variant");
                }
            },
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn lower_need_user_input_form_from_questions_array() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "need_user_input".into(),
            arguments: with_task_state_action(json!({
                "question": "A few things first",
                "input_type": "text",
                "questions": [
                    {"id": "release", "prompt": "Which channel?"},
                    {"id": "owner", "prompt": "Who owns it?"}
                ]
            })),
        };
        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::Valid(env) => {
                let Decision::NeedUserInput { input_type, .. } = env.decision else {
                    panic!("expected NeedUserInput");
                };
                let UserInputType::Form { questions } = input_type else {
                    panic!("two questions must lower to Form");
                };
                assert_eq!(questions.len(), 2);
                assert_eq!(questions[0].id, "release");
                assert_eq!(questions[1].id, "owner");
            },
            _ => panic!("Expected Valid"),
        }
    }

    // === P3 Task 3.4: typed one-time codes and typed form kinds ===

    #[test]
    fn lower_need_user_input_otp_and_typed_form_kinds() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "need_user_input".into(),
            arguments: with_task_state_action(json!({
                "question": "Enter the code we sent",
                "input_type": "otp"
            })),
        };
        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::Valid(env) => {
                let Decision::NeedUserInput { input_type, .. } = env.decision else {
                    panic!("expected NeedUserInput");
                };
                assert_eq!(input_type, UserInputType::Otp { placeholder: None });
            },
            _ => panic!("Expected Valid"),
        }

        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "need_user_input".into(),
            arguments: with_task_state_action(json!({
                "question": "Sign in",
                "input_type": "form",
                "questions": [
                    {"id": "user", "prompt": "Username"},
                    {"id": "pw", "prompt": "Password", "input_type": "password"},
                    {"id": "code", "prompt": "Code", "input_type": "otp"}
                ]
            })),
        };
        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::Valid(env) => {
                let Decision::NeedUserInput { input_type, .. } = env.decision else {
                    panic!("expected NeedUserInput");
                };
                let UserInputType::Form { questions } = input_type else {
                    panic!("expected Form");
                };
                let kinds: Vec<&str> = questions.iter().map(|q| q.input_type.as_str()).collect();
                assert_eq!(kinds, ["text", "password", "otp"]);
            },
            _ => panic!("Expected Valid"),
        }
    }

    // === Yield lowering ===
    // The lowering now preserves the structured payload — disposition
    // is computed in the executor's Yield handler. These tests pin the
    // payload-through-lowering contract and the evidence-string helper.

    #[test]
    fn lower_yield_preserves_structured_payload() {
        // Artifact data must arrive base64-encoded on the wire — the
        // Artifact struct uses `base64_serde` for the `data` field.
        // "a,b\n1,2" base64-encodes to "YSxiCjEsMg==".
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "yield".into(),
            arguments: with_task_state_action(json!({
                "summary": "Half done",
                "completed": ["pulled data"],
                "open": ["render report"],
                "artifacts": [{
                    "name": "data.csv",
                    "content_type": "text/csv",
                    "data": "YSxiCjEsMg=="
                }]
            })),
        };
        let NativeDecisionOutcome::Valid(env) = lower_native_tool_call(&tc) else {
            panic!("expected Valid");
        };
        let Decision::Yield { payload } = &env.decision else {
            panic!("expected Decision::Yield, got {:?}", env.decision);
        };
        assert_eq!(payload.summary, "Half done");
        assert_eq!(payload.completed, vec!["pulled data".to_string()]);
        assert_eq!(payload.open, vec!["render report".to_string()]);
        assert_eq!(payload.artifacts.len(), 1);
        assert_eq!(payload.artifacts[0].name, "data.csv");
    }

    #[test]
    fn lower_yield_parses_browser_handoff_flags() {
        // window-open flag flows through to the payload.
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "yield".into(),
            arguments: with_task_state_action(json!({
                "summary": "done; leaving the page for review",
                "keep_browser_window_open": true
            })),
        };
        let NativeDecisionOutcome::Valid(env) = lower_native_tool_call(&tc) else {
            panic!("expected Valid");
        };
        let Decision::Yield { payload } = &env.decision else {
            panic!("expected Decision::Yield");
        };
        assert!(payload.keep_browser_window_open);
        assert!(!payload.keep_browser_cdp_connection_alive);
    }

    #[test]
    fn lower_yield_accepts_legacy_keep_browser_session_alive_alias() {
        // The legacy `keep_browser_session_alive` name maps onto
        // `keep_browser_cdp_connection_alive` via #[serde(alias)].
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "yield".into(),
            arguments: with_task_state_action(json!({
                "summary": "done; keeping session for a follow-up",
                "keep_browser_session_alive": true
            })),
        };
        let NativeDecisionOutcome::Valid(env) = lower_native_tool_call(&tc) else {
            panic!("expected Valid");
        };
        let Decision::Yield { payload } = &env.decision else {
            panic!("expected Decision::Yield");
        };
        assert!(payload.keep_browser_cdp_connection_alive);
        assert!(!payload.keep_browser_window_open);
    }

    #[test]
    fn lower_yield_preserves_blockers_with_kind_taxonomy() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "yield".into(),
            arguments: with_task_state_action(json!({
                "summary": "Auth missing",
                "blockers": [
                    {"kind": "auth", "description": "gws token expired"},
                    {"kind": "transient", "description": "429"}
                ]
            })),
        };
        let NativeDecisionOutcome::Valid(env) = lower_native_tool_call(&tc) else {
            panic!("expected Valid");
        };
        let Decision::Yield { payload } = &env.decision else {
            panic!("expected Decision::Yield");
        };
        assert_eq!(payload.blockers.len(), 2);
        use crate::magician_v2::execution::agentic::yield_decision::YieldBlockerKind;
        assert!(matches!(payload.blockers[0].kind, YieldBlockerKind::Auth));
        assert!(matches!(
            payload.blockers[1].kind,
            YieldBlockerKind::Transient
        ));
    }

    #[test]
    fn lower_yield_rejects_empty_summary() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "yield".into(),
            arguments: with_task_state_action(json!({
                "summary": "   "
            })),
        };
        assert!(matches!(
            lower_native_tool_call(&tc),
            NativeDecisionOutcome::InvalidArguments { .. }
        ));
    }

    #[test]
    fn yield_evidence_string_includes_completed_blocked_markers() {
        use crate::magician_v2::execution::agentic::yield_decision::{
            YieldBlocker, YieldBlockerKind, YieldDecision,
        };
        let payload = YieldDecision {
            summary: "Half done".to_string(),
            completed: vec!["Q1".to_string()],
            open: vec!["Q2".to_string()],
            blockers: vec![YieldBlocker {
                kind: YieldBlockerKind::DataMissing,
                description: "no Q2 data".to_string(),
            }],
            next_step_hint: Some("retry tomorrow".to_string()),
            ..Default::default()
        };
        let evidence = super::yield_evidence_string(&payload);
        assert!(evidence.contains("Half done"));
        assert!(evidence.contains("COMPLETED:"));
        assert!(evidence.contains("Q1"));
        assert!(evidence.contains("BLOCKED:"));
        assert!(evidence.contains("Q2"));
        assert!(evidence.contains("DataMissing"));
        assert!(evidence.contains("no Q2 data"));
        assert!(evidence.contains("next: retry tomorrow"));
    }

    #[test]
    fn lower_spawn_sub_goal_clamps_budget() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "spawn_sub_goal".into(),
            arguments: with_task_state_action(json!({
                "goal": "check prices",
                "unblocks": "need current price reference",
                "budget_iterations": 999
            })),
        };
        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::Valid(env) => {
                if let Decision::SpawnSubGoal {
                    budget_iterations, ..
                } = &env.decision
                {
                    assert!(*budget_iterations <= 400);
                } else {
                    panic!("Expected SpawnSubGoal");
                }
            },
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn lower_handover_to_agent() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "handover_to_agent".into(),
            arguments: with_task_state_action(json!({
                "target_agent_id": "agent-search",
                "context": "Find pricing info",
                "preserve_live_execution_context": true
            })),
        };
        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::Valid(env) => {
                assert!(matches!(env.decision, Decision::HandoverToAgent { .. }));
            },
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn lower_delegate_to_agent() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "delegate_to_agent".into(),
            arguments: with_task_state_action(json!({
                "delegation_targets": [{
                    "target_agent_id": "agent-research",
                    "context": "Research competitors"
                }]
            })),
        };
        match lower_native_tool_call(&tc) {
            NativeDecisionOutcome::Valid(env) => {
                if let Decision::DelegateToAgent { targets } = &env.decision {
                    assert_eq!(targets.len(), 1);
                    assert_eq!(targets[0].target_agent_id, "agent-research");
                } else {
                    panic!("Expected DelegateToAgent");
                }
            },
            _ => panic!("Expected Valid"),
        }
    }

    // Note: `create_task` / `list_tasks` / `run_task` / `stop_task` no
    // longer have bespoke native_lowering arms — they fall through to
    // `lower_pack_execute` and dispatch via the compiled-handler
    // registry like every other agentic tool. See
    // `compiled_handlers/create_task.rs` etc.

    // === Executable tools ===
    //
    // Phase 0.8c-11: `file` / `http` / `bash` native-lane lowering
    // arms + their bespoke tests removed. The names are now compiled
    // packs (`files` / `http` / `shell`) that dispatch through
    // `lower_pack_execute` (the catch-all `other =>` arm) like any
    // other pack. Their schemas + lowering are validated at boot via
    // `embedded_compiled_pack_defs` parsing, so a tool-specific test
    // here would just duplicate that coverage.

    #[test]
    fn lower_duckdb_query() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "duckdb".into(),
            arguments: with_task_state_action(json!({"sql": "SELECT 1"})),
        };
        assert!(matches!(
            lower_native_tool_call(&tc),
            NativeDecisionOutcome::Valid(_)
        ));
    }

    #[test]
    fn lower_pack_decision_metadata_is_stripped_and_rationale_is_preserved() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "send_email".into(),
            arguments: with_task_state_action(json!({
                "parameters": {"to": "test@example.com"},
                "thinking": "Send the prepared message to the requested recipient.",
                "decision_metadata": {"step_completed": "step-send"}
            })),
        };
        let result = lower_native_tool_call(&tc);
        match result {
            NativeDecisionOutcome::Valid(env) => {
                let Decision::Execute {
                    candidates,
                    thinking,
                } = env.decision
                else {
                    panic!("expected execute decision");
                };
                assert_eq!(
                    thinking,
                    "Send the prepared message to the requested recipient."
                );
                assert_eq!(candidates.thinking, thinking);
                assert_eq!(candidates.candidates[0].reasoning, thinking);
                match &candidates.candidates[0].action {
                    ExecutableAction::Pack {
                        capability_name,
                        resolved_params,
                        ..
                    } => {
                        assert_eq!(capability_name, "send_email");
                        assert_eq!(resolved_params["to"], "test@example.com");
                        assert!(!resolved_params.contains_key(DECISION_METADATA_FIELD));
                    },
                    other => panic!("expected pack action, got {other:?}"),
                }
            },
            other => panic!("expected valid pack lowering, got {other:?}"),
        }
    }

    #[test]
    fn lower_pack_decision_rationale_uses_stable_fallback_when_omitted() {
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "shell".into(),
            arguments: json!({"command": "pwd"}),
        };

        let NativeDecisionOutcome::Valid(env) = lower_native_tool_call(&tc) else {
            panic!("expected valid pack lowering");
        };
        let Decision::Execute {
            candidates,
            thinking,
        } = env.decision
        else {
            panic!("expected execute decision");
        };
        assert_eq!(thinking, "Native pack tool call: shell");
        assert_eq!(candidates.thinking, thinking);
        assert_eq!(candidates.candidates[0].reasoning, thinking);
    }

    #[test]
    fn lower_pack_bounds_decision_rationale_at_runtime() {
        let overlong = "x".repeat(MAX_DECISION_RATIONALE_CHARS + 20);
        let tc = ExecutionToolCall {
            id: "tc".into(),
            name: "shell".into(),
            arguments: json!({"command": "pwd", "thinking": overlong}),
        };

        let NativeDecisionOutcome::Valid(env) = lower_native_tool_call(&tc) else {
            panic!("expected valid pack lowering");
        };
        let envelope_thinking = env.thinking.clone();
        let Decision::Execute { thinking, .. } = env.decision else {
            panic!("expected execute decision");
        };
        assert_eq!(thinking.chars().count(), MAX_DECISION_RATIONALE_CHARS);
        assert!(thinking.ends_with('…'));
        assert_eq!(envelope_thinking.as_deref(), Some(thinking.as_str()));
    }

    #[test]
    fn folded_pack_calls_keep_per_candidate_decision_rationales() {
        let response = native_response_for_test(
            vec![
                ExecutionToolCall {
                    id: "tc-1".into(),
                    name: "shell".into(),
                    arguments: json!({"command": "do-work", "thinking": "Perform the change."}),
                },
                ExecutionToolCall {
                    id: "tc-2".into(),
                    name: "shell".into(),
                    arguments: json!({"command": "verify-work", "thinking": "Verify the changed state."}),
                },
            ],
            None,
            Some("tool_calls"),
        );

        let NativeDecisionOutcome::Valid(env) = lower_native_response(&response) else {
            panic!("expected valid multi-tool lowering");
        };
        let Decision::Execute { candidates, .. } = env.decision else {
            panic!("expected execute decision");
        };
        assert_eq!(candidates.candidates.len(), 2);
        assert_eq!(candidates.candidates[0].reasoning, "Perform the change.");
        assert_eq!(
            candidates.candidates[1].reasoning,
            "Verify the changed state."
        );
    }

    #[test]
    fn pack_then_yield_folds_the_pack_and_defers_yield() {
        let response = native_response_for_test(
            vec![
                ExecutionToolCall {
                    id: "tc-1".into(),
                    name: "grep".into(),
                    arguments: json!({"pattern": "foo", "path": "."}),
                },
                ExecutionToolCall {
                    id: "tc-2".into(),
                    name: "yield".into(),
                    arguments: with_task_state_action(json!({"summary": "done looking"})),
                },
            ],
            None,
            Some("tool_calls"),
        );

        let NativeDecisionOutcome::Valid(env) = lower_native_response(&response) else {
            panic!("expected valid pack-then-yield lowering");
        };
        let Decision::Execute { candidates, .. } = env.decision else {
            panic!("expected execute decision for the leading pack call");
        };
        assert_eq!(candidates.candidates.len(), 1);
        assert_eq!(env.deferred_tool_calls.len(), 1);
        assert_eq!(env.deferred_tool_calls[0].name, "yield");
    }
}
