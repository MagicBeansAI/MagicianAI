//! Governed `tools/call` for the Magician plane.
//!
//! Task 4 of `docs/plans/2026-08-23-magician-plane-vertical-slice-plan.md`.
//! Every name other than `tool_search` lowers through `lower_native_tool_call`
//! and, when the grant carries live executors, runs `execute_action` — the
//! same entry the loop uses. Pack confirmation is **not** inside that entry;
//! the plane runs it itself after lowering and before dispatch. The one
//! other exception is a conversation grant's bridged names: the native chat
//! mouth's own tools, dispatched through the grant's `ChatMouthBridge`
//! (`bridged_dispatch`) under the native dispatcher's own rules.
//!
//! Course-corrections against the plan sketch:
//! - `NativeDecisionOutcome` is `Valid(ExecutionDecisionEnvelope)`, not a
//!   `Decision` variant. Execute is `Decision::Execute { candidates, .. }`.
//! - `execute_action` now takes `effect_id`, coding session ids, and returns
//!   a boxed future.
//! - `refresh_trust_dispatch_guard_for_decision` mutates `AgenticContext`.
//! - Terminal approval captures belong to the originating HTTP continuation.

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::config::PlaneConfig;
use crate::magician_v2::agents::approval::{ApprovalGate, ApprovalResult, ExecutionPlan};
use crate::magician_v2::agents::InvocationSurface;
use crate::magician_v2::execution::actions::{ActionResult, ExecutableAction};
use crate::magician_v2::execution::agentic::native_lowering::lower_native_tool_call;
use crate::magician_v2::execution::agentic::native_types::{
    ExecutionToolCall, NativeDecisionOutcome,
};
use crate::magician_v2::execution::agentic::types::action_signature;
use crate::magician_v2::execution::agentic::{
    approval_step_from_candidate, checked_approval, execute_action,
    refresh_trust_dispatch_guard_for_decision, Decision,
};
use crate::magician_v2::execution::compiled_dispatch::is_governed_app_compiled_tool;
use crate::magician_v2::execution::plane::catalog::{
    is_callable_configured, plane_tool_search, PLANE_CONTROL_VERBS,
};
use crate::magician_v2::execution::plane::engine_pin::with_launching_run_engine_pin;
use crate::magician_v2::execution::plane::grant::{
    head_within, mint_invocation_ref, PlaneGrant, PlanePauseDisposition, PlanePendingApproval,
    PlaneTurnStopReason, PlaneTurnToolCall, PLANE_TURN_RESULT_CUT_MARK,
    PLANE_TURN_RESULT_MAX_BYTES,
};
use crate::magician_v2::execution::plane::terminal_ledger::TerminalLedgerOutcome;
use crate::magician_v2::execution::runtime_boundary::spawn_execution_job;
use crate::magician_v2::execution::verified_executor::types::ActionCandidate;
use crate::magician_v2::query_analysis::parent_engine::with_parent_engine;
use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;
use crate::magician_v2::secrets::sanitize_json_for_provider;
use crate::magician_v2::RuntimeTransportEvent;

/// MCP `tools/call` for one grant.
pub async fn plane_tools_call(grant: &PlaneGrant, tool: &str, arguments: &Value) -> Value {
    plane_tools_call_configured(grant, None, None, tool, arguments).await
}

/// Config-aware MCP `tools/call`. Authorization uses the same projected hot +
/// grant-loaded set returned by `tools/list`; catalog tiering must be enforced
/// at dispatch, not treated as a discoverability hint.
pub async fn plane_tools_call_configured(
    grant: &PlaneGrant,
    config: Option<&PlaneConfig>,
    session_id: Option<&str>,
    tool: &str,
    arguments: &Value,
) -> Value {
    let _dispatch_guard = grant.lock_dispatch().await;
    // The whole call runs under the grant's parent engine, so every LLM
    // operation it reaches — a compiled tool's own calls, the native
    // dispatcher behind a bridged name — follows the engine that started
    // the flow rather than the process default.
    // And under the grant's launch pin: a run the call launches inherits the
    // chat mouth (conversation grant) or the run (run grant) it came from.
    with_launching_run_engine_pin(
        grant.ctx.run_engine_pin.clone(),
        with_parent_engine(
            grant_parent_engine(grant).as_deref(),
            plane_tools_call_under_dispatch_lock(grant, config, session_id, tool, arguments),
        ),
    )
    .await
}

/// The parent engine of a grant's calls: what the grant's context names on
/// its routing overrides — the chat mouth for a conversation grant, the run
/// engine for a run grant, whatever a terminal session sets on its grant
/// (none until it does) — or none.
pub(crate) fn grant_parent_engine(grant: &PlaneGrant) -> Option<String> {
    grant
        .ctx
        .llm_routing_overrides
        .as_ref()
        .and_then(|overrides| overrides.parent_engine.clone())
}

/// The call proper, under the grant's dispatch lock and parent-engine scope.
async fn plane_tools_call_under_dispatch_lock(
    grant: &PlaneGrant,
    config: Option<&PlaneConfig>,
    session_id: Option<&str>,
    tool: &str,
    arguments: &Value,
) -> Value {
    // The runless caller's ledger (Task 12b): MCP sessions only. A harness
    // turn's calls belong to its run's record, not to a terminal audit trail.
    let ledger_session = session_id.map(str::to_string);
    let record_ledger =
        |outcome: crate::magician_v2::execution::plane::terminal_ledger::TerminalLedgerOutcome| {
            if let Some(session_id) = ledger_session.as_deref() {
                crate::magician_v2::execution::plane::terminal_ledger::record(
                    session_id,
                    crate::magician_v2::execution::plane::terminal_ledger::TerminalLedgerEntry {
                        tool: tool.to_string(),
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                        outcome,
                    },
                );
            }
        };
    // `resolve` clones the grant before this await. Registry removal alone
    // cannot invalidate a clone that was already queued behind another call.
    if grant.is_revoked() {
        record_ledger(TerminalLedgerOutcome::Refused {
            reason: "revoked".to_string(),
        });
        return revoked_grant_error();
    }
    if let Some(reason) = grant.turn_stop_reason() {
        let (text, code) = match reason {
            PlaneTurnStopReason::NeedsApproval => (
                "this harness turn is paused for approval; no further tool calls are accepted",
                "needs_approval",
            ),
            PlaneTurnStopReason::TurnBudgetSpent => (
                "this harness turn's tool-call bound is spent; the turn is ending",
                "turn_budget_spent",
            ),
            PlaneTurnStopReason::Delegate => (
                "this harness turn is ending to run the delegation it asked for; no further tool calls are accepted",
                "delegate",
            ),
        };
        record_ledger(TerminalLedgerOutcome::Refused {
            reason: "turn_paused".to_string(),
        });
        return mcp_error_with_meta(text, json!({ "planeTurnStop": code }));
    }
    // A control verb the native mouth bridges (`read_result`) is the
    // mouth's own tool on this grant, not the loop's verb.
    if (is_control_verb(tool) && !grant.bridged_tools.contains_key(tool))
        || !grant.permits(tool)
        || !is_callable_configured(grant, config, tool)
    {
        record_ledger(TerminalLedgerOutcome::Refused {
            reason: "not_available".to_string(),
        });
        return mcp_error(format!("{tool} is not available on the plane"));
    }
    if tool == "tool_search" {
        return plane_tool_search(grant.tool_index.as_ref(), grant, arguments).await;
    }
    // The caller's own audit trail (Task 12b) — a read of this session's
    // ledger, never another session's.
    if tool == "session_ledger" {
        return match session_id {
            Some(session_id) => crate::magician_v2::execution::plane::terminal_ledger::ledger_as_mcp_result(session_id),
            None => mcp_error("session_ledger requires an MCP session; a harness turn's calls belong to its run's record"),
        };
    }

    // A bridged name is the native mouth's own tool: it crosses to the chat
    // service's dispatcher with the turn's context and never reaches
    // lowering, the pause gate, or the plane's executors. Before `run_task`
    // on purpose — a bridged `run_task` is the native mouth's, not a
    // delegated plane launch.
    if grant.bridged_tools.contains_key(tool) {
        let (response, outcome) = bridged_dispatch(grant, tool, arguments).await;
        record_ledger(outcome);
        return response;
    }

    // Delegated runs are a launch, not an ordinary tool effect (Task 6b):
    // authority, engine launchability, and attenuation live in run_ownership,
    // before any lowering or gate work.
    if tool == "run_task" {
        return crate::magician_v2::execution::plane::run_ownership::plane_run_task(
            grant, arguments,
        )
        .await;
    }

    // Delegation from a harness is a capture and a turn stop, never a spawn
    // inside the tool call. The loop that settles this turn spawns the
    // children, parks the run, and resumes it with their deliverables — the
    // same loop, and the same handler, that does it for magician. A harness
    // cannot wait inside its own turn, so there is nothing else this call
    // could honestly do.
    if tool == "delegate_to_agent" {
        return plane_delegate_to_agent(grant, arguments, &record_ledger);
    }

    let action = match lower_to_action(tool, arguments) {
        Ok(action) => action,
        Err(message) => return mcp_error(message),
    };

    // Turn tool-call bound (plane Task 8). A spent bound ends the turn via the
    // same signal an approval uses — the harness survives an `isError` refusal
    // (Task 1) and settles, returning control to the loop, which wakes with a
    // fresh budget next iteration. Counted after lowering and before any gate:
    // a refused call never counts, an attempted call does even if it errors.
    if grant.turn_tool_call_budget_exhausted() {
        record_ledger(TerminalLedgerOutcome::Refused {
            reason: "turn_budget_spent".to_string(),
        });
        grant.set_turn_stop(PlaneTurnStopReason::TurnBudgetSpent);
        return mcp_error_with_meta(
            format!("{tool}: harness turn tool-call bound is spent; the turn is ending"),
            json!({"planeTurnStop": "turn_budget_spent"}),
        );
    }

    if let Some(pending) = plane_pause_gate(grant, &action).await {
        let reason = pending.action_description;
        return match grant.pause_disposition() {
            PlanePauseDisposition::EndTurn => {
                // Capture the gated action in its stable serialization before
                // ending the turn: execute-on-resume depends on Magician
                // executing exactly this action after a human approves, not
                // on the harness re-issuing a byte-identical call.
                if let Ok(action_json) =
                    crate::magician_v2::execution::agentic::stable_confirmation_action_json(&action)
                {
                    grant.set_pending_approval(PlanePendingApproval {
                        capture_id: format!("pltcap_{}", uuid::Uuid::new_v4().simple()),
                        action_json,
                        action_summary: action_signature(&action),
                        action_type: action.action_type_name().to_string(),
                    });
                }
                grant.set_turn_stop(PlaneTurnStopReason::NeedsApproval);
                mcp_error_with_meta(
                    format!("approval required: {reason}"),
                    json!({"planeTurnStop": "needs_approval"}),
                )
            },
            PlanePauseDisposition::Elicit => {
                // This capture belongs to the calling HTTP continuation. It
                // never occupies the live harness turn's shared approval slot.
                let action_json =
                    match crate::magician_v2::execution::agentic::stable_confirmation_action_json(
                        &action,
                    ) {
                        Ok(action_json) => action_json,
                        Err(_) => return mcp_error("the approval action could not be captured"),
                    };
                let capture = PlanePendingApproval {
                    capture_id: format!("pltcap_{}", uuid::Uuid::new_v4().simple()),
                    action_json,
                    action_summary: action_signature(&action),
                    action_type: action.action_type_name().to_string(),
                };
                // Private adapter metadata: the HTTP layer consumes it before
                // writing any message to the client.
                mcp_error_with_meta(
                    format!("approval required: {reason}"),
                    json!({
                        "planeElicit": "pending", "planeApprovalCapture": capture
                    }),
                )
            },
            PlanePauseDisposition::Refuse => {
                record_ledger(TerminalLedgerOutcome::Refused {
                    reason: "needs_approval".to_string(),
                });
                mcp_error(format!("approval required: {reason}"))
            },
        };
    }

    let response = governed_execute_tail(grant, &action, tool, Some(arguments)).await;
    record_ledger(TerminalLedgerOutcome::Executed {
        is_error: response
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    });
    response
}

/// The governed execution tail, shared by the live dispatch path (which
/// holds the grant's dispatch lock) and the elicitation-accept path. No
/// lock of its own: callers serialize the authorize-and-dispatch
/// transaction. `arguments` are the call's arguments as the door received
/// them; the approved-capture path has only the lowered action and passes
/// `None`.
async fn governed_execute_tail(
    grant: &PlaneGrant,
    action: &ExecutableAction,
    tool: &str,
    arguments: Option<&Value>,
) -> Value {
    let Some(executors) = grant.executors.as_ref() else {
        return mcp_error_with_meta(
            format!("plane dispatch has no live executors; `{tool}` cannot run execute_action"),
            json!({"planeDispatch": "unwired"}),
        );
    };

    let mut ctx = grant.ctx.clone();
    let trust_guard = match refresh_trust_dispatch_guard_for_decision(&mut ctx, executors) {
        Ok(guard) => guard,
        Err(err) => {
            return mcp_error(format!("trust policy refresh failed: {err}"));
        },
    };

    let invocation_ref = mint_invocation_ref();
    let effect_id = mint_invocation_ref();
    // A chat harness turn's calls are the chat's own tool events; a run or
    // terminal grant emits none here, even one spawned from a chat turn.
    let chat_events = ChatTurnToolEvents::for_call(grant, executors, tool, &invocation_ref);
    // Revocation is allowed to race policy/approval I/O. Revalidate at the
    // last possible boundary before the governed executor receives the action.
    if grant.is_revoked() {
        return revoked_grant_error();
    }
    grant.count_turn_tool_call();
    let dispatch_started = std::time::Instant::now();
    if let Some(events) = chat_events.as_ref() {
        events.started(arguments);
    }
    let result = execute_action(
        &action,
        executors.as_ref(),
        &grant.session_id,
        Some(grant.tool_index.as_ref()),
        None,
        &ctx,
        trust_guard.as_ref(),
        &grant.cancellation_token,
        Some(effect_id.as_str()),
        None,
        None,
        &invocation_ref,
    )
    .await;

    // Plane Task 9 — one `AgenticActionExecuted` per governed `tools/call`,
    // as it completes: per-action visibility inside a harness turn is the
    // only event gap left with the loop intact (the loop still owns
    // iteration/execution lifecycle events; a structural test pins that this
    // file never emits them). Iteration is 0 by definition — a plane call
    // sits inside a harness turn, not a loop iteration.
    if ctx.has_observability() {
        let execution_id = ctx
            .execution_id
            .clone()
            .or_else(|| ctx.legacy_execution_id.clone())
            .unwrap_or_default();
        let action_type = action.action_type_name().to_string();
        let target = action_signature(&action);
        let latency_ms = dispatch_started.elapsed().as_millis() as u64;
        let (success, error) = match &result {
            Ok(action_result) => (action_result.is_success(), None),
            Err(err) => (false, Some(format!("{err:#}"))),
        };
        executors.emit_event(RuntimeTransportEvent::AgenticActionExecuted {
            execution_id,
            principal: ctx.principal.clone(),
            workspace: ctx.workspace.clone(),
            plan_id: ctx.plan_id.clone().unwrap_or_default(),
            step_id: ctx.step_id.clone().unwrap_or_default(),
            iteration: 0,
            action_type,
            target,
            success,
            latency_ms,
            error,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    let (is_error, text) = match &result {
        Ok(action_result) => (
            !action_result.is_success(),
            action_result_text(action_result),
        ),
        Err(err) => (true, err.to_string()),
    };
    if let Some(events) = chat_events.as_ref() {
        events.finished(is_error, &text, dispatch_started.elapsed());
    }
    let content = if is_governed_app_compiled_tool(tool) {
        GOVERNED_APP_RESULT_OMITTED.to_owned()
    } else {
        ledger_content(is_error, &text)
    };
    ledger_record(grant, tool, invocation_ref, arguments, is_error, content);
    crate::magician_v2::execution::plane::terminal_session::decorate_runless_refusal(
        grant,
        mcp_result(is_error, text),
    )
}

/// Dispatch a bridged name through the grant's mouth bridge: the native
/// chat dispatcher, with the turn's exact context. Shares the plane path's
/// turn bound, revocation check, chat tool events, and transcript record;
/// runs none of the plane's own gates. In particular no `plane_pause_gate`
/// or approval capture: the native dispatcher applies its own approval,
/// trust, deny, and allowlist rules inside `dispatch_chat_tool_call`, the
/// same ones a model-issued call on the native mouth meets. The bridge runs
/// under the plane's own invocation ref, so the native dispatcher's events
/// and the plane's rows name one call; its body is polled on the execution
/// runtime, as the native mouth's own dispatch is; it is raced against the
/// grant's dispatch token, so a revoke mid-call cancels it and returns the
/// door after a bounded grace instead of holding the dispatch lock behind a
/// call whose turn is gone; and its result takes the provider-boundary
/// sanitizer before anything is derived from it, as every result the native
/// mouth sends its model does. Returns the MCP response and what the
/// caller's session ledger records for it.
async fn bridged_dispatch(
    grant: &PlaneGrant,
    tool: &str,
    arguments: &Value,
) -> (Value, TerminalLedgerOutcome) {
    let Some(bridge) = grant.mouth_bridge.as_ref() else {
        return (
            mcp_error(format!(
                "{tool} is a mouth tool and this grant carries no mouth bridge"
            )),
            TerminalLedgerOutcome::Refused {
                reason: "no_mouth_bridge".to_string(),
            },
        );
    };
    // The same turn bound the plane path applies, at the same point: a
    // refused call never counts, an attempted call does even if it errors.
    if grant.turn_tool_call_budget_exhausted() {
        grant.set_turn_stop(PlaneTurnStopReason::TurnBudgetSpent);
        return (
            mcp_error_with_meta(
                format!("{tool}: harness turn tool-call bound is spent; the turn is ending"),
                json!({"planeTurnStop": "turn_budget_spent"}),
            ),
            TerminalLedgerOutcome::Refused {
                reason: "turn_budget_spent".to_string(),
            },
        );
    }
    let invocation_ref = mint_invocation_ref();
    // `for_call` takes the executors only to reach their broadcaster; a
    // conversation grant carries them, and one without simply emits nothing.
    let chat_events = grant.executors.as_deref().and_then(|executors| {
        ChatTurnToolEvents::for_call(grant, executors, tool, &invocation_ref)
    });
    // Revalidated at the last boundary before the bridge, as the plane path
    // does before its executor.
    if grant.is_revoked() {
        return (
            revoked_grant_error(),
            TerminalLedgerOutcome::Refused {
                reason: "revoked".to_string(),
            },
        );
    }
    grant.count_turn_tool_call();
    let dispatch_started = std::time::Instant::now();
    if let Some(events) = chat_events.as_ref() {
        events.started(Some(arguments));
    }
    // The call's own cancel token: a child of the grant's dispatch token,
    // handed to the native dispatcher as the call's cancellation, so a
    // revoke reaches whatever the native work started (a nested LLM call, a
    // browser subprocess) and not only this door.
    let call_cancel = grant
        .cancellation_token
        .as_ref()
        .map(CancellationToken::child_token)
        .unwrap_or_default();
    // The bridge body is the native dispatcher — an agentic future, which
    // `runtime_boundary` requires be constructed and polled on the
    // execution runtime, as the native mouth's own dispatch is inside
    // `run_execution_job`, never on the door's HTTP worker (whose smaller
    // stack agentic frames have overflowed before). The closure crosses,
    // not a built future, so the execution runtime owns construction too.
    // Spawned detached rather than abort-on-drop: the dispatcher is
    // cooperative and must reach its own exit (below).
    let mut job = {
        let bridge = std::sync::Arc::clone(bridge);
        let call_id = invocation_ref.clone();
        let name = tool.to_string();
        let arguments = arguments.clone();
        let cancel = call_cancel.clone();
        // The job is another task, so the call's parent engine is re-scoped
        // onto it: the native dispatcher's own LLM operations follow the
        // mouth, as a plane-executed call's do.
        let parent_engine = grant_parent_engine(grant);
        let launch_pin = grant.ctx.run_engine_pin.clone();
        spawn_execution_job(move || {
            with_launching_run_engine_pin(
                launch_pin,
                with_parent_engine(
                    parent_engine.as_deref(),
                    bridge(call_id, name, arguments, cancel),
                ),
            )
        })
    };
    // Raced against the revoke itself: a turn that settles or times out
    // revokes its grant, and an in-flight bridged call must return the
    // door then — it holds the dispatch lock.
    let joined = match grant.cancellation_token.as_ref() {
        Some(revoked) => tokio::select! {
            joined = &mut job => Some(joined),
            _ = revoked.cancelled() => None,
        },
        None => Some((&mut job).await),
    };
    let joined = match joined {
        Some(joined) => joined,
        None => {
            // Cancel first, then wait: the native dispatcher registers a
            // chat fan-out and a progress subscription
            // (`dispatch_capability_pack`) that it releases only on its own
            // way out, and reports the subscription id back through the
            // bridge — dropping its future here would leak both and lose
            // the id. The call's token reaches the dispatcher and whatever
            // it started; one that observes it answers within the grace
            // with its own cancelled-shaped result, and that answer is the
            // call's result, recorded as any other (a record landing after
            // the turn drained its ledger is inert). One that does not
            // answer in time is left to unwind detached — never aborted —
            // and the door returns without it.
            call_cancel.cancel();
            match tokio::time::timeout(BRIDGED_CANCEL_GRACE, &mut job).await {
                Ok(joined) => joined,
                Err(_grace_elapsed) => {
                    if let Some(events) = chat_events.as_ref() {
                        events.finished(true, REVOKED_IN_FLIGHT, dispatch_started.elapsed());
                    }
                    return (
                        revoked_grant_error_with(REVOKED_IN_FLIGHT),
                        TerminalLedgerOutcome::Refused {
                            reason: "revoked".to_string(),
                        },
                    );
                },
            }
        },
    };
    // A join error is the dispatcher's task ending without a result — a
    // panic, or the execution runtime aborting it at shutdown — reported
    // to the mouth in the native status vocabulary like any failure.
    let value = joined.unwrap_or_else(|error| {
        json!({
            "status": "error",
            "error_code": "bridged_call_lost",
            "reason": format!("the native dispatcher ended without a result: {error}"),
        })
    });
    // The provider boundary: this result leaves for a third-party CLI, so
    // it takes the same outbound credential sanitizer the native mouth runs
    // on every model-bound result (`provider_safe_model_value`) before the
    // text, the finished row, or the record is derived from it.
    let value = sanitize_json_for_provider(&value);
    // The native surface's own reading of its status vocabulary, so the
    // MCP `isError`, the finished row, and the record agree with what the
    // native mouth would have said of this result.
    let mut is_error = native_result_is_error(&value);
    // Captured planner images are transported as MCP image blocks. Do not
    // duplicate their base64 into text transcripts, turn ledgers or events.
    let image_content = super::decision_planner_harness::planner_image_content(tool, &value);
    let value = if let Some(content) = image_content.as_ref() {
        is_error |= content["isError"] == true;
        json!({"status":if is_error {"error"} else {"ok"},"summary":"Decision image evidence transport"})
    } else {
        value
    };
    let text = value.to_string();
    if let Some(events) = chat_events.as_ref() {
        events.finished_with(&value, dispatch_started.elapsed());
    }
    // The value is already the native status object (an error names itself
    // as `status` + `reason`), so the record carries it as it is, bounded,
    // rather than wrapped in a second one.
    ledger_record(
        grant,
        tool,
        invocation_ref,
        Some(arguments),
        is_error,
        bounded_ledger_text(&text),
    );
    (
        image_content.unwrap_or_else(|| mcp_result(is_error, text)),
        TerminalLedgerOutcome::Executed { is_error },
    )
}

/// Whether the native dispatcher's result is a failure, by the native
/// surface's own reading of its status vocabulary (`status: "error"`,
/// `denied`, `timeout`, an `error_code` without a status, …) — the reading
/// its `tool.call.finished` row takes.
fn native_result_is_error(value: &Value) -> bool {
    crate::magician_v2::tool_result_runtime::legacy_tool_outcome_from_platform_envelope(
        value,
        crate::magician_v2::tool_result_projection::ToolOutcomeStatus::Succeeded,
    )
    .is_failure()
}

/// The chat transcript's record of one dispatched call (a conversation
/// grant's ledger, drained when the turn settles). Recorded whether or not
/// the grant could mint chat events: the transcript is what a later cold
/// turn replays, the events are what the card shows now. The record is
/// built before the ledger lock is taken, so the lock holds only the push.
/// A run or terminal grant carries no ledger and records nothing.
fn ledger_record(
    grant: &PlaneGrant,
    tool: &str,
    invocation_ref: String,
    arguments: Option<&Value>,
    is_error: bool,
    content: String,
) {
    let Some(ledger) = grant.turn_ledger.as_ref() else {
        return;
    };
    let record = PlaneTurnToolCall {
        call_id: invocation_ref,
        tool_name: tool.to_string(),
        arguments: durable_tool_arguments(tool, arguments),
        content,
        is_error,
    };
    ledger
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .record(record);
}

/// Ceiling on the compact-JSON bytes of `args` a `tool.call.started` row
/// carries. The turn's projection is read back through a fixed byte window,
/// so one large harness argument must not be able to blank the rest of the
/// turn; past this the row carries an omission record instead.
const CHAT_TOOL_EVENT_ARGS_MAX_BYTES: usize = 16 * 1024;
/// How many top-level argument keys an omission record names.
const CHAT_TOOL_EVENT_ARGS_OMISSION_MAX_KEYS: usize = 32;
/// Ceiling on the `error` text of a `tool.call.finished` row; the head is
/// kept, since a refusal names its reason first.
const CHAT_TOOL_EVENT_ERROR_MAX_BYTES: usize = 4 * 1024;
/// What every durable surface carries in place of a governed app tool's
/// result content — the same marker the native mouth writes.
const GOVERNED_APP_RESULT_OMITTED: &str = "[governed app result: content omitted]";

/// The chat's own tool events for one governed call inside a chat harness
/// turn — the native mouth's `tool.call.started` / `tool.call.finished`
/// shapes, stamped with the chat turn and marked `via: plane`, so the
/// activity card shows the swapped mouth's hands and the conformance eval can
/// attribute them to the turn. `AgenticActionExecuted` stays the loop's
/// per-action event and keeps its observability gate; these ride the chat
/// turn's projection instead, which a conversation grant always has.
///
/// Conversation grants only: a run spawned from a chat turn carries the
/// parent turn's id on its invocation too, and its calls are the run's, not
/// the turn's — stamping them onto the turn would bypass the chat fan-out's
/// subscription gate and duplicate the card's per-action row.
struct ChatTurnToolEvents<'a> {
    broadcaster: &'a RuntimeTransportBroadcaster,
    chat_turn_id: String,
    agent_id: String,
    principal: String,
    workspace: String,
    harness_engine: Option<String>,
    call_id: String,
    tool: String,
    started_at_ms: i64,
}

impl<'a> ChatTurnToolEvents<'a> {
    /// `Some` only for a conversation grant whose invocation carries a chat
    /// turn, whose context is scoped, and whose executors carry a broadcaster.
    fn for_call(
        grant: &PlaneGrant,
        executors: &'a crate::magician_v2::execution::agentic::ActionExecutors,
        tool: &str,
        call_id: &str,
    ) -> Option<Self> {
        // Run and terminal mints stamp the Plane surface; a conversation mint
        // keeps its lane. The turn id alone does not tell them apart.
        if grant.surface() == InvocationSurface::Plane {
            return None;
        }
        let chat_turn_id = grant
            .ctx
            .invocation_context_override
            .as_ref()
            .and_then(|invocation| invocation.chat_turn_id.clone())?;
        let broadcaster = executors.event_broadcaster.as_deref()?;
        // An envelope without its scope lands in no turn projection; never
        // mint one with an empty principal or workspace.
        let principal = grant
            .ctx
            .principal
            .clone()
            .filter(|principal| !principal.is_empty())?;
        let workspace = grant
            .ctx
            .workspace
            .clone()
            .filter(|workspace| !workspace.is_empty())?;
        Some(Self {
            broadcaster,
            chat_turn_id,
            agent_id: grant.ctx.agent_id.clone().unwrap_or_default(),
            principal,
            workspace,
            harness_engine: grant.ctx.harness_engine.clone(),
            call_id: call_id.to_string(),
            tool: tool.to_string(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
        })
    }

    fn governed(&self) -> bool {
        is_governed_app_compiled_tool(&self.tool)
    }

    /// The call's arguments as the durable row carries them. `None` (the
    /// approved-capture path has only the lowered action) rides as null.
    fn durable_arguments(&self, arguments: Option<&Value>) -> Value {
        durable_tool_arguments(&self.tool, arguments)
    }

    fn emit(&self, event_type: &str, payload: Value) {
        self.broadcaster.emit_named(
            event_type,
            &self.agent_id,
            Some(&self.principal),
            Some(&self.workspace),
            payload,
        );
    }

    /// Arguments ride through the native surface's own redaction, so the
    /// durable per-turn record holds the same shape either mouth writes, then
    /// through the row's own size bound.
    fn started(&self, arguments: Option<&Value>) {
        self.emit(
            "tool.call.started",
            json!({
                "chat_turn_id": self.chat_turn_id,
                "call_id": self.call_id,
                "tool_name": self.tool,
                "args": self.durable_arguments(arguments),
                "started_at": self.started_at_ms,
                "via": "plane",
                "harness_engine": self.harness_engine,
            }),
        );
    }

    /// The summary is taken from the same text the harness received, through
    /// the native surface's own result summary; a governed app tool's content
    /// is omitted exactly as the native mouth omits it.
    fn finished(&self, is_error: bool, text: &str, duration: std::time::Duration) {
        self.finished_with(&tool_outcome_object(is_error, text), duration);
    }

    /// The finished row from the outcome object itself. A bridged call's
    /// outcome is the native dispatcher's result, sanitized for the provider
    /// boundary, so its row is the one the native mouth would have emitted
    /// for that result.
    fn finished_with(&self, outcome: &Value, duration: std::time::Duration) {
        let (success, preview, error) = if self.governed() {
            let (success, _, _) =
                crate::magician_v2::chat::service::summarize_chat_tool_result_for_event(outcome);
            (
                success,
                GOVERNED_APP_RESULT_OMITTED.to_owned(),
                (!success).then(|| "governed tool dispatch failed".to_owned()),
            )
        } else {
            crate::magician_v2::chat::service::summarize_chat_tool_result_for_event(outcome)
        };
        let error = error.map(|error| bounded_event_error(&error));
        self.emit(
            "tool.call.finished",
            json!({
                "chat_turn_id": self.chat_turn_id,
                "call_id": self.call_id,
                "tool_name": self.tool,
                "success": success,
                "duration_ms": u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                "content_preview": preview,
                "error": error,
                "finished_at": chrono::Utc::now().timestamp_millis(),
                "via": "plane",
                "harness_engine": self.harness_engine,
            }),
        );
    }
}

/// A governed call's outcome as the native surface's result summary reads
/// it. The transcript record carries an error in this same shape, so a
/// replay can tell a failure from a result that happens to read like one.
fn tool_outcome_object(is_error: bool, text: &str) -> Value {
    if is_error {
        json!({ "status": "error", "error": text })
    } else {
        json!({ "status": "ok", "text": text })
    }
}

/// The head of `text` within the record bound, marked when the bound cut it.
fn bounded_ledger_text(text: &str) -> String {
    let mut head = head_within(text, PLANE_TURN_RESULT_MAX_BYTES).to_owned();
    if head.len() < text.len() {
        head.push_str(PLANE_TURN_RESULT_CUT_MARK);
    }
    head
}

/// The transcript record's content for a plane-executed call that returned
/// `text`: the bounded head; an error wrapped as the status object the
/// chat's tool events carry, since the executor's text is not one.
fn ledger_content(is_error: bool, text: &str) -> String {
    let head = bounded_ledger_text(text);
    if is_error {
        tool_outcome_object(true, &head).to_string()
    } else {
        head
    }
}

/// A call's arguments as every durable surface carries them — the event row
/// and the transcript record alike: the native surface's own projection
/// (governed content omitted), then the row bound. `None` rides as null.
fn durable_tool_arguments(tool: &str, arguments: Option<&Value>) -> Value {
    arguments
        .map(|arguments| {
            bounded_event_arguments(
                crate::magician_v2::chat::service::tool_arguments_for_durable_surface(
                    tool,
                    arguments,
                    is_governed_app_compiled_tool(tool),
                ),
            )
        })
        .unwrap_or(Value::Null)
}

/// The arguments as the durable row carries them: the surface's own
/// projection when it fits the row bound, else an omission record naming the
/// size and the top-level keys, so the card can still say what was asked
/// without the row displacing the rest of the turn.
fn bounded_event_arguments(args: Value) -> Value {
    let bytes = serde_json::to_vec(&args)
        .map(|compact| compact.len())
        .unwrap_or(usize::MAX);
    if bytes <= CHAT_TOOL_EVENT_ARGS_MAX_BYTES {
        return args;
    }
    let keys: Vec<&str> = args
        .as_object()
        .map(|object| {
            object
                .keys()
                .take(CHAT_TOOL_EVENT_ARGS_OMISSION_MAX_KEYS)
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default();
    json!({
        "omitted": true,
        "bytes": bytes,
        "cap_bytes": CHAT_TOOL_EVENT_ARGS_MAX_BYTES,
        "keys": keys,
    })
}

/// The head of an error text within the row bound, cut on a character
/// boundary.
fn bounded_event_error(error: &str) -> String {
    head_within(error, CHAT_TOOL_EVENT_ERROR_MAX_BYTES).to_owned()
}

/// Execute the action a terminal's elicitation answer approved (Task 12a).
///
/// Takes the grant's dispatch lock for the full transaction — the same
/// serialization the live path uses — re-checks revocation, consumes the
/// captured pending approval exactly once, and records the outcome to the
/// caller's session ledger. A capture that cannot be reconstructed refuses;
/// it never executes a guess.
pub async fn plane_execute_pending_approval(
    grant: &PlaneGrant,
    session_id: Option<&str>,
    tool: &str,
    expected_capture_id: Option<&str>,
) -> Value {
    let _dispatch_guard = grant.lock_dispatch().await;
    // Under the grant's parent engine, as the call that captured the
    // approval ran.
    with_launching_run_engine_pin(
        grant.ctx.run_engine_pin.clone(),
        with_parent_engine(
            grant_parent_engine(grant).as_deref(),
            plane_execute_pending_approval_under_dispatch_lock(
                grant,
                session_id,
                tool,
                expected_capture_id,
            ),
        ),
    )
    .await
}

async fn plane_execute_pending_approval_under_dispatch_lock(
    grant: &PlaneGrant,
    session_id: Option<&str>,
    tool: &str,
    expected_capture_id: Option<&str>,
) -> Value {
    if grant.is_revoked() {
        return revoked_grant_error();
    }
    let pending = match expected_capture_id {
        Some(expected) => grant.take_matching_pending_approval(expected),
        None => grant.take_pending_approval(),
    };
    let Some(pending) = pending else {
        return mcp_error("no matching pending approval captured for this grant");
    };
    execute_captured_approval_tail(grant, session_id, tool, &pending).await
}

/// The authenticated HTTP continuation owns this capture and consumes its
/// answer once. It must never be reconstructed from client-supplied arguments.
pub async fn plane_execute_approved_capture(
    grant: &PlaneGrant,
    session_id: &str,
    tool: &str,
    pending: &PlanePendingApproval,
    config: Option<&PlaneConfig>,
    cancelled: &tokio_util::sync::CancellationToken,
) -> Value {
    let _dispatch_guard = grant.lock_dispatch().await;
    // Under the grant's parent engine, exactly as `plane_tools_call_configured`
    // scopes the call that captured the approval: the continuation's grant is
    // a fresh projection, and the door re-names the session's parent on it.
    with_launching_run_engine_pin(
        grant.ctx.run_engine_pin.clone(),
        with_parent_engine(
            grant_parent_engine(grant).as_deref(),
            plane_execute_approved_capture_under_dispatch_lock(
                grant, session_id, tool, pending, config, cancelled,
            ),
        ),
    )
    .await
}

async fn plane_execute_approved_capture_under_dispatch_lock(
    grant: &PlaneGrant,
    session_id: &str,
    tool: &str,
    pending: &PlanePendingApproval,
    config: Option<&PlaneConfig>,
    cancelled: &tokio_util::sync::CancellationToken,
) -> Value {
    if grant.is_revoked() {
        return revoked_grant_error();
    }
    if cancelled.is_cancelled() {
        return mcp_error("request cancelled before approved dispatch");
    }
    if !grant.permits(tool) || !is_callable_configured(grant, config, tool) {
        return mcp_error("the captured tool is no longer available on this grant");
    }
    let mut call_grant = grant.clone();
    call_grant.cancellation_token = Some(cancelled.clone());
    execute_captured_approval_tail(&call_grant, Some(session_id), tool, pending).await
}

async fn execute_captured_approval_tail(
    grant: &PlaneGrant,
    session_id: Option<&str>,
    tool: &str,
    pending: &PlanePendingApproval,
) -> Value {
    let Ok(action) = serde_json::from_str::<ExecutableAction>(&pending.action_json) else {
        return mcp_error(
            "the captured approval action could not be reconstructed; the call was not \
             executed — retry it",
        );
    };
    let response = governed_execute_tail(grant, &action, tool, None).await;
    if let Some(session_id) = session_id {
        crate::magician_v2::execution::plane::terminal_ledger::record(
            session_id,
            crate::magician_v2::execution::plane::terminal_ledger::TerminalLedgerEntry {
                tool: tool.to_string(),
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                outcome: TerminalLedgerOutcome::Executed {
                    is_error: response
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                },
            },
        );
    }
    response
}

fn is_control_verb(name: &str) -> bool {
    PLANE_CONTROL_VERBS.iter().any(|verb| *verb == name)
}

fn plane_delegate_to_agent(
    grant: &PlaneGrant,
    arguments: &Value,
    record_ledger: &dyn Fn(TerminalLedgerOutcome),
) -> Value {
    let targets: Vec<crate::magician_v2::execution::actions::DelegationTargetRequest> =
        match arguments.get("targets") {
            Some(raw) => match serde_json::from_value(raw.clone()) {
                Ok(targets) => targets,
                Err(error) => {
                    return mcp_error(format!("delegate_to_agent: invalid targets: {error}"))
                },
            },
            None => return mcp_error("delegate_to_agent requires `targets`"),
        };
    if targets.is_empty() {
        return mcp_error("delegate_to_agent requires at least one target");
    }
    if targets
        .iter()
        .any(|target| target.required_capability.is_some() || !target.expected_artifacts.is_empty())
    {
        return mcp_error(
            "delegate_to_agent: leave required_capability and expected_artifacts out — the target's \
             own tooling and the child's reported values are Magician's to judge, not the caller's; \
             put everything the child must report into context",
        );
    }
    if let Some(bad) = targets
        .iter()
        .find(|target| target.target_agent_id.trim().is_empty() || target.context.trim().is_empty())
    {
        return mcp_error(format!(
            "delegate_to_agent: every target needs a target_agent_id and a context (offending target: {})",
            bad.target_agent_id
        ));
    }
    if !grant.live_harness_turn {
        record_ledger(TerminalLedgerOutcome::Refused {
            reason: "no_turn_to_park".to_string(),
        });
        return mcp_error(
            "delegate_to_agent parks the run that called it; this grant is not driving a run turn",
        );
    }
    let count = targets.len();
    grant.set_pending_delegation(targets);
    grant.set_turn_stop(PlaneTurnStopReason::Delegate);
    mcp_error_with_meta(
        format!(
            "delegation accepted for {count} target(s); this turn is ending so Magician can run them — your next turn resumes with their deliverables"
        ),
        json!({"planeTurnStop": "delegate"}),
    )
}

fn lower_to_action(tool: &str, arguments: &Value) -> Result<ExecutableAction, String> {
    let call = ExecutionToolCall {
        id: format!("plane_{tool}"),
        name: tool.to_string(),
        arguments: arguments.clone(),
    };
    match lower_native_tool_call(&call) {
        NativeDecisionOutcome::Valid(envelope) => match envelope.decision {
            Decision::Execute { candidates, .. } => candidates
                .primary()
                .or_else(|| candidates.candidates.first())
                .map(|candidate| candidate.action.clone())
                .ok_or_else(|| format!("{tool} lowered to an empty execute batch")),
            _ => Err(format!("{tool} did not lower to an executable action")),
        },
        NativeDecisionOutcome::UnknownTool { tool_name, .. } => {
            Err(format!("{tool_name} is not a known Magician tool"))
        },
        NativeDecisionOutcome::InvalidArguments { error, .. } => Err(error),
        NativeDecisionOutcome::ZeroToolCalls { .. }
        | NativeDecisionOutcome::MultipleToolCalls { .. } => {
            Err(format!("{tool} did not lower to an executable action"))
        },
    }
}

async fn plane_pause_gate(
    grant: &PlaneGrant,
    action: &ExecutableAction,
) -> Option<crate::magician_v2::agents::approval::PendingApproval> {
    if grant.constraints.requires_approval.is_empty() {
        return None;
    }
    let candidate = ActionCandidate::new(1, 1.0, action.clone());
    let step = approval_step_from_candidate(&candidate, 1, "plane tools/call");
    let plan = ExecutionPlan {
        plan_id: "plane".to_string(),
        steps: vec![step],
    };
    let check = if let Some(executors) = grant.executors.as_ref() {
        checked_approval(&grant.ctx, executors.as_ref(), &plan, &grant.constraints).await
    } else {
        crate::magician_v2::agents::approval::ApprovalCheck {
            result: ApprovalGate.check(&plan, &grant.constraints),
            waived: Vec::new(),
            asked: Vec::new(),
        }
    };
    match check.result {
        ApprovalResult::NeedsApproval {
            pending_actions, ..
        } => pending_actions.into_iter().next(),
        ApprovalResult::Approved(_) => None,
    }
}

pub fn action_result_text(result: &ActionResult) -> String {
    match result {
        ActionResult::Success => "ok".to_string(),
        ActionResult::Text { content } => content.clone(),
        ActionResult::Binary { mime_type, .. } => format!(
            "binary {}",
            mime_type.as_deref().unwrap_or("application/octet-stream")
        ),
        ActionResult::Http { status, body, .. } => {
            if body.is_empty() {
                format!("http {status}")
            } else {
                body.clone()
            }
        },
        ActionResult::Bool { value } => value.to_string(),
        ActionResult::List { items } => items.join("\n"),
        ActionResult::Browser { data } => data.to_string(),
    }
}

fn mcp_result(is_error: bool, text: impl Into<String>) -> Value {
    json!({
        "isError": is_error,
        "content": [{"type": "text", "text": text.into()}],
    })
}

fn mcp_error(text: impl Into<String>) -> Value {
    mcp_result(true, text)
}

fn mcp_error_with_meta(text: impl Into<String>, meta: Value) -> Value {
    json!({
        "isError": true,
        "content": [{"type": "text", "text": text.into()}],
        "_meta": meta,
    })
}

fn revoked_grant_error() -> Value {
    revoked_grant_error_with("plane grant was revoked before dispatch")
}

/// What a bridged call returns, and what its finished row reads, when the
/// grant was revoked while the call was in flight.
const REVOKED_IN_FLIGHT: &str = "plane grant was revoked while the call was in flight";

/// How long a revoked bridged call is given to answer its cancel token
/// before the door returns without it (`bridged_dispatch`). Long enough for
/// the native dispatcher to release what it registered on its way out;
/// short enough that a settled turn is not held behind a call that ignores
/// the token.
const BRIDGED_CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

fn revoked_grant_error_with(text: &str) -> Value {
    mcp_error_with_meta(text, json!({"planeGrant": "revoked"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::plane::catalog::{test_grant_with_index, test_index_with};
    use crate::magician_v2::execution::plane::grant::{
        drain_turn_ledger, PlaneGrant, PlaneTurnLedger,
    };
    use crate::magician_v2::execution::plane::ChatMouthBridge;
    use futures_util::future::BoxFuture;
    use magicllm::types::LLMToolSpec;

    #[tokio::test]
    async fn a_tool_outside_the_grant_is_refused_before_lowering() {
        let mut grant = PlaneGrant::for_test("exec-1");
        grant.allowed_tools = vec!["read_file".to_string()];
        let result = plane_tools_call(&grant, "shell", &json!({"command": "echo hi"})).await;
        assert_eq!(result["isError"], json!(true));
    }

    /// A conversation grant with the turn's index: a Deferred allowlist name
    /// is refused at the door until the mouth loads it through `tool_search`,
    /// and after that the door lets it through to dispatch.
    #[tokio::test]
    async fn a_conversation_grant_loads_a_deferred_tool_through_the_door_and_can_then_call_it() {
        let mut grant = chat_turn_grant().with_tool_index(std::sync::Arc::new(test_index_with(&[
            "read_file",
            "duckdb__query",
        ])));
        grant.allowed_tools = vec!["tool_search".to_string(), "duckdb__query".to_string()];

        let refused = plane_tools_call(&grant, "duckdb__query", &json!({})).await;
        assert_eq!(refused["isError"], json!(true));
        let text = refused["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("is not available on the plane"),
            "a deferred name must be refused before it is loaded: {text}"
        );

        let searched = plane_tools_call(
            &grant,
            "tool_search",
            &json!({"query": "select:duckdb__query"}),
        )
        .await;
        let text = searched["content"][0]["text"]
            .as_str()
            .expect("plane responses carry text");
        assert!(
            !text.contains("is not available on the plane"),
            "tool_search must pass the door: {text}"
        );
        assert_eq!(searched["isError"], json!(false));

        let after = plane_tools_call(&grant, "duckdb__query", &json!({})).await;
        let text = after["content"][0]["text"]
            .as_str()
            .expect("plane responses carry text");
        assert!(
            !text.contains("is not available on the plane"),
            "the door must accept a loaded name: {text}"
        );
    }

    /// The door checks the allowlist before it intercepts `tool_search`, so a
    /// grant whose allowlist lacks it can load nothing.
    #[tokio::test]
    async fn a_conversation_grant_without_tool_search_in_its_allowlist_cannot_load_anything() {
        let mut grant = chat_turn_grant().with_tool_index(std::sync::Arc::new(test_index_with(&[
            "read_file",
            "duckdb__query",
        ])));
        grant.allowed_tools = vec!["duckdb__query".to_string()];

        let searched = plane_tools_call(
            &grant,
            "tool_search",
            &json!({"query": "select:duckdb__query"}),
        )
        .await;
        assert_eq!(searched["isError"], json!(true));
        let text = searched["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("is not available on the plane"),
            "tool_search outside the allowlist must be refused: {text}"
        );

        let after = plane_tools_call(&grant, "duckdb__query", &json!({})).await;
        let text = after["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("is not available on the plane"),
            "nothing was loaded, so the deferred name stays refused: {text}"
        );
    }

    #[tokio::test]
    async fn a_control_verb_is_refused_even_if_asked_for_directly() {
        let result = plane_tools_call(&PlaneGrant::for_test("exec-1"), "yield", &json!({})).await;
        assert_eq!(result["isError"], json!(true));
    }

    #[tokio::test]
    async fn a_dispatch_without_executors_is_an_error_not_a_success() {
        let grant = test_grant_with_index(&["create_task"]);
        let result = plane_tools_call(&grant, "create_task", &json!({})).await;
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["_meta"]["planeDispatch"], json!("unwired"));
    }

    #[tokio::test]
    async fn a_gated_action_in_a_live_turn_ends_the_turn_rather_than_refusing_quietly() {
        let mut grant = PlaneGrant::for_test("exec-1")
            .with_live_harness_turn()
            .with_approval_rule_for("read_file");
        grant.catalog_profile =
            crate::magician_v2::execution::plane::grant::PlaneCatalogProfile::SpawnedBare;
        let result = plane_tools_call(&grant, "read_file", &json!({"path": "Cargo.toml"})).await;
        assert_eq!(
            grant.turn_stop_reason(),
            Some(PlaneTurnStopReason::NeedsApproval)
        );
        assert!(
            grant
                .cancellation_token
                .as_ref()
                .map_or(true, |token| !token.is_cancelled()),
            "approval must pause the run, not cancel it"
        );
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["_meta"]["planeTurnStop"], json!("needs_approval"));
    }

    /// Delegation from a harness is a capture and a turn stop, never a spawn
    /// inside the tool call: the loop that settles this turn does the spawn,
    /// parks, and resumes — the same loop that does it for magician.
    #[tokio::test]
    async fn a_harness_delegation_captures_its_targets_and_ends_the_turn() {
        let mut grant = PlaneGrant::for_test("exec-1").with_live_harness_turn();
        // The verb is advertised only to a run that has a target (see
        // `catalog::builtin_hot_names_for_grant`), so this run has one.
        grant.ctx.delegation_targets = vec![
            crate::magician_v2::execution::agentic::delegation_dispatch::DelegationTarget {
                agent_id: "researcher".to_string(),
                name: "Scout".to_string(),
                aliases: Vec::new(),
                description: "reads fixtures".to_string(),
                tools: Vec::new(),
                allowed_invocation_surfaces: Vec::new(),
            },
        ];
        let result = plane_tools_call(
            &grant,
            "delegate_to_agent",
            &json!({"targets": [{
                "target_agent_id": "researcher",
                "context": "count the lines in the fixture and report the marker"
            }]}),
        )
        .await;
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["_meta"]["planeTurnStop"], json!("delegate"));
        assert_eq!(
            grant.turn_stop_reason(),
            Some(PlaneTurnStopReason::Delegate)
        );
        let targets = grant
            .take_pending_delegation()
            .expect("the targets wait on the grant for the loop");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target_agent_id, "researcher");
        assert!(
            grant
                .cancellation_token
                .as_ref()
                .map_or(true, |token| !token.is_cancelled()),
            "delegation parks the run, it does not cancel it"
        );
    }

    /// A harness offered `required_capability` filled it with the pack the
    /// child would use and had the target refused for not owning it; offered
    /// `expected_artifacts` it named the values, and the child that yielded
    /// them inline was re-run. Neither is the caller's to say.
    #[tokio::test]
    async fn a_delegation_that_claims_capabilities_or_names_artifacts_is_refused() {
        let grant = PlaneGrant::for_test("exec-1").with_live_harness_turn();
        for extra in [
            json!({"required_capability": "files"}),
            json!({"expected_artifacts": [{"name": "marker"}]}),
        ] {
            let mut target = json!({"target_agent_id": "researcher", "context": "count the lines"});
            for (k, v) in extra.as_object().unwrap() {
                target[k] = v.clone();
            }
            let result =
                plane_tools_call(&grant, "delegate_to_agent", &json!({"targets": [target]})).await;
            assert_eq!(result["isError"], json!(true));
            assert_eq!(grant.turn_stop_reason(), None, "{result}");
            assert!(grant.take_pending_delegation().is_none());
        }
    }

    #[tokio::test]
    async fn a_delegation_with_nothing_to_delegate_captures_nothing() {
        let grant = PlaneGrant::for_test("exec-1").with_live_harness_turn();
        let result = plane_tools_call(&grant, "delegate_to_agent", &json!({"targets": []})).await;
        assert_eq!(result["isError"], json!(true));
        assert_eq!(grant.turn_stop_reason(), None);
        assert!(grant.take_pending_delegation().is_none());
    }

    #[tokio::test]
    async fn a_gated_action_without_a_turn_or_elicitation_is_refused() {
        let mut grant = PlaneGrant::for_test("sess-1").with_approval_rule_for("read_file");
        grant.catalog_profile =
            crate::magician_v2::execution::plane::grant::PlaneCatalogProfile::SpawnedBare;
        let result = plane_tools_call(&grant, "read_file", &json!({"path": "Cargo.toml"})).await;
        assert_eq!(result["isError"], json!(true));
        assert_eq!(grant.turn_stop_reason(), None);
        assert!(result["_meta"].get("planeTurnStop").is_none());
    }

    fn chat_turn_grant() -> PlaneGrant {
        use crate::magician_v2::agents::{
            AgentInvocationContext, FeatureMode, InvocationSourceKind, InvocationSurface,
        };
        use crate::magician_v2::execution::agentic::AgenticContext;
        use tokio_util::sync::CancellationToken;

        let invocation = AgentInvocationContext {
            principal: "owner".to_string(),
            workspace: "home".to_string(),
            source_agent_id: None,
            target_agent_id: "personal-assistant".to_string(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::Direct,
            chat_session_id: Some("sess-chat".to_string()),
            chat_turn_id: Some("turn-1".to_string()),
        };
        let mut ctx = AgenticContext::default();
        ctx.invocation_context_override = Some(invocation.clone());
        ctx.principal = Some(invocation.principal);
        ctx.workspace = Some(invocation.workspace);
        ctx.agent_id = Some(invocation.target_agent_id);
        ctx.chat_session_id = invocation.chat_session_id;
        PlaneGrant::for_conversation(ctx, "sess-chat".to_string(), CancellationToken::new())
    }

    /// Chat HITL is the next user message. A gated action on a ChatScoped
    /// grant must refuse, not EndTurn into Magician `pending_inputs`.
    #[tokio::test]
    async fn chat_turn_gated_action_is_refused() {
        let mut grant = chat_turn_grant().with_approval_rule_for("read_file");
        grant.catalog_profile =
            crate::magician_v2::execution::plane::grant::PlaneCatalogProfile::SpawnedBare;
        assert_eq!(grant.pause_disposition(), PlanePauseDisposition::Refuse);
        let result = plane_tools_call(&grant, "read_file", &json!({"path": "Cargo.toml"})).await;
        assert_eq!(result["isError"], json!(true));
        assert_eq!(grant.turn_stop_reason(), None);
        assert!(result["_meta"].get("planeTurnStop").is_none());
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("approval required"),
            "refusal must name the gate: {text}"
        );
    }

    /// Hands stay on the plane: a conversation grant's `tools/call` uses the
    /// same dispatch as a run grant. Without live executors the door is
    /// honest-unwired rather than inventing a second path.
    #[tokio::test]
    async fn chat_turn_tools_call_crosses_the_plane() {
        let mut grant = chat_turn_grant();
        grant.catalog_profile =
            crate::magician_v2::execution::plane::grant::PlaneCatalogProfile::SpawnedBare;
        let result = plane_tools_call(&grant, "read_file", &json!({"path": "Cargo.toml"})).await;
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["_meta"]["planeDispatch"], json!("unwired"));
    }

    /// A spent bound ends the turn through the same signal an approval uses —
    /// the harness survives the refusal — but no human is asked, and the run's
    /// cancellation token stays untripped: budget exhaustion is a turn
    /// boundary, not `conclude_budget_exhausted`.
    #[tokio::test]
    async fn a_spent_tool_call_bound_ends_the_turn_instead_of_dispatching() {
        let mut grant = PlaneGrant::for_test("exec-bound")
            .with_live_harness_turn()
            .with_turn_tool_budget(1, 1);
        grant.catalog_profile =
            crate::magician_v2::execution::plane::grant::PlaneCatalogProfile::SpawnedBare;
        let result = plane_tools_call(&grant, "read_file", &json!({"path": "Cargo.toml"})).await;
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["_meta"]["planeTurnStop"], json!("turn_budget_spent"));
        assert_eq!(
            grant.turn_stop_reason(),
            Some(PlaneTurnStopReason::TurnBudgetSpent)
        );
        assert!(
            !grant
                .cancellation_token
                .as_ref()
                .map_or(true, |token| token.is_cancelled()),
            "a spent bound ends the turn; it must not cancel the run"
        );
    }

    #[tokio::test]
    async fn a_call_after_the_bound_stopped_the_turn_reports_the_budget_code() {
        let grant = PlaneGrant::for_test("exec-bound-entry");
        grant.set_turn_stop(PlaneTurnStopReason::TurnBudgetSpent);
        let result = plane_tools_call(&grant, "read_file", &json!({})).await;
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["_meta"]["planeTurnStop"], json!("turn_budget_spent"));
    }

    /// Only a governed dispatch spends the bound: a call that never reaches
    /// `execute_action` (here: no live executors) must not count, or the
    /// harness loses budget to refusals.
    #[tokio::test]
    async fn a_call_that_never_dispatches_does_not_spend_the_turn_budget() {
        let grant = test_grant_with_index(&["create_task"]).with_turn_tool_budget(5, 0);
        let result = plane_tools_call(&grant, "create_task", &json!({})).await;
        assert_eq!(result["isError"], json!(true));
        assert_eq!(
            grant.turn_tool_calls_spent(),
            0,
            "unwired calls must not consume the tool-call bound"
        );
    }

    /// The gate captures the action it paused on, in the loop's stable
    /// serialization — execute-on-resume depends on Magician executing
    /// exactly this action after a human approves.
    #[tokio::test]
    async fn a_gated_action_in_a_live_turn_is_captured_for_execute_on_resume() {
        let mut grant = PlaneGrant::for_test("exec-capture")
            .with_live_harness_turn()
            .with_approval_rule_for("read_file");
        grant.catalog_profile =
            crate::magician_v2::execution::plane::grant::PlaneCatalogProfile::SpawnedBare;
        let result = plane_tools_call(&grant, "read_file", &json!({"path": "Cargo.toml"})).await;
        assert_eq!(result["isError"], json!(true));
        let pending = grant
            .take_pending_approval()
            .expect("the gated action must be captured");
        assert!(
            pending.action_json.contains("read_file"),
            "stable serialization must name the tool: {}",
            pending.action_json
        );
        assert!(pending.action_summary.contains("read_file"));
        assert!(!pending.action_type.is_empty());
        // One capture, one consumption — a second take must be empty.
        assert!(grant.take_pending_approval().is_none());
    }

    /// Plane Task 9: every governed call emits `AgenticActionExecuted` as it
    /// completes, through the same `emit_event` the loop uses.
    #[test]
    fn each_plane_call_emits_an_action_event_as_it_completes() {
        let source = include_str!("dispatch.rs");
        let impl_src = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            impl_src.contains("AgenticActionExecuted"),
            "the plane must emit per-action events"
        );
        assert!(
            impl_src.contains("emit_event("),
            "emission goes through the loop's own broadcaster entry"
        );
    }

    /// The loop's own lifecycle events are the loop's to emit — the plane
    /// adds per-action visibility only, never duplicates.
    #[test]
    fn the_loops_own_events_are_not_duplicated() {
        // Implementation half only: this test's own forbidden-name array
        // lives in the test half, and scanning the full source would always
        // find it.
        let source = include_str!("dispatch.rs");
        let impl_src = source.split("#[cfg(test)]").next().unwrap_or(source);
        for owned_by_loop in [
            "AgenticIterationStarted",
            "AgenticIterationCompleted",
            "AgenticExecutionStarted",
            "AgenticExecutionCompleted",
        ] {
            assert!(
                !impl_src.contains(owned_by_loop),
                "{owned_by_loop} is the loop's to emit, not the plane's"
            );
        }
    }

    #[test]
    fn the_trust_guard_is_refreshed_per_call_not_per_run() {
        let source = include_str!("dispatch.rs");
        let impl_src = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            impl_src.contains("refresh_trust_dispatch_guard_for_decision("),
            "stale trust policy would last the run"
        );
        assert!(
            impl_src.contains("execute_action("),
            "tools/call must use the governed entry"
        );
        assert!(
            !impl_src.contains("dispatch_flat_action"),
            "the ungoverned flat dispatcher skips the outward gate"
        );
    }

    #[test]
    fn revocation_is_checked_after_queueing_and_immediately_before_execution() {
        let source = include_str!("dispatch.rs");
        let impl_src = source.split("#[cfg(test)]").next().unwrap_or(source);
        let after_lock = impl_src
            .split("let _dispatch_guard = grant.lock_dispatch().await;")
            .nth(1)
            .expect("dispatch lock");
        let ledger = after_lock
            .find("let ledger_session = session_id.map(str::to_string);")
            .expect("terminal ledger setup after dispatch lock");
        let revocation = after_lock
            .find("if grant.is_revoked()")
            .expect("revocation check after dispatch lock");
        assert!(ledger < revocation);

        let before_execute = impl_src
            .split("let result = execute_action(")
            .next()
            .expect("execute boundary");
        assert!(
            before_execute.ends_with(
                "if grant.is_revoked() {\n        return revoked_grant_error();\n    }\n    grant.count_turn_tool_call();\n    let dispatch_started = std::time::Instant::now();\n    if let Some(events) = chat_events.as_ref() {\n        events.started(arguments);\n    }\n    "
            ),
            "the final revocation check drifted away from the synchronous dispatch prelude"
        );
    }

    fn test_executors() -> crate::magician_v2::execution::agentic::ActionExecutors {
        use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use crate::magician_v2::test_utils::ConfigurableMockLlm;
        let storage = JsonPromptStorage::with_default_config().expect("prompt storage");
        crate::magician_v2::execution::agentic::ActionExecutors::new(
            std::sync::Arc::new(ConfigurableMockLlm::with_response("{}")),
            std::sync::Arc::new(PromptManager::new(std::sync::Arc::new(storage))),
        )
    }

    /// Every `tool.call.*` envelope the broadcaster has published so far.
    fn drain_tool_call_events(
        receiver: &mut tokio::sync::broadcast::Receiver<RuntimeTransportEvent>,
    ) -> Vec<crate::magician_v2::realtime_events::AgentEventEnvelope> {
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            if let RuntimeTransportEvent::AgentEvent { event } = event {
                if event.event_type.starts_with("tool.call.") {
                    events.push(event);
                }
            }
        }
        events
    }

    /// A chat harness turn's governed calls are the chat's own tool events:
    /// the native mouth's `tool.call.started` / `tool.call.finished`, stamped
    /// with the chat turn and marked as plane-dispatched, so the activity
    /// card shows the swapped mouth's hands and the conformance eval can
    /// attribute them to the turn.
    #[tokio::test]
    async fn a_chat_harness_turn_call_emits_the_chats_tool_events() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let mut grant = chat_turn_grant();
        grant.ctx.harness_engine = Some("claude_code".to_string());
        grant.executors = Some(std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(&broadcaster)),
        ));
        grant.allowed_tools = vec!["list_tasks".to_string()];

        let result = plane_tools_call(&grant, "list_tasks", &json!({"status": "open"})).await;
        assert_eq!(result["isError"], json!(true), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap_or("");

        let events = drain_tool_call_events(&mut receiver);
        let kinds: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(kinds, vec!["tool.call.started", "tool.call.finished"]);
        for event in &events {
            assert_eq!(event.agent_id, "personal-assistant");
            assert_eq!(event.principal.as_deref(), Some("owner"));
            assert_eq!(event.workspace.as_deref(), Some("home"));
            assert_eq!(event.payload["chat_turn_id"], json!("turn-1"));
            assert_eq!(event.payload["tool_name"], json!("list_tasks"));
            assert_eq!(event.payload["via"], json!("plane"));
            assert_eq!(event.payload["harness_engine"], json!("claude_code"));
            assert!(event.payload["call_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("pltinv_")));
        }
        let started = &events[0].payload;
        let finished = &events[1].payload;
        assert_eq!(started["call_id"], finished["call_id"]);
        assert_eq!(started["args"], json!({"status": "open"}));
        assert_eq!(finished["success"], json!(false));
        let error = finished["error"].as_str().unwrap_or("");
        assert!(
            !error.is_empty(),
            "a failed call names its error: {finished}"
        );
        assert_eq!(error, text, "the event carries what the harness saw");
        assert!(finished["duration_ms"].is_u64());
        assert!(
            started["started_at"].as_i64().unwrap() <= finished["finished_at"].as_i64().unwrap()
        );
    }

    /// Without a chat turn on the invocation — a terminal or run grant — the
    /// plane emits none of the chat's events.
    #[tokio::test]
    async fn a_call_without_a_chat_turn_emits_no_chat_tool_events() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let mut grant = chat_turn_grant();
        grant.ctx.invocation_context_override = None;
        grant.executors = Some(std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(&broadcaster)),
        ));
        grant.allowed_tools = vec!["list_tasks".to_string()];

        let result = plane_tools_call(&grant, "list_tasks", &json!({})).await;
        assert_eq!(result["isError"], json!(true), "{result}");
        assert!(
            drain_tool_call_events(&mut receiver).is_empty(),
            "no chat turn, no chat tool events"
        );
    }

    /// A run spawned from a chat turn carries the parent turn's id on its
    /// invocation, but its mint stamps the Plane surface: its calls are the
    /// run's, and must not be stamped onto the chat turn.
    #[tokio::test]
    async fn a_chat_spawned_run_call_emits_no_chat_tool_events() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let ctx = chat_turn_grant().ctx;
        assert_eq!(
            ctx.invocation_context_override
                .as_ref()
                .and_then(|invocation| invocation.chat_turn_id.as_deref()),
            Some("turn-1"),
            "the fixture must carry the parent turn's id"
        );
        let executors = std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(&broadcaster)),
        );
        let mut grant = PlaneGrant::for_run(ctx, executors, "exec-child".to_string());
        assert_eq!(grant.surface(), InvocationSurface::Plane);
        grant.allowed_tools = vec!["list_tasks".to_string()];

        let result = plane_tools_call(&grant, "list_tasks", &json!({})).await;
        assert_eq!(result["isError"], json!(true), "{result}");
        assert!(
            drain_tool_call_events(&mut receiver).is_empty(),
            "a run grant's calls are not the chat turn's hands"
        );
    }

    /// The row rides the scope on its envelope; a context missing either half
    /// mints no envelope rather than an unscoped one.
    #[tokio::test]
    async fn a_call_without_a_scoped_context_emits_no_chat_tool_events() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let mut grant = chat_turn_grant();
        grant.ctx.workspace = None;
        grant.executors = Some(std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(&broadcaster)),
        ));
        grant.allowed_tools = vec!["list_tasks".to_string()];

        let result = plane_tools_call(&grant, "list_tasks", &json!({})).await;
        assert_eq!(result["isError"], json!(true), "{result}");
        assert!(
            drain_tool_call_events(&mut receiver).is_empty(),
            "no scope, no envelope"
        );
    }

    /// One large argument must not become a row that displaces the rest of
    /// the turn from its bounded read window: past the cap the started row
    /// carries an omission record, never the blob.
    #[tokio::test]
    async fn an_oversized_argument_is_omitted_from_the_started_row() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let mut grant = chat_turn_grant();
        grant.executors = Some(std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(&broadcaster)),
        ));
        grant.allowed_tools = vec!["list_tasks".to_string()];
        let blob = "x".repeat(CHAT_TOOL_EVENT_ARGS_MAX_BYTES * 4);
        let arguments = json!({"status": "open", "blob": blob});
        let compact_bytes = serde_json::to_vec(&arguments).unwrap().len();

        let _ = plane_tools_call(&grant, "list_tasks", &arguments).await;

        let events = drain_tool_call_events(&mut receiver);
        let kinds: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(kinds, vec!["tool.call.started", "tool.call.finished"]);
        let args = &events[0].payload["args"];
        assert_eq!(args["omitted"], json!(true), "{args}");
        assert_eq!(args["bytes"], json!(compact_bytes));
        assert_eq!(args["cap_bytes"], json!(CHAT_TOOL_EVENT_ARGS_MAX_BYTES));
        assert_eq!(args["keys"], json!(["blob", "status"]));
        assert!(args.get("blob").is_none(), "the blob must not ride the row");
        let row_bytes = serde_json::to_vec(&events[0].payload).unwrap().len();
        assert!(
            row_bytes < CHAT_TOOL_EVENT_ARGS_MAX_BYTES,
            "the started row stays small once its arguments are omitted: {row_bytes}"
        );
    }

    /// The finished row's error keeps its head within the bound, cut on a
    /// character boundary; a short error is untouched.
    #[test]
    fn a_finished_rows_error_is_bounded_at_its_head() {
        assert_eq!(bounded_event_error("short"), "short");
        let long = "é".repeat(CHAT_TOOL_EVENT_ERROR_MAX_BYTES);
        let bounded = bounded_event_error(&long);
        assert!(bounded.len() <= CHAT_TOOL_EVENT_ERROR_MAX_BYTES);
        assert!(long.starts_with(&bounded), "the head is kept");
        assert!(
            bounded.chars().all(|c| c == 'é'),
            "cut on a character boundary"
        );
        assert_eq!(bounded.len(), CHAT_TOOL_EVENT_ERROR_MAX_BYTES);
    }

    /// A conversation grant's dispatched calls land in its turn ledger — the
    /// chat transcript's record — with the same id its chat events carry,
    /// the same argument projection the started row carries, and the text
    /// the harness received.
    #[tokio::test]
    async fn conversation_grant_records_each_governed_call_in_the_turn_ledger() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let mut grant = chat_turn_grant();
        grant.executors = Some(std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(&broadcaster)),
        ));
        grant.allowed_tools = vec!["list_tasks".to_string()];

        let result = plane_tools_call(&grant, "list_tasks", &json!({"status": "open"})).await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        let is_error = result["isError"].as_bool().expect("isError");

        let records = drain_turn_ledger(&grant.turn_ledger);
        assert_eq!(records.len(), 1, "{records:?}");
        let record = &records[0];
        assert_eq!(record.tool_name, "list_tasks");
        assert_eq!(record.arguments, json!({"status": "open"}));
        assert_eq!(record.is_error, is_error);
        if is_error {
            let object: Value =
                serde_json::from_str(&record.content).expect("an error records the status object");
            assert_eq!(object, json!({"status": "error", "error": text}));
        } else {
            assert_eq!(
                record.content, text,
                "the record carries what the harness saw"
            );
        }
        assert!(record.call_id.starts_with("pltinv_"), "{}", record.call_id);
        let events = drain_tool_call_events(&mut receiver);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].payload["call_id"],
            json!(record.call_id),
            "one id names the call on the card and in the transcript"
        );
        assert_eq!(events[0].payload["args"], record.arguments);
        assert!(
            drain_turn_ledger(&grant.turn_ledger).is_empty(),
            "the drain empties the ledger"
        );
    }

    /// The transcript record does not depend on the chat events: a
    /// conversation grant whose executors carry no broadcaster still
    /// records, and calls accumulate in dispatch order.
    #[tokio::test]
    async fn the_turn_ledger_records_without_a_broadcaster_and_in_order() {
        let mut grant = chat_turn_grant();
        grant.executors = Some(std::sync::Arc::new(test_executors()));
        grant.allowed_tools = vec!["list_tasks".to_string()];

        let _ = plane_tools_call(&grant, "list_tasks", &json!({"status": "open"})).await;
        let _ = plane_tools_call(&grant, "list_tasks", &json!({"status": "done"})).await;

        let records = drain_turn_ledger(&grant.turn_ledger);
        let statuses: Vec<&Value> = records
            .iter()
            .map(|record| &record.arguments["status"])
            .collect();
        assert_eq!(statuses, vec![&json!("open"), &json!("done")]);
        assert!(records
            .iter()
            .all(|record| record.tool_name == "list_tasks"));
        let ids: std::collections::HashSet<&str> = records
            .iter()
            .map(|record| record.call_id.as_str())
            .collect();
        assert_eq!(ids.len(), 2, "every call has its own id");
    }

    /// Run and terminal grants have their own records; neither carries a
    /// turn ledger, and a dispatch on them records nothing.
    #[tokio::test]
    async fn a_terminal_grant_records_nothing() {
        let mut terminal = PlaneGrant::for_test("exec-ledger");
        terminal.executors = Some(std::sync::Arc::new(test_executors()));
        terminal.allowed_tools = vec!["list_tasks".to_string()];
        let _ = plane_tools_call(&terminal, "list_tasks", &json!({})).await;
        assert!(terminal.turn_ledger.is_none());
        assert!(drain_turn_ledger(&terminal.turn_ledger).is_empty());

        let mut run = PlaneGrant::for_run(
            chat_turn_grant().ctx,
            std::sync::Arc::new(test_executors()),
            "exec-child".to_string(),
        );
        run.allowed_tools = vec!["list_tasks".to_string()];
        let _ = plane_tools_call(&run, "list_tasks", &json!({})).await;
        assert!(run.turn_ledger.is_none());
        assert!(drain_turn_ledger(&run.turn_ledger).is_empty());
    }

    /// A call that never dispatches (here: no live executors) leaves no
    /// record: the transcript carries results, not refusals.
    #[tokio::test]
    async fn a_refused_call_leaves_no_ledger_record() {
        let grant = chat_turn_grant();
        let result = plane_tools_call(&grant, "read_file", &json!({"path": "Cargo.toml"})).await;
        assert_eq!(result["_meta"]["planeDispatch"], json!("unwired"));
        assert!(drain_turn_ledger(&grant.turn_ledger).is_empty());
    }

    /// The record's content keeps its head within the bound, cut on a
    /// character boundary; a short result is untouched.
    #[test]
    fn ledger_content_is_cut_on_a_char_boundary_at_the_bound() {
        assert_eq!(head_within("short", PLANE_TURN_RESULT_MAX_BYTES), "short");
        let long = "é".repeat(PLANE_TURN_RESULT_MAX_BYTES);
        let bounded = head_within(&long, PLANE_TURN_RESULT_MAX_BYTES);
        assert!(bounded.len() <= PLANE_TURN_RESULT_MAX_BYTES);
        assert!(long.starts_with(&bounded), "the head is kept");
        assert!(
            bounded.chars().all(|c| c == 'é'),
            "cut on a character boundary"
        );
        assert_eq!(bounded.len(), PLANE_TURN_RESULT_MAX_BYTES);
        assert_eq!(
            head_within("héllo", 2),
            "h",
            "a cut inside a character backs up to its start"
        );
    }

    /// An error's content is the status object the chat's tool events
    /// carry, not bare text, so a replay can tell a failure from a result
    /// that reads like one; a result's content is the text itself.
    #[test]
    fn error_content_is_the_status_object() {
        let error = ledger_content(true, "trust policy refused the call");
        let object: Value = serde_json::from_str(&error).expect("compact JSON");
        assert_eq!(
            object,
            json!({"status": "error", "error": "trust policy refused the call"})
        );
        assert_eq!(
            ledger_content(false, "trust policy refused the call"),
            "trust policy refused the call"
        );
        assert_eq!(
            ledger_content(false, r#"{"status":"error","error":"looks like one"}"#),
            r#"{"status":"error","error":"looks like one"}"#,
            "a result is carried as it came, whatever it reads like"
        );
    }

    /// A result the record bound cut ends in the cut marker, after a head
    /// cut on a character boundary; a result within the bound carries no
    /// marker. A cut error keeps the marker inside its status object.
    #[test]
    fn a_cut_result_carries_the_marker() {
        let whole = "é".repeat(PLANE_TURN_RESULT_MAX_BYTES);
        let content = ledger_content(false, &whole);
        let head = content
            .strip_suffix(PLANE_TURN_RESULT_CUT_MARK)
            .expect("a cut result ends in the marker");
        assert_eq!(head.len(), PLANE_TURN_RESULT_MAX_BYTES);
        assert!(whole.starts_with(head), "the head is kept");
        assert!(
            head.chars().all(|c| c == 'é'),
            "cut on a character boundary"
        );

        let within = "x".repeat(PLANE_TURN_RESULT_MAX_BYTES);
        assert_eq!(
            ledger_content(false, &within),
            within,
            "no marker within the bound"
        );

        let cut_error = ledger_content(true, &whole);
        let object: Value = serde_json::from_str(&cut_error).expect("compact JSON");
        assert_eq!(object["status"], json!("error"));
        let error = object["error"].as_str().expect("error text");
        assert!(error.ends_with(PLANE_TURN_RESULT_CUT_MARK));
        assert_eq!(
            error.len(),
            PLANE_TURN_RESULT_MAX_BYTES + PLANE_TURN_RESULT_CUT_MARK.len()
        );
    }

    fn bridged_specs(names: &[&str]) -> std::collections::BTreeMap<String, LLMToolSpec> {
        names
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    LLMToolSpec {
                        name: name.to_string(),
                        description: format!("{name} on the native mouth"),
                        parameters: json!({"type": "object"}),
                    },
                )
            })
            .collect()
    }

    /// A bridge that answers every call with an ok object echoing what it
    /// was asked, so a test can see the call id, name, and arguments cross
    /// verbatim.
    fn echo_bridge() -> ChatMouthBridge {
        std::sync::Arc::new(
            |call_id: String,
             name: String,
             arguments: Value,
             _cancel: CancellationToken|
             -> BoxFuture<'static, Value> {
                Box::pin(async move {
                    json!({
                        "status": "ok",
                        "echo": {"call_id": call_id, "name": name, "args": arguments},
                    })
                })
            },
        )
    }

    /// A bridge that answers with a fixed object, whatever it is asked.
    fn fixed_bridge(answer: Value) -> ChatMouthBridge {
        std::sync::Arc::new(
            move |_call_id: String,
                  _name: String,
                  _arguments: Value,
                  _cancel: CancellationToken|
                  -> BoxFuture<'static, Value> {
                let answer = answer.clone();
                Box::pin(async move { answer })
            },
        )
    }

    /// A bridge that never answers, keeping the token it was handed so a
    /// test can see the plane cancel it.
    fn parked_bridge() -> (
        ChatMouthBridge,
        std::sync::Arc<std::sync::Mutex<Option<CancellationToken>>>,
    ) {
        let handed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let slot = std::sync::Arc::clone(&handed);
        let bridge: ChatMouthBridge = std::sync::Arc::new(
            move |_call_id: String,
                  _name: String,
                  _arguments: Value,
                  cancel: CancellationToken|
                  -> BoxFuture<'static, Value> {
                *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cancel);
                Box::pin(std::future::pending::<Value>())
            },
        );
        (bridge, handed)
    }

    /// A bridge that waits on the token it was handed and answers it as the
    /// native dispatcher does — with its own cancelled-shaped result — so a
    /// test can see that answer come back through the door.
    fn cooperative_bridge(answer: Value) -> ChatMouthBridge {
        std::sync::Arc::new(
            move |_call_id: String,
                  _name: String,
                  _arguments: Value,
                  cancel: CancellationToken|
                  -> BoxFuture<'static, Value> {
                let answer = answer.clone();
                Box::pin(async move {
                    cancel.cancelled().await;
                    answer
                })
            },
        )
    }

    /// A bridged call crosses to the mouth bridge with its name and
    /// arguments verbatim, comes back as the dispatcher's compact JSON, and
    /// is otherwise one of the turn's calls: it emits the chat's tool events
    /// with the same id it records in the turn ledger — the id the bridge
    /// itself ran under, so the native dispatcher's events name the same
    /// call — and spends the turn bound. No plane executor, no lowering, no
    /// pause gate.
    #[tokio::test]
    async fn a_bridged_call_runs_the_mouth_bridge_and_emits_turn_events() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let mut grant = chat_turn_grant().with_turn_tool_budget(3, 0);
        grant.ctx.harness_engine = Some("codex".to_string());
        grant.executors = Some(std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(&broadcaster)),
        ));
        grant.allowed_tools = vec!["list_tasks".to_string()];
        let grant = grant.with_bridged_tools(bridged_specs(&["create_chat_thread"]), echo_bridge());
        let arguments = json!({"title": "Plans"});

        let result = plane_tools_call(&grant, "create_chat_thread", &arguments).await;
        assert_eq!(result["isError"], json!(false), "{result}");
        let text = result["content"][0]["text"].as_str().expect("text");
        let value: Value = serde_json::from_str(text).expect("compact JSON");
        assert_eq!(value["status"], json!("ok"));
        assert_eq!(value["echo"]["name"], json!("create_chat_thread"));
        assert_eq!(value["echo"]["args"], arguments);
        assert!(result.get("_meta").is_none(), "{result}");

        let events = drain_tool_call_events(&mut receiver);
        let kinds: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(kinds, vec!["tool.call.started", "tool.call.finished"]);
        for event in &events {
            assert_eq!(event.payload["chat_turn_id"], json!("turn-1"));
            assert_eq!(event.payload["tool_name"], json!("create_chat_thread"));
            assert_eq!(event.payload["via"], json!("plane"));
            assert_eq!(event.payload["harness_engine"], json!("codex"));
        }
        let started = &events[0].payload;
        let finished = &events[1].payload;
        assert_eq!(started["args"], arguments);
        assert_eq!(finished["success"], json!(true));
        assert!(finished["error"].is_null(), "{finished}");
        assert!(finished["content_preview"]
            .as_str()
            .is_some_and(|preview| preview.contains("\"status\":\"ok\"")));

        let records = drain_turn_ledger(&grant.turn_ledger);
        assert_eq!(records.len(), 1, "{records:?}");
        let record = &records[0];
        assert_eq!(record.tool_name, "create_chat_thread");
        assert_eq!(record.arguments, arguments);
        assert_eq!(
            record.content, text,
            "the record carries what the harness saw"
        );
        assert!(!record.is_error);
        assert_eq!(started["call_id"], json!(record.call_id));
        assert!(record.call_id.starts_with("pltinv_"));
        assert_eq!(
            value["echo"]["call_id"],
            json!(record.call_id),
            "the bridge ran under the plane's own call id"
        );

        assert_eq!(
            grant.turn_tool_calls_spent(),
            1,
            "a bridged call spends the bound"
        );
        assert_eq!(grant.turn_stop_reason(), None);
    }

    /// A native tool's result can name credential material — the dispatcher
    /// returns the tool's value raw, and the native mouth sanitizes it on
    /// its way to the model. A bridged result leaves for a third-party CLI,
    /// so the same sanitizer runs before the MCP text, the finished row, or
    /// the record is derived from it: none of the three carries the value.
    #[tokio::test]
    async fn a_bridged_result_is_redacted_before_it_reaches_the_mouth() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let credential = "bridged-credential-material-0123456789";
        let mut grant = chat_turn_grant().with_turn_tool_budget(3, 0);
        grant.executors = Some(std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(&broadcaster)),
        ));
        let grant = grant.with_bridged_tools(
            bridged_specs(&["create_chat_thread"]),
            fixed_bridge(json!({"status": "ok", "thread_id": "thr-1", "api_key": credential})),
        );

        let result = plane_tools_call(&grant, "create_chat_thread", &json!({"title": "x"})).await;
        assert_eq!(result["isError"], json!(false), "{result}");
        let text = result["content"][0]["text"].as_str().expect("text");
        assert!(
            !text.contains(credential),
            "the mouth never sees the credential: {text}"
        );
        let value: Value = serde_json::from_str(text).expect("compact JSON");
        assert_eq!(value["status"], json!("ok"));
        assert_eq!(
            value["thread_id"],
            json!("thr-1"),
            "the rest of the result crosses"
        );
        assert_eq!(value["api_key"], json!("[REDACTED]"));

        let events = drain_tool_call_events(&mut receiver);
        assert_eq!(events.len(), 2, "{events:?}");
        let finished = events[1].payload.to_string();
        assert!(
            !finished.contains(credential),
            "the finished row is redacted: {finished}"
        );
        assert_eq!(events[1].payload["success"], json!(true));

        let records = drain_turn_ledger(&grant.turn_ledger);
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(
            !records[0].content.contains(credential),
            "the record is redacted: {}",
            records[0].content
        );
        assert_eq!(
            records[0].content, text,
            "the record carries what the mouth saw"
        );
    }

    /// A conversation grant with the given bridge, registered so a test
    /// can revoke it mid-call: the resolved grant, its ledger, and the
    /// registry with the token to revoke.
    async fn registered_bridged_grant(
        broadcaster: &std::sync::Arc<RuntimeTransportBroadcaster>,
        bridge: ChatMouthBridge,
    ) -> (
        PlaneGrant,
        Option<std::sync::Arc<std::sync::Mutex<PlaneTurnLedger>>>,
        crate::magician_v2::execution::plane::grant::PlaneGrantRegistry,
        String,
    ) {
        let mut grant = chat_turn_grant().with_turn_tool_budget(3, 0);
        grant.executors = Some(std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(broadcaster)),
        ));
        let grant = grant.with_bridged_tools(bridged_specs(&["create_chat_thread"]), bridge);
        let registry = crate::magician_v2::execution::plane::grant::PlaneGrantRegistry::default();
        let token = registry.mint_chat(grant).await;
        let grant = registry
            .resolve(&token)
            .await
            .expect("chat grant must resolve");
        let ledger = grant.turn_ledger.clone();
        (grant, ledger, registry, token)
    }

    /// A grant revoked while a bridged call is in flight — the turn
    /// settling, a harness timeout — cancels the call's own token and
    /// waits for the native dispatcher to answer it, rather than dropping
    /// the dispatcher mid-way (it releases what it registered only on its
    /// own way out). A dispatcher that observes the token answers with its
    /// own cancelled-shaped result, and that result is the call's: an
    /// error by the native status vocabulary, not the door's revoked
    /// error, with the finished row and the ledger record it would have
    /// had, and the bound spent.
    #[tokio::test]
    async fn a_revoked_grant_cancels_an_in_flight_bridged_call() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let (grant, ledger, registry, token) = registered_bridged_grant(
            &broadcaster,
            cooperative_bridge(json!({
                "status": "cancelled",
                "error_code": "CompiledDispatchCancelled",
                "reason": "compiled chat dispatch was cancelled while in progress",
            })),
        )
        .await;

        let revoker = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            registry.revoke(&token).await;
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            plane_tools_call(&grant, "create_chat_thread", &json!({"title": "x"})),
        )
        .await
        .expect("a dispatcher that answers its token returns the door promptly");
        revoker.await.expect("revoker");

        assert_eq!(result["isError"], json!(true), "{result}");
        assert!(
            result.get("_meta").is_none(),
            "the dispatcher's own answer, not the door's revoked error: {result}"
        );
        let text = result["content"][0]["text"].as_str().expect("text");
        let value: Value = serde_json::from_str(text).expect("compact JSON");
        assert_eq!(value["status"], json!("cancelled"));
        assert_eq!(value["error_code"], json!("CompiledDispatchCancelled"));

        let events = drain_tool_call_events(&mut receiver);
        let kinds: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(kinds, vec!["tool.call.started", "tool.call.finished"]);
        let finished = &events[1].payload;
        assert_eq!(finished["success"], json!(false));
        assert!(finished["error"]
            .as_str()
            .is_some_and(|error| error.contains("cancelled")));

        let records = drain_turn_ledger(&ledger);
        assert_eq!(
            records.len(),
            1,
            "the dispatcher's answer is recorded: {records:?}"
        );
        assert!(records[0].is_error);
        assert_eq!(records[0].content, text);
        assert_eq!(
            grant.turn_tool_calls_spent(),
            1,
            "an attempted call spends the bound"
        );
    }

    /// A dispatcher that ignores its token is given the cancel grace and no
    /// more: the door then returns the revoked error with a failed finished
    /// row and records nothing — the turn that drains the ledger is already
    /// gone — while the detached dispatch is left to unwind on its own,
    /// never aborted. The token it was handed is cancelled. Paused time:
    /// the grace elapses without the test waiting it out.
    #[tokio::test(start_paused = true)]
    async fn a_revoked_grant_returns_the_door_after_the_grace_when_the_bridge_ignores_its_token() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let (bridge, handed) = parked_bridge();
        let (grant, ledger, registry, token) = registered_bridged_grant(&broadcaster, bridge).await;

        let revoker = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            registry.revoke(&token).await;
        });
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            BRIDGED_CANCEL_GRACE + std::time::Duration::from_secs(5),
            plane_tools_call(&grant, "create_chat_thread", &json!({"title": "x"})),
        )
        .await
        .expect("the revoke must return the in-flight bridged call after the grace");
        revoker.await.expect("revoker");
        assert!(
            started.elapsed() >= BRIDGED_CANCEL_GRACE,
            "the dispatcher is given the whole grace to answer"
        );

        assert_eq!(result["isError"], json!(true), "{result}");
        assert_eq!(result["_meta"]["planeGrant"], json!("revoked"));
        assert!(result["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("revoked")));
        let handed = handed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("the bridge was handed the call's token");
        assert!(handed.is_cancelled(), "the native dispatch sees the cancel");

        let events = drain_tool_call_events(&mut receiver);
        let kinds: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(kinds, vec!["tool.call.started", "tool.call.finished"]);
        let finished = &events[1].payload;
        assert_eq!(finished["success"], json!(false));
        assert!(finished["error"]
            .as_str()
            .is_some_and(|error| error.contains("revoked")));

        assert!(
            drain_turn_ledger(&ledger).is_empty(),
            "a call that never answered is not recorded"
        );
        assert_eq!(
            grant.turn_tool_calls_spent(),
            1,
            "an attempted call spends the bound"
        );
    }

    /// A bridged name on a grant that carries no bridge is an honest error
    /// at the door, not a lowering attempt: nothing is dispatched, counted,
    /// recorded, or emitted.
    #[tokio::test]
    async fn a_bridged_name_without_a_bridge_is_an_honest_error() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let mut grant = chat_turn_grant().with_turn_tool_budget(3, 0);
        grant.executors = Some(std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(&broadcaster)),
        ));
        grant.bridged_tools = bridged_specs(&["create_chat_thread"]);
        assert!(grant.mouth_bridge.is_none());

        let result = plane_tools_call(&grant, "create_chat_thread", &json!({})).await;
        assert_eq!(result["isError"], json!(true), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("carries no mouth bridge"),
            "the refusal names the missing bridge: {text}"
        );
        assert_eq!(grant.turn_tool_calls_spent(), 0);
        assert!(drain_turn_ledger(&grant.turn_ledger).is_empty());
        assert!(drain_tool_call_events(&mut receiver).is_empty());
    }

    /// The native dispatcher signals failure in its own status vocabulary;
    /// a bridged failure is an MCP error carrying that object verbatim, its
    /// finished row reads the reason the native mouth would have shown, and
    /// its record is the object itself — already a status object, so not
    /// wrapped in a second one. A spent bound refuses before the bridge.
    #[tokio::test]
    async fn a_bridged_error_result_is_an_mcp_error() {
        let broadcaster = std::sync::Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut receiver = broadcaster.subscribe();
        let answer = json!({
            "status": "error",
            "reason": "Tool `create_chat_thread` is not available in this chat turn.",
            "error_code": "ChatToolNotAllowed",
        });
        let mut grant = chat_turn_grant().with_turn_tool_budget(1, 0);
        grant.executors = Some(std::sync::Arc::new(
            test_executors().with_event_broadcaster(std::sync::Arc::clone(&broadcaster)),
        ));
        let grant = grant.with_bridged_tools(
            bridged_specs(&["create_chat_thread"]),
            fixed_bridge(answer.clone()),
        );

        let result = plane_tools_call(&grant, "create_chat_thread", &json!({"title": "x"})).await;
        assert_eq!(result["isError"], json!(true), "{result}");
        let text = result["content"][0]["text"].as_str().expect("text");
        assert_eq!(
            serde_json::from_str::<Value>(text).expect("compact JSON"),
            answer
        );

        let events = drain_tool_call_events(&mut receiver);
        assert_eq!(events.len(), 2, "{events:?}");
        let finished = &events[1].payload;
        assert_eq!(finished["success"], json!(false));
        assert_eq!(
            finished["error"],
            json!("Tool `create_chat_thread` is not available in this chat turn.")
        );

        let records = drain_turn_ledger(&grant.turn_ledger);
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(records[0].is_error);
        assert_eq!(
            serde_json::from_str::<Value>(&records[0].content).expect("compact JSON"),
            answer,
            "the record is the native status object itself"
        );
        assert_eq!(grant.turn_tool_calls_spent(), 1);

        // The bound is spent: the next bridged call ends the turn the way
        // a plane-executed call would, and never reaches the bridge.
        let refused = plane_tools_call(&grant, "create_chat_thread", &json!({})).await;
        assert_eq!(refused["isError"], json!(true));
        assert_eq!(
            refused["_meta"]["planeTurnStop"],
            json!("turn_budget_spent")
        );
        assert_eq!(
            grant.turn_stop_reason(),
            Some(PlaneTurnStopReason::TurnBudgetSpent)
        );
        assert_eq!(
            grant.turn_tool_calls_spent(),
            1,
            "a refused call never counts"
        );
        assert!(drain_turn_ledger(&grant.turn_ledger).is_empty());
        assert!(drain_tool_call_events(&mut receiver).is_empty());
    }

    /// The native vocabulary decides failure, not the literal `error`
    /// status alone: a denial is an error; a plain ok object is not.
    #[test]
    fn a_bridged_result_is_read_in_the_native_status_vocabulary() {
        assert!(native_result_is_error(
            &json!({"status": "error", "reason": "no"})
        ));
        assert!(native_result_is_error(&json!({"status": "denied"})));
        assert!(native_result_is_error(
            &json!({"error_code": "ChatToolNotAllowed"})
        ));
        assert!(!native_result_is_error(&json!({"status": "ok"})));
        assert!(!native_result_is_error(
            &json!({"status": "approval_required"})
        ));
        assert!(!native_result_is_error(&json!({"sessions": []})));
    }

    /// `read_result` is the loop's control verb everywhere but on a grant
    /// that bridges it: there it is the native mouth's tool and crosses;
    /// on a terminal grant, and on a conversation grant that does not
    /// bridge it, the door still refuses it.
    #[tokio::test]
    async fn read_result_is_bridged_on_a_conversation_grant_but_stays_a_control_verb_elsewhere() {
        let mut grant = chat_turn_grant();
        grant.executors = Some(std::sync::Arc::new(test_executors()));
        grant.allowed_tools = vec!["list_tasks".to_string()];
        let grant = grant.with_bridged_tools(bridged_specs(&["read_result"]), echo_bridge());
        let arguments = json!({"result_ref": "res-1"});
        let result = plane_tools_call(&grant, "read_result", &arguments).await;
        assert_eq!(result["isError"], json!(false), "{result}");
        let value: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(value["echo"]["name"], json!("read_result"));
        assert_eq!(value["echo"]["args"], arguments);
        assert_eq!(drain_turn_ledger(&grant.turn_ledger).len(), 1);

        let unbridged = chat_turn_grant();
        let refused = plane_tools_call(&unbridged, "read_result", &arguments).await;
        assert_eq!(refused["isError"], json!(true));
        assert!(refused["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("is not available on the plane")));

        let terminal = PlaneGrant::for_test("exec-terminal");
        let refused = plane_tools_call(&terminal, "read_result", &arguments).await;
        assert_eq!(refused["isError"], json!(true));
        assert!(refused["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("is not available on the plane")));
    }

    #[tokio::test]
    async fn tool_search_still_loads_without_executors() {
        let grant = test_grant_with_index(&["duckdb__query"]);
        let result = plane_tools_call(
            &grant,
            "tool_search",
            &json!({"query": "select:duckdb__query"}),
        )
        .await;
        assert_eq!(result["isError"], json!(false));
        assert_eq!(result["_meta"]["toolsListChanged"], json!(true));
    }

    #[tokio::test]
    async fn knowing_a_hidden_tool_name_is_not_dispatch_authority() {
        let grant = test_grant_with_index(&["duckdb__query"]);
        let result = plane_tools_call(&grant, "duckdb__query", &json!({})).await;
        assert_eq!(result["isError"], json!(true));
        assert!(grant.loaded_tool_names().is_empty());
    }

    /// A bridge that answers with the parent engine its future observed, so
    /// a test can see what the native dispatcher behind a bridged name would
    /// route under — on the job the door spawns, not the door's own task.
    fn parent_probe_bridge() -> ChatMouthBridge {
        use crate::magician_v2::query_analysis::parent_engine::current_parent_engine;
        std::sync::Arc::new(
            |_call_id: String,
             _name: String,
             _arguments: Value,
             _cancel: CancellationToken|
             -> BoxFuture<'static, Value> {
                Box::pin(async move {
                    json!({"status": "ok", "parent_engine": current_parent_engine()})
                })
            },
        )
    }

    /// A conversation grant minted on the given chat mouth, with a bridged
    /// name that reports the parent engine it ran under.
    fn chat_turn_grant_on(engine: &str) -> PlaneGrant {
        let mut ctx = chat_turn_grant().ctx;
        ctx.harness_engine = Some(engine.to_string());
        let mut grant =
            PlaneGrant::for_conversation(ctx, "sess-chat".to_string(), CancellationToken::new());
        grant.executors = Some(std::sync::Arc::new(test_executors()));
        grant.allowed_tools = vec!["list_tasks".to_string()];
        grant.with_bridged_tools(
            bridged_specs(&["create_chat_thread"]),
            parent_probe_bridge(),
        )
    }

    /// Every plane call runs under the grant's parent engine — the chat
    /// mouth of a conversation grant — and a bridged call's job, another
    /// task, is re-scoped onto it, so the native dispatcher's own LLM
    /// operations follow the mouth. The scope ends with the call, and a
    /// native mouth's grant names no parent.
    #[tokio::test]
    async fn a_plane_call_runs_under_the_grant_parent() {
        use crate::magician_v2::query_analysis::parent_engine::current_parent_engine;

        let grant = chat_turn_grant_on("grok");
        assert_eq!(grant_parent_engine(&grant).as_deref(), Some("grok"));
        assert_eq!(
            current_parent_engine(),
            None,
            "no ambient parent outside a call"
        );
        let result = plane_tools_call(&grant, "create_chat_thread", &json!({"title": "x"})).await;
        assert_eq!(result["isError"], json!(false), "{result}");
        let value: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().expect("text"))
                .expect("compact JSON");
        assert_eq!(
            value["parent_engine"],
            json!("grok"),
            "the bridged job runs under the grant's parent: {value}"
        );
        assert_eq!(
            current_parent_engine(),
            None,
            "the scope ends with the call"
        );

        let native = chat_turn_grant_on("magician");
        assert_eq!(grant_parent_engine(&native), None);
        let result = plane_tools_call(&native, "create_chat_thread", &json!({"title": "x"})).await;
        assert_eq!(result["isError"], json!(false), "{result}");
        let value: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().expect("text"))
                .expect("compact JSON");
        assert_eq!(
            value["parent_engine"],
            Value::Null,
            "the native mouth is no parent: {value}"
        );
    }

    /// A run grant names the run's engine as its parent — the launch pin,
    /// else the process snapshot — and the native loop names none.
    #[test]
    fn a_run_grant_carries_the_run_engine_as_parent() {
        let mut ctx = chat_turn_grant().ctx;
        ctx.harness_engine = Some("codex".to_string());
        let run = PlaneGrant::for_run(
            ctx,
            std::sync::Arc::new(test_executors()),
            "exec-parent".to_string(),
        );
        assert_eq!(grant_parent_engine(&run).as_deref(), Some("codex"));

        let mut ctx = chat_turn_grant().ctx;
        ctx.harness_engine = Some("magician".to_string());
        let native = PlaneGrant::for_run(
            ctx,
            std::sync::Arc::new(test_executors()),
            "exec-native".to_string(),
        );
        assert_eq!(
            grant_parent_engine(&native),
            None,
            "the native loop is no parent"
        );
    }

    /// The parent scope wraps the whole transaction at every dispatch-lock
    /// site — the call, the pending approval, the approved capture — under
    /// the lock and ahead of the first gate, so no refusal, search, or
    /// dispatch path runs outside it.
    #[test]
    fn every_plane_call_is_scoped_under_the_grant_parent() {
        let source = include_str!("dispatch.rs");
        let impl_src = source.split("#[cfg(test)]").next().unwrap_or(source);
        let after_locks: Vec<&str> = impl_src
            .split("let _dispatch_guard = grant.lock_dispatch().await;")
            .skip(1)
            .collect();
        assert!(
            after_locks.len() >= 3,
            "the call, the pending approval, and the approved capture each take the lock"
        );
        for after_lock in &after_locks {
            let scope = after_lock
                .find("with_parent_engine(")
                .expect("the parent scope after the dispatch lock");
            let parent = after_lock
                .find("grant_parent_engine(grant)")
                .expect("the grant's own parent");
            let first_gate = after_lock
                .find("grant.is_revoked()")
                .expect("the first gate under the lock");
            assert!(scope < first_gate && parent < first_gate);
        }
        let ledger = after_locks[0]
            .find("let ledger_session = session_id.map(str::to_string);")
            .expect("the call proper");
        assert!(after_locks[0].find("with_parent_engine(").expect("scope") < ledger);
        let bridged = impl_src
            .split("async fn bridged_dispatch(")
            .nth(1)
            .expect("bridged dispatch");
        let spawn = bridged
            .find("spawn_execution_job(")
            .expect("the bridged job");
        let rescoped = bridged
            .find("with_parent_engine(")
            .expect("the bridged job re-scopes the parent");
        assert!(spawn < rescoped);
    }
}
