//! `Apply` — the eight-way branch that is most of an iteration.
//!
//! The fifth phase, and the loop itself: larger than the other five put
//! together. An earlier draft of the design modelled the iteration as six
//! linear stages ending `Gate → Dispatch → Settle`; reading the body end to end
//! disproved it. Those three are not peers of the other phases — **they exist
//! only inside `Decision::Execute`**, one arm of this match. All three
//! `execute_direct_path_on_scheduler_root` call sites are inside that arm, so
//! the other seven never enter them at all.
//!
//! # The phase has TWO entry points, and only `Execute` is why
//!
//! [`gate`] decides; [`dispatch`] fires. A driver calls them in that order and
//! commits an effect ledger intent in between, which is the whole reason the
//! split exists: `record_effect_intent` is contractually *before the fire*, and
//! this module deliberately cannot reach a store — its parameter list is the
//! claim about what a phase may touch. So the ordering is enforced by who holds
//! the store rather than by a rule a phase has to remember, and [`dispatch`]
//! additionally requires a receipt only a driver can mint.
//!
//! Seven of the eight arms never gate anything, so they answer
//! [`ApplyGate::Settled`] and are over. They live in [`settle_without_dispatch`]
//! and are unchanged by any of this. `Execute` is routed out before that match
//! runs, into [`gate_execute`] and then [`dispatch`].
//!
//! `Lines` counts an arm from its `Decision::` pattern through its closing
//! `},`; `Exits` counts its `PhaseStep::exits` sites, the column `outcome.rs`
//! pins at fifteen for this file. `Execute`'s row is the one the split moved:
//! its lines are now spread across two functions, so the figure is the arm as it
//! stood when the table was counted rather than a length anything can measure
//! today.
//!
//! | Arm | Lines | Exits | Resolves to |
//! |---|---:|---:|---|
//! | `Completed` | 339 | 4 | terminal |
//! | `Failed` | 179 | 1 | terminal |
//! | `Yield` | 702 | 6 | terminal — the most complex control flow in the loop |
//! | `HandoverToAgent` | 57 | 1 | owner transition, continue |
//! | `DelegateToAgent` | 66 | 1 | park on children |
//! | `SpawnSubGoal` | 65 | 1 | push a continuation frame, continue |
//! | `Execute` | 889 | 1 | the only arm that dispatches — gate and fire, split |
//! | `NeedUserInput` | 104 | 0 | pause |
//!
//! # Fifteen breaks and thirty-six returns, and why the compiler caught the
//! ones that mattered
//!
//! Fifteen `break 'iteration_body` became `PhaseStep::Exit`. Twenty-five *outer*
//! `return Ok(..)` became `PhaseStep::Return` — twenty-one that built an
//! `AgenticOutcome` inline, and four that already held a boxed one from
//! `ExecutePathControl` and hand it straight through rather than re-boxing. Two
//! outer `return Err(..)` and nine inner returns did not move.
//!
//! The word *outer* is the whole difficulty. The `Execute` arm wraps its 889
//! lines in one `Box::pin(async { … })`, and nine `return`s inside it belong to
//! **that** block, yielding `ExecutePathControl` rather than ending the run.
//! They were separated by walking the brace structure of the body — not by
//! indentation, which lies here: rustfmt gives up on a block this size and
//! leaves the async body at the same column as the arm containing it.
//!
//! Rewriting one of those nine would not have been silent. `PhaseStep` and
//! `ExecutePathControl` are different types, so the mistake is a type error at
//! the block boundary, which is why the classification could be trusted at all.
//!
//! `return Err(..)` is the case that needed thought and got no rewrite. An `Err`
//! short-circuits to the same place from either depth and carries the same
//! value, so converting one would have been a change with no meaning — and
//! converting an inner one would have broken its block. All five were left where
//! they were.
//!
//! # `PopFrame` lives here and nowhere else
//!
//! Two of the fifteen exits yield control back to a previous owner rather than
//! ending the run or taking another turn — `goal_reached` on the `Completed`
//! arm and `cannot_proceed` on the `Failed` arm, each of them the statement
//! after `yield_back_to_previous_owner`. They are the only producers of
//! [`BoundaryOutcome::PopFrame`] in the entire loop.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use chrono::Utc;
use futures_util::future::join_all;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::config::OnFailureMode;
use crate::magician_v2::agents::approval::{ApprovalGate, ApprovalResult, ExecutionPlan};
use crate::magician_v2::agents::types::AgentConstraints;
use crate::magician_v2::analytics::llm_tool_lineage::LlmToolLineageIdentity;
use crate::magician_v2::execution::actions::ExecutableAction;
use crate::magician_v2::execution::agentic::decision::Decision;
use crate::magician_v2::execution::agentic::executor::{
    action_summary_for_diagnostics, apply_outer_task_state_action, approval_step_from_candidate,
    begin_agentic_tool_lineage, bind_pause_spec_to_pending_challenge,
    build_action_confirmation_outcome, build_agentic_execution_summary,
    build_agentic_execution_summary_with_yield, build_full_pause_state, candidate_targets_iframe,
    checked_approval, commit_terminal_task_state_transition, conclude_work_budget_reached,
    context_work_budget_elapsed_ms, context_work_budget_reached, degrade_yield_to_partial,
    delegation_summary_artifact_entry, derive_alternate_capabilities, effect_id_for_candidate,
    emit_agent_execution_mapping, emit_and_persist_agentic_completion,
    emit_hitl_requested_for_agentic_pause, enforce_action_trust_policy,
    enrich_artifacts_from_declarations, escalate_to_user, execute_direct_path_on_scheduler_root,
    extract_partial_success_summary, find_matching_pending_input, finish_agentic_tool_lineage,
    finish_cancelled_agentic_execution, format_input_type_for_event, goal_for_diagnostics,
    handle_delegate_to_agent_decision, handle_handover_to_agent_decision,
    handle_repeated_failed_action_rejection, handle_spawn_sub_goal_decision,
    handle_terminal_evidence_rejection, in_turn_follow_up_may_join, in_turn_slice_failed,
    merge_prior_delegation_result_artifacts, persist_paused_execution_summary,
    persist_task_completion_result_if_root, persist_terminal_decision_artifact_records,
    premature_giveup_rejection_reason, prepare_terminal_task_state_transition,
    protected_app_terminal_result_readiness, push_loop_pressure_record,
    render_yield_partial_findings, repeated_failed_action_rejection, return_outcome,
    review_terminal_draft_against_opened_evidence, running_as_in_context_delegate,
    runtime_execution_id, sensitive_spec_for_pause, session_id_for_execution,
    should_skip_serial_follow_ups, spawn_work_ledger_write, take_matching_approved_confirmation,
    task_backed_completed_yield_artifact, terminal_grounding_draft,
    terminal_grounding_repair_decision, terminal_grounding_route, terminal_success_rejection,
    transient_retry_backoff, transport_scope, validate_action_against_allowlists,
    yield_back_to_previous_owner, yield_escalation_prompt, yield_payload_summary_for,
    ActionExecutors, AgenticToolLineageState, ExecutePathControl, HeapAwaitExt,
    LoopRecoveryContext, ProtectedAppTerminalReadiness, TerminalGroundingRepairDecision,
    TerminalGroundingRoute, TrustDispatchGuard, MAX_TRANSIENT_RETRIES,
};
use crate::magician_v2::execution::agentic::loop_detector::LoopCheckResult;
use crate::magician_v2::execution::agentic::outward_settle::{
    reconcile_ref_for_dispatch, DispatchReconcileRef,
};
use crate::magician_v2::execution::agentic::run_loop::effects::{
    ApplyIntents, BatchMode, CommittedActRef, EffectAction, EffectId, EffectOutcome, EffectPlan,
    PendingBatch, PendingEffect, RetrySafety,
};
use crate::magician_v2::execution::agentic::run_loop::outcome::{
    BoundaryOutcome, Phase, PhaseStep,
};
use crate::magician_v2::execution::agentic::types::{
    action_signature, is_parallelizable_read_only_action, ActionOutcomeCategory,
    ActionResultRecord, AgenticContext, AgenticOutcome, ApprovedConfirmationAction,
    EnvironmentState, ExecutionHistory, IterationRecord, LoopProtectiveState, UserInputType,
};
use crate::magician_v2::execution::durable_task_state::TaskStateActionEnvelope;
use crate::magician_v2::execution::merkle::PageMerkleTree;
use crate::magician_v2::execution::verified_executor::executor::criticality::CriticalityEvaluator;
use crate::magician_v2::execution::verified_executor::types::ActionCandidate;
use crate::magician_v2::execution::verified_executor::CandidateBatch;
use crate::magician_v2::RuntimeTransportEvent;

/// What [`gate`] produced: either the phase is over, or a batch is committed to
/// and nothing has fired.
///
/// # The seam, and why it is a type rather than a convention
///
/// `Apply::Execute` is *gate → (driver records intents) → dispatch → (driver
/// records outcomes)*. The middle step is the driver's, and
/// `record_effect_intent` is contractually **before the fire** — so the phase
/// has to stop between the two, and this is the value it stops on. A driver
/// holding [`Self::Gated`] has the whole committed batch in hand and nothing has
/// left the process.
///
/// [`dispatch`] then requires an
/// [`super::super::effects::ApplyIntents`], which only a driver can mint. So a
/// caller cannot get from here to a fire without going through something that
/// states its ledger posture. See that type for the exact ceiling on that claim.
pub(in crate::magician_v2::execution::agentic) enum ApplyGate {
    /// The phase decided without gating anything — seven of the eight decision
    /// arms, and every `Execute` path that ends before its first dispatch.
    Settled(PhaseStep<()>),
    /// A batch is gated. Nothing has fired, and nothing will until [`dispatch`]
    /// is handed this value back.
    Gated(Box<GatedApply>),
}

/// Whether this gate must leave a dispatch recoverable by another process.
///
/// The stateless worker requires a usable effect id for every outward dispatch
/// and for every local dispatch that is not positively retry-safe. The
/// in-process driver deliberately does not: its Rust stack is the continuation,
/// and refusing an unkeyable member there would break the rollback arm without
/// making anything more durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::magician_v2::execution::agentic) enum EffectIdentityPosture {
    Durable,
    InProcess,
}

/// One admitted dispatch, carried across the seam.
///
/// `row` is `None` for a dispatch this run could not give a loop-side identity
/// to — see [`gate_pending_effect`], where `None` is not a refusal. Such a
/// member still dispatches; it just has no ledger row, so the driver records
/// nothing for it in either direction.
pub(in crate::magician_v2::execution::agentic) struct GatedMember {
    candidate: ActionCandidate,
    effect_id: Option<String>,
    row: Option<PendingEffect>,
}

/// Everything [`dispatch`] needs that [`gate`] already computed.
///
/// # Why the continuation is carried rather than recomputed
///
/// Every field here is either the product of a **side-effecting** step — a
/// lineage record begun, a confirmation consumed out of
/// `approved_confirmation_actions`, a history index taken before anything was
/// appended — or a value the phase moved out of its own parameters. Re-running
/// the first half to rebuild them would debit an approval envelope twice and
/// mint a second lineage for one dispatch. So the split carries state forward
/// instead of being re-entrant, and `gate` is consequently **not** idempotent:
/// it is called once per `Apply`, by a driver, and its output is consumed once.
/// `pub`, unlike its siblings in this module, and the exception is the point.
///
/// Every other item here is `pub(in ..::agentic)` because it names a type an
/// outside caller could not name — `phases/mod.rs` states that rule and its
/// reason. This one is different: it crosses the **host seam**. It is the return
/// payload of `driver_worker::GatedPhase` and a parameter of
/// `WorkerHost::dispatch_apply`, both of which are `pub`, so declaring it
/// narrower earns `private_interfaces` — and the honest reading of that warning
/// is the one `phases/mod.rs` gives: the item was published at the wrong level,
/// not the type.
///
/// **Nothing is re-published by this.** Every field below is private and the one
/// accessor is `pub(in ..::agentic)`, so `AgenticToolLineageState` and the rest
/// of what it carries stay exactly as reachable as they were. It is the same
/// shape `effects::ApplyIntents` uses: a public type whose construction and
/// contents are not.
pub struct GatedApply {
    /// The primary is `members[0]`; `members[1..=parallel_n]` is the parallel
    /// slice; everything above that is the sequential tail. The same partition
    /// `PendingBatch::mode` records, held here in the form the dispatch walks.
    members: Vec<GatedMember>,
    parallel_n: usize,
    thinking: String,
    session_id: String,
    has_explicit_confirmation_for_action: bool,
    /// `history.iterations.len()` as it stood before any of this batch ran, so
    /// the dispatch half can still tell its own records from what came before.
    action_history_start: usize,
    first_tool_lineage: Option<AgenticToolLineageState>,
    observed_state: EnvironmentState,
    task_state_action_for_decision: TaskStateActionEnvelope,
    /// The batch this gate published, carried so a driver can write an intent
    /// for every member without reaching into the phase's own state.
    ///
    /// The same value `gate`'s `committed_batch` out-parameter holds — one
    /// `GatedBatch::publish` produced both — and both are kept because they are
    /// read on different paths. The out-parameter is the only one that exists
    /// when the gate returns `Err`; this one is the only one a driver has when
    /// it returns `Ok`.
    ///
    /// `None` means every admitted member was one the run could not key a row
    /// for, so there is nothing to record an intent against. It does **not**
    /// mean nothing will fire.
    batch: Option<PendingBatch>,
}

impl GatedApply {
    /// The batch a driver owes an intent commit for, before [`dispatch`].
    pub(in crate::magician_v2::execution::agentic) fn batch(&self) -> Option<&PendingBatch> {
        self.batch.as_ref()
    }

    /// A value carrying nothing but the batch, for testing a DRIVER.
    ///
    /// `driver_worker`'s tests need to hand a host something a driver will
    /// commit intents from, and every field this type carries is otherwise
    /// reachable only from inside [`gate_execute`] — which needs a live
    /// `AgenticContext` and `ActionExecutors` to reach at all. So the choice was
    /// a test constructor here or no driver-level test of the seam, and the seam
    /// is the whole feature.
    ///
    /// `members` is deliberately **empty**. A test host does not really
    /// dispatch, so the candidates would go unread — and leaving them out means
    /// this constructor cannot be mistaken for a way to build a batch that
    /// actually fires. What it builds is exactly the intent-commit input and
    /// nothing else.
    #[cfg(test)]
    pub(in crate::magician_v2::execution::agentic::run_loop) fn from_batch_for_test(
        batch: PendingBatch,
    ) -> Self {
        Self {
            members: Vec::new(),
            parallel_n: 0,
            thinking: String::new(),
            session_id: String::new(),
            has_explicit_confirmation_for_action: false,
            action_history_start: 0,
            first_tool_lineage: None,
            observed_state: EnvironmentState::Uninitialized,
            task_state_action_for_decision: TaskStateActionEnvelope::none("a test fixture"),
            batch: Some(batch),
        }
    }
}

/// What the `Execute` arm's first half decided.
///
/// Local to that arm: [`ApplyGate`] is the phase-level answer and this is the
/// arm-level one, and they are different because the arm speaks
/// [`ExecutePathControl`] while the phase speaks [`PhaseStep`]. Keeping them
/// distinct is the same discipline that made the original arm's `return`s
/// classifiable — the two types do not coerce, so a value cannot be returned to
/// the wrong depth without a type error.
enum ExecuteGate {
    Settled(ExecutePathControl),
    Ready(Box<GatedApply>),
}

/// Materialize a terminal's artifacts to the execution's outputs directory
/// and index them in the execution's persisted-artifact store.
///
/// Every terminal that ships artifacts goes through here — the synthetic
/// goal-reached arm and both live yield dispositions. Only the synthetic arm
/// used to; a live yield returned its artifacts to the runtime, which never
/// indexed them. A delegated run that ended `goal_achieved_partial` with the
/// correct answer in its `partial_findings.md` therefore finalized against an
/// execution whose artifact store held only tool results, and its synthesized
/// output said so. Empty-data artifacts (delegation preview only) are skipped
/// without error, and artifacts that already carry a `materialized_path` are
/// left where they are.
async fn materialize_and_index_terminal_artifacts(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    final_artifacts: &mut Vec<crate::magician_v2::execution::agentic::types::Artifact>,
    label: &'static str,
) {
    // Enrich artifacts with render_hints and artifact_type from
    // expected_artifact_declarations (plan step / delegation).
    enrich_artifacts_from_declarations(final_artifacts, &ctx.expected_artifact_declarations);

    // Internal contract: when the outer agent declares
    // `goal_reached` carrying artifacts with raw `data` bytes
    // (e.g., a worker producing a markdown briefing or HTML
    // dashboard), persist them to the task's outputs dir AND
    // annotate each persisted artifact with its on-disk
    // `materialized_path` so downstream consumers (delegation
    // parents, chat tool-result rendering, artifact adapters)
    // can find the file. Mirrors the inner-loop side via the
    // same shared helper. Empty-data artifacts (delegation
    // preview only) are skipped without error.
    if let (Some(principal), Some(workspace), Some(task_id)) = (
        ctx.principal.as_deref(),
        ctx.workspace.as_deref(),
        ctx.task_id.as_deref(),
    ) {
        let target_dir = if let Some(execution_id) = ctx
            .execution_id
            .as_deref()
            .or(ctx.root_execution_id.as_deref())
        {
            executors.artifact_v2_workspace.execution_outputs_dir(
                principal,
                workspace,
                task_id,
                execution_id,
            )
        } else {
            executors
                .artifact_v2_workspace
                .task_outputs_dir(principal, workspace, task_id)
        };
        let materialised =
            crate::magician_v2::execution::primitive_dispatch::outcome::persist_decision_artifacts_to_dir(
                &final_artifacts,
                &target_dir,
                label,
            )
            .heap_boxed().await;
        // Map materialised entries back onto the artifact list
        // by name so downstream consumers see the path. Built
        // from materialised's `name` + `path` fields.
        let mut path_by_name: std::collections::HashMap<String, String> =
            std::collections::HashMap::with_capacity(materialised.len());
        for entry in &materialised {
            if let (Some(name), Some(path)) = (
                entry.get("name").and_then(|v| v.as_str()),
                entry.get("path").and_then(|v| v.as_str()),
            ) {
                path_by_name.insert(name.to_string(), path.to_string());
            }
        }
        for artifact in final_artifacts.iter_mut() {
            if artifact.materialized_path.is_some() {
                continue;
            }
            if let Some(path) = path_by_name.get(&artifact.name) {
                artifact.materialized_path = Some(path.clone());
            }
        }
        persist_terminal_decision_artifact_records(ctx, executors, final_artifacts.as_slice())
            .heap_boxed()
            .await;
    }
}

/// Gate the apply phase: decide everything, and fire nothing.
///
/// The first half of the seam. See [`ApplyGate`].
///
/// # This phase never returns `Continue`, and the compiler says so
///
/// The design records a "twenty-first exit": the fall-through of the final
/// `match`, an exit with no statement, reachable when an arm neither breaks nor
/// returns. **There is no such path.** Writing this as a function made rustc
/// check the claim, and it answered `unreachable_expression` on the trailing
/// `Ok(PhaseStep::Continue(()))`: *"any code following this `match` expression
/// is unreachable, as all arms diverge."* All eight arms end the iteration or
/// end the run — including the three that route through
/// `ExecutePathControl`, whose own two variants both diverge here.
///
/// So the match is this function's tail expression, typed `!` and coerced. The
/// return type stays `PhaseStep<()>` rather than something narrower because the
/// driver's contract is per-phase, not per-implementation: a caller must handle
/// `Continue` from *a* phase, and `Resolve` produces it. The payload is `()`
/// because nothing after the match in the iteration body reads a value from it
/// — everything `Apply` computes it writes through the borrows it is handed:
/// history, the loop-protective counters, the merkle baseline, `current_state`.
///
/// `observed_state` is owned rather than borrowed because ten terminal
/// outcomes move it into `last_state`; borrowing would have added ten clones
/// of a browser DOM snapshot to the paths that end a run.
///
/// # `committed_batch` is an out-parameter, and that is not laziness
///
/// It is how this phase reports what it committed to before firing it. A host
/// reads it and puts it on `driver_worker::PhaseFailure::pending`, from which
/// `commit_failed_attempt` writes it to `LoopState::pending` at a cursor that
/// did not move — which is the one place `resolve_effects` will look for it.
/// Three other shapes were available and each loses something this one keeps:
///
/// - **On the return value.** This function's payload is `PhaseStep<()>` and
///   every arm of the eight-way match diverges, so `Continue(T)` is unreachable
///   — a batch returned there would never arrive. Widening the return to a
///   tuple would carry it out of the `Ok` path only, and the batch matters most
///   on the `Err` path.
/// - **`Err` loses it.** A dispatch that errored mid-fire is exactly the case a
///   resuming worker needs the batch for. An out parameter is written as each
///   member is gated, so whatever this phase committed to survives every one of
///   its fifteen exits, its `Return`s and its errors.
/// - **Reaching into the driver.** The phase would have to know which driver it
///   is running under. It reports; the driver records.
///
/// # The value is consumed on the failure path, and on that path only
///
/// That asymmetry belongs to the driver, but reading it here saves the next
/// person the trip. A **successful** `Apply` has already settled everything it
/// gated, and `LoopState::pending` means *in flight*: publishing a settled batch
/// there would leave `pending = Some(..)` at a cursor that has moved off
/// `Apply`, which `driver_worker::resolve_effects` refuses as
/// `Quarantine::OrphanedBatch` on the very next claim. So the hosts build
/// `PhaseReport` with `pending: None` on every `Ok` and read this parameter only
/// when the phase returned `Err`, where the cursor stays put. `commit_boundary`
/// enforces the same rule from the other side.
///
/// This phase therefore does **not** clear the parameter after a batch settles,
/// and must not: the value's one reader never sees a success.
#[allow(clippy::too_many_arguments)]
pub(in crate::magician_v2::execution::agentic) async fn gate(
    ctx: &mut AgenticContext,
    executors: &Arc<ActionExecutors>,
    history: &mut ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    // Read-only here. Every use in this phase is `.as_ref()`; `Resolve` is what
    // refreshes the guard. Taking `&mut` would widen what this signature claims
    // the phase may touch, and the module's own contract is that the parameter
    // list IS that claim — an unused `&mut` quietly weakens it.
    trust_dispatch_guard: &Option<TrustDispatchGuard>,
    approved_confirmation_actions: &mut Vec<ApprovedConfirmationAction>,
    pending_agentic_tool_lineages: &mut Vec<AgenticToolLineageState>,
    previous_merkle_tree: &mut Option<PageMerkleTree>,
    current_state: &mut EnvironmentState,
    observed_state: EnvironmentState,
    decision: Decision,
    task_state_action_for_decision: TaskStateActionEnvelope,
    decision_is_synthetic_stuck: bool,
    cancellation_token: &Option<CancellationToken>,
    execution_start: &Instant,
    // No `ephemeral_scope_id`, and its absence is the parameter list doing its
    // job. The scope is a DISPATCH input — `execute_direct_path_on_scheduler_root`
    // is its only reader — and this half dispatches nothing, so carrying it here
    // would claim a reach this function does not have. It is on `dispatch`
    // instead, where it is used.
    iteration: usize,
    // What this phase committed to before firing any of it. Written member by
    // member as the `Gate` admits each dispatch, so it is complete on every exit
    // path including the error ones.
    //
    // Still an out-parameter after the split, and for a reason that survived it:
    // this function's `Err` path is where a gate refusal lands, and an `Err`
    // cannot carry the members that were admitted before the refusal. The
    // SUCCESS path now has a second reader as well — `ApplyGate::Gated` reaches
    // a driver with the batch already published here, which is what makes the
    // intent commit possible at all.
    committed_batch: &mut Option<PendingBatch>,
    effect_identity: EffectIdentityPosture,
) -> Result<ApplyGate> {
    // `Execute` is routed out before the match below, and this is the whole of
    // the routing. It is here rather than inside the match because the two halves
    // answer different types: everything else settles to a `PhaseStep`, and this
    // one may suspend on a `GatedApply` instead.
    //
    // `committed_batch` goes no further than the `Execute` side, for the same
    // reason: it is the gated batch, `Execute` is the only arm that gates, and
    // handing it to a function that cannot gate would claim otherwise.
    if let Decision::Execute {
        candidates,
        thinking,
    } = decision
    {
        // **A call that already failed with these exact arguments cannot
        // succeed now.** Bounce it back with the reason instead of spending an
        // iteration re-proving it. The guard stands down after its own small
        // budget so it can never deadlock a model that insists — see
        // `MAX_REPEAT_FAILED_ACTION_REJECTIONS`.
        if let Some(rejection_reason) = candidates
            .primary()
            .or_else(|| candidates.candidates.first())
            .and_then(|candidate| repeated_failed_action_rejection(history, &candidate.action))
        {
            let stand_down = handle_repeated_failed_action_rejection(
                history,
                iteration,
                &rejection_reason,
                &mut loop_protective.consecutive_repeat_failed_action_rejections,
            );
            if !stand_down {
                // Nothing was dispatched, so there is no batch to hand back.
                // EXIT: Advance — settle until the model changes approach.
                return PhaseStep::exits(BoundaryOutcome::Advance).map(ApplyGate::Settled);
            }
        } else {
            loop_protective.consecutive_repeat_failed_action_rejections = 0;
        }
        return gate_execute(
            ctx,
            executors,
            history,
            loop_protective,
            trust_dispatch_guard,
            approved_confirmation_actions,
            pending_agentic_tool_lineages,
            observed_state,
            candidates,
            thinking,
            task_state_action_for_decision,
            execution_start,
            iteration,
            committed_batch,
            effect_identity,
        )
        .heap_boxed()
        .await;
    }
    settle_without_dispatch(
        ctx,
        executors,
        history,
        loop_protective,
        trust_dispatch_guard,
        approved_confirmation_actions,
        pending_agentic_tool_lineages,
        previous_merkle_tree,
        current_state,
        observed_state,
        decision,
        task_state_action_for_decision,
        decision_is_synthetic_stuck,
        cancellation_token,
        execution_start,
        iteration,
    )
    .heap_boxed()
    .await
    .map(ApplyGate::Settled)
}

/// The seven decision arms that never dispatch.
///
/// Split from [`gate`] so that `Execute` — the one arm with a gate/fire seam —
/// can answer a different type without every other arm having to be rewritten to
/// wrap its own answer. Each of these still ends the iteration or ends the run
/// exactly as it did when they all shared one function.
#[allow(clippy::too_many_arguments)]
async fn settle_without_dispatch(
    ctx: &mut AgenticContext,
    executors: &Arc<ActionExecutors>,
    history: &mut ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    // Read-only here, as in `gate`. See that signature.
    trust_dispatch_guard: &Option<TrustDispatchGuard>,
    approved_confirmation_actions: &mut Vec<ApprovedConfirmationAction>,
    pending_agentic_tool_lineages: &mut Vec<AgenticToolLineageState>,
    previous_merkle_tree: &mut Option<PageMerkleTree>,
    current_state: &mut EnvironmentState,
    observed_state: EnvironmentState,
    decision: Decision,
    task_state_action_for_decision: TaskStateActionEnvelope,
    decision_is_synthetic_stuck: bool,
    cancellation_token: &Option<CancellationToken>,
    execution_start: &Instant,
    iteration: usize,
) -> Result<PhaseStep<()>> {
    // Every non-execute decision at this seam is structural or
    // terminal. None may carry an implicit task-state side effect.
    // Execute applies task state only after its complete action batch
    // succeeds; the synthetic Completed path does so only after its
    // evidence gate. Yield, NeedUserInput, Failed, delegation, spawn,
    // and handover therefore leave task state untouched.

    match decision {
        // ═══════════════════════════════════════════════════════════
        // COMPLETED - synthetic success terminal (text-fallback path)
        // ═══════════════════════════════════════════════════════════
        Decision::Completed {
            evidence,
            artifacts,
        } => {
            // Lifted behind a pointer. Match arms are unioned, so this frame was
            // max(arms): a 68-line DelegateToAgent path paid for the 825-line Yield
            // arm it never runs. Measured 495,616 -> 143,360 for the parent frame.
            async move {
                let protected_terminal_readiness =
                    protected_app_terminal_result_readiness(ctx, executors)
                        .heap_boxed()
                        .await;
                if protected_terminal_readiness == ProtectedAppTerminalReadiness::Unavailable {
                    return Err(anyhow!(
                        "protected app terminal state is temporarily unavailable"
                    ));
                }
                if protected_terminal_readiness == ProtectedAppTerminalReadiness::Missing {
                    const REJECTION: &str =
                        "Protected app completion requires its typed terminal commit";
                    if handle_terminal_evidence_rejection(
                        history,
                        &observed_state,
                        iteration,
                        REJECTION,
                        None,
                        &mut loop_protective.consecutive_goal_reached_rejections,
                    ) {
                        return PhaseStep::ends_run(AgenticOutcome::Failed {
                            reason: "app_workflow_terminal_commit_missing".to_owned(),
                            last_state: observed_state,
                            iterations_used: iteration,
                        });
                    }
                    // EXIT: Advance — protected-app terminal commit missing, rejection recorded
                    return PhaseStep::exits(BoundaryOutcome::Advance);
                }
                let prepared_terminal_task_state = match prepare_terminal_task_state_transition(
                    ctx,
                    executors,
                    &task_state_action_for_decision,
                    iteration,
                    true,
                    evidence.as_deref().unwrap_or_default(),
                    &[],
                    &[],
                    trust_dispatch_guard.as_ref(),
                )
                .heap_boxed()
                .await
                {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let rejection_reason = format!(
                            "Proposed terminal task-state transition was rejected without \
                             mutation: {error}"
                        );
                        if handle_terminal_evidence_rejection(
                            history,
                            &observed_state,
                            iteration,
                            &rejection_reason,
                            evidence.as_deref(),
                            &mut loop_protective.consecutive_goal_reached_rejections,
                        ) {
                            return PhaseStep::ends_run(AgenticOutcome::Failed {
                                reason: rejection_reason,
                                last_state: observed_state,
                                iterations_used: iteration,
                            });
                        }
                        // EXIT: Advance — terminal evidence rejected, under the strike limit
                        return PhaseStep::exits(BoundaryOutcome::Advance);
                    },
                };
                if let Some(rejection_reason) = terminal_success_rejection(
                    ctx,
                    executors,
                    &history,
                    evidence.as_deref(),
                    &artifacts,
                    true,
                    prepared_terminal_task_state.state.as_ref(),
                )
                .heap_boxed()
                .await
                {
                    if handle_terminal_evidence_rejection(
                        history,
                        &observed_state,
                        iteration,
                        &rejection_reason,
                        evidence.as_deref(),
                        &mut loop_protective.consecutive_goal_reached_rejections,
                    ) {
                        warn!(
                            "[GOAL-REACHED-REJECTED] {} consecutive rejected terminal \
                             successes — aborting execution",
                            loop_protective.consecutive_goal_reached_rejections
                        );
                        return PhaseStep::ends_run(AgenticOutcome::Failed {
                            reason: format!(
                                "Aborted: {} consecutive goal_reached decisions were rejected \
                                 because completion evidence was incomplete. Last rejection: \
                                 {}",
                                loop_protective.consecutive_goal_reached_rejections,
                                rejection_reason
                            ),
                            last_state: observed_state,
                            iterations_used: iteration,
                        });
                    }

                    // EXIT: Advance — goal-reached rejected, under the strike limit
                    return PhaseStep::exits(BoundaryOutcome::Advance);
                }

                commit_terminal_task_state_transition(ctx, executors, prepared_terminal_task_state)
                    .heap_boxed()
                    .await
                    .map_err(|error| {
                        anyhow!(
                            "accepted terminal could not atomically persist durable task state: \
                         {error}"
                        )
                    })?;

                // The terminal app mutation tool already published the
                // typed result and receipts. Generic completion summaries,
                // artifacts and task-result projections are separate
                // hidden consumers that have no protected-content permit
                // in V1, so expose only a content-free runtime outcome.
                if ctx.app_disclosure_guard.is_some() {
                    info!(
                        iteration,
                        execution_id = %runtime_execution_id(ctx),
                        "Protected app workflow reached its terminal state"
                    );
                    return PhaseStep::ends_run(AgenticOutcome::Success {
                        completion:
                            crate::magician_v2::execution::agentic::types::CompletionKind::Full,
                        open: Vec::new(),
                        final_state: current_state.clone(),
                        iterations_used: iteration,
                        artifacts: Vec::new(),
                    });
                }

                if !ctx.owner_stack.is_empty() && !running_as_in_context_delegate(ctx) {
                    yield_back_to_previous_owner(
                        ctx,
                        &observed_state,
                        history,
                        iteration,
                        executors,
                        loop_protective,
                        previous_merkle_tree,
                        "goal_reached",
                        evidence.clone(),
                        artifacts,
                    )
                    .heap_boxed()
                    .await?;
                    // EXIT: PopFrame — yielded back to the previous owner after goal_reached
                    return PhaseStep::exits(BoundaryOutcome::PopFrame);
                }

                info!(
                    "Goal reached after {} iterations: '{}' (evidence: {:?})",
                    iteration, ctx.goal, evidence
                );

                let success_summary =
                    completed_terminal_summary(&ctx.goal, evidence.as_deref(), &artifacts);
                // Include artifact data content (truncated) alongside metadata
                let mut rich_artifacts: Vec<String> = artifacts
                    .iter()
                    .map(delegation_summary_artifact_entry)
                    .collect();

                // Append durable artifact paths written during this execution.
                // These use "durable:" prefix so the orchestrator can resolve them
                // to real filesystem paths for downstream task context injection.
                if let Ok(outputs) = executors.run_outputs.lock() {
                    for rel_path in outputs.durable_artifacts_written.iter() {
                        rich_artifacts.push(format!("durable:{}", rel_path));
                    }
                }

                // Append history artifacts synthesized from action results that
                // aren't already present from the LLM decision artifacts.
                // This bridges captured pack-action artifacts into the
                // completion_artifact_names so linked tasks and episode
                // artifact_output can resolve them.
                {
                    // Collect existing artifact names for dedup
                    let existing_names: std::collections::HashSet<&str> =
                        artifacts.iter().map(|a| a.name.as_str()).collect();

                    for hist_artifact in &history.artifacts {
                        if existing_names.contains(hist_artifact.name.as_str()) {
                            continue;
                        }
                        rich_artifacts.push(delegation_summary_artifact_entry(hist_artifact));
                    }
                }

                merge_prior_delegation_result_artifacts(ctx, executors, &mut rich_artifacts)
                    .heap_boxed()
                    .await;

                let completion_summary = build_agentic_execution_summary(
                    ctx,
                    executors,
                    "goal_achieved",
                    iteration,
                    rich_artifacts.clone(),
                    execution_start.elapsed().as_millis() as u64,
                    success_summary.clone(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                emit_and_persist_agentic_completion(
                    ctx,
                    executors,
                    Some((iteration, Phase::Apply)),
                    completion_summary,
                    Some(&history),
                )
                .heap_boxed()
                .await;

                // Persist completion result on the root task execution so the result card
                // survives browser closes without letting delegated children overwrite it.
                persist_task_completion_result_if_root(
                    executors,
                    ctx,
                    success_summary,
                    "goal_achieved".to_string(),
                    rich_artifacts,
                )
                .heap_boxed()
                .await;

                let mut final_artifacts = history.artifacts.clone();
                final_artifacts.extend(artifacts);

                materialize_and_index_terminal_artifacts(
                    ctx,
                    executors,
                    &mut final_artifacts,
                    "outer_loop_goal_reached",
                )
                .heap_boxed()
                .await;

                return PhaseStep::ends_run(AgenticOutcome::Success {
                    completion: crate::magician_v2::execution::agentic::types::CompletionKind::Full,
                    open: Vec::new(),
                    // Report the post-action state, not the pre-action observe
                    // snapshot. `current_state` is updated in place after each
                    // action (e.g. an HTTP action sets EnvironmentState::Http);
                    // `observed_state` is the iteration-start snapshot and is
                    // stale here. (0.8c regression fix.)
                    final_state: current_state.clone(),
                    iterations_used: iteration,
                    artifacts: final_artifacts,
                });
            }
            .heap_boxed()
            .await
        },

        // ═══════════════════════════════════════════════════════════
        // FAILED - synthetic failure terminal (invalid-response path)
        // ═══════════════════════════════════════════════════════════
        Decision::Failed { reason } => {
            // Lifted behind a pointer. Match arms are unioned, so this frame was
            // max(arms): a 68-line DelegateToAgent path paid for the 825-line Yield
            // arm it never runs. Measured 495,616 -> 143,360 for the parent frame.
            async move {
                if ctx.app_disclosure_guard.is_some() {
                    warn!(
                        iteration,
                        execution_id = %runtime_execution_id(ctx),
                        "Protected app workflow cannot proceed"
                    );
                    if ctx.on_failure == OnFailureMode::AskUser {
                        return PhaseStep::ends_run(escalate_to_user(
                            ctx,
                            executors,
                            Some((iteration, Phase::Apply)),
                            iteration,
                            &observed_state,
                            &history,
                            "cannot_proceed",
                            "Protected app workflow needs user guidance".to_owned(),
                            "app_workflow_needs_guidance".to_owned(),
                            &loop_protective,
                            &approved_confirmation_actions,
                        ));
                    }
                    return PhaseStep::ends_run(AgenticOutcome::CannotProceed {
                        reason: "app_workflow_cannot_proceed".to_owned(),
                        last_state: observed_state,
                        iterations_used: iteration,
                    });
                }

                if !ctx.owner_stack.is_empty() && !running_as_in_context_delegate(ctx) {
                    yield_back_to_previous_owner(
                        ctx,
                        &observed_state,
                        history,
                        iteration,
                        executors,
                        loop_protective,
                        previous_merkle_tree,
                        "cannot_proceed",
                        Some(reason.clone()),
                        Vec::new(),
                    )
                    .heap_boxed()
                    .await?;
                    // EXIT: PopFrame — yielded back to the previous owner after cannot_proceed
                    return PhaseStep::exits(BoundaryOutcome::PopFrame);
                }

                // === PARTIAL-SUCCESS SAFETY NET ===
                // If the agent produced substantive successful work before
                // emitting cannot_proceed, preserve that work as a
                // partial-success rather than discarding it as a generic
                // failure. Mirrors the goal_reached path. The agent's tool
                // descriptions and system prompt already push toward
                // partial goal_reached, this is the safety net for when
                // the model didn't reformulate the terminal decision.
                if let Some(partial_text) = extract_partial_success_summary(&history, &reason) {
                    info!(
                        "[PARTIAL-SUCCESS-SAFETY-NET] cannot_proceed converted to \
                         partial-success ({} chars of findings preserved)",
                        partial_text.len()
                    );

                    let mut partial_artifact =
                        crate::magician_v2::execution::agentic::types::Artifact::text(
                            "partial_findings.md",
                            partial_text.clone(),
                        );
                    // `Artifact::text` stamps `text/plain` by default,
                    // but the body is markdown (the `## PARTIAL
                    // SUCCESS — ...` header + indented code fences).
                    // Set the correct content type so downstream
                    // renderers (UI cards, downloads with proper
                    // browser preview) treat it as markdown.
                    partial_artifact.content_type = "text/markdown".to_string();
                    partial_artifact.artifact_type = Some("partial_findings".to_string());

                    let success_summary =
                        format!("Goal partially achieved: {}\n\n{}", ctx.goal, partial_text);

                    let completion_summary = build_agentic_execution_summary(
                        ctx,
                        executors,
                        "goal_achieved_partial",
                        iteration,
                        vec![format!("partial:{}", partial_artifact.name)],
                        execution_start.elapsed().as_millis() as u64,
                        success_summary.clone(),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(reason.clone()),
                    );
                    emit_and_persist_agentic_completion(
                        ctx,
                        executors,
                        Some((iteration, Phase::Apply)),
                        completion_summary,
                        Some(&history),
                    )
                    .heap_boxed()
                    .await;

                    persist_task_completion_result_if_root(
                        executors,
                        ctx,
                        success_summary,
                        "goal_achieved_partial".to_string(),
                        vec![],
                    )
                    .heap_boxed()
                    .await;

                    let mut final_artifacts = history.artifacts.clone();
                    final_artifacts.push(partial_artifact);
                    materialize_and_index_terminal_artifacts(
                        ctx,
                        executors,
                        &mut final_artifacts,
                        "outer_loop_cannot_proceed_partial",
                    )
                    .heap_boxed()
                    .await;

                    return PhaseStep::ends_run(AgenticOutcome::Success {
                        completion:
                            crate::magician_v2::execution::agentic::types::CompletionKind::Partial,
                        open: vec![reason.to_string()],
                        // Report the post-action state, not the pre-action observe
                        // snapshot. `current_state` is updated in place after each
                        // action (e.g. an HTTP action sets EnvironmentState::Http);
                        // `observed_state` is the iteration-start snapshot and is
                        // stale here. (0.8c regression fix.)
                        final_state: current_state.clone(),
                        iterations_used: iteration,
                        artifacts: final_artifacts,
                    });
                }

                warn!("Cannot proceed after {} iterations: {}", iteration, reason);

                // Give-up is terminal. AskUser is not an interview: permission
                // HITL is the Auth/Permission yield path, budget continue is
                // MaxIterationsReached. Generic "help me save costs" pauses
                // are gone.

                let completion_summary = build_agentic_execution_summary(
                    ctx,
                    executors,
                    "cannot_proceed",
                    iteration,
                    Vec::new(),
                    execution_start.elapsed().as_millis() as u64,
                    format!("Cannot proceed: {}", reason),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(reason.clone()),
                );
                emit_and_persist_agentic_completion(
                    ctx,
                    executors,
                    Some((iteration, Phase::Apply)),
                    completion_summary,
                    Some(&history),
                )
                .heap_boxed()
                .await;

                persist_task_completion_result_if_root(
                    executors,
                    ctx,
                    format!("Cannot proceed: {}", reason),
                    "cannot_proceed".to_string(),
                    vec![],
                )
                .heap_boxed()
                .await;

                return PhaseStep::ends_run(AgenticOutcome::CannotProceed {
                    reason,
                    last_state: observed_state,
                    iterations_used: iteration,
                });
            }
            .heap_boxed()
            .await
        },

        // ═══════════════════════════════════════════════════════════
        // YIELD - Unified terminal outcome report
        // ═══════════════════════════════════════════════════════════
        //
        // The structured `YieldDecision` payload reached the
        // executor untouched. We compute the disposition here
        // (orchestrator-side, not LLM self-grading) and map to
        // the right `AgenticOutcome` primitive. Structured
        // fields are preserved in the evidence string +
        // artifacts so the memory layer (tactical pattern T3 partial-
        // success classifier) and the event surface see the
        // same data.
        //
        // Yield is **terminal only** — done / partial / stuck.
        // For interactive asks the LLM emits `need_user_input`
        // (a separate primitive with a different lifecycle —
        // the conversation pauses and resumes rather than
        // terminating). The Yield handler therefore has no
        // `WaitingForUser` branch.
        Decision::Yield { payload } => {
            // Lifted behind a pointer. Match arms are unioned, so this frame was
            // max(arms): a 68-line DelegateToAgent path paid for the 825-line Yield
            // arm it never runs. Measured 495,616 -> 143,360 for the parent frame.
            async move {
                use crate::magician_v2::execution::agentic::yield_decision::{
                    dispose_yield, YieldDisposition,
                };

                // Both are re-derived once, below, if the grounding gate degrades
                // this terminal to a partial: the caveat joins `open` and the
                // disposition is recomputed by the same rules as every yield.
                let mut payload = payload;
                let mut disposition = dispose_yield(&payload);
                if ctx.app_disclosure_guard.is_some() {
                    if matches!(
                        disposition,
                        YieldDisposition::Completed { .. } | YieldDisposition::PartialSuccess { .. }
                    ) {
                        let terminal_readiness =
                            protected_app_terminal_result_readiness(ctx, executors)
                                .heap_boxed()
                                .await;
                        if terminal_readiness == ProtectedAppTerminalReadiness::Unavailable {
                            return Err(anyhow!(
                                "protected app terminal state is temporarily unavailable"
                            ));
                        }
                        if terminal_readiness == ProtectedAppTerminalReadiness::Ready {
                            info!(
                                iteration,
                                execution_id = %runtime_execution_id(ctx),
                                "Protected app workflow yielded a committed terminal result"
                            );
                            return PhaseStep::ends_run(AgenticOutcome::Success {
                                completion:
                                    crate::magician_v2::execution::agentic::types::CompletionKind::Full,
                                open: Vec::new(),
                                final_state: current_state.clone(),
                                iterations_used: iteration,
                                artifacts: Vec::new(),
                            });
                        }
                        const REJECTION: &str =
                            "Protected app completion requires its typed terminal commit";
                        if handle_terminal_evidence_rejection(
                            history,
                            &observed_state,
                            iteration,
                            REJECTION,
                            None,
                            &mut loop_protective.consecutive_goal_reached_rejections,
                        ) {
                            return PhaseStep::ends_run(AgenticOutcome::Failed {
                                reason: "app_workflow_terminal_commit_missing".to_owned(),
                                last_state: observed_state,
                                iterations_used: iteration,
                            });
                        }
                        // EXIT: Advance — protected-app terminal commit missing on the yield path
                        return PhaseStep::exits(BoundaryOutcome::Advance);
                    }
                    info!(
                        iteration,
                        execution_id = %runtime_execution_id(ctx),
                        disposition = match &disposition {
                            YieldDisposition::Completed { .. } => "completed",
                            YieldDisposition::PartialSuccess { .. } => "partial_success",
                            YieldDisposition::Failed { .. } => "failed",
                            YieldDisposition::RetryTransient { .. } => "retry_exhausted",
                        },
                        "Protected app workflow yielded a terminal disposition"
                    );
                    return PhaseStep::ends_run(match disposition {
                        YieldDisposition::Completed { .. }
                        | YieldDisposition::PartialSuccess { .. } => AgenticOutcome::Success {
                            completion:
                                crate::magician_v2::execution::agentic::types::CompletionKind::Full,
                            open: Vec::new(),
                            final_state: current_state.clone(),
                            iterations_used: iteration,
                            artifacts: Vec::new(),
                        },
                        YieldDisposition::Failed { .. } | YieldDisposition::RetryTransient { .. } => {
                            AgenticOutcome::Failed {
                                reason: "app_workflow_failed".to_owned(),
                                last_state: observed_state,
                                iterations_used: iteration,
                            }
                        },
                    });
                }
                let mut prepared_terminal_task_state = None;

                // ═══════════════════════════════════════════════════════
                // EVIDENCE GATE on the LIVE terminal.
                // The model's real terminal is always `Decision::Yield`
                // (yield + legacy goal_reached/cannot_proceed all lower to
                // Yield; `Decision::Completed` is synthetic-only). The
                // deterministic evidence gate previously ran ONLY on the
                // Completed arm, so the live success path was ungated — a
                // yield claiming success with no real page change (e.g. a
                // slider that never moved) was accepted verbatim. Run the
                // SAME gate here, keyed off the orchestrator-computed
                // disposition (NOT the LLM self_classification, which
                // `dispose_yield` deliberately ignores), and soft-reject +
                // retry identically to the Completed arm via the shared
                // helper. Blocked/Failed/RetryTransient are not success
                // claims and pass straight through.
                if matches!(
                    disposition,
                    YieldDisposition::Completed { .. } | YieldDisposition::PartialSuccess { .. }
                ) {
                    let evidence = terminal_grounding_draft(&payload);
                    let completed = matches!(disposition, YieldDisposition::Completed { .. });
                    let durable_blockers = payload
                        .blockers
                        .iter()
                        .map(|blocker| format!("{:?}: {}", blocker.kind, blocker.description.trim()))
                        .collect::<Vec<_>>();
                    let prepared = match prepare_terminal_task_state_transition(
                        ctx,
                        executors,
                        &task_state_action_for_decision,
                        iteration,
                        completed,
                        &payload.summary,
                        &payload.open,
                        &durable_blockers,
                        trust_dispatch_guard.as_ref(),
                    )
                    .heap_boxed()
                    .await
                    {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            let rejection_reason = format!(
                                "Proposed terminal task-state transition was rejected without \
                                 mutation: {error}"
                            );
                            if handle_terminal_evidence_rejection(
                                history,
                                &observed_state,
                                iteration,
                                &rejection_reason,
                                Some(&evidence),
                                &mut loop_protective.consecutive_goal_reached_rejections,
                            ) {
                                return PhaseStep::ends_run(AgenticOutcome::Failed {
                                    reason: rejection_reason,
                                    last_state: observed_state,
                                    iterations_used: iteration,
                                });
                            }
                            // EXIT: Advance — terminal evidence rejected on the yield path
                            return PhaseStep::exits(BoundaryOutcome::Advance);
                        },
                    };
                    if let Some(rejection_reason) = terminal_success_rejection(
                        ctx,
                        executors,
                        &history,
                        Some(&evidence),
                        &payload.artifacts,
                        completed,
                        prepared.state.as_ref(),
                    )
                    .heap_boxed()
                    .await
                    {
                        if handle_terminal_evidence_rejection(
                            history,
                            &observed_state,
                            iteration,
                            &rejection_reason,
                            Some(&evidence),
                            &mut loop_protective.consecutive_goal_reached_rejections,
                        ) {
                            warn!(
                                "[GOAL-REACHED-REJECTED] {} consecutive rejected terminal \
                                 yields — aborting execution",
                                loop_protective.consecutive_goal_reached_rejections
                            );
                            return PhaseStep::ends_run(AgenticOutcome::Failed {
                                reason: format!(
                                    "Aborted: {} consecutive yield successes were rejected \
                                     because completion evidence was incomplete. Last \
                                     rejection: {}",
                                    loop_protective.consecutive_goal_reached_rejections,
                                    rejection_reason
                                ),
                                last_state: observed_state,
                                iterations_used: iteration,
                            });
                        }
                        // EXIT: Advance — goal-reached rejected on the yield path
                        return PhaseStep::exits(BoundaryOutcome::Advance);
                    }
                    prepared_terminal_task_state = Some(prepared);
                }

                // Grounded-draft correction gate. Compare the proposed terminal
                // text to every successful, claim-eligible `opened_page` result
                // in the run — admitted by the shape of the evidence envelope,
                // never by which tool produced it. Failed and discovery-only
                // sources are not supplied to the critic, so they cannot
                // legitimize a claim; they are reported to it as coverage, so a
                // claim the source never addressed is judged `absent` rather
                // than invented.
                //
                // Three landings, none of which discard completed work:
                // - supported → accept;
                // - absent, with incomplete coverage → the claim is unverifiable,
                //   not false: the terminal degrades to a partial carrying the
                //   caveat, and no repair strike is spent bouncing a yield the
                //   agent cannot improve by resubmitting;
                // - contradicted, or absent with full coverage → the bounded
                //   repair. A strike is spent only when the agent brings NEW
                //   evidence; a resubmit ends the repair. Exhaustion degrades to
                //   a partial when there is completed work or an artifact to
                //   keep, and fails only the genuine "no progress, no evidence"
                //   case.
                if matches!(
                    disposition,
                    YieldDisposition::Completed { .. } | YieldDisposition::PartialSuccess { .. }
                ) {
                    let draft = terminal_grounding_draft(&payload);
                    let mut grounding_caveat: Option<String> = None;
                    match review_terminal_draft_against_opened_evidence(
                        ctx, executors, &history, &draft,
                    )
                    .heap_boxed()
                    .await
                    {
                        Ok(Some((verdict, telemetry, grounding))) => {
                            if let Some(cost) = telemetry.as_ref().map(|call| call.cost_usd) {
                                if cost.is_finite() && cost > 0.0 {
                                    loop_protective.cumulative_run_cost_usd += cost;
                                }
                            }
                            match terminal_grounding_route(&verdict, &grounding) {
                                TerminalGroundingRoute::Accept => {
                                    info!(
                                        successful_reads = grounding.successful_reads,
                                        failed_reads = grounding.failed_reads,
                                        non_admissible_retrievals = grounding.non_admissible_retrievals,
                                        "[TERMINAL-GROUNDING] draft supported by opened-page \
                                         evidence; unresolved read attempts are superseded"
                                    );
                                },
                                TerminalGroundingRoute::Caveat(caveat) => {
                                    warn!(
                                        successful_reads = grounding.successful_reads,
                                        non_admissible_retrievals = grounding.non_admissible_retrievals,
                                        "[TERMINAL-GROUNDING] claim is absent from admissible \
                                         evidence and coverage is incomplete; shipping as a \
                                         partial with a caveat: {}",
                                        verdict.reason
                                    );
                                    grounding_caveat = Some(caveat);
                                },
                                TerminalGroundingRoute::Repair => {
                                    match terminal_grounding_repair_decision(
                                        &mut loop_protective.terminal_grounding_repair_state,
                                        &verdict.reason,
                                        &grounding,
                                    ) {
                                        TerminalGroundingRepairDecision::Continue(rejection_reason) => {
                                            let _ = handle_terminal_evidence_rejection(
                                                history,
                                                &observed_state,
                                                iteration,
                                                &rejection_reason,
                                                Some(&draft),
                                                &mut loop_protective
                                                    .consecutive_goal_reached_rejections,
                                            );
                                            // EXIT: Advance — draft rejected
                                            return PhaseStep::exits(BoundaryOutcome::Advance);
                                        },
                                        TerminalGroundingRepairDecision::Exhausted(
                                            rejection_reason,
                                        ) => {
                                            if payload.completed.is_empty()
                                                && payload.artifacts.is_empty()
                                            {
                                                warn!(
                                                    "[TERMINAL-GROUNDING] bounded evidence repair \
                                                     exhausted with nothing completed; failing \
                                                     closed: {}",
                                                    verdict.reason
                                                );
                                                return PhaseStep::ends_run(AgenticOutcome::Failed {
                                                    reason: rejection_reason,
                                                    last_state: observed_state.clone(),
                                                    iterations_used: iteration,
                                                });
                                            }
                                            warn!(
                                                completed = payload.completed.len(),
                                                artifacts = payload.artifacts.len(),
                                                rejection = %rejection_reason,
                                                "[TERMINAL-GROUNDING] bounded evidence repair \
                                                 exhausted; keeping completed work as a partial \
                                                 with the verdict as its caveat: {}",
                                                verdict.reason
                                            );
                                            // The open item is user-facing: the judge's one
                                            // sentence, not the repair guidance that was
                                            // addressed to the model.
                                            grounding_caveat = Some(format!(
                                                "Unverified against opened-page evidence: {}",
                                                verdict.reason.trim()
                                            ));
                                        },
                                    }
                                },
                            }
                        },
                        Ok(None) => {},
                        Err(error) => {
                            // The grounding critic is an additive quality
                            // rail. Preserve normal task availability if its
                            // governed profile is temporarily unavailable;
                            // deterministic evidence checks still apply.
                            warn!(
                                task_id = ?ctx.task_id,
                                execution_id = ?ctx.execution_id,
                                error = %error,
                                "grounded terminal review unavailable; continuing with deterministic evidence gates"
                            );
                        },
                    }
                    if let Some(caveat) = grounding_caveat {
                        disposition = degrade_yield_to_partial(&mut payload, &caveat);
                        // The transition prepared above described a completed
                        // terminal. Re-prepare it for the partial the run is now
                        // shipping; if that cannot be prepared the partial still
                        // ships, and says so.
                        let durable_blockers = payload
                            .blockers
                            .iter()
                            .map(|blocker| {
                                format!("{:?}: {}", blocker.kind, blocker.description.trim())
                            })
                            .collect::<Vec<_>>();
                        prepared_terminal_task_state = match prepare_terminal_task_state_transition(
                            ctx,
                            executors,
                            &task_state_action_for_decision,
                            iteration,
                            false,
                            &payload.summary,
                            &payload.open,
                            &durable_blockers,
                            trust_dispatch_guard.as_ref(),
                        )
                        .heap_boxed()
                        .await
                        {
                            Ok(prepared) => Some(prepared),
                            Err(error) => {
                                warn!(
                                    task_id = ?ctx.task_id,
                                    execution_id = ?ctx.execution_id,
                                    error = %error,
                                    "terminal task-state transition could not be re-prepared for \
                                     the degraded partial; shipping the partial without it"
                                );
                                None
                            },
                        };
                        info!(
                            iteration = iteration,
                            completed = payload.completed.len(),
                            open = payload.open.len(),
                            disposition = ?disposition,
                            "[TERMINAL-GROUNDING] terminal degraded to a partial with a grounding caveat"
                        );
                    }
                }

                // ═══════════════════════════════════════════════════════
                // GIVE-UP GATE on the LIVE terminal (symmetric to the
                // success gate above). A yield that disposes to Failed /
                // RetryTransient is a SURRENDER. The success path is gated
                // (a bogus "done" is soft-rejected + retried), but the
                // give-up path was NOT — so a model that fumbled a step a
                // couple of times could yield Failed at iteration 17 of 2000
                // and have it rubber-stamped. Enforce the persistence
                // discipline (which otherwise lives only as an ignorable
                // prompt nudge): bounce a PREMATURE give-up — budget remains,
                // blocker non-permanent, recovery not exhausted — back into
                // the loop with concrete "switch grounding and retry"
                // guidance, via the SAME helper + 3-strike abort as the
                // success gate. Synthetic stuck-auto-yields are exempt (they
                // already ARE the exhaustion signal).
                if matches!(
                    disposition,
                    YieldDisposition::Failed { .. } | YieldDisposition::RetryTransient { .. }
                ) {
                    if let Some(rejection_reason) = premature_giveup_rejection_reason(
                        ctx,
                        &payload,
                        iteration,
                        decision_is_synthetic_stuck,
                    ) {
                        let abort = handle_terminal_evidence_rejection(
                            history,
                            &observed_state,
                            iteration,
                            &rejection_reason,
                            None,
                            &mut loop_protective.consecutive_giveup_rejections,
                        );
                        warn!(
                            "[GIVE-UP-REJECTED] iteration {}: bounced premature give-up \
                             (consecutive: {})",
                            iteration, loop_protective.consecutive_giveup_rejections
                        );
                        if !abort {
                            // Re-prompt the model with the rejection reason in
                            // history; it must re-ground and retry before it is
                            // allowed to surrender.
                            // EXIT: Advance — the model must re-ground before it may surrender
                            return PhaseStep::exits(BoundaryOutcome::Advance);
                        }
                        // 3-strike cap hit — stop bouncing and honour the
                        // give-up disposition below.
                    }
                }

                if let Some(prepared) = prepared_terminal_task_state {
                    commit_terminal_task_state_transition(ctx, executors, prepared)
                        .heap_boxed()
                        .await
                        .map_err(|error| {
                            anyhow!(
                                "accepted terminal could not atomically persist durable task \
                                 state: {error}"
                            )
                        })?;
                }

                // Browser session handoff: the LLM's terminal yield
                // carries the intent. `ctx` is an immutable borrow
                // here, so we record it on the interior-mutable
                // cleanup-override atomics on `executors` — the
                // post-loop `cleanup_browser_session_if_done` ORs
                // these with the static `ctx.keep_browser_*` flags.
                // cdp-alive wins over window-open (resolved in cleanup).
                if payload.keep_browser_cdp_connection_alive {
                    executors
                        .browser_run
                        .keep_alive_override
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                if payload.keep_browser_window_open {
                    executors
                        .browser_run
                        .window_open_override
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                let summary_for_log = payload.summary.clone();
                info!(
                    iteration = iteration,
                    completed = payload.completed.len(),
                    open = payload.open.len(),
                    blockers = payload.blockers.len(),
                    artifacts = payload.artifacts.len(),
                    disposition = ?disposition,
                    "[AGENTIC] yield: dispatching disposition for {}",
                    summary_for_log
                );

                // ═══════════════════════════════════════════════════════
                // WORK-LEDGER (P1): stamp exactly one `work_outcome` record
                // per terminal ROOT run. The `Decision::Yield` handler is
                // the model's real terminal. Completed and PartialSuccess now
                // both persist rich completion summaries below, while Failed
                // / RetryTransient can still return without that funnel.
                // Write the record HERE — before the disposition match — so
                // every terminal disposition is covered. `PartialSuccess` is
                // skipped (its own funnel writes the richer
                // `goal_achieved_partial` record); Completed may upsert the
                // same deterministic `evd:run:{root_execution_id}` from its
                // richer summary below. The write is idempotent.
                // Additive + fail-soft, mirrors the non-Yield terminal arms.
                if !matches!(disposition, YieldDisposition::PartialSuccess { .. }) {
                    let ledger_outcome = match disposition {
                        YieldDisposition::Completed { .. } => "goal_achieved",
                        // RetryTransient surfaces to the caller as
                        // `AgenticOutcome::Failed` (no first-class retry
                        // outcome yet), so ledger it as a failure too.
                        YieldDisposition::Failed { .. } | YieldDisposition::RetryTransient { .. } => {
                            "failed"
                        },
                        // Unreachable — guarded out above — but keep the match
                        // total so a new disposition variant is a compile error.
                        YieldDisposition::PartialSuccess { .. } => "goal_achieved_partial",
                    };
                    let ledger_yield_summary = yield_payload_summary_for(&payload, &disposition);
                    let ledger_summary = build_agentic_execution_summary_with_yield(
                        ctx,
                        executors,
                        ledger_outcome,
                        iteration,
                        payload.artifacts.iter().map(|a| a.name.clone()).collect(),
                        execution_start.elapsed().as_millis() as u64,
                        summary_for_log.clone(),
                        None,
                        ledger_yield_summary,
                    );
                    spawn_work_ledger_write(ctx, executors, &ledger_summary)
                        .heap_boxed()
                        .await;
                }

                match disposition {
                    YieldDisposition::Completed { .. } => {
                        let completed_text = payload
                            .completed
                            .iter()
                            .map(|item| item.trim())
                            .filter(|item| !item.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let success_summary =
                            terminal_success_summary(&payload.summary, &completed_text);
                        let mut final_artifacts = payload.artifacts.clone();
                        if let Some(deliverable) = task_backed_completed_yield_artifact(ctx, &payload) {
                            final_artifacts.push(deliverable);
                        }
                        materialize_and_index_terminal_artifacts(
                            ctx,
                            executors,
                            &mut final_artifacts,
                            "outer_loop_yield_completed",
                        )
                        .heap_boxed()
                        .await;

                        // Clean yields need the same durable completion
                        // projection as partial yields. Without this record,
                        // text-only `completed[]` content existed only in the
                        // transient decision and task user-output synthesis
                        // could degrade to a status summary.
                        let yield_summary = yield_payload_summary_for(&payload, &disposition);
                        let completion_summary = build_agentic_execution_summary_with_yield(
                            ctx,
                            executors,
                            "goal_achieved",
                            iteration,
                            final_artifacts
                                .iter()
                                .map(|artifact| artifact.name.clone())
                                .collect(),
                            execution_start.elapsed().as_millis() as u64,
                            success_summary.clone(),
                            None,
                            yield_summary,
                        );
                        emit_and_persist_agentic_completion(
                            ctx,
                            executors,
                            Some((iteration, Phase::Apply)),
                            completion_summary,
                            Some(&history),
                        )
                        .heap_boxed()
                        .await;

                        persist_task_completion_result_if_root(
                            executors,
                            ctx,
                            success_summary,
                            "goal_achieved".to_string(),
                            final_artifacts
                                .iter()
                                .map(|artifact| artifact.name.clone())
                                .collect(),
                        )
                        .heap_boxed()
                        .await;

                        return PhaseStep::ends_run(AgenticOutcome::Success {
                            completion:
                                crate::magician_v2::execution::agentic::types::CompletionKind::Full,
                            open: Vec::new(),
                            // Post-action state, not the stale observe snapshot
                            // (matches the Completed path; relevant for flat-mode
                            // yield terminals). 0.8c regression fix.
                            final_state: current_state.clone(),
                            iterations_used: iteration,
                            artifacts: final_artifacts,
                        });
                    },
                    YieldDisposition::PartialSuccess { .. } => {
                        // Mirror the PARTIAL-SUCCESS-SAFETY-NET on
                        // the legacy `cannot_proceed` path. Yield's
                        // structured fields make this path explicit
                        // and lossless: we build the
                        // `partial_findings.md` artifact directly
                        // from `completed[]` / `open[]` /
                        // `blockers[]` and stamp
                        // `outcome_type = "goal_achieved_partial"`
                        // so the memory layer
                        // (`legacy_episode_outcome`) classifies the
                        // episode as `PartialProgress` rather than
                        // a clean `GoalAchieved`. That's the
                        // surface the `failure_adaptation` feedback
                        // loop subscribes to via the `is_partial`
                        // filter token — without this branch a
                        // partial yield would never reach that
                        // learning surface.
                        //
                        // We also stamp `yield_payload` on the
                        // execution summary so UI surfaces /
                        // dashboards can render the structured
                        // `completed[]` / `open[]` / `blockers[]`
                        // without re-parsing the partial_findings
                        // markdown.
                        let partial_text = render_yield_partial_findings(&payload);
                        let mut partial_artifact =
                            crate::magician_v2::execution::agentic::types::Artifact::text(
                                "partial_findings.md",
                                partial_text.clone(),
                            );
                        partial_artifact.content_type = "text/markdown".to_string();
                        partial_artifact.artifact_type = Some("partial_findings".to_string());

                        let success_summary =
                            format!("Goal partially achieved: {}\n\n{}", ctx.goal, partial_text);

                        let yield_summary = yield_payload_summary_for(&payload, &disposition);

                        let completion_summary = build_agentic_execution_summary_with_yield(
                            ctx,
                            executors,
                            "goal_achieved_partial",
                            iteration,
                            vec![format!("partial:{}", partial_artifact.name)],
                            execution_start.elapsed().as_millis() as u64,
                            success_summary.clone(),
                            Some(summary_for_log.clone()),
                            yield_summary,
                        );
                        emit_and_persist_agentic_completion(
                            ctx,
                            executors,
                            Some((iteration, Phase::Apply)),
                            completion_summary,
                            Some(&history),
                        )
                        .heap_boxed()
                        .await;

                        persist_task_completion_result_if_root(
                            executors,
                            ctx,
                            success_summary,
                            "goal_achieved_partial".to_string(),
                            vec![],
                        )
                        .heap_boxed()
                        .await;

                        let mut final_artifacts = payload.artifacts.clone();
                        final_artifacts.push(partial_artifact);
                        materialize_and_index_terminal_artifacts(
                            ctx,
                            executors,
                            &mut final_artifacts,
                            "outer_loop_yield_partial",
                        )
                        .heap_boxed()
                        .await;

                        return PhaseStep::ends_run(AgenticOutcome::Success {
                            completion:
                                crate::magician_v2::execution::agentic::types::CompletionKind::Partial,
                            open: payload.open.clone(),
                            // Post-action state, not the stale observe snapshot
                            // (matches the Completed path; relevant for flat-mode
                            // yield terminals). 0.8c regression fix.
                            final_state: current_state.clone(),
                            iterations_used: iteration,
                            artifacts: final_artifacts,
                        });
                    },
                    YieldDisposition::Failed { reason } => {
                        // Auth/Permission stay HITL so the owner can unblock.
                        // Give-up (no permission wall) is terminal even when
                        // on_failure is AskUser — that mode is not an interview.
                        if ctx.on_failure == OnFailureMode::AskUser {
                            if let Some((trigger, question)) =
                                yield_escalation_prompt(&payload.blockers, &reason)
                            {
                                let outcome = escalate_to_user(
                                    ctx,
                                    executors,
                                    Some((iteration, Phase::Apply)),
                                    iteration,
                                    &observed_state,
                                    &history,
                                    trigger,
                                    question,
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
                                return PhaseStep::ends_run(outcome);
                            }
                        }

                        // on_failure == Fail: keep terminal failure, but emit
                        // the completion event FIRST so the reason surfaces
                        // (parity with the action-error Failed path at ~10301).
                        let completion_summary = build_agentic_execution_summary(
                            ctx,
                            executors,
                            "cannot_proceed",
                            iteration,
                            Vec::new(),
                            execution_start.elapsed().as_millis() as u64,
                            format!("Cannot proceed: {}", reason),
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            Some(reason.clone()),
                        );
                        emit_and_persist_agentic_completion(
                            ctx,
                            executors,
                            Some((iteration, Phase::Apply)),
                            completion_summary,
                            Some(&history),
                        )
                        .heap_boxed()
                        .await;
                        return PhaseStep::ends_run(AgenticOutcome::Failed {
                            reason,
                            last_state: observed_state.clone(),
                            iterations_used: iteration,
                        });
                    },
                    YieldDisposition::RetryTransient { blocker_count } => {
                        // P0.1: transient blockers are safe to retry. Rather
                        // than fabricate a `Failed`, bounce back into the loop
                        // with backoff — capped by `MAX_TRANSIENT_RETRIES` on a
                        // dedicated counter (NOT reset by decision success).
                        // Once the cap is hit we stop bouncing and surface the
                        // terminal state (HITL when `on_failure == AskUser`,
                        // else Failed).
                        loop_protective.yield_transient_retry_count += 1;
                        let retry_cap = MAX_TRANSIENT_RETRIES;
                        if loop_protective.yield_transient_retry_count <= retry_cap {
                            let backoff =
                                transient_retry_backoff(loop_protective.yield_transient_retry_count);
                            warn!(
                                "[YIELD-RETRY-TRANSIENT] iteration {}: {} transient \
                                 blocker(s); backing off {:?} then re-looping (attempt {}/{})",
                                iteration,
                                blocker_count,
                                backoff,
                                loop_protective.yield_transient_retry_count,
                                retry_cap
                            );
                            // The wait is requested, not taken. An in-place
                            // sleep holds whoever owns the executor for the
                            // whole backoff; the driver takes it at the
                            // boundary instead.
                            // Re-enter the loop; the model re-observes and can
                            // retry now that the transient condition may have
                            // cleared.
                            // EXIT: Retry — after the requested backoff; transient yield blocker
                            return PhaseStep::exits(BoundaryOutcome::Retry(backoff));
                        }

                        let reason = format!(
                            "{blocker_count} transient blocker(s) did not clear after \
                             {retry_cap} retries: {summary_for_log}",
                        );
                        let completion_summary = build_agentic_execution_summary(
                            ctx,
                            executors,
                            "cannot_proceed",
                            iteration,
                            Vec::new(),
                            execution_start.elapsed().as_millis() as u64,
                            format!("Cannot proceed: {}", reason),
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            Some(reason.clone()),
                        );
                        emit_and_persist_agentic_completion(
                            ctx,
                            executors,
                            Some((iteration, Phase::Apply)),
                            completion_summary,
                            Some(&history),
                        )
                        .heap_boxed()
                        .await;
                        return PhaseStep::ends_run(AgenticOutcome::Failed {
                            reason,
                            last_state: observed_state.clone(),
                            iterations_used: iteration,
                        });
                    },
                }
            }
            .heap_boxed()
            .await
        },

        // ═══════════════════════════════════════════════════════════
        // HANDOVER TO AGENT - Same-execution ownership transfer
        // ═══════════════════════════════════════════════════════════
        Decision::HandoverToAgent {
            target_agent_id,
            context,
            preserve_live_execution_context,
        } => {
            let history_start = history.iterations.len();
            let lineage = begin_agentic_tool_lineage(
                ctx,
                executors,
                Some((iteration, Phase::Apply)),
                &history,
                iteration,
                0,
                &mut loop_protective.agentic_tool_repeat_counts,
            );
            let approvals_before = approved_confirmation_actions.len();
            let handover_result = handle_handover_to_agent_decision(
                ctx,
                &observed_state,
                history,
                iteration,
                executors,
                Some((iteration, Phase::Apply)),
                &execution_start,
                approved_confirmation_actions,
                loop_protective,
                previous_merkle_tree,
                target_agent_id,
                context,
                preserve_live_execution_context,
            )
            .heap_boxed()
            .await;
            let approval_was_obtained = approved_confirmation_actions.len() < approvals_before;
            if let Some(lineage) = finish_agentic_tool_lineage(
                ctx,
                executors,
                Some((iteration, Phase::Apply)),
                &history,
                history_start,
                lineage,
                handover_result.as_ref().err(),
                approval_was_obtained,
                &mut loop_protective.agentic_failed_tool_fingerprints,
                &[],
            ) {
                pending_agentic_tool_lineages.push(lineage);
            }
            let handover_control = handover_result?;
            match handover_control {
                ExecutePathControl::Continue => {
                    // The next iteration refreshes owner and trust policy
                    // before it builds another provider decision.
                    // EXIT: NextIteration — owner handover, refresh owner and trust first
                    return PhaseStep::exits(BoundaryOutcome::NextIteration);
                },
                ExecutePathControl::Return(outcome) => return Ok(PhaseStep::Return(outcome)),
            }
        },

        // ═══════════════════════════════════════════════════════════
        // DELEGATE TO AGENT - Cross-agent dispatch-and-wait
        // ═══════════════════════════════════════════════════════════
        Decision::DelegateToAgent { targets } => {
            let history_start = history.iterations.len();
            let lineage = begin_agentic_tool_lineage(
                ctx,
                executors,
                Some((iteration, Phase::Apply)),
                &history,
                iteration,
                0,
                &mut loop_protective.agentic_tool_repeat_counts,
            );
            let approvals_before = approved_confirmation_actions.len();
            let delegate_result = handle_delegate_to_agent_decision(
                ctx,
                &observed_state,
                history,
                iteration,
                executors,
                Some((iteration, Phase::Apply)),
                &execution_start,
                &cancellation_token,
                approved_confirmation_actions,
                loop_protective,
                previous_merkle_tree,
                targets,
            )
            .heap_boxed()
            .await;
            let approval_was_obtained = approved_confirmation_actions.len() < approvals_before;
            let delegated_execution_ids = match &delegate_result {
                Ok(ExecutePathControl::Return(outcome)) => match outcome.as_ref() {
                    AgenticOutcome::WaitingForChildren {
                        child_execution_ids,
                        ..
                    } => child_execution_ids.as_slice(),
                    _ => &[],
                },
                _ => &[],
            };
            if let Some(lineage) = finish_agentic_tool_lineage(
                ctx,
                executors,
                Some((iteration, Phase::Apply)),
                &history,
                history_start,
                lineage,
                delegate_result.as_ref().err(),
                approval_was_obtained,
                &mut loop_protective.agentic_failed_tool_fingerprints,
                delegated_execution_ids,
            ) {
                pending_agentic_tool_lineages.push(lineage);
            }
            let delegate_control = delegate_result?;
            match delegate_control {
                ExecutePathControl::Continue => {
                    // An Option-B in-context delegation swapped the owner to
                    // the sub-agent and restored the PA root on THIS execution;
                    // re-derive the dispatch guard so the next iteration enforces
                    // the restored owner's trust (mirrors the handover Continue
                    // arm). Harmless no-op for the spawn-child path (owner
                    // unchanged there).
                    // EXIT: NextIteration — in-context delegation swapped the owner
                    return PhaseStep::exits(BoundaryOutcome::NextIteration);
                },
                ExecutePathControl::Return(outcome) => return Ok(PhaseStep::Return(outcome)),
            }
        },

        // ═══════════════════════════════════════════════════════════
        // SPAWN SUB-GOAL - Depth-bounded dynamic recursion
        // ═══════════════════════════════════════════════════════════
        Decision::SpawnSubGoal {
            goal: sub_goal,
            unblocks,
            budget_iterations,
        } => {
            let history_start = history.iterations.len();
            let lineage = begin_agentic_tool_lineage(
                ctx,
                executors,
                Some((iteration, Phase::Apply)),
                &history,
                iteration,
                0,
                &mut loop_protective.agentic_tool_repeat_counts,
            );
            let approvals_before = approved_confirmation_actions.len();
            let spawn_sub_goal_result = handle_spawn_sub_goal_decision(
                ctx,
                &observed_state,
                current_state,
                history,
                iteration,
                executors,
                Some((iteration, Phase::Apply)),
                &execution_start,
                &cancellation_token,
                approved_confirmation_actions,
                loop_protective,
                previous_merkle_tree,
                sub_goal,
                unblocks,
                budget_iterations,
            )
            .heap_boxed()
            .await;
            let approval_was_obtained = approved_confirmation_actions.len() < approvals_before;
            let child_execution_ids = match &spawn_sub_goal_result {
                Ok(ExecutePathControl::Return(outcome)) => match outcome.as_ref() {
                    AgenticOutcome::WaitingForChildren {
                        child_execution_ids,
                        ..
                    } => child_execution_ids.as_slice(),
                    _ => &[],
                },
                _ => &[],
            };
            if let Some(lineage) = finish_agentic_tool_lineage(
                ctx,
                executors,
                Some((iteration, Phase::Apply)),
                &history,
                history_start,
                lineage,
                spawn_sub_goal_result.as_ref().err(),
                approval_was_obtained,
                &mut loop_protective.agentic_failed_tool_fingerprints,
                child_execution_ids,
            ) {
                pending_agentic_tool_lineages.push(lineage);
            }
            let spawn_sub_goal_control = spawn_sub_goal_result?;
            match spawn_sub_goal_control {
                ExecutePathControl::Continue => {
                    // EXIT: NextIteration — sub-goal spawned
                    return PhaseStep::exits(BoundaryOutcome::NextIteration);
                },
                ExecutePathControl::Return(outcome) => return Ok(PhaseStep::Return(outcome)),
            }
        },

        // ═══════════════════════════════════════════════════════════
        // EXECUTE ACTION - execute the selected agent action
        // ═══════════════════════════════════════════════════════════
        // ═══════════════════════════════════════════════════════════
        // EXECUTE — routed away before this match ran
        // ═══════════════════════════════════════════════════════════
        //
        // The only arm that dispatches, so the only one with a gate/fire
        // seam, so the only one this function cannot answer: its answer is an
        // `ApplyGate` and everything here is a `PhaseStep`. `gate` sends it to
        // `gate_execute` before calling this.
        //
        // The arm still has to exist — a `match` is exhaustive — and what it
        // does matters. It **refuses** rather than panicking, because a phase
        // that returns `Err` is a failed attempt a driver can count, quarantine
        // and report, while a panic takes the worker with it. And it refuses
        // rather than falling through to a dispatch, because a dispatch reached
        // from here would have no way to publish the batch it fired.
        Decision::Execute { .. } => {
            return Err(anyhow!(
                "`Decision::Execute` reached the settle-only half of `Apply`. It is the only \
                 arm that dispatches, so it is the only one with a gate/fire seam, and `gate` \
                 routes it to `gate_execute` before this match runs. Reaching here means the \
                 routing and this arm have drifted"
            ));
        },

        // ═══════════════════════════════════════════════════════════
        // NEED USER INPUT - Pause and wait for user response
        // ═══════════════════════════════════════════════════════════
        Decision::NeedUserInput {
            question,
            input_type,
            hint,
            options: _,
        } => {
            // A harness turn's approval gate surfaces here as NeedUserInput
            // with a Confirmation payload (plane Task 8). The plane captured
            // the gated action when it ended the turn; replay it as the
            // loop's OWN confirmation pause so the human is asked through the
            // ordinary HITL path and resume executes exactly that action,
            // rather than depending on the harness to re-issue a
            // byte-identical call. The Confirmation gate is load-bearing:
            // a capture can only exist for this turn's gate, but matching on
            // the input type keeps any other NeedUserInput shape from ever
            // hijacking a capture into an executable confirmation.
            if matches!(input_type, UserInputType::Confirmation { .. }) {
                if let Some(pending) =
                    crate::magician_v2::execution::plane::turn_engine::peek_harness_pending_approval(
                        &crate::magician_v2::execution::plane::turn_engine::harness_continuation_key(ctx),
                    )
                {
                    if let Ok(action) = serde_json::from_str::<
                        crate::magician_v2::execution::actions::ExecutableAction,
                    >(&pending.action_json)
                    {
                        let reason = format!(
                            "Plane approval gate ended the harness turn.\n\n**Action**: {}",
                            pending.action_summary
                        );
                        let outcome = build_action_confirmation_outcome(
                            ctx,
                            executors,
                            Some((iteration, Phase::Apply)),
                            iteration,
                            &observed_state,
                            &history,
                            &action,
                            pending.action_json.clone(),
                            pending.action_summary.clone(),
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
                        return PhaseStep::ends_run(outcome);
                    }
                }
            }

            // Try to match the question to a pending input
            let asking_for_parameter = find_matching_pending_input(ctx, &question);

            if ctx.app_disclosure_guard.is_some() {
                info!(
                    iteration,
                    has_parameter = asking_for_parameter.is_some(),
                    input_type = %format_input_type_for_event(&input_type),
                    "[AGENTIC] Protected app workflow needs user input"
                );
            } else {
                info!(
                    iteration = iteration,
                    asking_for = ?asking_for_parameter,
                    "[AGENTIC] User input needed: {}",
                    question
                );
            }

            // Build pause state to preserve execution context
            let mut pause_state = build_full_pause_state(
                iteration,
                ctx,
                &observed_state,
                &history,
                &loop_protective,
                &approved_confirmation_actions,
            );
            // Decide the ask's sensitivity once, here: the announcement
            // publishes it and the resume path enforces it.
            pause_state.pending_sensitive = sensitive_spec_for_pause(
                &input_type,
                &question,
                hint.as_deref(),
                asking_for_parameter.as_deref(),
                &ctx.pending_inputs,
                Utc::now().timestamp_millis(),
            );
            bind_pause_spec_to_pending_challenge(&mut pause_state.pending_sensitive, executors);

            // Emit dedicated waiting for user event
            info!(
                has_observability = ctx.has_observability(),
                execution_id = %runtime_execution_id(ctx),
                plan_id = ?ctx.plan_id,
                step_id = ?ctx.step_id,
                has_broadcaster = executors.event_broadcaster.is_some(),
                "[AGENTIC-PAUSE] Checking observability for AgenticWaitingForUser event"
            );
            if ctx.has_observability() || ctx.has_agent_routing() {
                info!("[AGENTIC-PAUSE] Emitting AgenticWaitingForUser event");
                let (principal, workspace) = transport_scope(ctx);
                emit_agent_execution_mapping(ctx, executors, Some((iteration, Phase::Apply)));
                super::outbox::journal_and_emit(
                    ctx,
                    executors,
                    iteration,
                    Phase::Apply,
                    RuntimeTransportEvent::AgenticWaitingForUser {
                        execution_id: runtime_execution_id(ctx),
                        principal,
                        workspace,
                        plan_id: ctx.plan_id.clone().unwrap_or_default(),
                        step_id: ctx.step_id.clone().unwrap_or_default(),
                        iteration,
                        pause_state_id: Some(pause_state.storage_key()),
                        correlation_id: None,
                        is_retry: None,
                        retry_count: None,
                        agent_id: ctx.agent_id.clone(),
                        goal_id: ctx.goal_id.clone(),
                        cycle_id: ctx.cycle_id.clone(),
                        escalation_trigger: None,
                        timestamp: Utc::now().timestamp_millis(),
                    },
                );
                emit_hitl_requested_for_agentic_pause(
                    ctx,
                    executors,
                    Some((iteration, Phase::Apply)),
                    &pause_state.hitl_correlation_id(),
                    &input_type,
                    question.clone(),
                    hint.clone(),
                    pause_state.pending_sensitive.as_ref(),
                );
                info!("[AGENTIC-PAUSE] AgenticWaitingForUser event emitted successfully");
            } else {
                warn!(
                    execution_id = %runtime_execution_id(ctx),
                    plan_id = ?ctx.plan_id,
                    step_id = ?ctx.step_id,
                    "[AGENTIC-PAUSE] Skipping AgenticWaitingForUser event - no observability or agent routing"
                );
            }

            let outcome = AgenticOutcome::WaitingForUser {
                question,
                input_type,
                hint,
                pause_state: Box::new(pause_state),
                asking_for_parameter,
                pending_inputs: ctx.pending_inputs.clone(),
                resolved_inputs: ctx.resolved_inputs.clone(),
                escalation_trigger: None,
            };
            persist_paused_execution_summary(
                ctx,
                executors,
                &outcome,
                execution_start.elapsed().as_millis() as u64,
            )
            .heap_boxed()
            .await;
            return PhaseStep::ends_run(outcome);
        },
    }
}

/// Gate `Apply::Execute` — the only arm that dispatches, and so the only one
/// with a seam.
///
/// Everything up to and including the gate: candidate selection, loop pressure,
/// the trust boundary, the approval and confirmation gates, the allowlist, the
/// lineage this dispatch will be recorded under, and then one pass admitting the
/// whole in-turn batch. **No dispatch runs here.**
#[allow(clippy::too_many_arguments)]
async fn gate_execute(
    ctx: &mut AgenticContext,
    executors: &Arc<ActionExecutors>,
    history: &mut ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    // Read-only, as in every other half of this phase. See `gate`'s own note.
    trust_dispatch_guard: &Option<TrustDispatchGuard>,
    approved_confirmation_actions: &mut Vec<ApprovedConfirmationAction>,
    pending_agentic_tool_lineages: &mut Vec<AgenticToolLineageState>,
    observed_state: EnvironmentState,
    candidates: CandidateBatch,
    thinking: String,
    task_state_action_for_decision: TaskStateActionEnvelope,
    execution_start: &Instant,
    iteration: usize,
    committed_batch: &mut Option<PendingBatch>,
    effect_identity: EffectIdentityPosture,
) -> Result<ApplyGate> {
    let gated: ExecuteGate = Box::pin(async {


        let total_candidates = candidates.candidates.len();
        debug!(
            "[EXECUTOR] Multi-candidate execution with {} candidates",
            total_candidates
        );

        if candidates.candidates.is_empty() {
            return Ok(ExecuteGate::Settled(return_outcome(
                AgenticOutcome::CannotProceed {
                    reason: "Execute received with empty candidates list".to_string(),
                    last_state: observed_state,
                    iterations_used: iteration,
                },
            )));
        }

        // Note: AgenticDecisionMade event is already emitted at the common
        // decision handler above (site 2) for ALL decision types including Execute.
        // Do NOT emit a second event here to avoid duplicates.

        // Check for loop BEFORE executing using sophisticated fingerprinting
        // This detects: state loops, action cycles (A→B→A→B), and no-progress
        let first_candidate = candidates
            .candidates
            .first()
            .expect("Already checked candidates not empty")
            .clone();
        let llm_requires_confirmation =
            first_candidate.requires_confirmation.unwrap_or(false);
        let loop_check =
            loop_protective.loop_detector.check_before_action(&observed_state, &first_candidate.action);

        // ═══════════════════════════════════════════════════════════
        // POST-PRESSURE CHECK: After loop pressure, prepare_for_recovery()
        // clears state_history. With empty state history, check_before_action
        // returns Ok. If the model proposes the exact same action again,
        // surface another advisory turn instead of declaring semantic
        // failure in Rust.
        // ═══════════════════════════════════════════════════════════
        if !loop_check.is_loop_detected() {
            if let Some(ref recovery_ctx) = loop_protective.loop_recovery_context {
                let action_sig = action_signature(&first_candidate.action);
                if action_sig == recovery_ctx.action_signature {
                    warn!(
                        "[LOOP-PRESSURE] Model proposed same action '{}' after progress pressure. Returning advisory context.",
                        action_summary_for_diagnostics(
                            &first_candidate.action,
                            ctx.app_disclosure_guard.is_some(),
                        )
                    );
                    push_loop_pressure_record(
                        history,
                        iteration,
                        &observed_state,
                        &first_candidate,
                        recovery_ctx.format_repeated_action_for_llm(),
                    );
                    loop_protective.loop_detector.prepare_for_recovery();
                    return Ok(ExecuteGate::Settled(ExecutePathControl::Continue));
                }
            }
        }

        // ═══════════════════════════════════════════════════════════
        // IFRAME EXCEPTION: Skip STATE_LOOP for iframe actions (cross-origin OR same-origin).
        // Iframes (both types) don't change the parent page's DOM, so interacting with
        // them will naturally trigger state loop detection. This is a false positive.
        // We still respect ACTION_CYCLE detection to prevent infinite iframe clicking.
        // ═══════════════════════════════════════════════════════════
        let is_iframe_action = candidate_targets_iframe(&first_candidate, &observed_state);
        let skip_loop_for_iframe =
            is_iframe_action && matches!(&loop_check, LoopCheckResult::StateLoop { .. });

        if skip_loop_for_iframe {
            debug!(
                "[IFRAME-LOOP-SKIP] Skipping state_loop detection for iframe action: {}. \
                Iframes don't change parent DOM.",
                action_summary_for_diagnostics(
                    &first_candidate.action,
                    ctx.app_disclosure_guard.is_some(),
                )
            );
        }

        if loop_check.is_loop_detected() && !skip_loop_for_iframe {
            let action_sig = action_signature(&first_candidate.action);
            let (detection_type, recommendation) = match &loop_check {
                LoopCheckResult::StateLoop {
                    recommendation,
                    ..
                } => ("state_loop".to_string(), recommendation.clone()),
                LoopCheckResult::ActionCycle {
                    recommendation,
                    ..
                } => ("action_cycle".to_string(), recommendation.clone()),
                LoopCheckResult::NoProgress {
                    recommendation,
                    ..
                } => ("no_progress".to_string(), recommendation.clone()),
                LoopCheckResult::Ok => unreachable!(),
            };

            // ═══════════════════════════════════════════════════════════════════
            // LOOP PRESSURE: Feed repetition signals back to the LLM. The
            // detector is a progress sensor, not the owner of task semantics.
            // Hard iteration/time budgets remain the emergency stop.
            // ═══════════════════════════════════════════════════════════════════

            tracing::info!(
                target: "magician::metrics::loop_pressure",
                counter = 1_u64,
                detection_type = %detection_type,
                iterations = iteration,
                goal = %goal_for_diagnostics(ctx),
                "loop_pressure_total"
            );

            warn!(
                "[LOOP-PRESSURE] Detected {} after {} iterations for {}. Returning advisory context.",
                detection_type,
                iteration,
                action_summary_for_diagnostics(
                    &first_candidate.action,
                    ctx.app_disclosure_guard.is_some(),
                )
            );

            let alternate_capabilities = derive_alternate_capabilities(
                &first_candidate.action,
                ctx,
                executors,
            );
            if !alternate_capabilities.is_empty() {
                info!(
                    target: "magician::metrics::loop_recovery_alternates",
                    counter = 1_u64,
                    stuck_pack = %action_summary_for_diagnostics(
                        &first_candidate.action,
                        ctx.app_disclosure_guard.is_some(),
                    ),
                    alternates = %alternate_capabilities.join(","),
                    "loop_recovery_alternates_suggested"
                );
            }
            let pressure_context = LoopRecoveryContext {
                detection_type: detection_type.clone(),
                action_signature: action_sig,
                recommendation,
                iteration_detected: iteration,
                alternate_capabilities,
            };
            let loop_warning = pressure_context.format_for_llm();
            push_loop_pressure_record(
                history,
                iteration,
                &observed_state,
                &first_candidate,
                loop_warning,
            );
            loop_protective.loop_recovery_context = Some(pressure_context);
            loop_protective.loop_detector.prepare_for_recovery();

            return Ok(ExecuteGate::Settled(ExecutePathControl::Continue));
        }

        // Trust is the outer authority boundary and cannot be
        // overridden by an approval or by a provider emitting a tool
        // that was intentionally absent from its catalog. Check it
        // before the confirmation and visibility gates. Otherwise a
        // stale/hallucinated denied call is recorded as a recoverable
        // catalog miss and can repeat until the global iteration cap,
        // instead of failing loudly with the durable trust reason.
        if let Err(error) = enforce_action_trust_policy(
            &first_candidate.action,
            trust_dispatch_guard.as_ref(),
        ) {
            let reason = error.to_string();
            warn!(
                "[EXECUTOR] Trust policy denied action at iteration {}: {}",
                iteration, reason
            );
            let completion_summary = build_agentic_execution_summary(
                ctx,
                executors,
                "failed",
                iteration,
                Vec::new(),
                execution_start.elapsed().as_millis() as u64,
                format!("Failed: {}", reason),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            );
            emit_and_persist_agentic_completion(
                ctx,
                executors,
                Some((iteration, Phase::Apply)),
                completion_summary,
                Some(&history),
            )
            .heap_boxed().await;
            return Ok(ExecuteGate::Settled(return_outcome(AgenticOutcome::Failed {
                reason,
                last_state: observed_state.clone(),
                iterations_used: iteration,
            })));
        }

        let action_json =
            serde_json::to_string(&first_candidate.action).unwrap_or_default();
        let has_explicit_confirmation_for_action =
            take_matching_approved_confirmation(
                approved_confirmation_actions,
                &action_json,
                ctx,
            );
        if has_explicit_confirmation_for_action {
            info!(
                "[EXECUTOR] Skipping confirmation gate for previously confirmed action: {}",
                first_candidate.action.action_type_name()
            );
        } else {
            let mut confirmation_reasons = Vec::new();
            if !ctx.approval_rules.is_empty() {
                let plan = ExecutionPlan {
                    plan_id: ctx
                        .plan_id
                        .clone()
                        .unwrap_or_else(|| format!("iter-{iteration}")),
                    steps: vec![approval_step_from_candidate(
                        &first_candidate,
                        iteration,
                        &ctx.goal,
                    )],
                };
                let constraints = AgentConstraints {
                    requires_approval: ctx.approval_rules.clone(),
                    ..AgentConstraints::default()
                };
                // Envelopes in front of `requires_approval` — plan
                // phase 5. Nothing is waived unless the posture is
                // `enforcing` and a live envelope covers the act's
                // consequence class. The class is derived by the strict
                // approval-rule classifier, which never answers
                // `private_local` and fails closed to commitment, so an
                // unclassified gated act is never waived.
                let approval_check =
                    checked_approval(ctx, executors, &plan, &constraints).await;
                if let ApprovalResult::NeedsApproval {
                    pending_actions, ..
                } = approval_check.result
                {
                    if let Some(pending) = pending_actions.first() {
                        let rule_action =
                            serde_json::to_string(&pending.matched_rule.action)
                                .unwrap_or_else(|_| "\"*\"".to_string());
                        let confirmation_reason = format!(
                            "Action matched approval rule (tool=`{}`, action={})",
                            pending.matched_rule.tool, rule_action
                        );
                        confirmation_reasons.push(confirmation_reason);
                    }
                }
            }

            let criticality = CriticalityEvaluator::new().evaluate(&first_candidate);
            if criticality.requires_human_confirmation() {
                confirmation_reasons.push(format!(
                    "Action has {:?} criticality (page_context: {:?}) which requires human confirmation",
                    criticality, first_candidate.page_context_hint
                ));
            } else if llm_requires_confirmation {
                confirmation_reasons
                    .push("LLM marked this action as requiring confirmation".to_string());
            }

            if !confirmation_reasons.is_empty() {
                let history_start = history.iterations.len();
                let lineage = begin_agentic_tool_lineage(
                    ctx,
                    executors,
                    Some((iteration, Phase::Apply)),
                    &history,
                    iteration,
                    0,
                    &mut loop_protective.agentic_tool_repeat_counts,
                );
                let action_summary = if confirmation_reasons
                    .iter()
                    .any(|reason| reason.contains("approval rule"))
                {
                    "Execute action (approval required; confirmation required)"
                } else {
                    "Execute action (confirmation required)"
                };
                let reason = format!(
                    "{}\n\n**Action**: {}\n**Reasoning**: {}",
                    confirmation_reasons.join("\n"),
                    first_candidate.action.action_type_name(),
                    first_candidate.reasoning
                );
                warn!(
                    "[EXECUTOR] Action blocked pending confirmation: {} ({})",
                    first_candidate.action.action_type_name(),
                    confirmation_reasons.join(" | ")
                );
                let outcome = build_action_confirmation_outcome(
                    ctx,
                    executors,
                    Some((iteration, Phase::Apply)),
                    iteration,
                    &observed_state,
                    &history,
                    &first_candidate.action,
                    action_json,
                    format!(
                        "{} {}",
                        action_summary,
                        first_candidate.action.action_type_name()
                    ),
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
                .heap_boxed().await;
                let _ = finish_agentic_tool_lineage(
                    ctx,
                    executors,
                    Some((iteration, Phase::Apply)),
                    &history,
                    history_start,
                    lineage,
                    None,
                    false,
                    &mut loop_protective.agentic_failed_tool_fingerprints,
                    &[],
                );
                return Ok(ExecuteGate::Settled(return_outcome(outcome)));
            }
        }

        let session_id = session_id_for_execution(ctx.execution_id.as_deref());

        // Runtime validation: reject actions not in the allowlist.
        if let Some(rejection) =
            validate_action_against_allowlists(&first_candidate.action, ctx)
        {
            warn!("[EXECUTOR] Action rejected by allowlist: {}", rejection);
            let rejection_history_start = history.iterations.len();
            let rejection_lineage = begin_agentic_tool_lineage(
                ctx,
                executors,
                Some((iteration, Phase::Apply)),
                &history,
                iteration,
                0,
                &mut loop_protective.agentic_tool_repeat_counts,
            );
            history.iterations.push(IterationRecord {
                iteration,
                state_before: EnvironmentState::Uninitialized,
                action: first_candidate.action.clone_for_retention(),
                result: ActionResultRecord {
                    success: false,
                    output: Some(rejection.clone()),
                    error: Some("ActionNotAllowed".to_string()),
                    duration_ms: 0,
                    outcome_category: Some(ActionOutcomeCategory::Failed),
                    api_replay_used: None,
                    api_replay_time_ms: None,
                    browser_fallback_reason: None,
                    tool_result_projection: None,
                },
                state_after: EnvironmentState::Uninitialized,
                timestamp: Utc::now(),
                verification: None,
                llm_reasoning: Some(rejection),
            });
            if let Some(lineage) = finish_agentic_tool_lineage(
                ctx,
                executors,
                Some((iteration, Phase::Apply)),
                &history,
                rejection_history_start,
                rejection_lineage,
                None,
                false,
                &mut loop_protective.agentic_failed_tool_fingerprints,
                &[],
            ) {
                pending_agentic_tool_lineages.push(lineage);
            }
            return Ok(ExecuteGate::Settled(ExecutePathControl::Continue));
        }

        let action_history_start = history.iterations.len();
        let first_tool_lineage = begin_agentic_tool_lineage(
            ctx,
            executors,
            Some((iteration, Phase::Apply)),
            &history,
            iteration,
            0,
            &mut loop_protective.agentic_tool_repeat_counts,
        );
        // ═══════════════════════════════════════════════════════════
        // THE GATE — the WHOLE in-turn batch, before any of it fires
        // ═══════════════════════════════════════════════════════════
        //
        // Every dispatch this turn will make is admitted here, in one pass,
        // and none of them has run when this function returns. That is what
        // makes the driver's commit an *intent*: it happens after this and
        // before `dispatch`.
        //
        // # It used to be three passes, interleaved with the fires
        //
        // The primary was gated here, the parallel slice immediately before
        // `join_all`, and each sequential follow-up immediately before its own
        // dispatch. That was correct for a phase with no seam — every member
        // was still admitted before it fired — and it is unusable for one with
        // a seam, because the second and third passes happen after the first
        // dispatch has already left the process.
        //
        // Hoisting is sound because **nothing the gate reads changes while the
        // batch runs**, which was checked rather than assumed:
        //
        // - `effect_id_for_candidate` and `gate_arguments_fingerprint` both read
        //   `history.assistant_turns.last()`. `record_assistant_turn` is called
        //   from `phases::resolve` and nowhere else on the production path;
        //   dispatches append to `history.iterations` and `history.artifacts`.
        // - `reconcile_ref_for_dispatch` reads `ctx.principal`, `ctx.workspace`,
        //   `executors.artifact_v2_service` and the candidate's own
        //   `resolved_params`. None of the four moves inside `Apply`.
        // - The parallel/sequential split is pure:
        //   `is_parallelizable_read_only_action` over the primary and
        //   `in_turn_follow_up_may_join` over the follow-ups, neither of which
        //   consults a result.
        //
        // # Two things the hoist changed, both deliberate
        //
        // **A follow-up whose outward record cannot be named now stops the
        // primary from firing**, where before it stopped the tail after the
        // primary had gone out. That is the intended direction: if the ref
        // cannot be named the intent cannot be recorded, so that member could
        // never be reconciled — and refusing to start a batch one of whose
        // members is unrecoverable is better than discovering it halfway.
        //
        // **The batch names members a bail may stop short of.** They do not
        // stay unsettled: `dispatch` records `EffectOutcome::NotDispatched` for
        // every admitted member it decides against, which is the one outcome
        // that proves an effect did not go out.
        let mut gated = GatedBatch::new();
        let mut members: Vec<GatedMember> = Vec::with_capacity(candidates.candidates.len());

        // Owned rather than borrowed, as it was before the split and for a
        // reason the split did not remove: the dispatch takes `&mut history`,
        // so a borrow held across it would not compile.
        let first_effect_id = effect_id_for_candidate(&history, &first_candidate);
        let first_row = gate_pending_effect(
            ctx,
            executors,
            &history,
            &first_candidate,
            first_effect_id.as_deref(),
            effect_identity,
        )?;
        gated.admit(first_row.clone(), iteration, committed_batch);
        members.push(GatedMember {
            candidate: first_candidate.clone(),
            effect_id: first_effect_id,
            row: first_row,
        });

        // The in-turn split, decided here because the gate needs it in order to
        // admit the parallel members AS a slice — `BatchMode::Parallel` carries
        // the count, and a count computed in one function and acted on in
        // another is two decisions that can differ.
        let follow_ups: Vec<ActionCandidate> =
            candidates.candidates.iter().skip(1).cloned().collect();
        let parallel_n = if is_parallelizable_read_only_action(&first_candidate.action) {
            follow_ups
                .iter()
                .take_while(|follow_up| in_turn_follow_up_may_join(&follow_up.action, executors))
                .count()
        } else {
            0
        };

        // The slice is admitted only for capabilities on
        // `is_parallelizable_read_only_pack`'s closed list, and none of those is
        // on either outward table today, so every member here classifies
        // `NotOutward` and commits `None`. That is a fact about two lists
        // maintained independently of each other, not a property of the code,
        // which is why the gate runs over them anyway.
        let mut parallel_rows = Vec::with_capacity(parallel_n);
        for follow_up in follow_ups.iter().take(parallel_n) {
            // Each parallel member carries its own identity. These are separate
            // tool calls from the model, not a fan-out of one, so they get
            // distinct ids rather than sub-keys of a shared one. They also mint
            // no lineage record, which is why the id comes from the candidate
            // rather than from lineage state.
            let effect_id = effect_id_for_candidate(&history, follow_up);
            let row = gate_pending_effect(
                ctx,
                executors,
                &history,
                follow_up,
                effect_id.as_deref(),
                effect_identity,
            )?;
            parallel_rows.push(row.clone());
            members.push(GatedMember {
                candidate: follow_up.clone(),
                effect_id,
                row,
            });
        }
        // Between the primary and the tail, because `admit_parallel_slice`
        // counts what it contributed and `BatchMode::Parallel { admitted }` has
        // to name the same partition `members` records.
        gated.admit_parallel_slice(parallel_rows, iteration, committed_batch);

        for follow_up in follow_ups.iter().skip(parallel_n) {
            let effect_id = effect_id_for_candidate(&history, follow_up);
            let row = gate_pending_effect(
                ctx,
                executors,
                &history,
                follow_up,
                effect_id.as_deref(),
                effect_identity,
            )?;
            gated.admit(row.clone(), iteration, committed_batch);
            members.push(GatedMember {
                candidate: follow_up.clone(),
                effect_id,
                row,
            });
        }

        // Annotated for the same reason the dispatch half's tail is: this block
        // is a `Box::pin(async { .. })` whose only other exits are `?`s, and
        // `anyhow::Error` has many `From` impls in this crate for inference to
        // choose between.
        Ok::<ExecuteGate, anyhow::Error>(ExecuteGate::Ready(Box::new(GatedApply {
            members,
            parallel_n,
            thinking,
            session_id,
            has_explicit_confirmation_for_action,
            action_history_start,
            first_tool_lineage,
            observed_state,
            task_state_action_for_decision,
            // Cloned off the out-parameter rather than rebuilt, so the batch a
            // driver writes intents from is the same value `GatedBatch::publish`
            // produced — including the `admitted` count, which is arithmetic
            // that has to agree with what the dispatch actually spawns.
            batch: committed_batch.clone(),
        })))

    })
    .heap_boxed()
    .await?;
    Ok(match gated {
        ExecuteGate::Settled(control) => ApplyGate::Settled(execute_control_to_step(control)?),
        ExecuteGate::Ready(ready) => ApplyGate::Gated(ready),
    })
}

/// Fire the batch [`gate`] admitted, and report what happened to each member.
///
/// The second half of the seam. `intents` is the receipt: a caller cannot reach
/// this function without a value only a driver can mint, so a host cannot
/// dispatch a batch no driver considered. See
/// [`super::super::effects::ApplyIntents`] for the exact strength of that.
///
/// # This function does not gate, and must not start
///
/// Every admission decision was taken before the driver got its turn. A gate
/// added here would be one the intent commit could not have covered — the
/// dispatch that follows it would fire with no durable record that it was
/// attempted, which is the whole failure the seam exists to remove, reintroduced
/// at the one place it would be invisible.
///
/// # What the plan is, and why a plan is admissible where a store is not
///
/// `plan` is what the driver established about each of these members BEFORE the
/// gate ran — from the effect ledger and the outward record — and it is what
/// makes a re-entered `Apply` something other than a second dispatch of the
/// whole batch. `plan_the_batch` reads it once, for every member, before the
/// first fire.
///
/// It is a **value**, and that is the whole of why this phase may hold it. The
/// module contract in `phases/mod.rs` is that a phase's parameter list IS the
/// claim about what the phase may touch — this file's own gate takes
/// `&Option<TrustDispatchGuard>` rather than `&mut` for that reason, because *an
/// unused `&mut` quietly weakens it*. A store, a ledger-writer handle or a
/// callback would let this phase go and **ask** something, at a moment of its
/// own choosing, which is exactly the ordering the gate/dispatch seam takes away
/// from it. A plan can only be read. Nothing on it can be re-queried, refreshed
/// or written back, so it widens no claim — which is the sentence to quote at
/// the next change that wants to thread a store into a phase.
#[allow(clippy::too_many_arguments)]
pub(in crate::magician_v2::execution::agentic) async fn dispatch(
    gated: Box<GatedApply>,
    // The receipt. Unused as a value — nothing branches on it, because a
    // `NoDurableLedger` batch still has to run — and load-bearing as a
    // *parameter*: it is what a caller cannot produce without being a driver.
    intents: ApplyIntents,
    // What the driver already established about these exact members. Also
    // driver-minted, and unlike the receipt it IS branched on, per member. See
    // this function's docs for why a plan may cross into a phase and a store may
    // not, and `plan_the_batch` for what each of `EffectAction`'s four variants
    // costs a member.
    plan: EffectPlan,
    ctx: &mut AgenticContext,
    executors: &Arc<ActionExecutors>,
    history: &mut ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    trust_dispatch_guard: &Option<TrustDispatchGuard>,
    approved_confirmation_actions: &mut Vec<ApprovedConfirmationAction>,
    pending_agentic_tool_lineages: &mut Vec<AgenticToolLineageState>,
    current_state: &mut EnvironmentState,
    cancellation_token: &Option<CancellationToken>,
    execution_start: &Instant,
    ephemeral_scope_id: &str,
    iteration: usize,
    // What happened to each member, for the driver to record. An out-parameter
    // for the same reason `committed_batch` is one, and the reason is sharper
    // here: a dispatch that errored mid-batch is exactly the case whose partial
    // results a driver must not lose, and an `Err` return cannot carry them.
    // Written as each member settles, so it is complete on every exit path.
    settled: &mut Vec<(EffectId, EffectOutcome)>,
) -> Result<PhaseStep<()>> {
    debug!(
        iteration,
        members = gated.members.len(),
        recorded_intents = ?intents.recorded_intents(),
        planned_effects = ?plan.planned_effects(),
        "[APPLY] dispatching a gated batch"
    );
    // THE PLAN, read for the WHOLE batch before anything fires.
    //
    // Before the destructure and before the first `execute_direct_path_*`, which
    // is the fail-closed placement: `plan_the_batch` refuses a plan that does
    // not describe this batch, and that refusal has to happen while the batch is
    // still entirely un-fired. Computing it member by member as the dispatch
    // walked would fire the members ahead of the refusal and then stop.
    let dispositions = plan_the_batch(&plan, &gated.members)?;
    let GatedApply {
        members,
        parallel_n,
        thinking,
        session_id,
        has_explicit_confirmation_for_action,
        action_history_start,
        first_tool_lineage,
        observed_state,
        task_state_action_for_decision,
        // The batch was the DRIVER's copy: it wrote an intent for every member
        // before this function was called. Nothing here reads it back — the
        // members carry their own rows — and destructuring it explicitly rather
        // than with `..` is what makes that a decision rather than an omission.
        batch: _,
    } = *gated;
    // Bound to the names the dispatch body already used for them, so the half
    // that moved reads as the code it was rather than as a rewrite.
    let first_candidate = members[0].candidate.clone();
    let first_effect_id = members[0].effect_id.clone();
    // What the gate minted for this member, carried to the dispatch so the
    // handler journals the SAME invocation id the driver just wrote into this
    // effect's intent. Read off the row rather than re-derived, which is the
    // whole reason the row holds it — see `pending_effect_row`.
    let first_reattach_ref = member_reattach_ref(&members[0]);
    // THE SESSION THE DRIVER SAID TO RESUME, for this member only.
    //
    // Owned rather than borrowed out of `dispositions` for the same reason
    // `first_effect_id` is owned: the dispatch below crosses
    // `execute_direct_path_on_scheduler_root`'s scheduler-root spawn, and a
    // borrow of this function's locals cannot cross it. `None` for every member
    // whose verdict is an ordinary `Fire`, which is what makes the absence mean
    // *start a fresh session* rather than *resume something unnamed*.
    let first_resume_session_id = dispositions[0].resume_session_id().map(str::to_string);

    let execute_path_control: ExecutePathControl = Box::pin(async {

        // THE PRIMARY'S PLAN VERDICT, and the batch walks on either way.
        //
        // A skipped primary does NOT end the turn. The follow-ups below are
        // still owed — one of them may be the member that genuinely is — so a
        // skip answers `Continue` and the batch continues exactly as a primary
        // that succeeded would. Returning here instead would silently defer
        // every follow-up of a batch whose first member happened to be the one
        // already settled.
        //
        // `first_tool_lineage` is dropped rather than finished. The gate began
        // it, and that call has already debited `agentic_tool_repeat_counts`;
        // `finish_agentic_tool_lineage` is what WRITES the record. A member that
        // did not run has nothing to write, and finishing it with a fabricated
        // error or a fabricated success would put a tool call that never
        // happened into this run's lineage.
        let direct_path_control = if !dispositions[0].fires() {
            settle_member_not_fired(settled, &members[0], &dispositions[0]);
            record_member_not_fired_in_history(
                history,
                &members[0],
                &dispositions[0],
                &observed_state,
                current_state,
                iteration,
            );
            debug!(
                iteration,
                "[APPLY] the primary member of this batch was answered by the driver's effect \
                 plan and was not dispatched"
            );
            let _unfinished_lineage = first_tool_lineage;
            ExecutePathControl::Continue
        } else {

        let direct_path_result = execute_direct_path_on_scheduler_root(
            ctx,
            &observed_state,
            history,
            iteration,
            executors,
            Some((iteration, Phase::Apply)),
            &first_candidate,
            &thinking,
            &execution_start,
            &session_id,
            trust_dispatch_guard.as_ref(),
            current_state,
            first_effect_id.as_deref(),
            first_reattach_ref.as_deref(),
            first_resume_session_id.as_deref(),
            loop_protective,
            &approved_confirmation_actions,
            ephemeral_scope_id,
            &cancellation_token,
        )
        .await;
        if let Some(lineage) = finish_agentic_tool_lineage(
            ctx,
            executors,
            Some((iteration, Phase::Apply)),
            &history,
            action_history_start,
            first_tool_lineage,
            direct_path_result.as_ref().err(),
            has_explicit_confirmation_for_action,
            &mut loop_protective.agentic_failed_tool_fingerprints,
            &[],
        ) {
            pending_agentic_tool_lineages.push(lineage);
        }
        // OUTCOME for the primary. Read before the match below consumes the
        // result, and through the same helper every other member uses so the
        // three answers cannot be spelled differently per site.
        record_member_outcome(
            settled,
            &members[0],
            dispatched_outcome(
                direct_path_result.as_ref().err(),
                in_turn_slice_failed(&history, action_history_start),
                history
                    .iterations
                    .get(action_history_start..)
                    .and_then(|records| records.last())
                    .map(|record| &record.result),
            ),
        );
        // The tail of the `else` opened at the plan verdict above: this match is
        // what the branch evaluates to, so a fired primary and a skipped one
        // reach the follow-up walk holding the same kind of value.
        match direct_path_result {
            Ok(control) => *control,
            // ~~KNOWN HAZARD, unfixed here because the fix is not in this
            // file.~~ **FIXED 2026-08-28, in the place this note named.**
            //
            // `execute_direct_path_on_scheduler_root` takes `history`,
            // `current_state` and `loop_protective` by `&mut` and moves all
            // three into the scheduled job. Its two `?`s — `schedule(..)` on a
            // closed worker channel, `.await` on a dropped oneshot — used to
            // return between the move and the swap-back **without
            // reconciling**, so this arm received un-reconciled placeholders:
            // an empty history, `observed_state` as the current state, and a
            // `LoopProtectiveState::default()`. Handed to
            // `finish_cancelled_agentic_execution` alongside a manual pause,
            // they were persisted as a `PausedByUser` — and the run resumed
            // with loop detection, retry/abort budgets and accumulated spend
            // all reset.
            //
            // The state now travels in a shared cell rather than by move, so it
            // survives both early returns: the job puts it back before
            // returning, and the caller reconciles from the cell on every path.
            // **The `schedule` path needed that, not an extra swap** — the
            // state had been moved INTO the closure, so a refusal dropped the
            // closure and destroyed it. There was nothing left to reconcile
            // from, which is also why the guard this note declined would not
            // have worked: the data was already gone by the time this arm saw
            // the error.
            Err(_error)
                if cancellation_token
                    .as_ref()
                    .map(CancellationToken::is_cancelled)
                    .unwrap_or(false) =>
            {
                return_outcome(
                    finish_cancelled_agentic_execution(
                        ctx,
                        executors,
                        Some((iteration, Phase::Apply)),
                        current_state.clone(),
                        &history,
                        &execution_start,
                        iteration,
                        iteration,
                        executors.manual_pause_requested(),
                        &loop_protective,
                        &approved_confirmation_actions,
                    )
                    .heap_boxed().await,
                )
            },
            Err(error) => {
                return Err(settle_unreached_before_error(
                    settled,
                    &members,
                    &dispositions,
                    history,
                    &observed_state,
                    current_state,
                    iteration,
                    1,
                    "the primary dispatch errored before any follow-up was reached",
                    error,
                ));
            },
        }
        };
        // ═══════════════════════════════════════════════════════════
        // MULTI-TOOL-PER-TURN: the model may emit several tool calls in
        // one turn (e.g. act → verify-read → act). `lower_native_response`
        // folds the consecutive leading Execute calls into this batch;
        // the first ran above. Run the remaining candidates in order
        // against the same observed snapshot, feeding each result into
        // `history` (which the next decision turn — chained by
        // `previous_response_id` — sees). Bail on the first follow-up
        // that fails, needs confirmation, or yields a terminal outcome;
        // the model re-issues whatever's left next turn with the full
        // results in context. Mirrors the deleted inner loop's
        // `for tool_call in response.tool_calls` + `--bail` discipline.
        // ═══════════════════════════════════════════════════════════
        let first_failed = in_turn_slice_failed(&history, action_history_start);
        if first_failed && members.len() > 1 {
            debug!(
                "[MULTI-TOOL] Stopping in-turn batch after candidate #1 failed; \
                 remaining follow-ups stay deferred."
            );
        }
        let direct_path_control = if matches!(
            direct_path_control,
            ExecutePathControl::Continue
        ) && members.len() > 1
            && !first_failed
        {
            let mut control = direct_path_control;
            // How far the batch has been settled. Members below this index have
            // an outcome recorded; members at or above it do not, and whatever
            // is still above it when the tail stops is settled as never
            // dispatched. Kept as one number rather than a per-member flag
            // because the batch only ever stops at a prefix boundary.
            let mut settled_through = 1usize;
            // Where the sequential tail stops, and why. The parallel prefix can
            // pull the stop down to the tail's own start.
            let mut tail_stop = members.len();
            let mut deferred_reason: &str =
                "this turn's batch ended before this member was reached";
            // `parallel_n` and the partition it names were decided by the gate,
            // which is where they had to move: the gate needs the split in order
            // to admit the slice as a slice, and deciding it twice is how the
            // committed `BatchMode::Parallel { admitted }` and the members this
            // function actually spawns come to disagree.
            if parallel_n > 0 {
                // ALREADY GATED, by `gate_execute`, along with every other member
                // of this batch. The gate used to run here — a pass over the whole
                // slice before any of it was spawned, because these members settle
                // out of order and there is no per-member moment afterwards at
                // which one could be gated. That reasoning still holds; what
                // changed is that it now holds for the sequential members too,
                // and for the same reason: after the seam, *no* member has a
                // pre-fire moment inside this function.
                let ctx_shared: &AgenticContext = ctx;
                let cancel_token = cancellation_token.clone();
                let trust_guard = trust_dispatch_guard.as_ref();
                // THE SLICE'S PLAN VERDICTS, settled before the join rather than
                // after it. A member the driver already answered is settled here
                // and never spawned; the rest keep their positions.
                //
                // `spawned` is what maps a join slot back to a member. `join_all`
                // yields in INPUT order, so slot `k` is `spawned[k]` — but slot
                // `k` is no longer member `1 + k` once anything is filtered out,
                // and reading it as if it were would fold one member's history
                // and protective state into another member's row.
                let mut spawned: Vec<usize> = Vec::with_capacity(parallel_n);
                // One history slot per admitted position. Recovery can answer a
                // member from the durable ledger while an EARLIER sibling still
                // has to run. Writing the adopted result straight into `history`
                // here would put it ahead of that earlier sibling, because the
                // fired histories do not come back until after `join_all`.
                //
                // Tool-result order is causal input, not presentation order: a
                // provider associates these results with the assistant calls in
                // their admitted order. Stage both the skipped and fired members
                // by member index, then merge the whole slice once, in order.
                // Slot zero is unused because the primary was handled above.
                let mut parallel_histories: Vec<Option<ExecutionHistory>> =
                    (0..=parallel_n).map(|_| None).collect();
                // A recovered member can be skipped because the durable ledger
                // already holds its FAILED result. It is still a failed sibling
                // for `--bail`: recovery changes whether the call is sent, not
                // the control-flow meaning of its result. Keep this separately
                // because only fired members appear in `parallel_outputs`.
                let mut adopted_parallel_failed = false;
                for index in 1..=parallel_n {
                    if dispositions[index].fires() {
                        spawned.push(index);
                    } else {
                        adopted_parallel_failed |= dispositions[index].recorded_failure();
                        settle_member_not_fired(settled, &members[index], &dispositions[index]);
                        let mut member_history = ExecutionHistory::new();
                        record_member_not_fired_in_history(
                            &mut member_history,
                            &members[index],
                            &dispositions[index],
                            &observed_state,
                            current_state,
                            iteration,
                        );
                        parallel_histories[index] = Some(member_history);
                    }
                }
                // Each parallel member carries its own identity. These
                // are separate tool calls from the model, not a fan-out
                // of one, so they get distinct ids rather than sub-keys
                // of a shared one. They also mint no lineage record,
                // which is why the id comes from the candidate rather
                // than from lineage state.
                let parallel_jobs = spawned
                    .iter()
                    .map(|index| {
                    let member = &members[*index];
                    let follow_up = member.candidate.clone();
                    let thinking_owned: String =
                        if follow_up.reasoning.trim().is_empty() {
                            thinking.clone()
                        } else {
                            follow_up.reasoning.clone()
                        };
                    let before_state = observed_state.clone();
                    // DELIBERATELY DROPPED, unlike `scratch_history` and
                    // `scratch_protective`, which the join folds back.
                    // `execute_direct_path_post_setup_tail` ends with
                    // `*current_state = state_after`, so each member comes back
                    // holding its own post-action observation of a *shared*
                    // environment, taken at a moment the other members were also
                    // acting. There is no "the" state to adopt: the members
                    // settle out of order, so folding one would install an
                    // arbitrary member's snapshot over the one the first
                    // (sequential) candidate already wrote into `current_state`,
                    // and folding the last would make the run's state depend on
                    // scheduler order. The slice is admitted only for read-only
                    // capabilities, so the snapshot it would overwrite with is
                    // the one the run already has.
                    //
                    // Dropping it is therefore a choice and not the oversight the
                    // artifact drop below was. If a mutating action is ever
                    // admitted to the parallel slice, this stops being safe and
                    // the answer is not to pick a winner here — it is to record
                    // the members' state serially after the join.
                    let mut scratch_state = before_state.clone();
                    // One scratch bundle instead of three loose clones. The
                    // parallel slice is admitted only for preauthorized,
                    // non-HITL actions, so it cannot pause — but it still
                    // carries the real approvals rather than an empty slice,
                    // so the invariant does not depend on that admission rule
                    // staying true.
                    let mut scratch_protective = loop_protective.clone();
                    let scratch_approvals = approved_confirmation_actions.clone();
                    // The GATE's id for this member, for the reason spelled out
                    // at the sequential tail's own id.
                    let scratch_effect_id = member.effect_id.clone();
                    let scratch_reattach_ref = member_reattach_ref(member);
                    // Indexed by `*index` and NOT by the slice position: the
                    // verdicts are index-parallel to `members`, and `spawned`
                    // has already dropped the members the plan answered, so
                    // reading this off a join slot would hand one member's
                    // resume handle to another member's coding job.
                    let scratch_resume_session_id =
                        dispositions[*index].resume_session_id().map(str::to_string);
                    let scope_id = ephemeral_scope_id.to_string();
                    let session_id_owned = session_id.clone();
                    let cancel_token = cancel_token.clone();
                    async move {
                        let mut scratch_history = ExecutionHistory::new();
                        let result = execute_direct_path_on_scheduler_root(
                            ctx_shared,
                            &before_state,
                            &mut scratch_history,
                            iteration,
                            executors,
                            // Every member of this slice journals under the same
                            // `(iteration, Apply)`. Their ORDINALS are the
                            // unstable part — `join_all` buffers them in
                            // COMPLETION order — and **nothing repairs that
                            // today**.
                            //
                            // Corrected 2026-08-28. This said the instability
                            // was "`commit_boundary`'s to repair, not this
                            // call's", which named a repair that does not
                            // exist: `driver_worker::commit_boundary` stamps
                            // `ordinal: index` over `report.records` in the
                            // order it was handed them and sorts nothing, so it
                            // preserves completion order rather than fixing it.
                            // `run_loop::phases::outbox`'s *AND THE ORDER OFF
                            // THAT LANE IS NOT REPRODUCIBLE* says the same
                            // thing from the other end — this lane is "the only
                            // one a driver-side ordinal cannot repair on its
                            // own" — so the two comments were a mechanism with
                            // two stories, one of which told a reader the
                            // problem was already somebody else's.
                            //
                            // What it costs, unrepaired: the same event is
                            // addressed `(i, Apply, 3)` on one attempt and
                            // `(i, Apply, 5)` on a re-attempt, so the
                            // projector's dedupe re-emits the pair that misses
                            // its window and silently drops the pair that
                            // collides. The repair is per-member sub-addresses
                            // or a member-stable sort in the drain; see the
                            // note on `at` in
                            // `execute_direct_path_on_scheduler_root`, which
                            // carries the owner-tracked version of this gap.
                            Some((iteration, Phase::Apply)),
                            &follow_up,
                            &thinking_owned,
                            &execution_start,
                            &session_id_owned,
                            trust_guard,
                            &mut scratch_state,
                            scratch_effect_id.as_deref(),
                            scratch_reattach_ref.as_deref(),
                            scratch_resume_session_id.as_deref(),
                            &mut scratch_protective,
                            &scratch_approvals,
                            scope_id.as_str(),
                            &cancel_token,
                        )
                        .await
                        .map(|control| *control);
                        // The fork comes back. It is `&mut` in and swapped out
                        // by `execute_direct_path_on_scheduler_root`, so what
                        // this member's dispatch recorded is in here and nowhere
                        // else — see `adopt_parallel_member_protective_state`.
                        (scratch_history, scratch_protective, result)
                    }
                });
                let parallel_outputs = join_all(parallel_jobs).await;
                let mut parallel_failed = adopted_parallel_failed;
                let mut cancelled = false;
                let mut first_error = None;
                let mut member_protective: Vec<LoopProtectiveState> =
                    Vec::with_capacity(parallel_outputs.len());
                for (slot, (scratch_history, scratch_protective, result)) in
                    parallel_outputs.into_iter().enumerate()
                {
                    // WHICH MEMBER THIS SLOT IS. `join_all` yields in input
                    // order, and the input is `spawned` — the members the plan
                    // said to fire — so slot `k` is `spawned[k]` and NOT
                    // `1 + k`. It was `1 + k` while every member was spawned;
                    // reading it that way now would fold one member's history,
                    // protective state and outcome into another member's row the
                    // moment anything in the slice is skipped.
                    let member_index = spawned[slot];
                    // OUTCOME, per member, taken from that member's OWN fork and
                    // taken here rather than after the join, because the records
                    // are merged into the run's history a few lines below and
                    // after that there is no way back to which record was whose.
                    // This is the last moment the question can be asked.
                    record_member_outcome(
                        settled,
                        &members[member_index],
                        dispatched_outcome(
                            result.as_ref().err(),
                            scratch_history
                                .iterations
                                .iter()
                                .any(|record| !record.result.success),
                            scratch_history.iterations.last().map(|record| &record.result),
                        ),
                    );
                    // ONLY an `Ok` member's fork is evidence, and the guard is
                    // load-bearing rather than tidy.
                    // `execute_direct_path_on_scheduler_root` moves the bundle
                    // into the scheduled job with `std::mem::take` and swaps it
                    // back **after** the job returns (`executor.rs` 18072 /
                    // 18127). Two `?`s sit between those points — `.schedule(..)`
                    // when the scheduler-root worker channel is closed, and
                    // `.await` when the worker dropped the oneshot — and neither
                    // reconciles. A member that failed to schedule therefore
                    // hands back `LoopProtectiveState::default()`: an
                    // un-reconciled placeholder, not a fork that did anything.
                    //
                    // Folding that placeholder is worse than dropping it, because
                    // `adopt_parallel_member_protective_state` reads it as
                    // evidence in both directions — `loop_recovery_context: None`
                    // reads as "this member cleared the pressure signal", and
                    // `recorded_len() == 0 < base_len` reads as "this member
                    // proved a clear", which replaces the run's whole
                    // `LoopDetector` with an empty one. On the cancellation arm
                    // below that state can then be **persisted**:
                    // `finish_cancelled_agentic_execution` takes
                    // `&loop_protective`, and when `manual_pause_requested()` is
                    // set it builds `PausedByUser { pause_state:
                    // build_full_pause_state(.., loop_protective, ..) }` and
                    // writes it through `persist_paused_execution_summary`. The
                    // run then resumes with loop detection reset — verbatim the
                    // failure that parameter's own doc says it exists to
                    // prevent. That arm does not return early either: the
                    // `Err(_) if is_cancelled` guard leaves `first_error` unset.
                    //
                    // Dropping an errored member's fork loses at most one
                    // observation, which is the direction this fold is already
                    // allowed to err in (see
                    // `adopt_parallel_member_protective_state`) and is exactly
                    // what happened for every member before 2026-08-27.
                    if result.is_ok() {
                        member_protective.push(scratch_protective);
                    }
                    if scratch_history
                        .iterations
                        .iter()
                        .any(|record| !record.result.success)
                    {
                        parallel_failed = true;
                    }
                    // Keep this member's history under its admitted position.
                    // Appending it here would put every plan-adopted member
                    // (staged before the join) ahead of every fired member,
                    // regardless of the original tool-call order.
                    parallel_histories[member_index] = Some(scratch_history);
                    match result {
                        Ok(follow_up_control) => match follow_up_control {
                            ExecutePathControl::Continue => {},
                            ExecutePathControl::Return(outcome) => {
                                if matches!(control, ExecutePathControl::Continue) {
                                    control = ExecutePathControl::Return(outcome);
                                }
                            },
                        },
                        Err(_error)
                            if cancellation_token
                                .as_ref()
                                .map(CancellationToken::is_cancelled)
                                .unwrap_or(false) =>
                        {
                            cancelled = true;
                        },
                        Err(error) => {
                            if first_error.is_none() {
                                first_error = Some(error);
                            }
                        },
                    }
                }
                merge_parallel_member_histories(history, &mut parallel_histories);
                // Every member of the slice now has an outcome, so the tail
                // starts above it. Advanced here rather than at the loop's top:
                // the loop settles each member as it folds it, and a value moved
                // before the fold would name members whose outcome had not been
                // recorded yet if a `?` left in between.
                settled_through = 1 + parallel_n;
                // BEFORE the error return AND before the cancellation arm, not
                // after either: a sibling that errored must not discard what the
                // members that finished recorded, and the cancellation arm
                // persists `loop_protective` into the pause it writes.
                //
                // `member_protective` holds only the members whose dispatch
                // returned `Ok` — see the guard in the loop above.
                adopt_parallel_member_protective_state(loop_protective, member_protective);
                if let Some(error) = first_error {
                    return Err(settle_unreached_before_error(
                        settled,
                        &members,
                        &dispositions,
                        history,
                        &observed_state,
                        current_state,
                        iteration,
                        settled_through,
                        "a parallel member errored before the sequential tail was reached",
                        error,
                    ));
                }
                if cancelled {
                    control = return_outcome(
                        finish_cancelled_agentic_execution(
                            ctx,
                            executors,
                            Some((iteration, Phase::Apply)),
                            current_state.clone(),
                            &history,
                            &execution_start,
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
                if should_skip_serial_follow_ups(
                    matches!(control, ExecutePathControl::Continue),
                    parallel_failed,
                ) {
                    debug!(
                        "[MULTI-TOOL] Stopping mutating tail after parallel prefix: \
                         HITL, cancel, or a failed sibling; remaining follow-ups stay deferred."
                    );
                    tail_stop = settled_through;
                    deferred_reason = "the parallel prefix ended the turn or a sibling \
                                       failed, so the mutating tail stayed deferred";
                }
            }
            for member_index in settled_through..tail_stop {
                // THIS MEMBER'S PLAN VERDICT, ahead of every other stop
                // condition in this loop.
                //
                // Ahead of them deliberately: the `--bail`, the work budget, the
                // allowlist and the confirmation check all `break`, which leaves
                // the rest of the tail to `record_members_not_dispatched`. A
                // member the driver already answered must not be re-decided by
                // any of those — it is neither owed a fire nor deferred, it is
                // done — so it is settled and stepped over while the tail
                // continues to the members that are genuinely owed.
                if !dispositions[member_index].fires() {
                    settle_member_not_fired(
                        settled,
                        &members[member_index],
                        &dispositions[member_index],
                    );
                    record_member_not_fired_in_history(
                        history,
                        &members[member_index],
                        &dispositions[member_index],
                        &observed_state,
                        current_state,
                        iteration,
                    );
                    // Advanced with the settle and not after it, exactly as the
                    // fired path below does: whatever is still above
                    // `settled_through` when this loop stops is what gets
                    // `NotDispatched`, and this member has its answer already.
                    settled_through = member_index + 1;
                    continue;
                }
                let follow_up = members[member_index].candidate.clone();
                // 1-based position in the batch (the first candidate ran already).
                let position = member_index + 1;

                // Bail if the previous in-turn action failed (`--bail`).
                let previous_failed = history
                    .iterations
                    .last()
                    .map(|record| !record.result.success)
                    .unwrap_or(false);
                if previous_failed {
                    debug!(
                        "[MULTI-TOOL] Stopping in-turn batch before candidate #{position}: \
                         previous action failed; model re-issues with results in context."
                    );
                    break;
                }

                // The provider emitted this call in the same response,
                // but it is still a distinct operation. If the first
                // tool crossed the soft boundary, do not let batching
                // bypass the rule that no subsequent work may start.
                if context_work_budget_reached(ctx) {
                    tracing::info!(
                        target: "agentic.work_budget",
                        execution_id = %runtime_execution_id(ctx),
                        iteration,
                        position,
                        limit_secs = ctx.work_budget_secs.unwrap_or_default(),
                        elapsed_ms = context_work_budget_elapsed_ms(ctx),
                        "[AGENTIC-WORK-BUDGET] in-turn follow-up was not started after boundary"
                    );
                    control = return_outcome(
                        conclude_work_budget_reached(
                            ctx,
                            executors,
                            Some((iteration, Phase::Apply)),
                            &history,
                            current_state.clone(),
                            iteration,
                        )
                        .heap_boxed().await,
                    );
                    break;
                }

                // Allowlist validation per follow-up (same gate as the first).
                if let Some(rejection) =
                    validate_action_against_allowlists(&follow_up.action, ctx)
                {
                    warn!(
                        "[MULTI-TOOL] Follow-up candidate #{position} rejected by allowlist: {rejection}"
                    );
                    history.iterations.push(IterationRecord {
                        iteration,
                        state_before: EnvironmentState::Uninitialized,
                        action: follow_up.action.clone_for_retention(),
                        result: ActionResultRecord {
                            success: false,
                            output: Some(rejection.clone()),
                            error: Some("ActionNotAllowed".to_string()),
                            duration_ms: 0,
                            outcome_category: Some(ActionOutcomeCategory::Failed),
                            api_replay_used: None,
                            api_replay_time_ms: None,
                            browser_fallback_reason: None,
                            tool_result_projection: None,
                        },
                        state_after: EnvironmentState::Uninitialized,
                        timestamp: Utc::now(),
                        verification: None,
                        llm_reasoning: Some(rejection),
                    });
                    break;
                }

                // Never auto-run a confirmation-gated action inside a
                // batch. If a follow-up would need confirmation, stop so
                // it is re-issued next turn through the full gate.
                let follow_up_requires_confirmation = {
                    let criticality =
                        CriticalityEvaluator::new().evaluate(&follow_up);
                    criticality.requires_human_confirmation()
                        || follow_up.requires_confirmation.unwrap_or(false)
                        || (!ctx.approval_rules.is_empty() && {
                            let plan = ExecutionPlan {
                                plan_id: ctx
                                    .plan_id
                                    .clone()
                                    .unwrap_or_else(|| format!("iter-{iteration}")),
                                steps: vec![approval_step_from_candidate(
                                    &follow_up,
                                    iteration,
                                    &ctx.goal,
                                )],
                            };
                            let constraints = AgentConstraints {
                                requires_approval: ctx.approval_rules.clone(),
                                ..AgentConstraints::default()
                            };
                            // Deliberately the BARE check, and the
                            // only one left. This is not a gate: it
                            // asks "would the next turn's gate stop
                            // here?" and breaks the batch if so. A
                            // waiver applied here could only make the
                            // batch run further, and the act it would
                            // have waived is re-issued next turn
                            // through `checked_approval`, which waives
                            // it there and debits the envelope once.
                            // Waiving in both places would debit twice
                            // for one act.
                            matches!(
                                ApprovalGate.check(&plan, &constraints),
                                ApprovalResult::NeedsApproval { .. }
                            )
                        })
                };
                if follow_up_requires_confirmation {
                    debug!(
                        "[MULTI-TOOL] Stopping in-turn batch before candidate #{position}: \
                         requires confirmation; deferring to next turn's full gate."
                    );
                    break;
                }

                debug!(
                    "[MULTI-TOOL] Executing in-turn follow-up candidate #{position}: {}",
                    follow_up.action.action_type_name()
                );
                let follow_up_thinking: &String = if follow_up.reasoning.trim().is_empty() {
                    &thinking
                } else {
                    &follow_up.reasoning
                };
                let follow_up_history_start = history.iterations.len();
                let follow_up_lineage = begin_agentic_tool_lineage(
                    ctx,
                    executors,
                    Some((iteration, Phase::Apply)),
                    &history,
                    iteration,
                    position - 1,
                    &mut loop_protective.agentic_tool_repeat_counts,
                );
                // The id the GATE keyed this member's ledger row by, not a
                // second derivation of it. They agreed while both were computed
                // from `history.assistant_turns.last()`, but agreeing by
                // coincidence of purity is not the same as being one value: the
                // row a driver wrote an intent against and the id this dispatch
                // presents to a remote as an idempotency key have to be the same
                // string, and now they are the same `String`.
                let follow_up_effect_id = members[member_index].effect_id.clone();
                let follow_up_reattach_ref = member_reattach_ref(&members[member_index]);
                let follow_up_resume_session_id = dispositions[member_index]
                    .resume_session_id()
                    .map(str::to_string);
                let follow_up_result = execute_direct_path_on_scheduler_root(
                    ctx,
                    &observed_state,
                    history,
                    iteration,
                    executors,
                    Some((iteration, Phase::Apply)),
                    &follow_up,
                    follow_up_thinking,
                    &execution_start,
                    &session_id,
                    trust_dispatch_guard.as_ref(),
                    current_state,
                    follow_up_effect_id.as_deref(),
                    follow_up_reattach_ref.as_deref(),
                    follow_up_resume_session_id.as_deref(),
                    loop_protective,
                    &approved_confirmation_actions,
                    ephemeral_scope_id,
                    &cancellation_token,
                )
                .await;
                if let Some(lineage) = finish_agentic_tool_lineage(
                    ctx,
                    executors,
                    Some((iteration, Phase::Apply)),
                    &history,
                    follow_up_history_start,
                    follow_up_lineage,
                    follow_up_result.as_ref().err(),
                    false,
                    &mut loop_protective.agentic_failed_tool_fingerprints,
                    &[],
                ) {
                    pending_agentic_tool_lineages.push(lineage);
                }
                // OUTCOME. Before the match consumes the result, and from this
                // member's own records — `follow_up_history_start` is the index
                // taken immediately before its dispatch, so the slice above it
                // is exactly what this member produced.
                record_member_outcome(
                    settled,
                    &members[member_index],
                    dispatched_outcome(
                        follow_up_result.as_ref().err(),
                        in_turn_slice_failed(&history, follow_up_history_start),
                        history
                            .iterations
                            .get(follow_up_history_start..)
                            .and_then(|records| records.last())
                            .map(|record| &record.result),
                    ),
                );
                settled_through = member_index + 1;
                match follow_up_result {
                    Ok(follow_up_control) => match *follow_up_control {
                        ExecutePathControl::Continue => continue,
                        ExecutePathControl::Return(outcome) => {
                            control = ExecutePathControl::Return(outcome);
                            deferred_reason =
                                "an earlier member of this turn's batch ended the run";
                            break;
                        },
                    },
                    Err(_error)
                        if cancellation_token
                            .as_ref()
                            .map(CancellationToken::is_cancelled)
                            .unwrap_or(false) =>
                    {
                        control = return_outcome(
                            finish_cancelled_agentic_execution(
                                ctx,
                                executors,
                                Some((iteration, Phase::Apply)),
                                current_state.clone(),
                                &history,
                                &execution_start,
                                iteration,
                                iteration,
                                executors.manual_pause_requested(),
                                &loop_protective,
                                &approved_confirmation_actions,
                            )
                            .heap_boxed().await,
                        );
                        deferred_reason = "the run was cancelled mid-batch";
                        break;
                    },
                    Err(error) => {
                        return Err(settle_unreached_before_error(
                            settled,
                            &members,
                            &dispositions,
                            history,
                            &observed_state,
                            current_state,
                            iteration,
                            settled_through,
                            "a sequential member errored before the rest of the tail was reached",
                            error,
                        ));
                    },
                }
            }
            // Everything the tail did not reach. `NotDispatched` rather than
            // silence: these members hold intents the driver committed before
            // any of this ran, and an intent with no outcome reads as "may
            // have fired".
            record_members_not_dispatched(
                settled,
                &members,
                &dispositions,
                history,
                &observed_state,
                current_state,
                iteration,
                settled_through,
                deferred_reason,
            );
            control
        } else {
            // The follow-ups were admitted by the gate and none of them will be
            // reached — the primary failed, ended the turn, or was the only
            // member. Settling them here is what keeps the ledger's unsettled
            // set equal to the set that may have fired.
            //
            // `dispositions` matters most on THIS branch. It is the one that
            // settles members the tail never even looked at, so without the plan
            // it would stamp `NotDispatched` — *proof it did not go out* — over a
            // member the outward record says already left.
            record_members_not_dispatched(
                settled,
                &members,
                &dispositions,
                history,
                &observed_state,
                current_state,
                iteration,
                1,
                if first_failed {
                    "the first candidate in this turn's batch failed, so the follow-ups \
                     stayed deferred"
                } else {
                    "the first candidate ended this turn, so the follow-ups stayed deferred"
                },
            );
            direct_path_control
        };

        // The outer task-state update belongs to the decision as a
        // whole, not to its first executable call. Persist it only
        // after every emitted call that actually ran succeeded and
        // the batch remains non-terminal. A rejected, confirmation-
        // gated, failed, or terminal follow-up therefore cannot leave
        // behind a misleading task transition.
        //
        // Counted against the members this dispatch was actually asked to fire,
        // not against every member the gate admitted. A re-entered `Apply` whose
        // plan settled two members produces two fewer history records, and the
        // old comparison would read that as "the batch did not complete" — so
        // the decision's task transition would be dropped on exactly the runs
        // that recovered, silently, and only on those.
        let completed_batch = &history.iterations[action_history_start..];
        if matches!(direct_path_control, ExecutePathControl::Continue)
            && completed_batch.len() == members.len()
            && completed_batch
                .iter()
                .all(|record| record.iteration == iteration && record.result.success)
        {
            apply_outer_task_state_action(
                ctx,
                executors,
                &task_state_action_for_decision,
                iteration,
                trust_dispatch_guard.as_ref(),
            )
            .heap_boxed().await;
        }


        Ok::<ExecutePathControl, anyhow::Error>(direct_path_control)
    })
    .heap_boxed()
    .await?;
    execute_control_to_step(execute_path_control)
}

/// Settle an admitted batch when the host observes cancellation or a manual
/// pause at the final boundary before the first dispatch.
///
/// The driver has already published every member's intent and activated the
/// batch marker by this point. Returning a terminal report with an empty
/// `settled` out-parameter would leave those never-attempted effects looking
/// indeterminate until the terminal boundary commit clears the marker. In
/// particular, a failed terminal commit would make recovery reconcile work the
/// host knows it never sent.
///
/// Plan-answered members retain their positive evidence: an adopted row is not
/// overwritten, and an outward-recorded send settles as succeeded. Only a
/// member still licensed to dispatch receives `NotDispatched`.
pub(in crate::magician_v2::execution::agentic) fn settle_before_first_dispatch(
    gated: &GatedApply,
    plan: &EffectPlan,
    settled: &mut Vec<(EffectId, EffectOutcome)>,
    reason: &str,
) -> Result<()> {
    let dispositions = plan_the_batch(plan, &gated.members)?;
    settle_members_before_first_dispatch(&gated.members, &dispositions, settled, reason);
    Ok(())
}

fn settle_members_before_first_dispatch(
    members: &[GatedMember],
    dispositions: &[MemberDispatch],
    settled: &mut Vec<(EffectId, EffectOutcome)>,
    reason: &str,
) {
    let at_ms = Utc::now().timestamp_millis();
    for (index, member) in members.iter().enumerate() {
        match dispositions.get(index) {
            Some(MemberDispatch::Fire)
            | Some(MemberDispatch::FireResumingSession { .. })
            | None => record_member_outcome(
                settled,
                member,
                EffectOutcome::NotDispatched {
                    at_ms,
                    reason: reason.to_string(),
                },
            ),
            Some(disposition) => settle_member_not_fired(settled, member, disposition),
        }
    }
}

/// The one mapping from the `Execute` arm's control value to a phase step.
///
/// One function rather than one copy in each half of the seam. Both halves can
/// finish an `Execute` arm — `gate` when it decides before admitting anything,
/// `dispatch` when the batch is done — and a second spelling is how the two come
/// to disagree about what `Continue` means. It is also what keeps this file's
/// boundary-exit population at the number `outcome.rs::PHASE_SOURCES` pins —
/// two copies of this match would be sixteen exits where that table says
/// fifteen. The spelling is deliberately not repeated in this sentence: the
/// gate counts occurrences in the whole file, comments included.
fn execute_control_to_step(control: ExecutePathControl) -> Result<PhaseStep<()>> {
    match control {
        // EXIT: NextIteration — the execute path asked to continue
        ExecutePathControl::Continue => PhaseStep::exits(BoundaryOutcome::NextIteration),
        ExecutePathControl::Return(outcome) => Ok(PhaseStep::Return(outcome)),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// The plan: what a re-entered Apply may fire
// ════════════════════════════════════════════════════════════════════════════

/// What [`dispatch`] does about one member the gate admitted.
///
/// The per-member reading of `effects::EffectPlan`, computed for the WHOLE batch
/// before the first fire. Computed up front rather than member by member because
/// [`plan_the_batch`]'s refusal has to be fail-closed: a plan that describes a
/// different turn must not fire the members ahead of the mismatch and then stop.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MemberDispatch {
    /// Fire it. Either the driver licensed a re-fire, or it said nothing about
    /// this member at all — which for a member the gate just admitted means no
    /// ledger row claims it fired, so nothing licenses skipping it.
    Fire,
    /// Do not fire: a result is already recorded for this effect.
    ///
    /// Settles **nothing**. `EffectDisposition::Adopt` is reached precisely
    /// because the row already carries an outcome, and writing a second one
    /// would put this attempt's guess over a real answer.
    AdoptRecorded {
        result: Option<serde_json::Value>,
        succeeded: bool,
    },
    /// Do not fire: the outward record says the act left. Settle against it.
    ///
    /// `by_this_attempt` is carried for the settle's reason string. It does not
    /// change the verdict — for deciding whether to send, *the effect exists* is
    /// the whole of it, which is what `ReconciledEffect::AlreadyFired` says.
    SettleAsAlreadyFired { by_this_attempt: bool },
    /// Fire it, but as a RESUME of the session it already started — never as a
    /// fresh job.
    ///
    /// The dispatch answer to [`EffectAction::Reattach`], and the distinction
    /// from [`Self::Fire`] is the whole rule: both reach the executor, and only
    /// this one carries the `native_session_id` the driver resolved out of the
    /// effect ledger. It travels to the handler as the runtime-stamped
    /// `__coding_resume_session_id` parameter — see `executor.rs`'s stamp beside
    /// `__coding_invocation_id`, and `compiled_handlers::run_coding_task`'s bind
    /// ahead of `resolve_previous_chain_continuation`. Dispatching this member
    /// as a plain `Fire` would start a SECOND engine session against the same
    /// repository, which is the double-run the reattach rule exists to prevent.
    FireResumingSession { native_session_id: String },
}

impl MemberDispatch {
    /// Whether the executor is reached for this member at all.
    ///
    /// Two of the four variants reach it, and every site that used to spell this
    /// `matches!(.., MemberDispatch::Fire)` must ask it here instead. The one
    /// that forgot would settle a resuming member as un-fired while the dispatch
    /// below it fired the member anyway — two records of one act, disagreeing.
    fn fires(&self) -> bool {
        matches!(self, Self::Fire | Self::FireResumingSession { .. })
    }

    /// The live session this member must resume, when its verdict names one.
    ///
    /// `None` for an ordinary fire, and that absence is a statement: *start a
    /// fresh session*. It is never "resume whatever the ledger scan finds" —
    /// `run_coding_task`'s own resume binding does that, from the SETTLED
    /// continuation, and the whole point of this channel is to override it with
    /// the session the driver resolved for this exact effect.
    fn resume_session_id(&self) -> Option<&str> {
        match self {
            Self::FireResumingSession { native_session_id } => Some(native_session_id.as_str()),
            Self::Fire | Self::AdoptRecorded { .. } | Self::SettleAsAlreadyFired { .. } => None,
        }
    }

    /// Whether recovery supplied a failed result without dispatching this
    /// member on the current attempt.
    ///
    /// The answer feeds the same batch failure barrier as a freshly dispatched
    /// failed result. Treating an adopted failure as success would let the
    /// sequential, potentially mutating tail run only on recovery, even though
    /// the ordinary attempt would have stopped at this sibling.
    fn recorded_failure(&self) -> bool {
        matches!(
            self,
            Self::AdoptRecorded {
                succeeded: false,
                ..
            }
        )
    }
}

/// Read the driver's plan against the members the gate admitted.
///
/// # Order-preserving, and keyed by [`EffectId`]
///
/// The answer is index-parallel to `members`, so the gate's admission order —
/// which is the order the batch, the intents and `BatchMode::Parallel` all
/// record — survives. Lookup is by effect id and never by tool name: two members
/// of one batch can share a tool, and a plan matched by tool hands one member's
/// verdict to the other.
///
/// # Every variant of [`EffectAction`] is named
///
/// Exhaustively, with no `_` arm, and that is not style. A plan that handled
/// only [`EffectAction::Reattach`] and let the rest fall through would fire the
/// [`EffectAction::Adopt`] case a second time — an effect whose result is
/// already recorded, re-dispatched because nobody named its arm. Adding a
/// variant to `EffectAction` must break this build.
///
/// # A plan that does not correspond to this batch is a bug, not a shrug
///
/// Two disagreements are possible and they are not symmetrical:
///
/// - **The plan names an effect the gate did not admit.** Refused. The plan is
///   about a different batch than the one about to fire, and a dispatch that
///   quietly ignored the extra entry would be firing members whose verdicts it
///   never read. The driver builds the plan from the same `resolve_effects`
///   answer it puts on `PhaseEntry::effects`, so this means the gate re-derived
///   a different turn — which is a fact worth failing an attempt over.
/// - **The gate admitted a member the plan does not name.** *Not* an error. It
///   is the ordinary first entry into a turn (the plan is empty), and it is also
///   the honest answer for a member `gate_pending_effect` could not key: such a
///   member has no ledger row, so nothing could have been resolved about it.
///   It fires.
///
/// # ~~The residue: `Reattach` is refused~~ — CLOSED 2026-08-29
///
/// [`EffectAction::Reattach`] carries a live `native_session_id` and means
/// *resume that session; never fire this again*. Until 2026-08-29 this function
/// could do the second half and not the first, so it refused the member and held
/// the whole batch: `__coding_invocation_id` was the only value a dispatch
/// stamped into a coding call, and
/// `coding_engine::ledger::attach_live_invocation_session` writes the live
/// session to a field (`live_continuation`) that `run_coding_task`'s own resume
/// binding does not read — so a plain re-fire would have started a **fresh**
/// session rather than resumed, which is the double-run the refusal existed to
/// prevent.
///
/// The channel now exists, and it is a second runtime-stamped parameter rather
/// than anything written onto the action:
/// [`MemberDispatch::FireResumingSession`] carries the session to `executor.rs`,
/// which stamps it as `__coding_resume_session_id` beside
/// `__coding_invocation_id`, and `run_coding_task` binds it into the selected
/// engine's `resume_session_id`/`resume_thread_id` **ahead of**
/// `resolve_previous_chain_continuation`.
///
/// Three things about that shape are load-bearing and none is incidental:
///
/// - **A parameter, not `resolved_params`.** `executor.rs`'s
///   `without_model_hidden_params` strips every `__*` key an action arrives
///   with, so writing the session into the action at the gate would be a
///   transport that silently delivers nothing.
/// - **Not whitelisted through that strip.** A key a MODEL could supply would
///   let it name the session a coding job resumes — a reattach to the WRONG
///   session, which is worse than a reattach to none.
/// - **`live_continuation` stays unread by the resume binding.** That separation
///   is what stops a second coding job binding to a thread another process is
///   driving; the driver reaches the live session through
///   `WorkerHost::reattach_state`, keyed by THIS effect's invocation,
///   and hands it over explicitly. Collapsing the two would reintroduce the
///   scan.
///
/// # What this does NOT establish
///
/// That all four `EffectAction` variants are honoured here, and nothing more. A
/// mid-turn reattach also needs a foreign worker able to enter at an `Apply`
/// cursor at all, and it cannot: `Apply`'s `IterationCarry::resolved` derives no
/// serde, so `run_loop::state::ForeignPickup` classifies that cursor as
/// `MidIteration` and `worker_runner` refuses to claim it. The rule is not in
/// force end to end; this is the loop-side half of it.
///
/// **And that refusal is the only thing holding one window shut, which is worth
/// writing down because closing the window is not this function's to do.**
/// `EffectLedger::disposition` reads the EFFECT ledger's outcome, and
/// `driver_worker` writes that outcome in `record_batch_outcomes` — *after*
/// `dispatch_apply` returns. `ledger::invocation_continuation` meanwhile prefers
/// the SETTLED `continuation` over `live_continuation`. So a process death
/// between `run_coding_task::handle` returning (having already written
/// `attach_invocation_continuation` and `settle_coding_invocation`) and
/// `record_batch_outcomes` completing leaves a coding invocation marked
/// `Settled` with an effect row carrying no outcome — which resolves here as
/// `Reattach`, resolves in the host to the settled session, and re-sends the
/// prompt on a session that already did the work. Nothing downstream refuses it:
/// `prepare_coding_invocation`/`append_or_reuse` returns the stored entry
/// without inspecting `CodingInvocationState::dispatch`, and the handler never
/// reads it either. Before this function learned `Reattach` the same window
/// produced a HOLD, so this is a direction change and not a pre-existing one.
/// In-process it cannot fire — `record_batch_outcomes` runs on both arms of
/// `dispatch_apply` — and cross-process it cannot fire while the `Apply`-cursor
/// refusal above stands; it becomes live the day `IterationCarry::resolved`
/// derives serde. The repair belongs in
/// `coding_engine::ledger`: a session lookup that answers `None` for an
/// invocation whose `dispatch` has settled, so the driver holds as
/// `EffectIndeterminate` rather than dispatching.
fn plan_the_batch(plan: &EffectPlan, members: &[GatedMember]) -> Result<Vec<MemberDispatch>> {
    // The ids the gate actually admitted, in its order. A member with no row is
    // a dispatch the run could not key — see `gate_pending_effect` — so it names
    // no effect and the plan can say nothing about it.
    let admitted: Vec<Option<EffectId>> = members
        .iter()
        .map(|member| member.row.as_ref().map(|row| row.effect_id.clone()))
        .collect();

    for named_by_plan in plan.effect_ids() {
        let matched = admitted
            .iter()
            .filter(|candidate| candidate.as_ref() == Some(named_by_plan))
            .count();
        if matched == 0 {
            return Err(anyhow!(
                "apply: the driver's effect plan names {named_by_plan}, which the gate did not \
                 admit in this batch. The plan and the batch describe different turns, so no \
                 member's verdict can be trusted and nothing is dispatched"
            ));
        }
        if matched > 1 {
            // `PendingBatch::validate` already refuses a duplicate effect id, so
            // reaching here means a batch that never went through it. Checked
            // anyway: a duplicate makes `action_for` hand one row's verdict to
            // two members, and one of those two is wrong.
            return Err(anyhow!(
                "apply: {matched} members of this batch share the effect id {named_by_plan}, so \
                 one verdict would decide two dispatches. Nothing is dispatched"
            ));
        }
    }

    let mut dispositions = Vec::with_capacity(members.len());
    for effect_id in admitted.iter() {
        let action = effect_id
            .as_ref()
            .and_then(|effect_id| plan.action_for(effect_id));
        dispositions.push(match action {
            None | Some(EffectAction::Refire) => MemberDispatch::Fire,
            Some(EffectAction::Adopt { result, succeeded }) => MemberDispatch::AdoptRecorded {
                result: result.clone(),
                succeeded: *succeeded,
            },
            Some(EffectAction::AlreadyFired { by_this_attempt }) => {
                MemberDispatch::SettleAsAlreadyFired {
                    by_this_attempt: *by_this_attempt,
                }
            },
            // RESUMED, not re-fired, and the session travels with the verdict.
            //
            // The `clone` is the point rather than a cost: the plan is a value
            // this phase may only read, so the session has to be OWNED by the
            // disposition to survive into the dispatch — which for the parallel
            // slice crosses a `join_all` and for every member crosses the
            // scheduler-root spawn.
            Some(EffectAction::Reattach { native_session_id }) => {
                MemberDispatch::FireResumingSession {
                    native_session_id: native_session_id.clone(),
                }
            },
        });
    }
    Ok(dispositions)
}

/// Settle a member the plan said not to fire, and say nothing about the ones it
/// has nothing to say about.
///
/// The counterpart of [`record_member_outcome`], for the members this dispatch
/// decided against rather than reached. Deliberately **not** folded into
/// [`record_members_not_dispatched`]: that function writes
/// [`EffectOutcome::NotDispatched`], the one outcome in the enum that *proves*
/// an effect did not go out, and writing it over a member the outward record
/// says already fired would be a false negative of the exact kind this module
/// exists to prevent.
fn settle_member_not_fired(
    settled: &mut Vec<(EffectId, EffectOutcome)>,
    member: &GatedMember,
    disposition: &MemberDispatch,
) {
    match disposition {
        // Not reachable from a caller that checked first, and answered rather
        // than asserted: a `Fire` arriving here is a member that should have
        // been dispatched, and settling it as anything would hide that.
        // `FireResumingSession` is in the same arm for the same reason — it is a
        // dispatch too, differing only in what it hands the handler.
        MemberDispatch::Fire | MemberDispatch::FireResumingSession { .. } => {},
        // Nothing. The row already carries the outcome that made the driver
        // answer `Adopt`.
        MemberDispatch::AdoptRecorded { .. } => {},
        MemberDispatch::SettleAsAlreadyFired { by_this_attempt } => {
            // `Succeeded` and not `Indeterminate`, and the difference is what
            // the run knows: the outward record is positive evidence the act
            // LEFT. `Indeterminate` would say the opposite — that nothing can
            // tell — and would hold this run again on the next claim, forever,
            // over a send the record has already answered.
            //
            // It is also not `NotDispatched`, which claims the effect never went
            // out. It did.
            record_member_outcome(
                settled,
                member,
                EffectOutcome::Succeeded {
                    at_ms: Utc::now().timestamp_millis(),
                },
            );
            debug!(
                by_this_attempt = *by_this_attempt,
                "[APPLY] a member of this batch was settled against the outward record rather \
                 than dispatched"
            );
        },
    }
}

/// Project an already-settled effect back into the same causal history shape a
/// live dispatch would have written. Without this, recovery avoids the duplicate
/// side effect but silently drops the tool result, so the next Decide may issue
/// the same action again for lack of evidence.
fn record_member_not_fired_in_history(
    history: &mut ExecutionHistory,
    member: &GatedMember,
    disposition: &MemberDispatch,
    state_before: &EnvironmentState,
    state_after: &EnvironmentState,
    iteration: usize,
) {
    let (encoded, succeeded, description) = match disposition {
        MemberDispatch::AdoptRecorded { result, succeeded } => (
            result.as_ref(),
            *succeeded,
            "the durable effect ledger already held this dispatch result",
        ),
        MemberDispatch::SettleAsAlreadyFired { .. } => (
            None,
            true,
            "the outward assertion record proves this dispatch already fired",
        ),
        MemberDispatch::Fire | MemberDispatch::FireResumingSession { .. } => return,
    };
    let mut result = encoded
        .and_then(|value| serde_json::from_value::<ActionResultRecord>(value.clone()).ok())
        .unwrap_or_else(|| {
            if succeeded {
                ActionResultRecord::new(
                    true,
                    Some(description.to_string()),
                    None,
                    0,
                    Some(ActionOutcomeCategory::PartialProgress),
                )
            } else {
                ActionResultRecord::new(
                    false,
                    None,
                    Some(description.to_string()),
                    0,
                    Some(ActionOutcomeCategory::Failed),
                )
            }
        });
    // The outcome variant is the authoritative status. A malformed or stale
    // projection cannot turn a recorded failure into success (or vice versa).
    result.success = succeeded;
    history.iterations.push(IterationRecord {
        iteration,
        state_before: state_before.clone(),
        action: member.candidate.action.clone(),
        result,
        state_after: state_after.clone(),
        timestamp: Utc::now(),
        verification: None,
        llm_reasoning: (!member.candidate.reasoning.trim().is_empty())
            .then(|| member.candidate.reasoning.clone()),
    });
}

/// Merge a parallel slice's per-member histories in admitted member order.
///
/// `join_all` returns fired jobs in input order, but recovery filters already
/// settled members out of that input. Those members still need a synthetic
/// history result. If it is appended when the plan is inspected, every adopted
/// member appears before every fired member; if fired results are appended from
/// join slots alone, their slot is no longer their member index. The indexed
/// staging vector is the common coordinate for both populations.
///
/// Artifacts follow the same order. The scratch dispatch path can materialise
/// downloads and projected tool results into `history.artifacts`; omitting them
/// here would make the parallel path lose artifacts that the sequential path
/// retains.
fn merge_parallel_member_histories(
    history: &mut ExecutionHistory,
    staged: &mut [Option<ExecutionHistory>],
) {
    // Slot zero is the primary, whose history was written before the parallel
    // slice. Starting at one also makes an accidentally empty staging vector a
    // harmless no-op rather than an index panic on an error path.
    for member_history in staged.iter_mut().skip(1).filter_map(Option::take) {
        history.iterations.extend(member_history.iterations);
        history.artifacts.extend(member_history.artifacts);
    }
}

/// What the ledger records about one member the dispatch actually reached.
///
/// Three answers, and the line between them is *what the runtime can honestly
/// claim*, not how bad the news is:
///
/// - **`Err` from the dispatch call** is [`EffectOutcome::Indeterminate`]. The
///   call died between the scheduler and the settle — a closed worker channel, a
///   dropped oneshot, a transport that panicked its way to an `anyhow` — and
///   nothing in this process can say whether the act left. This is the case the
///   ledger exists for, and recording it as a failure would be the false
///   `DidNotFire` the whole module is built to prevent.
/// - **A recorded result that failed** is [`EffectOutcome::Failed`]. The
///   dispatch ran and the runtime has its report. `EffectLedger::disposition`
///   answers `Adopt`, which is right: there is a result to adopt.
/// - **A recorded result that succeeded** is [`EffectOutcome::Succeeded`].
///
/// [`EffectOutcome::NotDispatched`] is deliberately **not** reachable from here.
/// This function is only called for a member the dispatch reached, and the
/// members it did not reach go through [`record_members_not_dispatched`]. Two
/// functions rather than one with a flag, because the distinction they carry —
/// *we tried* against *we never tried* — is the one distinction in this file
/// that recovery acts on differently, and a flag is how it gets passed wrong.
fn durable_effect_result(
    result: Option<&crate::magician_v2::execution::agentic::types::ActionResultRecord>,
) -> Option<serde_json::Value> {
    let result = result?;
    // The bounded projection has already passed the provider/result sanitizer.
    // Never fall back to legacy raw output or an error string here: both may
    // contain credentials, and a missing projection is safely represented by
    // the generic adopted result on recovery.
    let mut durable = result.clone();
    durable.output = None;
    durable.error = durable
        .error
        .as_ref()
        .map(|_| "the dispatch reported an error".to_string());
    serde_json::to_value(durable).ok()
}

fn dispatched_outcome(
    error: Option<&anyhow::Error>,
    recorded_failure: bool,
    result: Option<&crate::magician_v2::execution::agentic::types::ActionResultRecord>,
) -> EffectOutcome {
    let at_ms = Utc::now().timestamp_millis();
    let result = durable_effect_result(result);
    match error {
        Some(error) => EffectOutcome::Indeterminate {
            at_ms,
            reason: error.to_string(),
        },
        None if recorded_failure => match result {
            Some(result) => EffectOutcome::FailedWithResult {
                at_ms,
                reason: "the dispatch ran and reported a failed result".to_string(),
                result,
            },
            None => EffectOutcome::Failed {
                at_ms,
                reason: "the dispatch ran and reported a failed result".to_string(),
            },
        },
        None => match result {
            Some(result) => EffectOutcome::SucceededWithResult { at_ms, result },
            None => EffectOutcome::Succeeded { at_ms },
        },
    }
}

/// Hand one member's outcome to the driver, if that member has a row to hang it
/// on.
///
/// A member with no row is one [`gate_pending_effect`] could not key — `None`
/// there is not a refusal — so there is no ledger row for an outcome to attach
/// to and nothing is reported. Silent by design: the gate already warned at the
/// site that minted the `None`, and warning again per settle would turn one
/// unkeyable dispatch into a line per member.
fn record_member_outcome(
    settled: &mut Vec<(EffectId, EffectOutcome)>,
    member: &GatedMember,
    outcome: EffectOutcome,
) {
    if let Some(row) = member.row.as_ref() {
        settled.push((row.effect_id.clone(), outcome));
    }
}

/// Settle every member from `from` onward as one the dispatch never reached.
///
/// The hoisted gate admits the whole in-turn batch, so a batch that stops short
/// leaves intents behind for members that were never attempted. Left alone those
/// rows are *unsettled*, which `EffectLedger::disposition` reads as **may have
/// fired** — so a resume would reconcile, or surface to a user, a dispatch this
/// process knows for certain never happened. This is where that knowledge is
/// written down.
///
/// # `dispositions` is consulted, and omitting it would write a false negative
///
/// [`EffectOutcome::NotDispatched`] is the one outcome that *proves* an effect
/// did not go out. A member the driver's plan already answered was not
/// *unreached* — it was un-fired **on purpose**, and for
/// [`MemberDispatch::SettleAsAlreadyFired`] the outward record says it left. So
/// each member is settled by its own plan verdict here, exactly as it would have
/// been had the batch run this far, and only the members that were genuinely
/// owed a fire get `NotDispatched`.
fn record_members_not_dispatched(
    settled: &mut Vec<(EffectId, EffectOutcome)>,
    members: &[GatedMember],
    dispositions: &[MemberDispatch],
    history: &mut ExecutionHistory,
    state_before: &EnvironmentState,
    state_after: &EnvironmentState,
    iteration: usize,
    from: usize,
    reason: &str,
) {
    let at_ms = Utc::now().timestamp_millis();
    for (index, member) in members.iter().enumerate().skip(from) {
        match dispositions.get(index) {
            // `None` cannot happen — `plan_the_batch` answers one disposition
            // per member — and is answered rather than asserted: a member with
            // no verdict is one nothing licensed skipping, so the fail-closed
            // reading is the one this arm already gives.
            Some(MemberDispatch::Fire)
            | Some(MemberDispatch::FireResumingSession { .. })
            | None => record_member_outcome(
                settled,
                member,
                EffectOutcome::NotDispatched {
                    at_ms,
                    reason: reason.to_string(),
                },
            ),
            Some(disposition) => {
                settle_member_not_fired(settled, member, disposition);
                record_member_not_fired_in_history(
                    history,
                    member,
                    disposition,
                    state_before,
                    state_after,
                    iteration,
                );
            },
        }
    }
}

/// Preserve positive evidence about every member an errored dispatch never
/// reached, then return the original error unchanged.
///
/// The gate publishes the whole batch before its first fire. A non-cancellation
/// error from the primary, the parallel prefix, or the sequential tail therefore
/// leaves durable intents for the suffix. That suffix is not indeterminate: this
/// function is still running and knows it never called the executor for those
/// members. Recording `NotDispatched` before the error escapes prevents recovery
/// from surfacing false reconciliation prompts for work that provably did not
/// happen.
#[allow(clippy::too_many_arguments)]
fn settle_unreached_before_error(
    settled: &mut Vec<(EffectId, EffectOutcome)>,
    members: &[GatedMember],
    dispositions: &[MemberDispatch],
    history: &mut ExecutionHistory,
    state_before: &EnvironmentState,
    state_after: &EnvironmentState,
    iteration: usize,
    from: usize,
    reason: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    record_members_not_dispatched(
        settled,
        members,
        dispositions,
        history,
        state_before,
        state_after,
        iteration,
        from,
        reason,
    );
    error
}

// ════════════════════════════════════════════════════════════════════════════
// The Gate: what the run commits to before a dispatch fires
// ════════════════════════════════════════════════════════════════════════════

/// The batch `Apply` is committing to, accumulated as each member is gated.
///
/// A member is appended **before** its dispatch runs, and the caller's
/// out-parameter is republished on every append — by **both** admission paths,
/// including the ones that appended nothing. So the value a driver reads after
/// this phase names every dispatch that was allowed to fire and none that was
/// not, whatever order the two paths ran in.
///
/// # What this is, and what a driver does with it
///
/// It is what the design's **intent commit** is written from. That commit puts
/// the whole batch on durable storage *before any of it fires*, and it belongs
/// to a driver rather than to this phase — `phases::apply` cannot reach a store,
/// deliberately. What changed on 2026-08-29 is that there is now a seam for the
/// driver to act at: [`gate`] returns this batch with nothing dispatched, and
/// [`dispatch`] will not run without a receipt only a driver can mint.
///
/// So there are two readers, on two paths, and both matter:
///
/// - **[`ApplyGate::Gated`], the success path** — `driver_worker` writes an
///   intent per member and then dispatches. This is the one the ledger needed.
/// - **`driver_worker::PhaseFailure::pending`, the error path** — where the
///   cursor does not move and the next worker re-enters the same `Apply`. A gate
///   that refused partway has still named dispatches, and although none of them
///   fired, the next attempt must not be told the run committed to nothing.
///
/// What is in force alongside that is narrower and worth having on its own: the
/// ref is computed before the fire, from the same `resolved_params` instance the
/// outward gate hashes, and an outward dispatch that will really send and whose
/// record cannot be named never fires at all.
struct GatedBatch {
    effects: Vec<PendingEffect>,
    /// How many rows the parallel slice contributed.
    ///
    /// A **count, not a range.** `BatchMode::Parallel { admitted }` carries the
    /// number and nothing else, and the positions cannot be recovered from it:
    ///
    /// - The primary contributes no row when [`gate_pending_effect`] answers
    ///   `Ok(None)` — a candidate with no usable effect id, which is reachable
    ///   on the text-fallback decision paths. Then the parallel members start at
    ///   index 0, not 1.
    /// - The sequential tail is admitted **after** the slice, so a
    ///   `Parallel { admitted }` batch also holds sequential rows above it.
    ///
    /// An earlier version of this comment claimed the members sit at
    /// `1..=admitted_in_parallel`. Nothing reads those offsets today —
    /// `PendingBatch::validate` only bounds `admitted <= effects.len()`, which
    /// both shapes satisfy — so it was a false written-down claim rather than a
    /// live miscount, which is exactly the kind a later reader implementing
    /// out-of-order resume would trust. If positions are ever needed, put them
    /// on the row.
    ///
    /// Written by [`GatedBatch::admit_parallel_slice`] and nowhere else.
    admitted_in_parallel: usize,
}

impl GatedBatch {
    fn new() -> Self {
        Self {
            effects: Vec::new(),
            admitted_in_parallel: 0,
        }
    }

    /// Admit one dispatch and republish the batch.
    ///
    /// `None` is not a refusal — it is a dispatch this run could not give a
    /// loop-side identity to (see [`gate_pending_effect`]). A refusal is an
    /// `Err` from that function and never reaches here.
    ///
    /// It still republishes. An earlier cut returned before [`Self::publish`]
    /// on the `None` arm, which left `*out` at whatever it already held while
    /// [`Self::admit_parallel_slice`] republished unconditionally — two paths
    /// disagreeing about what "nothing was admitted" does to the caller's value.
    /// Harmless only for as long as the two happen to run in one order.
    fn admit(
        &mut self,
        effect: Option<PendingEffect>,
        iteration: usize,
        out: &mut Option<PendingBatch>,
    ) {
        if let Some(effect) = effect {
            self.effects.push(effect);
        }
        self.publish(iteration, out);
    }

    /// Admit the whole parallel slice, and count what it actually contributed.
    ///
    /// The counting is **here** rather than at the call site, and that is the
    /// point of the method existing at all. `admitted` is counted from the rows
    /// that were admitted, never from the members that were offered: a member
    /// the run could not key a row for is not in the batch, so counting offers
    /// yields an `admitted` larger than `effects.len()` — which is exactly what
    /// `PendingBatch::validate` refuses as `ParallelSliceOverruns`, failing the
    /// commit of a batch that was otherwise correct. Written at the call site
    /// that arithmetic is one expression away from being wrong and nothing
    /// checks it; written here it is one expression that a test can hold.
    fn admit_parallel_slice(
        &mut self,
        slice: Vec<Option<PendingEffect>>,
        iteration: usize,
        out: &mut Option<PendingBatch>,
    ) {
        let before = self.effects.len();
        for effect in slice.into_iter().flatten() {
            self.effects.push(effect);
        }
        self.admitted_in_parallel += self.effects.len() - before;
        self.publish(iteration, out);
    }

    /// Write the batch as it now stands onto the caller's out-parameter.
    ///
    /// The single writer of `out`, so the two admission paths cannot come to
    /// disagree about what it holds.
    ///
    /// # A batch the store would refuse is published anyway, loudly
    ///
    /// `PendingBatch::validate` is consulted and its verdict is a `warn!`, never
    /// a refusal. Both directions were considered and only this one is safe:
    ///
    /// - **Refusing to publish** hides the batch from
    ///   `commit_failed_attempt`, so a worker re-entering `Apply` finds
    ///   `pending: None` and re-fires every member blind. That is the exact
    ///   failure the batch exists to prevent, arriving through the guard meant
    ///   to protect it.
    /// - **Refusing the dispatch** at `admit` would be a guard at the producer.
    ///   The reachable violation is `DuplicateEffectId`, which a model turn
    ///   produces by repeating a `tool_call_id`; stopping a live dispatch over
    ///   it would be a large blast radius for a durable-storage rule.
    ///
    /// So it publishes, and the later `resolve_effects` load refuses it as
    /// `Quarantine::UnreadableState` — which is the correct, human-visible
    /// outcome. The `warn!` exists so that outcome can be traced to the turn
    /// that caused it rather than to the worker that found it.
    fn publish(&self, iteration: usize, out: &mut Option<PendingBatch>) {
        if self.effects.is_empty() {
            // `PendingBatch::validate` refuses an empty batch — "an intent
            // commit with nothing to fire" — so nothing in flight is reported
            // as `None`, which is what `LoopState::pending` already means.
            *out = None;
            return;
        }
        let batch = PendingBatch {
            iteration,
            phase: Phase::Apply,
            mode: if self.admitted_in_parallel > 0 {
                BatchMode::Parallel {
                    admitted: self.admitted_in_parallel,
                }
            } else {
                BatchMode::Sequential
            },
            effects: self.effects.clone(),
        };
        if let Err(error) = batch.validate() {
            warn!(
                iteration = iteration,
                error = %error,
                effects = batch.effects.len(),
                "[GATE] this iteration's gated batch is not one the loop-state store will read \
                 back; it is still recorded, because hiding it would let a resuming worker \
                 re-fire every member blind — expect a quarantine rather than a resume"
            );
        }
        *out = Some(batch);
    }
}

/// Fold what a parallel follow-up slice's forked `LoopProtectiveState`s recorded
/// back into the run's own.
///
/// # The bug this closes
///
/// The slice's members run concurrently and
/// `execute_direct_path_on_scheduler_root` takes `&mut LoopProtectiveState`, so
/// each member is handed a **clone**. Until 2026-08-27 the join merged only
/// `scratch_history.iterations` and every clone was dropped, which silently
/// discarded both of the things the dispatch path writes to that bundle:
///
/// - `execute_direct_path_post_setup_tail`'s
///   `loop_detector.record_action_result(..)` (`executor.rs:19280`) — the only
///   writer of the detector's action/state fingerprint history on the dispatch
///   path, and **three-sided**: it appends when an action made no progress,
///   clears *both* histories when it made progress, and clears *only* the state
///   history for a content-modifying action. See "The three paths" below, which
///   is where that third one used to cost something.
/// - the same function's `loop_recovery_context = None` on a successful action
///   (`executor.rs:19607`).
///
/// So a parallel-dispatched action neither registered in cycle detection nor
/// cleared a stale loop-pressure signal. It shipped that way.
///
/// # The audit that makes this two fields and not eighteen
///
/// Every other `loop_protective` parameter reachable from
/// `execute_direct_path_on_scheduler_root` is a **shared** borrow —
/// `build_secret_approval_waiting_outcome`, the three `*_direct_action_preflight`
/// helpers, `build_full_pause_state` and `handle_path_access_denied` all take
/// `&LoopProtectiveState` — so only `execute_direct_path_post_setup_tail` can
/// write, and inside it the two lines above are the whole population.
/// `agentic_tool_repeat_counts`, `agentic_failed_tool_fingerprints` and
/// `loop_recovery_context = Some(..)` are written in `phases::resolve` and in
/// this phase *outside* the concurrent region, so a member's clone can never
/// carry a value for them that the base does not already have. Verified
/// 2026-08-27; re-run it by grepping `loop_protective` between `executor.rs`
/// 18270 and 19212 before adding a field here.
///
/// # `loop_recovery_context` is exact. The detector is NOT, and here is why
///
/// The clear is one-sided: the only write on this path sets `None`, and the two
/// writers that set `Some` are outside the concurrent region. So a member whose
/// copy is `None` where the base is `Some` **proves** that member cleared it, and
/// "any `None` wins" reproduces the sequential result exactly.
///
/// The detector is folded on one fact and one approximation, and it is worth
/// being exact about which is which. Each member is `base + at most one
/// record_action_result`, and until 2026-08-29 **a fork could not say which of
/// that call's three paths it took**: a cleared fork and a fork that appended to
/// an empty base were the same value, `record_action_result`'s `made_progress`
/// return is discarded at `executor.rs:19280` and never leaves that frame, and
/// `state_history`/`action_history` are private with no merge.
///
/// `LoopDetector::history_clears()` closes that half. Every clear of either
/// deque now goes through `clear_state_history`/`clear_both_histories`, which
/// bump a counter the fork carries with it, so "this fork cleared" is a value
/// the fold can *read* rather than a shape it has to infer. What remains
/// approximate is only the other question — which of several forks that all
/// merely appended should be adopted — and that one has no exact answer without
/// a merge.
///
/// ## The three paths, because `recorded_len()` sums two deques
///
/// `recorded_len()` is `state_history.len() + action_history.len()`
/// (`loop_detector.rs:1123`), and `record_action_result` reaches it three ways
/// (`loop_detector.rs:1593`). Writing `S`/`A` for the base's two lengths:
///
/// - **Progress** — clears *both*, records one state and one action. Length 2.
/// - **No progress** — records one action, and one state unless the action has
///   an uncertain state effect. Length `S+A+1` or `S+A+2`, capped by
///   `max_history` per deque.
/// - **Content-modifying** — clears **only `state_history`**, records one state
///   and one action, returns `true`. Length `A+2`, or `A+1` once
///   `action_history` is at `max_history`, since `record_action` pushes and then
///   trims to the bound.
///
/// The third path is why length alone could not be the test. With `S=1, A=4` it
/// yields 6 against a base of 5, so a genuine clear comes back **longer**; a
/// fold keyed on "shorter than the base" reads it as an append, adopts a longer
/// no-progress sibling instead, and re-arms the false `state_loop` detection the
/// reset exists to prevent. Nothing fails when that happens — the run just
/// starts detecting a cycle it is not in.
///
/// That was latent rather than live, and the reason it was latent is the part
/// worth keeping in mind. `ActionFingerprint::is_content_modifying` requires
/// `category == "browser"` (`loop_detector.rs`, in the `ActionFingerprint`
/// impl) while the parallel gate admits only File-read actions and read-only
/// Packs (`types::is_parallelizable_read_only_action`, around `types.rs:5134`).
/// The two sets do not intersect today — but they are maintained
/// independently, in different files, by different concerns, and **nothing
/// cross-checked them**. Admitting one browser read to the parallel slice, or
/// adding one non-browser verb to `is_content_modifying`, would have started
/// mis-folding silently.
///
/// So the dependency was removed rather than pinned: case 1 below now reads
/// `history_clears()`, which is true of a clear on *any* of the three paths and
/// on any future fourth one, so neither list has to stay in a particular
/// relationship with the other. A disjointness assertion would have made a
/// widening loud, but it would also have made a legitimate widening fail, and
/// it would have left the real defect — a *necessary* condition stated
/// incorrectly — in place.
///
/// The count also retires case 1's dead zone. A full clear settles at length 2,
/// so with `base_len <= 2` no fork was ever shorter than its base and the case
/// was unreachable; a counter that moved is visible at any length.
///
/// So this **approximates**, under one invariant: *never leave the detector
/// holding a longer history than a correct sequential fold would*, because
/// history length is what makes the detector fire and a fold that over-records
/// makes a cycle detection fire on a run that was making progress. Two cases:
///
/// 1. **A fork whose clear count moved past the base's proves a clear** — this
///    is the fact and not an inference from it, and it holds on all three paths of
///    `record_action_result` including the one that comes back longer. Adopt the
///    last such fork. That keeps the clear and loses the members after it, which
///    is the safe direction.
///
///    The old length test survives *beside* it as a second disjunct, and the
///    distinction matters: these are not two definitions of "cleared" that have
///    to agree — the defect this function had — but two independently sufficient
///    proofs, either of which is enough on its own. Length still catches one
///    thing the count cannot, because the count is zero on a member that never
///    ran: an un-reconciled `LoopProtectiveState::default()` placeholder. The
///    caller filters those;
///    `a_defaulted_member_erases_the_base_which_is_why_the_caller_filters` pins
///    that this function on its own does not. Length is sound as a proof in the
///    other direction too — nothing but a clear shrinks a fork, since
///    `record_action` at `max_history` pops one and pushes one, and the
///    detector's other shrinker, `LoopDetector::prepare_for_recovery`, is called
///    only from `check_control_decision_loop_pressure` and from this phase
///    outside the concurrent region.
/// 2. **Otherwise adopt the longest fork**, ties to the last. If any member
///    recorded, that fork is the longest and its observation survives; if none
///    did, every fork equals the base and the adoption is value-preserving. Note
///    that it is still an *adoption* and not a no-op: the base's detector is
///    replaced by a member's on every non-empty call.
///
/// What is lost either way: with `n` members recording, `n − 1` observations do
/// not reach the detector. That is a strict improvement on the `n` that were lost
/// before, and it errs toward detecting a cycle *later*, never earlier.
///
/// # Every member here must have returned `Ok`
///
/// The caller filters. This is a precondition and not an optimisation: a member
/// whose dispatch failed to *schedule* never reaches
/// `execute_direct_path_on_scheduler_root`'s reconciling swaps, so its bundle is
/// still the `std::mem::take` placeholder — a `LoopProtectiveState::default()`.
/// This function would read that placeholder as two positive claims: `None`
/// `loop_recovery_context` reads as "cleared it", and `recorded_len() == 0`
/// reads as "proved a clear", which installs an empty `LoopDetector` on the run
/// and — on the cancellation arm, when a manual pause is also requested —
/// persists it into the pause `finish_cancelled_agentic_execution` writes. See
/// the guard at the join for the full path.
///
/// What is still owed. Case 1 is now exact; case 2 is not, and cannot be made
/// exact here — choosing among `n` forks that each appended a different
/// observation is a merge, and `LoopDetector` has none. The fix for the
/// remainder is not a better rule at this call site: it is for the parallel
/// slice to stop forking the detector and to record its members serially after
/// the join, which is an `executor.rs` change.
fn adopt_parallel_member_protective_state(
    loop_protective: &mut LoopProtectiveState,
    mut members: Vec<LoopProtectiveState>,
) {
    if members.is_empty() {
        return;
    }

    if members
        .iter()
        .any(|member| member.loop_recovery_context.is_none())
    {
        loop_protective.loop_recovery_context = None;
    }

    let base_len = loop_protective.loop_detector.recorded_len();
    let base_clears = loop_protective.loop_detector.history_clears();
    let cleared = members
        .iter()
        .enumerate()
        .filter(|(_, member)| {
            // THE FACT, first. A fork whose clear count moved past the base's
            // cleared, whatever length it came back at — which is the whole
            // point, because the content-modifying path of
            // `record_action_result` clears and comes back LONGER.
            member.loop_detector.history_clears() > base_clears
                // The length test, kept as a SECOND SUFFICIENT PROOF and not as
                // a competing definition. It catches the one case the count
                // cannot: a member that never ran carries
                // `LoopProtectiveState::default()`, whose count is zero because
                // nothing bumped it, and whose emptiness is the only signal
                // there is. The caller filters those out; the pinned test says
                // this function does not.
                || member.loop_detector.recorded_len() < base_len
        })
        .map(|(index, _)| index)
        .last();
    let chosen = match cleared {
        Some(index) => index,
        // The key is a pair rather than the length alone so "ties go to the
        // last member" is stated here instead of resting on `max_by_key`'s
        // documented tie-breaking, which is a different file's promise.
        None => members
            .iter()
            .enumerate()
            .max_by_key(|entry| (entry.1.loop_detector.recorded_len(), entry.0))
            .map(|(index, _)| index)
            // Unreachable: `members` is non-empty by the guard above. Written as
            // a fallback rather than an `expect` because the alternative to
            // adopting member zero is panicking inside a live dispatch join.
            .unwrap_or(0),
    };
    loop_protective.loop_detector = members.swap_remove(chosen).loop_detector;
}

/// The capability or tool name a ledger row records for a dispatch.
///
/// The capability name for a pack call, because that is the coordinate the
/// outward classifier and `refuse_unless_same_dispatch` both key on; the action
/// type for everything else. Deliberately **not** `action_signature`, which
/// renders parameter values — a durable row must not carry argument bytes,
/// which is the reason `PendingEffect` has no `args` field in the first place.
fn pending_tool_name(action: &ExecutableAction) -> String {
    match action {
        ExecutableAction::Pack {
            capability_name, ..
        } => capability_name.clone(),
        other => other.action_type_name().to_string(),
    }
}

/// The `arguments_fingerprint` a committed row carries.
///
/// Computed through `LlmToolLineageIdentity::new` — the **same function**
/// `WorkerHost::rederive_dispatch` calls when a resuming worker re-derives this
/// dispatch from the conversation. That is what makes the two values
/// comparable, and comparability is the entire point: the fingerprint exists so
/// that a re-derivation which drifted is loud rather than silent, and two
/// independent spellings of it would make every honest re-derivation look like
/// a drift.
///
/// The digest reads exactly three of that constructor's arguments — the
/// workspace, the trace context's scope, and the tool call's arguments. The
/// rest is metadata it never sees, which is why this call and the re-derivation
/// agree while passing different `branch_id` and `source_surface` values.
///
/// # Empty is a state, not a placeholder
///
/// A turn with no trace receipt, a candidate the model gave no id, a tool call
/// the turn does not contain, or a scoped HMAC key that cannot be loaded all
/// yield `""`. `EffectLedger::authorize_refire` refuses an empty fingerprint
/// outright, so a row committed in that state can never license a re-fire —
/// which is the fail-closed direction, and the only honest one, because there
/// is nothing for a re-derivation to reproduce.
fn gate_arguments_fingerprint(
    executors: &ActionExecutors,
    history: &ExecutionHistory,
    candidate: &ActionCandidate,
) -> String {
    let Some(turn) = history.assistant_turns.last() else {
        return String::new();
    };
    let Some(context) = turn.llm_trace_context.clone() else {
        return String::new();
    };
    // Matched by the tool-call id rather than by position, which is the same
    // key `effect_id_for_candidate` mints the row's id from and the same one
    // the re-derivation looks the call up by. Position is what the three
    // dispatch sites used to compute separately, and getting it wrong keys a
    // row to a DIFFERENT tool call rather than failing to compile.
    let tool_call_id = candidate
        .tool_call_id
        .as_deref()
        .filter(|id| !id.is_empty());
    let Some(tool_call) =
        tool_call_id.and_then(|id| turn.tool_calls.iter().find(|call| call.id == id))
    else {
        return String::new();
    };
    let tool_family = tool_call
        .name
        .split_once("__")
        .map(|(family, _)| family.to_string())
        .or_else(|| Some(tool_call.name.clone()));
    LlmToolLineageIdentity::new(
        &executors.artifact_v2_workspace,
        context,
        tool_call.id.clone(),
        // Neither of these feeds the digest. Named for what this call is so a
        // log line reading the identity cannot be mistaken for the dispatch's
        // own lineage record, which `begin_agentic_tool_lineage` mints.
        format!("{}:gate", tool_call.id),
        turn.operation.clone(),
        "agentic_gate_commit".to_string(),
        tool_call.name.clone(),
        tool_family,
        &tool_call.arguments,
    )
    .map(|identity| identity.arguments_fingerprint)
    .unwrap_or_default()
}

/// Name the outward record a dispatch will reconcile against, before it fires.
///
/// **Decision 1's `Gate`.** `ctx`, `executors` and the candidate's
/// `ExecutableAction` are all live here, which is the one place the act ref can
/// be derived from the same `resolved_params` map instance the outward gate
/// hashes. Committing it deletes the re-derivation at pickup, and with it the
/// silent false `DidNotFire` a re-serialised `HashMap` produces.
///
/// # Four answers, one collapse
///
/// The collapse goes through `DispatchReconcileRef::committed`, so `Err` is
/// reachable from `Undeterminable` and from nowhere else. Writing the match here
/// instead would put the dangerous arm — `Undeterminable => None`, the shorter
/// one, the one that reads as tidying — at every commit site.
///
/// # `Undeterminable` fails the execution, and `Unscoped` deliberately does not
///
/// `Undeterminable` means the dispatch **is** outward, this execution has a
/// scoped store so the outward gate will not refuse it on that ground, and its
/// record still cannot be named. Committing `None` for one of those files a live
/// send as a dispatch that reaches nobody, and a worker reading that `None` has
/// no record to ask, calls it *nothing left*, and sends it again. An `Err` here
/// propagates out of the `Execute` arm's own `Box::pin(async { … })` through the
/// existing `?`, so the dispatch never runs.
///
/// The refusal is bounded at both ends, and the second bound was missing when
/// this gate was first written:
///
/// - It cannot swallow ordinary **local** work: `reconcile_ref_for_dispatch`
///   runs its **pure** classifier half first, so a run with no artifact service
///   answers `NotOutward` for everything that cannot reach anybody and never
///   reaches the branch that needs a store.
/// - It does not refuse an outward dispatch **solely** because the run has no
///   scope. That answers `Unscoped`, which commits `None`; with a usable effect
///   id the outward gate later produces its own *"NOT SENT"* result, which the
///   model can read and adapt to. The durable driver's separate identity rule
///   still refuses an unkeyable outward member — `Unscoped` is not positive
///   evidence that the action is local — while the in-process rollback posture
///   retains the resident behavior.
///
/// # Effect identity is driver-aware
///
/// `effect_id_for_candidate` answers `None` when the turn carries no trace
/// receipt or the model gave the call no id, and that is reachable on the
/// text-fallback decision paths. The stateless driver refuses such a dispatch
/// whenever it is outward, regardless of a `RetrySafe` declaration, and also
/// refuses local work that is not positively retry-safe. Only positively
/// non-outward retry-safe work keeps the durable exception. The in-process
/// rollback arm passes a separate posture and keeps resident-stack semantics;
/// it does not acquire a restart requirement merely because it shares this
/// gate.
fn gate_pending_effect(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    history: &ExecutionHistory,
    candidate: &ActionCandidate,
    effect_id: Option<&str>,
    effect_identity: EffectIdentityPosture,
) -> Result<Option<PendingEffect>> {
    let tool = pending_tool_name(&candidate.action);
    let retry_safety = retry_safety_for_tool(&tool, executors);
    let dispatch_reconcile_ref = reconcile_ref_for_dispatch(&candidate.action, executors, ctx);
    let durable_id_required =
        effect_id_required_for_dispatch(effect_identity, retry_safety, &dispatch_reconcile_ref);
    let reconcile_ref = dispatch_reconcile_ref.committed().map_err(|reason| {
        anyhow!(
            "this dispatch of {tool} is outward, this execution CAN send it, and the outward \
                 record it would reconcile against cannot be named ({reason}); it is refused \
                 rather than committed as `None`, because a worker reading that `None` would \
                 find no record to ask, call a live send 'nothing left', and send it a second \
                 time"
        )
    })?;

    // The SAME identity the dispatch stamps as `__execution_id` and the same one
    // a recovering host locates `coding_ledger.json` under — read off
    // `run_identity` rather than off `ctx`, because that cell is what
    // `executor.rs`'s compiled-dispatch block reads and a second source for one
    // value is how the minted id and the journalled one come apart.
    let execution_id = executors
        .run_identity
        .get()
        .execution_id
        .clone()
        .unwrap_or_default();

    let usable_effect_id = effect_id.is_some_and(|raw| EffectId::parse(raw).is_ok());
    if !usable_effect_id && durable_id_required {
        return Err(anyhow!(
            "this dispatch of {tool} has no usable stable tool-call identity; the stateless \
             loop refuses it before firing because it is outward or is not positively \
             retry-safe local work, so no restart could prove whether it already happened"
        ));
    }
    Ok(pending_effect_row(
        effect_id,
        &execution_id,
        tool,
        retry_safety,
        // Through the same constructor the re-derivation uses, so the two
        // values are comparable rather than merely similarly named. See
        // `gate_arguments_fingerprint` for what an empty one means.
        || gate_arguments_fingerprint(executors, history, candidate),
        reconcile_ref,
    ))
}

fn retry_safety_for_tool(tool: &str, executors: &ActionExecutors) -> RetrySafety {
    declared_retry_safety(
        tool,
        executors
            .effective_capability_registry_snapshot()
            .as_deref(),
    )
}

/// Whether the selected driver needs a durable identity for this dispatch.
///
/// Exactly one stateless exception exists: retry-safe work the classifier
/// positively identifies as non-outward. `Unscoped` is deliberately not folded
/// into that exception merely because both serialize to `reconcile_ref: None`:
/// it is still an outward action, while the in-process arm is separately exempt
/// because it never recovers through the effect ledger.
fn effect_id_required_for_dispatch(
    posture: EffectIdentityPosture,
    retry_safety: RetrySafety,
    reconcile_ref: &DispatchReconcileRef,
) -> bool {
    matches!(posture, EffectIdentityPosture::Durable)
        && (retry_safety != RetrySafety::RetrySafe
            || !matches!(reconcile_ref, DispatchReconcileRef::NotOutward { .. }))
}

/// Compose the committed row, or answer `None` for a dispatch that cannot be
/// keyed.
///
/// Split from [`gate_pending_effect`] because everything above it needs a live
/// `ActionExecutors` and `AgenticContext` — neither assemblable in a unit test —
/// while this is the whole of what the row SAYS. The fingerprint arrives as a
/// closure so the split costs nothing on the path that never reaches it: a
/// candidate with no parseable id mints no row, so it must not pay for an HMAC
/// key load and a canonical-JSON encode either.
/// What this capability declares about being run a second time.
///
/// # The default is conservative on purpose, and stays that way
///
/// An unlabelled effect that read as retry-safe would be re-fired on sight, so
/// everything not named here is [`RetrySafety::NotRetrySafe`]. Two derivations
/// were considered and are recorded as REFUSED rather than merely absent:
///
/// - **`is_parallelizable_read_only_action`.** Parallel admission answers "may
///   this run beside another call", which is not the same question as "may this
///   run twice".
/// - **A parent pack's `reliability` block for a generated leaf.** Reliability
///   is resolved at action grain. `whatsapp` reads may be safe while
///   `messages send` is not, so a leaf uses only its own native-action schema.
///   Pack metadata is accepted only for an exact pack with no action catalog.
///
/// # Why a coding job is `Reattachable`, and what carries the rule
///
/// The design's rule is *"`EffectIndeterminate` for a coding job means reattach,
/// never re-fire and never straight to the user"*. It is in force, and it took
/// three pieces that landed separately: this declaration, the ledger's writer
/// (`driver_worker::record_batch_intents`, on the far side of this phase's
/// gate/fire seam), and a `reattach_ref` on the intent — which
/// [`pending_effect_row`] mints, right here in the gate.
///
/// The ref is a **coding invocation id**, not a session handle, and that is the
/// whole of why the gate can write it. See [`pending_effect_row`] for the mint
/// and `super::super::effects::PendingEffect::reattach_ref` for the contract.
///
/// # The shape that was planned here, and why it was REFUSED
///
/// The plan of record until 2026-08-29 put the writer in `dispatch` — "beside
/// the dispatch that started it" — as a third out-parameter alongside `settled`
/// carrying `(EffectId, String)` per member, plus a fourth `LoopStateStore`
/// method to reach `EffectLedger::record_reattach_ref`. It is the shape the next
/// reader will independently reinvent, because the session id genuinely does not
/// exist until the job is running, so `dispatch` looks like the only place that
/// can observe it.
///
/// **It writes the ref exactly when it is not needed and never when it is.** An
/// out-parameter is populated only when `dispatch` RETURNS; if `dispatch`
/// returned, the effect has an outcome and `disposition` answers `Adopt`, not
/// `Reattach`. The `Reattach` case is precisely the one where `dispatch` never
/// returned — the worker died mid-job — and that path writes no out-parameter at
/// all.
///
/// What made the gate able to write it is a change of *what the ref names*.
/// `coding_engine::ledger::prepare_coding_invocation` runs BEFORE the job and
/// durably records an `invocation_id`; every session write happens later,
/// because no engine has named a session at the moment the intent is written.
/// The invocation is durable before the job starts, and the invocation id is
/// therefore the thing an intent can carry. Recovery resolves one to the other
/// through `driver_worker::WorkerHost::reattach_state`, and an
/// invocation with no session stays indeterminate rather than guessing.
///
/// ~~The session is durable only once the job returns.~~ **Narrowed
/// 2026-08-29**: `attach_live_invocation_session` records the session as soon as
/// the engine names one — mid-turn, per engine, earliest at the Codex/Grok
/// handshake — so the indeterminate case is now *the engine never named a
/// session*, not *the turn did not finish*. It does not change what the intent
/// can carry: the intent is written before the engine is spawned at all.
fn declared_retry_safety(
    tool: &str,
    registry: Option<&crate::magician_v2::execution::capability::CapabilityRegistry>,
) -> RetrySafety {
    if tool == crate::magician_v2::execution::compiled_handlers::run_coding_task::TOOL_NAME {
        // Not repeatable, but resumable — and `pending_effect_row` gives the row
        // the invocation id that resume goes through.
        //
        // The only capability named here, and checked rather than assumed:
        // `coding_engine::ledger::prepare_coding_invocation` is the sole
        // constructor of a coding invocation, and this handler is its sole
        // PRODUCTION caller (`run_project_checks` calls it from a test fixture
        // only). So there is no second capability whose invocation a minted id
        // would fail to reach.
        return RetrySafety::Reattachable;
    }
    if registry
        .and_then(|registry| registry.resolve_invocation_reliability(tool))
        .is_some_and(|reliability| reliability.read_only || reliability.idempotent)
    {
        return RetrySafety::RetrySafe;
    }
    RetrySafety::NotRetrySafe
}

/// What the gate minted for this member, for the dispatch to hand the handler.
///
/// **Read, never re-derived.** The point of minting in the gate is that the
/// value in the intent row and the value in `coding_ledger.json` are one string
/// carried, so a second call to `coding_invocation_id_for_effect` here — with
/// the same inputs, even — would reintroduce the drift surface the mint exists
/// to remove: the day the two sites read the execution id from different cells,
/// reattach breaks and nothing says so.
///
/// `None` for every member with no row, which is every member the gate could not
/// key. Such a dispatch runs and reattaches to nothing, which is the honest
/// state: there is no ledger row for a resume to read either.
fn member_reattach_ref(member: &GatedMember) -> Option<String> {
    member.row.as_ref().and_then(|row| row.reattach_ref.clone())
}

fn pending_effect_row(
    effect_id: Option<&str>,
    execution_id: &str,
    tool: String,
    retry_safety: RetrySafety,
    arguments_fingerprint: impl FnOnce() -> String,
    reconcile_ref: Option<CommittedActRef>,
) -> Option<PendingEffect> {
    let parsed = effect_id.and_then(|raw| match EffectId::parse(raw) {
        Ok(id) => Some(id),
        Err(error) => {
            warn!(
                tool = %tool,
                error = %error,
                "[GATE] this dispatch's effect id is not one the ledger can key a row by; it \
                 dispatches without a loop-side row"
            );
            None
        },
    });
    let Some(effect_id) = parsed else {
        if reconcile_ref.is_some() {
            warn!(
                tool = %tool,
                "[GATE] an OUTWARD dispatch has no effect id, so no loop-side row names it; the \
                 outward record still identifies the send by what it discloses, but nothing in \
                 this run's committed batch does"
            );
        }
        return None;
    };

    // THE REATTACH REF, minted here and only for a dispatch that declared
    // itself reattachable.
    //
    // Keyed off the SAFETY rather than off the tool name, so the two cannot
    // drift into a row that claims `Reattachable` and names nothing — which
    // `driver_worker::resolve_effects` would hold as indeterminate, and hold
    // silently, since a missing ref reads exactly like a run that never got
    // one. Minted at intent time because that is the only time it is reachable:
    // see `declared_retry_safety` for the dispatch-side shape this replaced.
    //
    // `effect_id` is the second half, and it is what makes the value both
    // STABLE across a re-run of this effect — the id names one turn's one tool
    // call, so the pickup mints the same string and
    // `prepare_coding_invocation` reuses the entry rather than appending a
    // second — and DISTINCT between two coding jobs in one batch, since
    // `PendingBatch::validate` refuses a batch with a duplicate effect id.
    let reattach_ref = match retry_safety {
        RetrySafety::Reattachable => Some(
            crate::magician_v2::execution::coding_engine::ledger::coding_invocation_id_for_effect(
                execution_id,
                effect_id.as_str(),
            ),
        ),
        RetrySafety::RetrySafe | RetrySafety::NotRetrySafe => None,
    };
    Some(PendingEffect {
        effect_id,
        arguments_fingerprint: arguments_fingerprint(),
        tool,
        retry_safety,
        reconcile_ref,
        reattach_ref,
    })
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use crate::magician_v2::execution::capability::{
        CapabilityPackDefinition, CapabilityRegistry, CapabilityReliabilityMetadata,
        ImplementationType, NativeActionSchemaDef,
    };

    fn pending(id: &str) -> PendingEffect {
        PendingEffect {
            effect_id: EffectId::parse(id).expect("a fixture id is well-formed"),
            tool: "browser__click".to_string(),
            arguments_fingerprint: String::new(),
            retry_safety: RetrySafety::NotRetrySafe,
            reconcile_ref: None,
            reattach_ref: None,
        }
    }

    fn reliability_pack(
        name: &str,
        reliability: Option<CapabilityReliabilityMetadata>,
        native_action_schemas: std::collections::HashMap<String, NativeActionSchemaDef>,
    ) -> CapabilityPackDefinition {
        CapabilityPackDefinition {
            name: name.to_string(),
            description: None,
            version: None,
            guide: None,
            native_action_schemas,
            parameters: Vec::new(),
            implementation: ImplementationType::Composite { steps: Vec::new() },
            execution: None,
            auth: None,
            reliability,
            result_projection: None,
        }
    }

    #[test]
    fn retry_safety_resolves_at_action_grain_and_never_leaks_from_a_mixed_pack() {
        let registry = CapabilityRegistry::new();
        registry.set_pack_definition(
            "safe_exact",
            reliability_pack(
                "safe_exact",
                Some(CapabilityReliabilityMetadata {
                    idempotent: true,
                    ..CapabilityReliabilityMetadata::default()
                }),
                std::collections::HashMap::new(),
            ),
        );
        assert_eq!(
            declared_retry_safety("safe_exact", Some(&registry)),
            RetrySafety::RetrySafe
        );

        let mut actions = std::collections::HashMap::new();
        actions.insert(
            "read".to_string(),
            NativeActionSchemaDef {
                reliability: Some(CapabilityReliabilityMetadata {
                    read_only: true,
                    ..CapabilityReliabilityMetadata::default()
                }),
                ..NativeActionSchemaDef::default()
            },
        );
        actions.insert("send".to_string(), NativeActionSchemaDef::default());
        registry.set_pack_definition(
            "mixed",
            reliability_pack(
                "mixed",
                // Deliberately unsafe as a fallback despite this declaration:
                // only the `read` action opted into replay.
                Some(CapabilityReliabilityMetadata {
                    idempotent: true,
                    ..CapabilityReliabilityMetadata::default()
                }),
                actions,
            ),
        );
        assert_eq!(
            declared_retry_safety("mixed__read", Some(&registry)),
            RetrySafety::RetrySafe
        );
        assert_eq!(
            declared_retry_safety("mixed__send", Some(&registry)),
            RetrySafety::NotRetrySafe
        );
        assert_eq!(
            declared_retry_safety("mixed", Some(&registry)),
            RetrySafety::NotRetrySafe,
            "a multi-action container is not itself a reliability-bearing action"
        );
    }

    #[test]
    fn durable_effect_identity_is_required_by_outwardness_not_only_retry_safety() {
        let outward = DispatchReconcileRef::Ref(
            CommittedActRef::new(
                format!("act-{}", "0123456789abcdef".repeat(2)),
                "anonymous",
                "default",
            )
            .expect("fixture act ref"),
        );
        assert!(effect_id_required_for_dispatch(
            EffectIdentityPosture::Durable,
            RetrySafety::RetrySafe,
            &outward,
        ));
        assert!(effect_id_required_for_dispatch(
            EffectIdentityPosture::Durable,
            RetrySafety::RetrySafe,
            &DispatchReconcileRef::Unscoped {
                reason: "the outward gate will refuse this process".to_string(),
            },
        ));
        assert!(!effect_id_required_for_dispatch(
            EffectIdentityPosture::Durable,
            RetrySafety::RetrySafe,
            &DispatchReconcileRef::NotOutward {
                reason: "a local read".to_string(),
            },
        ));
        assert!(effect_id_required_for_dispatch(
            EffectIdentityPosture::Durable,
            RetrySafety::NotRetrySafe,
            &DispatchReconcileRef::NotOutward {
                reason: "a local mutation".to_string(),
            },
        ));
        assert!(!effect_id_required_for_dispatch(
            EffectIdentityPosture::InProcess,
            RetrySafety::NotRetrySafe,
            &outward,
        ));
    }

    /// The batch the `Gate` publishes has to be one the store will accept, and
    /// the arithmetic that decides whether it is lives in one method.
    #[test]
    fn a_parallel_slice_counts_the_rows_it_admitted_not_the_members_it_was_offered() {
        // The shape that breaks it: three candidates joined the slice and only
        // one of them could be keyed — the other two had no usable effect id,
        // which `effect_id_for_candidate` really does answer on the
        // text-fallback paths. Counting the OFFER gives `admitted = 3` against
        // a two-row batch, and `PendingBatch::validate` refuses that as
        // `ParallelSliceOverruns`: a batch that describes real dispatches
        // correctly would be rejected at commit, and the run would fail for a
        // reason nothing that happened caused.
        let mut gated = GatedBatch::new();
        let mut out = None;
        gated.admit(Some(pending("llm-1:tool:primary")), 7, &mut out);
        gated.admit_parallel_slice(
            vec![None, Some(pending("llm-1:tool:parallel-a")), None],
            7,
            &mut out,
        );

        let batch = out.expect("a batch with members is published");
        assert_eq!(batch.effects.len(), 2);
        assert_eq!(batch.mode, BatchMode::Parallel { admitted: 1 });
        assert_eq!(batch.iteration, 7);
        assert_eq!(batch.phase, Phase::Apply);
        batch
            .validate()
            .expect("the published batch must be one the store would accept");
    }

    /// Nothing gated is `None`, not an empty batch.
    #[test]
    fn a_gate_that_admitted_nothing_publishes_nothing() {
        // `PendingBatch::validate` refuses an empty batch outright — "an intent
        // commit with nothing to fire" — so publishing `Some(empty)` would make
        // every commit of a phase that gated nothing fail. `None` is what
        // `LoopState::pending` already means by nothing in flight.
        //
        // Each admission path is asked SEPARATELY, on its own `GatedBatch` and
        // its own out-parameter, and each is handed an out-parameter that is
        // already `Some`. An earlier cut ran both against one fresh `None`,
        // where the second call covered for the first — `admit(None, …)` proved
        // nothing, because `admit_parallel_slice` happened to run afterwards and
        // answer for it — and neither call was ever asked to CLEAR a value.
        let stale = || {
            Some(PendingBatch {
                iteration: 0,
                phase: Phase::Apply,
                mode: BatchMode::Sequential,
                effects: vec![pending("llm-0:tool:from-an-earlier-turn")],
            })
        };

        let mut only_admit = GatedBatch::new();
        let mut out = stale();
        only_admit.admit(None, 1, &mut out);
        assert!(
            out.is_none(),
            "an admission path that admitted nothing must publish nothing, including over a \
             value the caller already held"
        );

        let mut only_slice = GatedBatch::new();
        let mut out = stale();
        only_slice.admit_parallel_slice(vec![None, None], 1, &mut out);
        assert!(out.is_none());

        // And the sequential shape is reachable: a batch with no parallel slice
        // must not claim one, or a resuming worker reads members that settled
        // in order as members that settled out of it.
        let mut gated = GatedBatch::new();
        let mut out = None;
        gated.admit(Some(pending("llm-1:tool:only")), 1, &mut out);
        let batch = out.expect("one admitted member publishes a batch");
        assert_eq!(batch.mode, BatchMode::Sequential);
        batch.validate().expect("valid");
    }

    /// A slice that contributed rows while the primary contributed none.
    #[test]
    fn the_parallel_count_is_a_count_and_not_a_range_of_positions() {
        // The shape the old comment on `admitted_in_parallel` said was
        // impossible: it claimed the parallel members occupy `1..=admitted`,
        // "the primary at index 0". `gate_pending_effect` answers `Ok(None)` for
        // a candidate with no usable effect id, `admit` then pushes nothing, and
        // the parallel members start at index 0 instead.
        //
        // Nothing indexes those positions today, so this pins the count and the
        // shape a reader may rely on — not offsets that would be invented here
        // rather than recorded.
        let mut gated = GatedBatch::new();
        let mut out = None;
        gated.admit(None, 3, &mut out);
        gated.admit_parallel_slice(
            vec![
                Some(pending("llm-1:tool:parallel-a")),
                Some(pending("llm-1:tool:parallel-b")),
            ],
            3,
            &mut out,
        );
        // And the sequential tail, which is admitted AFTER the slice — the other
        // half of why an offset range cannot be recovered from the count.
        gated.admit(Some(pending("llm-1:tool:tail")), 3, &mut out);

        let batch = out.expect("three rows were admitted");
        assert_eq!(batch.effects.len(), 3);
        assert_eq!(batch.mode, BatchMode::Parallel { admitted: 2 });
        assert_eq!(
            batch.effects[0].effect_id.as_str(),
            "llm-1:tool:parallel-a",
            "a dropped primary means index 0 is a PARALLEL member, which is why the count is \
             not a position"
        );
        batch
            .validate()
            .expect("a slice that overran its rows would be refused here");
    }

    /// An outward dispatch carries the ref it will reconcile against; one that
    /// cannot be keyed carries nothing at all.
    #[test]
    fn only_a_keyable_dispatch_becomes_a_row_and_an_outward_one_keeps_its_ref() {
        // `gate_pending_effect`'s two remaining outcomes once the classifier has
        // answered, which is the part of it that needs no executor. The
        // fingerprint closure is asserted NOT to run on the unkeyable path: it
        // loads a scoped HMAC key, and paying for one to build a row that is
        // then discarded is the kind of cost that only shows up under load.
        let act_ref = CommittedActRef::new(
            format!("act-{}", "0123456789abcdef".repeat(2)),
            "anonymous",
            "default",
        )
        .expect("the fixture ref must be the shape derive_act_ref mints");

        let row = pending_effect_row(
            Some("llm-1:tool:call-1"),
            "exec-1",
            "gmail__send".to_string(),
            RetrySafety::NotRetrySafe,
            || "fp-1".to_string(),
            Some(act_ref.clone()),
        )
        .expect("a keyable dispatch mints a row");
        assert_eq!(row.tool, "gmail__send");
        assert_eq!(row.arguments_fingerprint, "fp-1");
        assert_eq!(
            row.reconcile_ref.as_ref(),
            Some(&act_ref),
            "an outward row that lost its ref reads as `not outward`, and a worker reconciling \
             it finds no record — which is a licence to re-send a live message"
        );
        assert_eq!(
            row.retry_safety,
            RetrySafety::NotRetrySafe,
            "an unlabelled effect that read as retry-safe would be re-fired on sight"
        );

        // At this constructor boundary, no id at all and an id the ledger cannot
        // key both mint no row. The driver-aware gate above decides whether that
        // is admissible: a durable outward dispatch is refused, while the
        // in-process rollback arm and positively local retry-safe work remain
        // able to reach this `None`.
        for unkeyable in [None, Some("not-an-effect-id"), Some("llm-1:tool:")] {
            assert!(
                pending_effect_row(
                    unkeyable,
                    "exec-1",
                    "gmail__send".to_string(),
                    RetrySafety::NotRetrySafe,
                    || panic!("the fingerprint must not be computed for a row that is discarded"),
                    Some(act_ref.clone()),
                )
                .is_none(),
                "a dispatch with no usable effect id gets no loop-side row: {unkeyable:?}"
            );
        }
    }

    /// The gate names the job a coding dispatch reattaches through, and names a
    /// different one for every dispatch.
    ///
    /// Three properties, and the third is the one a weaker test would miss.
    /// *Present* on a coding row and *absent* everywhere else are easy;
    /// **stable across a re-gate and distinct between two members** is the whole
    /// bug in a different costume, because a mint that drifted would leave the
    /// pickup naming an invocation nobody prepared, and a mint that collided
    /// would resume a session belonging to the other job.
    #[test]
    fn the_gate_names_a_coding_dispatchs_invocation_and_only_a_coding_dispatchs() {
        let coding = |effect_id: &str, execution_id: &str| {
            pending_effect_row(
                Some(effect_id),
                execution_id,
                crate::magician_v2::execution::compiled_handlers::run_coding_task::TOOL_NAME
                    .to_string(),
                RetrySafety::Reattachable,
                String::new,
                None,
            )
            .expect("a keyable dispatch mints a row")
        };

        let first = coding("llm-1:tool:code-1", "exec-1");
        assert_eq!(first.retry_safety, RetrySafety::Reattachable);
        let named = first
            .reattach_ref
            .as_deref()
            .expect("a reattachable row that names no job is held, never resumed");
        assert_eq!(
            named,
            crate::magician_v2::execution::coding_engine::ledger::coding_invocation_id_for_effect(
                "exec-1",
                "llm-1:tool:code-1"
            ),
            "the gate and the coding engine must name ONE value, or the id in the effect ledger \
             is not the id on disk"
        );

        // Re-gated after a pickup: the same string, or `prepare_coding_invocation`
        // appends a second entry and the ref points at an invocation that never
        // reports.
        assert_eq!(
            coding("llm-1:tool:code-1", "exec-1")
                .reattach_ref
                .as_deref(),
            Some(named)
        );
        // A sibling in the same batch, and the same run.
        assert_ne!(
            coding("llm-1:tool:code-2", "exec-1")
                .reattach_ref
                .as_deref(),
            Some(named)
        );
        // A different run replaying the same turn — the coding ledger is
        // per-execution, so two executions must not share an invocation id.
        assert_ne!(
            coding("llm-1:tool:code-1", "exec-2")
                .reattach_ref
                .as_deref(),
            Some(named)
        );

        // And nothing else gets one. A row that claimed a job it has no way to
        // resume would be held by `resolve_effects` rather than reconciled,
        // which for an outward send is strictly worse than asking the record.
        assert_eq!(
            pending_effect_row(
                Some("llm-1:tool:send-1"),
                "exec-1",
                "gmail__send".to_string(),
                RetrySafety::NotRetrySafe,
                String::new,
                None,
            )
            .expect("a keyable dispatch mints a row")
            .reattach_ref,
            None
        );
    }

    /// The `Gate` and the re-derivation must agree, and they agree only because
    /// the digest reads three of nine arguments.
    #[test]
    fn the_gate_and_the_rederivation_fingerprint_the_same_dispatch_the_same_way() {
        // `EffectLedger::authorize_refire` compares the committed fingerprint
        // with the one `WorkerHost::rederive_dispatch` computes, and refuses a
        // mismatch as `ArgumentsConflict`. The two calls deliberately pass
        // DIFFERENT `branch_id` and `source_surface` — `{id}:gate` against
        // `{raw}:rederive`, `agentic_gate_commit` against
        // `stateless_worker_rederive` — so that a log line reading one identity
        // cannot be mistaken for the other.
        //
        // That is safe only while `LlmToolLineageIdentity::new` folds none of
        // them into the digest: it reads the workspace, the trace scope and the
        // canonical arguments, and nothing else. Fold one more in — the obvious
        // candidate is `model_tool_call_id`, which sits right beside them in the
        // argument list — and every honest re-derivation becomes
        // `ArgumentsConflict` and every reconciled re-fire fails.
        //
        // `llm_tool_lineage::tests::multiple_and_parallel_tool_calls_keep_distinct
        // _provider_identities` already covers the `model_tool_call_id` and
        // `branch_id` axes as its closing assertion. This is here because it
        // covers `source_surface` too — the third value the two callers differ
        // on — and because it is the only test NAMED for the property the
        // re-fire gate depends on, so the assertion cannot be tidied away as
        // incidental to a test about something else.
        let tmp = tempfile::tempdir().expect("temp dir");
        let workspace = crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
            tmp.path().to_path_buf(),
        );
        let context = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("anonymous", "default"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        );
        let arguments = serde_json::json!({ "to": "someone@example.com", "subject": "hello" });

        let gate = LlmToolLineageIdentity::new(
            &workspace,
            context.clone(),
            "call-1",
            "call-1:gate",
            "chat".to_string(),
            "agentic_gate_commit".to_string(),
            "gmail__send",
            Some("gmail".to_string()),
            &arguments,
        )
        .expect("the gate's identity");
        let rederived = LlmToolLineageIdentity::new(
            &workspace,
            context.clone(),
            "call-1",
            "call-1:rederive",
            "chat".to_string(),
            "stateless_worker_rederive".to_string(),
            "gmail__send",
            Some("gmail".to_string()),
            &arguments,
        )
        .expect("the re-derivation's identity");

        assert_eq!(
            gate.arguments_fingerprint, rederived.arguments_fingerprint,
            "the two spellings of one dispatch must fingerprint alike, or `authorize_refire` \
             refuses every faithful re-derivation"
        );
        assert!(
            !gate.arguments_fingerprint.is_empty(),
            "an empty pair would compare equal without proving anything — `authorize_refire` \
             refuses those outright for exactly that reason"
        );

        // And the fingerprint is not indifferent to everything: different
        // ARGUMENTS must differ, or the check above passes because the digest
        // reads nothing at all.
        let other_arguments = LlmToolLineageIdentity::new(
            &workspace,
            context,
            "call-1",
            "call-1:gate",
            "chat".to_string(),
            "agentic_gate_commit".to_string(),
            "gmail__send",
            Some("gmail".to_string()),
            &serde_json::json!({ "to": "someone-else@example.com", "subject": "hello" }),
        )
        .expect("a third identity");
        assert_ne!(
            gate.arguments_fingerprint, other_arguments.arguments_fingerprint,
            "a digest that ignored the arguments would make the agreement above vacuous"
        );
    }

    /// A durable row names the capability, never the arguments.
    #[test]
    fn the_row_names_the_capability_and_carries_no_argument_bytes() {
        // `PendingEffect` has no `args` field precisely so a durable row cannot
        // write secret values to disk. `action_signature` renders every
        // parameter value inline and is right there in this file, so it is the
        // obvious wrong thing to reach for; this pins that the row's `tool` is
        // the capability coordinate the outward classifier and
        // `refuse_unless_same_dispatch` both key on, and that the secret does
        // not travel with it.
        let params: std::collections::HashMap<String, serde_json::Value> = [(
            "api_key".to_string(),
            serde_json::json!("sk-live-do-not-persist"),
        )]
        .into_iter()
        .collect();
        let action = ExecutableAction::Pack {
            capability_name: "gmail__send".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "test".to_string(),
            },
            resolved_params: params,
        };
        // One assertion, not two. `assert_eq!` already fixes `tool` exactly, so
        // a following `assert!(!tool.contains(secret))` could not fire whatever
        // production did — an assertion that cannot fail reads as protection and
        // is not any.
        let tool = pending_tool_name(&action);
        assert_eq!(tool, "gmail__send");
        // The claim against the value that WOULD have carried the bytes, so this
        // test fails rather than passes if `pending_tool_name` is ever pointed at
        // the signature. This is the assertion doing the work.
        assert!(action_signature(&action).contains("sk-live-do-not-persist"));
    }
}

#[cfg(test)]
mod parallel_protective_tests {
    use super::*;
    use crate::magician_v2::execution::agentic::loop_detector::{
        EnvironmentFingerprint, LoopDetector,
    };

    /// A detector holding exactly `len` recorded fingerprints and NO recorded
    /// clears.
    ///
    /// Seeded through `seed_history_for_testing` rather than by driving
    /// `record_action_result`, because driving the real recorder would make the
    /// fixture depend on the similarity thresholds — which are what decide
    /// progress, not what this function is being asked about.
    ///
    /// The seeder counts no clears, which is right for every case below that is
    /// about length. The cases that are about a *clear* use
    /// `member_with_clears`, which supplies the count the recorder would have
    /// left behind.
    fn detector_of_len(len: usize) -> LoopDetector {
        let mut detector = LoopDetector::new();
        let fingerprint = EnvironmentFingerprint::from_state(&EnvironmentState::Uninitialized);
        detector.seed_history_for_testing(Vec::new(), vec![fingerprint; len]);
        assert_eq!(
            detector.recorded_len(),
            len,
            "the fixture must actually produce the length the case is about"
        );
        detector
    }

    fn member(detector_len: usize, recovery: Option<LoopRecoveryContext>) -> LoopProtectiveState {
        LoopProtectiveState {
            loop_detector: detector_of_len(detector_len),
            loop_recovery_context: recovery,
            ..LoopProtectiveState::default()
        }
    }

    /// A member of `detector_len` whose fork recorded `clears` clears.
    ///
    /// The two halves come from different fixtures on purpose.
    /// `seed_history_for_testing` builds a history without going through the
    /// recorder, so it counts nothing; `mark_history_cleared_for_testing`
    /// supplies the count. Together they reproduce the shape no length test can
    /// recognise — the content-modifying path of `record_action_result`, which
    /// clears `state_history`, then records one state and one action, and so
    /// hands back a detector LONGER than the base it was cloned from.
    fn member_with_clears(detector_len: usize, clears: u64) -> LoopProtectiveState {
        let mut detector = detector_of_len(detector_len);
        for _ in 0..clears {
            detector.mark_history_cleared_for_testing();
        }
        LoopProtectiveState {
            loop_detector: detector,
            ..LoopProtectiveState::default()
        }
    }

    /// A detector of `len` carrying a bound nothing else in the test uses, so
    /// two detectors of the same length can be told apart.
    ///
    /// `recorded_len()` and `history_clears()` are the only public readers of a
    /// detector's history, and they are exactly what the fold keys on — so a
    /// test that asked only those could not distinguish "the base was left
    /// alone" from "a member of equal length and equal clear count was adopted".
    /// `max_history` is private and read back through the derived `Serialize`,
    /// which is the narrowest way to see identity without widening production
    /// visibility for a test.
    fn tagged_detector(len: usize, bound: usize) -> LoopDetector {
        let mut detector = LoopDetector::with_max_history(bound);
        let fingerprint = EnvironmentFingerprint::from_state(&EnvironmentState::Uninitialized);
        // Seeds past `bound` on purpose where the case needs it:
        // `seed_history_for_testing` pushes without truncating, and the fold
        // never re-records, so the bound is a label here and not a cap.
        detector.seed_history_for_testing(Vec::new(), vec![fingerprint; len]);
        assert_eq!(detector.recorded_len(), len);
        detector
    }

    fn bound_of(detector: &LoopDetector) -> u64 {
        serde_json::to_value(detector)
            .ok()
            .and_then(|encoded| {
                encoded
                    .get("max_history")
                    .and_then(serde_json::Value::as_u64)
            })
            .expect("`LoopDetector` derives `Serialize` and carries its bound")
    }

    fn pressure() -> LoopRecoveryContext {
        LoopRecoveryContext {
            detection_type: "action_cycle".to_string(),
            action_signature: "file:read".to_string(),
            recommendation: "try something else".to_string(),
            iteration_detected: 4,
            alternate_capabilities: Vec::new(),
        }
    }

    #[test]
    fn a_member_that_cleared_the_pressure_signal_clears_it_on_the_run() {
        // The half of the bug that is exactly recoverable: the dispatch path's
        // only write to `loop_recovery_context` is `= None`, so a member holding
        // `None` where the base holds `Some` can only have cleared it.
        let mut base = member(2, Some(pressure()));
        let members = vec![member(2, Some(pressure())), member(3, None)];

        adopt_parallel_member_protective_state(&mut base, members);

        assert!(
            base.loop_recovery_context.is_none(),
            "a successful parallel action must clear the stale loop-pressure signal, which is \
             what dropping the forks used to prevent"
        );
    }

    #[test]
    fn no_member_clearing_leaves_the_pressure_signal_standing() {
        // The other direction, and it has to be asserted separately: a rule that
        // cleared unconditionally would pass the test above and would throw away
        // a live cycle signal every time a read-only slice ran.
        let mut base = member(2, Some(pressure()));
        let members = vec![member(3, Some(pressure())), member(3, Some(pressure()))];

        adopt_parallel_member_protective_state(&mut base, members);

        assert!(base.loop_recovery_context.is_some());
    }

    #[test]
    fn the_observation_a_member_recorded_reaches_the_runs_detector() {
        // The bug itself. Before the fold, `base` kept its own detector and the
        // member's recorded fingerprint went nowhere, so a parallel-dispatched
        // action never registered in cycle detection at all.
        let mut base = member(2, None);
        let members = vec![member(2, None), member(3, None)];

        adopt_parallel_member_protective_state(&mut base, members);

        assert_eq!(
            base.loop_detector.recorded_len(),
            3,
            "the member that recorded must be the one adopted; dropping it is the shipped bug"
        );
    }

    #[test]
    fn a_fork_shorter_than_the_base_wins_over_a_longer_sibling() {
        // A fork can only shrink by clearing, and a clear means a member made
        // progress. Keeping the longer sibling instead would leave the detector
        // holding a history a correct sequential fold had already thrown away —
        // which makes a cycle fire on a run that was progressing, the one
        // direction this fold must never err in.
        let mut base = member(6, None);
        let members = vec![member(1, None), member(7, None)];

        adopt_parallel_member_protective_state(&mut base, members);

        assert_eq!(
            base.loop_detector.recorded_len(),
            1,
            "the proven clear must survive a sibling that merely appended"
        );
    }

    #[test]
    fn a_fork_that_cleared_wins_even_though_it_came_back_longer() {
        // THE DEFECT, and it is invisible to any test written in lengths. The
        // content-modifying path of `record_action_result` clears
        // `state_history` only and then records a state and an action, so
        // against a base of `S=1, A=4` it returns length 6 — LONGER than the
        // base's 5. A fold that inferred "cleared" from "shorter than the base"
        // sees an append, falls through to "adopt the longest", and installs
        // the no-progress sibling's history instead. Nothing fails; the run
        // just re-arms the false `state_loop` detection the clear existed to
        // prevent.
        //
        // Latent when this was written — `is_content_modifying` requires
        // `category == "browser"` and the parallel gate admits only File-reads
        // and read-only Packs — and this test is what makes widening either set
        // safe rather than silently wrong.
        let mut base = member_with_clears(5, 0);
        let members = vec![member_with_clears(6, 1), member_with_clears(7, 0)];

        adopt_parallel_member_protective_state(&mut base, members);

        assert_eq!(
            base.loop_detector.recorded_len(),
            6,
            "the fork that CLEARED must be adopted over a longer sibling that only appended; \
             reading the length instead of the fact picks the sibling"
        );
    }

    #[test]
    fn the_last_fork_that_cleared_wins_over_an_earlier_longer_one() {
        // Two clears in one slice. The rule is the same as it was for the
        // length disjunct — take the LAST, which keeps that clear and loses the
        // members after it, the direction this fold is allowed to err in.
        //
        // The lengths are arranged so the answer cannot come from case 2: the
        // earlier fork is the longer one, so "adopt the longest" would pick
        // index 0 and this asserts index 1.
        let mut base = member_with_clears(5, 0);
        let members = vec![member_with_clears(9, 1), member_with_clears(6, 1)];

        adopt_parallel_member_protective_state(&mut base, members);

        assert_eq!(
            base.loop_detector.recorded_len(),
            6,
            "ties among proven clears go to the last member"
        );
    }

    #[test]
    fn a_base_that_had_already_cleared_is_the_reference_and_not_zero() {
        // The count is a DELTA against the base the forks were cloned from, not
        // an absolute. A base that cleared earlier in the run hands every fork a
        // non-zero count for free, and none of them proved anything by carrying
        // it.
        //
        // The lengths make the two readings disagree: keyed on the delta, no
        // member cleared and case 2 adopts the longest (index 0, length 8);
        // keyed on `history_clears() > 0`, every member looks cleared and the
        // last one wins (index 1, length 7).
        let mut base = member_with_clears(6, 1);
        let members = vec![member_with_clears(8, 1), member_with_clears(7, 1)];

        adopt_parallel_member_protective_state(&mut base, members);

        assert_eq!(
            base.loop_detector.recorded_len(),
            8,
            "a clear the BASE performed before the fork is not evidence about any member"
        );
    }

    #[test]
    fn a_slice_where_nothing_recorded_still_adopts_a_member() {
        // NOT a no-op, and the earlier comment here claimed one. There is no
        // no-op branch: after the empty guard the fold always executes
        // `loop_protective.loop_detector = members.swap_remove(chosen)`, so a
        // slice where every fork equals the base still replaces the base's
        // detector with the last member's.
        //
        // Value-preserving in production, because the members are clones of the
        // base — but "equal by value" is not "left alone", and the difference
        // becomes load-bearing the moment a member's detector can differ from
        // the base's in anything `recorded_len()` does not see. The bound is
        // that difference here.
        let mut base = member(4, None);
        base.loop_detector = tagged_detector(4, 11);
        let members = vec![
            LoopProtectiveState {
                loop_detector: tagged_detector(4, 22),
                ..LoopProtectiveState::default()
            },
            LoopProtectiveState {
                loop_detector: tagged_detector(4, 33),
                ..LoopProtectiveState::default()
            },
        ];

        adopt_parallel_member_protective_state(&mut base, members);

        assert_eq!(base.loop_detector.recorded_len(), 4);
        assert_eq!(
            bound_of(&base.loop_detector),
            33,
            "ties go to the LAST member and the adoption is total; a test that only read \
             `recorded_len()` here would pass for a no-op the function does not have"
        );
    }

    #[test]
    fn a_defaulted_member_erases_the_base_which_is_why_the_caller_filters() {
        // NOT a property this function should have. It is pinned because it is
        // the reason the join pushes only members whose dispatch returned `Ok`.
        //
        // `execute_direct_path_on_scheduler_root` moves the bundle out with
        // `std::mem::take` (`executor.rs:18072`) and swaps it back only after
        // the scheduled job returns (`executor.rs:18127`). The two `?`s between
        // those points — the `schedule(..)` whose worker channel is closed, and
        // the `.await` whose oneshot was dropped — return without reconciling,
        // so the member's bundle is still `LoopProtectiveState::default()`.
        //
        // This function cannot tell that placeholder from a fork that cleared,
        // and reads it as two positive claims at once: the pressure signal is
        // cleared, and an empty `LoopDetector` is adopted as a "proven clear".
        // On the cancellation arm, when a manual pause is also requested,
        // `finish_cancelled_agentic_execution` writes that state into the pause
        // it persists, so the run resumes with loop detection reset.
        //
        // If the fold is ever hardened to ignore a pristine member, delete this
        // test AND the caller's filter together — not one of them.
        let mut base = member(6, Some(pressure()));

        adopt_parallel_member_protective_state(&mut base, vec![LoopProtectiveState::default()]);

        assert_eq!(
            base.loop_detector.recorded_len(),
            0,
            "an un-reconciled placeholder is read as a proven clear"
        );
        assert!(
            base.loop_recovery_context.is_none(),
            "and as a cleared pressure signal"
        );
    }

    #[test]
    fn an_empty_slice_changes_nothing() {
        // A fold that indexed member zero unconditionally would panic inside a
        // live dispatch join, so the empty case is a guard and not a formality.
        let mut base = member(5, Some(pressure()));

        adopt_parallel_member_protective_state(&mut base, Vec::new());

        assert_eq!(base.loop_detector.recorded_len(), 5);
        assert!(base.loop_recovery_context.is_some());
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use crate::magician_v2::execution::agentic::run_loop::effects::PlannedEffect;

    fn row(effect_id: &str) -> PendingEffect {
        PendingEffect {
            effect_id: EffectId::parse(effect_id).expect("a fixture id is well-formed"),
            tool: "browser__click".to_string(),
            arguments_fingerprint: String::new(),
            retry_safety: RetrySafety::NotRetrySafe,
            reconcile_ref: None,
            reattach_ref: None,
        }
    }

    /// A member the gate keyed, so the plan can say something about it.
    fn member(effect_id: &str) -> GatedMember {
        let action =
            ExecutableAction::Http(crate::magician_v2::execution::actions::HttpAction::get(
                "https://example.invalid/resource",
            ));
        GatedMember {
            candidate: ActionCandidate::new(1, 0.9, action),
            effect_id: Some(effect_id.to_string()),
            row: Some(row(effect_id)),
        }
    }

    /// A member the gate could NOT key — `gate_pending_effect`'s `Ok(None)`. It
    /// dispatches and has no ledger row, so no plan can name it.
    fn unkeyable_member() -> GatedMember {
        let action =
            ExecutableAction::Http(crate::magician_v2::execution::actions::HttpAction::get(
                "https://example.invalid/unkeyed",
            ));
        GatedMember {
            candidate: ActionCandidate::new(1, 0.9, action),
            effect_id: None,
            row: None,
        }
    }

    fn planned(effect_id: &str, action: EffectAction) -> PlannedEffect {
        PlannedEffect {
            effect_id: EffectId::parse(effect_id).expect("a fixture id is well-formed"),
            action,
        }
    }

    fn fires(dispositions: &[MemberDispatch]) -> usize {
        dispositions
            .iter()
            .filter(|disposition| disposition.fires())
            .count()
    }

    fn tagged_member_history(effect_id: &str, tag: &str) -> ExecutionHistory {
        let mut history = ExecutionHistory::new();
        let state = EnvironmentState::Uninitialized;
        record_member_not_fired_in_history(
            &mut history,
            &member(effect_id),
            &MemberDispatch::AdoptRecorded {
                result: None,
                succeeded: true,
            },
            &state,
            &state,
            1,
        );
        history
            .iterations
            .last_mut()
            .expect("the adopted result writes one history record")
            .llm_reasoning = Some(tag.to_string());
        history.artifacts.push(
            crate::magician_v2::execution::agentic::types::Artifact::text(
                format!("{tag}.txt"),
                tag,
            ),
        );
        history
    }

    #[test]
    fn a_mixed_recovery_parallel_slice_merges_history_in_admitted_order() {
        // Member 2 is answered immediately by the plan while member 1 has to
        // wait for `join_all`. This is the production timing that used to append
        // member 2 first even though the assistant emitted member 1 first.
        let mut staged: Vec<Option<ExecutionHistory>> = (0..=2).map(|_| None).collect();
        staged[2] = Some(tagged_member_history("llm-1:tool:second", "adopted-second"));
        staged[1] = Some(tagged_member_history("llm-1:tool:first", "fired-first"));
        let mut history = ExecutionHistory::new();

        merge_parallel_member_histories(&mut history, &mut staged);

        assert_eq!(
            history
                .iterations
                .iter()
                .map(|record| record.llm_reasoning.as_deref().unwrap_or(""))
                .collect::<Vec<_>>(),
            vec!["fired-first", "adopted-second"],
            "recovery must preserve assistant tool-call order even when the later member is \
             available before the earlier member's job completes"
        );
        assert_eq!(
            history
                .artifacts
                .iter()
                .map(|artifact| artifact.name.as_str())
                .collect::<Vec<_>>(),
            vec!["fired-first.txt", "adopted-second.txt"],
            "parallel artifacts follow the same stable member order as their results"
        );
        assert!(
            staged.iter().all(Option::is_none),
            "the merge consumes every staged history exactly once"
        );
    }

    #[test]
    fn an_adopted_parallel_failure_still_stops_the_mutating_tail() {
        let adopted_failure = MemberDispatch::AdoptRecorded {
            result: None,
            succeeded: false,
        };
        let adopted_success = MemberDispatch::AdoptRecorded {
            result: None,
            succeeded: true,
        };

        assert!(adopted_failure.recorded_failure());
        assert!(!adopted_success.recorded_failure());
        assert!(!MemberDispatch::SettleAsAlreadyFired {
            by_this_attempt: false,
        }
        .recorded_failure());
        assert!(should_skip_serial_follow_ups(
            true,
            adopted_failure.recorded_failure(),
        ));
    }

    #[test]
    fn an_error_settles_every_unreached_intent_before_it_escapes() {
        let members = vec![
            member("llm-1:tool:failed-prefix"),
            member("llm-1:tool:unreached-one"),
            member("llm-1:tool:unreached-two"),
        ];
        let dispositions = vec![
            MemberDispatch::Fire,
            MemberDispatch::Fire,
            MemberDispatch::Fire,
        ];
        let mut settled = Vec::new();
        let mut history = ExecutionHistory::new();
        let state = EnvironmentState::Uninitialized;

        let error = settle_unreached_before_error(
            &mut settled,
            &members,
            &dispositions,
            &mut history,
            &state,
            &state,
            1,
            1,
            "the prefix errored",
            anyhow!("dispatch failed"),
        );

        assert_eq!(error.to_string(), "dispatch failed");
        assert_eq!(
            settled
                .iter()
                .map(|(effect_id, _)| effect_id.as_str())
                .collect::<Vec<_>>(),
            vec!["llm-1:tool:unreached-one", "llm-1:tool:unreached-two"],
            "only the suffix the executor never reached is settled here"
        );
        assert!(
            settled
                .iter()
                .all(|(_, outcome)| matches!(outcome, EffectOutcome::NotDispatched { .. })),
            "an intent for work this live dispatcher never called has positive not-dispatched \
             evidence, not an indeterminate outcome"
        );
    }

    /// THE ACCEPTANCE CASE for the whole plan.
    ///
    /// A re-entered `Apply` whose driver says one member already left and
    /// another has a result on the ledger must dispatch **neither**, and must
    /// still dispatch the member nothing has answered for. The assertion is on
    /// the per-member verdict — what the executor is about to be asked to do —
    /// and not on the phase returning `Ok`, because a phase that re-fired every
    /// member would also return `Ok`.
    #[test]
    fn a_re_entered_apply_dispatches_neither_the_settled_member_nor_the_adopted_one() {
        let members = vec![
            member("llm-1:tool:already-sent"),
            member("llm-1:tool:has-a-result"),
            member("llm-1:tool:owed"),
        ];
        let plan = EffectPlan::resolved(vec![
            planned(
                "llm-1:tool:already-sent",
                EffectAction::AlreadyFired {
                    by_this_attempt: true,
                },
            ),
            planned(
                "llm-1:tool:has-a-result",
                EffectAction::Adopt {
                    result: None,
                    succeeded: true,
                },
            ),
        ]);

        let dispositions = plan_the_batch(&plan, &members).expect("this plan describes this batch");

        assert_eq!(
            dispositions,
            vec![
                MemberDispatch::SettleAsAlreadyFired {
                    by_this_attempt: true
                },
                MemberDispatch::AdoptRecorded {
                    result: None,
                    succeeded: true,
                },
                MemberDispatch::Fire,
            ],
            "the member the outward record answered and the member the ledger answered are both \
             skipped, in their admitted positions; only the member nothing answered for is \
             dispatched"
        );
        assert_eq!(
            fires(&dispositions),
            1,
            "exactly ONE of three admitted members may reach the executor. Counted as well as \
             matched: a verdict list that named the right members in the wrong arms would still \
             satisfy a length check"
        );
    }

    #[test]
    fn a_pre_fire_interrupt_settles_the_whole_active_batch_without_false_negatives() {
        let members = vec![
            member("llm-1:tool:fresh"),
            member("llm-1:tool:adopted"),
            member("llm-1:tool:outward-recorded"),
            member("llm-1:tool:reattach"),
        ];
        let dispositions = vec![
            MemberDispatch::Fire,
            MemberDispatch::AdoptRecorded {
                result: None,
                succeeded: true,
            },
            MemberDispatch::SettleAsAlreadyFired {
                by_this_attempt: false,
            },
            MemberDispatch::FireResumingSession {
                native_session_id: "session-1".to_string(),
            },
        ];
        let mut settled = Vec::new();

        settle_members_before_first_dispatch(
            &members,
            &dispositions,
            &mut settled,
            "cancelled before dispatch",
        );

        assert_eq!(
            settled
                .iter()
                .map(|(effect_id, _)| effect_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "llm-1:tool:fresh",
                "llm-1:tool:outward-recorded",
                "llm-1:tool:reattach",
            ],
            "an adopted row keeps its existing outcome, while every row still awaiting a send is settled"
        );
        assert!(matches!(&settled[0].1, EffectOutcome::NotDispatched { .. }));
        assert!(matches!(&settled[1].1, EffectOutcome::Succeeded { .. }));
        assert!(matches!(&settled[2].1, EffectOutcome::NotDispatched { .. }));
    }

    /// The other half of not re-firing: the ledger must stop calling a skipped
    /// member unsettled, and must not call it *proof it never went out*.
    #[test]
    fn a_member_the_outward_record_answered_settles_as_fired_and_never_as_not_dispatched() {
        let members = vec![
            member("llm-1:tool:already-sent"),
            member("llm-1:tool:has-a-result"),
            member("llm-1:tool:owed"),
        ];
        let dispositions = vec![
            MemberDispatch::SettleAsAlreadyFired {
                by_this_attempt: false,
            },
            MemberDispatch::AdoptRecorded {
                result: None,
                succeeded: true,
            },
            MemberDispatch::Fire,
        ];

        // From index 0, as the branch that reaches no follow-up at all does.
        let mut settled: Vec<(EffectId, EffectOutcome)> = Vec::new();
        let mut history = ExecutionHistory::new();
        let state = EnvironmentState::Uninitialized;
        record_members_not_dispatched(
            &mut settled,
            &members,
            &dispositions,
            &mut history,
            &state,
            &state,
            1,
            0,
            "the batch stopped short",
        );

        let by_id: Vec<(String, EffectOutcome)> = settled
            .into_iter()
            .map(|(id, outcome)| (id.to_string(), outcome))
            .collect();
        assert_eq!(
            by_id.len(),
            2,
            "the adopted member is settled by NOBODY — its row already carries the outcome that \
             made the driver answer `Adopt`, and a second write would put this attempt's guess \
             over a real answer"
        );
        assert_eq!(by_id[0].0, "llm-1:tool:already-sent");
        assert!(
            matches!(by_id[0].1, EffectOutcome::Succeeded { .. }),
            "the outward record is positive evidence the act LEFT. `NotDispatched` would claim \
             the opposite and `Indeterminate` would hold this run again on every future claim, \
             got {:?}",
            by_id[0].1
        );
        assert_eq!(by_id[1].0, "llm-1:tool:owed");
        assert!(
            matches!(by_id[1].1, EffectOutcome::NotDispatched { .. }),
            "the member that was genuinely owed a fire and did not get one is the only member \
             this may be written for, got {:?}",
            by_id[1].1
        );
    }

    /// ~~The residue, refused by name~~ — the resume, CARRIED by name.
    ///
    /// The assertion that matters is not that the batch is dispatchable again;
    /// it is that the resuming member is distinguishable from an ordinary fire
    /// all the way to the executor. A verdict list that answered plain `Fire`
    /// here would also let the batch through, and would start a second engine
    /// session against the same repository.
    #[test]
    fn a_member_carrying_a_live_session_is_dispatched_as_a_resume_and_never_as_a_fresh_fire() {
        let members = vec![
            member("llm-1:tool:already-sent"),
            member("llm-1:tool:coding-job"),
            member("llm-1:tool:owed"),
        ];
        let plan = EffectPlan::resolved(vec![
            planned(
                "llm-1:tool:already-sent",
                EffectAction::AlreadyFired {
                    by_this_attempt: true,
                },
            ),
            planned(
                "llm-1:tool:coding-job",
                EffectAction::Reattach {
                    native_session_id: "sess-01H-live".to_string(),
                },
            ),
        ]);

        let dispositions =
            plan_the_batch(&plan, &members).expect("a member that must be resumed is dispatchable");

        assert_eq!(
            dispositions,
            vec![
                MemberDispatch::SettleAsAlreadyFired {
                    by_this_attempt: true
                },
                MemberDispatch::FireResumingSession {
                    native_session_id: "sess-01H-live".to_string(),
                },
                MemberDispatch::Fire,
            ],
            "the reattaching member keeps its admitted position and carries the session the \
             driver resolved for it; the members either side of it are untouched"
        );
        assert_eq!(
            dispositions[1].resume_session_id(),
            Some("sess-01H-live"),
            "the session has to survive as a VALUE on the verdict — it is what `dispatch` stamps \
             as `__coding_resume_session_id`, and a verdict that dropped it would fire a fresh \
             coding job against the same repository"
        );
        assert_ne!(
            dispositions[1],
            MemberDispatch::Fire,
            "a resuming member must never be spelled as an ordinary fire: they differ by exactly \
             the thing that prevents the double-run"
        );
        assert_eq!(
            dispositions[2].resume_session_id(),
            None,
            "and the absence is a statement — a member with no reattach starts a fresh session \
             rather than resuming an unnamed one"
        );
        assert_eq!(
            fires(&dispositions),
            2,
            "the resuming member reaches the executor, so it is counted with the fires. Counting \
             it as skipped is what would settle it un-fired while the dispatch fired it anyway"
        );
    }

    /// A plan about a different batch is a bug, not something to dispatch
    /// around.
    #[test]
    fn a_plan_naming_an_effect_this_batch_did_not_admit_is_refused() {
        let members = vec![member("llm-1:tool:admitted")];
        let plan = EffectPlan::resolved(vec![
            planned(
                "llm-1:tool:admitted",
                EffectAction::Adopt {
                    result: None,
                    succeeded: true,
                },
            ),
            planned(
                "llm-1:tool:from-another-turn",
                EffectAction::Adopt {
                    result: None,
                    succeeded: true,
                },
            ),
        ]);

        let refused = plan_the_batch(&plan, &members)
            .expect_err("a plan the batch does not match must not be dispatched around");
        assert!(
            refused.to_string().contains("llm-1:tool:from-another-turn"),
            "the refusal names the member that does not belong, got: {refused}"
        );
    }

    /// The ordinary turn, and the one every non-recovering claim takes.
    #[test]
    fn a_batch_with_nothing_resolved_about_it_fires_every_member() {
        let members = vec![
            member("llm-1:tool:one"),
            unkeyable_member(),
            member("llm-1:tool:two"),
        ];

        for plan in [
            EffectPlan::nothing_resolved(),
            EffectPlan::resolved(Vec::new()),
        ] {
            let dispositions =
                plan_the_batch(&plan, &members).expect("an empty plan describes any batch");
            assert_eq!(
                fires(&dispositions),
                3,
                "nothing licenses skipping a member no ledger row answered for — including the \
                 member the gate could not key, which has no row at all"
            );
        }
    }

    /// A verdict is looked up by effect id, never by what the member does.
    #[test]
    fn two_members_sharing_a_tool_do_not_share_a_verdict() {
        // Both fixtures carry `tool: "browser__click"`. A plan matched on the
        // tool would answer `Adopt` for both and skip a live dispatch.
        let members = vec![member("llm-1:tool:click-a"), member("llm-1:tool:click-b")];
        let plan = EffectPlan::resolved(vec![planned(
            "llm-1:tool:click-a",
            EffectAction::Adopt {
                result: None,
                succeeded: true,
            },
        )]);

        let dispositions = plan_the_batch(&plan, &members).expect("the plan describes this batch");
        assert_eq!(
            dispositions,
            vec![
                MemberDispatch::AdoptRecorded {
                    result: None,
                    succeeded: true,
                },
                MemberDispatch::Fire,
            ],
            "the second click is a different effect and is still owed"
        );
    }

    /// `Refire` is a licence, and a licence is not a skip.
    #[test]
    fn a_licensed_refire_still_fires() {
        let members = vec![member("llm-1:tool:retry-safe")];
        let plan =
            EffectPlan::resolved(vec![planned("llm-1:tool:retry-safe", EffectAction::Refire)]);

        assert_eq!(
            plan_the_batch(&plan, &members).expect("the plan describes this batch"),
            vec![MemberDispatch::Fire],
            "`resolve_effects` reaches `Refire` only through a retry-safety declaration or \
             positive evidence nothing left, PLUS a fingerprint that reproduced. Treating it as a \
             skip would strand a member nothing else will ever fire"
        );
    }
}

/// The deliverable text of a completed yield. The yield `summary` is the
/// agent's headline — often the answer itself (`1987`) — and `completed[]`
/// narrates the work. Publishing only the narration dropped the answer; the
/// summary leads unless the narration already carries it.
/// The outcome statement of a completed terminal. When the terminal's
/// evidence is its deliverable — a settled harness turn's answer, typed
/// `task_deliverable` — the statement is that answer and nothing else: the
/// finalizer publishes a verified deliverable's bytes unchanged only when the
/// statement says nothing the deliverable does not, and "Goal achieved:
/// <goal>. <answer>" said the goal, so every harness answer was composed with
/// it, judged for grounding on the harness and rejected. A synthetic
/// completion without a deliverable keeps the goal-achieved statement.
pub(super) fn completed_terminal_summary(
    goal: &str,
    evidence: Option<&str>,
    artifacts: &[crate::magician_v2::execution::agentic::types::Artifact],
) -> String {
    let evidence = evidence.map(str::trim).filter(|text| !text.is_empty());
    let delivers_its_evidence = artifacts
        .iter()
        .any(|artifact| artifact.artifact_type.as_deref() == Some("task_deliverable"));
    match evidence {
        Some(answer) if delivers_its_evidence => terminal_success_summary("", answer),
        Some(answer) => format!("Goal achieved: {goal}. {answer}"),
        None => format!("Goal achieved: {goal}"),
    }
}

pub(super) fn terminal_success_summary(summary: &str, completed_text: &str) -> String {
    let summary = summary.trim();
    let completed_text = completed_text.trim();
    match (summary.is_empty(), completed_text.is_empty()) {
        (true, _) => completed_text.to_owned(),
        (false, true) => summary.to_owned(),
        (false, false) if completed_text.contains(summary) => completed_text.to_owned(),
        (false, false) => format!("{summary}\n\n{completed_text}"),
    }
}

#[cfg(test)]
mod terminal_success_summary_tests {
    use super::terminal_success_summary;

    #[test]
    fn the_yield_summary_leads_the_completed_narration() {
        assert_eq!(
            terminal_success_summary(
                "1987",
                "Read the About page and verified the founding year."
            ),
            "1987\n\nRead the About page and verified the founding year."
        );
        assert_eq!(
            terminal_success_summary("", "Read the page."),
            "Read the page."
        );
        assert_eq!(terminal_success_summary("3119", ""), "3119");
        // Narration that already states the answer is not duplicated.
        assert_eq!(
            terminal_success_summary("3119", "Verified the first result has 3119 points."),
            "Verified the first result has 3119 points."
        );
    }

    /// A completed terminal whose evidence is its deliverable — a settled
    /// harness answer — is summarised by that answer alone. The finalizer
    /// publishes the deliverable's bytes unchanged only when the outcome
    /// statement says nothing the deliverable does not; "Goal achieved:
    /// <goal>. <answer>" says the goal, so the answer was composed with it,
    /// judged for grounding and rejected. A synthetic completion with no
    /// deliverable keeps the goal-achieved statement.
    #[test]
    fn a_completed_terminal_s_summary_is_its_deliverable_when_it_has_one() {
        use crate::magician_v2::execution::agentic::types::Artifact;
        let answer = "Second line: cedar. Line count: 4.";
        assert_eq!(
            super::completed_terminal_summary(
                "count the lines",
                Some(answer),
                &[Artifact::task_deliverable(answer)]
            ),
            answer
        );
        assert_eq!(
            super::completed_terminal_summary("count the lines", Some(answer), &[]),
            "Goal achieved: count the lines. Second line: cedar. Line count: 4."
        );
        assert_eq!(
            super::completed_terminal_summary("count the lines", None, &[]),
            "Goal achieved: count the lines"
        );
    }
}
