//! `Decide` — ask the model what to do next.
//!
//! The third phase lifted out of `'iteration_body`, and the largest one that
//! could still move as-is: roughly 950 lines of body with **zero
//! `break 'iteration_body` and zero outer `return`**. Everything that ends a run
//! on the decision path — the token-budget exhaustion check, the retry ladder,
//! the parse-failure abort — sits *after* the value this phase produces, in
//! [`super::resolve`].
//!
//! # `Decide` is not effect-free, and the phase model says so
//!
//! [`crate::magician_v2::execution::agentic::run_loop::outcome::Phase::may_have_fired_an_effect`]
//! returns `true` for `Decide`, which is the opposite of what "ask the model"
//! suggests. Three things here leave the process: a `browser__screenshot`
//! dispatch when Yutori is the configured decider (so the turn routes onto the
//! image arm at all), a durable read of the task's micro-goal state, and a write
//! of the decision into the observation file. A worker that retried this phase
//! believing it a pure model call would repeat all three.
//!
//! # Two results, deliberately
//!
//! [`run`] returns `Result<DecideOutput>` and `DecideOutput` carries a
//! `decision_result: Result<Decision>`. That is not an oversight, but the split
//! is not where an earlier version of this comment claimed.
//!
//! **The inner one carries every failure this phase actually produces.** A
//! cancelled job, a decision the provider could not deliver, a browser state
//! reached without the primitive pack — all of them land in `decision_result`,
//! because they arise inside the `decision_branch` async block rather than at
//! this function's own scope. `Resolve` is what reads them, and it is what turns
//! them into a backoff, an extra turn, or a settled run.
//!
//! **The outer `Result` currently never fails.** Nothing at `run`'s own scope is
//! fallible; it is `Result` so that a hard failure has somewhere to go if one is
//! ever introduced here, and so the driver's call site does not have to change
//! shape when that happens. Saying so plainly is better than implying a
//! propagation path that does not exist — a reader chasing "no scheduler lane"
//! out of this function would not find it.
//!
//! What the split buys is unchanged and is the reason to keep it: collapsing the
//! two would make a transient parse failure terminal, because the loop absorbs
//! the inner error and propagates the outer one.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::magician_v2::execution::actions::ExecutableAction;
use crate::magician_v2::execution::agentic::decision::Decision;
use crate::magician_v2::execution::agentic::executor::{
    action_summary_for_diagnostics, agentic_decision_call_identity, agentic_decision_uses_yutori,
    agentic_iteration_id, agentic_llm_event_correlation, build_flat_browser_state,
    build_primitive_exec_ctx, definition_of_done_signal, delegation_targets_markdown,
    emit_agent_execution_mapping, flatten_scheduled_agentic_decision, llm_task_ref_for_context,
    resolved_durable_micro_goals, response_kind_from_decision, routing_overrides_for_run,
    runtime_execution_id, runtime_execution_id_opt, schedule_agentic_decision_job, transport_scope,
    ActionExecutors, HeapAwaitExt,
};
use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;
use crate::magician_v2::execution::agentic::types::{
    action_signature, action_type_and_tool_name, AgenticAssistantTurnRecord, AgenticContext,
    ApprovedConfirmationAction, EnvironmentState, ExecutionHistory, LoopProtectiveState,
};
use crate::magician_v2::execution::durable_task_state::TaskStateActionEnvelope;
use crate::magician_v2::execution::screenshot_cache::SoMDecisionRecord;
use crate::magician_v2::execution::verified_executor::types::{ActionCandidate, CandidateBatch};
use crate::magician_v2::RuntimeTransportEvent;

/// Iterations a kept screenshot stays offered to the planner model. Each one
/// is an action the rail ran after it was taken, so an older image is more
/// likely to show a screen that is gone than to help.
const MAX_CARRIED_SCREENSHOT_AGE: usize = 3;

/// Keep this state's screenshot for the planner model's next turns. The model
/// only sees the screenshot of the state it decides on: when the rail decides
/// the step it never sees this one, and when it decides itself it loses it on
/// the next turn (often spent fetching element references). A state without a
/// screenshot leaves an earlier kept one in place. True when this state's
/// screenshot is new; false when there is none or it is the kept one again.
async fn keep_screenshot_for_planner(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    observed_state: &EnvironmentState,
    history: &ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    iteration: usize,
) -> bool {
    let image = crate::magician_v2::execution::agentic::decision::extract_screenshot(
        observed_state,
        executors.screenshot_storage.as_ref(),
        runtime_execution_id_opt(ctx),
        history,
    )
    .await
    .and_then(|images| images.into_iter().next());
    let Some(image) = image else {
        return false;
    };
    // After a click the browser state can still hold the capture from before
    // it. The same image is not a new one: it keeps the iteration it was
    // taken in, so its caption still names the steps since.
    if loop_protective
        .planner_screenshot_carry
        .as_ref()
        .is_some_and(|carry| carry.image.base64 == image.base64)
    {
        return false;
    }
    loop_protective.planner_screenshot_carry =
        Some(crate::magician_v2::execution::agentic::types::CarriedScreenshot { image, iteration });
    true
}

/// The kept screenshot for this planner turn, with a caption naming the steps
/// run since it was taken. Offered on every turn until a newer screenshot
/// replaces it or it is older than `MAX_CARRIED_SCREENSHOT_AGE` iterations.
fn carried_screenshot_for_turn(
    ctx: &AgenticContext,
    history: &ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    iteration: usize,
) -> Option<(
    crate::magician_v2::slot_graph::extraction::ImageData,
    String,
)> {
    let carry = loop_protective.planner_screenshot_carry.as_ref()?;
    if iteration.saturating_sub(carry.iteration) > MAX_CARRIED_SCREENSHOT_AGE {
        loop_protective.planner_screenshot_carry = None;
        return None;
    }
    let carry = carry.clone();
    let since: Vec<String> = history
        .iterations
        .iter()
        .filter(|record| record.iteration >= carry.iteration)
        .map(|record| {
            let summary =
                action_summary_for_diagnostics(&record.action, ctx.app_disclosure_guard.is_some());
            match summary.char_indices().nth(120) {
                Some((cut, _)) => format!("{}…", &summary[..cut]),
                None => summary,
            }
        })
        .collect();
    if since.is_empty() {
        // The state it came from is the one being decided: the model sees it
        // directly.
        return None;
    }
    let caption = format!(
        "The attached screenshot was taken at iteration {}, before these steps: {}. It \
         may not show the current screen; trust the current observation for anything \
         those steps changed.",
        carry.iteration,
        since.join("; ")
    );
    Some((carry.image, caption))
}

/// What the rest of the iteration reads from `Decide`.
///
/// Six of these seven fields are `let mut` bindings the decision branch fills
/// in as the provider call settles, and every one of them is read *after* the
/// branch — by `Resolve` when it records the assistant turn, and by three arms
/// of `Apply`. Lifting the branch without carrying them out would have compiled
/// only if the reads were recomputed, and there is nothing to recompute them
/// from: they are the model call's own byproducts.
pub struct DecideOutput {
    /// The decision, or the soft failure `Resolve` gets to absorb.
    pub decision_result: Result<Decision>,
    /// Durable task-state mutation the decision asked for. Applied by the
    /// `Completed`, `Yield` and `Execute` arms of `Apply`, never by the others.
    pub task_state_action_for_decision: TaskStateActionEnvelope,
    /// The assistant turn to append to history, when the provider returned one.
    pub assistant_turn_for_decision: Option<AgenticAssistantTurnRecord>,
    /// Provider response id, threaded into the next turn's continuation.
    pub response_id_for_decision: Option<String>,
    /// Whether this turn's prompt carried images; drives the next turn's
    /// continuation shape.
    pub has_images_for_decision: bool,
    /// Per-call LLM trace, moved onto the assistant turn by `Resolve`.
    pub llm_trace_context_for_decision: Option<magicllm::LlmTraceContext>,
    /// True when the decision is a runtime stop rather than a model-emitted
    /// terminal: the orchestrator-synthesised stuck-auto-yield, or a
    /// definition-of-done yield (the model wrote its text, the runtime made it
    /// the terminal). The give-up gate in `Apply::Yield` exempts it, because it
    /// already *is* the exhaustion signal, and Resolve does not refuse it under
    /// the owner's allowlist. (Named for its first use; it is persisted.)
    pub decision_is_synthetic_stuck: bool,
}

/// Serializable half of [`DecideOutput`]. `anyhow::Error` deliberately does
/// not cross the boundary; its bounded display text does, so Resolve preserves
/// the same soft-failure behavior without attempting to deserialize a dynamic
/// error chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableDecideOutput {
    pub decision_result: std::result::Result<Decision, String>,
    pub task_state_action_for_decision: TaskStateActionEnvelope,
    pub assistant_turn_for_decision: Option<AgenticAssistantTurnRecord>,
    pub response_id_for_decision: Option<String>,
    pub has_images_for_decision: bool,
    pub llm_trace_context_for_decision: Option<magicllm::LlmTraceContext>,
    pub decision_is_synthetic_stuck: bool,
}

impl DecideOutput {
    pub fn to_durable(&self) -> DurableDecideOutput {
        DurableDecideOutput {
            decision_result: self
                .decision_result
                .as_ref()
                .map(Clone::clone)
                .map_err(ToString::to_string),
            task_state_action_for_decision: self.task_state_action_for_decision.clone(),
            assistant_turn_for_decision: self.assistant_turn_for_decision.clone(),
            response_id_for_decision: self.response_id_for_decision.clone(),
            has_images_for_decision: self.has_images_for_decision,
            llm_trace_context_for_decision: self.llm_trace_context_for_decision.clone(),
            decision_is_synthetic_stuck: self.decision_is_synthetic_stuck,
        }
    }
}

impl DurableDecideOutput {
    pub fn into_live(self) -> DecideOutput {
        DecideOutput {
            decision_result: self.decision_result.map_err(|error| anyhow!(error)),
            task_state_action_for_decision: self.task_state_action_for_decision,
            assistant_turn_for_decision: self.assistant_turn_for_decision,
            response_id_for_decision: self.response_id_for_decision,
            has_images_for_decision: self.has_images_for_decision,
            llm_trace_context_for_decision: self.llm_trace_context_for_decision,
            decision_is_synthetic_stuck: self.decision_is_synthetic_stuck,
        }
    }
}

/// Run the decide phase.
///
/// `observed_state` is `&mut` for one reason: when Yutori is the configured
/// browser decider and a session is open, this phase re-observes through a
/// `browser__screenshot` dispatch and replaces the state the decision is made
/// against. Passing it by value and returning it would have said the same thing
/// less clearly, since every other use here is a read.
///
/// `ctx` is shared rather than exclusive. Every use here is a field read, a
/// clone, or a call whose parameter is `&AgenticContext`; the phases that
/// genuinely mutate it are `Resolve` (the owner/trust refresh) and `Apply` (the
/// owner transition). The module's contract is that the parameter list IS the
/// checked statement of what a phase may touch, and a `&mut` nothing writes
/// through weakens it.
#[allow(clippy::too_many_arguments)]
/// The marker the routing notice carries, so it is appended once per
/// execution and not once per iteration.
const WORKING_SET_ROUTING_NOTICE_MARKER: &str = "## WORKING-SET ROUTING IS ACTIVE";

/// Append the working-set routing notice to the decision prompt's
/// supplemental knowledge the first time the execution's index says the path
/// is open. One small file read per iteration until then; nothing for an
/// execution that never captured.
pub(super) async fn refresh_working_set_routing_notice(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
) {
    let (Some(execution_id), Some(principal), Some(workspace)) = (
        ctx.execution_id.as_deref(),
        ctx.principal.as_deref(),
        ctx.workspace.as_deref(),
    ) else {
        return;
    };
    let already = ctx
        .scratch
        .supplemental_environment_knowledge
        .lock()
        .map(|value| value.contains(WORKING_SET_ROUTING_NOTICE_MARKER))
        .unwrap_or(true);
    if already {
        return;
    }
    let store = crate::magician_v2::artifact_v2::WorkingSetStore::new(
        executors.artifact_v2_workspace.clone(),
    );
    let Ok(Some(index)) = store
        .execution_index(principal, workspace, execution_id)
        .await
    else {
        return;
    };
    let Some(activation) = index.activation.as_ref() else {
        return;
    };
    // Open but narrowing nothing — every page so far fit the ordinary window
    // — the model sees exactly what it always saw, and telling it pages are
    // withheld would send it searching for what is already in front of it.
    if index.narrowed_reads == 0 {
        return;
    }
    let notice = working_set_routing_notice(&index, &activation.reason);
    if let Ok(mut supplemental) = ctx.scratch.supplemental_environment_knowledge.lock() {
        if !supplemental.is_empty() {
            supplemental.push_str("\n\n");
        }
        supplemental.push_str(&notice);
    }
    info!(
        execution_id,
        members = index.members.len(),
        total_source_bytes = index.total_source_bytes,
        "[WORKING-SET] routing notice joined the decision prompt"
    );
}

fn working_set_routing_notice(
    index: &crate::magician_v2::artifact_v2::WorkingSetExecutionIndex,
    reason: &str,
) -> String {
    format!(
        "{WORKING_SET_ROUTING_NOTICE_MARKER}\n\
         This task's evidence is held in a working set ({} page{} captured, {} KB; opened \
         because {reason}). A page larger than the ordinary window is now shown only by its \
         head; the whole page is captured and searchable. A page that fits is shown whole. Keep opening NEW pages with `content_read` as the \
         research needs them. But before you settle any specific fact a page you already opened \
         should hold — a price, a rate, a figure, a name, a date, a unit — call \
         `working_set_search` with no `working_set_id` and the precise phrase, value or identifier \
         you expect, then `working_set_read` the cited chunk. Never conclude from a page's head \
         that the page lacks a fact, never re-read an opened page to find one, and never use \
         `content_search` to find again a page you already opened. Cite the working_set_id, \
         source and chunk you read.",
        index.members.len(),
        if index.members.len() == 1 { "" } else { "s" },
        index.total_source_bytes / 1024,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::magician_v2::execution::agentic) async fn run(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    history: &ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    approved_confirmation_actions: &[ApprovedConfirmationAction],
    observed_state: &mut EnvironmentState,
    pending_operator_steer: &[String],
    cancellation_token: Option<&CancellationToken>,
    browser_primitive_enabled: bool,
    iteration: usize,
) -> Result<DecideOutput> {
    let output = Box::pin(run_inner(
        ctx,
        executors,
        history,
        loop_protective,
        approved_confirmation_actions,
        observed_state,
        pending_operator_steer,
        cancellation_token,
        browser_primitive_enabled,
        iteration,
    ))
    .await;
    // Publish after every decider — the pinned harness, the decision rail,
    // a caller-decided call and the native model — and on every exit, early
    // returns included. A terminal decision may never reach another iteration
    // check, so this is the last chance to record what the turn spent: a
    // one-turn harness run that returned early used to finish with
    // `cost_used_usd: 0.0, tokens_used: 0`.
    super::super::super::run_budget::refresh_running_budget_with_harness_tokens(
        ctx,
        loop_protective.cumulative_run_cost_usd,
        loop_protective.cumulative_harness_tokens,
        iteration,
        loop_protective.parked_iteration_refunds,
    );
    // A coding-engine turn is paid for outside the router, so the router's
    // refusal never reaches it. A decision that would start one while the
    // owner's spend envelope is spent hands Resolve the typed refusal
    // instead: Resolve parks the run (no failed step is recorded and no retry
    // or stuck counter moves) and it decides again once the window resets.
    // Dispatching it would only record a refused, failed step.
    output.map(|mut output| {
        if let Some(refusal) = coding_decision_spend_refusal(
            ctx,
            &output.decision_result,
            crate::magician_v2::spend_envelope_producers::preflight,
        ) {
            output.decision_result = Err(refusal);
        }
        output
    })
}

/// The spend envelope's refusal for a decision that would start a
/// coding-engine turn (`run_coding_task`) under this run's class, `None` for
/// any other decision or while the envelope admits the run.
fn coding_decision_spend_refusal(
    ctx: &AgenticContext,
    decision: &Result<Decision>,
    preflight: impl Fn(magicllm::LlmWorkloadClass) -> Result<()>,
) -> Option<anyhow::Error> {
    let Ok(Decision::Execute { candidates, .. }) = decision else {
        return None;
    };
    let starts_coding = candidates.candidates.iter().any(|candidate| {
        matches!(
            &candidate.action,
            ExecutableAction::Pack { capability_name, .. }
                if capability_name
                    == crate::magician_v2::execution::compiled_handlers::run_coding_task::TOOL_NAME
        )
    });
    if !starts_coding {
        return None;
    }
    preflight(crate::magician_v2::spend_envelope_producers::agentic_run_class(ctx)).err()
}

#[allow(clippy::too_many_arguments)]
async fn run_inner(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    history: &ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    approved_confirmation_actions: &[ApprovedConfirmationAction],
    observed_state: &mut EnvironmentState,
    pending_operator_steer: &[String],
    cancellation_token: Option<&CancellationToken>,
    browser_primitive_enabled: bool,
    iteration: usize,
) -> Result<DecideOutput> {
    // A caller-decided pack call (chat pack direct dispatch) is returned
    // before any decider runs: the rail, a pinned harness and the model never
    // see the turn. Its `Execute` first projects the owner's tool policy the
    // way a model turn does, so Apply's snapshot check, and everything after
    // it, applies unchanged.
    if let Some(output) =
        super::predecided_decision::maybe_predecided_decide(ctx, history, iteration)
    {
        let output = super::predecided_decision::authorize(ctx, iteration, output).await;
        super::predecided_decision::emit_decision_made(
            ctx,
            executors,
            iteration,
            Phase::Decide,
            &output,
        );
        return Ok(output);
    }
    // The first `Decide` after an approving resume runs the call the owner
    // approved instead of asking the model to re-emit it byte for byte; see
    // `approved_replay`. Every later iteration decides as usual.
    if let Some(output) = super::approved_replay::maybe_replay_approved(
        ctx,
        executors,
        approved_confirmation_actions,
        loop_protective,
        pending_operator_steer,
        iteration,
    )
    .await
    {
        return Ok(output);
    }
    // Set when Jev cannot ground a step for Magician's own model: the model
    // decides below, and the engine validates its calls before they run.
    let mut proposal_check = None;
    match super::decision_rail::maybe_decide(
        ctx,
        executors,
        observed_state,
        history,
        loop_protective,
        pending_operator_steer,
        cancellation_token,
        iteration,
        &mut proposal_check,
    )
    .await
    {
        Ok(Some(output)) => {
            let _ = keep_screenshot_for_planner(
                ctx,
                executors,
                observed_state,
                history,
                loop_protective,
                iteration,
            )
            .heap_boxed()
            .await;
            return Ok(output);
        },
        Ok(None) => {},
        Err(error) => {
            return Ok(DecideOutput {
                decision_result: Err(error),
                task_state_action_for_decision: TaskStateActionEnvelope::default(),
                assistant_turn_for_decision: None,
                response_id_for_decision: None,
                has_images_for_decision: false,
                llm_trace_context_for_decision: None,
                decision_is_synthetic_stuck: false,
            })
        },
    }
    let harness_decision =
        match crate::magician_v2::execution::plane::turn_engine::maybe_harness_decide(
            ctx,
            executors,
            history,
            loop_protective,
            observed_state,
            pending_operator_steer,
            cancellation_token,
            iteration,
        )
        .await
        {
            Ok(decision) => decision,
            // The owner's spend envelope is spent. That is not a phase failure
            // (which would walk the run toward quarantine): hand it to Resolve
            // as a decision error, which parks the run until the window resets.
            Err(error) if crate::magician_v2::spend_envelope::is_exhaustion_error(&error) => {
                return Ok(DecideOutput {
                    decision_result: Err(error),
                    task_state_action_for_decision: TaskStateActionEnvelope::default(),
                    assistant_turn_for_decision: None,
                    response_id_for_decision: None,
                    has_images_for_decision: false,
                    llm_trace_context_for_decision: None,
                    decision_is_synthetic_stuck: false,
                });
            },
            Err(error) => return Err(error),
        };
    if let Some(decision) = harness_decision {
        // The harness submitted a plan (`submit_plan`) instead of finishing:
        // start it now. Its first step is validated by the engine like every
        // later one; if it cannot start, the harness gets a new turn and sees
        // why, rather than the run ending on the turn's text.
        if loop_protective.decision_rail_plan.is_some() {
            let mut unused_check = None;
            let started = super::decision_rail::maybe_decide(
                ctx,
                executors,
                observed_state,
                history,
                loop_protective,
                pending_operator_steer,
                cancellation_token,
                iteration,
                &mut unused_check,
            )
            .await;
            let decision_result = match started {
                Ok(Some(output)) => {
                    let _ = keep_screenshot_for_planner(
                        ctx,
                        executors,
                        observed_state,
                        history,
                        loop_protective,
                        iteration,
                    )
                    .heap_boxed()
                    .await;
                    return Ok(output);
                },
                Ok(None) => Err(anyhow!(
                    "The plan you submitted did not start: the Decision Engine rejected its \
                     first step, or the run was steered. Decide the next step again."
                )),
                Err(error) => Err(error),
            };
            loop_protective.decision_rail_plan = None;
            return Ok(DecideOutput {
                decision_result,
                task_state_action_for_decision: TaskStateActionEnvelope::default(),
                assistant_turn_for_decision: None,
                response_id_for_decision: None,
                has_images_for_decision: false,
                llm_trace_context_for_decision: None,
                decision_is_synthetic_stuck: false,
            });
        }
        return Ok(DecideOutput {
            decision_result: Ok(decision),
            task_state_action_for_decision: TaskStateActionEnvelope::default(),
            assistant_turn_for_decision: None,
            response_id_for_decision: None,
            has_images_for_decision: false,
            llm_trace_context_for_decision: None,
            decision_is_synthetic_stuck: false,
        });
    }
    loop_protective.decision_rail_consecutive_steps = 0;

    // Boundary B. A tool result can tell the model how to work its evidence,
    // and the first live A/B showed that is a weak steer against the agent's
    // own research instructions: the researcher saw the guidance and searched
    // the web instead. The strong steer is the prompt. Once the router has put
    // this execution on the working-set path — its own record, read here —
    // the notice rides the decision prompt's environment-knowledge section
    // every iteration, alongside the instructions the model actually follows.
    refresh_working_set_routing_notice(ctx, executors).await;
    crate::magician_v2::execution::agentic::executor::refresh_task_filesystem_notice(
        ctx, executors,
    );
    let use_hint_directly = false;
    let mut task_state_action_for_decision = TaskStateActionEnvelope::default();
    let mut assistant_turn_for_decision: Option<AgenticAssistantTurnRecord> = None;
    let mut response_id_for_decision: Option<String> = None;
    let mut prompt_projection_mode_for_decision: Option<String> = None;
    let mut has_images_for_decision: bool = false;
    let mut llm_trace_context_for_decision: Option<magicllm::LlmTraceContext> = None;
    // True when this turn's decision is the orchestrator-synthesised
    // stuck-auto-yield (NOT a model-emitted give-up). The give-up gate
    // exempts it — it already IS the exhaustion signal.
    let mut decision_is_synthetic_stuck: bool = false;
    let decision_result: Result<Decision> = if use_hint_directly {
        let hint_action = ctx
            .hint_action
            .as_ref()
            .map(ExecutableAction::clone_for_retention)
            .unwrap();
        info!(
            "[AGENTIC] Skipping LLM decision on iteration 1 - using Navigate hint \
             directly: {}",
            action_summary_for_diagnostics(&hint_action, ctx.app_disclosure_guard.is_some(),)
        );

        // Emit event for hint usage
        if ctx.has_observability() {
            let (principal, workspace) = transport_scope(ctx);
            emit_agent_execution_mapping(ctx, executors, Some((iteration, Phase::Decide)));
            let (h_action_type, h_tool_name) = action_type_and_tool_name(&hint_action);
            let hint_decision_event = RuntimeTransportEvent::AgenticDecisionMade {
                execution_id: runtime_execution_id(ctx),
                principal,
                workspace,
                plan_id: ctx.plan_id.clone().unwrap_or_default(),
                step_id: ctx.step_id.clone().unwrap_or_default(),
                iteration,
                decision_type: "execute".to_string(),
                action_summary: Some(action_summary_for_diagnostics(
                    &hint_action,
                    ctx.app_disclosure_guard.is_some(),
                )),
                reasoning: "Using hint action directly (blank page optimization)".to_string(),
                confidence: 1.0,
                thinking: Some(
                    "Blank page optimization — using hint action directly without LLM".to_string(),
                ),
                evidence: None,
                tool_name: Some(h_tool_name),
                action_type: Some(h_action_type),
                element_id: None,
                candidates_count: Some(1),
                raw_decision: None,
                timestamp: Utc::now().timestamp_millis(),
            };
            super::outbox::journal_and_emit(
                ctx,
                executors,
                iteration,
                Phase::Decide,
                hint_decision_event,
            );
        }

        // Wrap hint action in a CandidateBatch for unified execution path
        let candidate = ActionCandidate::new(1, 1.0, hint_action)
            .with_reasoning("Hint action for blank page optimization".to_string())
            .with_criticality_hint("low".to_string());
        let batch = CandidateBatch::new(
            vec![candidate],
            "Blank page optimization - using hint action directly",
        );

        Ok(Decision::Execute {
            candidates: batch,
            thinking: "Using hint action directly (blank page optimization)".to_string(),
        })
    } else {
        let decision_branch = async {
            // Normal path: Ask LLM for decision
            // Emit LLMRequestSent for decision, carrying the identity the call
            // will be routed under so every surface can pair this request
            // with the call's `LLMRouted` and `LLMResponseReceived`.
            let decision_call_identity = if ctx.has_observability() {
                let (principal, workspace) = transport_scope(ctx);
                let (identity, correlation) = agentic_decision_call_identity(
                    ctx,
                    principal.as_deref(),
                    workspace.as_deref(),
                    iteration,
                    "decision",
                );
                let request_sent_event = RuntimeTransportEvent::LLMRequestSent {
                    execution_id: runtime_execution_id(ctx),
                    principal,
                    workspace,
                    plan_id: ctx.plan_id.clone().unwrap_or_default(),
                    step_id: ctx.step_id.clone(),
                    step_index: None,
                    capability: "decision".to_string(),
                    request_summary: format!("Deciding next action for iteration {}", iteration),
                    input_tokens_estimate: None,
                    budget_remaining: 0.0,
                    correlation,
                    timestamp: Utc::now().timestamp_millis(),
                };
                // Inline, never journalled — like `AgenticActionStarted`. A
                // journalled record is delivered when the Decide phase
                // commits, which is after this call has returned: the
                // request would reach every surface together with its own
                // response, and a minute-long call would show nothing while
                // it ran.
                executors.emit_event(request_sent_event);
                Some(identity)
            } else {
                None
            };

            let decision_start = Instant::now();
            // Slot that decision functions populate with per-call LLM telemetry
            // (tokens / cache / cost / provider / model). Emitted on the
            // LLMResponseReceived event below.
            let decision_telemetry_slot =
                crate::magician_v2::execution::agentic::decision::new_telemetry_slot();
            let decision_side_call_telemetry = match (
        executors.event_broadcaster.clone(),
        ctx.principal.as_deref(),
        ctx.workspace.as_deref(),
    ) {
        (Some(broadcaster), Some(principal), Some(workspace)) => Some(
            crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
                broadcaster,
                principal,
                workspace,
                "agentic_decision_support",
            ),
        ),
        _ => None,
    };
            // The same overrides the run's scoped router resolves under —
            // the context's endpoints with the run's engine as parent — so the
            // decision's text-only side calls follow the run's engine too.
            let routed_native_adapter = executors
                .native_adapter
                .with_routing_overrides(Some(routing_overrides_for_run(ctx)))
                .with_disclosure_guard(ctx.app_disclosure_guard.clone())
                .with_physical_resource_guard(ctx.physical_resource_guard.clone());
            let iteration_native_adapter = llm_task_ref_for_context(ctx)
                .map(|task_ref| {
                    routed_native_adapter.with_task_context(
                        task_ref.with_iteration(agentic_iteration_id(ctx, iteration)),
                    )
                })
                .unwrap_or(routed_native_adapter);
            let iteration_native_adapter = match decision_call_identity {
                Some(identity) => iteration_native_adapter.with_preassigned_decision_call(identity),
                None => iteration_native_adapter,
            };
            let native_adapter = Arc::new(iteration_native_adapter);

            // Yutori grounds every action on a screenshot, but the flat loop's
            // per-turn observation is the browser TOOL RESULT — for normal actions
            // (snapshot/click/scroll) `build_flat_browser_state` returns
            // `EnvironmentState::Shell` (text, no image), so `extract_screenshot`
            // returns None, `has_images` is false, and the decision never routes to
            // Yutori. The retired Magicutor observe path is gone, so the only
            // working way to get an image here is the agent-browser
            // `browser__screenshot` primitive. So when Yutori is the configured
            // browser decider and a browser session is open, dispatch a read-only
            // `browser__screenshot` and rebuild the observation from it
            // (`build_flat_browser_state` → `Browser` + screenshot). That flips this
            // turn onto the Browser decision arm and makes `has_images` true → routes
            // to Yutori. On any failure we keep the tool-result (text) state.
            if browser_primitive_enabled
                && agentic_decision_uses_yutori(ctx, executors)
                && executors
                    .browser_run
                    .session_used
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                if let Some(registry) = executors.effective_capability_registry_snapshot() {
                    let exec_ctx = build_primitive_exec_ctx(executors);
                    let shot_args = serde_json::json!({ "args": [] });
                    let label = format!("iter{}_yutori_vision", iteration);
                    match crate::magician_v2::execution::flat_loop::dispatch_flat_action(
                        "browser__screenshot",
                        &shot_args,
                        &registry,
                        &exec_ctx,
                        None,
                    )
                    .heap_boxed()
                    .await
                    {
                        Ok(shot_result) => {
                            match build_flat_browser_state(
                                Ok(&shot_result),
                                executors,
                                Some(ctx),
                                &label,
                                None,
                            )
                            .heap_boxed()
                            .await
                            {
                                Ok(state) => *observed_state = state,
                                Err(e) => warn!(
                                    "[YUTORI] build browser state from screenshot failed \
                                     ({e}); using tool-result state"
                                ),
                            }
                        },
                        Err(e) => warn!(
                            "[YUTORI] browser__screenshot dispatch failed ({e}); using \
                             tool-result state"
                        ),
                    }
                }
            }

            // Definition-of-done (phase 2): once the durable task state's
            // micro-goals are ALL resolved, this turn is the run's last — one
            // model turn with tools disabled writes the final answer (the
            // deliverable, what was found, what blocked each blocked
            // requirement) instead of more tool turns re-verifying closed
            // requirements. Computed executor-side so it can load the durable
            // state, then threaded into `decide_next_action`, which runs that
            // turn and builds the terminal (`definition_of_done_terminal`).
            //
            // Only a state this execution wrote counts
            // (`resolved_durable_micro_goals`): a task's previous run leaves
            // its resolved state behind, and a retry or follow-up must not
            // conclude on it before doing any work.
            //
            // Not after the Completed arm rejected one, never twice for the
            // same micro-goal set, at most twice a run
            // (`take_definition_of_done_turn`). The gate reads the same
            // history plus one rejection record, so a re-issued conclusion
            // was rejected verbatim and the third one aborted the run —
            // measured live on a parent whose child had just delivered the
            // answer. After a rejection the model decides with its tools, and
            // can yield with the evidence the gate asks for.
            //
            // Nor while the run still owes its owner a final report (an
            // unattended Autopilot coordinator): the model gets a turn WITH
            // its tools, steered at that report (`definition_of_done_signal`,
            // `decision_preflight`); the final no-tools turn follows once the
            // report is filed.
            let dod_previous_fingerprint = loop_protective.definition_of_done_fingerprint.clone();
            let resolved_micro_goals = if loop_protective.consecutive_goal_reached_rejections == 0 {
                resolved_durable_micro_goals(ctx, executors)
                    .heap_boxed()
                    .await
            } else {
                None
            };
            let dod_signal =
                definition_of_done_signal(ctx, executors, resolved_micro_goals.is_some())
                    .heap_boxed()
                    .await
                    .with_operator_steer(pending_operator_steer);
            let definition_of_done = if dod_signal
                == crate::magician_v2::execution::agentic::decision::DefinitionOfDone::Met
            {
                crate::magician_v2::execution::agentic::decision::take_definition_of_done_turn(
                    loop_protective,
                    resolved_micro_goals,
                )
            } else {
                None
            };
            let definition_of_done_turn = definition_of_done.is_some();
            let dod_goals_for_fallback = definition_of_done.clone();

            // Provider prompt composition and transport are polled from a fresh
            // scheduler-root task. A boxed child still polls synchronously on
            // its parent's call stack; the task boundary prevents a long
            // resumed history from combining provider mapping frames with the
            // executor/resume frames. The helper clones only immutable decision
            // inputs and preserves the exact shared execution token meter.
            let carried_screenshot =
                carried_screenshot_for_turn(ctx, &history, loop_protective, iteration);
            // A new screenshot in this state is the model's to see as current,
            // and is kept for its next turns, which may first fetch element
            // references. Otherwise the kept one goes with its caption, also
            // when the state still holds that same image after an action.
            let fresh_screenshot = keep_screenshot_for_planner(
                ctx,
                executors,
                observed_state,
                &history,
                loop_protective,
                iteration,
            )
            .heap_boxed()
            .await;
            let carried_screenshot = carried_screenshot.filter(|_| !fresh_screenshot);
            if let (Some((_, caption)), Some(broadcaster)) = (
                carried_screenshot.as_ref(),
                executors.event_broadcaster.as_ref(),
            ) {
                super::outbox::journal_and_emit_named(
                    ctx,
                    broadcaster,
                    iteration,
                    Phase::Decide,
                    "planner.earlier_screenshot",
                    ctx.agent_id.as_deref().unwrap_or("__system__"),
                    ctx.principal.as_deref(),
                    ctx.workspace.as_deref(),
                    serde_json::json!({
                        "execution_id": ctx.execution_id,
                        "iteration": iteration,
                        "caption": caption,
                    }),
                );
            }
            let mut decision_job = match &observed_state {
                EnvironmentState::Browser(_) if !browser_primitive_enabled => None,
                _ => Some(schedule_agentic_decision_job(
                    ctx,
                    &observed_state,
                    &history,
                    executors,
                    native_adapter,
                    Arc::clone(&decision_telemetry_slot),
                    decision_side_call_telemetry,
                    dod_signal,
                    definition_of_done,
                    &pending_operator_steer,
                    carried_screenshot,
                )?),
            };
            let decision_outcome = if let Some(mut job) = decision_job.take() {
                if let Some(token) = cancellation_token.as_ref() {
                    if token.is_cancelled() {
                        if ctx.app_disclosure_guard.is_some() {
                            // A guarded physical attempt owns a
                            // move-only durable resource reservation.
                            // The provider/router timeout is already
                            // the canonical bound; retain the receiver
                            // until it settles, then discard its result
                            // so terminal cleanup cannot race a
                            // detached paid attempt.
                            let _ = flatten_scheduled_agentic_decision(job.await);
                        } else {
                            drop(job);
                        }
                        Err(anyhow!("execution cancelled"))
                    } else {
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => {
                                if ctx.app_disclosure_guard.is_some() {
                                    let _ = flatten_scheduled_agentic_decision((&mut job).await);
                                }
                                Err(anyhow!("execution cancelled"))
                            },
                            result = &mut job => flatten_scheduled_agentic_decision(result),
                        }
                    }
                } else {
                    flatten_scheduled_agentic_decision(job.await)
                }
            } else {
                Err(anyhow!(
                    "browser automation requires the `browser` capability pack to declare \
                     `implementation.type: primitive`; legacy outer browser SoM/text \
                     decision paths are disabled"
                ))
            };
            // A definition-of-done turn that failed transiently (or to a provider
            // outage, which has its own larger budget) goes back to the retry
            // policy and is not spent; once the policy's budget is
            // gone (Resolve would abort), the run concludes from its
            // micro-goals instead of failing with the work done.
            let decision_outcome = match (dod_goals_for_fallback.as_deref(), decision_outcome) {
                (Some(goals), Err(error))
                    if crate::magician_v2::execution::agentic::decision::provider_retry_budget_spent(
                        &error,
                        crate::magician_v2::execution::agentic::decision::provider_retries_used(
                            &error,
                            loop_protective,
                        ),
                    ) && !crate::magician_v2::spend_envelope::is_exhaustion_error(&error) =>
                {
                    Ok(
                        crate::magician_v2::execution::agentic::decision::definition_of_done_terminal(
                            ctx,
                            Err(error),
                            goals,
                        ),
                    )
                },
                (Some(_), Err(error)) => {
                    crate::magician_v2::execution::agentic::decision::release_definition_of_done_turn(
                        loop_protective,
                        dod_previous_fingerprint,
                    );
                    Err(error)
                },
                (_, outcome) => outcome,
            };
            let decision_result = decision_outcome.map(|(decision, metadata)| {
                loop_protective.last_request_hover_discovery = metadata.request_hover_discovery;
                loop_protective.pending_step_completed = metadata.step_completed;
                loop_protective.pending_step_failed = metadata.step_failed;
                loop_protective.pending_step_skipped = metadata.step_skipped;
                loop_protective.pending_step_started = metadata.step_started;
                loop_protective.pending_step_note = metadata.step_note;
                // The model submitted a plan: its later steps run without
                // asking a model again (`decision_rail::maybe_decide`).
                if metadata.plan_continuation.is_some() {
                    loop_protective.decision_rail_plan = metadata.plan_continuation;
                }
                task_state_action_for_decision = metadata.task_state_action;
                assistant_turn_for_decision = metadata.assistant_turn;
                response_id_for_decision = metadata.response_id;
                prompt_projection_mode_for_decision =
                    metadata.prompt_projection_mode.map(str::to_string);
                has_images_for_decision = metadata.has_images;
                // A definition-of-done partial is the runtime's stop, like the
                // stuck yield: the owner's allowlist does not refuse it.
                decision_is_synthetic_stuck = assistant_turn_for_decision
                    .as_ref()
                    .map(|turn| turn.operation == "stuck_auto_yield")
                    .unwrap_or(false)
                    || (definition_of_done_turn && matches!(decision, Decision::Yield { .. }));
                decision
            });
            let decision_result = match decision_result {
                Ok(
                    Decision::SpawnSubGoal { .. }
                    | Decision::HandoverToAgent { .. }
                    | Decision::DelegateToAgent { .. },
                ) if ctx.app_disclosure_guard.is_some() => Err(anyhow!(
                    "protected app workflows cannot dispatch generic control decisions"
                )),
                other => other,
            };

            let decision_latency_ms = decision_start.elapsed().as_millis() as u64;
            let decision_telemetry = decision_telemetry_slot.lock().ok().and_then(|g| g.clone());
            llm_trace_context_for_decision = decision_telemetry
                .as_ref()
                .and_then(|telemetry| telemetry.trace_receipt.as_ref())
                .map(|receipt| receipt.context.clone());
            // P3 (#17): accumulate this decision call's USD cost into the per-run
            // total so the cost ceiling at the next iteration top can settle a
            // runaway run as a budget stop. Finite-guarded so a NaN/inf never
            // poisons the accumulator.
            if let Some(cost) = decision_telemetry.as_ref().map(|t| t.cost_usd) {
                if cost.is_finite() && cost > 0.0 {
                    loop_protective.cumulative_run_cost_usd += cost;
                }
            }
            // Record whether this decision came from the Yutori provider so the
            // browser dispatcher enables the Yutori N1.5 action-translation shim
            // only for Yutori-driven runs (it emits coordinate-based
            // left_click/drag/scroll vocab; every other provider uses native
            // agent-browser commands directly).
            executors.controls.set_provider_is_yutori(
                decision_telemetry
                    .as_ref()
                    .is_some_and(|t| t.provider.eq_ignore_ascii_case("yutori")),
            );

            match decision_result {
                Ok(d) => {
                    // Emit LLMResponseReceived for successful decision
                    if ctx.has_observability() {
                        let (principal, workspace) = transport_scope(ctx);
                        let response_kind = response_kind_from_decision(&d);
                        let summary = if ctx.app_disclosure_guard.is_some() {
                            // App prompts, tool results and model output are protected
                            // continuation content. Generic observability receives only
                            // the typed decision class; free text stays behind the app
                            // disclosure/retention boundary.
                            format!("App workflow decision: {response_kind}")
                        } else {
                            match &d {
                                Decision::HarnessCheckpoint { note, .. } => note.clone(),
                                Decision::Completed { .. } => "Goal reached".to_string(),
                                Decision::Failed { reason } => {
                                    format!("Cannot proceed: {}", reason)
                                },
                                Decision::Execute { candidates, .. } => {
                                    let count = candidates.candidates.len();
                                    if count == 1 {
                                        format!(
                                            "Execute: {}",
                                            action_signature(&candidates.candidates[0].action)
                                        )
                                    } else {
                                        format!("Execute: {} candidates", count)
                                    }
                                },
                                Decision::NeedUserInput { question, .. } => {
                                    format!("Need user input: {}", question)
                                },
                                Decision::SpawnSubGoal { goal, .. } => {
                                    format!("Spawn sub-goal: {}", goal)
                                },
                                Decision::HandoverToAgent {
                                    target_agent_id,
                                    context,
                                    ..
                                } => {
                                    format!("Handover to {}: {}", target_agent_id, context)
                                },
                                Decision::DelegateToAgent { targets } => {
                                    format!("Delegate to {} child agent(s)", targets.len())
                                },
                                Decision::Yield { payload } => {
                                    format!("Yield: {}", payload.summary)
                                },
                            }
                        };
                        let tel = decision_telemetry.clone().unwrap_or_default();
                        let correlation = agentic_llm_event_correlation(
                            &tel,
                            ctx,
                            principal.as_deref(),
                            workspace.as_deref(),
                            iteration,
                            prompt_projection_mode_for_decision.as_deref(),
                        );
                        if let Some(correlation) = correlation {
                            let response_event = RuntimeTransportEvent::LLMResponseReceived {
                                execution_id: runtime_execution_id(ctx),
                                principal,
                                workspace,
                                correlation: Some(correlation),
                                plan_id: ctx.plan_id.clone().unwrap_or_default(),
                                step_id: ctx.step_id.clone(),
                                step_index: None,
                                capability: "decision".to_string(),
                                success: true,
                                decision_summary: summary,
                                cost: tel.cost_usd,
                                latency_ms: decision_latency_ms,
                                error: None,
                                provider: tel.provider,
                                model: tel.model,
                                usage_reported: tel.usage_reported,
                                input_tokens: tel.input_tokens,
                                output_tokens: tel.output_tokens,
                                reasoning_tokens: tel.reasoning_tokens,
                                reasoning_summary: ctx
                                    .app_disclosure_guard
                                    .is_none()
                                    .then(|| tel.reasoning_summary.clone())
                                    .flatten(),
                                cache_read_tokens: tel.cache_read_tokens,
                                cache_creation_tokens: tel.cache_creation_tokens,
                                // Agentic-decision calls are non-streaming today;
                                // first-byte ≈ last-byte so TTFT is unmeasured.
                                // Wired as None so the schema is consistent and
                                // streaming-instrumented call sites (chat-inline,
                                // future agent streaming paths) can populate it.
                                audio_input_tokens: None,
                                audio_output_tokens: None,
                                audio_cached_tokens: None,
                                search_calls: 0,
                                ttft_ms: None,
                                task_id: ctx.task_id.clone(),
                                agent_id: ctx.agent_id.clone(),
                                delegated_agent_id: None,
                                chat_session_id: ctx.chat_session_id.clone(),
                                operation: tel
                                    .operation
                                    .clone()
                                    .unwrap_or_else(|| "agentic_decision".to_string()),
                                profile: tel.profile.clone(),
                                attempt: 1,
                                response_kind: response_kind.to_string(),
                                started_at_ms: tel.started_at_ms,
                                timestamp: Utc::now().timestamp_millis(),
                            };
                            super::outbox::journal_and_emit(
                                ctx,
                                executors,
                                iteration,
                                Phase::Decide,
                                response_event,
                            );
                        }
                    }

                    // Emit decision made event with full observability data
                    if ctx.has_observability() {
                        let (principal, workspace) = transport_scope(ctx);
                        emit_agent_execution_mapping(
                            ctx,
                            executors,
                            Some((iteration, Phase::Decide)),
                        );
                        // Extract decision info including all enriched fields
                        let (
                            decision_type,
                            action_summary,
                            reasoning,
                            ev_evidence,
                            ev_thinking,
                            ev_tool_name,
                            ev_action_type,
                            ev_element_id,
                            ev_candidates_count,
                            ev_confidence,
                            ev_raw_decision,
                        ) = match &d {
                            Decision::HarnessCheckpoint { note, .. } => (
                                "harness_checkpoint".to_string(),
                                None,
                                note.clone(),
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                                1.0,
                                Some(serde_json::json!({ "decision": "harness_checkpoint" })),
                            ),
                            Decision::Completed {
                                evidence,
                                artifacts,
                            } => {
                                let reason = evidence
                                    .clone()
                                    .unwrap_or_else(|| "Goal has been achieved".to_string());
                                let raw = serde_json::json!({
                                    "decision": "goal_reached",
                                    "evidence": evidence,
                                    "artifacts_count": artifacts.len(),
                                });
                                (
                                    "goal_reached".to_string(),
                                    None,
                                    reason,
                                    evidence.clone(),
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    1.0,
                                    Some(raw),
                                )
                            },
                            Decision::Failed { reason } => {
                                let raw = serde_json::json!({
                                    "decision": "cannot_proceed",
                                    "reason": reason,
                                });
                                (
                                    "cannot_proceed".to_string(),
                                    None,
                                    reason.clone(),
                                    Some(reason.clone()),
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    0.0,
                                    Some(raw),
                                )
                            },
                            Decision::SpawnSubGoal {
                                goal,
                                unblocks,
                                budget_iterations,
                            } => {
                                let raw = serde_json::json!({
                                    "decision": "spawn_sub_goal",
                                    "goal": goal,
                                    "unblocks": unblocks,
                                    "budget_iterations": budget_iterations,
                                });
                                (
                                    "spawn_sub_goal".to_string(),
                                    Some(format!("Spawn sub-goal: {}", goal)),
                                    format!("Budget allocated: {}", budget_iterations),
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    1.0,
                                    Some(raw),
                                )
                            },
                            Decision::HandoverToAgent {
                                target_agent_id,
                                context,
                                preserve_live_execution_context,
                            } => {
                                let raw = serde_json::json!({
                                    "decision": "handover_to_agent",
                                    "target_agent_id": target_agent_id,
                                    "context": context,
                                    "preserve_live_execution_context": preserve_live_execution_context,
                                });
                                (
                                    "handover_to_agent".to_string(),
                                    Some(format!("Handover to {}: {}", target_agent_id, context)),
                                    context.clone(),
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    1.0,
                                    Some(raw),
                                )
                            },
                            Decision::Execute {
                                candidates,
                                thinking,
                            } => {
                                let count = candidates.candidates.len();
                                let first = candidates.candidates.first();
                                let summary = if count == 1 {
                                    action_signature(&first.unwrap().action)
                                } else {
                                    format!("{} candidates", count)
                                };
                                let (at, tn) = first
                                    .map(|c| action_type_and_tool_name(&c.action))
                                    .unwrap_or_default();
                                let conf = first.map(|c| c.confidence).unwrap_or(0.5);
                                let raw = serde_json::json!({
                                    "decision": "execute",
                                    "thinking": thinking,
                                    "candidates_count": count,
                                    "first_candidate": first.map(|c| serde_json::json!({
                                        "tool_name": tn,
                                        "action_type": at,
                                        "confidence": c.confidence,
                                        "reasoning": c.reasoning,
                                        "criticality_hint": c.criticality_hint,
                                    })),
                                });
                                (
                                    "execute".to_string(),
                                    Some(summary),
                                    thinking.clone(),
                                    None,
                                    Some(thinking.clone()),
                                    Some(tn),
                                    Some(at),
                                    None,
                                    Some(count),
                                    conf,
                                    Some(raw),
                                )
                            },
                            Decision::NeedUserInput {
                                question,
                                input_type,
                                hint,
                                options,
                            } => {
                                let raw = serde_json::json!({
                                    "decision": "need_user_input",
                                    "question": question,
                                    "input_type": format!("{:?}", input_type),
                                    "hint": hint,
                                    "options_count": options.as_ref().map(|o| o.len()),
                                });
                                (
                                    "need_user_input".to_string(),
                                    None,
                                    question.clone(),
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    1.0,
                                    Some(raw),
                                )
                            },
                            Decision::DelegateToAgent { targets } => {
                                let raw = serde_json::json!({
                                    "decision": "delegate_to_agent",
                                    "delegation_targets": targets,
                                });
                                (
                                    "delegate_to_agent".to_string(),
                                    Some(format!("Delegate to {} child agent(s)", targets.len())),
                                    delegation_targets_markdown(targets),
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    1.0,
                                    Some(raw),
                                )
                            },
                            Decision::Yield { payload } => {
                                let raw = serde_json::json!({
                                    "decision": "yield",
                                    "summary": payload.summary,
                                    "completed_count": payload.completed.len(),
                                    "open_count": payload.open.len(),
                                    "blocker_count": payload.blockers.len(),
                                    "artifact_count": payload.artifacts.len(),
                                });
                                (
                                    "yield".to_string(),
                                    Some(payload.summary.clone()),
                                    format!("Yield: {}", payload.summary),
                                    None,
                                    None,
                                    Some("yield".to_string()),
                                    Some("control_tool".to_string()),
                                    None,
                                    None,
                                    1.0,
                                    Some(raw),
                                )
                            },
                        };

                        let protected_app_decision = ctx.app_disclosure_guard.is_some();
                        // App content may include the original protected input or an
                        // exact labeled tool result. Never copy it into generic logs.
                        if protected_app_decision {
                            info!(
                                "[DECISION] type={} tool={} candidates={} confidence={:.2}",
                                decision_type,
                                ev_tool_name.as_deref().unwrap_or("-"),
                                ev_candidates_count.unwrap_or_default(),
                                ev_confidence,
                            );
                        } else {
                            info!(
                                "[DECISION] type={} action={} tool={} confidence={:.2} \
                                 thinking={}",
                                decision_type,
                                action_summary.as_deref().unwrap_or("-"),
                                ev_tool_name.as_deref().unwrap_or("-"),
                                ev_confidence,
                                ev_thinking
                                    .as_deref()
                                    .unwrap_or(&reasoning)
                                    .chars()
                                    .take(200)
                                    .collect::<String>(),
                            );
                        }

                        // Clone ev_evidence once for the persisted decision record below.
                        let ev_evidence_for_record = ev_evidence.clone();

                        let decision_made_event = RuntimeTransportEvent::AgenticDecisionMade {
                            execution_id: runtime_execution_id(ctx),
                            principal,
                            workspace,
                            plan_id: ctx.plan_id.clone().unwrap_or_default(),
                            step_id: ctx.step_id.clone().unwrap_or_default(),
                            iteration,
                            decision_type: decision_type.clone(),
                            action_summary: if protected_app_decision {
                                Some(format!("App workflow decision: {decision_type}"))
                            } else {
                                action_summary.clone()
                            },
                            reasoning: if protected_app_decision {
                                String::new()
                            } else {
                                reasoning.clone()
                            },
                            confidence: ev_confidence,
                            thinking: (!protected_app_decision).then_some(ev_thinking).flatten(),
                            evidence: (!protected_app_decision).then_some(ev_evidence).flatten(),
                            tool_name: ev_tool_name,
                            action_type: ev_action_type,
                            element_id: ev_element_id,
                            candidates_count: ev_candidates_count,
                            raw_decision: (!protected_app_decision)
                                .then_some(ev_raw_decision)
                                .flatten(),
                            timestamp: Utc::now().timestamp_millis(),
                        };
                        super::outbox::journal_and_emit(
                            ctx,
                            executors,
                            iteration,
                            Phase::Decide,
                            decision_made_event,
                        );

                        // Save LLM decision to the observation file for debugging.
                        if !protected_app_decision {
                            if let EnvironmentState::Browser(page_state) = &observed_state {
                                if let (Some(execution_id), Some(storage)) =
                                    (runtime_execution_id_opt(ctx), &executors.screenshot_storage)
                                {
                                    if let Some(obs_id) = page_state.observation_id.as_ref() {
                                        // Build decision record based on decision type
                                        let (element_id, tool_name, parameters) = match &d {
                                            Decision::Execute { candidates, .. }
                                                if !candidates.candidates.is_empty() =>
                                            {
                                                let first = &candidates.candidates[0];
                                                let tool = first.action.action_type_name();
                                                let params =
                                                    serde_json::to_value(&first.action).ok();
                                                (None, Some(tool.to_string()), params)
                                            },
                                            Decision::SpawnSubGoal {
                                                goal,
                                                unblocks,
                                                budget_iterations,
                                            } => {
                                                let params = serde_json::json!({
                                                    "goal": goal,
                                                    "unblocks": unblocks,
                                                    "budget_iterations": budget_iterations
                                                });
                                                (
                                                    None,
                                                    Some("spawn_sub_goal".to_string()),
                                                    Some(params),
                                                )
                                            },
                                            Decision::HandoverToAgent {
                                                target_agent_id,
                                                context,
                                                preserve_live_execution_context,
                                            } => {
                                                let params = serde_json::json!({
                                                    "target_agent_id": target_agent_id,
                                                    "context": context,
                                                    "preserve_live_execution_context": preserve_live_execution_context
                                                });
                                                (
                                                    None,
                                                    Some("handover_to_agent".to_string()),
                                                    Some(params),
                                                )
                                            },
                                            Decision::DelegateToAgent { targets } => {
                                                let params = serde_json::json!({
                                                    "delegation_targets": targets
                                                });
                                                (
                                                    None,
                                                    Some("delegate_to_agent".to_string()),
                                                    Some(params),
                                                )
                                            },
                                            Decision::HarnessCheckpoint { .. }
                                            | Decision::Execute { .. }
                                            | Decision::Completed { .. }
                                            | Decision::Failed { .. }
                                            | Decision::NeedUserInput { .. }
                                            | Decision::Yield { .. } => (None, None, None),
                                        };

                                        let decision_record = SoMDecisionRecord {
                                            decision_type: decision_type.clone(),
                                            thinking: Some(reasoning.clone()),
                                            evidence: ev_evidence_for_record,
                                            element_id,
                                            tool_name,
                                            parameters,
                                            raw_response: action_summary
                                                .clone()
                                                .unwrap_or_default(),
                                            validation_passed: true,
                                            validation_error: None,
                                        };

                                        if let Err(e) = storage
                                            .update_som_decision(
                                                obs_id,
                                                execution_id,
                                                decision_record,
                                            )
                                            .heap_boxed()
                                            .await
                                        {
                                            warn!(
                                                "[DECISION] Failed to save decision to \
                                                 observation file (non-fatal): {}",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }

                    Ok(d)
                },
                Err(e) => {
                    // Emit canonical response telemetry only when a real
                    // provider response/receipt reached execution-native
                    // lowering. Queue transport failures are already owned by
                    // the dispatch bridge; pre-call failures are not model
                    // calls. Emitting either as an uncorrelated response would
                    // manufacture a Phase 1 mapping gap.
                    if ctx.has_observability() {
                        let (principal, workspace) = transport_scope(ctx);
                        let validation_failure =
                    crate::magician_v2::execution::multi_llm_agent_adapter::MultiLlmAgentAdapter::validation_failure_from_error(&e);
                        let tel = decision_telemetry
                            .clone()
                            .or_else(|| {
                                validation_failure
                                    .as_ref()
                                    .and_then(|(_, telemetry)| telemetry.clone())
                            })
                            .unwrap_or_default();
                        let correlation = agentic_llm_event_correlation(
                            &tel,
                            ctx,
                            principal.as_deref(),
                            workspace.as_deref(),
                            iteration,
                            prompt_projection_mode_for_decision.as_deref(),
                        );
                        if let (Some((validation_error_class, _)), Some(correlation)) =
                            (validation_failure, correlation)
                        {
                            let validation_failure_event =
                                RuntimeTransportEvent::LLMResponseReceived {
                                    execution_id: runtime_execution_id(ctx),
                                    principal,
                                    workspace,
                                    correlation: Some(correlation),
                                    plan_id: ctx.plan_id.clone().unwrap_or_default(),
                                    step_id: ctx.step_id.clone(),
                                    step_index: None,
                                    capability: "decision".to_string(),
                                    // The provider transport succeeded; execution
                                    // rejected the returned decision contract.
                                    success: true,
                                    decision_summary: "Decision response failed contract \
                                                       validation"
                                        .to_string(),
                                    cost: tel.cost_usd,
                                    latency_ms: decision_latency_ms,
                                    // Never persist raw parse errors here: they can
                                    // include model output, user text, or tool args.
                                    // The typed response kind is the durable diagnosis.
                                    error: Some(format!(
                                        "caller contract validation failed \
                                         ({validation_error_class})"
                                    )),
                                    provider: tel.provider,
                                    model: tel.model,
                                    usage_reported: tel.usage_reported,
                                    input_tokens: tel.input_tokens,
                                    output_tokens: tel.output_tokens,
                                    reasoning_tokens: tel.reasoning_tokens,
                                    reasoning_summary: ctx
                                        .app_disclosure_guard
                                        .is_none()
                                        .then(|| tel.reasoning_summary.clone())
                                        .flatten(),
                                    cache_read_tokens: tel.cache_read_tokens,
                                    cache_creation_tokens: tel.cache_creation_tokens,
                                    // Agentic-decision calls are non-streaming today;
                                    // first-byte ≈ last-byte so TTFT is unmeasured.
                                    // Wired as None so the schema is consistent and
                                    // streaming-instrumented call sites (chat-inline,
                                    // future agent streaming paths) can populate it.
                                    audio_input_tokens: None,
                                    audio_output_tokens: None,
                                    audio_cached_tokens: None,
                                    search_calls: 0,
                                    ttft_ms: None,
                                    task_id: ctx.task_id.clone(),
                                    agent_id: ctx.agent_id.clone(),
                                    delegated_agent_id: None,
                                    chat_session_id: ctx.chat_session_id.clone(),
                                    operation: tel
                                        .operation
                                        .clone()
                                        .unwrap_or_else(|| "agentic_decision".to_string()),
                                    profile: tel.profile.clone(),
                                    attempt: 1,
                                    response_kind: format!(
                                        "validation_error:{validation_error_class}"
                                    ),
                                    started_at_ms: tel.started_at_ms,
                                    timestamp: Utc::now().timestamp_millis(),
                                };
                            super::outbox::journal_and_emit(
                                ctx,
                                executors,
                                iteration,
                                Phase::Decide,
                                validation_failure_event,
                            );
                        }
                    }
                    Err(e).with_context(|| format!("Decision failed at iteration {iteration}"))
                },
            }
        };
        decision_branch.heap_boxed().await
    };
    // All engines' actions go through the Decision Engine: the model's work
    // calls are validated before they run. A rejected call drops any plan the
    // model just submitted, so no later step of it runs either.
    let decision_result = match (proposal_check, decision_result) {
        (Some(check), Ok(decision)) => {
            let checked = crate::off_frame!(
                check
                    .validate(ctx, decision, loop_protective, cancellation_token)
                    .await
            );
            if checked.is_err() {
                loop_protective.decision_rail_plan = None;
            }
            checked
        },
        (_, result) => result,
    };

    // The run's budget is published by `run` after this returns, on every
    // exit path.
    Ok(DecideOutput {
        decision_result,
        task_state_action_for_decision,
        assistant_turn_for_decision,
        response_id_for_decision,
        has_images_for_decision,
        llm_trace_context_for_decision,
        decision_is_synthetic_stuck,
    })
}

#[cfg(test)]
mod working_set_routing_tests {
    use super::*;

    /// The notice carries the marker it is deduplicated on, the reason the
    /// router gave, and the two instructions the first live A/B showed the
    /// model needs: keep opening new pages, and search before concluding a
    /// page lacks a fact.
    #[test]
    fn the_routing_notice_says_what_the_researcher_must_keep_doing_and_stop_doing() {
        let index = crate::magician_v2::artifact_v2::WorkingSetExecutionIndex {
            schema_version: 1,
            execution_id: "exec-1".to_string(),
            scope: crate::magician_v2::artifact_v2::WorkingSetScope {
                principal: "owner".to_string(),
                workspace: "default".to_string(),
            },
            members: vec![],
            total_source_bytes: 18_813,
            distinct_sources: 3,
            read_rounds: 3,
            activation: None,
            source_hashes: vec![],
            narrowed_reads: 1,
            beyond_window_bytes: 30_000,
            decision: None,
            updated_at: chrono::Utc::now(),
        };
        let notice = working_set_routing_notice(
            &index,
            "30000 bytes beyond the window meet the 24576-byte activation threshold",
        );
        assert!(notice.starts_with(WORKING_SET_ROUTING_NOTICE_MARKER));
        assert!(notice.contains("24576-byte activation threshold"));
        assert!(notice.contains("18 KB"));
        assert!(notice.contains("Keep opening NEW pages with `content_read`"));
        assert!(notice.contains("Never conclude from a page's head"));
        assert!(notice.contains("working_set_search"));
    }
}

#[cfg(test)]
mod spend_envelope_tests {
    use super::*;

    fn decision_calling(tool: &str) -> Result<Decision> {
        let action = ExecutableAction::Pack {
            capability_name: tool.to_string(),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Compiled {
                    provider_name: tool.to_string(),
                },
            resolved_params: Default::default(),
        };
        Ok(Decision::Execute {
            candidates: CandidateBatch::new(vec![ActionCandidate::new(1, 1.0, action)], ""),
            thinking: String::new(),
        })
    }

    /// An envelope that refuses autonomous work and admits everything else.
    fn spent(class: magicllm::LlmWorkloadClass) -> Result<()> {
        if crate::magician_v2::spend_envelope::is_gated(class) {
            Err(anyhow::Error::from(
                magicllm::LLMError::SpendEnvelopeExhausted { resets_at_ms: 0 },
            ))
        } else {
            Ok(())
        }
    }

    /// A coding decision of an autonomous run becomes the typed refusal Resolve
    /// parks on; a chat-launched run's, or any other decision, passes through.
    #[test]
    fn a_coding_decision_parks_an_autonomous_run_while_the_envelope_is_spent() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        let refusal =
            coding_decision_spend_refusal(&ctx, &decision_calling("run_coding_task"), spent)
                .expect("an autonomous coding decision is refused");
        assert!(
            crate::magician_v2::spend_envelope::is_exhaustion_error(&refusal),
            "the refusal is the typed error Resolve parks on"
        );
        assert!(
            coding_decision_spend_refusal(&ctx, &decision_calling("read_file"), spent).is_none()
        );
        assert!(
            coding_decision_spend_refusal(&ctx, &decision_calling("run_coding_task"), |_| Ok(()))
                .is_none()
        );
        ctx.chat_session_id = Some("session".into());
        assert!(
            coding_decision_spend_refusal(&ctx, &decision_calling("run_coding_task"), spent)
                .is_none(),
            "a chat-launched run is never refused"
        );
    }
}
