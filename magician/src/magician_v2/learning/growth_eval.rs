//! Phase 9 learning-growth evaluation rollups.
//!
//! The runner deliberately consumes existing evidence surfaces instead of
//! creating another task harness. It asks whether the learning flywheel has
//! observable proof for memory quality, skill reuse, capability improvement,
//! program progress, user teaching, and guardrails.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use duckdb::Connection;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::magician_v2::analytics::duckdb_safety::{
    analytics_duckdb_guard, configure_analytics_connection_checked,
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::capability_bridge::LearningCapabilityEvolutionBridge;
use super::store::{
    LearningCandidateFilters, LearningCapabilityEvolutionApplicationFilters,
    LearningCapabilityEvolutionBacklogFilters, LearningCapabilityEvolutionImplementationFilters,
    LearningCapabilityEvolutionPostPromotionMonitorFilters,
    LearningCapabilityEvolutionPromotionFilters, LearningCapabilityEvolutionProposalFilters,
    LearningCapabilityEvolutionRollbackRecommendationFilters,
    LearningCapabilityEvolutionValidationFilters, LearningEvaluationBacklogFilters,
    LearningEvaluationRunFilters, LearningProcedureFilters, LearningStore,
};
use super::types::{
    CreateLearningCandidateRequest, LearningCandidate, LearningCandidateState,
    LearningCandidateType, LearningCapabilityEvolutionBacklogItem,
    LearningCapabilityEvolutionBacklogStatus,
    LearningCapabilityEvolutionPostPromotionMonitorRecord,
    LearningCapabilityEvolutionPostPromotionMonitorStatus,
    LearningCapabilityEvolutionPromotionRecord, LearningCapabilityEvolutionProposal,
    LearningCapabilityEvolutionProposalStatus,
    LearningCapabilityEvolutionRollbackRecommendationRecord,
    LearningCapabilityEvolutionRollbackRecommendationStatus,
    LearningCapabilityEvolutionValidationReport, LearningCapabilityEvolutionValidationStatus,
    LearningEvaluationBacklogItem, LearningEvaluationRunReport, LearningEvaluationRunStatus,
    LearningEvent, LearningEvidenceRef, LearningGrowthEvaluationDimensionReport,
    LearningGrowthEvaluationRunReport, LearningGrowthEvaluationScenarioReport, LearningProcedure,
    LearningProcedureStatus, LearningRiskLevel, LearningScope, RunLearningGrowthEvaluationRequest,
};

const DEFAULT_GROWTH_EVAL_WINDOW_DAYS: i64 = 30;
const MAX_GROWTH_EVAL_RECORDS: usize = 5_000;
const MAX_GROWTH_EVAL_EVIDENCE_REFS: usize = 24;
const STALE_BACKLOG_FAILURE_RATE: f64 = 0.25;

#[derive(Debug)]
struct GrowthEvidenceSnapshot {
    cutoff: DateTime<Utc>,
    candidates: Vec<LearningCandidate>,
    eval_backlog: Vec<LearningEvaluationBacklogItem>,
    eval_runs: Vec<LearningEvaluationRunReport>,
    all_capability_backlog: Vec<LearningCapabilityEvolutionBacklogItem>,
    capability_backlog: Vec<LearningCapabilityEvolutionBacklogItem>,
    capability_proposals: Vec<LearningCapabilityEvolutionProposal>,
    capability_validations: Vec<LearningCapabilityEvolutionValidationReport>,
    capability_promotions: Vec<LearningCapabilityEvolutionPromotionRecord>,
    post_promotion_monitors: Vec<LearningCapabilityEvolutionPostPromotionMonitorRecord>,
    rollback_recommendations: Vec<LearningCapabilityEvolutionRollbackRecommendationRecord>,
    procedures: Vec<LearningProcedure>,
    events: Vec<LearningEvent>,
    program_state_count: usize,
    memory_analytics: MemoryEvalAnalyticsSummary,
    llm_analytics: LlmPromptAnalyticsSummary,
}

#[derive(Debug, Default)]
struct MemoryEvalAnalyticsSummary {
    case_count: usize,
    passed_count: usize,
    failed_count: usize,
    suite_count: usize,
    query_error: Option<String>,
}

#[derive(Debug, Default)]
struct LlmPromptAnalyticsSummary {
    call_count: usize,
    avg_total_tokens: Option<f64>,
    max_total_tokens: Option<i64>,
    previous_avg_total_tokens: Option<f64>,
    recent_avg_total_tokens: Option<f64>,
    query_error: Option<String>,
}

pub fn run_learning_growth_evaluation(
    store: &LearningStore,
    scope: LearningScope,
    request: RunLearningGrowthEvaluationRequest,
) -> Result<LearningGrowthEvaluationRunReport> {
    let suite_id = request
        .suite_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "agent_growth_phase9".to_string());
    let window_days = request
        .window_days
        .unwrap_or(DEFAULT_GROWTH_EVAL_WINDOW_DAYS)
        .clamp(1, 3650);
    let cutoff = Utc::now() - Duration::days(window_days);
    let snapshot = load_snapshot(store, &scope, cutoff)?;

    let dimensions = vec![
        evaluate_memory_recall(store, &scope, &snapshot),
        evaluate_memory_precision(store, &scope, &snapshot),
        evaluate_stale_memory_correction(store, &scope, &snapshot),
        evaluate_skill_reuse(store, &scope, &snapshot),
        evaluate_capability_improvement(store, &scope, &snapshot),
        evaluate_candidate_dedupe_quality(store, &scope, &snapshot),
        evaluate_stale_backlog_rate(store, &scope, &snapshot),
        evaluate_proposal_validation_pass_rate(store, &scope, &snapshot),
        evaluate_autonomous_program_progress(store, &scope, &snapshot),
        evaluate_user_feedback_incorporation(store, &scope, &snapshot),
        evaluate_false_learning_prevention(store, &scope, &snapshot),
        evaluate_prompt_token_growth(&snapshot),
        evaluate_tool_failure_reduction(store, &scope, &snapshot),
        evaluate_procedure_extraction_precision(store, &scope, &snapshot),
        evaluate_procedure_retrieval_relevance(store, &scope, &snapshot),
        evaluate_procedure_misuse_rate(store, &scope, &snapshot),
        evaluate_procedure_helped_hurt_outcome_signal(store, &scope, &snapshot),
        evaluate_stale_procedure_correction(store, &scope, &snapshot),
        evaluate_duplicate_procedure_rate(store, &scope, &snapshot),
        evaluate_procedure_to_skill_promotion_quality(store, &scope, &snapshot),
        evaluate_post_promotion_stable_promotions(store, &scope, &snapshot),
        evaluate_post_promotion_regression_rate(store, &scope, &snapshot),
        evaluate_rollback_followup_rate(store, &scope, &snapshot),
    ];
    let scenarios = evaluate_scenarios(store, &scope, &snapshot);
    let failed_count = dimensions
        .iter()
        .filter(|dimension| dimension.status == LearningEvaluationRunStatus::Failed)
        .count()
        + scenarios
            .iter()
            .filter(|scenario| scenario.status == LearningEvaluationRunStatus::Failed)
            .count();
    let blocked_count = dimensions
        .iter()
        .filter(|dimension| dimension.status == LearningEvaluationRunStatus::Blocked)
        .count()
        + scenarios
            .iter()
            .filter(|scenario| scenario.status == LearningEvaluationRunStatus::Blocked)
            .count();
    let passed_count = dimensions
        .iter()
        .filter(|dimension| dimension.status == LearningEvaluationRunStatus::Passed)
        .count()
        + scenarios
            .iter()
            .filter(|scenario| scenario.status == LearningEvaluationRunStatus::Passed)
            .count();
    let status = if failed_count > 0 {
        LearningEvaluationRunStatus::Failed
    } else if blocked_count > 0 {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    let evidence_refs = collect_report_evidence(&dimensions, &scenarios);
    let dimension_count = dimensions.len();
    let scenario_count = scenarios.len();
    let summary = format!(
        "Growth evaluation `{suite_id}` checked {} dimensions and {} scenarios over {window_days} day(s): {passed_count} passed, {failed_count} failed, {blocked_count} blocked.",
        dimension_count,
        scenario_count
    );

    let mut report = LearningGrowthEvaluationRunReport {
        id: format!("lge_{}", Uuid::new_v4().simple()),
        scope,
        suite_id,
        status,
        summary,
        dimensions,
        scenarios,
        evidence_refs,
        metrics: json!({
            "window_days": window_days,
            "dimension_count": dimension_count,
            "scenario_count": scenario_count,
            "passed_count": passed_count,
            "failed_count": failed_count,
            "blocked_count": blocked_count,
            "candidate_count": snapshot.candidates.len(),
            "procedure_count": snapshot.procedures.len(),
            "learning_event_count": snapshot.events.len(),
            "evaluation_backlog_count": snapshot.eval_backlog.len(),
            "evaluation_run_count": snapshot.eval_runs.len(),
            "capability_backlog_total_count": snapshot.all_capability_backlog.len(),
            "capability_backlog_count": snapshot.capability_backlog.len(),
            "capability_proposal_count": snapshot.capability_proposals.len(),
            "capability_validation_count": snapshot.capability_validations.len(),
            "capability_promotion_count": snapshot.capability_promotions.len(),
            "post_promotion_monitor_count": snapshot.post_promotion_monitors.len(),
            "rollback_recommendation_count": snapshot.rollback_recommendations.len(),
            "program_state_count": snapshot.program_state_count,
            "memory_eval_case_count": snapshot.memory_analytics.case_count,
            "llm_call_count": snapshot.llm_analytics.call_count
        }),
        payload: json!({
            "request_payload": request.payload,
            "cutoff": cutoff,
            "memory_analytics_query_error": snapshot.memory_analytics.query_error,
            "llm_analytics_query_error": snapshot.llm_analytics.query_error
        }),
        created_at: Utc::now(),
    };
    let routed_failures = route_growth_eval_failures(store, &report);
    let routed_count = routed_failures
        .iter()
        .filter(|outcome| outcome.get("routed").and_then(Value::as_bool) == Some(true))
        .count();
    let skipped_count = routed_failures.len().saturating_sub(routed_count);
    if let Some(metrics) = report.metrics.as_object_mut() {
        metrics.insert(
            "growth_eval_failure_route_count".to_string(),
            json!(routed_failures.len()),
        );
        metrics.insert(
            "growth_eval_failure_routed_count".to_string(),
            json!(routed_count),
        );
        metrics.insert(
            "growth_eval_failure_skipped_count".to_string(),
            json!(skipped_count),
        );
    }
    if let Some(payload) = report.payload.as_object_mut() {
        payload.insert(
            "skill_evolution_failure_routing".to_string(),
            json!(routed_failures),
        );
    }

    Ok(report)
}

fn load_snapshot(
    store: &LearningStore,
    scope: &LearningScope,
    cutoff: DateTime<Utc>,
) -> Result<GrowthEvidenceSnapshot> {
    let candidates = store
        .list_candidates(
            scope,
            LearningCandidateFilters {
                limit: Some(MAX_GROWTH_EVAL_RECORDS),
                ..LearningCandidateFilters::default()
            },
        )?
        .into_iter()
        .filter(|candidate| candidate.updated_at >= cutoff || candidate.created_at >= cutoff)
        .collect::<Vec<_>>();
    let eval_backlog = store
        .list_evaluation_backlog_items(
            scope,
            LearningEvaluationBacklogFilters {
                limit: Some(MAX_GROWTH_EVAL_RECORDS),
                ..LearningEvaluationBacklogFilters::default()
            },
        )?
        .into_iter()
        .filter(|item| item.updated_at >= cutoff || item.created_at >= cutoff)
        .collect::<Vec<_>>();
    let eval_runs = store
        .list_evaluation_run_reports(
            scope,
            LearningEvaluationRunFilters {
                limit: Some(MAX_GROWTH_EVAL_RECORDS),
                ..LearningEvaluationRunFilters::default()
            },
        )?
        .into_iter()
        .filter(|run| run.created_at >= cutoff)
        .collect::<Vec<_>>();
    let all_capability_backlog = store.list_capability_evolution_backlog_items(
        scope,
        LearningCapabilityEvolutionBacklogFilters {
            limit: Some(MAX_GROWTH_EVAL_RECORDS),
            ..LearningCapabilityEvolutionBacklogFilters::default()
        },
    )?;
    let capability_backlog = all_capability_backlog
        .iter()
        .filter(|item| item.updated_at >= cutoff || item.created_at >= cutoff)
        .cloned()
        .collect::<Vec<_>>();
    let capability_proposals = store
        .list_capability_evolution_proposals(
            scope,
            LearningCapabilityEvolutionProposalFilters {
                limit: Some(MAX_GROWTH_EVAL_RECORDS),
                ..LearningCapabilityEvolutionProposalFilters::default()
            },
        )?
        .into_iter()
        .filter(|proposal| proposal.updated_at >= cutoff || proposal.created_at >= cutoff)
        .collect::<Vec<_>>();
    let capability_validations = store
        .list_capability_evolution_validation_reports(
            scope,
            LearningCapabilityEvolutionValidationFilters {
                limit: Some(MAX_GROWTH_EVAL_RECORDS),
                ..LearningCapabilityEvolutionValidationFilters::default()
            },
        )?
        .into_iter()
        .filter(|report| report.created_at >= cutoff)
        .collect::<Vec<_>>();
    let capability_promotions = store
        .list_capability_evolution_promotion_records(
            scope,
            LearningCapabilityEvolutionPromotionFilters {
                limit: Some(MAX_GROWTH_EVAL_RECORDS),
                ..LearningCapabilityEvolutionPromotionFilters::default()
            },
        )?
        .into_iter()
        .filter(|record| record.created_at >= cutoff)
        .collect::<Vec<_>>();
    let post_promotion_monitors = store
        .list_capability_evolution_post_promotion_monitor_records(
            scope,
            LearningCapabilityEvolutionPostPromotionMonitorFilters {
                limit: Some(MAX_GROWTH_EVAL_RECORDS),
                ..LearningCapabilityEvolutionPostPromotionMonitorFilters::default()
            },
        )?
        .into_iter()
        .filter(|record| record.updated_at >= cutoff || record.created_at >= cutoff)
        .collect::<Vec<_>>();
    let rollback_recommendations = store
        .list_capability_evolution_rollback_recommendation_records(
            scope,
            LearningCapabilityEvolutionRollbackRecommendationFilters {
                limit: Some(MAX_GROWTH_EVAL_RECORDS),
                ..LearningCapabilityEvolutionRollbackRecommendationFilters::default()
            },
        )?;
    let procedures = store.list_procedures(
        scope,
        LearningProcedureFilters {
            limit: Some(MAX_GROWTH_EVAL_RECORDS),
            ..LearningProcedureFilters::default()
        },
    )?;
    let events = store
        .list_events(scope, MAX_GROWTH_EVAL_RECORDS)?
        .into_iter()
        .filter(|event| event.created_at >= cutoff)
        .collect::<Vec<_>>();
    let program_state_count = count_json_files(
        store.workspace_layout(),
        &store
            .workspace_layout()
            .program_runtime_states_dir(&scope.principal, &scope.workspace),
    );
    let memory_analytics = load_memory_eval_analytics(store.workspace_layout(), scope, cutoff);
    let llm_analytics = load_llm_prompt_analytics(store.workspace_layout(), scope, cutoff);

    // Touch these listing APIs so Phase 9 observes the full proposal/apply chain
    // and surfaces storage errors even when the current score only needs the
    // downstream validation/promotion records.
    let _ = store.list_capability_evolution_implementation_records(
        scope,
        LearningCapabilityEvolutionImplementationFilters {
            limit: Some(MAX_GROWTH_EVAL_RECORDS),
            ..LearningCapabilityEvolutionImplementationFilters::default()
        },
    )?;
    let _ = store.list_capability_evolution_application_records(
        scope,
        LearningCapabilityEvolutionApplicationFilters {
            limit: Some(MAX_GROWTH_EVAL_RECORDS),
            ..LearningCapabilityEvolutionApplicationFilters::default()
        },
    )?;

    Ok(GrowthEvidenceSnapshot {
        cutoff,
        candidates,
        eval_backlog,
        eval_runs,
        all_capability_backlog,
        capability_backlog,
        capability_proposals,
        capability_validations,
        capability_promotions,
        post_promotion_monitors,
        rollback_recommendations,
        procedures,
        events,
        program_state_count,
        memory_analytics,
        llm_analytics,
    })
}

fn route_growth_eval_failures(
    store: &LearningStore,
    report: &LearningGrowthEvaluationRunReport,
) -> Vec<Value> {
    let bridge = LearningCapabilityEvolutionBridge::new(store.workspace_layout().clone());
    let mut outcomes = Vec::new();
    for dimension in report
        .dimensions
        .iter()
        .filter(|dimension| dimension.status == LearningEvaluationRunStatus::Failed)
    {
        let concrete_evidence = concrete_skill_evolution_failure_evidence(&dimension.evidence_refs);
        if concrete_evidence.is_empty() {
            outcomes.push(json!({
                "kind": "dimension",
                "name": dimension.dimension,
                "routed": false,
                "reason": "no_concrete_skill_evolution_evidence"
            }));
            continue;
        }

        match route_growth_eval_failure_dimension(
            store,
            report,
            &bridge,
            dimension,
            concrete_evidence,
        ) {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => outcomes.push(json!({
                "kind": "dimension",
                "name": dimension.dimension,
                "routed": false,
                "reason": "routing_error",
                "error": error.to_string()
            })),
        }
    }
    outcomes
}

fn route_growth_eval_failure_dimension(
    store: &LearningStore,
    report: &LearningGrowthEvaluationRunReport,
    bridge: &LearningCapabilityEvolutionBridge,
    dimension: &LearningGrowthEvaluationDimensionReport,
    concrete_evidence: Vec<LearningEvidenceRef>,
) -> Result<Value> {
    let target = growth_eval_failure_target(&dimension.dimension, &concrete_evidence);
    let candidate = store.create_candidate(
        report.scope.clone(),
        CreateLearningCandidateRequest {
            principal: None,
            workspace: None,
            candidate_type: LearningCandidateType::SkillUpdate,
            state: LearningCandidateState::Observed,
            title: format!(
                "Investigate failed Skill Evolution health dimension `{}`",
                dimension.dimension
            ),
            summary: dimension.summary.clone(),
            rationale: format!(
                "Growth evaluation `{}` failed `{}` with concrete persisted evidence. Route it through Skill Evolution instead of treating it as a detached score.",
                report.suite_id, dimension.dimension
            ),
            proposed_change: json!({
                "skill_update": {
                    "capability_id": "skill_evolution:growth_eval",
                    "failure_pattern": format!(
                        "growth_eval_dimension_failed:{}:{}",
                        dimension.dimension, target
                    ),
                    "proposed_fix_type": "skill_evolution_health_rollup",
                    "target_type": "growth_eval_dimension",
                    "target": target.clone(),
                    "growth_eval_run_id": report.id.clone(),
                    "growth_eval_suite_id": report.suite_id.clone(),
                    "growth_eval_dimension": dimension.dimension.clone(),
                    "growth_eval_status": dimension.status.as_str(),
                    "summary": dimension.summary.clone(),
                    "metrics": dimension.metrics.clone(),
                    "evidence_refs": concrete_evidence.clone(),
                    "occurrence_count": concrete_evidence.len(),
                    "blocked": true
                }
            }),
            proposed_target: Some(target.clone()),
            confidence: Some(0.82),
            source_agent_id: Some("growth_eval".to_string()),
            source_task_id: Some(report.id.clone()),
            source_execution_id: None,
            source_chat_session_id: None,
            event_refs: Vec::new(),
            evidence_refs: concrete_evidence,
            risk_level: LearningRiskLevel::High,
            review_required: true,
            review_reason: Some(format!(
                "Growth-eval health dimension `{}` failed.",
                dimension.dimension
            )),
            review_policy: json!({
                "requires_operator_review": true,
                "source": "growth_eval",
                "dimension": dimension.dimension.clone()
            }),
            promotion_target: Some("skill_evolution".to_string()),
            promotion_policy: json!({
                "requires_validation": true,
                "source": "growth_eval"
            }),
        },
    )?;
    let outcome = bridge.route_candidate(store, &report.scope, &candidate)?;
    Ok(json!({
        "kind": "dimension",
        "name": dimension.dimension,
        "routed": outcome.routed,
        "candidate_id": outcome.candidate_id,
        "backlog_path": outcome.backlog_path,
        "reason": outcome.reason,
        "target": target
    }))
}

fn concrete_skill_evolution_failure_evidence(
    evidence_refs: &[LearningEvidenceRef],
) -> Vec<LearningEvidenceRef> {
    evidence_refs
        .iter()
        .filter(|evidence| {
            matches!(
                evidence.kind.as_str(),
                "learning_capability_backlog"
                    | "learning_capability_proposal"
                    | "learning_capability_validation"
                    | "learning_capability_post_promotion_monitor"
                    | "learning_capability_rollback_recommendation"
            ) && (evidence.id.is_some() || evidence.path.is_some() || evidence.uri.is_some())
        })
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .cloned()
        .collect()
}

fn growth_eval_failure_target(dimension: &str, evidence_refs: &[LearningEvidenceRef]) -> String {
    let primary = evidence_refs.first();
    let primary_kind = primary
        .map(|evidence| evidence.kind.as_str())
        .unwrap_or("unknown");
    let primary_id = primary
        .and_then(|evidence| evidence.id.as_deref())
        .or_else(|| primary.and_then(|evidence| evidence.path.as_deref()))
        .or_else(|| primary.and_then(|evidence| evidence.uri.as_deref()))
        .unwrap_or("unknown");
    format!("skill_evolution:growth_eval:{dimension}:{primary_kind}:{primary_id}")
}

fn evaluate_memory_recall(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let analytics = &snapshot.memory_analytics;
    let pass_rate = if analytics.case_count == 0 {
        None
    } else {
        Some(analytics.passed_count as f64 / analytics.case_count as f64)
    };
    if let Some(error) = analytics.query_error.as_deref() {
        return dimension(
            "memory_recall",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            format!("Memory recall could not be measured from memory_events: {error}"),
            vec![analytics_ref(store, scope, "memory_events")],
            json!({ "query_error": error }),
        );
    }
    if analytics.case_count == 0 {
        return dimension(
            "memory_recall",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No memory eval rows were found; run the memory eval suite before judging recall."
                .to_string(),
            vec![analytics_ref(store, scope, "memory_events")],
            json!({ "case_count": 0 }),
        );
    }
    let rate = pass_rate.unwrap_or(0.0);
    dimension(
        "memory_recall",
        if rate >= 0.8 {
            LearningEvaluationRunStatus::Passed
        } else {
            LearningEvaluationRunStatus::Failed
        },
        rate,
        format!(
            "Memory eval recall pass rate is {:.0}% across {} case(s) and {} suite(s).",
            rate * 100.0,
            analytics.case_count,
            analytics.suite_count
        ),
        vec![analytics_ref(store, scope, "memory_events")],
        json!({
            "case_count": analytics.case_count,
            "passed_count": analytics.passed_count,
            "failed_count": analytics.failed_count,
            "suite_count": analytics.suite_count,
            "pass_rate": rate
        }),
    )
}

fn evaluate_memory_precision(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let memory_candidates = snapshot
        .candidates
        .iter()
        .filter(|candidate| candidate.candidate_type.is_memory_candidate())
        .collect::<Vec<_>>();
    let promoted = memory_candidates
        .iter()
        .filter(|candidate| candidate.state == LearningCandidateState::Promoted)
        .copied()
        .collect::<Vec<_>>();
    let risky_promoted = promoted
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.risk_level,
                LearningRiskLevel::High | LearningRiskLevel::Critical
            ) && !has_approval_or_promotion_decision(store, scope, &candidate.id)
        })
        .copied()
        .collect::<Vec<_>>();
    if !risky_promoted.is_empty() {
        return dimension(
            "memory_precision",
            LearningEvaluationRunStatus::Failed,
            0.0,
            format!(
                "{} high/critical-risk memory candidate(s) were promoted without an approval/promotion decision.",
                risky_promoted.len()
            ),
            candidate_refs(store, scope, &risky_promoted),
            json!({
                "memory_candidate_count": memory_candidates.len(),
                "promoted_count": promoted.len(),
                "risky_promoted_count": risky_promoted.len()
            }),
        );
    }
    if memory_candidates.is_empty() {
        return dimension(
            "memory_precision",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No memory candidates were available to judge precision.".to_string(),
            Vec::new(),
            json!({ "memory_candidate_count": 0 }),
        );
    }
    let score = if promoted.is_empty() { 0.5 } else { 1.0 };
    dimension(
        "memory_precision",
        LearningEvaluationRunStatus::Passed,
        score,
        format!(
            "{} memory candidate(s) observed; {} promoted without unreviewed high/critical-risk promotion.",
            memory_candidates.len(),
            promoted.len()
        ),
        candidate_refs(
            store,
            scope,
            &memory_candidates
                .iter()
                .copied()
                .take(6)
                .collect::<Vec<_>>(),
        ),
        json!({
            "memory_candidate_count": memory_candidates.len(),
            "promoted_count": promoted.len(),
            "risky_promoted_count": 0
        }),
    )
}

fn evaluate_stale_memory_correction(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let correction_events = teaching_events(snapshot, &["correct", "forget"]);
    let correction_candidates = snapshot
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.candidate_type.is_memory_candidate()
                && matches!(
                    memory_operation(candidate).as_deref(),
                    Some("replace") | Some("remove")
                )
        })
        .collect::<Vec<_>>();
    if correction_events.is_empty() && correction_candidates.is_empty() {
        return dimension(
            "stale_memory_correction",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No correction or forget evidence was found in the current window.".to_string(),
            Vec::new(),
            json!({ "correction_event_count": 0, "correction_candidate_count": 0 }),
        );
    }
    dimension(
        "stale_memory_correction",
        LearningEvaluationRunStatus::Passed,
        1.0,
        format!(
            "Found {} correction/forget teaching event(s) and {} memory replace/remove candidate(s).",
            correction_events.len(),
            correction_candidates.len()
        ),
        event_refs(&correction_events)
            .into_iter()
            .chain(candidate_refs(store, scope, &correction_candidates))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "correction_event_count": correction_events.len(),
            "correction_candidate_count": correction_candidates.len()
        }),
    )
}

fn evaluate_skill_reuse(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let skill_candidates = snapshot
        .candidates
        .iter()
        .filter(|candidate| candidate.candidate_type.is_skill_or_workflow_candidate())
        .collect::<Vec<_>>();
    let reusable_backlog = snapshot
        .capability_backlog
        .iter()
        .filter(|item| item.candidate_type.is_skill_or_workflow_candidate())
        .collect::<Vec<_>>();
    let promoted = skill_candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.state,
                LearningCandidateState::Implemented
                    | LearningCandidateState::Evaluated
                    | LearningCandidateState::Promoted
            )
        })
        .copied()
        .collect::<Vec<_>>();
    if promoted.is_empty() && reusable_backlog.is_empty() {
        return dimension(
            "skill_reuse",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No reusable skill/workflow candidates were found.".to_string(),
            Vec::new(),
            json!({ "skill_candidate_count": 0, "skill_backlog_count": 0 }),
        );
    }
    let status = if promoted.is_empty() {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    let score = if promoted.is_empty() { 0.5 } else { 1.0 };
    dimension(
        "skill_reuse",
        status,
        score,
        format!(
            "{} reusable workflow candidate(s), {} backlog item(s), {} evaluated/implemented/promoted.",
            skill_candidates.len(),
            reusable_backlog.len(),
            promoted.len()
        ),
        candidate_refs(
            store,
            scope,
            &skill_candidates.iter().copied().take(8).collect::<Vec<_>>(),
        ),
        json!({
            "skill_candidate_count": skill_candidates.len(),
            "skill_backlog_count": reusable_backlog.len(),
            "promoted_or_evaluated_count": promoted.len()
        }),
    )
}

fn evaluate_capability_improvement(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let capability_candidates = snapshot
        .candidates
        .iter()
        .filter(|candidate| candidate.candidate_type.is_capability_evolution_candidate())
        .collect::<Vec<_>>();
    let passed_validations = passed_validations(snapshot);
    let failed_validations = snapshot
        .capability_validations
        .iter()
        .filter(|validation| {
            validation.status == LearningCapabilityEvolutionValidationStatus::Failed
        })
        .collect::<Vec<_>>();
    if passed_validations.is_empty() && !failed_validations.is_empty() {
        return dimension(
            "capability_improvement_success",
            LearningEvaluationRunStatus::Failed,
            0.0,
            format!(
                "{} capability validation(s) failed and no passing validation was found.",
                failed_validations.len()
            ),
            validation_refs(store, scope, &failed_validations),
            json!({
                "capability_candidate_count": capability_candidates.len(),
                "passed_validation_count": 0,
                "failed_validation_count": failed_validations.len(),
                "promotion_count": snapshot.capability_promotions.len()
            }),
        );
    }
    if capability_candidates.is_empty() && snapshot.capability_backlog.is_empty() {
        return dimension(
            "capability_improvement_success",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No capability/tool improvement candidates were found.".to_string(),
            Vec::new(),
            json!({ "capability_candidate_count": 0 }),
        );
    }
    let status = if passed_validations.is_empty() && snapshot.capability_promotions.is_empty() {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    let score = if status == LearningEvaluationRunStatus::Passed {
        1.0
    } else {
        0.5
    };
    dimension(
        "capability_improvement_success",
        status,
        score,
        format!(
            "{} capability candidate(s), {} backlog item(s), {} passing validation(s), {} promotion(s).",
            capability_candidates.len(),
            snapshot.capability_backlog.len(),
            passed_validations.len(),
            snapshot.capability_promotions.len()
        ),
        candidate_refs(
            store,
            scope,
            &capability_candidates
                .iter()
                .copied()
                .take(8)
                .collect::<Vec<_>>(),
        )
        .into_iter()
        .chain(validation_refs(store, scope, &passed_validations))
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .collect(),
        json!({
            "capability_candidate_count": capability_candidates.len(),
            "capability_backlog_count": snapshot.capability_backlog.len(),
            "passed_validation_count": passed_validations.len(),
            "failed_validation_count": failed_validations.len(),
            "promotion_count": snapshot.capability_promotions.len()
        }),
    )
}

fn evaluate_candidate_dedupe_quality(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let open_backlog = snapshot
        .all_capability_backlog
        .iter()
        .filter(|item| item.status.is_open_for_dedupe())
        .collect::<Vec<_>>();
    if open_backlog.is_empty() {
        return dimension(
            "candidate_dedupe_quality",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No open Skill Evolution backlog items were found for dedupe checks.".to_string(),
            Vec::new(),
            json!({ "open_backlog_count": 0 }),
        );
    }

    let missing_fingerprint = open_backlog
        .iter()
        .copied()
        .filter(|item| {
            item.dedupe_fingerprint
                .as_deref()
                .is_none_or(|fingerprint| fingerprint.trim().is_empty())
        })
        .collect::<Vec<_>>();
    let mut by_fingerprint: BTreeMap<String, Vec<&LearningCapabilityEvolutionBacklogItem>> =
        BTreeMap::new();
    for item in &open_backlog {
        let Some(fingerprint) = item
            .dedupe_fingerprint
            .as_deref()
            .map(str::trim)
            .filter(|fingerprint| !fingerprint.is_empty())
        else {
            continue;
        };
        by_fingerprint
            .entry(fingerprint.to_string())
            .or_default()
            .push(*item);
    }
    let duplicate_groups = by_fingerprint
        .values()
        .filter(|items| items.len() > 1)
        .collect::<Vec<_>>();
    let duplicate_backlog = duplicate_groups
        .iter()
        .flat_map(|items| items.iter().copied())
        .collect::<Vec<_>>();
    let duplicate_extra_count = duplicate_groups
        .iter()
        .map(|items| items.len().saturating_sub(1))
        .sum::<usize>();
    let issue_count = missing_fingerprint.len() + duplicate_extra_count;
    let issue_rate = issue_count as f64 / open_backlog.len() as f64;
    let superseded_count = snapshot
        .all_capability_backlog
        .iter()
        .filter(|item| {
            item.status == LearningCapabilityEvolutionBacklogStatus::Superseded
                || !item.supersedes_candidate_ids.is_empty()
        })
        .count();
    let status = if issue_count == 0 {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    dimension(
        "candidate_dedupe_quality",
        status,
        1.0 - issue_rate.min(1.0),
        format!(
            "{} open backlog item(s), {} missing dedupe fingerprint(s), {} duplicate fingerprint group(s).",
            open_backlog.len(),
            missing_fingerprint.len(),
            duplicate_groups.len()
        ),
        backlog_refs(store, scope, &missing_fingerprint)
            .into_iter()
            .chain(backlog_refs(store, scope, &duplicate_backlog))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "open_backlog_count": open_backlog.len(),
            "missing_dedupe_fingerprint_count": missing_fingerprint.len(),
            "duplicate_fingerprint_group_count": duplicate_groups.len(),
            "duplicate_open_backlog_extra_count": duplicate_extra_count,
            "dedupe_issue_rate": issue_rate,
            "superseded_backlog_count": superseded_count
        }),
    )
}

fn evaluate_stale_backlog_rate(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let open_backlog = snapshot
        .all_capability_backlog
        .iter()
        .filter(|item| item.status.is_open_for_dedupe())
        .collect::<Vec<_>>();
    if open_backlog.is_empty() {
        return dimension(
            "stale_backlog_rate",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No open Skill Evolution backlog items were found for staleness checks.".to_string(),
            Vec::new(),
            json!({ "open_backlog_count": 0 }),
        );
    }
    let stale_backlog = open_backlog
        .iter()
        .copied()
        .filter(|item| item.updated_at < snapshot.cutoff && item.created_at < snapshot.cutoff)
        .collect::<Vec<_>>();
    let stale_rate = stale_backlog.len() as f64 / open_backlog.len() as f64;
    let status = if stale_rate > STALE_BACKLOG_FAILURE_RATE {
        LearningEvaluationRunStatus::Failed
    } else {
        LearningEvaluationRunStatus::Passed
    };
    dimension(
        "stale_backlog_rate",
        status,
        1.0 - stale_rate.min(1.0),
        format!(
            "{} stale open backlog item(s) across {} open backlog item(s).",
            stale_backlog.len(),
            open_backlog.len()
        ),
        backlog_refs(store, scope, &stale_backlog),
        json!({
            "open_backlog_count": open_backlog.len(),
            "stale_backlog_count": stale_backlog.len(),
            "stale_backlog_rate": stale_rate,
            "failure_threshold": STALE_BACKLOG_FAILURE_RATE,
            "cutoff": snapshot.cutoff
        }),
    )
}

fn evaluate_proposal_validation_pass_rate(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let approved_proposals = snapshot
        .capability_proposals
        .iter()
        .filter(|proposal| proposal.status == LearningCapabilityEvolutionProposalStatus::Approved)
        .collect::<Vec<_>>();
    let passed_validations = passed_validations(snapshot);
    let failed_or_blocked_validations = snapshot
        .capability_validations
        .iter()
        .filter(|validation| {
            validation.status != LearningCapabilityEvolutionValidationStatus::Passed
        })
        .collect::<Vec<_>>();
    let validated_proposal_ids = snapshot
        .capability_validations
        .iter()
        .map(|validation| validation.proposal_id.as_str())
        .collect::<BTreeSet<_>>();
    let approved_without_validation = approved_proposals
        .iter()
        .copied()
        .filter(|proposal| !validated_proposal_ids.contains(proposal.id.as_str()))
        .collect::<Vec<_>>();
    if snapshot.capability_validations.is_empty() && approved_proposals.is_empty() {
        return dimension(
            "proposal_validation_pass_rate",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No approved proposals or validation reports were found.".to_string(),
            Vec::new(),
            json!({
                "approved_proposal_count": 0,
                "validation_count": 0
            }),
        );
    }
    let validation_count = snapshot.capability_validations.len();
    let denominator = validation_count + approved_without_validation.len();
    let pass_rate = if denominator == 0 {
        0.0
    } else {
        passed_validations.len() as f64 / denominator as f64
    };
    let status =
        if failed_or_blocked_validations.is_empty() && approved_without_validation.is_empty() {
            LearningEvaluationRunStatus::Passed
        } else {
            LearningEvaluationRunStatus::Failed
        };
    dimension(
        "proposal_validation_pass_rate",
        status,
        pass_rate,
        format!(
            "{} passing validation(s), {} failed/blocked validation(s), {} approved proposal(s) missing validation.",
            passed_validations.len(),
            failed_or_blocked_validations.len(),
            approved_without_validation.len()
        ),
        validation_refs(store, scope, &failed_or_blocked_validations)
            .into_iter()
            .chain(proposal_refs(store, scope, &approved_without_validation))
            .chain(validation_refs(store, scope, &passed_validations))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "approved_proposal_count": approved_proposals.len(),
            "validation_count": validation_count,
            "passed_validation_count": passed_validations.len(),
            "failed_or_blocked_validation_count": failed_or_blocked_validations.len(),
            "approved_without_validation_count": approved_without_validation.len(),
            "validation_pass_rate": pass_rate
        }),
    )
}

fn evaluate_post_promotion_stable_promotions(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    if snapshot.capability_promotions.is_empty() {
        return dimension(
            "post_promotion_stable_promotions",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No Skill Evolution promotions were available to judge post-promotion stability."
                .to_string(),
            Vec::new(),
            json!({ "promotion_count": 0, "monitor_count": 0 }),
        );
    }
    if snapshot.post_promotion_monitors.is_empty() {
        return dimension(
            "post_promotion_stable_promotions",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            format!(
                "{} promotion(s) exist, but none have post-promotion monitor records yet.",
                snapshot.capability_promotions.len()
            ),
            Vec::new(),
            json!({
                "promotion_count": snapshot.capability_promotions.len(),
                "monitor_count": 0
            }),
        );
    }

    let stable = snapshot
        .post_promotion_monitors
        .iter()
        .filter(|monitor| {
            monitor.status == LearningCapabilityEvolutionPostPromotionMonitorStatus::Stable
        })
        .collect::<Vec<_>>();
    let regression = snapshot
        .post_promotion_monitors
        .iter()
        .filter(|monitor| {
            monitor.status
                == LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected
        })
        .collect::<Vec<_>>();
    let decisive_count = stable.len() + regression.len();
    if decisive_count == 0 {
        return dimension(
            "post_promotion_stable_promotions",
            LearningEvaluationRunStatus::Blocked,
            0.25,
            format!(
                "{} monitor(s) exist, but none have enough evidence to mark a promotion stable or regressed.",
                snapshot.post_promotion_monitors.len()
            ),
            monitor_refs(
                store,
                scope,
                &snapshot
                    .post_promotion_monitors
                    .iter()
                    .take(8)
                    .collect::<Vec<_>>(),
            ),
            json!({
                "promotion_count": snapshot.capability_promotions.len(),
                "monitor_count": snapshot.post_promotion_monitors.len(),
                "stable_count": 0,
                "regression_count": 0,
                "decisive_monitor_count": 0
            }),
        );
    }

    let stable_rate = stable.len() as f64 / decisive_count as f64;
    let status = if stable_rate >= 0.8 {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    dimension(
        "post_promotion_stable_promotions",
        status,
        stable_rate,
        format!(
            "{} of {} decisive post-promotion monitor(s) are stable; {} regression monitor(s) remain.",
            stable.len(),
            decisive_count,
            regression.len()
        ),
        monitor_refs(
            store,
            scope,
            &stable
                .iter()
                .copied()
                .chain(regression.iter().copied())
                .take(12)
                .collect::<Vec<_>>(),
        ),
        json!({
            "promotion_count": snapshot.capability_promotions.len(),
            "monitor_count": snapshot.post_promotion_monitors.len(),
            "decisive_monitor_count": decisive_count,
            "stable_count": stable.len(),
            "regression_count": regression.len(),
            "stable_rate": stable_rate
        }),
    )
}

fn evaluate_post_promotion_regression_rate(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let decisive = snapshot
        .post_promotion_monitors
        .iter()
        .filter(|monitor| {
            matches!(
                monitor.status,
                LearningCapabilityEvolutionPostPromotionMonitorStatus::Stable
                    | LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected
            )
        })
        .collect::<Vec<_>>();
    if decisive.is_empty() {
        return dimension(
            "post_promotion_regression_rate",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No decisive post-promotion monitor records were available to compute a regression rate."
                .to_string(),
            monitor_refs(
                store,
                scope,
                &snapshot
                    .post_promotion_monitors
                    .iter()
                    .take(8)
                    .collect::<Vec<_>>(),
            ),
            json!({
                "monitor_count": snapshot.post_promotion_monitors.len(),
                "decisive_monitor_count": 0,
                "regression_count": 0,
                "regression_rate": null
            }),
        );
    }
    let regressions = decisive
        .iter()
        .filter(|monitor| {
            monitor.status
                == LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected
        })
        .copied()
        .collect::<Vec<_>>();
    let regression_rate = regressions.len() as f64 / decisive.len() as f64;
    let rollback_backed = regressions
        .iter()
        .filter(|monitor| monitor.rollback_recommendation_id.is_some())
        .count();
    let follow_up_backed = regressions
        .iter()
        .filter(|monitor| monitor.follow_up_candidate_id.is_some())
        .count();
    let negative_feedback = regressions
        .iter()
        .filter(|monitor| monitor.user_negative_feedback_count > 0)
        .count();
    let status = if regression_rate <= 0.1 {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    dimension(
        "post_promotion_regression_rate",
        status,
        1.0 - regression_rate,
        format!(
            "{} of {} decisive post-promotion monitor(s) detected regression ({:.0}%).",
            regressions.len(),
            decisive.len(),
            regression_rate * 100.0
        ),
        monitor_refs(store, scope, &regressions),
        json!({
            "monitor_count": snapshot.post_promotion_monitors.len(),
            "decisive_monitor_count": decisive.len(),
            "regression_count": regressions.len(),
            "regression_rate": regression_rate,
            "rollback_backed_regression_count": rollback_backed,
            "follow_up_backed_regression_count": follow_up_backed,
            "negative_feedback_regression_count": negative_feedback
        }),
    )
}

fn evaluate_rollback_followup_rate(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let regressions = snapshot
        .post_promotion_monitors
        .iter()
        .filter(|monitor| {
            monitor.status
                == LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected
        })
        .collect::<Vec<_>>();
    let rollback_ids = snapshot
        .rollback_recommendations
        .iter()
        .map(|recommendation| recommendation.id.as_str())
        .collect::<BTreeSet<_>>();
    let handled_regressions = regressions
        .iter()
        .copied()
        .filter(|monitor| {
            monitor.follow_up_candidate_id.is_some()
                || monitor
                    .rollback_recommendation_id
                    .as_deref()
                    .is_some_and(|id| rollback_ids.contains(id))
        })
        .collect::<Vec<_>>();
    let unhandled_regressions = regressions
        .iter()
        .copied()
        .filter(|monitor| {
            monitor.follow_up_candidate_id.is_none()
                && monitor
                    .rollback_recommendation_id
                    .as_deref()
                    .is_none_or(|id| !rollback_ids.contains(id))
        })
        .collect::<Vec<_>>();
    let missing_rollback_links = regressions
        .iter()
        .copied()
        .filter(|monitor| {
            monitor
                .rollback_recommendation_id
                .as_deref()
                .is_some_and(|id| !rollback_ids.contains(id))
        })
        .collect::<Vec<_>>();
    if regressions.is_empty() && snapshot.rollback_recommendations.is_empty() {
        return dimension(
            "rollback_followup_rate",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No regression monitors or rollback recommendations were available.".to_string(),
            Vec::new(),
            json!({
                "regression_count": 0,
                "rollback_recommendation_count": 0
            }),
        );
    }
    let followup_rate = if regressions.is_empty() {
        1.0
    } else {
        handled_regressions.len() as f64 / regressions.len() as f64
    };
    let open_recommendations = snapshot
        .rollback_recommendations
        .iter()
        .filter(|recommendation| {
            recommendation.status
                == LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended
        })
        .collect::<Vec<_>>();
    let dismissed_or_superseded = snapshot
        .rollback_recommendations
        .iter()
        .filter(|recommendation| {
            matches!(
                recommendation.status,
                LearningCapabilityEvolutionRollbackRecommendationStatus::Dismissed
                    | LearningCapabilityEvolutionRollbackRecommendationStatus::Superseded
            )
        })
        .collect::<Vec<_>>();
    let status = if unhandled_regressions.is_empty() && missing_rollback_links.is_empty() {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    dimension(
        "rollback_followup_rate",
        status,
        followup_rate,
        format!(
            "{} of {} regression monitor(s) have rollback or follow-up records.",
            handled_regressions.len(),
            regressions.len()
        ),
        monitor_refs(store, scope, &unhandled_regressions)
            .into_iter()
            .chain(monitor_refs(store, scope, &missing_rollback_links))
            .chain(rollback_recommendation_refs(
                store,
                scope,
                &snapshot.rollback_recommendations.iter().collect::<Vec<_>>(),
            ))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "regression_count": regressions.len(),
            "handled_regression_count": handled_regressions.len(),
            "unhandled_regression_count": unhandled_regressions.len(),
            "missing_rollback_link_count": missing_rollback_links.len(),
            "rollback_recommendation_count": snapshot.rollback_recommendations.len(),
            "open_rollback_recommendation_count": open_recommendations.len(),
            "dismissed_or_superseded_rollback_recommendation_count": dismissed_or_superseded.len(),
            "followup_rate": followup_rate
        }),
    )
}

fn evaluate_autonomous_program_progress(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let program_candidates = snapshot
        .candidates
        .iter()
        .filter(|candidate| candidate.candidate_type == LearningCandidateType::ProgramStateUpdate)
        .collect::<Vec<_>>();
    let applied_events = snapshot
        .events
        .iter()
        .filter(|event| event.event_type == "learning_program_state_candidate_applied")
        .collect::<Vec<_>>();
    if snapshot.program_state_count == 0 && program_candidates.is_empty() {
        return dimension(
            "autonomous_program_progress",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No OPC runtime state or program-state learning candidates were found.".to_string(),
            Vec::new(),
            json!({ "program_state_count": 0, "program_candidate_count": 0 }),
        );
    }
    let status = if snapshot.program_state_count > 0 && !applied_events.is_empty() {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Blocked
    };
    let score = if status == LearningEvaluationRunStatus::Passed {
        1.0
    } else {
        0.5
    };
    dimension(
        "autonomous_program_progress",
        status,
        score,
        format!(
            "{} runtime state file(s), {} program-state candidate(s), {} applied event(s).",
            snapshot.program_state_count,
            program_candidates.len(),
            applied_events.len()
        ),
        candidate_refs(store, scope, &program_candidates)
            .into_iter()
            .chain(event_refs(&applied_events))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "program_state_count": snapshot.program_state_count,
            "program_candidate_count": program_candidates.len(),
            "applied_event_count": applied_events.len()
        }),
    )
}

fn evaluate_user_feedback_incorporation(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let teaching = snapshot
        .events
        .iter()
        .filter(|event| event.event_type == "learning_user_teaching_recorded")
        .collect::<Vec<_>>();
    let teaching_event_ids = teaching
        .iter()
        .map(|event| event.id.as_str())
        .collect::<Vec<_>>();
    let teaching_candidates = snapshot
        .candidates
        .iter()
        .filter(|candidate| candidate_is_from_teaching(candidate, &teaching_event_ids))
        .collect::<Vec<_>>();
    if teaching.is_empty() {
        return dimension(
            "user_feedback_incorporation",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No explicit user teaching events were found.".to_string(),
            Vec::new(),
            json!({ "teaching_event_count": 0 }),
        );
    }
    let status = if teaching_candidates.is_empty() {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    let score = if status == LearningEvaluationRunStatus::Passed {
        1.0
    } else {
        0.5
    };
    dimension(
        "user_feedback_incorporation",
        status,
        score,
        format!(
            "{} teaching event(s) and {} related candidate(s) found.",
            teaching.len(),
            teaching_candidates.len()
        ),
        event_refs(&teaching)
            .into_iter()
            .chain(candidate_refs(store, scope, &teaching_candidates))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "teaching_event_count": teaching.len(),
            "teaching_candidate_count": teaching_candidates.len()
        }),
    )
}

fn evaluate_false_learning_prevention(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let rejected_or_superseded = snapshot
        .candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.state,
                LearningCandidateState::Rejected | LearningCandidateState::Superseded
            )
        })
        .collect::<Vec<_>>();
    let unreviewed_risky_promoted = snapshot
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.state == LearningCandidateState::Promoted
                && matches!(
                    candidate.risk_level,
                    LearningRiskLevel::High | LearningRiskLevel::Critical
                )
                && !has_approval_or_promotion_decision(store, scope, &candidate.id)
        })
        .collect::<Vec<_>>();
    if !unreviewed_risky_promoted.is_empty() {
        return dimension(
            "false_learning_prevention",
            LearningEvaluationRunStatus::Failed,
            0.0,
            format!(
                "{} review-required high/critical candidate(s) are promoted; inspect decision logs.",
                unreviewed_risky_promoted.len()
            ),
            candidate_refs(store, scope, &unreviewed_risky_promoted),
            json!({
                "rejected_or_superseded_count": rejected_or_superseded.len(),
                "review_required_risky_promoted_count": unreviewed_risky_promoted.len()
            }),
        );
    }
    if rejected_or_superseded.is_empty()
        && !snapshot.candidates.iter().any(|candidate| {
            matches!(
                candidate.risk_level,
                LearningRiskLevel::High | LearningRiskLevel::Critical
            )
        })
    {
        return dimension(
            "false_learning_prevention",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No rejected/superseded or high-risk candidates were found to test false-learning prevention."
                .to_string(),
            Vec::new(),
            json!({ "rejected_or_superseded_count": 0 }),
        );
    }
    dimension(
        "false_learning_prevention",
        LearningEvaluationRunStatus::Passed,
        1.0,
        format!(
            "{} rejected/superseded candidate(s); no review-required high/critical promotion detected.",
            rejected_or_superseded.len()
        ),
        candidate_refs(store, scope, &rejected_or_superseded),
        json!({
            "rejected_or_superseded_count": rejected_or_superseded.len(),
            "review_required_risky_promoted_count": 0
        }),
    )
}

fn evaluate_prompt_token_growth(
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let analytics = &snapshot.llm_analytics;
    if let Some(error) = analytics.query_error.as_deref() {
        return dimension(
            "prompt_token_growth",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            format!("Prompt-token growth could not be measured from llm_calls: {error}"),
            Vec::new(),
            json!({ "query_error": error }),
        );
    }
    if analytics.call_count < 4 {
        return dimension(
            "prompt_token_growth",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "Not enough LLM-call telemetry exists to compare prompt-token growth.".to_string(),
            Vec::new(),
            json!({ "call_count": analytics.call_count }),
        );
    }
    let previous = analytics.previous_avg_total_tokens.unwrap_or(0.0);
    let recent = analytics.recent_avg_total_tokens.unwrap_or(0.0);
    let ratio = if previous > 0.0 {
        Some(recent / previous)
    } else {
        None
    };
    let runaway = ratio.is_some_and(|value| value > 2.0) && recent > 120_000.0;
    dimension(
        "prompt_token_growth",
        if runaway {
            LearningEvaluationRunStatus::Failed
        } else {
            LearningEvaluationRunStatus::Passed
        },
        if runaway { 0.0 } else { 1.0 },
        format!(
            "LLM telemetry has {} calls; average total tokens {:.0}, recent/previous ratio {}.",
            analytics.call_count,
            analytics.avg_total_tokens.unwrap_or(0.0),
            ratio
                .map(|value| format!("{value:.2}x"))
                .unwrap_or_else(|| "n/a".to_string())
        ),
        Vec::new(),
        json!({
            "call_count": analytics.call_count,
            "avg_total_tokens": analytics.avg_total_tokens,
            "max_total_tokens": analytics.max_total_tokens,
            "previous_avg_total_tokens": analytics.previous_avg_total_tokens,
            "recent_avg_total_tokens": analytics.recent_avg_total_tokens,
            "recent_to_previous_ratio": ratio
        }),
    )
}

fn evaluate_tool_failure_reduction(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let tool_fix_candidates = snapshot
        .candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.candidate_type,
                LearningCandidateType::ToolSchemaUpdate | LearningCandidateType::ToolWrapperFix
            )
        })
        .collect::<Vec<_>>();
    let passed = passed_validations(snapshot);
    let promotions = &snapshot.capability_promotions;
    if tool_fix_candidates.is_empty() {
        return dimension(
            "tool_failure_reduction",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No tool schema/wrapper-fix candidates were found.".to_string(),
            Vec::new(),
            json!({ "tool_fix_candidate_count": 0 }),
        );
    }
    let status = if passed.is_empty() && promotions.is_empty() {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    let score = if status == LearningEvaluationRunStatus::Passed {
        1.0
    } else {
        0.5
    };
    dimension(
        "tool_failure_reduction",
        status,
        score,
        format!(
            "{} tool-fix candidate(s), {} passing validation(s), {} promotion(s).",
            tool_fix_candidates.len(),
            passed.len(),
            promotions.len()
        ),
        candidate_refs(store, scope, &tool_fix_candidates)
            .into_iter()
            .chain(validation_refs(store, scope, &passed))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "tool_fix_candidate_count": tool_fix_candidates.len(),
            "passed_validation_count": passed.len(),
            "promotion_count": promotions.len()
        }),
    )
}

fn evaluate_procedure_extraction_precision(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let procedure_candidates = procedure_candidates(snapshot);
    let reviewable_procedures = snapshot
        .procedures
        .iter()
        .filter(|procedure| {
            matches!(
                procedure.status,
                LearningProcedureStatus::Draft | LearningProcedureStatus::Active
            )
        })
        .collect::<Vec<_>>();
    if procedure_candidates.is_empty() && reviewable_procedures.is_empty() {
        return dimension(
            "procedure_extraction_precision",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No procedure candidates or draft/active procedure records were found.".to_string(),
            Vec::new(),
            json!({ "procedure_candidate_count": 0, "reviewable_procedure_count": 0 }),
        );
    }
    if reviewable_procedures.is_empty() {
        return dimension(
            "procedure_extraction_precision",
            LearningEvaluationRunStatus::Blocked,
            0.5,
            format!(
                "{} procedure candidate(s) exist, but none have been routed into draft/active procedures yet.",
                procedure_candidates.len()
            ),
            candidate_refs(store, scope, &procedure_candidates),
            json!({
                "procedure_candidate_count": procedure_candidates.len(),
                "reviewable_procedure_count": 0
            }),
        );
    }

    let malformed = reviewable_procedures
        .iter()
        .copied()
        .filter(|procedure| !procedure_has_usable_shape(procedure))
        .collect::<Vec<_>>();
    let valid_count = reviewable_procedures.len().saturating_sub(malformed.len());
    let score = valid_count as f64 / reviewable_procedures.len() as f64;
    let status = if malformed.is_empty() {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    dimension(
        "procedure_extraction_precision",
        status,
        score,
        format!(
            "{} procedure candidate(s), {} draft/active procedure(s), {} malformed procedure(s).",
            procedure_candidates.len(),
            reviewable_procedures.len(),
            malformed.len()
        ),
        candidate_refs(
            store,
            scope,
            &procedure_candidates
                .iter()
                .copied()
                .take(8)
                .collect::<Vec<_>>(),
        )
        .into_iter()
        .chain(procedure_refs(
            store,
            scope,
            &malformed.iter().copied().take(8).collect::<Vec<_>>(),
        ))
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .collect(),
        json!({
            "procedure_candidate_count": procedure_candidates.len(),
            "reviewable_procedure_count": reviewable_procedures.len(),
            "malformed_procedure_count": malformed.len(),
            "valid_shape_rate": score
        }),
    )
}

fn evaluate_procedure_retrieval_relevance(
    _store: &LearningStore,
    _scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let retrieval_events = procedure_retrieval_events(snapshot);
    let all_feedback_events = procedure_feedback_events(snapshot);
    let retrieval_event_ids = retrieval_events
        .iter()
        .map(|event| event.id.as_str())
        .collect::<BTreeSet<_>>();
    let selected_procedure_ids = procedure_ids_from_retrieval_events(&retrieval_events);
    let earliest_retrieval_at = earliest_event_created_at(&retrieval_events);
    let feedback_events = all_feedback_events
        .iter()
        .copied()
        .filter(|event| {
            event_payload_str(event, "retrieval_event_id")
                .is_some_and(|event_id| retrieval_event_ids.contains(event_id))
                || (!selected_procedure_ids.is_empty()
                    && earliest_retrieval_at
                        .map(|created_at| event.created_at >= created_at)
                        .unwrap_or(true)
                    && procedure_ids_from_event(event)
                        .iter()
                        .any(|procedure_id| selected_procedure_ids.contains(procedure_id)))
        })
        .collect::<Vec<_>>();
    let stats = procedure_feedback_stats(&feedback_events);
    let selected_count = retrieval_events
        .iter()
        .map(|event| event_payload_usize(event, "selected_count"))
        .sum::<usize>();
    if retrieval_events.is_empty() {
        return dimension(
            "procedure_retrieval_relevance",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No procedure retrieval audit events were found.".to_string(),
            Vec::new(),
            json!({
                "retrieval_event_count": 0,
                "feedback_event_count": 0,
                "correlated_feedback_event_count": 0
            }),
        );
    }
    if stats.total == 0 {
        return dimension(
            "procedure_retrieval_relevance",
            LearningEvaluationRunStatus::Blocked,
            0.5,
            format!(
                "{} retrieval event(s) selected {} procedure(s), but no post-run procedure feedback has been recorded yet.",
                retrieval_events.len(),
                selected_count
            ),
            event_refs(&retrieval_events),
            json!({
                "retrieval_event_count": retrieval_events.len(),
                "selected_count": selected_count,
                "feedback_event_count": all_feedback_events.len(),
                "correlated_feedback_event_count": 0
            }),
        );
    }
    let retrieval_quality_issue_count = feedback_events
        .iter()
        .filter(|event| feedback_has_retrieval_quality_issue(event))
        .count();
    let status = if retrieval_quality_issue_count > 0 {
        LearningEvaluationRunStatus::Failed
    } else {
        LearningEvaluationRunStatus::Passed
    };
    let judged_count = stats.positive + stats.negative;
    let score = if judged_count == 0 {
        0.5
    } else {
        stats.positive as f64 / judged_count as f64
    };
    dimension(
        "procedure_retrieval_relevance",
        status,
        score,
        format!(
            "{} retrieval event(s), {} selected procedure(s), {} positive and {} negative relevance signal(s).",
            retrieval_events.len(),
            selected_count,
            stats.positive,
            stats.negative
        ),
        event_refs(&retrieval_events)
            .into_iter()
            .chain(event_refs(&feedback_events))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "retrieval_event_count": retrieval_events.len(),
            "selected_count": selected_count,
            "feedback_event_count": all_feedback_events.len(),
            "correlated_feedback_event_count": stats.total,
            "positive_signal_count": stats.positive,
            "negative_signal_count": stats.negative,
            "retrieval_quality_issue_count": retrieval_quality_issue_count,
            "relevance_score": score
        }),
    )
}

fn evaluate_procedure_misuse_rate(
    _store: &LearningStore,
    _scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let feedback_events = procedure_feedback_events(snapshot);
    let stats = procedure_feedback_stats(&feedback_events);
    if stats.total == 0 {
        return dimension(
            "procedure_misuse_rate",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No procedure feedback exists to estimate misuse.".to_string(),
            Vec::new(),
            json!({ "feedback_event_count": 0 }),
        );
    }
    let misuse_rate = stats.misuse as f64 / stats.total as f64;
    let status = if misuse_rate > 0.25 {
        LearningEvaluationRunStatus::Failed
    } else {
        LearningEvaluationRunStatus::Passed
    };
    dimension(
        "procedure_misuse_rate",
        status,
        1.0 - misuse_rate.min(1.0),
        format!(
            "{} misuse signal(s) across {} procedure feedback event(s) ({:.0}%).",
            stats.misuse,
            stats.total,
            misuse_rate * 100.0
        ),
        event_refs(&feedback_events),
        json!({
            "feedback_event_count": stats.total,
            "misuse_signal_count": stats.misuse,
            "misuse_rate": misuse_rate
        }),
    )
}

fn evaluate_procedure_helped_hurt_outcome_signal(
    _store: &LearningStore,
    _scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let feedback_events = procedure_feedback_events(snapshot);
    let stats = procedure_feedback_stats(&feedback_events);
    if feedback_events.is_empty() {
        return dimension(
            "procedure_helped_hurt_outcome_signal",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No procedure feedback events exist to judge helped/hurt outcome signals.".to_string(),
            Vec::new(),
            json!({ "feedback_event_count": 0 }),
        );
    }
    let signal_count = stats.success + stats.failure;
    if signal_count == 0 {
        return dimension(
            "procedure_helped_hurt_outcome_signal",
            LearningEvaluationRunStatus::Blocked,
            0.5,
            format!(
                "{} procedure feedback event(s) were neutral/unknown; no helped/hurt signal is measurable yet.",
                feedback_events.len()
            ),
            event_refs(&feedback_events),
            json!({
                "feedback_event_count": feedback_events.len(),
                "success_signal_count": stats.success,
                "failure_signal_count": stats.failure
            }),
        );
    }
    let status = if stats.success > stats.failure {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    let score = stats.success as f64 / signal_count as f64;
    dimension(
        "procedure_helped_hurt_outcome_signal",
        status,
        score,
        format!(
            "{} helped signal(s) and {} hurt signal(s) were recorded for reusable procedures.",
            stats.success, stats.failure
        ),
        event_refs(&feedback_events),
        json!({
            "feedback_event_count": feedback_events.len(),
            "success_signal_count": stats.success,
            "failure_signal_count": stats.failure,
            "helped_hurt_score": score
        }),
    )
}

fn evaluate_stale_procedure_correction(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let feedback_events = procedure_feedback_events(snapshot);
    let stale_feedback = feedback_events
        .iter()
        .copied()
        .filter(|event| feedback_suggests_stale_or_harmful(event))
        .collect::<Vec<_>>();
    let stale_feedback_cutoffs = procedure_feedback_cutoffs(&stale_feedback);
    let stale_procedure_ids = stale_feedback_cutoffs
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let deprecated_procedures = snapshot
        .procedures
        .iter()
        .filter(|procedure| {
            if !stale_feedback.is_empty() {
                stale_feedback_cutoffs
                    .get(&procedure.id)
                    .is_some_and(|created_at| {
                        procedure.status == LearningProcedureStatus::Deprecated
                            && procedure.updated_at >= *created_at
                    })
            } else {
                procedure.status == LearningProcedureStatus::Deprecated
            }
        })
        .collect::<Vec<_>>();
    let deprecation_events = procedure_deprecation_events(snapshot)
        .into_iter()
        .filter(|event| {
            stale_feedback_cutoffs.is_empty()
                || event_matches_feedback_cutoffs(event, &stale_feedback_cutoffs)
        })
        .collect::<Vec<_>>();
    let deprecation_candidates = procedure_deprecation_candidates(snapshot)
        .into_iter()
        .filter(|candidate| {
            stale_feedback_cutoffs.is_empty()
                || candidate_matches_feedback_cutoffs(candidate, &stale_feedback_cutoffs)
        })
        .collect::<Vec<_>>();
    if stale_feedback.is_empty()
        && deprecation_events.is_empty()
        && deprecation_candidates.is_empty()
        && deprecated_procedures.is_empty()
    {
        return dimension(
            "stale_procedure_correction",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No stale/harmful procedure feedback or deprecation evidence was found.".to_string(),
            Vec::new(),
            json!({
                "stale_feedback_count": 0,
                "deprecation_event_count": 0,
                "deprecation_candidate_count": 0,
                "deprecated_procedure_count": 0
            }),
        );
    }
    let corrected_target_ids = procedure_ids_with_correction_evidence(
        &deprecated_procedures,
        &deprecation_events,
        &deprecation_candidates,
        &stale_procedure_ids,
    );
    let uncorrected_target_ids = stale_procedure_ids
        .difference(&corrected_target_ids)
        .cloned()
        .collect::<Vec<_>>();
    let status = if !stale_feedback.is_empty()
        && (stale_procedure_ids.is_empty() || !uncorrected_target_ids.is_empty())
    {
        LearningEvaluationRunStatus::Failed
    } else {
        LearningEvaluationRunStatus::Passed
    };
    let score = if status == LearningEvaluationRunStatus::Passed {
        1.0
    } else {
        0.0
    };
    dimension(
        "stale_procedure_correction",
        status,
        score,
        format!(
            "{} stale/harmful feedback signal(s), {} deprecation event(s), {} deprecation candidate(s), {} deprecated procedure(s).",
            stale_feedback.len(),
            deprecation_events.len(),
            deprecation_candidates.len(),
            deprecated_procedures.len()
        ),
        event_refs(&stale_feedback)
            .into_iter()
            .chain(event_refs(&deprecation_events))
            .chain(candidate_refs(store, scope, &deprecation_candidates))
            .chain(procedure_refs(store, scope, &deprecated_procedures))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "stale_feedback_count": stale_feedback.len(),
            "target_procedure_id_count": stale_procedure_ids.len(),
            "corrected_target_procedure_id_count": corrected_target_ids.len(),
            "uncorrected_target_procedure_id_count": uncorrected_target_ids.len(),
            "uncorrected_target_procedure_ids": uncorrected_target_ids,
            "deprecation_event_count": deprecation_events.len(),
            "deprecation_candidate_count": deprecation_candidates.len(),
            "deprecated_procedure_count": deprecated_procedures.len()
        }),
    )
}

fn evaluate_duplicate_procedure_rate(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let active_procedures = snapshot
        .procedures
        .iter()
        .filter(|procedure| procedure.status == LearningProcedureStatus::Active)
        .collect::<Vec<_>>();
    if active_procedures.is_empty() {
        return dimension(
            "duplicate_procedure_rate",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No active procedures were found to check for duplicates.".to_string(),
            Vec::new(),
            json!({ "active_procedure_count": 0 }),
        );
    }
    let duplicate_groups = duplicate_procedure_groups(&active_procedures);
    let duplicate_procedures = duplicate_groups
        .values()
        .flat_map(|procedures| procedures.iter().copied())
        .collect::<Vec<_>>();
    let duplicate_count = duplicate_procedures
        .len()
        .saturating_sub(duplicate_groups.len());
    let duplicate_rate = duplicate_count as f64 / active_procedures.len() as f64;
    let status = if duplicate_count == 0 {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    dimension(
        "duplicate_procedure_rate",
        status,
        1.0 - duplicate_rate.min(1.0),
        format!(
            "{} duplicate active procedure(s) across {} active procedure(s).",
            duplicate_count,
            active_procedures.len()
        ),
        procedure_refs(store, scope, &duplicate_procedures),
        json!({
            "active_procedure_count": active_procedures.len(),
            "duplicate_group_count": duplicate_groups.len(),
            "duplicate_procedure_count": duplicate_count,
            "duplicate_rate": duplicate_rate
        }),
    )
}

fn evaluate_procedure_to_skill_promotion_quality(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationDimensionReport {
    let promotion_candidates = procedure_skill_promotion_candidates(snapshot);
    let promotion_events = procedure_skill_promotion_events(snapshot);
    let promotion_payload_procedures = snapshot
        .procedures
        .iter()
        .filter(|procedure| !procedure_skill_promotion_entries(procedure).is_empty())
        .collect::<Vec<_>>();
    let expected_candidate_ids = expected_procedure_skill_promotion_candidate_ids(
        &promotion_candidates,
        &promotion_events,
        &promotion_payload_procedures,
    );
    if promotion_candidates.is_empty()
        && promotion_events.is_empty()
        && promotion_payload_procedures.is_empty()
    {
        return dimension(
            "procedure_to_skill_promotion_quality",
            LearningEvaluationRunStatus::Blocked,
            0.0,
            "No procedure-to-skill promotion evidence was found.".to_string(),
            Vec::new(),
            json!({
                "promotion_candidate_count": 0,
                "promotion_event_count": 0,
                "procedure_payload_promotion_count": 0
            }),
        );
    }

    let under_evidenced_procedures = promotion_payload_procedures
        .iter()
        .copied()
        .filter(|procedure| !procedure_has_skill_promotion_evidence(procedure))
        .collect::<Vec<_>>();
    let promotion_events_without_candidate_id = promotion_events
        .iter()
        .filter(|event| promotion_candidate_id_from_event(event).is_none())
        .count();
    let promotion_payload_entries_without_candidate_id = promotion_payload_procedures
        .iter()
        .flat_map(|procedure| procedure_skill_promotion_entries(procedure))
        .filter(|entry| promotion_candidate_id_from_payload_entry(entry).is_none())
        .count();
    let mut existing_promotion_candidates = Vec::new();
    let mut missing_candidate_ids = Vec::new();
    let mut malformed_candidate_ids = Vec::new();
    let mut missing_capability_backlog_ids = Vec::new();
    let mut missing_eval_backlog_ids = Vec::new();
    for candidate_id in &expected_candidate_ids {
        match store.read_candidate(scope, candidate_id) {
            Ok(candidate) => {
                if !candidate.candidate_type.is_skill_or_workflow_candidate()
                    || candidate_source_procedure_id(&candidate).is_none()
                {
                    malformed_candidate_ids.push(candidate_id.clone());
                }
                existing_promotion_candidates.push(candidate);
            },
            Err(error) if store.error_is_not_found(&error) => {
                missing_candidate_ids.push(candidate_id.clone());
            },
            Err(_) => {
                missing_candidate_ids.push(candidate_id.clone());
            },
        }
        match store.read_capability_evolution_backlog_item(scope, candidate_id) {
            Ok(_) => {},
            Err(error) if store.error_is_not_found(&error) => {
                missing_capability_backlog_ids.push(candidate_id.clone());
            },
            Err(_) => {
                missing_capability_backlog_ids.push(candidate_id.clone());
            },
        }
        match store.read_evaluation_backlog_item(scope, candidate_id) {
            Ok(_) => {},
            Err(error) if store.error_is_not_found(&error) => {
                missing_eval_backlog_ids.push(candidate_id.clone());
            },
            Err(_) => {
                missing_eval_backlog_ids.push(candidate_id.clone());
            },
        }
    }
    let existing_candidate_refs = existing_promotion_candidates.iter().collect::<Vec<_>>();
    let routed_candidate_count = expected_candidate_ids
        .iter()
        .filter(|candidate_id| {
            let candidate_id = *candidate_id;
            !missing_candidate_ids.contains(candidate_id)
                && !malformed_candidate_ids.contains(candidate_id)
                && !missing_capability_backlog_ids.contains(candidate_id)
                && !missing_eval_backlog_ids.contains(candidate_id)
        })
        .count();
    let issue_count = under_evidenced_procedures.len()
        + promotion_events_without_candidate_id
        + promotion_payload_entries_without_candidate_id
        + missing_candidate_ids.len()
        + malformed_candidate_ids.len()
        + missing_capability_backlog_ids.len()
        + missing_eval_backlog_ids.len();
    let status = if issue_count == 0 {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    let score = if status == LearningEvaluationRunStatus::Passed {
        1.0
    } else {
        0.0
    };
    dimension(
        "procedure_to_skill_promotion_quality",
        status,
        score,
        format!(
            "{} procedure skill-promotion candidate(s), {} routed through both capability and eval backlog, {} quality issue(s).",
            promotion_candidates.len(),
            routed_candidate_count,
            issue_count
        ),
        candidate_refs(store, scope, &existing_candidate_refs)
            .into_iter()
            .chain(procedure_refs(store, scope, &under_evidenced_procedures))
            .chain(event_refs(&promotion_events))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "promotion_candidate_count": existing_promotion_candidates.len(),
            "expected_promotion_candidate_count": expected_candidate_ids.len(),
            "promotion_event_count": promotion_events.len(),
            "procedure_payload_promotion_count": promotion_payload_procedures.len(),
            "routed_candidate_count": routed_candidate_count,
            "missing_candidate_count": missing_candidate_ids.len(),
            "malformed_candidate_count": malformed_candidate_ids.len(),
            "missing_capability_backlog_count": missing_capability_backlog_ids.len(),
            "missing_eval_backlog_count": missing_eval_backlog_ids.len(),
            "promotion_events_without_candidate_id_count": promotion_events_without_candidate_id,
            "promotion_payload_entries_without_candidate_id_count": promotion_payload_entries_without_candidate_id,
            "missing_candidate_ids": missing_candidate_ids,
            "malformed_candidate_ids": malformed_candidate_ids,
            "missing_capability_backlog_ids": missing_capability_backlog_ids,
            "missing_eval_backlog_ids": missing_eval_backlog_ids,
            "under_evidenced_source_procedure_count": under_evidenced_procedures.len()
        }),
    )
}

fn evaluate_scenarios(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> Vec<LearningGrowthEvaluationScenarioReport> {
    vec![
        scenario_from_candidates(
            store,
            scope,
            "user_preference_learned_and_reused",
            snapshot
                .candidates
                .iter()
                .filter(|candidate| {
                    candidate.candidate_type == LearningCandidateType::MemoryPreference
                        && candidate.state == LearningCandidateState::Promoted
                })
                .collect(),
            "Promoted memory preference evidence exists.",
            "No promoted user-preference memory candidate was found.",
        ),
        scenario_from_counts(
            "stale_fact_corrected",
            teaching_events(snapshot, &["correct", "forget"]).len(),
            "Correction/forget teaching evidence exists.",
            "No correction/forget teaching evidence was found.",
        ),
        scenario_from_candidates(
            store,
            scope,
            "repeated_browser_failure_produces_capability_candidate",
            snapshot
                .candidates
                .iter()
                .filter(|candidate| {
                    candidate.candidate_type.is_capability_evolution_candidate()
                        && candidate_text(candidate).contains("browser")
                })
                .collect(),
            "Browser-related capability/tool candidate evidence exists.",
            "No browser-related capability/tool candidate was found.",
        ),
        scenario_from_candidates(
            store,
            scope,
            "repeated_data_workflow_becomes_skill_candidate",
            snapshot
                .candidates
                .iter()
                .filter(|candidate| candidate.candidate_type.is_skill_or_workflow_candidate())
                .collect(),
            "Reusable workflow/skill candidate evidence exists.",
            "No reusable workflow/skill candidate was found.",
        ),
        tool_wrapper_fix_eval_scenario(store, scope, snapshot),
        scenario_from_counts(
            "program_md_advances_over_multiple_runs",
            snapshot.program_state_count,
            "OPC runtime state file evidence exists.",
            "No OPC runtime state file was found.",
        ),
        forget_memory_stopped_rendering_scenario(store, scope, snapshot),
        scenario_from_candidates(
            store,
            scope,
            "candidate_rejected_does_not_affect_future_behavior",
            snapshot
                .candidates
                .iter()
                .filter(|candidate| candidate.state == LearningCandidateState::Rejected)
                .collect(),
            "Rejected candidate evidence exists.",
            "No rejected candidate evidence was found.",
        ),
        explicit_workflow_teaching_reused_later_scenario(store, scope, snapshot),
        repeated_successful_workflow_becomes_one_active_procedure_scenario(store, scope, snapshot),
        irrelevant_procedure_withheld_despite_keyword_overlap_scenario(snapshot),
        procedure_updated_after_user_correction_scenario(store, scope, snapshot),
        harmful_or_stale_procedure_deprecated_scenario(store, scope, snapshot),
        active_procedure_graduates_to_skill_evolution_after_evidence_scenario(
            store, scope, snapshot,
        ),
    ]
}

fn tool_wrapper_fix_eval_scenario(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationScenarioReport {
    let fix_candidates = snapshot
        .candidates
        .iter()
        .filter(|candidate| candidate.candidate_type == LearningCandidateType::ToolWrapperFix)
        .collect::<Vec<_>>();
    if fix_candidates.is_empty() {
        return scenario(
            "tool_wrapper_bug_produces_fix_candidate_and_eval",
            LearningEvaluationRunStatus::Blocked,
            "No tool-wrapper fix candidate was found.".to_string(),
            Vec::new(),
            json!({ "tool_wrapper_fix_candidate_count": 0 }),
        );
    }
    let eval_evidence_count = fix_candidates
        .iter()
        .filter(|candidate| {
            snapshot
                .eval_backlog
                .iter()
                .any(|item| item.candidate_id == candidate.id)
                || snapshot
                    .eval_runs
                    .iter()
                    .any(|run| run.candidate_id == candidate.id)
        })
        .count();
    let status = if eval_evidence_count == 0 {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    scenario(
        "tool_wrapper_bug_produces_fix_candidate_and_eval",
        status,
        format!(
            "{} tool-wrapper fix candidate(s); {} have eval backlog/run evidence.",
            fix_candidates.len(),
            eval_evidence_count
        ),
        candidate_refs(store, scope, &fix_candidates),
        json!({
            "tool_wrapper_fix_candidate_count": fix_candidates.len(),
            "candidate_with_eval_evidence_count": eval_evidence_count
        }),
    )
}

fn forget_memory_stopped_rendering_scenario(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationScenarioReport {
    let forget_events = teaching_events(snapshot, &["forget"]);
    let promoted_removals = snapshot
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.candidate_type.is_memory_candidate()
                && candidate.state == LearningCandidateState::Promoted
                && memory_operation(candidate).as_deref() == Some("remove")
        })
        .collect::<Vec<_>>();
    if forget_events.is_empty() {
        return scenario(
            "forget_this_stops_memory_rendering",
            LearningEvaluationRunStatus::Blocked,
            "No explicit forget teaching evidence was found.".to_string(),
            Vec::new(),
            json!({ "forget_event_count": 0, "promoted_removal_count": 0 }),
        );
    }
    let status = if promoted_removals.is_empty() {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    scenario(
        "forget_this_stops_memory_rendering",
        status,
        format!(
            "{} forget event(s); {} promoted memory removal candidate(s).",
            forget_events.len(),
            promoted_removals.len()
        ),
        event_refs(&forget_events)
            .into_iter()
            .chain(candidate_refs(store, scope, &promoted_removals))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "forget_event_count": forget_events.len(),
            "promoted_removal_count": promoted_removals.len()
        }),
    )
}

fn explicit_workflow_teaching_reused_later_scenario(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationScenarioReport {
    let reusable_teaching = teaching_events(snapshot, &["make_reusable", "this_was_useful"]);
    let reused_procedures = snapshot
        .procedures
        .iter()
        .filter(|procedure| {
            procedure.status == LearningProcedureStatus::Active
                && (procedure.success_count > 0 || procedure.last_used_at.is_some())
        })
        .collect::<Vec<_>>();
    if reusable_teaching.is_empty() && reused_procedures.is_empty() {
        return scenario(
            "explicit_workflow_teaching_reused_later",
            LearningEvaluationRunStatus::Blocked,
            "No explicit reusable-workflow teaching or reused active procedure evidence was found."
                .to_string(),
            Vec::new(),
            json!({ "teaching_event_count": 0, "reused_procedure_count": 0 }),
        );
    }
    let status = if reusable_teaching.is_empty() || reused_procedures.is_empty() {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    scenario(
        "explicit_workflow_teaching_reused_later",
        status,
        format!(
            "{} reusable teaching event(s); {} active procedure(s) show later use.",
            reusable_teaching.len(),
            reused_procedures.len()
        ),
        event_refs(&reusable_teaching)
            .into_iter()
            .chain(procedure_refs(store, scope, &reused_procedures))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "teaching_event_count": reusable_teaching.len(),
            "reused_procedure_count": reused_procedures.len()
        }),
    )
}

fn repeated_successful_workflow_becomes_one_active_procedure_scenario(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationScenarioReport {
    let stable_procedures = snapshot
        .procedures
        .iter()
        .filter(|procedure| {
            procedure.status == LearningProcedureStatus::Active && procedure.success_count >= 2
        })
        .collect::<Vec<_>>();
    if stable_procedures.is_empty() {
        return scenario(
            "repeated_successful_workflow_becomes_one_active_procedure",
            LearningEvaluationRunStatus::Blocked,
            "No active procedure has repeated success evidence yet.".to_string(),
            Vec::new(),
            json!({ "stable_active_procedure_count": 0 }),
        );
    }
    let duplicate_groups = duplicate_procedure_groups(&stable_procedures);
    let status = if duplicate_groups.is_empty() {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    scenario(
        "repeated_successful_workflow_becomes_one_active_procedure",
        status,
        format!(
            "{} active procedure(s) have repeated success; {} duplicate signature group(s).",
            stable_procedures.len(),
            duplicate_groups.len()
        ),
        procedure_refs(store, scope, &stable_procedures),
        json!({
            "stable_active_procedure_count": stable_procedures.len(),
            "duplicate_group_count": duplicate_groups.len()
        }),
    )
}

fn irrelevant_procedure_withheld_despite_keyword_overlap_scenario(
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationScenarioReport {
    let retrieval_events = procedure_retrieval_events(snapshot);
    let withheld_events = retrieval_events
        .iter()
        .copied()
        .filter(|event| {
            event_payload_usize(event, "candidate_count") > 0
                && event_payload_usize(event, "selected_count") == 0
        })
        .collect::<Vec<_>>();
    let irrelevant_feedback = procedure_feedback_events(snapshot)
        .into_iter()
        .filter(|event| feedback_verdict(event) == Some("irrelevant"))
        .collect::<Vec<_>>();
    let latest_irrelevant_feedback_at = latest_event_created_at(&irrelevant_feedback);
    let correlated_withheld_events = match latest_irrelevant_feedback_at {
        Some(latest_feedback_at) => withheld_events
            .iter()
            .copied()
            .filter(|event| event.created_at > latest_feedback_at)
            .collect::<Vec<_>>(),
        None => withheld_events.clone(),
    };
    if retrieval_events.is_empty() && irrelevant_feedback.is_empty() {
        return scenario(
            "irrelevant_procedure_withheld_despite_keyword_overlap",
            LearningEvaluationRunStatus::Blocked,
            "No procedure retrieval or irrelevant-feedback evidence was found.".to_string(),
            Vec::new(),
            json!({ "withheld_retrieval_count": 0, "irrelevant_feedback_count": 0 }),
        );
    }
    let status = if correlated_withheld_events.is_empty() && !irrelevant_feedback.is_empty() {
        LearningEvaluationRunStatus::Failed
    } else if correlated_withheld_events.is_empty() {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    scenario(
        "irrelevant_procedure_withheld_despite_keyword_overlap",
        status,
        format!(
            "{} retrieval event(s) withheld all procedure candidates; {} irrelevant feedback signal(s).",
            correlated_withheld_events.len(),
            irrelevant_feedback.len()
        ),
        event_refs(&correlated_withheld_events)
            .into_iter()
            .chain(event_refs(&irrelevant_feedback))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "retrieval_event_count": retrieval_events.len(),
            "withheld_retrieval_count": withheld_events.len(),
            "correlated_withheld_retrieval_count": correlated_withheld_events.len(),
            "irrelevant_feedback_count": irrelevant_feedback.len()
        }),
    )
}

fn procedure_updated_after_user_correction_scenario(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationScenarioReport {
    let correction_feedback = procedure_feedback_events(snapshot)
        .into_iter()
        .filter(|event| {
            event_payload_bool(event, "explicit_negative_feedback")
                || event_payload_str(event, "outcome") == Some("explicit_negative_correction")
                || matches!(
                    feedback_verdict(event),
                    Some("too_broad" | "too_narrow" | "misleading" | "irrelevant")
                )
        })
        .collect::<Vec<_>>();
    let correction_feedback_cutoffs = procedure_feedback_cutoffs(&correction_feedback);
    let corrected_procedure_ids = correction_feedback_cutoffs
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if correction_feedback.is_empty() {
        return scenario(
            "procedure_updated_after_user_correction",
            LearningEvaluationRunStatus::Blocked,
            "No procedure correction feedback was found.".to_string(),
            Vec::new(),
            json!({ "correction_feedback_count": 0, "correction_candidate_count": 0 }),
        );
    }
    if corrected_procedure_ids.is_empty() {
        return scenario(
            "procedure_updated_after_user_correction",
            LearningEvaluationRunStatus::Failed,
            "Procedure correction feedback did not identify a target procedure.".to_string(),
            event_refs(&correction_feedback),
            json!({
                "correction_feedback_count": correction_feedback.len(),
                "target_procedure_id_count": 0,
                "correction_candidate_count": 0
            }),
        );
    }
    let deprecated_procedures = snapshot
        .procedures
        .iter()
        .filter(|procedure| {
            correction_feedback_cutoffs
                .get(&procedure.id)
                .is_some_and(|created_at| {
                    procedure.status == LearningProcedureStatus::Deprecated
                        && procedure.updated_at >= *created_at
                })
        })
        .collect::<Vec<_>>();
    let correction_candidates = procedure_update_candidates(snapshot)
        .into_iter()
        .filter(|candidate| {
            candidate_matches_feedback_cutoffs(candidate, &correction_feedback_cutoffs)
        })
        .collect::<Vec<_>>();
    let deprecation_events = procedure_deprecation_events(snapshot)
        .into_iter()
        .filter(|event| event_matches_feedback_cutoffs(event, &correction_feedback_cutoffs))
        .collect::<Vec<_>>();
    let corrected_target_ids = procedure_ids_with_correction_evidence(
        &deprecated_procedures,
        &deprecation_events,
        &correction_candidates,
        &corrected_procedure_ids,
    );
    let uncorrected_target_ids = corrected_procedure_ids
        .difference(&corrected_target_ids)
        .cloned()
        .collect::<Vec<_>>();
    let status = if !uncorrected_target_ids.is_empty() {
        LearningEvaluationRunStatus::Failed
    } else {
        LearningEvaluationRunStatus::Passed
    };
    scenario(
        "procedure_updated_after_user_correction",
        status,
        format!(
            "{} correction feedback signal(s), {} deprecated procedure(s), {} deprecation event(s), {} correction candidate(s).",
            correction_feedback.len(),
            deprecated_procedures.len(),
            deprecation_events.len(),
            correction_candidates.len()
        ),
        event_refs(&correction_feedback)
            .into_iter()
            .chain(event_refs(&deprecation_events))
            .chain(procedure_refs(store, scope, &deprecated_procedures))
            .chain(candidate_refs(store, scope, &correction_candidates))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "correction_feedback_count": correction_feedback.len(),
            "target_procedure_id_count": corrected_procedure_ids.len(),
            "corrected_target_procedure_id_count": corrected_target_ids.len(),
            "uncorrected_target_procedure_id_count": uncorrected_target_ids.len(),
            "uncorrected_target_procedure_ids": uncorrected_target_ids,
            "deprecated_procedure_count": deprecated_procedures.len(),
            "deprecation_event_count": deprecation_events.len(),
            "correction_candidate_count": correction_candidates.len()
        }),
    )
}

fn harmful_or_stale_procedure_deprecated_scenario(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationScenarioReport {
    let stale_feedback = procedure_feedback_events(snapshot)
        .into_iter()
        .filter(|event| feedback_suggests_stale_or_harmful(event))
        .collect::<Vec<_>>();
    let stale_feedback_cutoffs = procedure_feedback_cutoffs(&stale_feedback);
    let stale_procedure_ids = stale_feedback_cutoffs
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let deprecated_procedures = snapshot
        .procedures
        .iter()
        .filter(|procedure| {
            if !stale_feedback.is_empty() {
                stale_feedback_cutoffs
                    .get(&procedure.id)
                    .is_some_and(|created_at| {
                        procedure.status == LearningProcedureStatus::Deprecated
                            && procedure.updated_at >= *created_at
                    })
            } else {
                procedure.status == LearningProcedureStatus::Deprecated
            }
        })
        .collect::<Vec<_>>();
    let deprecation_events = procedure_deprecation_events(snapshot)
        .into_iter()
        .filter(|event| {
            stale_feedback_cutoffs.is_empty()
                || event_matches_feedback_cutoffs(event, &stale_feedback_cutoffs)
        })
        .collect::<Vec<_>>();
    if stale_feedback.is_empty()
        && deprecation_events.is_empty()
        && deprecated_procedures.is_empty()
    {
        return scenario(
            "harmful_or_stale_procedure_deprecated",
            LearningEvaluationRunStatus::Blocked,
            "No harmful/stale feedback or deprecated procedure evidence was found.".to_string(),
            Vec::new(),
            json!({ "stale_feedback_count": 0, "deprecated_procedure_count": 0 }),
        );
    }
    let corrected_target_ids = procedure_ids_with_correction_evidence(
        &deprecated_procedures,
        &deprecation_events,
        &[],
        &stale_procedure_ids,
    );
    let uncorrected_target_ids = stale_procedure_ids
        .difference(&corrected_target_ids)
        .cloned()
        .collect::<Vec<_>>();
    let status = if !stale_feedback.is_empty()
        && (stale_procedure_ids.is_empty() || !uncorrected_target_ids.is_empty())
    {
        LearningEvaluationRunStatus::Failed
    } else {
        LearningEvaluationRunStatus::Passed
    };
    scenario(
        "harmful_or_stale_procedure_deprecated",
        status,
        format!(
            "{} harmful/stale feedback signal(s), {} deprecation event(s), {} deprecated procedure(s).",
            stale_feedback.len(),
            deprecation_events.len(),
            deprecated_procedures.len()
        ),
        event_refs(&stale_feedback)
            .into_iter()
            .chain(event_refs(&deprecation_events))
            .chain(procedure_refs(store, scope, &deprecated_procedures))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "stale_feedback_count": stale_feedback.len(),
            "target_procedure_id_count": stale_procedure_ids.len(),
            "corrected_target_procedure_id_count": corrected_target_ids.len(),
            "uncorrected_target_procedure_id_count": uncorrected_target_ids.len(),
            "uncorrected_target_procedure_ids": uncorrected_target_ids,
            "deprecation_event_count": deprecation_events.len(),
            "deprecated_procedure_count": deprecated_procedures.len()
        }),
    )
}

fn active_procedure_graduates_to_skill_evolution_after_evidence_scenario(
    store: &LearningStore,
    scope: &LearningScope,
    snapshot: &GrowthEvidenceSnapshot,
) -> LearningGrowthEvaluationScenarioReport {
    let promotion_candidates = procedure_skill_promotion_candidates(snapshot);
    let source_procedures = snapshot
        .procedures
        .iter()
        .filter(|procedure| {
            !procedure_skill_promotion_entries(procedure).is_empty()
                || promotion_candidates.iter().any(|candidate| {
                    candidate_source_procedure_id(candidate).as_deref()
                        == Some(procedure.id.as_str())
                })
        })
        .collect::<Vec<_>>();
    if promotion_candidates.is_empty() && source_procedures.is_empty() {
        return scenario(
            "active_procedure_graduates_to_skill_evolution_after_evidence",
            LearningEvaluationRunStatus::Blocked,
            "No procedure-to-skill promotion candidate or procedure payload evidence was found."
                .to_string(),
            Vec::new(),
            json!({ "promotion_candidate_count": 0, "source_procedure_count": 0 }),
        );
    }
    let under_evidenced = source_procedures
        .iter()
        .copied()
        .filter(|procedure| !procedure_has_skill_promotion_evidence(procedure))
        .collect::<Vec<_>>();
    let status = if under_evidenced.is_empty() {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    scenario(
        "active_procedure_graduates_to_skill_evolution_after_evidence",
        status,
        format!(
            "{} promotion candidate(s), {} source procedure(s), {} under-evidenced source procedure(s).",
            promotion_candidates.len(),
            source_procedures.len(),
            under_evidenced.len()
        ),
        candidate_refs(store, scope, &promotion_candidates)
            .into_iter()
            .chain(procedure_refs(store, scope, &source_procedures))
            .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
            .collect(),
        json!({
            "promotion_candidate_count": promotion_candidates.len(),
            "source_procedure_count": source_procedures.len(),
            "under_evidenced_source_procedure_count": under_evidenced.len()
        }),
    )
}

fn scenario_from_candidates(
    store: &LearningStore,
    scope: &LearningScope,
    scenario_name: &str,
    candidates: Vec<&LearningCandidate>,
    passed_summary: &str,
    blocked_summary: &str,
) -> LearningGrowthEvaluationScenarioReport {
    let status = if candidates.is_empty() {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    scenario(
        scenario_name,
        status,
        if candidates.is_empty() {
            blocked_summary.to_string()
        } else {
            format!("{passed_summary} Count: {}.", candidates.len())
        },
        candidate_refs(store, scope, &candidates),
        json!({ "candidate_count": candidates.len() }),
    )
}

fn scenario_from_counts(
    scenario_name: &str,
    count: usize,
    passed_summary: &str,
    blocked_summary: &str,
) -> LearningGrowthEvaluationScenarioReport {
    let status = if count == 0 {
        LearningEvaluationRunStatus::Blocked
    } else {
        LearningEvaluationRunStatus::Passed
    };
    scenario(
        scenario_name,
        status,
        if count == 0 {
            blocked_summary.to_string()
        } else {
            format!("{passed_summary} Count: {count}.")
        },
        Vec::new(),
        json!({ "count": count }),
    )
}

fn dimension(
    name: &str,
    status: LearningEvaluationRunStatus,
    score: f64,
    summary: String,
    evidence_refs: Vec<LearningEvidenceRef>,
    metrics: serde_json::Value,
) -> LearningGrowthEvaluationDimensionReport {
    LearningGrowthEvaluationDimensionReport {
        dimension: name.to_string(),
        status,
        score,
        summary,
        evidence_refs,
        metrics,
    }
}

fn scenario(
    name: &str,
    status: LearningEvaluationRunStatus,
    summary: String,
    evidence_refs: Vec<LearningEvidenceRef>,
    metrics: serde_json::Value,
) -> LearningGrowthEvaluationScenarioReport {
    LearningGrowthEvaluationScenarioReport {
        scenario: name.to_string(),
        status,
        summary,
        evidence_refs,
        metrics,
    }
}

fn passed_validations(
    snapshot: &GrowthEvidenceSnapshot,
) -> Vec<&LearningCapabilityEvolutionValidationReport> {
    snapshot
        .capability_validations
        .iter()
        .filter(|validation| {
            validation.status == LearningCapabilityEvolutionValidationStatus::Passed
        })
        .collect()
}

fn teaching_events<'a>(
    snapshot: &'a GrowthEvidenceSnapshot,
    actions: &[&str],
) -> Vec<&'a LearningEvent> {
    snapshot
        .events
        .iter()
        .filter(|event| {
            if event.event_type != "learning_user_teaching_recorded" {
                return false;
            }
            let action = event
                .payload
                .get("action")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            actions.contains(&action)
        })
        .collect()
}

fn candidate_is_from_teaching(candidate: &LearningCandidate, teaching_event_ids: &[&str]) -> bool {
    candidate.event_refs.iter().any(|event_ref| {
        event_ref.event_type == "learning_user_teaching_recorded"
            || teaching_event_ids.contains(&event_ref.event_id.as_str())
    }) || value_has_source(&candidate.proposed_change, "user_teaching")
        || value_has_source(&candidate.proposed_change, "learning_teaching_feedback")
        || candidate.proposed_change.get("teaching").is_some()
        || candidate
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "learning_teaching_event")
}

fn value_has_source(value: &serde_json::Value, expected: &str) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.get("source").and_then(|source| source.as_str()) == Some(expected)
                || map.values().any(|child| value_has_source(child, expected))
        },
        serde_json::Value::Array(items) => {
            items.iter().any(|child| value_has_source(child, expected))
        },
        _ => false,
    }
}

fn memory_operation(candidate: &LearningCandidate) -> Option<String> {
    candidate
        .proposed_change
        .get("memory")
        .or_else(|| candidate.proposed_change.get("proposed_memory"))
        .and_then(|memory| memory.get("operation"))
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_ascii_lowercase())
}

fn candidate_text(candidate: &LearningCandidate) -> String {
    format!(
        "{} {} {} {}",
        candidate.title, candidate.summary, candidate.rationale, candidate.proposed_change
    )
    .to_ascii_lowercase()
}

#[derive(Debug, Default)]
struct ProcedureFeedbackStats {
    total: usize,
    positive: usize,
    negative: usize,
    misuse: usize,
    success: usize,
    failure: usize,
}

fn procedure_candidates(snapshot: &GrowthEvidenceSnapshot) -> Vec<&LearningCandidate> {
    snapshot
        .candidates
        .iter()
        .filter(|candidate| candidate.candidate_type.is_procedure_candidate())
        .collect()
}

fn procedure_deprecation_candidates(snapshot: &GrowthEvidenceSnapshot) -> Vec<&LearningCandidate> {
    snapshot
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.candidate_type.is_procedure_candidate()
                && (bool_field_recursive(&candidate.proposed_change, "deprecation_recommended")
                    || candidate_text(candidate).contains("deprecat"))
        })
        .collect()
}

fn procedure_update_candidates(snapshot: &GrowthEvidenceSnapshot) -> Vec<&LearningCandidate> {
    snapshot
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.candidate_type.is_procedure_candidate()
                && (candidate_existing_procedure_id(candidate).is_some()
                    || string_field_recursive(&candidate.proposed_change, "procedure_id").is_some())
        })
        .collect()
}

fn procedure_retrieval_events(snapshot: &GrowthEvidenceSnapshot) -> Vec<&LearningEvent> {
    snapshot
        .events
        .iter()
        .filter(|event| event.event_type == "learning_procedure_retrieval_rendered")
        .collect()
}

fn procedure_feedback_events(snapshot: &GrowthEvidenceSnapshot) -> Vec<&LearningEvent> {
    snapshot
        .events
        .iter()
        .filter(|event| event.event_type == "learning_procedure_feedback_recorded")
        .collect()
}

fn procedure_deprecation_events(snapshot: &GrowthEvidenceSnapshot) -> Vec<&LearningEvent> {
    snapshot
        .events
        .iter()
        .filter(|event| event.event_type == "learning_procedure_deprecated")
        .collect()
}

fn procedure_skill_promotion_events(snapshot: &GrowthEvidenceSnapshot) -> Vec<&LearningEvent> {
    snapshot
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.event_type.as_str(),
                "learning_procedure_skill_promotion_candidate_created"
                    | "learning_procedure_skill_promotion_reconciled"
                    | "learning_procedure_skill_promotion_routed"
            )
        })
        .collect()
}

fn procedure_skill_promotion_candidates(
    snapshot: &GrowthEvidenceSnapshot,
) -> Vec<&LearningCandidate> {
    snapshot
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.candidate_type.is_skill_or_workflow_candidate()
                && candidate_source_procedure_id(candidate).is_some()
        })
        .collect()
}

fn procedure_skill_promotion_entries(procedure: &LearningProcedure) -> Vec<&Value> {
    procedure
        .payload
        .get("phase_14_skill_promotions")
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

fn expected_procedure_skill_promotion_candidate_ids(
    promotion_candidates: &[&LearningCandidate],
    promotion_events: &[&LearningEvent],
    promotion_payload_procedures: &[&LearningProcedure],
) -> BTreeSet<String> {
    let mut ids = promotion_candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<BTreeSet<_>>();
    ids.extend(
        promotion_events
            .iter()
            .filter_map(|event| promotion_candidate_id_from_event(event)),
    );
    for procedure in promotion_payload_procedures {
        ids.extend(
            procedure_skill_promotion_entries(procedure)
                .into_iter()
                .filter_map(promotion_candidate_id_from_payload_entry),
        );
    }
    ids
}

fn promotion_candidate_id_from_event(event: &LearningEvent) -> Option<String> {
    [
        "promotion_candidate_id",
        "candidate_id",
        "eval_backlog_candidate_id",
    ]
    .into_iter()
    .find_map(|key| {
        event_payload_str(event, key)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn promotion_candidate_id_from_payload_entry(entry: &Value) -> Option<String> {
    entry
        .get("promotion_candidate_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn procedure_feedback_stats(events: &[&LearningEvent]) -> ProcedureFeedbackStats {
    let mut stats = ProcedureFeedbackStats::default();
    for event in events {
        stats.total += 1;
        if feedback_is_positive(event) {
            stats.positive += 1;
        }
        if feedback_is_negative(event) {
            stats.negative += 1;
        }
        if feedback_is_misuse(event) {
            stats.misuse += 1;
        }
        if feedback_counts_success(event) {
            stats.success += 1;
        }
        if feedback_counts_failure(event) {
            stats.failure += 1;
        }
    }
    stats
}

fn procedure_has_usable_shape(procedure: &LearningProcedure) -> bool {
    !procedure.title.trim().is_empty()
        && (!procedure.activation.use_when.is_empty()
            || !procedure.activation.example_goals.is_empty())
        && procedure
            .workflow
            .iter()
            .any(|step| !step.trim().is_empty())
        && (procedure.source_candidate_id.is_some()
            || !procedure.evidence_refs.is_empty()
            || !procedure.source_task_ids.is_empty()
            || !procedure.source_chat_session_ids.is_empty())
}

fn procedure_has_skill_promotion_evidence(procedure: &LearningProcedure) -> bool {
    procedure.status == LearningProcedureStatus::Active
        && procedure.success_count >= 3
        && procedure.failure_count < procedure.success_count
        && procedure_evidence_count(procedure) >= 2
        && procedure
            .workflow
            .iter()
            .any(|step| !step.trim().is_empty())
}

fn procedure_evidence_count(procedure: &LearningProcedure) -> usize {
    let mut refs = BTreeSet::new();
    for evidence in &procedure.evidence_refs {
        if let Some(id) = evidence
            .id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            refs.insert(format!("id:{id}"));
        }
        if let Some(path) = evidence
            .path
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            refs.insert(format!("path:{path}"));
        }
    }
    refs.extend(
        procedure
            .source_task_ids
            .iter()
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("task:{value}")),
    );
    refs.extend(
        procedure
            .source_chat_session_ids
            .iter()
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("chat:{value}")),
    );
    refs.len()
}

fn duplicate_procedure_groups<'a>(
    procedures: &[&'a LearningProcedure],
) -> BTreeMap<String, Vec<&'a LearningProcedure>> {
    let mut by_signature: BTreeMap<String, Vec<&'a LearningProcedure>> = BTreeMap::new();
    for procedure in procedures {
        let signature = procedure_signature(procedure);
        if signature.is_empty() {
            continue;
        }
        by_signature.entry(signature).or_default().push(*procedure);
    }
    by_signature.retain(|_, values| values.len() > 1);
    by_signature
}

fn procedure_signature(procedure: &LearningProcedure) -> String {
    if let Some(signature) = procedure
        .payload
        .get("workflow_signature")
        .and_then(Value::as_str)
        .map(normalize_for_matching)
        .filter(|value| !value.is_empty())
    {
        return signature;
    }
    normalize_for_matching(&format!(
        "{} {} {} {}",
        procedure.owner_agent.as_deref().unwrap_or_default(),
        procedure.title,
        procedure.activation.use_when.join(" "),
        procedure.workflow.join(" ")
    ))
}

fn candidate_source_procedure_id(candidate: &LearningCandidate) -> Option<String> {
    string_field_recursive(&candidate.proposed_change, "source_procedure_id")
        .or_else(|| string_field_recursive(&candidate.promotion_policy, "source_procedure_id"))
}

fn candidate_existing_procedure_id(candidate: &LearningCandidate) -> Option<String> {
    string_field_recursive(&candidate.proposed_change, "existing_procedure_id")
}

fn candidate_matches_feedback_cutoffs(
    candidate: &LearningCandidate,
    cutoffs: &BTreeMap<String, DateTime<Utc>>,
) -> bool {
    candidate_procedure_ids(candidate)
        .iter()
        .any(|procedure_id| {
            cutoffs
                .get(procedure_id)
                .is_some_and(|created_at| candidate.created_at >= *created_at)
        })
}

fn event_matches_feedback_cutoffs(
    event: &LearningEvent,
    cutoffs: &BTreeMap<String, DateTime<Utc>>,
) -> bool {
    procedure_ids_from_event(event).iter().any(|procedure_id| {
        cutoffs
            .get(procedure_id)
            .is_some_and(|created_at| event.created_at >= *created_at)
    })
}

fn candidate_procedure_ids(candidate: &LearningCandidate) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if let Some(id) = candidate_source_procedure_id(candidate) {
        ids.insert(id);
    }
    if let Some(id) = candidate_existing_procedure_id(candidate) {
        ids.insert(id);
    }
    if let Some(id) = string_field_recursive(&candidate.proposed_change, "procedure_id") {
        ids.insert(id);
    }
    ids
}

fn procedure_ids_with_correction_evidence(
    procedures: &[&LearningProcedure],
    events: &[&LearningEvent],
    candidates: &[&LearningCandidate],
    target_ids: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    ids.extend(
        procedures
            .iter()
            .map(|procedure| procedure.id.clone())
            .filter(|procedure_id| target_ids.contains(procedure_id)),
    );
    ids.extend(events.iter().flat_map(|event| {
        procedure_ids_from_event(event)
            .into_iter()
            .filter(|procedure_id| target_ids.contains(procedure_id))
    }));
    ids.extend(candidates.iter().flat_map(|candidate| {
        candidate_procedure_ids(candidate)
            .into_iter()
            .filter(|procedure_id| target_ids.contains(procedure_id))
    }));
    ids
}

fn procedure_feedback_cutoffs(events: &[&LearningEvent]) -> BTreeMap<String, DateTime<Utc>> {
    let mut cutoffs = BTreeMap::new();
    for event in events {
        for procedure_id in procedure_ids_from_event(event) {
            cutoffs
                .entry(procedure_id)
                .and_modify(|created_at| {
                    if event.created_at < *created_at {
                        *created_at = event.created_at;
                    }
                })
                .or_insert(event.created_at);
        }
    }
    cutoffs
}

fn procedure_ids_from_event(event: &LearningEvent) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for key in [
        "procedure_id",
        "target_procedure_id",
        "existing_procedure_id",
    ] {
        if let Some(id) = event_payload_str(event, key)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            ids.insert(id.to_string());
        }
    }
    if let Some(id) = event
        .payload
        .get("procedure_judgement")
        .and_then(|value| value.get("procedure_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        ids.insert(id.to_string());
    }
    ids
}

fn procedure_ids_from_retrieval_events(events: &[&LearningEvent]) -> BTreeSet<String> {
    events
        .iter()
        .flat_map(|event| {
            event
                .payload
                .get("selected")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| item.get("procedure_id").and_then(Value::as_str))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .collect()
}

fn earliest_event_created_at(events: &[&LearningEvent]) -> Option<DateTime<Utc>> {
    events.iter().map(|event| event.created_at).min()
}

fn latest_event_created_at(events: &[&LearningEvent]) -> Option<DateTime<Utc>> {
    events.iter().map(|event| event.created_at).max()
}

fn feedback_is_positive(event: &LearningEvent) -> bool {
    event_payload_str(event, "outcome") == Some("success")
        || feedback_verdict(event) == Some("useful")
}

fn feedback_is_negative(event: &LearningEvent) -> bool {
    matches!(
        event_payload_str(event, "outcome"),
        Some("failure" | "explicit_negative_correction")
    ) || matches!(
        feedback_verdict(event),
        Some("harmful" | "stale" | "misleading" | "too_broad" | "too_narrow" | "irrelevant")
    )
}

fn feedback_is_misuse(event: &LearningEvent) -> bool {
    event_payload_bool(event, "explicit_negative_feedback")
        || matches!(
            feedback_verdict(event),
            Some("harmful" | "misleading" | "too_broad" | "too_narrow" | "irrelevant")
        )
}

fn feedback_suggests_stale_or_harmful(event: &LearningEvent) -> bool {
    matches!(
        feedback_verdict(event),
        Some("harmful" | "stale" | "misleading")
    ) || feedback_recommends_deprecation(event)
}

fn feedback_has_retrieval_quality_issue(event: &LearningEvent) -> bool {
    event_payload_bool(event, "explicit_negative_feedback")
        || event_payload_str(event, "outcome") == Some("explicit_negative_correction")
        || matches!(
            feedback_verdict(event),
            Some("harmful" | "stale" | "misleading" | "too_broad" | "too_narrow" | "irrelevant")
        )
        || feedback_recommends_deprecation(event)
}

fn feedback_recommends_deprecation(event: &LearningEvent) -> bool {
    event_payload_bool(event, "deprecation_recommended")
        || event
            .payload
            .get("procedure_judgement")
            .and_then(|value| value.get("deprecation_recommended"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn feedback_counts_success(event: &LearningEvent) -> bool {
    event_payload_str(event, "outcome") == Some("success")
}

fn feedback_counts_failure(event: &LearningEvent) -> bool {
    matches!(
        event_payload_str(event, "outcome"),
        Some("failure" | "explicit_negative_correction")
    )
}

fn feedback_verdict(event: &LearningEvent) -> Option<&str> {
    event
        .payload
        .get("procedure_judgement")
        .and_then(|value| value.get("verdict"))
        .and_then(Value::as_str)
}

fn event_payload_str<'a>(event: &'a LearningEvent, key: &str) -> Option<&'a str> {
    event.payload.get(key).and_then(Value::as_str)
}

fn event_payload_bool(event: &LearningEvent, key: &str) -> bool {
    event
        .payload
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn event_payload_usize(event: &LearningEvent, key: &str) -> usize {
    event
        .payload
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

fn string_field_recursive(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(found) = map
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Some(found.to_string());
            }
            map.values()
                .find_map(|child| string_field_recursive(child, key))
        },
        Value::Array(items) => items
            .iter()
            .find_map(|child| string_field_recursive(child, key)),
        _ => None,
    }
}

fn bool_field_recursive(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.get(key).and_then(Value::as_bool).unwrap_or(false)
                || map.values().any(|child| bool_field_recursive(child, key))
        },
        Value::Array(items) => items.iter().any(|child| bool_field_recursive(child, key)),
        _ => false,
    }
}

fn normalize_for_matching(value: &str) -> String {
    value
        .split_whitespace()
        .map(|part| part.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_approval_or_promotion_decision(
    store: &LearningStore,
    scope: &LearningScope,
    candidate_id: &str,
) -> bool {
    store
        .read_decisions(scope, candidate_id)
        .map(|decisions| {
            decisions.iter().any(|decision| {
                matches!(
                    &decision.to_state,
                    LearningCandidateState::Approved | LearningCandidateState::Promoted
                ) && decision.actor != "system"
            })
        })
        .unwrap_or(false)
}

fn collect_report_evidence(
    dimensions: &[LearningGrowthEvaluationDimensionReport],
    scenarios: &[LearningGrowthEvaluationScenarioReport],
) -> Vec<LearningEvidenceRef> {
    let mut refs = Vec::new();
    for evidence in dimensions
        .iter()
        .flat_map(|dimension| dimension.evidence_refs.iter())
        .chain(
            scenarios
                .iter()
                .flat_map(|scenario| scenario.evidence_refs.iter()),
        )
    {
        let duplicate = refs.iter().any(|existing: &LearningEvidenceRef| {
            existing.kind == evidence.kind
                && existing.id == evidence.id
                && existing.path == evidence.path
        });
        if !duplicate {
            refs.push(evidence.clone());
        }
        if refs.len() >= MAX_GROWTH_EVAL_EVIDENCE_REFS {
            break;
        }
    }
    refs
}

fn candidate_refs(
    store: &LearningStore,
    scope: &LearningScope,
    candidates: &[&LearningCandidate],
) -> Vec<LearningEvidenceRef> {
    candidates
        .iter()
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .map(|candidate| LearningEvidenceRef {
            kind: "learning_candidate".to_string(),
            id: Some(candidate.id.clone()),
            path: Some(
                store
                    .workspace_layout()
                    .learning_candidate_path(&scope.principal, &scope.workspace, &candidate.id)
                    .display()
                    .to_string(),
            ),
            uri: None,
            summary: Some(candidate.title.clone()),
        })
        .collect()
}

fn backlog_refs(
    store: &LearningStore,
    scope: &LearningScope,
    backlog: &[&LearningCapabilityEvolutionBacklogItem],
) -> Vec<LearningEvidenceRef> {
    backlog
        .iter()
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .map(|item| LearningEvidenceRef {
            kind: "learning_capability_backlog".to_string(),
            id: Some(item.id.clone()),
            path: Some({
                let workspace = store.workspace_layout();
                preferred_existing_path(
                    workspace,
                    workspace.skill_evolution_backlog_path(
                        &scope.principal,
                        &scope.workspace,
                        &item.candidate_id,
                    ),
                    workspace.capability_evolution_backlog_path(
                        &scope.principal,
                        &scope.workspace,
                        &item.candidate_id,
                    ),
                )
                .display()
                .to_string()
            }),
            uri: None,
            summary: Some(item.title.clone()),
        })
        .collect()
}

fn proposal_refs(
    store: &LearningStore,
    scope: &LearningScope,
    proposals: &[&LearningCapabilityEvolutionProposal],
) -> Vec<LearningEvidenceRef> {
    proposals
        .iter()
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .map(|proposal| LearningEvidenceRef {
            kind: "learning_capability_proposal".to_string(),
            id: Some(proposal.id.clone()),
            path: Some({
                let workspace = store.workspace_layout();
                preferred_existing_path(
                    workspace,
                    workspace.skill_evolution_proposal_path(
                        &scope.principal,
                        &scope.workspace,
                        &proposal.candidate_id,
                    ),
                    workspace.capability_evolution_proposal_path(
                        &scope.principal,
                        &scope.workspace,
                        &proposal.candidate_id,
                    ),
                )
                .display()
                .to_string()
            }),
            uri: None,
            summary: Some(proposal.title.clone()),
        })
        .collect()
}

fn procedure_refs(
    store: &LearningStore,
    scope: &LearningScope,
    procedures: &[&LearningProcedure],
) -> Vec<LearningEvidenceRef> {
    procedures
        .iter()
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .map(|procedure| LearningEvidenceRef {
            kind: "learning_procedure".to_string(),
            id: Some(procedure.id.clone()),
            path: Some(
                store
                    .workspace_layout()
                    .learning_procedure_path(
                        &scope.principal,
                        &scope.workspace,
                        procedure.status.as_str(),
                        &procedure.id,
                    )
                    .display()
                    .to_string(),
            ),
            uri: None,
            summary: Some(procedure.title.clone()),
        })
        .collect()
}

fn validation_refs(
    store: &LearningStore,
    scope: &LearningScope,
    validations: &[&LearningCapabilityEvolutionValidationReport],
) -> Vec<LearningEvidenceRef> {
    validations
        .iter()
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .map(|validation| LearningEvidenceRef {
            kind: "learning_capability_validation".to_string(),
            id: Some(validation.id.clone()),
            path: Some({
                let workspace = store.workspace_layout();
                preferred_existing_path(
                    workspace,
                    workspace.skill_evolution_validation_path(
                        &scope.principal,
                        &scope.workspace,
                        &validation.candidate_id,
                        &validation.id,
                    ),
                    workspace.capability_evolution_validation_path(
                        &scope.principal,
                        &scope.workspace,
                        &validation.candidate_id,
                        &validation.id,
                    ),
                )
                .display()
                .to_string()
            }),
            uri: None,
            summary: Some(validation.summary.clone()),
        })
        .collect()
}

fn monitor_refs(
    store: &LearningStore,
    scope: &LearningScope,
    monitors: &[&LearningCapabilityEvolutionPostPromotionMonitorRecord],
) -> Vec<LearningEvidenceRef> {
    monitors
        .iter()
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .map(|monitor| LearningEvidenceRef {
            kind: "learning_capability_post_promotion_monitor".to_string(),
            id: Some(monitor.id.clone()),
            path: Some({
                let workspace = store.workspace_layout();
                preferred_existing_path(
                    workspace,
                    workspace.skill_evolution_post_promotion_monitor_path(
                        &scope.principal,
                        &scope.workspace,
                        &monitor.promotion_id,
                    ),
                    workspace.capability_evolution_post_promotion_monitor_path(
                        &scope.principal,
                        &scope.workspace,
                        &monitor.promotion_id,
                    ),
                )
                .display()
                .to_string()
            }),
            uri: None,
            summary: Some(monitor.summary.clone()),
        })
        .collect()
}

fn rollback_recommendation_refs(
    store: &LearningStore,
    scope: &LearningScope,
    recommendations: &[&LearningCapabilityEvolutionRollbackRecommendationRecord],
) -> Vec<LearningEvidenceRef> {
    recommendations
        .iter()
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .map(|recommendation| LearningEvidenceRef {
            kind: "learning_capability_rollback_recommendation".to_string(),
            id: Some(recommendation.id.clone()),
            path: Some({
                let workspace = store.workspace_layout();
                preferred_existing_path(
                    workspace,
                    workspace.skill_evolution_rollback_recommendation_path(
                        &scope.principal,
                        &scope.workspace,
                        &recommendation.candidate_id,
                        &recommendation.id,
                    ),
                    workspace.capability_evolution_rollback_recommendation_path(
                        &scope.principal,
                        &scope.workspace,
                        &recommendation.candidate_id,
                        &recommendation.id,
                    ),
                )
                .display()
                .to_string()
            }),
            uri: None,
            summary: Some(recommendation.summary.clone()),
        })
        .collect()
}

fn event_refs(events: &[&LearningEvent]) -> Vec<LearningEvidenceRef> {
    events
        .iter()
        .take(MAX_GROWTH_EVAL_EVIDENCE_REFS)
        .map(|event| LearningEvidenceRef {
            kind: "learning_event".to_string(),
            id: Some(event.id.clone()),
            path: None,
            uri: None,
            summary: Some(event.summary.clone()),
        })
        .collect()
}

fn preferred_existing_path(
    workspace_layout: &ArtifactV2Workspace,
    preferred: PathBuf,
    legacy: PathBuf,
) -> PathBuf {
    let preferred_exists = workspace_layout
        .exists_path_sync(&preferred)
        .unwrap_or(false);
    let legacy_exists = workspace_layout.exists_path_sync(&legacy).unwrap_or(false);
    if preferred_exists || !legacy_exists {
        preferred
    } else {
        legacy
    }
}

fn analytics_ref(store: &LearningStore, scope: &LearningScope, name: &str) -> LearningEvidenceRef {
    LearningEvidenceRef {
        kind: format!("analytics_{name}"),
        id: None,
        path: Some(
            store
                .workspace_layout()
                .analytics_memory_events_root(&scope.principal, &scope.workspace)
                .display()
                .to_string(),
        ),
        uri: None,
        summary: Some(format!("{name} scoped analytics root")),
    }
}

fn load_memory_eval_analytics(
    workspace_layout: &crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    scope: &LearningScope,
    cutoff: DateTime<Utc>,
) -> MemoryEvalAnalyticsSummary {
    let root = workspace_layout.analytics_memory_events_root(&scope.principal, &scope.workspace);
    if !has_parquet_batches(&root) {
        return MemoryEvalAnalyticsSummary::default();
    }
    let result = (|| -> Result<MemoryEvalAnalyticsSummary> {
        let _duckdb_guard = analytics_duckdb_guard();
        let conn = Connection::open_in_memory().context("opening in-memory DuckDB")?;
        configure_analytics_connection_checked(&conn, "growth_eval_memory_events")
            .context("configuring conservative DuckDB limits for memory eval analytics")?;
        let glob_path = root.join("dt=*/*.parquet");
        let glob_sql = glob_path.display().to_string().replace('\'', "''");
        conn.execute_batch(&format!(
            "CREATE OR REPLACE VIEW memory_events AS SELECT * FROM read_parquet('{glob_sql}', hive_partitioning = true, union_by_name = true);"
        ))
        .context("creating memory_events view")?;
        let cutoff_ms = cutoff.timestamp_millis();
        let sql = format!(
            "SELECT COUNT(*) AS case_count,
                        SUM(CASE WHEN COALESCE(eval_pass, false) THEN 1 ELSE 0 END) AS passed_count,
                        COUNT(DISTINCT eval_suite) AS suite_count
                 FROM memory_events
                 WHERE event_kind = 'eval_case'
                   AND timestamp_ms >= {cutoff_ms}"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("preparing memory eval summary query")?;
        let (case_count, passed_count, suite_count): (i64, Option<i64>, i64) = stmt
            .query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .context("running memory eval summary query")?;
        let case_count = non_negative_usize(case_count);
        let passed_count = non_negative_usize(passed_count.unwrap_or(0));
        Ok(MemoryEvalAnalyticsSummary {
            case_count,
            passed_count,
            failed_count: case_count.saturating_sub(passed_count),
            suite_count: non_negative_usize(suite_count),
            query_error: None,
        })
    })();
    match result {
        Ok(summary) => summary,
        Err(error) => MemoryEvalAnalyticsSummary {
            query_error: Some(error.to_string()),
            ..MemoryEvalAnalyticsSummary::default()
        },
    }
}

fn load_llm_prompt_analytics(
    workspace_layout: &crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    scope: &LearningScope,
    cutoff: DateTime<Utc>,
) -> LlmPromptAnalyticsSummary {
    let root = workspace_layout.analytics_llm_calls_root(&scope.principal, &scope.workspace);
    if !has_parquet_batches(&root) {
        return LlmPromptAnalyticsSummary::default();
    }
    let result = (|| -> Result<LlmPromptAnalyticsSummary> {
        let _duckdb_guard = analytics_duckdb_guard();
        let conn = Connection::open_in_memory().context("opening in-memory DuckDB")?;
        configure_analytics_connection_checked(&conn, "growth_eval_llm_calls")
            .context("configuring conservative DuckDB limits for LLM prompt analytics")?;
        let glob_path = root.join("dt=*/*.parquet");
        let glob_sql = glob_path.display().to_string().replace('\'', "''");
        conn.execute_batch(&format!(
            "CREATE OR REPLACE VIEW llm_calls AS SELECT * FROM read_parquet('{glob_sql}', hive_partitioning = true, union_by_name = true);"
        ))
        .context("creating llm_calls view")?;
        let cutoff_ms = cutoff.timestamp_millis();
        let midpoint_ms = cutoff_ms + (Utc::now().timestamp_millis() - cutoff_ms) / 2;
        let sql = format!(
            "SELECT COUNT(*) AS call_count,
                    AVG(input_tokens + output_tokens + reasoning_tokens) AS avg_total_tokens,
                    MAX(input_tokens + output_tokens + reasoning_tokens) AS max_total_tokens,
                    AVG(CASE WHEN timestamp_ms < {midpoint_ms} THEN input_tokens + output_tokens + reasoning_tokens END) AS previous_avg_total_tokens,
                    AVG(CASE WHEN timestamp_ms >= {midpoint_ms} THEN input_tokens + output_tokens + reasoning_tokens END) AS recent_avg_total_tokens
             FROM llm_calls
             WHERE timestamp_ms >= {cutoff_ms}"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("preparing llm prompt-token summary query")?;
        let (
            call_count,
            avg_total_tokens,
            max_total_tokens,
            previous_avg_total_tokens,
            recent_avg_total_tokens,
        ): (i64, Option<f64>, Option<i64>, Option<f64>, Option<f64>) = stmt
            .query_row([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .context("running llm prompt-token summary query")?;
        Ok(LlmPromptAnalyticsSummary {
            call_count: non_negative_usize(call_count),
            avg_total_tokens,
            max_total_tokens,
            previous_avg_total_tokens,
            recent_avg_total_tokens,
            query_error: None,
        })
    })();
    match result {
        Ok(summary) => summary,
        Err(error) => LlmPromptAnalyticsSummary {
            query_error: Some(error.to_string()),
            ..LlmPromptAnalyticsSummary::default()
        },
    }
}

fn has_parquet_batches(root: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("parquet")
        {
            return true;
        }
        if !file_type.is_dir() {
            continue;
        }
        let Ok(children) = fs::read_dir(entry.path()) else {
            continue;
        };
        if children
            .flatten()
            .any(|child| child.path().extension().and_then(|ext| ext.to_str()) == Some("parquet"))
        {
            return true;
        }
    }
    false
}

fn count_json_files(workspace_layout: &ArtifactV2Workspace, root: &Path) -> usize {
    let Ok(entries) = workspace_layout.read_dir_path_sync(root) else {
        return 0;
    };
    entries
        .into_iter()
        .filter(|entry| entry.is_file)
        .filter(|entry| {
            entry.relative_path.extension().and_then(|ext| ext.to_str()) == Some("json")
        })
        .count()
}

fn non_negative_usize(value: i64) -> usize {
    usize::try_from(value.max(0)).unwrap_or(usize::MAX)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::learning::{
        CreateLearningCandidateRequest, CreateLearningEventRequest, CreateLearningProcedureRequest,
        LearningProcedureActivation, LearningProcedureSkillPromotionBridge,
        PromoteLearningProcedureToSkillRequest,
    };

    #[test]
    fn growth_eval_reports_procedure_quality_dimensions_and_scenarios() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, "proc_eval_stable", 3, 0);

        LearningProcedureSkillPromotionBridge::new(workspace)
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest::default(),
            )
            .expect("promote procedure to skill candidate");

        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_retrieval_rendered".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_eval".to_string()),
                    execution_id: Some("exec_eval".to_string()),
                    chat_session_id: None,
                    summary: "Selected procedure for eval fixture.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "candidate_count": 1,
                        "selected_count": 1,
                        "dropped_count": 0,
                        "selected": [{
                            "procedure_id": &procedure.id,
                            "title": &procedure.title,
                            "score": 20,
                            "reason": "fixture"
                        }]
                    }),
                },
            )
            .expect("append retrieval event");
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_feedback_recorded".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_eval".to_string()),
                    execution_id: Some("exec_eval".to_string()),
                    chat_session_id: None,
                    summary: "Procedure helped the run.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "procedure_id": &procedure.id,
                        "outcome": "success",
                        "success_count": 4,
                        "failure_count": 0,
                        "explicit_negative_feedback": false,
                        "procedure_judgement": { "verdict": "useful" }
                    }),
                },
            )
            .expect("append feedback event");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_fixture".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        assert!(report.dimensions.iter().any(|dimension| dimension.dimension
            == "procedure_to_skill_promotion_quality"
            && dimension.status == LearningEvaluationRunStatus::Passed));
        assert!(report.scenarios.iter().any(|scenario| {
            scenario.scenario == "active_procedure_graduates_to_skill_evolution_after_evidence"
                && scenario.status == LearningEvaluationRunStatus::Passed
        }));
        assert_eq!(report.metrics["dimension_count"], json!(23));
        assert_eq!(report.metrics["scenario_count"], json!(14));
    }

    #[test]
    fn growth_eval_reports_post_promotion_stability_and_regression_rate() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let now = Utc::now();
        let stable_promotion = LearningCapabilityEvolutionPromotionRecord {
            id: "lcepromo_stable".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate_stable".to_string(),
            proposal_id: "proposal_stable".to_string(),
            validation_id: "validation_stable".to_string(),
            implementation_id: None,
            application_id: None,
            capability_id: Some("skill:stable".to_string()),
            actor: "tester".to_string(),
            summary: "stable promotion".to_string(),
            applied_files: vec!["skills/stable/SKILL.md".to_string()],
            evidence_refs: Vec::new(),
            payload: Value::Null,
            created_at: now,
        };
        let regressed_promotion = LearningCapabilityEvolutionPromotionRecord {
            id: "lcepromo_regressed".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate_regressed".to_string(),
            proposal_id: "proposal_regressed".to_string(),
            validation_id: "validation_regressed".to_string(),
            implementation_id: None,
            application_id: None,
            capability_id: Some("skill:regressed".to_string()),
            actor: "tester".to_string(),
            summary: "regressed promotion".to_string(),
            applied_files: vec!["skills/regressed/SKILL.md".to_string()],
            evidence_refs: Vec::new(),
            payload: Value::Null,
            created_at: now,
        };
        store
            .append_capability_evolution_promotion_record(&stable_promotion)
            .expect("write stable promotion");
        store
            .append_capability_evolution_promotion_record(&regressed_promotion)
            .expect("write regressed promotion");
        for (promotion, status, after_success_rate, follow_up_candidate_id) in [
            (
                &stable_promotion,
                LearningCapabilityEvolutionPostPromotionMonitorStatus::Stable,
                Some(1.0),
                None,
            ),
            (
                &regressed_promotion,
                LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected,
                Some(0.0),
                Some("candidate_follow_up".to_string()),
            ),
        ] {
            store
                .write_capability_evolution_post_promotion_monitor_record(
                    &LearningCapabilityEvolutionPostPromotionMonitorRecord {
                        id: format!("lceppm_{}", promotion.id),
                        scope: scope.clone(),
                        promotion_id: promotion.id.clone(),
                        candidate_id: promotion.candidate_id.clone(),
                        proposal_id: promotion.proposal_id.clone(),
                        validation_id: promotion.validation_id.clone(),
                        implementation_id: promotion.implementation_id.clone(),
                        application_id: promotion.application_id.clone(),
                        capability_id: promotion.capability_id.clone(),
                        status,
                        summary: format!("{} monitor", promotion.id),
                        skill_names: vec![promotion
                            .capability_id
                            .as_deref()
                            .unwrap_or("skill:unknown")
                            .trim_start_matches("skill:")
                            .to_string()],
                        before_invocation_count: 3,
                        after_invocation_count: 3,
                        before_success_count: 3,
                        after_success_count: if after_success_rate == Some(1.0) {
                            3
                        } else {
                            0
                        },
                        before_failure_count: 0,
                        after_failure_count: if after_success_rate == Some(1.0) {
                            0
                        } else {
                            3
                        },
                        before_success_rate: Some(1.0),
                        after_success_rate,
                        same_failure_recurrence_count: if after_success_rate == Some(1.0) {
                            0
                        } else {
                            1
                        },
                        new_failure_classes: if after_success_rate == Some(1.0) {
                            Vec::new()
                        } else {
                            vec!["tool_misuse".to_string()]
                        },
                        user_negative_feedback_count: 0,
                        rollback_recommendation_id: None,
                        follow_up_candidate_id,
                        evidence_refs: Vec::new(),
                        payload: Value::Null,
                        created_at: now,
                        updated_at: now,
                    },
                )
                .expect("write monitor");
        }

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase7_post_promotion".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let stable_dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "post_promotion_stable_promotions")
            .expect("stable promotions dimension");
        assert_eq!(stable_dimension.metrics["stable_count"], json!(1));
        assert_eq!(stable_dimension.metrics["regression_count"], json!(1));
        let regression_dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "post_promotion_regression_rate")
            .expect("regression rate dimension");
        assert_eq!(
            regression_dimension.status,
            LearningEvaluationRunStatus::Failed
        );
        assert_eq!(regression_dimension.metrics["regression_rate"], json!(0.5));
        assert_eq!(report.metrics["post_promotion_monitor_count"], json!(2));
    }

    #[test]
    fn growth_eval_reports_phase9_backlog_dedupe_and_validation_health() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let now = Utc::now();
        let stale = now - Duration::days(3);

        for item in [
            phase9_backlog_item(
                &scope,
                "candidate_duplicate_one",
                LearningCapabilityEvolutionBacklogStatus::Queued,
                Some("dedupe:same"),
                stale,
                stale,
            ),
            phase9_backlog_item(
                &scope,
                "candidate_duplicate_two",
                LearningCapabilityEvolutionBacklogStatus::InReview,
                Some("dedupe:same"),
                now,
                now,
            ),
            phase9_backlog_item(
                &scope,
                "candidate_missing_fingerprint",
                LearningCapabilityEvolutionBacklogStatus::Validated,
                None,
                stale,
                stale,
            ),
        ] {
            store
                .write_capability_evolution_backlog_item(&item)
                .expect("write backlog item");
        }

        for proposal in [
            phase9_proposal(
                &scope,
                "proposal_failed_validation",
                "candidate_failed_validation",
                LearningCapabilityEvolutionProposalStatus::Approved,
                now,
            ),
            phase9_proposal(
                &scope,
                "proposal_missing_validation",
                "candidate_missing_validation",
                LearningCapabilityEvolutionProposalStatus::Approved,
                now,
            ),
        ] {
            store
                .write_capability_evolution_proposal(&proposal)
                .expect("write proposal");
        }
        store
            .write_capability_evolution_validation_report(&phase9_validation(
                &scope,
                "validation_failed",
                "candidate_failed_validation",
                "proposal_failed_validation",
                LearningCapabilityEvolutionValidationStatus::Failed,
                now,
            ))
            .expect("write failed validation");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase9_health_backlog_validation".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let dedupe = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "candidate_dedupe_quality")
            .expect("candidate dedupe dimension");
        assert_eq!(dedupe.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(dedupe.metrics["missing_dedupe_fingerprint_count"], json!(1));
        assert_eq!(
            dedupe.metrics["duplicate_fingerprint_group_count"],
            json!(1)
        );

        let stale_backlog = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "stale_backlog_rate")
            .expect("stale backlog dimension");
        assert_eq!(stale_backlog.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(stale_backlog.metrics["stale_backlog_count"], json!(2));

        let validation = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "proposal_validation_pass_rate")
            .expect("proposal validation dimension");
        assert_eq!(validation.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(
            validation.metrics["failed_or_blocked_validation_count"],
            json!(1)
        );
        assert_eq!(
            validation.metrics["approved_without_validation_count"],
            json!(1)
        );
    }

    #[test]
    fn growth_eval_routes_failed_health_dimensions_to_skill_evolution_backlog() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let now = Utc::now();

        store
            .write_capability_evolution_proposal(&phase9_proposal(
                &scope,
                "proposal_failed_validation",
                "candidate_failed_validation",
                LearningCapabilityEvolutionProposalStatus::Approved,
                now,
            ))
            .expect("write proposal");
        store
            .write_capability_evolution_validation_report(&phase9_validation(
                &scope,
                "validation_failed",
                "candidate_failed_validation",
                "proposal_failed_validation",
                LearningCapabilityEvolutionValidationStatus::Failed,
                now,
            ))
            .expect("write failed validation");

        let report = run_learning_growth_evaluation(
            &store,
            scope.clone(),
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase9_failure_routing".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        assert!(
            report.metrics["growth_eval_failure_routed_count"]
                .as_u64()
                .unwrap_or_default()
                >= 1
        );
        let backlog = store
            .list_capability_evolution_backlog_items(
                &scope,
                LearningCapabilityEvolutionBacklogFilters::default(),
            )
            .expect("list backlog");
        let routed = backlog
            .iter()
            .find(|item| {
                item.proposed_fix_type.as_deref() == Some("skill_evolution_health_rollup")
                    && item
                        .fix_spec
                        .get("growth_eval_dimension")
                        .and_then(Value::as_str)
                        == Some("proposal_validation_pass_rate")
            })
            .expect("routed growth-eval backlog item");
        assert_eq!(routed.candidate_type, LearningCandidateType::SkillUpdate);
        assert_eq!(
            routed.status,
            LearningCapabilityEvolutionBacklogStatus::Queued
        );
        assert!(routed.dedupe_fingerprint.is_some());
        assert!(!routed.evidence_refs.is_empty());
    }

    #[test]
    fn growth_eval_records_routing_errors_without_failing_report() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let now = Utc::now();

        store
            .write_capability_evolution_proposal(&phase9_proposal(
                &scope,
                "proposal_failed_validation",
                "candidate_failed_validation",
                LearningCapabilityEvolutionProposalStatus::Approved,
                now,
            ))
            .expect("write proposal");
        store
            .write_capability_evolution_validation_report(&phase9_validation(
                &scope,
                "validation_failed",
                "candidate_failed_validation",
                "proposal_failed_validation",
                LearningCapabilityEvolutionValidationStatus::Failed,
                now,
            ))
            .expect("write failed validation");

        let decisions_dir = store
            .workspace_layout()
            .learning_decisions_dir(&scope.principal, &scope.workspace);
        let learning_root = decisions_dir.parent().expect("learning root");
        std::fs::create_dir_all(learning_root).expect("create learning root");
        std::fs::write(&decisions_dir, b"not a directory").expect("block decision log writes");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase9_failure_routing_error".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("growth evaluation should not fail when routing side effects fail");

        let routing = report
            .payload
            .get("skill_evolution_failure_routing")
            .and_then(Value::as_array)
            .expect("routing outcomes");
        assert!(routing.iter().any(|outcome| {
            outcome.get("name").and_then(Value::as_str) == Some("proposal_validation_pass_rate")
                && outcome.get("routed").and_then(Value::as_bool) == Some(false)
                && outcome.get("reason").and_then(Value::as_str) == Some("routing_error")
                && outcome.get("error").and_then(Value::as_str).is_some()
        }));
        assert!(
            report.metrics["growth_eval_failure_skipped_count"]
                .as_u64()
                .unwrap_or_default()
                >= 1
        );
    }

    #[test]
    fn growth_eval_reports_phase9_rollback_followup_health() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let now = Utc::now();

        let rollback = phase9_rollback_recommendation(
            &scope,
            "rollback_handled",
            "candidate_rollback",
            "proposal_rollback",
            "application_rollback",
            now,
        );
        store
            .write_capability_evolution_rollback_recommendation_record(&rollback)
            .expect("write rollback recommendation");

        for monitor in [
            phase9_monitor(
                &scope,
                "monitor_rollback",
                "promotion_rollback",
                "candidate_rollback",
                "proposal_rollback",
                Some("rollback_handled"),
                None,
                now,
            ),
            phase9_monitor(
                &scope,
                "monitor_followup",
                "promotion_followup",
                "candidate_followup",
                "proposal_followup",
                None,
                Some("candidate_followup_remediation"),
                now,
            ),
            phase9_monitor(
                &scope,
                "monitor_unhandled",
                "promotion_unhandled",
                "candidate_unhandled",
                "proposal_unhandled",
                None,
                None,
                now,
            ),
        ] {
            store
                .write_capability_evolution_post_promotion_monitor_record(&monitor)
                .expect("write monitor");
        }

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase9_rollback_followup".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "rollback_followup_rate")
            .expect("rollback followup dimension");
        assert_eq!(dimension.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(dimension.metrics["regression_count"], json!(3));
        assert_eq!(dimension.metrics["handled_regression_count"], json!(2));
        assert_eq!(dimension.metrics["unhandled_regression_count"], json!(1));
        assert_eq!(dimension.metrics["rollback_recommendation_count"], json!(1));
    }

    #[test]
    fn growth_eval_flags_duplicate_active_procedures() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        active_procedure(&store, &scope, "proc_duplicate_one", 2, 0);
        active_procedure(&store, &scope, "proc_duplicate_two", 2, 0);

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_duplicates".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let duplicate_dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "duplicate_procedure_rate")
            .expect("duplicate dimension");
        assert_eq!(
            duplicate_dimension.status,
            LearningEvaluationRunStatus::Failed
        );
        assert_eq!(
            duplicate_dimension.metrics["duplicate_group_count"],
            json!(1)
        );
    }

    #[test]
    fn correction_feedback_without_correlated_update_fails_scenario() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, "proc_correction_without_update", 1, 0);
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_feedback_recorded".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_correction".to_string()),
                    execution_id: Some("exec_correction".to_string()),
                    chat_session_id: None,
                    summary: "Procedure correction feedback without update candidate.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "procedure_id": &procedure.id,
                        "outcome": "explicit_negative_correction",
                        "explicit_negative_feedback": true,
                        "procedure_judgement": {
                            "procedure_id": &procedure.id,
                            "verdict": "too_broad",
                            "deprecation_recommended": false
                        }
                    }),
                },
            )
            .expect("append feedback event");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_correction".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let scenario = report
            .scenarios
            .iter()
            .find(|scenario| scenario.scenario == "procedure_updated_after_user_correction")
            .expect("correction scenario");
        assert_eq!(scenario.status, LearningEvaluationRunStatus::Failed);
    }

    #[test]
    fn correction_candidate_without_feedback_does_not_satisfy_correction_scenario() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, "proc_candidate_without_feedback", 1, 0);
        procedure_update_candidate(&store, &scope, &procedure.id);

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_candidate_without_feedback".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let scenario = report
            .scenarios
            .iter()
            .find(|scenario| scenario.scenario == "procedure_updated_after_user_correction")
            .expect("correction scenario");
        assert_eq!(scenario.status, LearningEvaluationRunStatus::Blocked);
    }

    #[test]
    fn correction_feedback_without_target_id_fails_scenario() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_feedback_recorded".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_correction".to_string()),
                    execution_id: Some("exec_correction".to_string()),
                    chat_session_id: None,
                    summary: "Procedure correction feedback without a target id.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "outcome": "explicit_negative_correction",
                        "explicit_negative_feedback": true,
                        "procedure_judgement": { "verdict": "too_broad" }
                    }),
                },
            )
            .expect("append feedback event");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_correction_missing_target".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let scenario = report
            .scenarios
            .iter()
            .find(|scenario| scenario.scenario == "procedure_updated_after_user_correction")
            .expect("correction scenario");
        assert_eq!(scenario.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(scenario.metrics["target_procedure_id_count"], json!(0));
    }

    #[test]
    fn promotion_event_without_candidate_or_backlogs_fails_quality() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_skill_promotion_routed".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_promotion".to_string()),
                    execution_id: None,
                    chat_session_id: None,
                    summary: "Promotion event without materialized candidate.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "procedure_id": "proc_missing_candidate",
                        "promotion_candidate_id": "lc_missing_promotion_candidate",
                        "target_skill": "dashboard-variance"
                    }),
                },
            )
            .expect("append promotion event");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_promotion_missing".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "procedure_to_skill_promotion_quality")
            .expect("promotion quality dimension");
        assert_eq!(dimension.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(dimension.metrics["missing_candidate_count"], json!(1));
    }

    #[test]
    fn promotion_event_without_candidate_id_fails_quality() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_skill_promotion_routed".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_promotion".to_string()),
                    execution_id: None,
                    chat_session_id: None,
                    summary: "Promotion event without a candidate id.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "procedure_id": "proc_missing_candidate_id",
                        "target_skill": "dashboard-variance"
                    }),
                },
            )
            .expect("append promotion event");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_promotion_missing_id".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "procedure_to_skill_promotion_quality")
            .expect("promotion quality dimension");
        assert_eq!(dimension.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(
            dimension.metrics["promotion_events_without_candidate_id_count"],
            json!(1)
        );
    }

    #[test]
    fn irrelevant_feedback_requires_later_withheld_retrieval() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_retrieval_rendered".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_irrelevant".to_string()),
                    execution_id: Some("exec_irrelevant".to_string()),
                    chat_session_id: None,
                    summary: "Withheld before irrelevant feedback.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "candidate_count": 1,
                        "selected_count": 0,
                        "dropped_count": 1,
                        "selected": []
                    }),
                },
            )
            .expect("append withheld event");
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_feedback_recorded".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_irrelevant".to_string()),
                    execution_id: Some("exec_irrelevant".to_string()),
                    chat_session_id: None,
                    summary: "Procedure was irrelevant after earlier withheld event.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "procedure_id": "proc_irrelevant",
                        "outcome": "failure",
                        "procedure_judgement": {
                            "procedure_id": "proc_irrelevant",
                            "verdict": "irrelevant"
                        }
                    }),
                },
            )
            .expect("append irrelevant feedback");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_irrelevant".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let scenario = report
            .scenarios
            .iter()
            .find(|scenario| {
                scenario.scenario == "irrelevant_procedure_withheld_despite_keyword_overlap"
            })
            .expect("irrelevant withholding scenario");
        assert_eq!(scenario.status, LearningEvaluationRunStatus::Failed);
    }

    #[test]
    fn stale_feedback_requires_later_deprecation_evidence() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, "proc_stale_before_feedback", 1, 0);
        store
            .transition_procedure_status(
                &scope,
                &procedure.id,
                LearningProcedureStatus::Deprecated,
                "test",
                "deprecated_before_feedback",
                "fixture deprecation before stale feedback",
                Vec::new(),
            )
            .expect("deprecate procedure");
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_feedback_recorded".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_stale".to_string()),
                    execution_id: Some("exec_stale".to_string()),
                    chat_session_id: None,
                    summary: "Stale feedback after old deprecation.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "procedure_id": &procedure.id,
                        "outcome": "failure",
                        "procedure_judgement": {
                            "procedure_id": &procedure.id,
                            "verdict": "stale"
                        }
                    }),
                },
            )
            .expect("append stale feedback");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_stale_old_deprecation".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "stale_procedure_correction")
            .expect("stale correction dimension");
        assert_eq!(dimension.status, LearningEvaluationRunStatus::Failed);
        let scenario = report
            .scenarios
            .iter()
            .find(|scenario| scenario.scenario == "harmful_or_stale_procedure_deprecated")
            .expect("stale deprecation scenario");
        assert_eq!(scenario.status, LearningEvaluationRunStatus::Failed);
    }

    #[test]
    fn retrieval_relevance_ignores_unrelated_feedback() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_retrieval_rendered".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_retrieval".to_string()),
                    execution_id: Some("exec_retrieval".to_string()),
                    chat_session_id: None,
                    summary: "Selected target procedure.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "candidate_count": 1,
                        "selected_count": 1,
                        "dropped_count": 0,
                        "selected": [{
                            "procedure_id": "proc_target",
                            "title": "Target procedure",
                            "score": 20,
                            "reason": "fixture"
                        }]
                    }),
                },
            )
            .expect("append retrieval event");
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_feedback_recorded".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_other".to_string()),
                    execution_id: Some("exec_other".to_string()),
                    chat_session_id: None,
                    summary: "Unrelated useful procedure feedback.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "procedure_id": "proc_other",
                        "outcome": "success",
                        "procedure_judgement": {
                            "procedure_id": "proc_other",
                            "verdict": "useful"
                        }
                    }),
                },
            )
            .expect("append unrelated feedback");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_retrieval_unrelated_feedback".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "procedure_retrieval_relevance")
            .expect("retrieval relevance dimension");
        assert_eq!(dimension.status, LearningEvaluationRunStatus::Blocked);
        assert_eq!(
            dimension.metrics["correlated_feedback_event_count"],
            json!(0)
        );
    }

    #[test]
    fn correction_feedback_requires_every_target_to_have_evidence() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let first = active_procedure(&store, &scope, "proc_corrected_one", 1, 0);
        let second = active_procedure(&store, &scope, "proc_uncorrected_two", 1, 0);
        for procedure in [&first, &second] {
            store
                .append_event(
                    scope.clone(),
                    CreateLearningEventRequest {
                        principal: None,
                        workspace: None,
                        event_type: "learning_procedure_feedback_recorded".to_string(),
                        agent_id: Some("simple-data-analyst".to_string()),
                        task_id: Some("task_multi_correction".to_string()),
                        execution_id: Some("exec_multi_correction".to_string()),
                        chat_session_id: None,
                        summary: "Procedure correction feedback.".to_string(),
                        evidence_refs: Vec::new(),
                        payload: json!({
                            "procedure_id": &procedure.id,
                            "outcome": "explicit_negative_correction",
                            "explicit_negative_feedback": true,
                            "procedure_judgement": {
                                "procedure_id": &procedure.id,
                                "verdict": "too_broad"
                            }
                        }),
                    },
                )
                .expect("append correction feedback");
        }
        procedure_update_candidate(&store, &scope, &first.id);

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_multi_correction".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let scenario = report
            .scenarios
            .iter()
            .find(|scenario| scenario.scenario == "procedure_updated_after_user_correction")
            .expect("correction scenario");
        assert_eq!(scenario.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(
            scenario.metrics["uncorrected_target_procedure_id_count"],
            json!(1)
        );
    }

    #[test]
    fn too_broad_feedback_is_not_stale_harmful_deprecation_pressure() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, "proc_too_broad_feedback", 1, 0);
        store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_feedback_recorded".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_too_broad".to_string()),
                    execution_id: Some("exec_too_broad".to_string()),
                    chat_session_id: None,
                    summary: "Procedure was too broad, not stale or harmful.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "procedure_id": &procedure.id,
                        "outcome": "failure",
                        "procedure_judgement": {
                            "procedure_id": &procedure.id,
                            "verdict": "too_broad",
                            "deprecation_recommended": false
                        }
                    }),
                },
            )
            .expect("append feedback");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_too_broad_not_stale".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "stale_procedure_correction")
            .expect("stale correction dimension");
        assert_eq!(dimension.status, LearningEvaluationRunStatus::Blocked);
        let scenario = report
            .scenarios
            .iter()
            .find(|scenario| scenario.scenario == "harmful_or_stale_procedure_deprecated")
            .expect("stale deprecation scenario");
        assert_eq!(scenario.status, LearningEvaluationRunStatus::Blocked);
    }

    #[test]
    fn retrieval_relevance_fails_when_any_correlated_bad_signal_exists() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let retrieval_event = store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_retrieval_rendered".to_string(),
                    agent_id: Some("simple-data-analyst".to_string()),
                    task_id: Some("task_retrieval_quality".to_string()),
                    execution_id: Some("exec_retrieval_quality".to_string()),
                    chat_session_id: None,
                    summary: "Selected mixed procedure quality.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "candidate_count": 2,
                        "selected_count": 2,
                        "dropped_count": 0,
                        "selected": [
                            { "procedure_id": "proc_good", "title": "Good", "score": 20, "reason": "fixture" },
                            { "procedure_id": "proc_bad", "title": "Bad", "score": 20, "reason": "fixture" }
                        ]
                    }),
                },
            )
            .expect("append retrieval event");
        for (procedure_id, outcome, verdict) in [
            ("proc_good", "success", "useful"),
            ("proc_bad", "failure", "harmful"),
        ] {
            store
                .append_event(
                    scope.clone(),
                    CreateLearningEventRequest {
                        principal: None,
                        workspace: None,
                        event_type: "learning_procedure_feedback_recorded".to_string(),
                        agent_id: Some("simple-data-analyst".to_string()),
                        task_id: Some("task_retrieval_quality".to_string()),
                        execution_id: Some("exec_retrieval_quality".to_string()),
                        chat_session_id: None,
                        summary: "Correlated retrieval feedback.".to_string(),
                        evidence_refs: Vec::new(),
                        payload: json!({
                            "procedure_id": procedure_id,
                            "retrieval_event_id": &retrieval_event.id,
                            "outcome": outcome,
                            "procedure_judgement": {
                                "procedure_id": procedure_id,
                                "verdict": verdict
                            }
                        }),
                    },
                )
                .expect("append feedback");
        }

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_retrieval_bad_signal".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "procedure_retrieval_relevance")
            .expect("retrieval relevance dimension");
        assert_eq!(dimension.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(dimension.metrics["retrieval_quality_issue_count"], json!(1));
    }

    #[test]
    fn stale_feedback_requires_deprecation_for_every_target() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("test", "workspace");
        let first = active_procedure(&store, &scope, "proc_stale_corrected_one", 1, 0);
        let second = active_procedure(&store, &scope, "proc_stale_uncorrected_two", 1, 0);
        for procedure in [&first, &second] {
            store
                .append_event(
                    scope.clone(),
                    CreateLearningEventRequest {
                        principal: None,
                        workspace: None,
                        event_type: "learning_procedure_feedback_recorded".to_string(),
                        agent_id: Some("simple-data-analyst".to_string()),
                        task_id: Some("task_multi_stale".to_string()),
                        execution_id: Some("exec_multi_stale".to_string()),
                        chat_session_id: None,
                        summary: "Procedure stale feedback.".to_string(),
                        evidence_refs: Vec::new(),
                        payload: json!({
                            "procedure_id": &procedure.id,
                            "outcome": "failure",
                            "procedure_judgement": {
                                "procedure_id": &procedure.id,
                                "verdict": "stale"
                            }
                        }),
                    },
                )
                .expect("append stale feedback");
        }
        store
            .transition_procedure_status(
                &scope,
                &first.id,
                LearningProcedureStatus::Deprecated,
                "test",
                "deprecated_after_feedback",
                "fixture deprecation after stale feedback",
                Vec::new(),
            )
            .expect("deprecate procedure");

        let report = run_learning_growth_evaluation(
            &store,
            scope,
            RunLearningGrowthEvaluationRequest {
                principal: None,
                workspace: None,
                suite_id: Some("phase15_multi_stale".to_string()),
                window_days: Some(1),
                payload: json!({}),
            },
        )
        .expect("run growth evaluation");

        let dimension = report
            .dimensions
            .iter()
            .find(|dimension| dimension.dimension == "stale_procedure_correction")
            .expect("stale correction dimension");
        assert_eq!(dimension.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(
            dimension.metrics["uncorrected_target_procedure_id_count"],
            json!(1)
        );
        let scenario = report
            .scenarios
            .iter()
            .find(|scenario| scenario.scenario == "harmful_or_stale_procedure_deprecated")
            .expect("stale deprecation scenario");
        assert_eq!(scenario.status, LearningEvaluationRunStatus::Failed);
        assert_eq!(
            scenario.metrics["uncorrected_target_procedure_id_count"],
            json!(1)
        );
    }

    fn phase9_backlog_item(
        scope: &LearningScope,
        candidate_id: &str,
        status: LearningCapabilityEvolutionBacklogStatus,
        dedupe_fingerprint: Option<&str>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> LearningCapabilityEvolutionBacklogItem {
        LearningCapabilityEvolutionBacklogItem {
            id: format!("lcebl_{candidate_id}"),
            scope: scope.clone(),
            candidate_id: candidate_id.to_string(),
            status,
            candidate_type: LearningCandidateType::ToolWrapperFix,
            title: format!("Fix {candidate_id}"),
            summary: "Fixture Skill Evolution backlog item.".to_string(),
            rationale: "fixture".to_string(),
            capability_id: Some("tool:fixture".to_string()),
            failure_pattern: Some("fixture_failure".to_string()),
            proposed_fix_type: Some("tool_wrapper_fix".to_string()),
            proposed_files: vec!["skillshub/fixture/wrapper.ts".to_string()],
            required_eval: None,
            promotion_gate: None,
            proposed_target: Some("skillshub/fixture".to_string()),
            risk_level: LearningRiskLevel::Low,
            dedupe_fingerprint: dedupe_fingerprint.map(str::to_string),
            recurrence_count: 1,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: true,
            rank_score: 1.0,
            rank_reasons: vec!["fixture".to_string()],
            owner_hints: vec!["tool-runtime".to_string()],
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: Some("test".to_string()),
            source_task_id: Some("task_phase9".to_string()),
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: json!({ "fixture": true }),
            created_at,
            updated_at,
        }
    }

    fn phase9_proposal(
        scope: &LearningScope,
        proposal_id: &str,
        candidate_id: &str,
        status: LearningCapabilityEvolutionProposalStatus,
        now: DateTime<Utc>,
    ) -> LearningCapabilityEvolutionProposal {
        LearningCapabilityEvolutionProposal {
            id: proposal_id.to_string(),
            scope: scope.clone(),
            candidate_id: candidate_id.to_string(),
            backlog_id: format!("lcebl_{candidate_id}"),
            status,
            title: format!("Proposal {proposal_id}"),
            summary: "Fixture proposal.".to_string(),
            capability_id: Some("tool:fixture".to_string()),
            proposed_fix_type: Some("tool_wrapper_fix".to_string()),
            proposed_files: vec!["skillshub/fixture/wrapper.ts".to_string()],
            change_plan: json!({ "fixture": true }),
            patches: Vec::new(),
            eval_plan: Some(json!({ "commands": ["npm test"] })),
            validation_plan: Some(json!({ "commands": ["npm test"] })),
            promotion_gate: Some(json!({ "requires_validation": true })),
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn phase9_validation(
        scope: &LearningScope,
        validation_id: &str,
        candidate_id: &str,
        proposal_id: &str,
        status: LearningCapabilityEvolutionValidationStatus,
        now: DateTime<Utc>,
    ) -> LearningCapabilityEvolutionValidationReport {
        LearningCapabilityEvolutionValidationReport {
            id: validation_id.to_string(),
            scope: scope.clone(),
            candidate_id: candidate_id.to_string(),
            proposal_id: proposal_id.to_string(),
            status,
            capability_id: Some("tool:fixture".to_string()),
            runner: "test".to_string(),
            summary: "Fixture validation.".to_string(),
            commands: vec!["npm test".to_string()],
            evidence_refs: Vec::new(),
            metrics: json!({ "fixture": true }),
            payload: json!({}),
            created_at: now,
        }
    }

    fn phase9_monitor(
        scope: &LearningScope,
        monitor_id: &str,
        promotion_id: &str,
        candidate_id: &str,
        proposal_id: &str,
        rollback_recommendation_id: Option<&str>,
        follow_up_candidate_id: Option<&str>,
        now: DateTime<Utc>,
    ) -> LearningCapabilityEvolutionPostPromotionMonitorRecord {
        LearningCapabilityEvolutionPostPromotionMonitorRecord {
            id: monitor_id.to_string(),
            scope: scope.clone(),
            promotion_id: promotion_id.to_string(),
            candidate_id: candidate_id.to_string(),
            proposal_id: proposal_id.to_string(),
            validation_id: format!("validation_{candidate_id}"),
            implementation_id: None,
            application_id: Some(format!("application_{candidate_id}")),
            capability_id: Some("skill:fixture".to_string()),
            status: LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected,
            summary: "Fixture regression monitor.".to_string(),
            skill_names: vec!["fixture".to_string()],
            before_invocation_count: 3,
            after_invocation_count: 3,
            before_success_count: 3,
            after_success_count: 0,
            before_failure_count: 0,
            after_failure_count: 3,
            before_success_rate: Some(1.0),
            after_success_rate: Some(0.0),
            same_failure_recurrence_count: 1,
            new_failure_classes: vec!["tool_misuse".to_string()],
            user_negative_feedback_count: 0,
            rollback_recommendation_id: rollback_recommendation_id.map(str::to_string),
            follow_up_candidate_id: follow_up_candidate_id.map(str::to_string),
            evidence_refs: Vec::new(),
            payload: json!({}),
            created_at: now,
            updated_at: now,
        }
    }

    fn phase9_rollback_recommendation(
        scope: &LearningScope,
        recommendation_id: &str,
        candidate_id: &str,
        proposal_id: &str,
        application_id: &str,
        now: DateTime<Utc>,
    ) -> LearningCapabilityEvolutionRollbackRecommendationRecord {
        LearningCapabilityEvolutionRollbackRecommendationRecord {
            id: recommendation_id.to_string(),
            scope: scope.clone(),
            candidate_id: candidate_id.to_string(),
            proposal_id: proposal_id.to_string(),
            validation_id: Some(format!("validation_{candidate_id}")),
            implementation_id: None,
            application_id: application_id.to_string(),
            promotion_id: Some(format!("promotion_{candidate_id}")),
            capability_id: Some("skill:fixture".to_string()),
            status: LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended,
            trigger_kind: "post_promotion_regression".to_string(),
            severity: "high".to_string(),
            actor: "test".to_string(),
            summary: "Fixture rollback recommendation.".to_string(),
            rollback_files: vec!["skills/fixture/SKILL.md".to_string()],
            evidence_refs: Vec::new(),
            payload: json!({}),
            created_at: now,
        }
    }

    fn active_procedure(
        store: &LearningStore,
        scope: &LearningScope,
        id: &str,
        success_count: u64,
        failure_count: u64,
    ) -> LearningProcedure {
        let procedure = store
            .create_procedure(
                scope.clone(),
                CreateLearningProcedureRequest {
                    principal: None,
                    workspace: None,
                    id: Some(id.to_string()),
                    actor: "test".to_string(),
                    reason: Some("fixture".to_string()),
                    status: LearningProcedureStatus::Draft,
                    title: "Analyze dashboard variance".to_string(),
                    summary: "A recurring dashboard variance analysis workflow.".to_string(),
                    owner_agent: Some("simple-data-analyst".to_string()),
                    activation: LearningProcedureActivation {
                        use_when: vec!["Dashboard totals disagree with source tables.".to_string()],
                        avoid_when: vec!["The user only needs a one-off explanation.".to_string()],
                        example_goals: vec!["Find why dashboard revenue changed.".to_string()],
                    },
                    workflow: vec![
                        "Inspect the dashboard card query.".to_string(),
                        "Run the query against the source database.".to_string(),
                        "Compare transformed totals with the dashboard.".to_string(),
                    ],
                    decision_points: vec![
                        "Use Metabase first when dashboard ids are known.".to_string()
                    ],
                    verification: vec![
                        "The final totals reconcile or the discrepancy is explained.".to_string(),
                    ],
                    failure_modes: vec![
                        "Do not save a card before the query is validated.".to_string()
                    ],
                    evidence_refs: vec![
                        LearningEvidenceRef {
                            kind: "task".to_string(),
                            id: Some("task_one".to_string()),
                            path: None,
                            uri: None,
                            summary: Some("First successful run".to_string()),
                        },
                        LearningEvidenceRef {
                            kind: "task".to_string(),
                            id: Some("task_two".to_string()),
                            path: None,
                            uri: None,
                            summary: Some("Second successful run".to_string()),
                        },
                    ],
                    source_candidate_id: Some(format!("lc_{id}")),
                    source_task_ids: vec!["task_one".to_string(), "task_two".to_string()],
                    source_chat_session_ids: Vec::new(),
                    success_count,
                    failure_count,
                    payload: json!({
                        "workflow_signature": "agent:simple-data-analyst|tools:metabase>duckdb"
                    }),
                },
            )
            .expect("create procedure");
        store
            .transition_procedure_status(
                scope,
                &procedure.id,
                LearningProcedureStatus::Active,
                "test",
                "activate",
                "activate fixture procedure",
                procedure.evidence_refs.clone(),
            )
            .expect("activate procedure")
    }

    fn procedure_update_candidate(
        store: &LearningStore,
        scope: &LearningScope,
        procedure_id: &str,
    ) {
        store
            .create_candidate(
                scope.clone(),
                CreateLearningCandidateRequest {
                    principal: None,
                    workspace: None,
                    candidate_type: LearningCandidateType::MemoryProcedure,
                    state: LearningCandidateState::Proposed,
                    title: "Update reusable procedure".to_string(),
                    summary: "Fixture procedure update candidate.".to_string(),
                    rationale: "fixture".to_string(),
                    proposed_change: json!({
                        "procedure": {
                            "existing_procedure_id": procedure_id,
                            "workflow": ["Use the corrected workflow next time."]
                        }
                    }),
                    proposed_target: Some(format!("procedure:{procedure_id}")),
                    confidence: Some(0.9),
                    source_agent_id: Some("simple-data-analyst".to_string()),
                    source_task_id: Some("task_candidate".to_string()),
                    source_execution_id: None,
                    source_chat_session_id: None,
                    event_refs: Vec::new(),
                    evidence_refs: Vec::new(),
                    risk_level: LearningRiskLevel::Low,
                    review_required: false,
                    review_reason: None,
                    review_policy: json!({}),
                    promotion_target: Some("procedure".to_string()),
                    promotion_policy: json!({}),
                },
            )
            .expect("create procedure update candidate");
    }
}
