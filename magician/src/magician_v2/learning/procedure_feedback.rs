use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::{memory::V3EpisodeRecord, workspace::ArtifactV2Workspace};

use super::{
    CreateLearningCandidateRequest, CreateLearningEventRequest, LearningCandidateState,
    LearningCandidateType, LearningEvent, LearningEventRef, LearningEvidenceRef, LearningProcedure,
    LearningProcedureBridge, LearningProcedureSkillPromotionBridge, LearningProcedureStatus,
    LearningRiskLevel, LearningScope, LearningStore,
};

const MAX_RETRIEVAL_EVENTS_FOR_FEEDBACK: usize = 500;
const PROCEDURE_FEEDBACK_SOURCE: &str = "learning_procedure_feedback";

#[derive(Debug, Clone, Default, Serialize)]
pub struct LearningProcedureUsageContext {
    pub used_procedures: Vec<LearningProcedureUsageRecord>,
    pub matched_event_ids: Vec<String>,
}

impl LearningProcedureUsageContext {
    pub fn is_empty(&self) -> bool {
        self.used_procedures.is_empty()
    }

    pub fn as_prompt_json(&self) -> Value {
        json!({
            "used_procedure_count": self.used_procedures.len(),
            "matched_event_ids": &self.matched_event_ids,
            "used_procedures": &self.used_procedures,
            "review_instruction": "Decide whether each retrieved procedure helped, hurt, was stale, or needs an update. Runtime bookkeeping handles last_used_at/counters/evidence and may create review-gated procedure-to-skill promotion candidates after repeated successes; create memory_procedure candidates only for substantive text, activation, boundary, split/merge, or deprecation proposals."
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LearningProcedureUsageRecord {
    pub procedure_id: String,
    pub title: Option<String>,
    pub owner_agent: Option<String>,
    pub score: Option<i64>,
    pub reason: Option<String>,
    pub success_count_at_retrieval: Option<u64>,
    pub failure_count_at_retrieval: Option<u64>,
    pub retrieval_event_id: String,
    pub retrieved_at: String,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub chat_session_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LearningProcedureFeedbackOutcome {
    pub used_count: usize,
    pub updated_count: usize,
    pub deprecated_count: usize,
    pub skipped_count: usize,
    pub feedback_event_ids: Vec<String>,
    pub deprecated_procedure_ids: Vec<String>,
    pub skill_promotion_candidate_ids: Vec<String>,
    pub skill_promotion_event_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LearningProcedureRunFeedbackJudgement {
    pub procedure_id: String,
    pub verdict: LearningProcedureRunFeedbackVerdict,
    pub rationale: String,
    pub confidence: Option<f64>,
    pub deprecation_recommended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningProcedureRunFeedbackVerdict {
    Useful,
    Harmful,
    Stale,
    Misleading,
    TooBroad,
    TooNarrow,
    Irrelevant,
    Unknown,
}

impl LearningProcedureRunFeedbackVerdict {
    pub fn parse(value: &str) -> Self {
        match normalized_text(value).as_str() {
            "useful" | "helpful" | "worked" | "correct" => Self::Useful,
            "harmful" | "bad" | "wrong" => Self::Harmful,
            "stale" | "outdated" => Self::Stale,
            "misleading" => Self::Misleading,
            "too broad" | "overbroad" => Self::TooBroad,
            "too narrow" => Self::TooNarrow,
            "irrelevant" | "unused" | "not used" | "not relevant" => Self::Irrelevant,
            _ => Self::Unknown,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Useful => "useful",
            Self::Harmful => "harmful",
            Self::Stale => "stale",
            Self::Misleading => "misleading",
            Self::TooBroad => "too_broad",
            Self::TooNarrow => "too_narrow",
            Self::Irrelevant => "irrelevant",
            Self::Unknown => "unknown",
        }
    }

    fn is_positive(self) -> bool {
        matches!(self, Self::Useful)
    }

    fn is_negative(self) -> bool {
        matches!(
            self,
            Self::Harmful | Self::Stale | Self::Misleading | Self::TooBroad | Self::TooNarrow
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProcedureRunOutcome {
    Success,
    Failure,
    Neutral,
    ExplicitNegativeCorrection,
}

impl ProcedureRunOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Neutral => "neutral",
            Self::ExplicitNegativeCorrection => "explicit_negative_correction",
        }
    }

    fn count_success(self) -> bool {
        matches!(self, Self::Success)
    }

    fn count_failure(self) -> bool {
        matches!(self, Self::Failure | Self::ExplicitNegativeCorrection)
    }
}

#[derive(Debug, Clone)]
pub struct LearningProcedureFeedbackBridge {
    workspace_layout: ArtifactV2Workspace,
}

impl LearningProcedureFeedbackBridge {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub fn collect_usage_context_for_episode(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        episode: &V3EpisodeRecord,
    ) -> Result<LearningProcedureUsageContext> {
        let mut events = store.list_events(scope, MAX_RETRIEVAL_EVENTS_FOR_FEEDBACK)?;
        events.retain(|event| event.event_type == "learning_procedure_retrieval_rendered");

        let mut matched_event_ids = Vec::new();
        let mut seen_procedures = HashSet::new();
        let mut used_procedures = Vec::new();
        for event in events {
            if !retrieval_event_matches_episode(&event, episode) {
                continue;
            }
            matched_event_ids.push(event.id.clone());
            for usage in usage_records_from_event(&event) {
                if seen_procedures.insert(usage.procedure_id.clone()) {
                    used_procedures.push(usage);
                }
            }
        }

        Ok(LearningProcedureUsageContext {
            used_procedures,
            matched_event_ids,
        })
    }

    pub fn record_post_run_feedback(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        episode: &V3EpisodeRecord,
        usage_context: &LearningProcedureUsageContext,
        judgements: &[LearningProcedureRunFeedbackJudgement],
        reflection_event: Option<&LearningEventRef>,
        no_op_reason: Option<&str>,
        extra_context: &Value,
    ) -> Result<LearningProcedureFeedbackOutcome> {
        let mut outcome = LearningProcedureFeedbackOutcome {
            used_count: usage_context.used_procedures.len(),
            ..LearningProcedureFeedbackOutcome::default()
        };
        if usage_context.is_empty() {
            return Ok(outcome);
        }

        let run_outcome = infer_episode_outcome(episode);
        let judgements_by_id = judgements
            .iter()
            .map(|judgement| (judgement.procedure_id.as_str(), judgement))
            .collect::<HashMap<_, _>>();
        for usage in &usage_context.used_procedures {
            let procedure = match store.read_procedure(scope, &usage.procedure_id) {
                Ok(procedure) => procedure,
                Err(error) if store.error_is_not_found(&error) => {
                    outcome.skipped_count = outcome.skipped_count.saturating_add(1);
                    continue;
                },
                Err(error) => return Err(error),
            };

            let explicit_negative =
                has_explicit_negative_procedure_feedback(episode, extra_context, usage);
            let judgement = judgements_by_id.get(usage.procedure_id.as_str()).copied();
            let procedure_outcome =
                classify_procedure_outcome(run_outcome, explicit_negative, judgement);
            let evidence_refs = feedback_evidence_refs(episode, usage, reflection_event);
            let updated = store.update_procedure(
                scope,
                &procedure.id,
                PROCEDURE_FEEDBACK_SOURCE,
                "phase_13_usage_feedback",
                feedback_reason(usage, procedure_outcome, no_op_reason),
                evidence_refs.clone(),
                |procedure| {
                    apply_usage_feedback(
                        procedure,
                        usage,
                        episode,
                        procedure_outcome,
                        judgement,
                        reflection_event,
                    )
                },
            )?;

            outcome.updated_count = outcome.updated_count.saturating_add(1);
            let feedback_event = store.append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_feedback_recorded".to_string(),
                    agent_id: Some(episode.agent_id.clone()),
                    task_id: episode.task_id.clone(),
                    execution_id: episode.execution_id.clone(),
                    chat_session_id: chat_session_id(episode),
                    summary: format!(
                        "Procedure `{}` received post-run feedback: {}.",
                        updated.id,
                        procedure_outcome.as_str()
                    ),
                    evidence_refs: evidence_refs.clone(),
                    payload: json!({
                        "procedure_id": &updated.id,
                        "procedure_status": updated.status.as_str(),
                        "retrieval_event_id": &usage.retrieval_event_id,
                        "retrieval_reason": &usage.reason,
                        "outcome": procedure_outcome.as_str(),
                        "success_count": updated.success_count,
                        "failure_count": updated.failure_count,
                        "last_used_at": updated.last_used_at.as_ref().map(|ts| ts.to_rfc3339()),
                        "explicit_negative_feedback": explicit_negative,
                        "procedure_judgement": judgement.map(procedure_judgement_payload),
                        "reflection_event": reflection_event,
                    }),
                },
            )?;
            let feedback_event_id = feedback_event.id.clone();
            outcome.feedback_event_ids.push(feedback_event_id.clone());

            let should_auto_deprecate =
                should_deprecate_after_feedback(&updated, procedure_outcome);
            let deprecation_recommended = judgement
                .map(|judgement| judgement.deprecation_recommended)
                .unwrap_or(false);
            if !should_auto_deprecate
                && !deprecation_recommended
                && procedure_outcome.count_success()
            {
                match LearningProcedureSkillPromotionBridge::new(self.workspace_layout.clone())
                    .maybe_promote_after_feedback(store, scope, &updated)
                {
                    Ok(promotion) => {
                        if let Some(candidate_id) = promotion.promotion_candidate_id {
                            outcome.skill_promotion_candidate_ids.push(candidate_id);
                        }
                        outcome
                            .skill_promotion_event_ids
                            .extend(promotion.event_ids);
                    },
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            procedure_id = %updated.id,
                            "Procedure-to-skill promotion check failed after procedure feedback"
                        );
                    },
                }
            }
            if !should_auto_deprecate {
                if let Some(judgement) = judgement {
                    if let Some(event_id) = self.create_deprecation_candidate_if_recommended(
                        store,
                        scope,
                        episode,
                        &updated,
                        usage,
                        judgement,
                        &feedback_event_id,
                        reflection_event,
                        &evidence_refs,
                    )? {
                        outcome.feedback_event_ids.push(event_id);
                    }
                }
            }

            if should_auto_deprecate {
                let deprecated = store.transition_procedure_status(
                    scope,
                    &updated.id,
                    LearningProcedureStatus::Deprecated,
                    PROCEDURE_FEEDBACK_SOURCE,
                    "phase_13_auto_deprecated",
                    deprecation_reason(&updated, procedure_outcome),
                    evidence_refs.clone(),
                )?;
                let event = store.append_event(
                    scope.clone(),
                    CreateLearningEventRequest {
                        principal: None,
                        workspace: None,
                        event_type: "learning_procedure_deprecated".to_string(),
                        agent_id: Some(episode.agent_id.clone()),
                        task_id: episode.task_id.clone(),
                        execution_id: episode.execution_id.clone(),
                        chat_session_id: chat_session_id(episode),
                        summary: format!(
                            "Procedure `{}` was deprecated after post-run feedback.",
                            deprecated.id
                        ),
                        evidence_refs,
                        payload: json!({
                            "procedure_id": &deprecated.id,
                            "outcome": procedure_outcome.as_str(),
                            "success_count": deprecated.success_count,
                            "failure_count": deprecated.failure_count,
                            "reason": deprecation_reason(&deprecated, procedure_outcome),
                        }),
                    },
                )?;
                outcome.deprecated_count = outcome.deprecated_count.saturating_add(1);
                outcome.deprecated_procedure_ids.push(deprecated.id);
                outcome.feedback_event_ids.push(event.id);
            }
        }

        Ok(outcome)
    }

    fn create_deprecation_candidate_if_recommended(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        episode: &V3EpisodeRecord,
        procedure: &LearningProcedure,
        usage: &LearningProcedureUsageRecord,
        judgement: &LearningProcedureRunFeedbackJudgement,
        feedback_event_id: &str,
        reflection_event: Option<&LearningEventRef>,
        evidence_refs: &[LearningEvidenceRef],
    ) -> Result<Option<String>> {
        if !judgement_requests_reviewed_deprecation(judgement)
            || !matches!(
                procedure.status,
                LearningProcedureStatus::Active | LearningProcedureStatus::Draft
            )
        {
            return Ok(None);
        }

        let rationale = if judgement.rationale.trim().is_empty() {
            format!(
                "Post-run procedure feedback marked procedure `{}` as `{}` and recommended deprecation.",
                procedure.id,
                judgement.verdict.as_str()
            )
        } else {
            truncate_text(judgement.rationale.trim(), 2_000)
        };
        let mut candidate_evidence = evidence_refs.to_vec();
        candidate_evidence.push(LearningEvidenceRef {
            kind: "learning_procedure_feedback".to_string(),
            id: Some(feedback_event_id.to_string()),
            path: None,
            uri: None,
            summary: Some(format!(
                "Procedure feedback verdict `{}` recommended deprecation.",
                judgement.verdict.as_str()
            )),
        });
        let candidate = store.create_candidate(
            scope.clone(),
            CreateLearningCandidateRequest {
                principal: None,
                workspace: None,
                candidate_type: LearningCandidateType::MemoryProcedure,
                state: LearningCandidateState::Proposed,
                title: truncate_text(&format!("Deprecate procedure `{}`", procedure.id), 220),
                summary: truncate_text(
                    &format!(
                        "Review whether active reusable procedure `{}` should stop being injected.",
                        procedure.id
                    ),
                    2_000,
                ),
                rationale: rationale.clone(),
                proposed_change: json!({
                    "procedure": {
                        "procedure_id": &procedure.id,
                        "existing_procedure_id": &procedure.id,
                        "title": &procedure.title,
                        "deprecation_recommended": true,
                        "deprecation_reason": rationale,
                        "feedback_verdict": judgement.verdict.as_str(),
                        "feedback_confidence": judgement.confidence,
                        "source_refs": [
                            {
                                "kind": "learning_procedure_feedback",
                                "id": feedback_event_id
                            },
                            {
                                "kind": "learning_procedure_retrieval",
                                "id": &usage.retrieval_event_id
                            }
                        ]
                    }
                }),
                proposed_target: Some(procedure.id.clone()),
                confidence: judgement.confidence,
                source_agent_id: Some(episode.agent_id.clone()),
                source_task_id: episode.task_id.clone(),
                source_execution_id: episode.execution_id.clone(),
                source_chat_session_id: chat_session_id(episode),
                event_refs: reflection_event.cloned().into_iter().collect(),
                evidence_refs: candidate_evidence.clone(),
                risk_level: LearningRiskLevel::Medium,
                review_required: true,
                review_reason: Some(
                    "Procedure deprecation changes future prompt injection and requires review."
                        .to_string(),
                ),
                review_policy: json!({
                    "phase": "phase_13_procedure_feedback",
                    "requires_review": true,
                    "auto_apply": false,
                    "source": "top_level_procedure_feedback_deprecation_recommended"
                }),
                promotion_target: Some("learning_procedure".to_string()),
                promotion_policy: json!({
                    "phase": "phase_13_procedure_feedback",
                    "bridge": "learning_procedure_bridge",
                    "eligible_for_auto_promotion": false,
                    "reason": "Deprecation candidates are routed for review and only applied after approval."
                }),
            },
        )?;
        let event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_procedure_deprecation_candidate_created".to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Procedure feedback created deprecation candidate `{}` for procedure `{}`.",
                    candidate.id, procedure.id
                ),
                evidence_refs: candidate_evidence,
                payload: json!({
                    "candidate_id": &candidate.id,
                    "procedure_id": &procedure.id,
                    "feedback_event_id": feedback_event_id,
                    "feedback_verdict": judgement.verdict.as_str(),
                    "deprecation_recommended": true,
                }),
            },
        )?;

        LearningProcedureBridge::new(self.workspace_layout.clone())
            .route_candidate(store, scope, &candidate)?;
        Ok(Some(event.id))
    }
}

fn retrieval_event_matches_episode(event: &LearningEvent, episode: &V3EpisodeRecord) -> bool {
    if let Some(execution_id) = episode.execution_id.as_deref() {
        return event.execution_id.as_deref() == Some(execution_id);
    }
    if let Some(chat_session_id) = chat_session_id(episode).as_deref() {
        return event.chat_session_id.as_deref() == Some(chat_session_id);
    }
    if let Some(task_id) = episode.task_id.as_deref() {
        return event.task_id.as_deref() == Some(task_id);
    }
    false
}

fn usage_records_from_event(event: &LearningEvent) -> Vec<LearningProcedureUsageRecord> {
    let Some(selected) = event.payload.get("selected").and_then(Value::as_array) else {
        return Vec::new();
    };
    selected
        .iter()
        .filter_map(|item| {
            let procedure_id = item
                .get("procedure_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            Some(LearningProcedureUsageRecord {
                procedure_id: procedure_id.to_string(),
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                owner_agent: item
                    .get("owner_agent")
                    .and_then(Value::as_str)
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                score: item.get("score").and_then(Value::as_i64),
                reason: item
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                success_count_at_retrieval: item.get("success_count").and_then(Value::as_u64),
                failure_count_at_retrieval: item.get("failure_count").and_then(Value::as_u64),
                retrieval_event_id: event.id.clone(),
                retrieved_at: event.created_at.to_rfc3339(),
                task_id: event.task_id.clone(),
                execution_id: event.execution_id.clone(),
                chat_session_id: event.chat_session_id.clone(),
            })
        })
        .collect()
}

fn infer_episode_outcome(episode: &V3EpisodeRecord) -> ProcedureRunOutcome {
    let outcome = normalized_text(&episode.outcome_kind);
    let status = episode
        .execution_status
        .as_deref()
        .map(normalized_text)
        .unwrap_or_default();
    let summary = normalized_text(&episode.outcome_summary);

    let has_terminal_failure = episode.outcome_is_failed()
        || failure_outcome_token(outcome.as_str())
        || failure_status_token(status.as_str())
        || failure_summary_phrase(summary.as_str());
    if has_terminal_failure {
        return ProcedureRunOutcome::Failure;
    }

    let has_success = episode.outcome_is_succeeded()
        || success_outcome_token(outcome.as_str())
        || success_status_token(status.as_str())
        || success_summary_phrase(summary.as_str());
    if has_success {
        ProcedureRunOutcome::Success
    } else if episode.last_error.is_some() || episode.failure_count.unwrap_or_default() > 0 {
        ProcedureRunOutcome::Failure
    } else {
        ProcedureRunOutcome::Neutral
    }
}

fn success_outcome_token(value: &str) -> bool {
    matches!(
        value,
        "goal achieved" | "partial progress" | "success" | "succeeded" | "completed"
    )
}

fn success_status_token(value: &str) -> bool {
    matches!(value, "completed" | "success" | "succeeded")
}

fn success_summary_phrase(value: &str) -> bool {
    if negated_completion_phrase(value) {
        return false;
    }
    [
        "completed successfully",
        "execution completed successfully",
        "goal reached",
        "goal achieved",
        "succeeded",
    ]
    .iter()
    .any(|needle| value.contains(needle))
}

fn failure_outcome_token(value: &str) -> bool {
    matches!(
        value,
        "failed"
            | "failure"
            | "budget exhausted"
            | "circuit open"
            | "cannot proceed"
            | "loop detected"
            | "cancelled"
            | "canceled"
            | "user intervened"
            | "not completed"
            | "not achieved"
            | "goal not achieved"
            | "incomplete"
            | "unsuccessful"
    )
}

fn failure_status_token(value: &str) -> bool {
    matches!(
        value,
        "failed"
            | "failure"
            | "runtime error"
            | "budget exhausted"
            | "max iterations reached"
            | "cannot proceed"
            | "loop detected"
            | "cancelled"
            | "canceled"
            | "not completed"
            | "incomplete"
            | "unsuccessful"
    )
}

fn failure_summary_phrase(value: &str) -> bool {
    [
        "failed",
        "could not",
        "cannot proceed",
        "not completed",
        "not achieved",
        "goal not achieved",
        "did not succeed",
        "not succeeded",
        "unsuccessful",
        "incomplete",
        "cancelled",
        "canceled",
        "blocked",
        "error",
    ]
    .iter()
    .any(|needle| value.contains(needle))
}

fn negated_completion_phrase(value: &str) -> bool {
    [
        "not completed",
        "not complete",
        "not achieved",
        "goal not achieved",
        "did not succeed",
        "not succeeded",
        "unsuccessful",
        "incomplete",
    ]
    .iter()
    .any(|needle| value.contains(needle))
}

fn classify_procedure_outcome(
    run_outcome: ProcedureRunOutcome,
    explicit_negative: bool,
    judgement: Option<&LearningProcedureRunFeedbackJudgement>,
) -> ProcedureRunOutcome {
    if explicit_negative {
        return ProcedureRunOutcome::ExplicitNegativeCorrection;
    }
    let Some(judgement) = judgement else {
        return ProcedureRunOutcome::Neutral;
    };
    if judgement.verdict.is_negative() {
        return ProcedureRunOutcome::Failure;
    }
    if judgement.verdict.is_positive() && run_outcome == ProcedureRunOutcome::Success {
        return ProcedureRunOutcome::Success;
    }
    ProcedureRunOutcome::Neutral
}

fn apply_usage_feedback(
    procedure: &mut LearningProcedure,
    usage: &LearningProcedureUsageRecord,
    episode: &V3EpisodeRecord,
    procedure_outcome: ProcedureRunOutcome,
    judgement: Option<&LearningProcedureRunFeedbackJudgement>,
    reflection_event: Option<&LearningEventRef>,
) {
    let now = Utc::now();
    procedure.last_used_at = Some(now);
    if procedure_outcome.count_success() {
        procedure.success_count = procedure.success_count.saturating_add(1);
    }
    if procedure_outcome.count_failure() {
        procedure.failure_count = procedure.failure_count.saturating_add(1);
    }
    append_unique_evidence_refs(
        &mut procedure.evidence_refs,
        &feedback_evidence_refs(episode, usage, reflection_event),
    );
    if let Some(task_id) = episode.task_id.as_deref() {
        append_unique_string(&mut procedure.source_task_ids, task_id);
    }
    if let Some(session_id) = chat_session_id(episode).as_deref() {
        append_unique_string(&mut procedure.source_chat_session_ids, session_id);
    }
    append_usage_payload(
        procedure,
        usage,
        episode,
        procedure_outcome,
        judgement,
        reflection_event,
        now,
    );
}

fn append_usage_payload(
    procedure: &mut LearningProcedure,
    usage: &LearningProcedureUsageRecord,
    episode: &V3EpisodeRecord,
    procedure_outcome: ProcedureRunOutcome,
    judgement: Option<&LearningProcedureRunFeedbackJudgement>,
    reflection_event: Option<&LearningEventRef>,
    used_at: chrono::DateTime<Utc>,
) {
    if !procedure.payload.is_object() {
        let previous = std::mem::take(&mut procedure.payload);
        procedure.payload = json!({ "previous_payload": previous });
    }
    let Some(root) = procedure.payload.as_object_mut() else {
        return;
    };
    root.entry("phase_13_feedback".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(items) = root
        .get_mut("phase_13_feedback")
        .and_then(Value::as_array_mut)
    {
        items.push(json!({
            "used_at": used_at.to_rfc3339(),
            "episode_id": &episode.episode_id,
            "task_id": &episode.task_id,
            "execution_id": &episode.execution_id,
            "chat_session_id": chat_session_id(episode),
            "retrieval_event_id": &usage.retrieval_event_id,
            "retrieval_reason": &usage.reason,
            "outcome": procedure_outcome.as_str(),
            "procedure_judgement": judgement.map(procedure_judgement_payload),
            "reflection_event": reflection_event,
        }));
        if items.len() > 50 {
            let excess = items.len().saturating_sub(50);
            items.drain(0..excess);
        }
    }
}

fn procedure_judgement_payload(judgement: &LearningProcedureRunFeedbackJudgement) -> Value {
    json!({
        "procedure_id": &judgement.procedure_id,
        "verdict": judgement.verdict.as_str(),
        "rationale": &judgement.rationale,
        "confidence": judgement.confidence,
        "deprecation_recommended": judgement.deprecation_recommended,
    })
}

fn feedback_evidence_refs(
    episode: &V3EpisodeRecord,
    usage: &LearningProcedureUsageRecord,
    reflection_event: Option<&LearningEventRef>,
) -> Vec<LearningEvidenceRef> {
    let mut refs = vec![
        LearningEvidenceRef {
            kind: "learning_procedure_retrieval".to_string(),
            id: Some(usage.retrieval_event_id.clone()),
            path: None,
            uri: None,
            summary: usage.reason.clone().or_else(|| usage.title.clone()),
        },
        LearningEvidenceRef {
            kind: "memory_episode".to_string(),
            id: Some(episode.episode_id.clone()),
            path: None,
            uri: None,
            summary: Some(truncate_text(&episode.outcome_summary, 1_000)),
        },
    ];
    if let Some(task_id) = episode.task_id.as_deref() {
        refs.push(LearningEvidenceRef {
            kind: "task".to_string(),
            id: Some(task_id.to_string()),
            path: episode
                .provenance
                .as_ref()
                .map(|provenance| provenance.task_manifest_relative_path.clone()),
            uri: None,
            summary: episode.task_title.clone(),
        });
    }
    if let Some(execution_id) = episode.execution_id.as_deref() {
        refs.push(LearningEvidenceRef {
            kind: "execution".to_string(),
            id: Some(execution_id.to_string()),
            path: episode
                .provenance
                .as_ref()
                .map(|provenance| provenance.execution_state_relative_path.clone()),
            uri: None,
            summary: episode.execution_status.clone(),
        });
    }
    if let Some(session_id) = chat_session_id(episode) {
        refs.push(LearningEvidenceRef {
            kind: "chat_session".to_string(),
            id: Some(session_id),
            path: None,
            uri: None,
            summary: episode.ui_thread_id.clone(),
        });
    }
    if let Some(reflection_event) = reflection_event {
        refs.push(LearningEvidenceRef {
            kind: "learning_reflection".to_string(),
            id: Some(reflection_event.event_id.clone()),
            path: None,
            uri: None,
            summary: Some(reflection_event.event_type.clone()),
        });
    }
    refs
}

fn feedback_reason(
    usage: &LearningProcedureUsageRecord,
    outcome: ProcedureRunOutcome,
    no_op_reason: Option<&str>,
) -> String {
    let mut reason = format!(
        "Procedure was retrieved for a run and the post-run outcome was `{}`.",
        outcome.as_str()
    );
    if let Some(retrieval_reason) = usage.reason.as_deref() {
        reason.push_str(" Retrieval reason: ");
        reason.push_str(retrieval_reason);
        reason.push('.');
    }
    if let Some(no_op_reason) = no_op_reason {
        let no_op_reason = no_op_reason.trim();
        if !no_op_reason.is_empty() {
            reason.push_str(" Reflection no-op reason: ");
            reason.push_str(&truncate_text(no_op_reason, 500));
        }
    }
    reason
}

fn should_deprecate_after_feedback(
    procedure: &LearningProcedure,
    outcome: ProcedureRunOutcome,
) -> bool {
    if !matches!(
        procedure.status,
        LearningProcedureStatus::Active | LearningProcedureStatus::Draft
    ) {
        return false;
    }
    if outcome == ProcedureRunOutcome::ExplicitNegativeCorrection {
        return true;
    }
    outcome == ProcedureRunOutcome::Failure
        && procedure.failure_count >= 2
        && procedure.failure_count > procedure.success_count
}

fn judgement_requests_reviewed_deprecation(
    judgement: &LearningProcedureRunFeedbackJudgement,
) -> bool {
    judgement.deprecation_recommended
        && matches!(
            judgement.verdict,
            LearningProcedureRunFeedbackVerdict::Harmful
                | LearningProcedureRunFeedbackVerdict::Stale
                | LearningProcedureRunFeedbackVerdict::Misleading
                | LearningProcedureRunFeedbackVerdict::TooBroad
                | LearningProcedureRunFeedbackVerdict::TooNarrow
        )
}

fn deprecation_reason(procedure: &LearningProcedure, outcome: ProcedureRunOutcome) -> String {
    match outcome {
        ProcedureRunOutcome::ExplicitNegativeCorrection => format!(
            "A user correction or explicit negative feedback targeted procedure `{}`.",
            procedure.id
        ),
        _ => format!(
            "Procedure `{}` accumulated more failed uses ({}) than successful uses ({}) after retrieval.",
            procedure.id, procedure.failure_count, procedure.success_count
        ),
    }
}

fn has_explicit_negative_procedure_feedback(
    episode: &V3EpisodeRecord,
    extra_context: &Value,
    usage: &LearningProcedureUsageRecord,
) -> bool {
    value_has_targeted_negative_feedback(extra_context, usage)
        || episode
            .trigger_payload
            .as_ref()
            .is_some_and(|payload| value_has_targeted_negative_feedback(payload, usage))
}

fn value_has_targeted_negative_feedback(
    value: &Value,
    usage: &LearningProcedureUsageRecord,
) -> bool {
    match value {
        Value::Object(map) => {
            if object_has_negative_signal(map) && object_targets_procedure(map, usage) {
                return true;
            }
            map.iter()
                .filter(|(key, _)| !is_generated_procedure_context_key(key))
                .any(|(_, value)| value_has_targeted_negative_feedback(value, usage))
        },
        Value::Array(items) => items
            .iter()
            .any(|value| value_has_targeted_negative_feedback(value, usage)),
        Value::String(text) => targeted_negative_text(text, usage),
        _ => false,
    }
}

fn is_generated_procedure_context_key(key: &str) -> bool {
    matches!(
        key,
        "procedure_feedback" | "used_procedures" | "selected" | "procedure_judgement"
    )
}

fn object_has_negative_signal(map: &serde_json::Map<String, Value>) -> bool {
    for key in [
        "teaching_action",
        "action",
        "feedback_action",
        "verdict",
        "recommendation",
        "correction",
        "reason",
        "summary",
        "text",
        "message",
    ] {
        if map
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(contains_negative_procedure_signal)
        {
            return true;
        }
    }
    false
}

fn object_targets_procedure(
    map: &serde_json::Map<String, Value>,
    usage: &LearningProcedureUsageRecord,
) -> bool {
    for key in [
        "procedure_id",
        "target_procedure_id",
        "existing_procedure_id",
        "target_id",
    ] {
        if map
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| procedure_identifier_equals(value, usage))
        {
            return true;
        }
    }

    for key in ["target", "learning_target", "procedure"] {
        if let Some(Value::Object(target)) = map.get(key) {
            let target_kind = target
                .get("kind")
                .or_else(|| target.get("type"))
                .and_then(Value::as_str)
                .map(normalized_text)
                .unwrap_or_default();
            let kind_is_procedure = target_kind.is_empty() || target_kind.contains("procedure");
            if kind_is_procedure
                && ["id", "procedure_id", "name", "title"].iter().any(|key| {
                    target
                        .get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|value| {
                            nested_target_procedure_field_matches(key, value, usage)
                        })
                })
            {
                return true;
            }
        }
    }

    false
}

fn nested_target_procedure_field_matches(
    field: &str,
    value: &str,
    usage: &LearningProcedureUsageRecord,
) -> bool {
    match field {
        "id" | "procedure_id" => procedure_identifier_equals(value, usage),
        "name" | "title" => {
            procedure_identifier_equals(value, usage) || procedure_title_equals(value, usage)
        },
        _ => false,
    }
}

fn targeted_negative_text(text: &str, usage: &LearningProcedureUsageRecord) -> bool {
    contains_negative_procedure_signal(text)
        && (procedure_identifier_matches(text, usage) || procedure_title_matches(text, usage))
}

fn contains_negative_procedure_signal(text: &str) -> bool {
    let text = normalized_text(text);
    [
        "this was wrong",
        "never do this",
        "do not use",
        "wrong procedure",
        "bad procedure",
        "stale procedure",
        "procedure was wrong",
        "procedure is wrong",
        "procedure is stale",
        "misleading procedure",
        "explicit user correction",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn procedure_identifier_matches(text: &str, usage: &LearningProcedureUsageRecord) -> bool {
    normalized_text(text) == normalized_text(&usage.procedure_id)
        || normalized_text(text).contains(&normalized_text(&usage.procedure_id))
}

fn procedure_identifier_equals(text: &str, usage: &LearningProcedureUsageRecord) -> bool {
    normalized_text(text) == normalized_text(&usage.procedure_id)
}

fn procedure_title_matches(text: &str, usage: &LearningProcedureUsageRecord) -> bool {
    let Some(title) = usage.title.as_deref() else {
        return false;
    };
    let title = normalized_text(title);
    !title.is_empty() && normalized_text(text).contains(&title)
}

fn procedure_title_equals(text: &str, usage: &LearningProcedureUsageRecord) -> bool {
    let Some(title) = usage.title.as_deref() else {
        return false;
    };
    let title = normalized_text(title);
    !title.is_empty() && normalized_text(text) == title
}

fn append_unique_string(target: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() && !target.iter().any(|existing| existing == value) {
        target.push(value.to_string());
    }
}

fn append_unique_evidence_refs(
    target: &mut Vec<LearningEvidenceRef>,
    additions: &[LearningEvidenceRef],
) {
    for addition in additions {
        if !target.contains(addition) {
            target.push(addition.clone());
        }
    }
}

fn chat_session_id(episode: &V3EpisodeRecord) -> Option<String> {
    episode
        .trigger_payload
        .as_ref()
        .and_then(|payload| payload.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn normalized_text(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value.chars().take(max_chars).collect::<String>();
    out.push_str("...[truncated]");
    out
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::learning::{
        CreateLearningProcedureRequest, LearningCandidateFilters, LearningProcedureActivation,
    };

    #[test]
    fn generated_procedure_context_does_not_create_explicit_negative_feedback() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        seed_active_procedure(&store, &scope, "proc_browser", 1, 0);
        let bridge = LearningProcedureFeedbackBridge::new(workspace);
        let episode = episode("goal_achieved", Some("completed"), Some(1), None);
        let usage_context = usage_context("proc_browser", "Browser procedure");
        let generated_context = json!({
            "procedure_feedback": usage_context.as_prompt_json(),
            "note": "this was wrong"
        });

        bridge
            .record_post_run_feedback(
                &store,
                &scope,
                &episode,
                &usage_context,
                &[],
                None,
                None,
                &generated_context,
            )
            .expect("record feedback");

        let procedure = store
            .read_procedure(&scope, "proc_browser")
            .expect("read procedure");
        assert_eq!(procedure.status, LearningProcedureStatus::Active);
        assert_eq!(procedure.success_count, 1);
        assert_eq!(procedure.failure_count, 0);
        assert!(procedure.last_used_at.is_some());
    }

    #[test]
    fn useful_procedure_on_successful_retry_counts_success_not_failure() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        seed_active_procedure(&store, &scope, "proc_retry", 0, 0);
        let bridge = LearningProcedureFeedbackBridge::new(workspace);
        let episode = episode(
            "goal_achieved",
            Some("completed"),
            Some(3),
            Some("earlier attempt failed before recovery"),
        );
        let usage_context = usage_context("proc_retry", "Retry-safe procedure");
        let judgement = LearningProcedureRunFeedbackJudgement {
            procedure_id: "proc_retry".to_string(),
            verdict: LearningProcedureRunFeedbackVerdict::Useful,
            rationale: "The procedure guided the successful recovery.".to_string(),
            confidence: Some(0.9),
            deprecation_recommended: false,
        };

        bridge
            .record_post_run_feedback(
                &store,
                &scope,
                &episode,
                &usage_context,
                &[judgement],
                None,
                None,
                &json!({}),
            )
            .expect("record feedback");

        let procedure = store
            .read_procedure(&scope, "proc_retry")
            .expect("read procedure");
        assert_eq!(procedure.success_count, 1);
        assert_eq!(procedure.failure_count, 0);
        assert_eq!(procedure.status, LearningProcedureStatus::Active);
    }

    #[test]
    fn useful_procedure_on_negated_completion_does_not_count_success() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        seed_active_procedure(&store, &scope, "proc_incomplete", 0, 0);
        let bridge = LearningProcedureFeedbackBridge::new(workspace);
        let mut episode = episode("goal_not_achieved", None, None, None);
        episode.outcome_summary =
            "The task was not completed because the final verification failed.".to_string();
        let usage_context = usage_context("proc_incomplete", "Incomplete flow procedure");
        let judgement = LearningProcedureRunFeedbackJudgement {
            procedure_id: "proc_incomplete".to_string(),
            verdict: LearningProcedureRunFeedbackVerdict::Useful,
            rationale: "The procedure was partially helpful but the run did not finish."
                .to_string(),
            confidence: Some(0.7),
            deprecation_recommended: false,
        };

        bridge
            .record_post_run_feedback(
                &store,
                &scope,
                &episode,
                &usage_context,
                &[judgement],
                None,
                None,
                &json!({}),
            )
            .expect("record feedback");

        let procedure = store
            .read_procedure(&scope, "proc_incomplete")
            .expect("read procedure");
        assert_eq!(procedure.success_count, 0);
        assert_eq!(procedure.failure_count, 0);
        assert_eq!(procedure.status, LearningProcedureStatus::Active);
        assert!(procedure.last_used_at.is_some());
    }

    #[test]
    fn useful_procedure_with_failed_status_does_not_count_success() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        seed_active_procedure(&store, &scope, "proc_conflicting_status", 0, 0);
        let bridge = LearningProcedureFeedbackBridge::new(workspace);
        let episode = episode("goal_achieved", Some("not completed"), None, None);
        let usage_context =
            usage_context("proc_conflicting_status", "Conflicting status procedure");
        let judgement = LearningProcedureRunFeedbackJudgement {
            procedure_id: "proc_conflicting_status".to_string(),
            verdict: LearningProcedureRunFeedbackVerdict::Useful,
            rationale: "The procedure looked useful, but terminal status says the run failed."
                .to_string(),
            confidence: Some(0.7),
            deprecation_recommended: false,
        };

        bridge
            .record_post_run_feedback(
                &store,
                &scope,
                &episode,
                &usage_context,
                &[judgement],
                None,
                None,
                &json!({}),
            )
            .expect("record feedback");

        let procedure = store
            .read_procedure(&scope, "proc_conflicting_status")
            .expect("read procedure");
        assert_eq!(procedure.success_count, 0);
        assert_eq!(procedure.failure_count, 0);
        assert_eq!(procedure.status, LearningProcedureStatus::Active);
    }

    #[test]
    fn too_broad_feedback_counts_as_failure_pressure() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        seed_active_procedure(&store, &scope, "proc_too_broad", 0, 0);
        let bridge = LearningProcedureFeedbackBridge::new(workspace);
        let episode = episode("goal_achieved", Some("completed"), None, None);
        let usage_context = usage_context("proc_too_broad", "Too broad procedure");
        let judgement = LearningProcedureRunFeedbackJudgement {
            procedure_id: "proc_too_broad".to_string(),
            verdict: LearningProcedureRunFeedbackVerdict::TooBroad,
            rationale: "The procedure matched this task too broadly and misled selection."
                .to_string(),
            confidence: Some(0.8),
            deprecation_recommended: false,
        };

        bridge
            .record_post_run_feedback(
                &store,
                &scope,
                &episode,
                &usage_context,
                &[judgement],
                None,
                None,
                &json!({}),
            )
            .expect("record feedback");

        let procedure = store
            .read_procedure(&scope, "proc_too_broad")
            .expect("read procedure");
        assert_eq!(procedure.success_count, 0);
        assert_eq!(procedure.failure_count, 1);
        assert_eq!(procedure.status, LearningProcedureStatus::Active);
    }

    #[test]
    fn structured_negative_feedback_deprecates_targeted_procedure_only() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        seed_active_procedure(&store, &scope, "proc_bad", 0, 0);
        seed_active_procedure(&store, &scope, "proc_other", 0, 0);
        let bridge = LearningProcedureFeedbackBridge::new(workspace);
        let episode = episode("goal_achieved", Some("completed"), None, None);
        let usage_context = LearningProcedureUsageContext {
            used_procedures: vec![
                usage_record("proc_bad", "Bad procedure"),
                usage_record("proc_other", "Other procedure"),
            ],
            matched_event_ids: vec!["evt_retrieval".to_string()],
        };
        let explicit_context = json!({
            "teaching_action": "this_was_wrong",
            "target": {
                "kind": "procedure",
                "id": "proc_bad"
            }
        });

        bridge
            .record_post_run_feedback(
                &store,
                &scope,
                &episode,
                &usage_context,
                &[],
                None,
                None,
                &explicit_context,
            )
            .expect("record feedback");

        let bad = store
            .read_procedure(&scope, "proc_bad")
            .expect("read bad procedure");
        let other = store
            .read_procedure(&scope, "proc_other")
            .expect("read other procedure");
        assert_eq!(bad.status, LearningProcedureStatus::Deprecated);
        assert_eq!(bad.failure_count, 1);
        assert_eq!(other.status, LearningProcedureStatus::Active);
        assert_eq!(other.failure_count, 0);
    }

    #[test]
    fn structured_negative_feedback_uses_exact_id_matching() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        seed_active_procedure(&store, &scope, "proc_browser", 0, 0);
        seed_active_procedure(&store, &scope, "proc_browser_v2", 0, 0);
        let bridge = LearningProcedureFeedbackBridge::new(workspace);
        let episode = episode("goal_achieved", Some("completed"), None, None);
        let usage_context = LearningProcedureUsageContext {
            used_procedures: vec![
                usage_record("proc_browser", "Browser procedure"),
                usage_record("proc_browser_v2", "Browser procedure v2"),
            ],
            matched_event_ids: vec!["evt_retrieval".to_string()],
        };
        let explicit_context = json!({
            "teaching_action": "this_was_wrong",
            "target_id": "proc_browser_v2"
        });

        bridge
            .record_post_run_feedback(
                &store,
                &scope,
                &episode,
                &usage_context,
                &[],
                None,
                None,
                &explicit_context,
            )
            .expect("record feedback");

        let original = store
            .read_procedure(&scope, "proc_browser")
            .expect("read original procedure");
        let targeted = store
            .read_procedure(&scope, "proc_browser_v2")
            .expect("read targeted procedure");
        assert_eq!(original.status, LearningProcedureStatus::Active);
        assert_eq!(original.failure_count, 0);
        assert_eq!(targeted.status, LearningProcedureStatus::Deprecated);
        assert_eq!(targeted.failure_count, 1);
    }

    #[test]
    fn structured_negative_feedback_matches_nested_target_title() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        seed_active_procedure(&store, &scope, "proc_stale_title", 0, 0);
        seed_active_procedure(&store, &scope, "proc_other_title", 0, 0);
        let bridge = LearningProcedureFeedbackBridge::new(workspace);
        let episode = episode("goal_achieved", Some("completed"), None, None);
        let usage_context = LearningProcedureUsageContext {
            used_procedures: vec![
                usage_record("proc_stale_title", "Stale browser procedure"),
                usage_record("proc_other_title", "Other browser procedure"),
            ],
            matched_event_ids: vec!["evt_retrieval".to_string()],
        };
        let explicit_context = json!({
            "teaching_action": "this_was_wrong",
            "target": {
                "kind": "procedure",
                "title": "Stale browser procedure"
            }
        });

        bridge
            .record_post_run_feedback(
                &store,
                &scope,
                &episode,
                &usage_context,
                &[],
                None,
                None,
                &explicit_context,
            )
            .expect("record feedback");

        let stale = store
            .read_procedure(&scope, "proc_stale_title")
            .expect("read stale procedure");
        let other = store
            .read_procedure(&scope, "proc_other_title")
            .expect("read other procedure");
        assert_eq!(stale.status, LearningProcedureStatus::Deprecated);
        assert_eq!(stale.failure_count, 1);
        assert_eq!(other.status, LearningProcedureStatus::Active);
        assert_eq!(other.failure_count, 0);
    }

    #[test]
    fn deprecation_recommended_feedback_creates_review_candidate() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        seed_active_procedure(&store, &scope, "proc_stale_feedback", 0, 0);
        let bridge = LearningProcedureFeedbackBridge::new(workspace);
        let episode = episode("goal_achieved", Some("completed"), None, None);
        let usage_context = usage_context("proc_stale_feedback", "Stale feedback procedure");
        let judgement = LearningProcedureRunFeedbackJudgement {
            procedure_id: "proc_stale_feedback".to_string(),
            verdict: LearningProcedureRunFeedbackVerdict::Stale,
            rationale: "The procedure refers to a stale page flow.".to_string(),
            confidence: Some(0.82),
            deprecation_recommended: true,
        };

        bridge
            .record_post_run_feedback(
                &store,
                &scope,
                &episode,
                &usage_context,
                &[judgement],
                None,
                None,
                &json!({}),
            )
            .expect("record feedback");

        let procedure = store
            .read_procedure(&scope, "proc_stale_feedback")
            .expect("read procedure");
        assert_eq!(procedure.status, LearningProcedureStatus::Active);
        assert_eq!(procedure.failure_count, 1);

        let candidates = store
            .list_candidates(
                &scope,
                LearningCandidateFilters {
                    state: Some("triaged".to_string()),
                    candidate_type: Some("memory_procedure".to_string()),
                    source_agent_id: Some("agent-a".to_string()),
                    limit: None,
                },
            )
            .expect("list candidates");
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.proposed_target.as_deref() == Some("proc_stale_feedback"))
            .expect("deprecation candidate should be routed for review");
        assert_eq!(
            candidate.proposed_change["procedure"]["existing_procedure_id"],
            "proc_stale_feedback"
        );
        assert_eq!(
            candidate.proposed_change["procedure"]["deprecation_recommended"],
            true
        );
    }

    fn seed_active_procedure(
        store: &LearningStore,
        scope: &LearningScope,
        id: &str,
        success_count: u64,
        failure_count: u64,
    ) {
        store
            .create_procedure(
                scope.clone(),
                CreateLearningProcedureRequest {
                    principal: None,
                    workspace: None,
                    id: Some(id.to_string()),
                    actor: "test".to_string(),
                    reason: Some("seed".to_string()),
                    status: LearningProcedureStatus::Draft,
                    title: id.to_string(),
                    summary: "Seed procedure".to_string(),
                    owner_agent: Some("agent-a".to_string()),
                    activation: LearningProcedureActivation::default(),
                    workflow: vec!["Do the thing".to_string()],
                    decision_points: Vec::new(),
                    verification: Vec::new(),
                    failure_modes: Vec::new(),
                    evidence_refs: Vec::new(),
                    source_candidate_id: None,
                    source_task_ids: Vec::new(),
                    source_chat_session_ids: Vec::new(),
                    success_count,
                    failure_count,
                    payload: json!({}),
                },
            )
            .expect("create procedure");
        store
            .transition_procedure_status(
                scope,
                id,
                LearningProcedureStatus::Active,
                "test",
                "activate_seed",
                "activate seed procedure",
                Vec::new(),
            )
            .expect("activate procedure");
    }

    fn usage_context(procedure_id: &str, title: &str) -> LearningProcedureUsageContext {
        LearningProcedureUsageContext {
            used_procedures: vec![usage_record(procedure_id, title)],
            matched_event_ids: vec!["evt_retrieval".to_string()],
        }
    }

    fn usage_record(procedure_id: &str, title: &str) -> LearningProcedureUsageRecord {
        LearningProcedureUsageRecord {
            procedure_id: procedure_id.to_string(),
            title: Some(title.to_string()),
            owner_agent: Some("agent-a".to_string()),
            score: Some(12),
            reason: Some("matched test".to_string()),
            success_count_at_retrieval: Some(0),
            failure_count_at_retrieval: Some(0),
            retrieval_event_id: "evt_retrieval".to_string(),
            retrieved_at: "2026-05-15T00:00:00Z".to_string(),
            task_id: Some("task-a".to_string()),
            execution_id: Some("exec-a".to_string()),
            chat_session_id: None,
        }
    }

    fn episode(
        outcome_kind: &str,
        execution_status: Option<&str>,
        failure_count: Option<usize>,
        last_error: Option<&str>,
    ) -> V3EpisodeRecord {
        V3EpisodeRecord {
            schema_version: "v3_memory_episode_v1".to_string(),
            record_type: "episode".to_string(),
            principal: Some("principal".to_string()),
            workspace: Some("workspace".to_string()),
            agent_id: "agent-a".to_string(),
            episode_id: "episode-a".to_string(),
            goal_key: "goal-a".to_string(),
            consolidation_key: "consolidation-a".to_string(),
            trigger_type: "test".to_string(),
            trigger_seq: 1,
            trigger_timestamp: "2026-05-15T00:00:00Z".to_string(),
            trigger_payload: None,
            started_at: "2026-05-15T00:00:00Z".to_string(),
            completed_at: "2026-05-15T00:00:01Z".to_string(),
            outcome_kind: outcome_kind.to_string(),
            outcome_summary: "Completed after retries".to_string(),
            outcome_remaining: None,
            pending_actions: Vec::new(),
            failure_count,
            last_error: last_error.map(str::to_string),
            task_id: Some("task-a".to_string()),
            execution_id: Some("exec-a".to_string()),
            root_execution_id: None,
            parent_execution_id: None,
            relationship_type: None,
            ui_thread_id: None,
            task_title: Some("Task A".to_string()),
            task_description: None,
            execution_status: execution_status.map(str::to_string),
            outcome_type: None,
            execution_output_id: None,
            task_agent_output_id: None,
            task_user_output_id: None,
            source_output_ids: Vec::new(),
            actions_taken: Vec::new(),
            observations: Vec::new(),
            memory_updates: Vec::new(),
            memory_candidates: Vec::new(),
            strategy_summary: None,
            context_at_start: None,
            artifact_output: None,
            provenance: None,
            origin_surface: None,
            origin_meeting: None,
        }
    }
}
