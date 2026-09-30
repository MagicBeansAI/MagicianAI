//! `Resolve` — turn a decision result into a decision, or stop trying.
//!
//! The fourth phase lifted out of `'iteration_body`, and the first that could
//! not move as it stood. `Prepare`, `Observe` and `Decide` hold **no** exits
//! between them — the source gate in `run_loop::outcome` pins all three at
//! zero, which is why they moved as they stood; `Observe`'s one hard failure is
//! a `return Err` that propagates through `?` and needs no value to carry it.
//! This stretch holds **eleven** — five `break 'iteration_body` and six
//! `return Ok(..)` — and neither construct crosses a function boundary. So the
//! lift had to convert control flow into a value first, which is what
//! [`PhaseStep`] is.
//!
//! ("Exit" means one of those eleven throughout this file, never an `Err`
//! propagation. An earlier draft counted `Observe`'s `return Err` as the
//! siblings' "one exit" while excluding `Err` from its own eleven — two
//! meanings in one sentence, and the reason the count looked inconsistent with
//! the gate.)
//!
//! # The distinction that had to be got right eleven times
//!
//! A `break` and a `return` sit line-adjacent here and mean opposite things. The
//! transient-provider path records a history entry, asks for a backoff and
//! **breaks** — the run continues and the model decides again. Two lines of
//! condition away, the same path **returns** `AgenticOutcome::Failed` once the
//! retry budget is spent — the run is over. Each of the eleven was classified at
//! its own site rather than from a summary table, because nothing about getting
//! it backwards produces a compile error: a run would simply end early, or fail
//! to end at all.
//!
//! # What it does, in order
//!
//! 1. the token-budget check, which pauses resumably rather than failing,
//! 2. the decision unwrap — recording the assistant turn and the provider
//!    continuation id on success, classifying the error on failure into a
//!    transient backoff or a strike against the five-failure parse abort,
//! 3. the active-work budget, checked only for a decision that starts new work,
//! 4. the owner/trust reload that closes the TOCTOU window the model call opens,
//! 5. control-decision loop pressure, and the structural-action policy gate.
//!
//! Step 4 is the one worth not simplifying: the provider's decision is not
//! authority, and a policy that changed while the model was thinking invalidates
//! the decision *before* any tool, task, subscription or owner transition runs.

use std::time::Instant;

use anyhow::{anyhow, Result};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::decide::DecideOutput;
use crate::magician_v2::execution::agentic::decision::Decision;
use crate::magician_v2::execution::agentic::executor::{
    begin_agentic_tool_lineage, browser_observation_history_action,
    build_action_confirmation_outcome, check_control_decision_loop_pressure,
    conclude_budget_exhausted, conclude_work_budget_reached, context_work_budget_elapsed_ms,
    context_work_budget_reached, control_decision_action, decision_starts_new_work,
    emit_agentic_tool_consumption, enforce_action_trust_policy, finish_agentic_tool_lineage,
    finish_cancelled_agentic_execution, lifecycle_structural_action_requiring_approval,
    persist_paused_execution_summary, push_task_control_iteration,
    refresh_owner_profile_for_decision, refresh_trust_dispatch_guard_for_decision,
    reset_decision_continuation_after_authority_change, runtime_execution_id,
    stable_confirmation_action_json, synthetic_outer_assistant_turn_from_decision,
    take_matching_approved_confirmation, transient_retry_backoff,
    validate_action_against_allowlists, ActionExecutors, AgenticToolLineageState, HeapAwaitExt,
    TrustDispatchGuard, MAX_TRANSIENT_RETRIES,
};
use crate::magician_v2::execution::agentic::run_loop::outcome::{
    BoundaryOutcome, Phase, PhaseStep,
};
use crate::magician_v2::execution::agentic::types::{
    execution_token_budget_snapshot, ActionOutcomeCategory, ActionResultRecord, AgenticContext,
    AgenticOutcome, ApprovedConfirmationAction, BudgetDimension, EnvironmentState,
    ExecutionHistory, IterationRecord, LoopProtectiveState,
};

/// What `Apply` reads from `Resolve`.
///
/// Two of these four are pass-throughs from [`DecideOutput`], and that is
/// deliberate rather than lazy: `Resolve` consumes the decide output whole —
/// it is the phase that unwraps the decision, records the assistant turn and
/// takes the trace context — so the two fields `Apply` still needs would
/// otherwise have to be threaded around it by the caller, which is exactly the
/// ambient plumbing this refactor removes.
pub struct ResolveOutput {
    /// The decision `Apply` will branch on, unwrapped and re-validated against
    /// a policy generation that may have moved while the model was thinking.
    pub decision: Decision,
    /// The state the decision was made against, handed back because `Apply`
    /// takes ownership of it: ten of its terminal outcomes move it into
    /// `last_state` rather than cloning.
    pub observed_state: EnvironmentState,
    /// Durable task-state mutation the decision asked for. Applied by the
    /// `Completed`, `Yield` and `Execute` arms of `Apply` only.
    pub task_state_action_for_decision:
        crate::magician_v2::execution::durable_task_state::TaskStateActionEnvelope,
    /// Whether this turn's decision is the orchestrator-synthesised
    /// stuck-auto-yield rather than a model-emitted give-up.
    pub decision_is_synthetic_stuck: bool,
}

/// Run the resolve phase.
///
/// `current_state` is borrowed rather than owned because this phase reads it
/// exactly once, on the active-work-budget terminal. That one site takes it by
/// value, so it clones — one `EnvironmentState` copy on a path that ends the run
/// immediately afterwards, and the identical shape `Apply`'s `Execute` arm
/// already uses at the only other call of the same helper. Owning it here
/// instead would mean handing it back out of all three `PhaseStep` variants for
/// the sake of that one move.
#[allow(clippy::too_many_arguments)]
pub(in crate::magician_v2::execution::agentic) async fn run(
    ctx: &mut AgenticContext,
    executors: &ActionExecutors,
    history: &mut ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    trust_dispatch_guard: &mut Option<TrustDispatchGuard>,
    approved_confirmation_actions: &mut Vec<ApprovedConfirmationAction>,
    pending_agentic_tool_lineages: &mut Vec<AgenticToolLineageState>,
    pending_operator_steer: &mut Vec<String>,
    observed_state: EnvironmentState,
    current_state: &EnvironmentState,
    decided: DecideOutput,
    cancellation_token: &Option<CancellationToken>,
    execution_start: &Instant,
    iteration: usize,
) -> Result<PhaseStep<ResolveOutput>> {
    let DecideOutput {
        decision_result,
        task_state_action_for_decision,
        mut assistant_turn_for_decision,
        mut response_id_for_decision,
        has_images_for_decision,
        mut llm_trace_context_for_decision,
        decision_is_synthetic_stuck,
    } = decided;

    if let Some((used, limit)) = execution_token_budget_snapshot() {
        if used > limit || (used >= limit && decision_result.is_err()) {
            // P1.2: resumable budget pause (see the iteration-top site).
            return PhaseStep::ends_run(
                conclude_budget_exhausted(
                    ctx,
                    executors,
                    Some((iteration, Phase::Resolve)),
                    &history,
                    observed_state,
                    BudgetDimension::Tokens { used, limit },
                    iteration,
                    &execution_start,
                    &loop_protective,
                    &approved_confirmation_actions,
                )
                .heap_boxed()
                .await,
            );
        }
    }
    let decision = match decision_result {
        Ok(d) => {
            loop_protective.consecutive_parse_failures = 0;
            // A clean decision means the provider recovered — reset the
            // transient-retry budget (P0.3) so an earlier hiccup doesn't
            // linger and prematurely abort a later, unrelated one.
            loop_protective.transient_retry_count = 0;
            // P2.1: the buffered steer has now been folded into this
            // decision's prompt exactly once — clear it so it isn't
            // re-applied on subsequent turns. A FAILED decision keeps the
            // buffer (the retry re-applies the steer).
            pending_operator_steer.clear();
            // A step the loop synthesized without a model turn — an act→observe
            // snapshot, a structured-decision gate — carries a trace context
            // whose call id the decide phase minted (`synthetic:…`). It is not
            // a provider turn: the chain id and image shape stay as the last
            // model turn left them, and `decide_next_action` folds the step's
            // result into the next continuation suffix as text. Resetting the
            // chain here made every following decide a full re-send: run 6
            // went from 74% to 7% prompt-cache hits and doubled its billed
            // tokens while halving its model calls.
            let loop_synthesized = llm_trace_context_for_decision
                .as_ref()
                .is_some_and(|context| context.llm_call_id.starts_with("synthetic:"));
            let mut assistant_turn = assistant_turn_for_decision
                .take()
                .unwrap_or_else(|| {
                    synthetic_outer_assistant_turn_from_decision("agentic_decision_synthetic", &d)
                })
                .with_iteration(iteration);
            assistant_turn.llm_trace_context = llm_trace_context_for_decision.take();
            if let Some(consuming_call_id) = assistant_turn
                .llm_trace_context
                .as_ref()
                .map(|context| context.llm_call_id.as_str())
            {
                emit_agentic_tool_consumption(
                    ctx,
                    executors,
                    Some((iteration, Phase::Resolve)),
                    pending_agentic_tool_lineages,
                    consuming_call_id,
                );
            }
            history.record_assistant_turn(assistant_turn);
            // Update the opaque provider continuation id for the next
            // outer turn. Treat the response metadata as authoritative:
            // `Some` from a stateful adapter extends its chain; `None`
            // (including cache/replay-only providers) resets it so a
            // later eligible turn starts from a clean bootstrap. A
            // loop-synthesized step is not a turn of the chain at all.
            // Keep a stateless-provider turn's prompt text in the conversation
            // before the chain id is taken (a chain keeps it server-side).
            crate::magician_v2::execution::agentic::decision::keep_local_turn(
                ctx,
                history,
                !loop_synthesized,
                response_id_for_decision.as_deref(),
            );
            if !loop_synthesized {
                history.last_response_id = response_id_for_decision.take();
                history.last_has_images = Some(has_images_for_decision);
            }
            d
        },
        Err(e) => {
            if cancellation_token
                .as_ref()
                .map(CancellationToken::is_cancelled)
                .unwrap_or(false)
            {
                return PhaseStep::ends_run(
                    finish_cancelled_agentic_execution(
                        ctx,
                        executors,
                        Some((iteration, Phase::Resolve)),
                        observed_state.clone(),
                        &history,
                        &execution_start,
                        // `iteration`, NOT `iteration.saturating_sub(1)`, and the
                        // difference is a resume that dies rather than an off-by-one
                        // in a summary string.
                        //
                        // When a manual pause is in flight this argument is the
                        // `iterations_used` `finish_cancelled_agentic_execution`
                        // hands to `build_full_pause_state`, which writes it as
                        // `AgenticPauseState::iteration`. `restore_context_from_pause`
                        // then sets the resumed context's `iteration_offset` to
                        // `pause_state.iteration_offset + pause_state.iteration`, and
                        // that sum is the `-r{n}` component of `loop_state_address` —
                        // the whole reason a resumed invocation gets a loop-state key
                        // of its own instead of re-entering the paused invocation's.
                        // See `loop_state_execution_id`'s *THREE THINGS NEST INSIDE
                        // ONE EXECUTION ID*.
                        //
                        // Subtracting one makes that sum non-increasing at the only
                        // iteration where the subtraction can reach zero. A pause
                        // raised here on iteration 1 recorded `iteration = 0`, so the
                        // resume restored the SAME offset, minted a byte-identical
                        // `ExecutionKey`, and claimed the committed cursor this
                        // invocation had already left mid-iteration at
                        // `(1, Resolve)` — with `Prepare`/`Observe`/`Decide` durable
                        // and their `IterationCarry` gone with the process. The
                        // resumed `run_phase` then fails in
                        // `carry_missing(Resolve, "decided", Decide)`, and because
                        // `commit_failed_attempt` durably increments
                        // `phase_attempts`, each retry walks the run toward
                        // `Quarantined(PhaseAttemptsExhausted)`. That is the exact
                        // failure the resume generation was introduced to end.
                        //
                        // The loop is `for iteration in 1..=ctx.max_iterations`, so
                        // `iteration >= 1` always and the sum strictly increases. It
                        // also puts this site back in step with the three sibling
                        // cancellation arms in `phases::apply`, which pass
                        // `iteration, iteration` — the address is the reason they do.
                        //
                        // The two remaining `iteration.saturating_sub(1)` sites
                        // (`executor.rs`, the top-of-loop cancel and the wall-clock
                        // deadline) are the same class and are NOT collisions on
                        // their own: they run before `advance_iteration`, so the
                        // invocation they end committed nothing under its key and the
                        // next one seeds fresh.
                        iteration,
                        iteration,
                        executors.manual_pause_requested(),
                        &loop_protective,
                        &approved_confirmation_actions,
                    )
                    .heap_boxed()
                    .await,
                );
            }
            // P0.3: classify the decision error. A transient provider
            // hiccup (rate-limit/429/503/timeout/connection reset) is NOT
            // the model being stuck, so it must NOT feed the 5-strike
            // `consecutive_parse_failures` parse-abort. Back it off (with
            // jitter) on the dedicated `transient_retry_count` budget and ask
            // the driver to wait before the next turn so we don't hammer a
            // failing provider. Only genuine ParseFailure / Permanent errors
            // count toward the parse-abort.
            let error_kind =
                crate::magician_v2::execution::agentic::decision::classify_decision_error(&e);
            // Reset any stateful provider chain on a failed decision.
            // A decision that failed to parse/lower (e.g. an unparseable
            // `yield`) came from a provider response that holds a
            // function_call with NO matching function_call_output. If we
            // keep chaining that response, a provider may reject every
            // subsequent turn because the stored tool protocol is
            // incomplete. Dropping the chain makes the next turn a clean,
            // self-contained bootstrap so the model can re-decide.
            history.last_response_id = None;
            crate::magician_v2::execution::agentic::decision::keep_local_turn(
                ctx, history, false, None,
            );

            if matches!(
                error_kind,
                crate::magician_v2::execution::agentic::decision::DecisionErrorKind::Transient
            ) {
                loop_protective.transient_retry_count += 1;
                if loop_protective.transient_retry_count > MAX_TRANSIENT_RETRIES {
                    warn!(
                        "[DECISION-TRANSIENT-ERROR] {} transient decision errors — provider not recovering, aborting execution. Last error: {}",
                        loop_protective.transient_retry_count, e
                    );
                    return PhaseStep::ends_run(AgenticOutcome::Failed {
                        reason: format!(
                            "Aborted: {} consecutive transient decision errors (provider not recovering). Last error: {}",
                            loop_protective.transient_retry_count, e
                        ),
                        last_state: observed_state,
                        iterations_used: iteration,
                    });
                }
                let backoff = transient_retry_backoff(loop_protective.transient_retry_count);
                warn!(
                    "[DECISION-TRANSIENT-ERROR] iteration {}: {} (transient attempt {}/{}) — backing off {:?} then retrying",
                    iteration, e, loop_protective.transient_retry_count, MAX_TRANSIENT_RETRIES, backoff
                );
                history.iterations.push(IterationRecord {
                    iteration,
                    timestamp: chrono::Utc::now(),
                    state_before: EnvironmentState::Uninitialized,
                    action: browser_observation_history_action(),
                    result: ActionResultRecord {
                        success: false,
                        output: Some(format!("DECISION TRANSIENT ERROR: {}", e)),
                        error: Some(e.to_string()),
                        duration_ms: 0,
                        outcome_category: Some(ActionOutcomeCategory::Failed),
                        api_replay_used: None,
                        api_replay_time_ms: None,
                        browser_fallback_reason: None,
                        tool_result_projection: None,
                    },
                    state_after: EnvironmentState::Uninitialized,
                    verification: None,
                    llm_reasoning: None,
                });
                // The wait is requested, not taken. An in-place sleep
                // holds whoever owns the executor for the whole backoff,
                // which is wrong for a worker and wrong for a foreign
                // harness session; the driver takes it at the boundary.
                // EXIT: Retry — after the requested backoff; transient decision failure
                return PhaseStep::exits(BoundaryOutcome::Retry(backoff));
            }

            // ParseFailure / Permanent — feed the 5-strike parse-abort.
            loop_protective.consecutive_parse_failures += 1;
            // `{:#}` keeps the cause chain; the outer context alone hides
            // why the decision never reached a provider.
            warn!(
                "[DECISION-PARSE-ERROR] iteration {}: {:#} (consecutive: {}) — reset \
                 response chain",
                iteration, e, loop_protective.consecutive_parse_failures
            );

            // Record the failure in history so it's visible in the taskplan.
            history.iterations.push(IterationRecord {
                iteration,
                timestamp: chrono::Utc::now(),
                state_before: EnvironmentState::Uninitialized,
                action: browser_observation_history_action(),
                result: ActionResultRecord {
                    success: false,
                    output: Some(format!("DECISION PARSE FAILED: {e:#}")),
                    error: Some(format!("{e:#}")),
                    duration_ms: 0,
                    outcome_category: Some(ActionOutcomeCategory::Failed),
                    api_replay_used: None,
                    api_replay_time_ms: None,
                    browser_fallback_reason: None,
                    tool_result_projection: None,
                },
                state_after: EnvironmentState::Uninitialized,
                verification: None,
                llm_reasoning: None,
            });

            // Bail after 5 consecutive parse failures — the LLM is stuck.
            if loop_protective.consecutive_parse_failures >= 5 {
                warn!(
                    "[DECISION-PARSE-ERROR] {} consecutive parse failures — aborting \
                     execution",
                    loop_protective.consecutive_parse_failures
                );
                return PhaseStep::ends_run(AgenticOutcome::Failed {
                    reason: format!(
                        "Aborted: {} consecutive decision parse failures. Last error: {e:#}",
                        loop_protective.consecutive_parse_failures
                    ),
                    last_state: observed_state,
                    iterations_used: iteration,
                });
            }
            // EXIT: Advance — decision parse failure, below the abort limit
            return PhaseStep::exits(BoundaryOutcome::Advance);
        },
    };

    if decision_starts_new_work(&decision) && context_work_budget_reached(ctx) {
        let elapsed_ms = context_work_budget_elapsed_ms(ctx);
        tracing::info!(
            target: "agentic.work_budget",
            execution_id = %runtime_execution_id(ctx),
            iteration,
            limit_secs = ctx.work_budget_secs.unwrap_or_default(),
            elapsed_ms,
            "[AGENTIC-WORK-BUDGET] in-flight decision finished after boundary; proposed work was not started"
        );
        return PhaseStep::ends_run(
            conclude_work_budget_reached(
                ctx,
                executors,
                Some((iteration, Phase::Resolve)),
                &history,
                current_state.clone(),
                // `saturating_sub(1)` is correct HERE and wrong at the cancellation
                // arm above, and the difference is the terminal, not the arithmetic.
                // `conclude_work_budget_reached` returns `AgenticOutcome::Success`
                // and builds no pause record, so this number is a count in a summary
                // and never becomes a resume generation. Nothing can resume this
                // run, so nothing can collide with its loop-state key.
                iteration.saturating_sub(1),
            )
            .heap_boxed()
            .await,
        );
    }

    // Decide is the operation that accrues provider cost. Under the stateless
    // driver Resolve is a separately claimed phase, so the outer iteration-top
    // guard cannot observe a newly-crossed ceiling before this decision's
    // action would dispatch. Preserve terminal model answers, but refuse new
    // work through the same resumable budget reducer used at the outer boundary.
    if decision_starts_new_work(&decision) {
        if let Some(limit_dollars) = crate::config::agentic_max_cost_usd() {
            let used_dollars = loop_protective.cumulative_run_cost_usd;
            if used_dollars >= limit_dollars {
                return PhaseStep::ends_run(
                    conclude_budget_exhausted(
                        ctx,
                        executors,
                        Some((iteration, Phase::Resolve)),
                        &history,
                        current_state.clone(),
                        BudgetDimension::Cost {
                            used_dollars,
                            limit_dollars,
                        },
                        iteration,
                        execution_start,
                        loop_protective,
                        approved_confirmation_actions,
                    )
                    .heap_boxed()
                    .await,
                );
            }
        }
    }

    // Reset the consecutive-rejection counter only when this turn is
    // NEITHER terminal-success arm. The live model terminal is
    // `Decision::Yield`, so excluding it (alongside `Completed`) is what
    // lets the >= 3 abort actually trip for a repeatedly-rejected yield;
    // any real intervening action (Execute, etc.) still resets it.
    if !matches!(
        &decision,
        Decision::Completed { .. } | Decision::Yield { .. }
    ) {
        loop_protective.consecutive_goal_reached_rejections = 0;
        loop_protective.consecutive_giveup_rejections = 0;
    }

    // The provider decision is not authority. Close the definition and
    // trust-policy TOCTOU window by reloading both after the model call
    // and before any tool, task, subscription, or owner-transition side
    // effect. A changed policy invalidates this decision and causes the
    // next iteration to rebuild a fresh catalog.
    let prior_trust_digest = trust_dispatch_guard
        .as_ref()
        .map(|guard| serde_json::to_vec(guard.enforcer.policies()))
        .transpose()
        .map_err(|error| anyhow!("trust_policy_digest_failed:{error}"))?;
    let owner_policy_changed = refresh_owner_profile_for_decision(ctx, executors)
        .heap_boxed()
        .await?;
    let refreshed_trust_guard = refresh_trust_dispatch_guard_for_decision(ctx, executors)?;
    let refreshed_trust_digest = refreshed_trust_guard
        .as_ref()
        .map(|guard| serde_json::to_vec(guard.enforcer.policies()))
        .transpose()
        .map_err(|error| anyhow!("trust_policy_digest_failed:{error}"))?;
    let trust_policy_changed = prior_trust_digest != refreshed_trust_digest;
    *trust_dispatch_guard = refreshed_trust_guard;
    if owner_policy_changed || trust_policy_changed {
        reset_decision_continuation_after_authority_change(ctx, history);
        let changed_component = match (owner_policy_changed, trust_policy_changed) {
            (true, true) => "owner/capability and trust policies",
            (true, false) => "owner/capability policy",
            (false, true) => "trust policy",
            (false, false) => unreachable!("policy guard entered without a changed policy"),
        };
        let reason = format!(
            "Effective {changed_component} changed while the provider was deciding; the \
             stale decision was discarded before side effects."
        );
        warn!(
            owner_policy_changed,
            trust_policy_changed,
            iteration,
            "Discarding stale provider decision after an authority refresh"
        );
        push_task_control_iteration(
            history,
            &observed_state,
            iteration,
            "# policy-generation-changed".to_string(),
            false,
            reason.clone(),
            Some("PolicyGenerationChanged".to_string()),
            reason,
        );
        // EXIT: Advance — owner/trust policy generation changed mid-iteration
        return PhaseStep::exits(BoundaryOutcome::Advance);
    }

    // Pre-dispatch loop pressure for native control decisions
    // (`SpawnSubGoal`, `HandoverToAgent`, `DelegateToAgent`).
    // `Decision::Execute` has its own inline check inside the
    // Execute arm against `ExecutableAction`; these control
    // decisions never reach that path, so without this guard
    // they accumulate
    // unbounded — e.g. the `task_5e5e02b1...` execution that
    // emitted 656 identical `list_tasks` decisions across 1h41m
    // before the iteration budget caught it.
    //
    // The helper builds an `ActionFingerprint` from the decision
    // (same `(call, args)` granularity as `ExecutableAction`),
    // records it into `loop_detector.action_history`, and on
    // detected cycle pushes a synthetic advisory `IterationRecord`
    // (via `push_task_control_iteration`) so the LLM sees the
    // pressure warning in the next decision prompt. Returns
    // `true` when pressure was applied; the caller `break`s the
    // iteration body so the LLM decides again with the warning
    // in history.
    if check_control_decision_loop_pressure(
        &decision,
        &mut loop_protective.loop_detector,
        &observed_state,
        history,
        iteration,
        &mut loop_protective.loop_recovery_context,
        ctx,
    ) {
        // EXIT: Advance — loop/no-progress recovery injected guidance
        return PhaseStep::exits(BoundaryOutcome::Advance);
    }

    // Structural controls are policy objects, not privileged escape
    // hatches. Enforce whole-tool/parameter denies and trust before
    // task rows, executions, subscriptions or owner transitions.
    if let Some(control_action) = control_decision_action(&decision) {
        let policy_denial =
            validate_action_against_allowlists(&control_action, ctx).or_else(|| {
                enforce_action_trust_policy(&control_action, trust_dispatch_guard.as_ref())
                    .err()
                    .map(|error| error.to_string())
            });
        if let Some(reason) = policy_denial {
            let history_start = history.iterations.len();
            let lineage = begin_agentic_tool_lineage(
                ctx,
                executors,
                Some((iteration, Phase::Resolve)),
                &history,
                iteration,
                0,
                &mut loop_protective.agentic_tool_repeat_counts,
            );
            push_task_control_iteration(
                history,
                &observed_state,
                iteration,
                format!(
                    "# structural-policy-denial: {}",
                    control_action.action_type_name()
                ),
                false,
                reason.clone(),
                Some("StructuralActionDenied".to_string()),
                reason,
            );
            let _ = finish_agentic_tool_lineage(
                ctx,
                executors,
                Some((iteration, Phase::Resolve)),
                &history,
                history_start,
                lineage,
                None,
                false,
                &mut loop_protective.agentic_failed_tool_fingerprints,
                &[],
            );
            // EXIT: NextIteration — a control action was handled
            return PhaseStep::exits(BoundaryOutcome::NextIteration);
        }

        if let Some(action_name) =
            lifecycle_structural_action_requiring_approval(&control_action, ctx)
        {
            let action_json =
                stable_confirmation_action_json(&control_action).map_err(|error| {
                    anyhow!(
                        "failed to serialize structural action `{action_name}` for \
                         approval: {error}"
                    )
                })?;
            if take_matching_approved_confirmation(approved_confirmation_actions, &action_json, ctx)
            {
                info!(
                    action = action_name,
                    "[EXECUTOR] Consumed one-time approval for structural lifecycle action"
                );
            } else {
                let history_start = history.iterations.len();
                let lineage = begin_agentic_tool_lineage(
                    ctx,
                    executors,
                    Some((iteration, Phase::Resolve)),
                    &history,
                    iteration,
                    0,
                    &mut loop_protective.agentic_tool_repeat_counts,
                );
                let action_summary = match action_name {
                    "need_user_input" => "Ask the user for input (approval required)".to_string(),
                    "cannot_proceed" => "Stop because execution cannot proceed (approval \
                                         required)"
                        .to_string(),
                    _ => "Finish and yield execution (approval required)".to_string(),
                };
                let reason = format!(
                    "Structural action `{action_name}` requires approval under the active \
                     policy (canonical coordinates `orchestrator.{action_name}`)."
                );
                let outcome = build_action_confirmation_outcome(
                    ctx,
                    executors,
                    Some((iteration, Phase::Resolve)),
                    iteration,
                    &observed_state,
                    &history,
                    &control_action,
                    action_json,
                    action_summary,
                    reason,
                    &loop_protective,
                    &approved_confirmation_actions,
                );
                persist_paused_execution_summary(
                    ctx,
                    executors,
                    &outcome,
                    execution_start.elapsed().as_millis() as u64,
                )
                .heap_boxed()
                .await;
                let _ = finish_agentic_tool_lineage(
                    ctx,
                    executors,
                    Some((iteration, Phase::Resolve)),
                    &history,
                    history_start,
                    lineage,
                    None,
                    false,
                    &mut loop_protective.agentic_failed_tool_fingerprints,
                    &[],
                );
                return PhaseStep::ends_run(outcome);
            }
        }
    }

    Ok(PhaseStep::Continue(ResolveOutput {
        decision,
        observed_state,
        task_state_action_for_decision,
        decision_is_synthetic_stuck,
    }))
}
