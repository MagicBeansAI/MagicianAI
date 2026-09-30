//! Durable summary record for the latest agentic execution.

use serde::{Deserialize, Serialize};

/// Durable summary for the latest agentic execution for an execution run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgenticExecutionSummaryRecord {
    #[serde(rename = "execution_id")]
    pub execution_id: String,
    pub plan_id: String,
    pub step_id: String,
    pub outcome: String,
    pub iterations_used: usize,
    pub artifacts: Vec<String>,
    pub duration_ms: u64,
    pub summary: String,
    pub timestamp: i64,
    /// Execution loop topology for this run: `"flat"` (the flatten plan's
    /// single-loop flat-catalog mode) or `"inner"` (the default nested-LLM
    /// inner-loop mode). Stamped from `AgenticContext.loop_mode` so flat-vs-inner
    /// A/B (completion rate / latency / iterations) is separable per execution
    /// (join to per-call token telemetry by `execution_id`). `None` for legacy /
    /// summaries built without loop-mode context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_detection_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_repeated_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_recommendation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_cycle_pattern: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_similarity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_dimension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cannot_proceed_reason: Option<String>,
    /// Structured yield payload — present when the execution terminated
    /// via `Decision::Yield`. Carries the LLM's structured outcome
    /// fields (`completed`, `open`, `blockers`, `next_step_hint`) so UI
    /// surfaces and downstream consumers can render a partial-progress
    /// card instead of relying on the freeform `summary` / artifact.
    ///
    /// Phase 2.5 of the yield-decision migration; see
    /// `docs/components/magician/execution/YIELD_DECISION.md` and
    /// `docs/plans/2026-05-27-yield-decision-migration.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_payload: Option<YieldPayloadSummary>,
    /// Structured per-child deliverables threaded into a resumed parent
    /// orchestrator after its delegated children finished. Present only on the
    /// parent's `delegation_results_ready` record so the resume prompt can
    /// surface each child's primary text + media refs — otherwise synthesis is
    /// blind to its own children's outputs and reports them as "missing".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_deliverables: Vec<ChildDeliverableSummary>,
}

/// Wire-format projection of `YieldDecision` for the durable summary
/// record. Mirrors the structured fields without the artifact bytes
/// (those land separately in the `artifacts` list as `output_id`
/// references).

impl AgenticExecutionSummaryRecord {
    /// The completion kind this record proves, and the yield's `open[]`: the
    /// payload's own disposition when present, the `goal_achieved_partial`
    /// outcome string for records written before the payload existed. `None`
    /// for any outcome that is not a completion.
    pub fn completion(
        &self,
    ) -> (
        Option<crate::magician_v2::execution::agentic::types::CompletionKind>,
        Vec<String>,
    ) {
        let completed = self.outcome == "goal_achieved"
            || self.outcome == "goal_achieved_partial"
            || self.outcome == "success";
        if !completed {
            return (None, Vec::new());
        }
        let partial = self
            .yield_payload
            .as_ref()
            .is_some_and(|payload| payload.disposition == "partial_success")
            || self.outcome == "goal_achieved_partial";
        (
            Some(if partial {
                crate::magician_v2::execution::agentic::types::CompletionKind::Partial
            } else {
                crate::magician_v2::execution::agentic::types::CompletionKind::Full
            }),
            self.yield_payload
                .as_ref()
                .map(|payload| payload.open.clone())
                .unwrap_or_default(),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct YieldPayloadSummary {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<YieldBlockerSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step_hint: Option<String>,
    /// String-encoded `YieldDisposition` so the UI can branch without
    /// re-running `dispose_yield`. Values: `completed`, `partial_success`,
    /// `failed`, `retry_transient`.
    pub disposition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct YieldBlockerSummary {
    /// String-encoded `YieldBlockerKind` (`auth`, `data_missing`,
    /// `permission`, `transient`, `external`, `other`).
    pub kind: String,
    pub description: String,
}

/// A media artifact a delegated child promoted into the shared task `outputs/`
/// dir (image / video / audio), referenced back to the resuming parent so its
/// synthesis can cite/inline it instead of declaring it missing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChildMediaRef {
    /// Path under the task `outputs/` dir (leading `outputs/` already stripped),
    /// e.g. `image-generation__run_0_..._exec_....jpg`.
    pub relative_path: String,
    pub media_type: String,
    /// Ready-to-serve URL:
    /// `/api/magician/v3/tasks/{task_id}/outputs/{path}` (bearer-authenticated).
    pub serving_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

/// One delegated child's deliverables, assembled at reconcile time so the
/// resuming parent orchestrator sees the child's actual outputs (primary text +
/// media) rather than only a one-line disposition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChildDeliverableSummary {
    pub child_execution_id: String,
    pub agent_id: String,
    /// Human label (child execution title / context).
    pub context: String,
    /// The child's primary execution output (`out_exec_<child>` body —
    /// a disposition + key findings), length-bounded by the reader's existing
    /// cap. `None` when the child produced no readable text output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_text: Option<String>,
    /// References to the child's media artifacts promoted into the task
    /// `outputs/` dir.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<ChildMediaRef>,
}

/// Render structured child deliverables into a prompt block the resuming parent
/// orchestrator can act on. Returns an empty string when there are none.
///
/// Shared by BOTH delegation-results injection paths — the decision-prompt
/// section (`decision::build_delegation_results_section`) and the
/// direct/autonomous child-results section
/// (`v2_orchestrator::build_delegation_child_results_section`) — so the parent
/// sees its children's real outputs on whichever route it resumes through, and
/// never reports them as "missing".
pub fn render_child_deliverables_block(deliverables: &[ChildDeliverableSummary]) -> String {
    if deliverables.is_empty() {
        return String::new();
    }
    let mut block = String::from(
        "Child deliverables (use these as your source material — do NOT claim they are missing):\n",
    );
    for child in deliverables {
        block.push_str(&format!(
            "\n**{}** (`{}`):\n",
            child.context, child.agent_id
        ));
        if let Some(text) = child
            .primary_text
            .as_ref()
            .map(|text| text.trim())
            .filter(|text| !text.is_empty())
        {
            block.push_str("Primary execution output (disposition + key findings):\n");
            block.push_str(text);
            block.push('\n');
        }
        if !child.media.is_empty() {
            block
                .push_str("Media artifacts (reference these directly in the final deliverable):\n");
            for media in &child.media {
                block.push_str(&format!(
                    "- {} ({}) — serve at {}\n",
                    media.relative_path, media.media_type, media.serving_url
                ));
            }
        }
    }
    block.push_str(
        "\nAssemble these into ONE complete deliverable and report what was produced accurately.\n",
    );
    block
}

/// STEP 3: one stage of a single-execution-context pipeline — the agent that runs
/// it and the stage's instruction. Prior stages' deliverables are threaded into
/// the stage at run time via [`render_stage_goal`], so the always-sequential
/// `orchestrate_pipeline` can run stages within ONE execution (no spawned child
/// per stage, no reconcile round-trip).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PipelineStage {
    pub agent_id: String,
    pub context: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_criteria: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// STEP 3: in-memory state of a single-execution-context pipeline — the ordered
/// stage roster, the cursor, and the accumulated per-stage deliverables (threaded
/// into each subsequent stage and aggregated into the final deliverable). This
/// replaces the spawn-child-per-stage + reconcile model for the always-sequential
/// `orchestrate_pipeline` path.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PipelineState {
    pub stages: Vec<PipelineStage>,
    pub cursor: usize,
    pub deliverables: Vec<ChildDeliverableSummary>,
}

/// STEP 3: build a pipeline stage's goal — its own instruction plus the prior
/// stages' deliverables threaded in via the SAME renderer the delegation-resume
/// paths use ([`render_child_deliverables_block`]). This is how a
/// single-execution-context pipeline threads stage N's output into stage N+1
/// IN MEMORY — no reconcile, no disk round-trip. Pure function.
pub fn render_stage_goal(
    stage_context: &str,
    prior_deliverables: &[ChildDeliverableSummary],
) -> String {
    let block = render_child_deliverables_block(prior_deliverables);
    if block.trim().is_empty() {
        stage_context.to_string()
    } else {
        format!("{stage_context}\n\n{block}")
    }
}

// ─── Work-ledger projector (P1) ─────────────────────────────────────────────

/// Pure projector: build one deterministic `work_outcome` evidence record from a
/// terminal root run's completion summary. The work-ledger writer wires this into
/// the completion funnel (gated on root-ness) so every terminal root agentic run
/// stamps exactly one run-grained ledger record.
///
/// Mapping (prefer the structured yield payload, fall back to the freeform
/// summary fields):
/// - `outcome`         ← `summary.outcome`
/// - `summary`         ← `summary.yield_payload.summary` if present, else `summary.summary`
/// - `artifacts`       ← `summary.artifacts`
/// - `open_loops`      ← `summary.yield_payload.open` (else empty)
/// - `next_step_hint`  ← `summary.yield_payload.next_step_hint`
/// - `timestamp_ms`    ← `summary.timestamp`
/// - `entity_keys`     ← empty (no entity resolution on the deterministic stamp)
///
/// Pure so it is unit-testable and free of I/O / scope resolution.
/// Build the deterministic [`WorkOutcomeInput`](crate::magician_v2::evidence::WorkOutcomeInput)
/// from a completed root run's summary. This is the shared projection consumed
/// both by the work-ledger evidence record ([`work_outcome_from_summary`]) and by
/// the HARNESS program-state distiller (P2.1), so both lanes read exactly the same
/// open-loops / next-step / summary the run yielded.
pub fn work_outcome_input_from_summary(
    summary: &AgenticExecutionSummaryRecord,
    agent_id: &str,
    task_id: Option<&str>,
    root_execution_id: &str,
) -> crate::magician_v2::evidence::WorkOutcomeInput {
    let (final_summary, open_loops, next_step_hint) = match summary.yield_payload.as_ref() {
        Some(payload) => (
            payload.summary.clone(),
            payload.open.clone(),
            payload.next_step_hint.clone(),
        ),
        None => (summary.summary.clone(), Vec::new(), None),
    };

    crate::magician_v2::evidence::WorkOutcomeInput {
        root_execution_id: root_execution_id.to_string(),
        task_id: task_id.map(str::to_string),
        agent_id: agent_id.to_string(),
        outcome: summary.outcome.clone(),
        summary: final_summary,
        artifacts: summary.artifacts.clone(),
        open_loops,
        next_step_hint,
        entity_keys: Vec::new(),
        timestamp_ms: summary.timestamp,
    }
}

pub fn work_outcome_from_summary(
    summary: &AgenticExecutionSummaryRecord,
    agent_id: &str,
    task_id: Option<&str>,
    root_execution_id: &str,
) -> crate::magician_v2::evidence::EvidenceRecord {
    crate::magician_v2::evidence::EvidenceRecord::from_work_outcome(
        work_outcome_input_from_summary(summary, agent_id, task_id, root_execution_id),
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn base_summary() -> AgenticExecutionSummaryRecord {
        AgenticExecutionSummaryRecord {
            execution_id: "exec-root".into(),
            plan_id: "plan-1".into(),
            step_id: "step-1".into(),
            outcome: "success".into(),
            iterations_used: 3,
            artifacts: vec!["artifact:report.md".into(), "artifact:pr-42".into()],
            duration_ms: 1234,
            summary: "Freeform run summary".into(),
            timestamp: 1_760_000_000_000,
            loop_mode: None,
            loop_detection_type: None,
            loop_repeated_action: None,
            loop_recommendation: None,
            loop_cycle_pattern: None,
            loop_similarity: None,
            budget_dimension: None,
            budget_details: None,
            cannot_proceed_reason: None,
            yield_payload: None,
            child_deliverables: Vec::new(),
        }
    }

    #[test]
    fn work_outcome_from_summary_maps_freeform_fields_when_no_yield() {
        let summary = base_summary();
        let record =
            work_outcome_from_summary(&summary, "agent-a", Some("task-1"), &summary.execution_id);

        // id is run-grained off the root execution id
        assert_eq!(record.evidence_id, "evd:run:exec-root");
        assert_eq!(record.evidence_kind, "work_outcome");
        assert_eq!(record.producer, "work_outcome");
        // outcome mapping
        assert_eq!(record.metadata["outcome"], "success");
        // summary falls back to the freeform summary
        assert_eq!(record.summary, "Freeform run summary");
        // artifacts mapped through verbatim
        assert_eq!(
            record.artifact_refs,
            vec![
                "artifact:report.md".to_string(),
                "artifact:pr-42".to_string()
            ]
        );
        // no yield payload → empty open loops, null next-step hint
        assert!(record.metadata["open_loops"].as_array().unwrap().is_empty());
        assert!(record.metadata["next_step_hint"].is_null());
        // task/agent ride in metadata; entity_keys empty on the deterministic stamp
        assert_eq!(record.metadata["task_id"], "task-1");
        assert_eq!(record.metadata["agent_id"], "agent-a");
        assert!(record.entity_keys.is_empty());
        assert_eq!(record.source_refs, vec!["execution:exec-root".to_string()]);
    }

    #[test]
    fn work_outcome_from_summary_prefers_yield_payload() {
        let mut summary = base_summary();
        summary.outcome = "goal_achieved_partial".into();
        summary.yield_payload = Some(YieldPayloadSummary {
            summary: "Structured yield summary".into(),
            completed: vec!["did the thing".into()],
            open: vec!["follow up on review".into(), "await CI".into()],
            blockers: Vec::new(),
            next_step_hint: Some("merge after CI".into()),
            disposition: "partial_success".into(),
        });

        let record = work_outcome_from_summary(&summary, "agent-a", None, &summary.execution_id);

        // outcome comes from the record-level outcome, not the disposition
        assert_eq!(record.metadata["outcome"], "goal_achieved_partial");
        // summary prefers the structured yield summary
        assert_eq!(record.summary, "Structured yield summary");
        // open loops come from yield_payload.open
        assert_eq!(record.metadata["open_loops"][0], "follow up on review");
        assert_eq!(record.metadata["open_loops"][1], "await CI");
        assert_eq!(record.metadata["next_step_hint"], "merge after CI");
        // task-less run → metadata task_id is null
        assert!(record.metadata["task_id"].is_null());
    }

    /// Guards the P1 blocker fix: the clean-success terminal yield
    /// (`YieldDisposition::Completed`) is the primary happy path and MUST
    /// ledger a `work_outcome`. The executor's pre-disposition-match
    /// placement builds a summary with `outcome = "goal_achieved"` and the
    /// `completed`-disposition yield payload, then calls
    /// `spawn_work_ledger_write` → `work_outcome_from_summary`. This asserts
    /// that projection produces a correct, run-grained `goal_achieved`
    /// ledger record (the shape the Completed arm now emits). The live
    /// executor arm itself needs a full agentic integration harness to
    /// exercise end-to-end; this covers the projector contract it relies on.
    #[test]
    fn work_outcome_from_summary_covers_completed_disposition_success() {
        let mut summary = base_summary();
        summary.outcome = "goal_achieved".into();
        summary.yield_payload = Some(YieldPayloadSummary {
            summary: "Shipped the report".into(),
            completed: vec!["wrote report.md".into()],
            open: Vec::new(),
            blockers: Vec::new(),
            next_step_hint: None,
            disposition: "completed".into(),
        });

        let record =
            work_outcome_from_summary(&summary, "agent-a", Some("task-9"), &summary.execution_id);

        assert_eq!(record.evidence_id, "evd:run:exec-root");
        assert_eq!(record.evidence_kind, "work_outcome");
        // clean-success outcome string, exactly as the Completed arm stamps it
        assert_eq!(record.metadata["outcome"], "goal_achieved");
        assert_eq!(record.summary, "Shipped the report");
        // a clean success has no open loops / next-step hint
        assert!(record.metadata["open_loops"].as_array().unwrap().is_empty());
        assert!(record.metadata["next_step_hint"].is_null());
        assert_eq!(record.metadata["task_id"], "task-9");
    }
}
