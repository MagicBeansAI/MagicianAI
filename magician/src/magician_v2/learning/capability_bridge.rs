use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use tracing::warn;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::{
    CreateLearningEventRequest, LearningCandidate, LearningCandidateState,
    LearningCapabilityEvolutionBacklogFilters, LearningCapabilityEvolutionBacklogItem,
    LearningCapabilityEvolutionBacklogStatus, LearningEvidenceRef, LearningRiskLevel,
    LearningScope, LearningStore,
};

const LEARNING_CAPABILITY_BRIDGE_ACTOR: &str = "learning_capability_evolution_bridge";
const MAX_DEDUPED_BACKLOG_EVIDENCE_REFS: usize = 48;

#[derive(Debug, Clone, Serialize)]
pub struct LearningCapabilityEvolutionRouteOutcome {
    pub candidate_id: String,
    pub routed: bool,
    pub backlog_path: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct LearningCapabilityEvolutionBridge {
    workspace_layout: ArtifactV2Workspace,
}

#[derive(Debug, Clone)]
struct CapabilityBacklogSpec {
    capability_id: Option<String>,
    failure_pattern: Option<String>,
    proposed_fix_type: Option<String>,
    proposed_files: Vec<String>,
    required_eval: Option<Value>,
    promotion_gate: Option<Value>,
    fix_spec: Value,
}

#[derive(Debug, Clone)]
struct CapabilityBacklogRanking {
    recurrence_count: u64,
    blocked_task_count: u64,
    user_pain_signal_count: u64,
    validation_failure_count: u64,
    local_validation_available: bool,
    rank_score: f64,
    rank_reasons: Vec<String>,
    owner_hints: Vec<String>,
}

impl LearningCapabilityEvolutionBridge {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub fn route_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
    ) -> Result<LearningCapabilityEvolutionRouteOutcome> {
        if !candidate
            .candidate_type
            .is_skill_or_capability_evolution_candidate()
        {
            return Ok(LearningCapabilityEvolutionRouteOutcome {
                candidate_id: candidate.id.clone(),
                routed: false,
                backlog_path: None,
                reason: "not_a_skill_or_capability_evolution_candidate".to_string(),
            });
        }
        let stored_candidate = match store.read_candidate(scope, &candidate.id) {
            Ok(value) => Some(value),
            Err(error) if store.error_is_not_found(&error) => None,
            Err(error) => return Err(error),
        };
        let candidate_is_stored = stored_candidate.is_some();
        let candidate = stored_candidate.as_ref().unwrap_or(candidate);
        if candidate.state.is_terminal() {
            return Ok(LearningCapabilityEvolutionRouteOutcome {
                candidate_id: candidate.id.clone(),
                routed: false,
                backlog_path: None,
                reason: format!("candidate_state_is_terminal: {}", candidate.state.as_str()),
            });
        }

        let spec = CapabilityBacklogSpec::from_candidate(candidate);
        let now = Utc::now();
        let dedupe_fingerprint = capability_candidate_dedupe_fingerprint(candidate, &spec);
        let existing_for_candidate = store
            .read_capability_evolution_backlog_item(scope, &candidate.id)
            .ok();
        let existing = match existing_for_candidate {
            Some(item) => Some(item),
            None => find_open_backlog_item_by_fingerprint(store, scope, &dedupe_fingerprint)?,
        };
        let existing_item_updated = existing.is_some();
        let deduped_to_existing_candidate = existing
            .as_ref()
            .is_some_and(|item| item.candidate_id != candidate.id);
        let mut item = if let Some(mut existing) = existing {
            merge_capability_backlog_item(
                &mut existing,
                candidate,
                &spec,
                dedupe_fingerprint.clone(),
                deduped_to_existing_candidate,
                now,
            );
            existing
        } else {
            let ranking = CapabilityBacklogRanking::from_candidate(candidate, &spec, 1);
            LearningCapabilityEvolutionBacklogItem {
                id: format!(
                    "lceb_{}",
                    candidate.id.strip_prefix("lc_").unwrap_or(&candidate.id)
                ),
                scope: scope.clone(),
                candidate_id: candidate.id.clone(),
                status: LearningCapabilityEvolutionBacklogStatus::Queued,
                candidate_type: candidate.candidate_type.clone(),
                title: candidate.title.clone(),
                summary: candidate.summary.clone(),
                rationale: candidate.rationale.clone(),
                capability_id: spec.capability_id.clone(),
                failure_pattern: spec.failure_pattern.clone(),
                proposed_fix_type: spec.proposed_fix_type.clone(),
                proposed_files: normalized_unique_strings(spec.proposed_files.clone()),
                required_eval: spec.required_eval.clone(),
                promotion_gate: spec.promotion_gate.clone(),
                proposed_target: candidate.proposed_target.clone(),
                risk_level: candidate.risk_level.clone(),
                dedupe_fingerprint: Some(dedupe_fingerprint.clone()),
                recurrence_count: ranking.recurrence_count,
                blocked_task_count: ranking.blocked_task_count,
                user_pain_signal_count: ranking.user_pain_signal_count,
                validation_failure_count: ranking.validation_failure_count,
                local_validation_available: ranking.local_validation_available,
                rank_score: ranking.rank_score,
                rank_reasons: ranking.rank_reasons,
                owner_hints: ranking.owner_hints,
                supersedes_candidate_ids: Vec::new(),
                source_agent_id: candidate.source_agent_id.clone(),
                source_task_id: candidate.source_task_id.clone(),
                source_execution_id: candidate.source_execution_id.clone(),
                source_chat_session_id: candidate.source_chat_session_id.clone(),
                evidence_refs: candidate.evidence_refs.clone(),
                fix_spec: spec.fix_spec.clone(),
                created_at: now,
                updated_at: now,
            }
        };
        item.dedupe_fingerprint = Some(dedupe_fingerprint.clone());
        annotate_fix_spec_dedupe(&mut item, &dedupe_fingerprint);
        store.write_capability_evolution_backlog_item(&item)?;

        let backlog_path = self.workspace_layout.skill_evolution_backlog_path(
            &scope.principal,
            &scope.workspace,
            &item.candidate_id,
        );
        let route_event_type = if candidate.candidate_type.is_skill_or_workflow_candidate() {
            "learning_skill_candidate_routed"
        } else {
            "learning_capability_candidate_routed"
        };
        let route_label = if candidate.candidate_type.is_skill_or_workflow_candidate() {
            "skill/workflow"
        } else {
            "capability-evolution"
        };
        let event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: route_event_type.to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Learning {route_label} candidate `{}` routed to skill-evolution backlog.",
                    candidate.id,
                ),
                evidence_refs: candidate.evidence_refs.clone(),
                payload: json!({
                    "candidate_id": candidate.id,
                    "canonical_candidate_id": item.candidate_id,
                    "candidate_type": candidate.candidate_type.as_str(),
                    "backlog_id": item.id,
                    "backlog_path": backlog_path.display().to_string(),
                    "capability_id": item.capability_id,
                    "proposed_fix_type": item.proposed_fix_type,
                    "proposed_files": item.proposed_files,
                    "existing_item_updated": existing_item_updated,
                    "deduped_to_existing_candidate": deduped_to_existing_candidate,
                    "dedupe_fingerprint": item.dedupe_fingerprint,
                    "rank_score": item.rank_score,
                    "rank_reasons": item.rank_reasons,
                    "owner_hints": item.owner_hints,
                }),
            },
        )?;

        let route_reason = if deduped_to_existing_candidate {
            if candidate.candidate_type.is_skill_or_workflow_candidate() {
                "deduped_to_skill_evolution_backlog"
            } else {
                "deduped_to_capability_evolution_backlog"
            }
        } else if candidate.candidate_type.is_skill_or_workflow_candidate() {
            "routed_to_skill_evolution_backlog"
        } else {
            "routed_to_capability_evolution_backlog"
        };

        if candidate_is_stored && deduped_to_existing_candidate {
            let _ = store.transition_candidate(
                scope,
                &candidate.id,
                LearningCandidateState::Superseded,
                LEARNING_CAPABILITY_BRIDGE_ACTOR,
                route_reason,
                format!(
                    "{route_label} candidate was deduped into canonical backlog candidate `{}`; learning event {} records the merge.",
                    item.candidate_id, event.id
                ),
                candidate.evidence_refs.clone(),
            )?;
        } else if candidate_is_stored
            && matches!(
                candidate.state,
                LearningCandidateState::Observed | LearningCandidateState::Proposed
            )
        {
            let _ = store.transition_candidate(
                scope,
                &candidate.id,
                LearningCandidateState::Triaged,
                LEARNING_CAPABILITY_BRIDGE_ACTOR,
                route_reason,
                format!(
                    "{route_label} candidate was routed to the backlog; learning event {} records the item.",
                    event.id
                ),
                candidate.evidence_refs.clone(),
            )?;
        }

        Ok(LearningCapabilityEvolutionRouteOutcome {
            candidate_id: candidate.id.clone(),
            routed: true,
            backlog_path: Some(backlog_path.display().to_string()),
            reason: route_reason.to_string(),
        })
    }
}

impl CapabilityBacklogSpec {
    fn from_candidate(candidate: &LearningCandidate) -> Self {
        let payload = candidate_payload(candidate);
        let capability_id = read_string_any(
            &payload,
            &[
                "capability_id",
                "capability",
                "tool_name",
                "tool",
                "pack_name",
                "pack",
                "skill_id",
                "skill_name",
                "skill",
                "target_skill",
                "workflow_id",
                "workflow_name",
            ],
        )
        .or_else(|| candidate.proposed_target.clone());
        let failure_pattern = read_string_any(
            &payload,
            &[
                "failure_pattern",
                "failure_mode",
                "problem",
                "issue",
                "symptom",
                "workflow_pattern",
                "procedure_pattern",
                "reusable_pattern",
            ],
        );
        let proposed_fix_type = read_string_any(
            &payload,
            &[
                "proposed_fix_type",
                "fix_type",
                "change_type",
                "target_type",
                "target",
            ],
        )
        .map(|value| normalize_token(&value))
        .or_else(|| default_fix_type(candidate).map(str::to_string));
        let proposed_files = read_string_array_any(
            &payload,
            &[
                "proposed_files",
                "files",
                "target_files",
                "paths",
                "skill_files",
            ],
        );
        let required_eval = read_value_any(
            &payload,
            &["required_eval", "eval", "evaluation", "eval_plan"],
        );
        let promotion_gate = read_value_any(
            &payload,
            &["promotion_gate", "promotion_policy", "gate", "validation"],
        );
        let fix_spec = if payload.is_object() {
            payload
        } else {
            json!({ "value": payload })
        };

        Self {
            capability_id,
            failure_pattern,
            proposed_fix_type,
            proposed_files,
            required_eval,
            promotion_gate,
            fix_spec,
        }
    }
}

impl CapabilityBacklogRanking {
    fn from_candidate(
        candidate: &LearningCandidate,
        spec: &CapabilityBacklogSpec,
        recurrence_count: u64,
    ) -> Self {
        let payload = &spec.fix_spec;
        let blocked_task_count = read_u64_any(
            payload,
            &[
                "blocked_task_count",
                "blocked_tasks",
                "blocked_work_count",
                "task_blocked_count",
            ],
        )
        .unwrap_or_else(|| u64::from(read_bool_any(payload, &["blocked", "task_blocked"])));
        let user_pain_signal_count = read_u64_any(
            payload,
            &[
                "user_pain_signal_count",
                "user_pain_count",
                "negative_user_feedback_count",
            ],
        )
        .unwrap_or_default()
            + u64::from(has_user_pain_signal(candidate, payload));
        let validation_failure_count = read_u64_any(
            payload,
            &[
                "validation_failure_count",
                "validation_failures",
                "failed_validation_count",
            ],
        )
        .unwrap_or_else(|| u64::from(has_validation_failure_signal(candidate, payload)));
        let local_validation_available = has_local_validation_command(spec.required_eval.as_ref())
            || has_local_validation_command(spec.promotion_gate.as_ref())
            || has_local_validation_command(Some(payload));

        let mut rank_score = 0.0;
        let mut rank_reasons = Vec::new();
        if recurrence_count > 1 {
            let recurrence_score = ((recurrence_count - 1) as f64 * 9.0).min(36.0);
            rank_score += recurrence_score;
            rank_reasons.push(format!("recurred {recurrence_count} times"));
        }
        if user_pain_signal_count > 0 {
            rank_score += (user_pain_signal_count as f64 * 18.0).min(36.0);
            rank_reasons.push(format!("{} user-pain signal(s)", user_pain_signal_count));
        }
        if blocked_task_count > 0 {
            rank_score += (blocked_task_count as f64 * 14.0).min(28.0);
            rank_reasons.push(format!("{} blocked task signal(s)", blocked_task_count));
        }
        if validation_failure_count > 0 {
            rank_score += (validation_failure_count as f64 * 16.0).min(32.0);
            rank_reasons.push(format!(
                "{} validation failure signal(s)",
                validation_failure_count
            ));
        }
        match &candidate.risk_level {
            LearningRiskLevel::Low => {
                rank_score += 10.0;
                rank_reasons.push("low risk".to_string());
            },
            LearningRiskLevel::Medium => {
                rank_score += 5.0;
                rank_reasons.push("medium risk".to_string());
            },
            LearningRiskLevel::High => {
                rank_reasons.push("high risk".to_string());
            },
            LearningRiskLevel::Critical => {
                rank_score = (rank_score - 8.0).max(0.0);
                rank_reasons.push("critical risk".to_string());
            },
        }
        if let Some(confidence) = candidate.confidence {
            if confidence >= 0.85 {
                rank_score += 10.0;
                rank_reasons.push(format!("high confidence {:.0}%", confidence * 100.0));
            } else if confidence >= 0.70 {
                rank_score += 5.0;
                rank_reasons.push(format!("confidence {:.0}%", confidence * 100.0));
            }
        }
        if local_validation_available {
            rank_score += 10.0;
            rank_reasons.push("local validation available".to_string());
        }
        if !candidate.evidence_refs.is_empty() {
            rank_score += (candidate.evidence_refs.len() as f64).min(6.0);
        }

        let mut owner_hints = Vec::new();
        let source_code_change_needed = source_code_change_needed(spec);
        if source_code_change_needed {
            push_unique(&mut owner_hints, "source_code_change_needed");
        }
        if candidate.review_required
            || matches!(
                &candidate.risk_level,
                LearningRiskLevel::High | LearningRiskLevel::Critical
            )
            || source_code_change_needed
        {
            push_unique(&mut owner_hints, "operator_review_required");
        }
        if !matches!(&candidate.risk_level, LearningRiskLevel::Critical)
            && (local_validation_available || !spec.proposed_files.is_empty())
        {
            push_unique(&mut owner_hints, "autonomous_steward_can_draft_validate");
        }
        if source_code_change_needed
            || matches!(
                &candidate.risk_level,
                LearningRiskLevel::High | LearningRiskLevel::Critical
            )
        {
            push_unique(&mut owner_hints, "specialist_agent_recommended");
        }
        if owner_hints.is_empty() {
            push_unique(&mut owner_hints, "autonomous_steward_can_draft_validate");
        }

        Self {
            recurrence_count: recurrence_count.max(1),
            blocked_task_count,
            user_pain_signal_count,
            validation_failure_count,
            local_validation_available,
            rank_score,
            rank_reasons,
            owner_hints,
        }
    }
}

fn find_open_backlog_item_by_fingerprint(
    store: &LearningStore,
    scope: &LearningScope,
    dedupe_fingerprint: &str,
) -> Result<Option<LearningCapabilityEvolutionBacklogItem>> {
    let items = store.list_capability_evolution_backlog_items(
        scope,
        LearningCapabilityEvolutionBacklogFilters::default(),
    )?;
    Ok(items.into_iter().find(|item| {
        item.status.is_open_for_dedupe()
            && item.dedupe_fingerprint.as_deref() == Some(dedupe_fingerprint)
    }))
}

fn merge_capability_backlog_item(
    item: &mut LearningCapabilityEvolutionBacklogItem,
    candidate: &LearningCandidate,
    spec: &CapabilityBacklogSpec,
    dedupe_fingerprint: String,
    deduped_to_existing_candidate: bool,
    now: chrono::DateTime<Utc>,
) {
    let candidate_recurrence = recurrence_count_from_candidate(candidate, spec);
    let next_recurrence = if deduped_to_existing_candidate {
        item.recurrence_count
            .max(1)
            .saturating_add(1)
            .max(candidate_recurrence)
    } else {
        item.recurrence_count.max(candidate_recurrence).max(1)
    };
    let ranking = CapabilityBacklogRanking::from_candidate(candidate, spec, next_recurrence);

    if candidate.title.trim().len() > item.title.trim().len() {
        item.title = candidate.title.clone();
    }
    if candidate.summary.trim().len() > item.summary.trim().len() {
        item.summary = candidate.summary.clone();
    }
    if candidate.rationale.trim().len() > item.rationale.trim().len() {
        item.rationale = candidate.rationale.clone();
    }
    item.capability_id = item
        .capability_id
        .clone()
        .or_else(|| spec.capability_id.clone());
    item.failure_pattern = most_specific_string(&item.failure_pattern, &spec.failure_pattern);
    item.proposed_fix_type = item
        .proposed_fix_type
        .clone()
        .or_else(|| spec.proposed_fix_type.clone());
    item.proposed_files = merge_string_lists(
        &item.proposed_files,
        &normalized_unique_strings(spec.proposed_files.clone()),
    );
    item.required_eval = item
        .required_eval
        .clone()
        .or_else(|| spec.required_eval.clone());
    item.promotion_gate = item
        .promotion_gate
        .clone()
        .or_else(|| spec.promotion_gate.clone());
    item.proposed_target = item
        .proposed_target
        .clone()
        .or_else(|| candidate.proposed_target.clone());
    item.risk_level = higher_risk_level(&item.risk_level, &candidate.risk_level);
    item.dedupe_fingerprint = Some(dedupe_fingerprint.clone());
    item.recurrence_count = ranking.recurrence_count;
    item.blocked_task_count = item.blocked_task_count.max(ranking.blocked_task_count);
    item.user_pain_signal_count = item
        .user_pain_signal_count
        .max(ranking.user_pain_signal_count);
    item.validation_failure_count = item
        .validation_failure_count
        .max(ranking.validation_failure_count);
    item.local_validation_available =
        item.local_validation_available || ranking.local_validation_available;
    item.rank_score = item.rank_score.max(ranking.rank_score);
    item.rank_reasons = merge_string_lists(&item.rank_reasons, &ranking.rank_reasons);
    item.owner_hints = merge_string_lists(&item.owner_hints, &ranking.owner_hints);
    item.evidence_refs = merge_evidence_refs(&item.evidence_refs, &candidate.evidence_refs);
    if deduped_to_existing_candidate {
        item.supersedes_candidate_ids.push(candidate.id.clone());
        item.supersedes_candidate_ids =
            normalized_unique_strings(item.supersedes_candidate_ids.clone());
    }
    merge_missing_object_fields(&mut item.fix_spec, &spec.fix_spec);
    annotate_fix_spec_dedupe(item, &dedupe_fingerprint);
    item.updated_at = now;
}

fn capability_candidate_dedupe_fingerprint(
    candidate: &LearningCandidate,
    spec: &CapabilityBacklogSpec,
) -> String {
    let target = normalized_fingerprint_component(
        spec.capability_id
            .as_deref()
            .or(candidate.proposed_target.as_deref())
            .unwrap_or(&candidate.title),
    );
    let signal = normalized_fingerprint_component(
        read_string_any(
            &spec.fix_spec,
            &[
                "failure_class",
                "workflow_signature",
                "failure_pattern",
                "skill_invocation_cluster_key",
                "procedure_signature",
                "input_fingerprint",
                "workflow_pattern",
            ],
        )
        .as_deref()
        .or(spec.failure_pattern.as_deref())
        .unwrap_or(&candidate.summary),
    );
    let fix_type = normalized_fingerprint_component(
        spec.proposed_fix_type
            .as_deref()
            .unwrap_or(candidate.candidate_type.as_str()),
    );
    let files = normalized_unique_strings(spec.proposed_files.clone()).join(",");
    let evidence_kinds = normalized_unique_strings(
        candidate
            .evidence_refs
            .iter()
            .map(|evidence| evidence.kind.clone())
            .collect(),
    )
    .join(",");
    let raw = format!(
        "skill-evolution-v1\ntype={}\ntarget={target}\nsignal={signal}\nfiles={files}\nfix={fix_type}\nevidence={evidence_kinds}",
        candidate.candidate_type.as_str()
    );
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    format!("sev1:{:x}", hasher.finalize())
}

fn recurrence_count_from_candidate(
    candidate: &LearningCandidate,
    spec: &CapabilityBacklogSpec,
) -> u64 {
    read_u64_any(
        &spec.fix_spec,
        &[
            "occurrence_count",
            "recurrence_count",
            "repeat_count",
            "failure_count",
        ],
    )
    .unwrap_or_else(|| candidate.evidence_refs.len() as u64)
    .max(1)
}

fn candidate_payload(candidate: &LearningCandidate) -> Value {
    match &candidate.candidate_type {
        super::LearningCandidateType::SkillUpdate => candidate
            .proposed_change
            .get("skill_update")
            .or_else(|| candidate.proposed_change.get("skill"))
            .or_else(|| candidate.proposed_change.get("workflow"))
            .cloned()
            .unwrap_or_else(|| candidate.proposed_change.clone()),
        super::LearningCandidateType::WorkflowTemplate => candidate
            .proposed_change
            .get("workflow_template")
            .or_else(|| candidate.proposed_change.get("workflow"))
            .or_else(|| candidate.proposed_change.get("skill_update"))
            .cloned()
            .unwrap_or_else(|| candidate.proposed_change.clone()),
        _ => candidate
            .proposed_change
            .get("capability_evolution")
            .or_else(|| candidate.proposed_change.get("capability"))
            .or_else(|| candidate.proposed_change.get("tool"))
            .cloned()
            .unwrap_or_else(|| candidate.proposed_change.clone()),
    }
}

fn default_fix_type(candidate: &LearningCandidate) -> Option<&'static str> {
    match &candidate.candidate_type {
        super::LearningCandidateType::SkillUpdate => Some("skill_guide"),
        super::LearningCandidateType::WorkflowTemplate => Some("workflow_template"),
        _ => None,
    }
}

fn read_string_any(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(|entry| match entry {
            Value::String(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        })
}

fn read_string_array_any(value: &Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .map(read_string_array)
        .unwrap_or_default()
}

fn read_string_array(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.trim().to_string()),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect(),
        Value::String(text) => text
            .split(',')
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn read_value_any(value: &Value, keys: &[&str]) -> Option<Value> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(|entry| {
            if entry.is_null() {
                None
            } else {
                Some(entry.clone())
            }
        })
}

fn read_u64_any(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(value_to_u64)
}

fn value_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64().or_else(|| {
            number
                .as_f64()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(|value| value.floor() as u64)
        }),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        Value::Array(items) => Some(items.len() as u64),
        Value::Bool(flag) => Some(u64::from(*flag)),
        _ => None,
    }
}

fn read_bool_any(value: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .any(value_to_bool)
}

fn value_to_bool(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_u64().is_some_and(|value| value > 0),
        Value::String(text) => matches!(
            normalize_token(text).as_str(),
            "true" | "yes" | "y" | "1" | "blocked" | "failed" | "failure"
        ),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
        Value::Null => false,
    }
}

fn has_user_pain_signal(candidate: &LearningCandidate, payload: &Value) -> bool {
    let mut haystack = format!(
        "{} {} {}",
        candidate.title, candidate.summary, candidate.rationale
    )
    .to_ascii_lowercase();
    if let Some(source) =
        read_string_any(payload, &["source", "feedback_source", "teaching_action"])
    {
        haystack.push(' ');
        haystack.push_str(&source.to_ascii_lowercase());
    }
    candidate.evidence_refs.iter().any(|evidence| {
        matches!(
            normalize_token(&evidence.kind).as_str(),
            "teaching_feedback" | "user_teaching" | "user_feedback" | "chat_correction"
        ) || evidence
            .summary
            .as_deref()
            .is_some_and(|summary| user_pain_text(summary))
    }) || user_pain_text(&haystack)
        || read_bool_any(
            payload,
            &[
                "user_pain",
                "owner_reported",
                "user_reported",
                "this_was_wrong",
                "negative_feedback",
            ],
        )
}

fn user_pain_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "this was wrong",
        "wrong answer",
        "failed for user",
        "user reported",
        "owner reported",
        "blocked user",
        "improve_tool",
        "this_was_wrong",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn has_validation_failure_signal(candidate: &LearningCandidate, payload: &Value) -> bool {
    candidate.evidence_refs.iter().any(|evidence| {
        normalize_token(&evidence.kind).contains("validation")
            && evidence
                .summary
                .as_deref()
                .is_some_and(|summary| summary.to_ascii_lowercase().contains("fail"))
    }) || read_bool_any(
        payload,
        &["validation_failed", "eval_failed", "regression_failed"],
    )
}

fn has_local_validation_command(value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return false;
    };
    match value {
        Value::Array(items) => items
            .iter()
            .any(|item| has_local_validation_command(Some(item))),
        Value::Object(map) => map.iter().any(|(key, value)| {
            let key = normalize_token(key);
            let command_key = matches!(
                key.as_str(),
                "command"
                    | "commands"
                    | "validation_command"
                    | "validation_commands"
                    | "regression_command"
                    | "regression_commands"
                    | "smoke_command"
                    | "smoke_commands"
            );
            command_key && value_to_bool(value) || has_local_validation_command(Some(value))
        }),
        Value::String(text) => !text.trim().is_empty() && looks_like_local_command(text),
        _ => false,
    }
}

fn looks_like_local_command(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("cargo ")
        || trimmed.starts_with("make ")
        || trimmed.starts_with("npm ")
        || trimmed.starts_with("pnpm ")
        || trimmed.starts_with("yarn ")
        || trimmed.starts_with("python ")
        || trimmed.starts_with("python3 ")
        || trimmed.starts_with("./")
        || trimmed.starts_with("scripts/")
}

fn source_code_change_needed(spec: &CapabilityBacklogSpec) -> bool {
    let fix_type = spec
        .proposed_fix_type
        .as_deref()
        .map(normalize_token)
        .unwrap_or_default();
    fix_type.contains("wrapper")
        || fix_type.contains("source")
        || fix_type.contains("code")
        || spec.proposed_files.iter().any(|path| {
            let lower = path.to_ascii_lowercase();
            !(lower.starts_with("skills/") || lower.ends_with("/skill.md"))
                && (lower.starts_with("skillshub/")
                    || lower.starts_with("magician/")
                    || lower.starts_with("ui/")
                    || lower.contains("/scripts/")
                    || lower.ends_with(".rs")
                    || lower.ends_with(".py")
                    || lower.ends_with(".js")
                    || lower.ends_with(".ts")
                    || lower.ends_with(".svelte"))
        })
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn normalized_unique_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .filter(|value| seen.insert(normalize_token(value)))
        .collect::<Vec<_>>();
    normalized.sort_by_key(|value| normalize_token(value));
    normalized
}

fn merge_string_lists(left: &[String], right: &[String]) -> Vec<String> {
    normalized_unique_strings(left.iter().chain(right.iter()).cloned().collect())
}

fn most_specific_string(left: &Option<String>, right: &Option<String>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) if right.trim().len() > left.trim().len() => Some(right.clone()),
        (Some(left), _) => Some(left.clone()),
        (None, Some(right)) => Some(right.clone()),
        (None, None) => None,
    }
}

fn higher_risk_level(left: &LearningRiskLevel, right: &LearningRiskLevel) -> LearningRiskLevel {
    if risk_level_rank(right) > risk_level_rank(left) {
        right.clone()
    } else {
        left.clone()
    }
}

fn risk_level_rank(value: &LearningRiskLevel) -> u8 {
    match value {
        LearningRiskLevel::Low => 0,
        LearningRiskLevel::Medium => 1,
        LearningRiskLevel::High => 2,
        LearningRiskLevel::Critical => 3,
    }
}

fn merge_evidence_refs(
    left: &[LearningEvidenceRef],
    right: &[LearningEvidenceRef],
) -> Vec<LearningEvidenceRef> {
    let mut seen = HashSet::new();
    left.iter()
        .chain(right.iter())
        .filter(|reference| seen.insert(evidence_ref_key(reference)))
        .take(MAX_DEDUPED_BACKLOG_EVIDENCE_REFS)
        .cloned()
        .collect()
}

fn evidence_ref_key(reference: &LearningEvidenceRef) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        normalize_token(&reference.kind),
        reference.id.as_deref().unwrap_or_default(),
        reference.path.as_deref().unwrap_or_default(),
        reference.uri.as_deref().unwrap_or_default(),
        reference.summary.as_deref().unwrap_or_default()
    )
}

fn merge_missing_object_fields(target: &mut Value, incoming: &Value) {
    if !target.is_object() {
        *target = json!({ "previous_fix_spec": target.clone() });
    }
    let Some(target_object) = target.as_object_mut() else {
        return;
    };
    let Some(incoming_object) = incoming.as_object() else {
        return;
    };
    for (key, value) in incoming_object {
        target_object
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
}

fn annotate_fix_spec_dedupe(
    item: &mut LearningCapabilityEvolutionBacklogItem,
    dedupe_fingerprint: &str,
) {
    if !item.fix_spec.is_object() {
        item.fix_spec = json!({ "previous_fix_spec": item.fix_spec.clone() });
    }
    if let Some(object) = item.fix_spec.as_object_mut() {
        object.insert(
            "dedupe_fingerprint".to_string(),
            Value::String(dedupe_fingerprint.to_string()),
        );
        object.insert(
            "recurrence_count".to_string(),
            Value::Number(serde_json::Number::from(item.recurrence_count)),
        );
        object.insert(
            "rank_score".to_string(),
            serde_json::Number::from_f64(item.rank_score)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
        if !item.supersedes_candidate_ids.is_empty() {
            object.insert(
                "supersedes_candidate_ids".to_string(),
                Value::Array(
                    item.supersedes_candidate_ids
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
    }
}

fn normalized_fingerprint_component(value: &str) -> String {
    normalize_token(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

pub fn log_capability_route_error(candidate_id: &str, error: &anyhow::Error) {
    warn!(
        candidate_id = %candidate_id,
        error = %error,
        "Learning capability-evolution candidate routing failed"
    );
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use crate::magician_v2::learning::{
        CreateLearningCandidateRequest, LearningCandidateState, LearningCandidateType,
        LearningEvidenceRef, LearningRiskLevel,
    };

    use super::*;

    #[test]
    fn route_capability_candidate_writes_backlog_and_triages_source_candidate() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test-principal", "test-workspace");
        let candidate = store
            .create_candidate(scope.clone(), capability_candidate_request())
            .expect("create candidate");

        let outcome = LearningCapabilityEvolutionBridge::new(workspace)
            .route_candidate(&store, &scope, &candidate)
            .expect("route capability candidate");

        assert!(outcome.routed);
        assert_eq!(outcome.candidate_id, candidate.id);
        assert_eq!(outcome.reason, "routed_to_capability_evolution_backlog");
        assert!(outcome
            .backlog_path
            .as_deref()
            .expect("backlog path")
            .ends_with(&format!("{}.json", candidate.id)));

        let item = store
            .read_capability_evolution_backlog_item(&scope, &candidate.id)
            .expect("read capability backlog item");
        assert_eq!(item.candidate_id, candidate.id);
        assert_eq!(
            item.status,
            LearningCapabilityEvolutionBacklogStatus::Queued
        );
        assert_eq!(item.candidate_type, LearningCandidateType::ToolWrapperFix);
        assert_eq!(item.capability_id.as_deref(), Some("whatsapp"));
        assert_eq!(item.proposed_fix_type.as_deref(), Some("wrapper_script"));
        assert_eq!(
            item.proposed_files,
            vec!["skillshub/whatsapp/tool_schema.yaml"]
        );
        assert_eq!(
            item.fix_spec.get("failure_pattern").and_then(Value::as_str),
            Some("reaction command failed because the wrapper appended --json")
        );
        assert!(item.dedupe_fingerprint.is_some());
        assert_eq!(item.recurrence_count, 1);
        assert!(item.rank_score > 0.0);
        assert!(item
            .owner_hints
            .contains(&"operator_review_required".to_string()));

        let reread_candidate = store
            .read_candidate(&scope, &candidate.id)
            .expect("read candidate");
        assert_eq!(reread_candidate.state, LearningCandidateState::Triaged);

        let decisions = store
            .read_decisions(&scope, &candidate.id)
            .expect("read decisions");
        assert!(decisions
            .iter()
            .any(|decision| decision.decision == "routed_to_capability_evolution_backlog"));

        let events = store.list_events(&scope, 10).expect("list events");
        assert!(events
            .iter()
            .any(|event| event.event_type == "learning_capability_candidate_routed"));
    }

    #[test]
    fn route_skill_candidate_writes_skill_backlog_payload() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test-principal", "test-workspace");
        let candidate = store
            .create_candidate(scope.clone(), skill_candidate_request())
            .expect("create candidate");

        let outcome = LearningCapabilityEvolutionBridge::new(workspace)
            .route_candidate(&store, &scope, &candidate)
            .expect("route skill candidate");

        assert!(outcome.routed);
        assert_eq!(outcome.reason, "routed_to_skill_evolution_backlog");

        let item = store
            .read_capability_evolution_backlog_item(&scope, &candidate.id)
            .expect("read skill backlog item");
        assert_eq!(item.candidate_type, LearningCandidateType::SkillUpdate);
        assert_eq!(item.capability_id.as_deref(), Some("analyst-workflow"));
        assert_eq!(item.proposed_fix_type.as_deref(), Some("skill_guide"));
        assert_eq!(
            item.proposed_files,
            vec!["skills/analyst-workflow/SKILL.md"]
        );
        assert_eq!(
            item.fix_spec
                .get("workflow_signature")
                .and_then(Value::as_str),
            Some("agent:simple-data-analyst|tools:metabase>csvkit")
        );

        let decisions = store
            .read_decisions(&scope, &candidate.id)
            .expect("read decisions");
        assert!(decisions
            .iter()
            .any(|decision| decision.decision == "routed_to_skill_evolution_backlog"));

        let events = store.list_events(&scope, 10).expect("list events");
        assert!(events
            .iter()
            .any(|event| event.event_type == "learning_skill_candidate_routed"));
    }

    #[test]
    fn equivalent_candidates_merge_into_one_ranked_backlog_item() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test-principal", "test-workspace");
        let first = store
            .create_candidate(scope.clone(), capability_candidate_request())
            .expect("create first candidate");
        let bridge = LearningCapabilityEvolutionBridge::new(workspace);

        bridge
            .route_candidate(&store, &scope, &first)
            .expect("route first candidate");

        let mut duplicate_request = capability_candidate_request();
        duplicate_request.source_task_id = Some("task_capability_duplicate".to_string());
        duplicate_request.source_execution_id = Some("exec_capability_duplicate".to_string());
        duplicate_request.evidence_refs = vec![LearningEvidenceRef {
            kind: "chat_session".to_string(),
            id: Some("chat_capability_duplicate".to_string()),
            path: None,
            uri: None,
            summary: Some("The same reaction wrapper failure recurred.".to_string()),
        }];
        let duplicate = store
            .create_candidate(scope.clone(), duplicate_request)
            .expect("create duplicate candidate");

        let outcome = bridge
            .route_candidate(&store, &scope, &duplicate)
            .expect("route duplicate candidate");

        assert!(outcome.routed);
        assert_eq!(outcome.reason, "deduped_to_capability_evolution_backlog");
        assert!(outcome
            .backlog_path
            .as_deref()
            .expect("backlog path")
            .ends_with(&format!("{}.json", first.id)));

        let items = store
            .list_capability_evolution_backlog_items(
                &scope,
                crate::magician_v2::learning::LearningCapabilityEvolutionBacklogFilters::default(),
            )
            .expect("list backlog");
        assert_eq!(items.len(), 1);
        let item = store
            .read_capability_evolution_backlog_item(&scope, &first.id)
            .expect("read canonical backlog item");
        assert_eq!(item.candidate_id, first.id);
        assert_eq!(item.recurrence_count, 2);
        assert_eq!(item.evidence_refs.len(), 2);
        assert!(item.supersedes_candidate_ids.contains(&duplicate.id));
        assert!(item
            .rank_reasons
            .iter()
            .any(|reason| reason.contains("recurred 2 times")));
        assert!(item.rank_score > 0.0);

        let duplicate = store
            .read_candidate(&scope, &duplicate.id)
            .expect("read duplicate candidate");
        assert_eq!(duplicate.state, LearningCandidateState::Superseded);
        let decisions = store
            .read_decisions(&scope, &duplicate.id)
            .expect("read duplicate decisions");
        assert!(decisions
            .iter()
            .any(|decision| decision.decision == "deduped_to_capability_evolution_backlog"));
    }

    fn capability_candidate_request() -> CreateLearningCandidateRequest {
        CreateLearningCandidateRequest {
            principal: None,
            workspace: None,
            candidate_type: LearningCandidateType::ToolWrapperFix,
            state: LearningCandidateState::Proposed,
            title: "WhatsApp reaction wrapper fix".to_string(),
            summary: "The WhatsApp wrapper should not append --json to react commands.".to_string(),
            rationale: "The CLI supports reactions, but the wrapper made the command fail."
                .to_string(),
            proposed_change: json!({
                "capability_evolution": {
                    "capability_id": "whatsapp",
                    "failure_pattern": "reaction command failed because the wrapper appended --json",
                    "proposed_fix_type": "wrapper-script",
                    "proposed_files": ["skillshub/whatsapp/tool_schema.yaml"],
                    "required_eval": {
                        "goal": "react to a direct and group message without --json failure"
                    },
                    "promotion_gate": {
                        "requires_meta_harness_review": true,
                        "requires_local_validation": true
                    },
                    "expected_behavior": "messages react should send the requested emoji reaction"
                }
            }),
            proposed_target: Some("whatsapp".to_string()),
            confidence: Some(0.91),
            source_agent_id: Some("personal-assistant".to_string()),
            source_task_id: Some("task_capability".to_string()),
            source_execution_id: Some("exec_capability".to_string()),
            source_chat_session_id: None,
            event_refs: Vec::new(),
            evidence_refs: vec![LearningEvidenceRef {
                kind: "chat_session".to_string(),
                id: Some("chat_capability".to_string()),
                path: None,
                uri: None,
                summary: Some("wu messages react failed through the wrapper.".to_string()),
            }],
            risk_level: LearningRiskLevel::High,
            review_required: true,
            review_reason: Some("Tool wrapper changes require review and eval.".to_string()),
            review_policy: json!({"requires_review": true}),
            promotion_target: None,
            promotion_policy: json!({"eligible_for_auto_promotion": false}),
        }
    }

    fn skill_candidate_request() -> CreateLearningCandidateRequest {
        CreateLearningCandidateRequest {
            principal: None,
            workspace: None,
            candidate_type: LearningCandidateType::SkillUpdate,
            state: LearningCandidateState::Proposed,
            title: "Make analyst workflow reusable".to_string(),
            summary: "Repeated successful analyst workflow should become durable guidance."
                .to_string(),
            rationale: "Multiple related episodes followed the same tool/procedure sequence."
                .to_string(),
            proposed_change: json!({
                "skill_update": {
                    "skill_name": "analyst-workflow",
                    "workflow_signature": "agent:simple-data-analyst|tools:metabase>csvkit",
                    "trigger_conditions": "User asks for a cross-source analyst workflow.",
                    "procedure_steps": ["inspect sources", "validate transformed output"],
                    "proposed_files": ["skills/analyst-workflow/SKILL.md"],
                    "expected_behavior": "The analyst can activate the workflow later."
                }
            }),
            proposed_target: Some("skill:analyst-workflow".to_string()),
            confidence: Some(0.86),
            source_agent_id: Some("simple-data-analyst".to_string()),
            source_task_id: Some("task_skill".to_string()),
            source_execution_id: Some("exec_skill".to_string()),
            source_chat_session_id: None,
            event_refs: Vec::new(),
            evidence_refs: vec![LearningEvidenceRef {
                kind: "episode".to_string(),
                id: Some("exec_skill".to_string()),
                path: None,
                uri: None,
                summary: Some("Two analyst runs used the same successful sequence.".to_string()),
            }],
            risk_level: LearningRiskLevel::Medium,
            review_required: true,
            review_reason: Some("Skill updates require review and eval.".to_string()),
            review_policy: json!({"requires_review": true}),
            promotion_target: None,
            promotion_policy: json!({"eligible_for_auto_promotion": false}),
        }
    }
}
