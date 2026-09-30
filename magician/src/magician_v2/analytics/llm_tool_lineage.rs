//! Shared Phase 4 tool/action lineage construction and transport bridge.
//!
//! Producers emit immutable, content-free facts through the existing runtime
//! broadcaster. The process-owned trace activation validates and journals
//! them; tool execution remains authoritative in its existing runtime/canonical
//! event rail.

use std::collections::BTreeMap;

use magicllm::LlmTraceContext;
use serde_json::Value;

use super::{
    llm_trace_content::scoped_content_fingerprint,
    llm_trace_recorder::{
        LlmToolBranchState, LlmToolFailureOwner, LlmToolLineageOutcome, LlmToolLineageRecord,
        LlmToolLineageStage, LlmToolSideEffectState, LLM_TRACE_FACT_SCHEMA_VERSION,
        MAX_TOOL_RELATED_EXECUTION_IDS, MAX_TOOL_RELATED_EXECUTION_ID_BYTES,
    },
};
use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace,
    realtime_events::{AgentEventEnvelope, RuntimeTransportBroadcaster},
};

pub const LLM_TOOL_LINEAGE_EVENT_TYPE: &str = "llm.tool.lineage";

#[derive(Debug, Clone)]
pub struct LlmToolLineageIdentity {
    pub context: LlmTraceContext,
    pub model_tool_call_id: String,
    pub tool_execution_id: String,
    pub branch_id: String,
    pub operation: String,
    pub source_surface: String,
    pub tool_name: String,
    pub tool_family: Option<String>,
    pub arguments_fingerprint: String,
}

impl LlmToolLineageIdentity {
    /// The per-dispatch-attempt key, in one place.
    ///
    /// Callers that need the key WITHOUT minting a lineage record use this, so
    /// the two can never drift into disagreeing about what identifies an
    /// attempt. Changing the shape here changes it everywhere, which is the
    /// point.
    ///
    /// Both parts must be non-empty, and it is the CALLER's job to establish
    /// that. Composition alone cannot: a blank `model_tool_call_id` yields
    /// `{llm_call_id}:tool:` for every unidentified call in a turn, so two
    /// distinct requests would carry one key — and a remote honouring it would
    /// answer the second with the first one's response, dropping a real effect
    /// with no error anywhere. Returning an `Option` here would push the same
    /// judgement onto callers that legitimately hold both values; instead each
    /// entry point filters before it composes (see `effect_id_for_candidate`).
    pub fn tool_execution_id_for(llm_call_id: &str, model_tool_call_id: &str) -> String {
        format!("{llm_call_id}:tool:{model_tool_call_id}")
    }

    /// The value we are willing to send to a THIRD PARTY as an idempotency key.
    ///
    /// Derived, never the raw effect id. The raw id embeds `llm_call_id` — an
    /// internal identifier — and an `Idempotency-Key` header travels to
    /// arbitrary remote APIs, including ones we do not control. A digest
    /// deduplicates exactly as well, because the far side only requires the
    /// value to be stable for one logical attempt and distinct across attempts;
    /// it has no need to parse it.
    ///
    /// Stable across a crash and re-run of the same attempt (same input, same
    /// digest), and different for a deliberate retry (new `llm_call_id`), which
    /// is the property that makes it safe to send at all.
    pub fn far_side_idempotency_key(effect_id: &str) -> String {
        format!("mag-{}", Self::attempt_digest("far-side", effect_id))
    }

    /// A one-way key for correlating LOCAL records of one dispatch attempt.
    ///
    /// Same shape as the far-side key and deliberately not the same value. That
    /// one leaves the process; a value that both travels to a third party and
    /// indexes our own audit trail would let a remote recognise our internal
    /// records, so the two derivations are domain-separated and cannot collide.
    ///
    /// Used where a record needs to be findable from an attempt but cannot hold
    /// the raw id — because a format forbids it, as `CredentialCallId` forbids
    /// the effect id's colons, or because the value reaches somewhere an
    /// internal identifier does not belong, such as a child process environment.
    /// One-way but deterministic: holding an effect id you can compute the key
    /// and find the record; holding the key you learn nothing.
    pub fn local_attempt_key(effect_id: &str) -> String {
        Self::attempt_digest("local-attempt", effect_id)
    }

    /// Domain-separated digest of one attempt.
    ///
    /// The domain is hashed with a NUL separator so no two domains can be made
    /// to agree by choosing an effect id — without it, `far-side` + `x:y` and
    /// `far-sidex` + `:y` would hash identically.
    fn attempt_digest(domain: &str, effect_id: &str) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(domain.as_bytes());
        hasher.update(b"\0");
        hasher.update(effect_id.as_bytes());
        hasher.finalize().to_hex()[..32].to_string()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace: &ArtifactV2Workspace,
        context: LlmTraceContext,
        model_tool_call_id: impl Into<String>,
        branch_id: impl Into<String>,
        operation: impl Into<String>,
        source_surface: impl Into<String>,
        tool_name: impl Into<String>,
        tool_family: Option<String>,
        arguments: &Value,
    ) -> Result<Self, &'static str> {
        let model_tool_call_id = model_tool_call_id.into();
        let tool_execution_id =
            Self::tool_execution_id_for(&context.llm_call_id, &model_tool_call_id);
        let canonical_arguments = canonical_json_value(arguments);
        let raw = serde_json::to_vec(&canonical_arguments)
            .map_err(|_| "tool_arguments_canonicalization_failed")?;
        let arguments_fingerprint = scoped_content_fingerprint(workspace, &context.scope, &raw)?;
        Ok(Self {
            context,
            model_tool_call_id,
            tool_execution_id,
            branch_id: branch_id.into(),
            operation: operation.into(),
            source_surface: source_surface.into(),
            tool_name: tool_name.into(),
            tool_family,
            arguments_fingerprint,
        })
    }

    pub fn record(
        &self,
        stage: LlmToolLineageStage,
        stage_index: u32,
        occurred_at_ms: i64,
    ) -> LlmToolLineageRecord {
        LlmToolLineageRecord {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context: self.context.clone(),
            model_tool_call_id: self.model_tool_call_id.clone(),
            tool_execution_id: self.tool_execution_id.clone(),
            branch_id: self.branch_id.clone(),
            operation: self.operation.clone(),
            source_surface: self.source_surface.clone(),
            tool_name: self.tool_name.clone(),
            tool_family: self.tool_family.clone(),
            stage,
            stage_index,
            occurred_at_ms,
            observed_at_ms: chrono::Utc::now().timestamp_millis().max(occurred_at_ms),
            arguments_fingerprint: Some(self.arguments_fingerprint.clone()),
            result_ref: None,
            canonical_event_ref: None,
            related_execution_ids: Vec::new(),
            consumed_by_call_id: None,
            name_known: None,
            arguments_parsed: None,
            schema_matched: None,
            policy_allowed: None,
            approval_required: None,
            approval_obtained: None,
            transport_ran: None,
            tool_reported_success: None,
            result_validation_success: None,
            outcome: LlmToolLineageOutcome::Pending,
            failure_owner: None,
            failure_code: None,
            side_effect_state: LlmToolSideEffectState::Unknown,
            branch_state: LlmToolBranchState::Active,
            on_successful_path: None,
            same_tool_arguments_count: 1,
            observation_action_cycle_count: 0,
            recovered_after_failure: false,
            linkage_gap: None,
        }
    }

    pub fn result_ref(&self) -> String {
        format!("{}:result", self.tool_execution_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn without_content_fingerprint(
        context: LlmTraceContext,
        model_tool_call_id: impl Into<String>,
        branch_id: impl Into<String>,
        operation: impl Into<String>,
        source_surface: impl Into<String>,
        tool_name: impl Into<String>,
        tool_family: Option<String>,
    ) -> Self {
        let model_tool_call_id = model_tool_call_id.into();
        Self {
            tool_execution_id: Self::tool_execution_id_for(
                &context.llm_call_id,
                &model_tool_call_id,
            ),
            context,
            model_tool_call_id,
            branch_id: branch_id.into(),
            operation: operation.into(),
            source_surface: source_surface.into(),
            tool_name: tool_name.into(),
            tool_family,
            arguments_fingerprint: "0".repeat(64),
        }
    }
}

/// Build the transport envelope for one lineage record, without sending it.
///
/// # Why this exists apart from [`emit_tool_lineage_record`]
///
/// The agentic loop is being converted from inline emission to an **outbox**:
/// a phase journals the event it wants and a projector emits it, so a phase
/// that re-runs after a crash does not emit twice. See
/// `execution::agentic::run_loop::phases::outbox`.
///
/// A journalled call site needs the finished envelope in hand — it hands it to
/// `journal_and_emit_at`, which records it and then emits it exactly as the
/// call site used to. Until this split existed, every emitter in this file
/// built its envelope and sent it in one call, so the only way to journal one
/// was for the call site to REBUILD the envelope. That is the second event
/// vocabulary the outbox design refuses: two constructions of one payload
/// drift the first time this file's record shape changes, and nothing catches
/// it.
///
/// So the construction stays here, in one place, and the two callers differ
/// only in what they do with the result.
///
/// **`None` is the serialization failure, already logged.** It is not "nothing
/// to send": a caller must treat it exactly as [`emit_tool_lineage_record`]
/// does, by doing nothing further. Journalling a record for an envelope that
/// was never built would put a hole in the log that no reader could explain.
pub fn tool_lineage_envelope(
    agent_id: &str,
    record: LlmToolLineageRecord,
) -> Option<AgentEventEnvelope> {
    let scope = record.context.scope.clone();
    match serde_json::to_value(record) {
        Ok(payload) => Some(AgentEventEnvelope::new_scoped(
            LLM_TOOL_LINEAGE_EVENT_TYPE,
            agent_id,
            &scope.principal,
            &scope.workspace,
            payload,
        )),
        Err(error) => {
            tracing::warn!(
                target: "analytics::llm_tool_lineage",
                error = %error,
                "tool-lineage event serialization failed"
            );
            None
        },
    }
}

/// Build the envelope and send it on the transport, which is what every
/// emitter in this file below did inline until 2026-08-28.
///
/// # NOT JOURNALLED, and every caller of this is a known unjournalled site
///
/// `emit_agent_transport_event` is a transport-only `AgentEvent` send, so an
/// event that goes out through here reaches a live surface and leaves no
/// record behind.
///
/// This file has **six public emitters** besides this one, and **five of the
/// six end here**: `emit_tool_proposed`, `emit_tool_linkage_gap`,
/// `emit_completed_tool_dispatch`, `emit_tool_branch_materialized` and
/// `emit_tool_rollback` (plus the private `emit_failed_stage`). Every one of
/// those five is a known unjournalled site in
/// `execution::agentic::run_loop::phases::outbox`'s *WHAT IS NOT JOURNALLED*,
/// numbered 15–19 there. What each is owed is written at its own call site in
/// `executor.rs` — `begin_agentic_tool_lineage` for the proposed/linkage-gap
/// pair, `finish_agentic_tool_lineage` for the dispatch/rollback/branch trio.
///
/// (Note that the five are not the five `emit_tool_*` names: the family is
/// spelled inconsistently, and `emit_completed_tool_dispatch` is in it while
/// `emit_tool_result_consumed` is not. A sweep matching on the `emit_tool_`
/// prefix gets a different set than a sweep matching on what reaches a
/// transport, and only the second one is the population.)
///
/// **The sixth does not come through here any more**, and that is the
/// difference this split bought. [`emit_tool_result_consumed`] now goes
/// through [`tool_result_consumed_envelope`] and sends the envelope itself, so
/// that `executor.rs::emit_agentic_tool_consumption` can take the same
/// envelope and journal it instead. A reader tracing "what reaches a transport
/// unjournalled" must therefore check both this function AND
/// `emit_tool_result_consumed` — this one is no longer the single funnel it
/// was before 2026-08-28.
///
/// The **chat** path (`chat::service`) also calls into this file and is
/// deliberately outside that census: chat is not the agentic loop, has no
/// phase, and journals nothing.
pub fn emit_tool_lineage_record(
    broadcaster: &RuntimeTransportBroadcaster,
    agent_id: &str,
    record: LlmToolLineageRecord,
) {
    if let Some(envelope) = tool_lineage_envelope(agent_id, record) {
        broadcaster.emit_agent_transport_event(envelope);
    }
}

/// Build the `Proposed` envelope without sending it, so a caller holding a
/// [`PhaseAddress`] can journal it instead.
///
/// Census site 15's half of the split. The emitting form below is unchanged in
/// behaviour and is still what the chat path and any addressless caller use;
/// what this adds is the ability for `executor.rs::begin_agentic_tool_lineage`
/// — which has held an `at` since the mapping-gap thread landed — to put the
/// record on the outbox rather than straight on the wire.
pub fn tool_proposed_envelope(
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    occurred_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
) -> Option<AgentEventEnvelope> {
    let mut record = identity.record(LlmToolLineageStage::Proposed, 0, occurred_at_ms);
    apply_repeat_features(&mut record, repeat_count, cycle_count);
    record.side_effect_state = LlmToolSideEffectState::None;
    tool_lineage_envelope(agent_id, record)
}

pub fn emit_tool_proposed(
    broadcaster: &RuntimeTransportBroadcaster,
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    occurred_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
) {
    if let Some(envelope) = tool_proposed_envelope(
        agent_id,
        identity,
        occurred_at_ms,
        repeat_count,
        cycle_count,
    ) {
        broadcaster.emit_agent_transport_event(envelope);
    }
}

/// Build the `LinkageGap` envelope without sending it.
///
/// Census site 16's half of the split, and the one that needed a care the
/// others did not: [`emit_completed_tool_dispatch`] calls the EMITTING form
/// below internally for truncated related ids, so that form has to keep
/// sending exactly as it did. It does — the split adds a builder beside it and
/// changes nothing about what the emitter does — but the constraint is written
/// here because a later tidy that collapses the emitter into the builder would
/// silently take that internal call off the wire.
/// The linkage-gap record itself, built once and shared by the envelope form
/// below and by [`completed_tool_dispatch_records`], which pushes it rather
/// than emitting it.
fn linkage_gap_record(
    identity: &LlmToolLineageIdentity,
    occurred_at_ms: i64,
    gap: &str,
) -> LlmToolLineageRecord {
    let mut record = identity.record(LlmToolLineageStage::LinkageGap, 1, occurred_at_ms);
    record.outcome = LlmToolLineageOutcome::Failed;
    record.failure_owner = Some(LlmToolFailureOwner::Runtime);
    record.failure_code = Some(normalize_machine_code(gap));
    record.linkage_gap = Some(normalize_machine_code(gap));
    record.side_effect_state = LlmToolSideEffectState::None;
    record
}

pub fn tool_linkage_gap_envelope(
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    occurred_at_ms: i64,
    gap: &str,
) -> Option<AgentEventEnvelope> {
    tool_lineage_envelope(agent_id, linkage_gap_record(identity, occurred_at_ms, gap))
}

pub fn emit_tool_linkage_gap(
    broadcaster: &RuntimeTransportBroadcaster,
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    occurred_at_ms: i64,
    gap: &str,
) {
    if let Some(envelope) = tool_linkage_gap_envelope(agent_id, identity, occurred_at_ms, gap) {
        broadcaster.emit_agent_transport_event(envelope);
    }
}

/// Build every record one completed dispatch produces, in order, without
/// sending any of them.
///
/// # Census site 17, and why this is a restructure and not a twin
///
/// This is the one split of the seven that was not mechanical, and the shape
/// matters more than the diff. The emitting form used to run the stage pipeline
/// and call the transport at each step, through eight early returns, producing
/// between one and eight records depending on where the dispatch stopped. The
/// obvious split — an envelope-returning twin written beside it — would have
/// duplicated that control flow, and `finish_agentic_tool_lineage`'s note names
/// the hazard exactly: get one branch wrong and a stage record goes missing
/// with nothing to say it is missing, on a rail whose only job is to say what
/// happened.
///
/// So the pipeline was not copied. It lives HERE, once. Both the emitting path
/// and the journalling path are thin wrappers over this vector, so there is no
/// second branch structure that can drift out of agreement with the first.
///
/// **Order is load-bearing** and is the order of the pushes: a reader replays
/// the dispatch's progress from the sequence, so a stage moved is a stage
/// misreported.
#[allow(clippy::too_many_arguments)]
pub fn completed_tool_dispatch_records(
    identity: &LlmToolLineageIdentity,
    execution_started_at_ms: i64,
    execution_finished_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
    classification: &ToolDispatchClassification,
    canonical_event_ref: Option<String>,
    related_execution_ids: &[String],
) -> Vec<LlmToolLineageRecord> {
    let mut records = Vec::new();
    let (related_execution_ids, related_ids_incomplete) =
        normalized_related_execution_ids(related_execution_ids);
    if related_ids_incomplete {
        // Was a direct `emit_tool_linkage_gap` call. It is a record like any
        // other now, first in the vector because it was first on the wire.
        records.push(linkage_gap_record(
            identity,
            execution_finished_at_ms,
            "related_execution_ids_invalid_or_truncated",
        ));
    }
    let success_stage = |stage, fact: fn(&mut LlmToolLineageRecord)| {
        let mut record = identity.record(stage, 0, execution_finished_at_ms);
        record.outcome = LlmToolLineageOutcome::Succeeded;
        record.side_effect_state = LlmToolSideEffectState::None;
        fact(&mut record);
        apply_repeat_features(&mut record, repeat_count, cycle_count);
        record
    };

    if !classification.name_valid {
        records.push(failed_stage_record(
            identity,
            LlmToolLineageStage::NameValidated,
            execution_finished_at_ms,
            repeat_count,
            cycle_count,
            classification,
            |record| record.name_known = Some(false),
        ));
        return records;
    }
    records.push(success_stage(
        LlmToolLineageStage::NameValidated,
        |record| record.name_known = Some(true),
    ));
    if !classification.arguments_parsed {
        records.push(failed_stage_record(
            identity,
            LlmToolLineageStage::ArgumentsParsed,
            execution_finished_at_ms,
            repeat_count,
            cycle_count,
            classification,
            |record| record.arguments_parsed = Some(false),
        ));
        return records;
    }
    records.push(success_stage(
        LlmToolLineageStage::ArgumentsParsed,
        |record| record.arguments_parsed = Some(true),
    ));
    if !classification.schema_valid {
        records.push(failed_stage_record(
            identity,
            LlmToolLineageStage::SchemaValidated,
            execution_finished_at_ms,
            repeat_count,
            cycle_count,
            classification,
            |record| record.schema_matched = Some(false),
        ));
        return records;
    }
    records.push(success_stage(
        LlmToolLineageStage::SchemaValidated,
        |record| record.schema_matched = Some(true),
    ));
    if !classification.policy_allowed {
        records.push(failed_stage_record(
            identity,
            LlmToolLineageStage::AuthorizationResolved,
            execution_finished_at_ms,
            repeat_count,
            cycle_count,
            classification,
            |record| record.policy_allowed = Some(false),
        ));
        return records;
    }
    records.push(success_stage(
        LlmToolLineageStage::AuthorizationResolved,
        |record| record.policy_allowed = Some(true),
    ));
    if classification.approval_required && !classification.approval_obtained {
        records.push(failed_stage_record(
            identity,
            LlmToolLineageStage::ApprovalResolved,
            execution_finished_at_ms,
            repeat_count,
            cycle_count,
            classification,
            |record| {
                record.approval_required = Some(true);
                record.approval_obtained = Some(false);
            },
        ));
        return records;
    }
    // Built inline rather than through `success_stage` because it sets two
    // fields from the classification instead of a fixed fact, exactly as it did
    // when it emitted.
    let mut approval = identity.record(
        LlmToolLineageStage::ApprovalResolved,
        0,
        execution_finished_at_ms,
    );
    approval.outcome = LlmToolLineageOutcome::Succeeded;
    approval.side_effect_state = LlmToolSideEffectState::None;
    approval.approval_required = Some(classification.approval_required);
    approval.approval_obtained = Some(classification.approval_obtained);
    apply_repeat_features(&mut approval, repeat_count, cycle_count);
    records.push(approval);
    if !classification.transport_ran {
        return records;
    }

    let mut started = identity.record(
        LlmToolLineageStage::ExecutionStarted,
        1,
        execution_started_at_ms,
    );
    started.transport_ran = Some(true);
    started.side_effect_state = LlmToolSideEffectState::Pending;
    apply_repeat_features(&mut started, repeat_count, cycle_count);
    records.push(started);

    let result_ref = identity.result_ref();
    let mut finished = identity.record(
        LlmToolLineageStage::ExecutionFinished,
        1,
        execution_finished_at_ms,
    );
    finished.outcome = if classification.tool_reported_success {
        LlmToolLineageOutcome::Succeeded
    } else {
        classification.outcome
    };
    finished.failure_owner = (!classification.tool_reported_success)
        .then_some(classification.failure_owner)
        .flatten();
    finished.failure_code = (!classification.tool_reported_success)
        .then(|| classification.failure_code.clone())
        .flatten();
    finished.tool_reported_success = Some(classification.tool_reported_success);
    finished.result_ref = Some(result_ref.clone());
    finished.canonical_event_ref = canonical_event_ref;
    finished.related_execution_ids = related_execution_ids;
    // Once transport ran, a failed/cancelled/timed-out result cannot prove
    // that no mutation happened. Only an authoritative rollback may move this
    // to `reversed`; admission failures that never ran remain `none`. A result
    // the tool reported as successful DOES resolve — see
    // `resolved_side_effect_state`, which used to read only `transport_ran` and
    // so answered `unknown` for every success this runtime ever recorded.
    let side_effect_state = resolved_side_effect_state(
        classification.transport_ran,
        classification.tool_reported_success,
    );
    apply_side_effect_resolution(&mut finished, true, side_effect_state);
    apply_repeat_features(&mut finished, repeat_count, cycle_count);
    records.push(finished);

    let mut validated = identity.record(
        LlmToolLineageStage::ResultValidated,
        0,
        execution_finished_at_ms,
    );
    validated.outcome = classification.outcome;
    validated.failure_owner = classification.failure_owner;
    validated.failure_code = classification.failure_code.clone();
    validated.result_ref = Some(result_ref);
    validated.result_validation_success = Some(classification.result_validation_success);
    // `Some(true)` is not an assumption: this function returns early above unless
    // `classification.transport_ran`, so nothing below that guard is reached by a
    // dispatch that never left the process.
    apply_side_effect_resolution(&mut validated, true, side_effect_state);
    apply_repeat_features(&mut validated, repeat_count, cycle_count);
    records.push(validated);
    records
}

/// The same records as envelopes, for a caller holding a `PhaseAddress`.
///
/// A record that fails to serialize is dropped here with the warning
/// [`tool_lineage_envelope`] logs, which is what the emitting path does with it
/// too — so a serialization failure costs the same one record on both paths
/// rather than the whole dispatch on one of them.
#[allow(clippy::too_many_arguments)]
pub fn completed_tool_dispatch_envelopes(
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    execution_started_at_ms: i64,
    execution_finished_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
    classification: &ToolDispatchClassification,
    canonical_event_ref: Option<String>,
    related_execution_ids: &[String],
) -> Vec<AgentEventEnvelope> {
    completed_tool_dispatch_records(
        identity,
        execution_started_at_ms,
        execution_finished_at_ms,
        repeat_count,
        cycle_count,
        classification,
        canonical_event_ref,
        related_execution_ids,
    )
    .into_iter()
    .filter_map(|record| tool_lineage_envelope(agent_id, record))
    .collect()
}

/// Send every record of one completed dispatch. Unchanged in behaviour, and
/// still the entry point for the **chat** path, which has no phase to address a
/// record to.
#[allow(clippy::too_many_arguments)]
pub fn emit_completed_tool_dispatch(
    broadcaster: &RuntimeTransportBroadcaster,
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    execution_started_at_ms: i64,
    execution_finished_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
    classification: &ToolDispatchClassification,
    canonical_event_ref: Option<String>,
    related_execution_ids: &[String],
) {
    for record in completed_tool_dispatch_records(
        identity,
        execution_started_at_ms,
        execution_finished_at_ms,
        repeat_count,
        cycle_count,
        classification,
        canonical_event_ref,
        related_execution_ids,
    ) {
        emit_tool_lineage_record(broadcaster, agent_id, record);
    }
}

/// Build the result-consumed envelope for one lineage, without sending it.
///
/// # The journalled half of the result-consumed rail
///
/// `executor.rs::emit_agentic_tool_consumption` calls this and hands the
/// envelope to `journal_and_emit_at`, so the loop's copy of this event gets a
/// journal record AND the same inline send it always had.
/// [`emit_tool_result_consumed`] below is the unjournalled entry point, kept
/// for the **chat** path, which has no phase to address a record to.
///
/// The record is built here and only here. A call site that rebuilt it would
/// be a second construction of one payload — see [`tool_lineage_envelope`] for
/// why that is refused rather than merely disliked.
#[allow(clippy::too_many_arguments)]
pub fn tool_result_consumed_envelope(
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    consumed_by_call_id: String,
    consumption_index: u32,
    occurred_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
    // Whether a transport ran for the dispatch this result came from. Cannot be
    // assumed `true`: the chat path queues a lineage for consumption whatever
    // `classification.transport_ran` said, so a refused dispatch's error text is
    // consumed exactly like a successful send's output.
    transport_ran: bool,
    // Carried from the dispatch this result came from rather than decided here.
    // Consumption cannot observe a side effect — it only sees that a result was
    // read — and a failed dispatch's error text is consumed exactly like a
    // successful one's output, so deciding locally would have to guess. It used
    // to hardcode `unknown`, which contradicted the `succeeded` outcome on the
    // same row.
    side_effect_state: LlmToolSideEffectState,
) -> Option<AgentEventEnvelope> {
    let mut record = identity.record(
        LlmToolLineageStage::ResultConsumed,
        consumption_index,
        occurred_at_ms,
    );
    record.result_ref = Some(identity.result_ref());
    record.consumed_by_call_id = Some(consumed_by_call_id);
    record.outcome = LlmToolLineageOutcome::Succeeded;
    apply_side_effect_resolution(&mut record, transport_ran, side_effect_state);
    apply_repeat_features(&mut record, repeat_count, cycle_count);
    tool_lineage_envelope(agent_id, record)
}

/// Build the result-consumed envelope and send it, with no journal record.
///
/// The **chat** path's entry point. `chat::service` has no `AgenticContext`,
/// no phase and no outbox, so there is nothing to address a record to and this
/// stays exactly what it was. The agentic loop no longer comes through here —
/// see [`tool_result_consumed_envelope`], which is also where the two
/// non-obvious parameters are documented: `transport_ran` cannot be assumed
/// `true` on this path, and `side_effect_state` is carried from the dispatch
/// rather than decided here.
///
/// Note that it does **not** go through [`emit_tool_lineage_record`]: it sends
/// the envelope the builder returns, so a change to the send made there does
/// not reach this rail.
#[allow(clippy::too_many_arguments)]
pub fn emit_tool_result_consumed(
    broadcaster: &RuntimeTransportBroadcaster,
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    consumed_by_call_id: String,
    consumption_index: u32,
    occurred_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
    transport_ran: bool,
    side_effect_state: LlmToolSideEffectState,
) {
    if let Some(envelope) = tool_result_consumed_envelope(
        agent_id,
        identity,
        consumed_by_call_id,
        consumption_index,
        occurred_at_ms,
        repeat_count,
        cycle_count,
        transport_ran,
        side_effect_state,
    ) {
        broadcaster.emit_agent_transport_event(envelope);
    }
}

#[allow(clippy::too_many_arguments)]
/// The branch-materialized record, built without sending.
#[allow(clippy::too_many_arguments)]
fn tool_branch_materialized_record(
    identity: &LlmToolLineageIdentity,
    successful: bool,
    transport_ran: bool,
    occurred_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
    recovered_after_failure: bool,
    related_execution_ids: &[String],
    // Taken, not derived from `successful`. That parameter means the branch
    // reached a presentable answer — the chat path passes
    // `turn_reached_presentable_response && succeeded` — which is a question
    // about the CONVERSATION, not about whether the tool fired. Deriving the
    // side-effect state from it would mark a tool that plainly ran as
    // unresolved because the turn it belonged to went nowhere.
    side_effect_state: LlmToolSideEffectState,
) -> LlmToolLineageRecord {
    let mut record = identity.record(LlmToolLineageStage::BranchMaterialized, 0, occurred_at_ms);
    record.outcome = if successful {
        LlmToolLineageOutcome::Succeeded
    } else {
        LlmToolLineageOutcome::Abandoned
    };
    record.branch_state = if successful {
        LlmToolBranchState::Successful
    } else {
        LlmToolBranchState::Abandoned
    };
    record.on_successful_path = Some(successful);
    record.recovered_after_failure = recovered_after_failure;
    record.related_execution_ids = normalized_related_execution_ids(related_execution_ids).0;
    // Recorded, not dropped. The caller has always passed `transport_ran` and
    // this record has always discarded it, which left a row asserting a
    // resolved side-effect state with nothing on it to justify the claim.
    apply_side_effect_resolution(&mut record, transport_ran, side_effect_state);
    apply_repeat_features(&mut record, repeat_count, cycle_count);
    record
}

/// Census site 19's split — the six-line shape, unchanged in behaviour.
#[allow(clippy::too_many_arguments)]
pub fn tool_branch_materialized_envelope(
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    successful: bool,
    transport_ran: bool,
    occurred_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
    recovered_after_failure: bool,
    related_execution_ids: &[String],
    side_effect_state: LlmToolSideEffectState,
) -> Option<AgentEventEnvelope> {
    tool_lineage_envelope(
        agent_id,
        tool_branch_materialized_record(
            identity,
            successful,
            transport_ran,
            occurred_at_ms,
            repeat_count,
            cycle_count,
            recovered_after_failure,
            related_execution_ids,
            side_effect_state,
        ),
    )
}

/// Unchanged entry point, still used by the **chat** path.
#[allow(clippy::too_many_arguments)]
pub fn emit_tool_branch_materialized(
    broadcaster: &RuntimeTransportBroadcaster,
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    successful: bool,
    transport_ran: bool,
    occurred_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
    recovered_after_failure: bool,
    related_execution_ids: &[String],
    side_effect_state: LlmToolSideEffectState,
) {
    emit_tool_lineage_record(
        broadcaster,
        agent_id,
        tool_branch_materialized_record(
            identity,
            successful,
            transport_ran,
            occurred_at_ms,
            repeat_count,
            cycle_count,
            recovered_after_failure,
            related_execution_ids,
            side_effect_state,
        ),
    );
}

fn normalized_related_execution_ids(values: &[String]) -> (Vec<String>, bool) {
    let mut invalid_or_truncated = false;
    let mut normalized = values
        .iter()
        .filter_map(|value| {
            if value.is_empty()
                || value.trim() != value
                || value.len() > MAX_TOOL_RELATED_EXECUTION_ID_BYTES
            {
                invalid_or_truncated = true;
                None
            } else {
                Some(value.clone())
            }
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    if normalized.len() > MAX_TOOL_RELATED_EXECUTION_IDS {
        normalized.truncate(MAX_TOOL_RELATED_EXECUTION_IDS);
        invalid_or_truncated = true;
    }
    (normalized, invalid_or_truncated)
}

/// Attach an authoritative compensation/rollback attempt to the same tool
/// execution. Callers invoke this only after the runtime has actually entered
/// its rollback path; the helper never infers reversal from an ordinary tool
/// failure.
#[allow(clippy::too_many_arguments)]
/// Census site 18's split. Its return is a PAIR, not an `Option`: this rail
/// emits started-then-finished, and a caller that dropped either half would
/// leave a rollback that never ends or one that never begins.
#[allow(clippy::too_many_arguments)]
fn tool_rollback_records(
    identity: &LlmToolLineageIdentity,
    rollback_index: u32,
    started_at_ms: i64,
    finished_at_ms: i64,
    succeeded: bool,
    failure_code: Option<&str>,
) -> [LlmToolLineageRecord; 2] {
    let mut started = identity.record(
        LlmToolLineageStage::RollbackStarted,
        rollback_index,
        started_at_ms,
    );
    started.side_effect_state = LlmToolSideEffectState::Pending;

    let mut finished = identity.record(
        LlmToolLineageStage::RollbackFinished,
        rollback_index,
        finished_at_ms,
    );
    if succeeded {
        finished.outcome = LlmToolLineageOutcome::Succeeded;
        finished.side_effect_state = LlmToolSideEffectState::Reversed;
    } else {
        finished.outcome = LlmToolLineageOutcome::Failed;
        finished.failure_owner = Some(LlmToolFailureOwner::Runtime);
        finished.failure_code = Some(
            failure_code
                .map(normalize_machine_code)
                .unwrap_or_else(|| "rollback_failed".to_string()),
        );
        finished.side_effect_state = LlmToolSideEffectState::RollbackFailed;
    }
    [started, finished]
}

/// The pair as envelopes, in order, for a caller holding a `PhaseAddress`.
#[allow(clippy::too_many_arguments)]
pub fn tool_rollback_envelopes(
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    rollback_index: u32,
    started_at_ms: i64,
    finished_at_ms: i64,
    succeeded: bool,
    failure_code: Option<&str>,
) -> Vec<AgentEventEnvelope> {
    tool_rollback_records(
        identity,
        rollback_index,
        started_at_ms,
        finished_at_ms,
        succeeded,
        failure_code,
    )
    .into_iter()
    .filter_map(|record| tool_lineage_envelope(agent_id, record))
    .collect()
}

/// Unchanged entry point.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn emit_tool_rollback(
    broadcaster: &RuntimeTransportBroadcaster,
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    rollback_index: u32,
    started_at_ms: i64,
    finished_at_ms: i64,
    succeeded: bool,
    failure_code: Option<&str>,
) {
    for record in tool_rollback_records(
        identity,
        rollback_index,
        started_at_ms,
        finished_at_ms,
        succeeded,
        failure_code,
    ) {
        emit_tool_lineage_record(broadcaster, agent_id, record);
    }
}

/// The failed-stage record itself, built once and shared by the emitting form
/// below and by [`completed_tool_dispatch_records`].
#[allow(clippy::too_many_arguments)]
fn failed_stage_record(
    identity: &LlmToolLineageIdentity,
    stage: LlmToolLineageStage,
    occurred_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
    classification: &ToolDispatchClassification,
    fact: impl FnOnce(&mut LlmToolLineageRecord),
) -> LlmToolLineageRecord {
    let mut record = identity.record(stage, 0, occurred_at_ms);
    record.outcome = classification.outcome;
    record.failure_owner = classification.failure_owner;
    record.failure_code = classification.failure_code.clone();
    record.side_effect_state = LlmToolSideEffectState::None;
    fact(&mut record);
    apply_repeat_features(&mut record, repeat_count, cycle_count);
    record
}

/// Kept for symmetry with the rest of the family and for any future emitting
/// caller; the dispatch pipeline now takes the record form directly.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
fn emit_failed_stage(
    broadcaster: &RuntimeTransportBroadcaster,
    agent_id: &str,
    identity: &LlmToolLineageIdentity,
    stage: LlmToolLineageStage,
    occurred_at_ms: i64,
    repeat_count: u32,
    cycle_count: u32,
    classification: &ToolDispatchClassification,
    fact: impl FnOnce(&mut LlmToolLineageRecord),
) {
    emit_tool_lineage_record(
        broadcaster,
        agent_id,
        failed_stage_record(
            identity,
            stage,
            occurred_at_ms,
            repeat_count,
            cycle_count,
            classification,
            fact,
        ),
    );
}

fn apply_repeat_features(record: &mut LlmToolLineageRecord, repeat_count: u32, cycle_count: u32) {
    record.same_tool_arguments_count = repeat_count.max(1);
    record.observation_action_cycle_count = cycle_count;
}

/// What the run knows about whether this dispatch's side effect stands.
///
/// Resolved from the authoritative execution record, not inferred. Three cases,
/// and the middle one is the point:
///
/// - No transport ran — an approval gate or admission failure returned first, so
///   there is nothing to have happened. `None`.
/// - The transport ran and the tool reported success. The tool's own report that
///   it did the thing is the strongest evidence available short of far-side
///   reconciliation, so the effect `Remained`.
/// - The transport ran and the tool did not report success. This is the case
///   that genuinely cannot be resolved here: a failure, cancellation or timeout
///   after transport cannot prove no mutation happened — the request may have
///   landed and the response been lost. `Unknown`, honestly.
///
/// `Remained` had no producer anywhere in the codebase before this. Every
/// successful side-effecting dispatch was recorded `Unknown`, which collapsed
/// "we know this happened" into "we have no idea" — the exact distinction the
/// effect-identity work exists to draw. A dataset that cannot separate them
/// cannot answer "did it fire" for any dispatch at all.
///
/// # What this axis does and does not say
///
/// It answers whether a dispatch's effects, IF ANY, stand. It does not say
/// whether there were any. A successful read resolves to `Remained` here, and
/// that is vacuously true rather than informative — nothing happened, so nothing
/// is unresolved. Neither did the old rule distinguish them: it called a
/// successful read `Unknown`, which was wrong in the other direction.
///
/// Separating read-only dispatches would need a signal these emitters are not
/// given — `classify_tool_dispatch` is built from a success flag and a failure
/// code, and the action never reaches it. Until that changes, count `Unknown`
/// to find the dispatches that need reconciling; `Remained` on its own is close
/// to a restatement of `tool_reported_success`. Retiring the false `Unknown` is
/// the win here, and it holds either way.
///
/// Reversal is not decided here. Only an authoritative rollback marker written
/// by the runtime that attempted restoration may move a record to `Reversed` or
/// `RollbackFailed`, and it does so on the rollback-finished record.
/// The same rule, read off a classification the caller already holds.
///
/// Exists so a caller that must remember the answer — the agentic loop carries
/// it from the finished dispatch to the result-consumed record — cannot derive
/// it a second way and drift from what the emitters recorded.
pub fn resolved_side_effect_state_for(
    classification: &ToolDispatchClassification,
) -> LlmToolSideEffectState {
    resolved_side_effect_state(
        classification.transport_ran,
        classification.tool_reported_success,
    )
}

/// Record a resolved side-effect state together with the fact that supports it.
///
/// The two are set through one call because a row carrying one without the other
/// is not merely untidy — `validate_tool_lineage` refuses a `Remained` whose
/// record does not also say a transport ran, so an emitter that set only the
/// state would have its records rejected at journal append. That is exactly what
/// happened to `ResultValidated` and `ResultConsumed` when the state became
/// resolvable: both had always set the state alone, which was harmless while the
/// state was never `Remained`.
///
/// Keeping them together also makes the dataset queryable — filtering on
/// `side_effect_state = 'remained' AND transport_ran` can never disagree with
/// itself.
fn apply_side_effect_resolution(
    record: &mut LlmToolLineageRecord,
    transport_ran: bool,
    side_effect_state: LlmToolSideEffectState,
) {
    record.transport_ran = Some(transport_ran);
    record.side_effect_state = side_effect_state;
}

pub fn resolved_side_effect_state(
    transport_ran: bool,
    tool_reported_success: bool,
) -> LlmToolSideEffectState {
    match (transport_ran, tool_reported_success) {
        (false, _) => LlmToolSideEffectState::None,
        (true, true) => LlmToolSideEffectState::Remained,
        (true, false) => LlmToolSideEffectState::Unknown,
    }
}

/// Update the per-run recovery ledger using only authoritative transport/tool
/// failures. Admission denials (policy, approval, schema) never ran the tool
/// and therefore cannot make a later successful dispatch a "recovery".
pub fn observe_tool_recovery(
    failed_tool_fingerprints: &mut std::collections::BTreeSet<(String, String)>,
    tool_name: &str,
    arguments_fingerprint: &str,
    transport_ran: bool,
    success: bool,
) -> bool {
    let key = (tool_name.to_string(), arguments_fingerprint.to_string());
    let recovered = success && failed_tool_fingerprints.contains(&key);
    if transport_ran && !success {
        failed_tool_fingerprints.insert(key);
    }
    recovered
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDispatchClassification {
    pub name_valid: bool,
    pub arguments_parsed: bool,
    pub schema_valid: bool,
    pub policy_allowed: bool,
    pub approval_required: bool,
    pub approval_obtained: bool,
    pub transport_ran: bool,
    pub tool_reported_success: bool,
    pub result_validation_success: bool,
    pub outcome: LlmToolLineageOutcome,
    pub failure_owner: Option<LlmToolFailureOwner>,
    pub failure_code: Option<String>,
}

impl ToolDispatchClassification {
    pub fn record_obtained_approval(&mut self) {
        self.approval_required = true;
        self.approval_obtained = true;
    }
}

/// Classify a normalized tool result without inspecting prose. The explicit
/// machine error code is authoritative; unknown failure shapes are owned by
/// the tool rather than guessed as model or policy failures.
pub fn classify_tool_dispatch(
    success: bool,
    error_code: Option<&str>,
) -> ToolDispatchClassification {
    let code = error_code.map(normalize_machine_code);
    let code_ref = code.as_deref().unwrap_or_default();
    let owner = if success {
        None
    } else if code_ref.contains("unknown_tool") || code_ref.contains("unknowntool") {
        Some(LlmToolFailureOwner::Model)
    } else if code_ref.contains("argument") || code_ref.contains("parameter") {
        Some(LlmToolFailureOwner::Arguments)
    } else if code_ref.contains("schema") {
        Some(LlmToolFailureOwner::Schema)
    } else if code_ref.contains("approval") || code_ref.contains("confirmation") {
        Some(LlmToolFailureOwner::Approval)
    } else if code_ref.contains("not_allowed")
        || code_ref.contains("notallowed")
        || code_ref.contains("trust")
        || code_ref.contains("policy")
        || code_ref.contains("authorization")
    {
        Some(LlmToolFailureOwner::Policy)
    } else if code_ref.contains("resource_authority") || code_ref.contains("spend") {
        Some(LlmToolFailureOwner::ResourceAuthority)
    } else if code_ref.contains("result_validation") || code_ref.contains("result_contract") {
        Some(LlmToolFailureOwner::ResultValidation)
    } else if code_ref.contains("cancel") || code_ref.contains("stopped") {
        Some(LlmToolFailureOwner::Cancellation)
    } else if code_ref.contains("timeout") || code_ref.contains("runtime_dispatch") {
        Some(LlmToolFailureOwner::Runtime)
    } else if code_ref.contains("provider") || code_ref.contains("upstream") {
        Some(LlmToolFailureOwner::ExternalProvider)
    } else {
        Some(LlmToolFailureOwner::Tool)
    };
    let name_valid = !matches!(owner, Some(LlmToolFailureOwner::Model));
    let arguments_parsed = !(code_ref.contains("argument_parse")
        || code_ref.contains("arguments_parse")
        || code_ref.contains("invalid_json")
        || code_ref.contains("malformed_args"));
    let schema_valid = !matches!(
        owner,
        Some(LlmToolFailureOwner::Arguments | LlmToolFailureOwner::Schema)
    );
    let policy_allowed = !matches!(
        owner,
        Some(LlmToolFailureOwner::Policy | LlmToolFailureOwner::ResourceAuthority)
    );
    let approval_required = matches!(owner, Some(LlmToolFailureOwner::Approval));
    let approval_obtained = false;
    let transport_ran = success
        || matches!(
            owner,
            Some(
                LlmToolFailureOwner::Tool
                    | LlmToolFailureOwner::ResultValidation
                    | LlmToolFailureOwner::ExternalProvider
                    | LlmToolFailureOwner::Cancellation
            )
        )
        || matches!(owner, Some(LlmToolFailureOwner::Runtime))
            && !code_ref.contains("runtime_dispatch");
    let result_validation_success =
        transport_ran && !matches!(owner, Some(LlmToolFailureOwner::ResultValidation));
    ToolDispatchClassification {
        name_valid,
        arguments_parsed,
        schema_valid,
        policy_allowed,
        approval_required,
        approval_obtained,
        transport_ran,
        tool_reported_success: success
            || matches!(owner, Some(LlmToolFailureOwner::ResultValidation)),
        result_validation_success,
        outcome: if success {
            LlmToolLineageOutcome::Succeeded
        } else if matches!(
            owner,
            Some(
                LlmToolFailureOwner::Policy
                    | LlmToolFailureOwner::ResourceAuthority
                    | LlmToolFailureOwner::Approval
            )
        ) {
            LlmToolLineageOutcome::Denied
        } else if matches!(owner, Some(LlmToolFailureOwner::Cancellation)) {
            LlmToolLineageOutcome::Cancelled
        } else if code_ref.contains("timeout") {
            LlmToolLineageOutcome::TimedOut
        } else {
            LlmToolLineageOutcome::Failed
        },
        failure_owner: owner,
        failure_code: code,
    }
}

fn normalize_machine_code(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len().min(128));
    for ch in value.trim().chars().take(128) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':' | '/') {
            normalized.push(ch.to_ascii_lowercase());
        } else {
            normalized.push('_');
        }
    }
    if normalized.is_empty() {
        "unknown_tool_failure".to_string()
    } else {
        normalized
    }
}

fn canonical_json_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_json_value).collect()),
        Value::Object(values) => {
            let sorted = values
                .iter()
                .map(|(key, value)| (key.clone(), canonical_json_value(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        },
        _ => value.clone(),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    // ========================================================================
    // Effect identity — docs/components/magician/effect-identity.md
    // ========================================================================

    #[test]
    fn a_replayed_attempt_keeps_its_key_but_a_redecided_retry_gets_a_new_one() {
        // The property the whole design rests on. A crash and re-run of the
        // SAME decision must reuse the key, so the effect is recognised as
        // already-attempted. A deliberate retry re-decides, producing a new
        // llm_call_id, and must NOT collide — otherwise a legitimate retry
        // would be suppressed as a duplicate.
        let replay_of_same_attempt =
            LlmToolLineageIdentity::tool_execution_id_for("llm_call_abc", "toolu_01");
        let same_again = LlmToolLineageIdentity::tool_execution_id_for("llm_call_abc", "toolu_01");
        assert_eq!(
            replay_of_same_attempt, same_again,
            "re-running the same decision must reuse the key"
        );

        let after_redecide =
            LlmToolLineageIdentity::tool_execution_id_for("llm_call_xyz", "toolu_01");
        assert_ne!(
            replay_of_same_attempt, after_redecide,
            "a re-decided retry must not be mistaken for a replay"
        );

        let other_call_same_turn =
            LlmToolLineageIdentity::tool_execution_id_for("llm_call_abc", "toolu_02");
        assert_ne!(
            replay_of_same_attempt, other_call_same_turn,
            "two tool calls in one turn are separate effects"
        );
    }

    #[test]
    fn a_blank_tool_call_id_would_collapse_two_effects_into_one_key() {
        // Not a demonstration of correct behaviour — a demonstration of WHY
        // callers must filter. Composition cannot tell two unidentified calls
        // apart, so both would present a remote with the same
        // `Idempotency-Key` and the second effect would silently vanish into
        // the first one's cached response. `effect_id_for_candidate` is where
        // that is prevented, by refusing to build a key at all.
        let first = LlmToolLineageIdentity::tool_execution_id_for("llm_call_abc", "");
        let second = LlmToolLineageIdentity::tool_execution_id_for("llm_call_abc", "");
        assert_eq!(
            first, second,
            "two distinct calls with no id compose to one key — hence the caller-side filter"
        );
    }

    #[test]
    fn the_far_side_key_does_not_disclose_the_internal_identifier() {
        // This value is sent to arbitrary third-party APIs in an
        // `Idempotency-Key` header. The raw effect id embeds `llm_call_id`;
        // leaking it would disclose internal structure to anyone we call.
        let effect_id = "llm_call_secret123:tool:toolu_01";
        let key = LlmToolLineageIdentity::far_side_idempotency_key(effect_id);

        assert!(
            !key.contains("llm_call_secret123"),
            "the internal call id must not travel to a remote: {key}"
        );
        assert!(
            !key.contains("toolu_01"),
            "nor the provider tool-call id: {key}"
        );
        assert!(!key.contains(effect_id), "nor the raw effect id: {key}");
        assert!(
            key.starts_with("mag-"),
            "expected a namespaced key, got {key}"
        );
    }

    #[test]
    fn the_value_sent_to_a_remote_never_equals_the_one_indexing_our_own_records() {
        // Domain separation, and the reason for it: the far-side key travels to
        // third parties. If it were also what indexes a local audit line, a
        // remote holding it would be holding our internal index. The two
        // derivations must not agree for any attempt.
        for effect_id in [
            "llm_call_abc:tool:toolu_01",
            "llm_call_xyz:tool:toolu_99",
            "",
        ] {
            let far = LlmToolLineageIdentity::far_side_idempotency_key(effect_id);
            let local = LlmToolLineageIdentity::local_attempt_key(effect_id);
            assert_ne!(
                far.trim_start_matches("mag-"),
                local,
                "the two derivations collided for {effect_id:?}"
            );
        }
    }

    #[test]
    fn the_local_key_is_stable_per_attempt_and_distinct_across_attempts() {
        // Stability is what lets a replayed attempt reuse one credential call
        // id, so the audit shows one use rather than two. Distinctness is what
        // stops two real invocations collapsing onto one line.
        let first = LlmToolLineageIdentity::local_attempt_key("call_a:tool:t1");
        let repeat = LlmToolLineageIdentity::local_attempt_key("call_a:tool:t1");
        let other = LlmToolLineageIdentity::local_attempt_key("call_b:tool:t1");

        assert_eq!(first, repeat);
        assert_ne!(first, other);
    }

    #[test]
    fn the_local_key_is_spelled_the_way_a_credential_call_id_must_be() {
        // `CredentialCallId::new` refuses anything outside `[A-Za-z0-9_-.+]`,
        // and an effect id contains colons — which is why this is derived at
        // all. A digest that ever emitted another character would fail the
        // constructor at runtime, on the governed path, mid-dispatch.
        let key = LlmToolLineageIdentity::local_attempt_key("llm_call_abc:tool:toolu_01");
        assert!(
            key.bytes()
                .all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'_' | b'-' | b'.' | b'+')),
            "not a valid credential call id component: {key}"
        );
        assert!(!key.is_empty());
        assert!(
            !key.contains("llm_call_abc"),
            "the internal call id must not survive into a child environment: {key}"
        );
    }

    #[test]
    fn the_far_side_key_is_stable_per_attempt_and_distinct_across_attempts() {
        // Stability is what makes a remote able to collapse a duplicate;
        // distinctness is what stops it collapsing two real attempts into one.
        let first = LlmToolLineageIdentity::far_side_idempotency_key("call_a:tool:t1");
        let repeat = LlmToolLineageIdentity::far_side_idempotency_key("call_a:tool:t1");
        let other = LlmToolLineageIdentity::far_side_idempotency_key("call_b:tool:t1");

        assert_eq!(first, repeat, "the same attempt must derive the same key");
        assert_ne!(
            first, other,
            "different attempts must derive different keys"
        );
    }

    #[test]
    fn per_step_keys_stay_distinct_under_the_far_side_derivation() {
        // A workflow replay turns ONE tool call into several HTTP requests. If
        // the derivation collapsed the per-step keys, a remote honouring
        // idempotency would drop every step after the first — turning a
        // fan-out into a single request and silently losing work.
        let effect_id = "llm_call_abc:tool:toolu_01";
        let step_one =
            LlmToolLineageIdentity::far_side_idempotency_key(&format!("{effect_id}:step:s1"));
        let step_two =
            LlmToolLineageIdentity::far_side_idempotency_key(&format!("{effect_id}:step:s2"));
        let whole = LlmToolLineageIdentity::far_side_idempotency_key(effect_id);

        assert_ne!(step_one, step_two, "each step is its own effect");
        assert_ne!(step_one, whole, "a step is not the whole dispatch");
    }

    // ========================================================================
    // Phase 6 — side-effect state resolved from the execution record
    // ========================================================================

    fn lineage_identity() -> LlmToolLineageIdentity {
        LlmToolLineageIdentity::without_content_fingerprint(
            magicllm::LlmTraceContext::new(
                magicllm::LlmScope::new("principal-a", "workspace-a"),
                magicllm::LlmWorkloadClass::ForegroundChat,
            ),
            "toolu_01",
            "branch-1",
            "agentic_decision",
            "agentic_task",
            "browser",
            Some("browser".to_string()),
        )
    }

    #[test]
    fn a_resolved_state_is_never_recorded_without_the_fact_that_supports_it() {
        // REGRESSION GUARD for a bug this review found in its own work.
        // `validate_tool_lineage` refuses a `Remained` whose record does not
        // also say a transport ran. `ResultValidated` and `ResultConsumed` had
        // always set the state alone — harmless while the state could never be
        // `Remained`, and an instant rejection at journal append once it could.
        //
        // Every emitter now sets the pair through one call, so the two cannot
        // drift. This checks that what that call produces is a record the
        // recorder actually accepts.
        let identity = lineage_identity();
        for (transport_ran, success) in [(true, true), (true, false), (false, false)] {
            let mut record = identity.record(LlmToolLineageStage::ResultConsumed, 1, 1_700_000_000);
            record.result_ref = Some(identity.result_ref());
            record.consumed_by_call_id = Some("llm_call_next".to_string());
            record.outcome = LlmToolLineageOutcome::Succeeded;
            apply_side_effect_resolution(
                &mut record,
                transport_ran,
                resolved_side_effect_state(transport_ran, success),
            );

            assert_eq!(record.transport_ran, Some(transport_ran));
            crate::magician_v2::analytics::llm_trace_recorder::LlmTraceRecord::ToolLineage(record)
                .validate()
                .unwrap_or_else(|error| {
                    panic!(
                        "the recorder rejected a record this module emits \
                         (transport_ran={transport_ran}, success={success}): {error}"
                    )
                });
        }
    }

    #[test]
    fn every_path_that_names_an_attempt_composes_it_the_same_way() {
        // Two derivations of one id drift, and the drift would look like a
        // dispatch whose lineage could not find its own effect. The
        // fingerprint-free fallback used to format this string by hand.
        let identity = lineage_identity();
        assert_eq!(
            identity.tool_execution_id,
            LlmToolLineageIdentity::tool_execution_id_for(
                &identity.context.llm_call_id,
                &identity.model_tool_call_id,
            )
        );
    }

    #[test]
    fn a_successful_dispatch_records_that_its_effect_stands() {
        // REGRESSION GUARD, and the whole of Phase 6. `Remained` had no
        // producer anywhere: every successful side-effecting dispatch was
        // recorded `Unknown`, which collapsed "we know this happened" into "we
        // have no idea" — the one distinction the effect-identity work exists
        // to draw.
        assert_eq!(
            resolved_side_effect_state(true, true),
            LlmToolSideEffectState::Remained
        );
    }

    #[test]
    fn a_failure_after_transport_stays_honestly_unresolved() {
        // The case that genuinely cannot be decided here: a failure,
        // cancellation or timeout after the transport ran cannot prove no
        // mutation happened — the request may have landed and the response been
        // lost. That is exactly when a far-side idempotency key earns its keep.
        assert_eq!(
            resolved_side_effect_state(true, false),
            LlmToolSideEffectState::Unknown
        );
    }

    #[test]
    fn a_dispatch_that_never_left_the_process_has_no_effect_to_resolve() {
        // An approval gate or admission failure returned before anything ran,
        // so there is nothing to have happened — distinct from "we don't know".
        for tool_reported_success in [true, false] {
            assert_eq!(
                resolved_side_effect_state(false, tool_reported_success),
                LlmToolSideEffectState::None,
                "no transport means no effect, whatever the result claims"
            );
        }
    }

    #[test]
    fn the_classification_reader_agrees_with_the_rule_it_wraps() {
        // Two derivations of one answer drift. The loop carries the state from
        // the finished dispatch to the result-consumed record through this
        // reader precisely so it cannot compute a second, different one.
        for (transport_ran, success) in [(true, true), (true, false), (false, false)] {
            let mut classification = classify_tool_dispatch(success, (!success).then_some("boom"));
            classification.transport_ran = transport_ran;
            classification.tool_reported_success = success;

            assert_eq!(
                resolved_side_effect_state_for(&classification),
                resolved_side_effect_state(transport_ran, success),
                "reader disagreed for transport_ran={transport_ran} success={success}"
            );
        }
    }

    #[test]
    fn classification_keeps_policy_approval_and_tool_failures_separate() {
        let malformed = classify_tool_dispatch(false, Some("invalid_json_arguments"));
        assert_eq!(
            malformed.failure_owner,
            Some(LlmToolFailureOwner::Arguments)
        );
        assert!(!malformed.arguments_parsed);
        assert!(!malformed.schema_valid);
        assert!(!malformed.transport_ran);

        let unknown = classify_tool_dispatch(false, Some("unknown_tool"));
        assert_eq!(unknown.failure_owner, Some(LlmToolFailureOwner::Model));
        assert!(!unknown.name_valid);
        assert!(!unknown.transport_ran);

        let policy = classify_tool_dispatch(false, Some("ChatPolicySnapshotChanged"));
        assert_eq!(policy.outcome, LlmToolLineageOutcome::Denied);
        assert_eq!(policy.failure_owner, Some(LlmToolFailureOwner::Policy));
        assert!(!policy.policy_allowed);
        assert!(!policy.transport_ran);

        let approval = classify_tool_dispatch(false, Some("ChatStructuralApprovalRequired"));
        assert_eq!(approval.outcome, LlmToolLineageOutcome::Denied);
        assert_eq!(approval.failure_owner, Some(LlmToolFailureOwner::Approval));
        assert!(approval.approval_required);
        assert!(!approval.approval_obtained);
        assert!(!approval.transport_ran);

        let mut approved_success = classify_tool_dispatch(true, None);
        approved_success.record_obtained_approval();
        assert!(approved_success.approval_required);
        assert!(approved_success.approval_obtained);

        let outage = classify_tool_dispatch(false, Some("upstream_provider_unavailable"));
        assert_eq!(
            outage.failure_owner,
            Some(LlmToolFailureOwner::ExternalProvider)
        );
        assert!(outage.transport_ran);
        assert!(!outage.tool_reported_success);
        assert!(outage.result_validation_success);

        let never_dispatched = classify_tool_dispatch(false, Some("action_not_allowed"));
        assert!(!never_dispatched.transport_ran);

        let cancellation = classify_tool_dispatch(false, Some("execution_cancelled"));
        assert_eq!(cancellation.outcome, LlmToolLineageOutcome::Cancelled);
        assert_eq!(
            cancellation.failure_owner,
            Some(LlmToolFailureOwner::Cancellation)
        );
        assert!(cancellation.transport_ran);

        let timeout = classify_tool_dispatch(false, Some("runtime_timeout"));
        assert_eq!(timeout.outcome, LlmToolLineageOutcome::TimedOut);
        assert_eq!(timeout.failure_owner, Some(LlmToolFailureOwner::Runtime));
        assert!(timeout.transport_ran);

        let result_contract =
            classify_tool_dispatch(false, Some("result_contract_validation_failed"));
        assert_eq!(
            result_contract.failure_owner,
            Some(LlmToolFailureOwner::ResultValidation)
        );
        assert!(result_contract.transport_ran);
        assert!(result_contract.tool_reported_success);
        assert!(!result_contract.result_validation_success);

        let dispatch = classify_tool_dispatch(false, Some("runtime_dispatch_error"));
        assert_eq!(dispatch.failure_owner, Some(LlmToolFailureOwner::Runtime));
        assert!(!dispatch.transport_ran);
        assert!(!dispatch.result_validation_success);
    }

    #[test]
    fn recovery_requires_an_earlier_authoritative_transport_failure() {
        let mut failed = std::collections::BTreeSet::new();
        assert!(!observe_tool_recovery(
            &mut failed,
            "browser__click",
            "fingerprint",
            false,
            false,
        ));
        assert!(!observe_tool_recovery(
            &mut failed,
            "browser__click",
            "fingerprint",
            true,
            true,
        ));
        assert!(!observe_tool_recovery(
            &mut failed,
            "browser__click",
            "fingerprint",
            true,
            false,
        ));
        assert!(observe_tool_recovery(
            &mut failed,
            "browser__click",
            "fingerprint",
            true,
            true,
        ));
    }

    #[test]
    fn failed_transport_never_invents_absence_of_side_effects() {
        // The invariant this has guarded since before the rule could resolve
        // anything: a transport that ran must never be recorded as having had
        // no side effect, whatever it went on to report. `None` is legitimate
        // only when nothing ran at all — anything looser would let a failed
        // mutation read as a dispatch that never touched the world.
        for tool_reported_success in [true, false] {
            assert_ne!(
                resolved_side_effect_state(true, tool_reported_success),
                LlmToolSideEffectState::None,
                "a transport that ran cannot have produced no side effect"
            );
        }
        assert_eq!(
            resolved_side_effect_state(false, false),
            LlmToolSideEffectState::None
        );
    }

    #[test]
    fn related_execution_ids_are_deduplicated_bounded_and_content_free() {
        let mut values = (0..=MAX_TOOL_RELATED_EXECUTION_IDS)
            .map(|index| format!("child_{index:03}"))
            .collect::<Vec<_>>();
        values.push("child_000".to_string());
        values.push(" invalid ".to_string());
        let (normalized, incomplete) = normalized_related_execution_ids(&values);
        assert!(incomplete);
        assert_eq!(normalized.len(), MAX_TOOL_RELATED_EXECUTION_IDS);
        assert_eq!(normalized[0], "child_000");
        assert_eq!(normalized[MAX_TOOL_RELATED_EXECUTION_IDS - 1], "child_063");
    }

    #[test]
    fn canonical_json_sorts_object_keys_without_reordering_arrays() {
        let first = serde_json::json!({"b": [2, 1], "a": {"z": true, "x": false}});
        let second = serde_json::json!({"a": {"x": false, "z": true}, "b": [2, 1]});
        assert_eq!(canonical_json_value(&first), canonical_json_value(&second));
    }

    #[test]
    fn multiple_and_parallel_tool_calls_keep_distinct_provider_identities() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let context = LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        );
        let arguments = serde_json::json!({"url": "https://example.test"});
        let first = LlmToolLineageIdentity::new(
            &workspace,
            context.clone(),
            "provider_call_1",
            "branch_1",
            "chat",
            "chat",
            "browser__open",
            Some("browser".to_string()),
            &arguments,
        )
        .expect("first identity");
        let second = LlmToolLineageIdentity::new(
            &workspace,
            context,
            "provider_call_2",
            "branch_2",
            "chat",
            "chat",
            "browser__open",
            Some("browser".to_string()),
            &arguments,
        )
        .expect("second identity");

        assert_ne!(first.model_tool_call_id, second.model_tool_call_id);
        assert_ne!(first.tool_execution_id, second.tool_execution_id);
        assert_ne!(first.branch_id, second.branch_id);
        assert_eq!(first.arguments_fingerprint, second.arguments_fingerprint);
    }
}
