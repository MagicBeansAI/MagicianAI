use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::{
    CreateLearningCandidateRequest, CreateLearningEventRequest, LearningCandidate,
    LearningCandidateFilters, LearningCandidateState, LearningCandidateType,
    LearningCapabilityEvolutionBridge, LearningEvaluationBacklogItem,
    LearningEvaluationBacklogStatus, LearningEvidenceRef, LearningProcedure,
    LearningProcedureStatus, LearningRiskLevel, LearningScope, LearningStore,
};

const PROCEDURE_SKILL_PROMOTION_SOURCE: &str = "learning_procedure_skill_promotion";
const DEFAULT_MIN_SUCCESS_COUNT: u64 = 3;
const DEFAULT_MIN_EVIDENCE_COUNT: usize = 2;
const MAX_PROMOTION_HISTORY_ITEMS: usize = 20;

#[derive(Debug, Clone, Deserialize)]
pub struct PromoteLearningProcedureToSkillRequest {
    pub principal: Option<String>,
    pub workspace: Option<String>,
    #[serde(default = "default_actor")]
    pub actor: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub target_skill: Option<String>,
    #[serde(default)]
    pub candidate_type: Option<LearningCandidateType>,
    #[serde(default)]
    pub min_success_count: Option<u64>,
    #[serde(default)]
    pub min_evidence_count: Option<usize>,
    #[serde(default)]
    pub force: bool,
    #[serde(default = "default_true")]
    pub route_to_backlog: bool,
    #[serde(default = "default_true")]
    pub create_eval_candidate: bool,
    #[serde(default)]
    pub payload: Value,
}

impl Default for PromoteLearningProcedureToSkillRequest {
    fn default() -> Self {
        Self {
            principal: None,
            workspace: None,
            actor: default_actor(),
            reason: None,
            target_skill: None,
            candidate_type: None,
            min_success_count: None,
            min_evidence_count: None,
            force: false,
            route_to_backlog: true,
            create_eval_candidate: true,
            payload: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LearningProcedureSkillPromotionOutcome {
    pub procedure_id: String,
    pub procedure_path: Option<String>,
    pub eligible: bool,
    pub reason: String,
    pub target_skill: Option<String>,
    pub promotion_candidate_id: Option<String>,
    pub promotion_candidate_type: Option<LearningCandidateType>,
    pub capability_backlog_path: Option<String>,
    pub eval_candidate_id: Option<String>,
    pub eval_backlog_candidate_id: Option<String>,
    pub eval_backlog_path: Option<String>,
    pub event_ids: Vec<String>,
    pub reused_existing_candidate: bool,
}

#[derive(Debug, Clone)]
pub struct LearningProcedureSkillPromotionBridge {
    workspace_layout: ArtifactV2Workspace,
}

impl LearningProcedureSkillPromotionBridge {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub fn promote_procedure(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        procedure_id: &str,
        request: PromoteLearningProcedureToSkillRequest,
    ) -> Result<LearningProcedureSkillPromotionOutcome> {
        let procedure = store.read_procedure(scope, procedure_id)?;
        self.promote_loaded_procedure(store, scope, &procedure, request)
    }

    pub fn maybe_promote_after_feedback(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        procedure: &LearningProcedure,
    ) -> Result<LearningProcedureSkillPromotionOutcome> {
        self.promote_loaded_procedure(
            store,
            scope,
            procedure,
            PromoteLearningProcedureToSkillRequest::default(),
        )
    }

    fn promote_loaded_procedure(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        procedure: &LearningProcedure,
        request: PromoteLearningProcedureToSkillRequest,
    ) -> Result<LearningProcedureSkillPromotionOutcome> {
        let procedure_path = self.procedure_path(procedure);
        let target_skill = request
            .target_skill
            .as_deref()
            .and_then(skill_slug)
            .unwrap_or_else(|| default_skill_slug(procedure));
        let candidate_type = request
            .candidate_type
            .clone()
            .filter(LearningCandidateType::is_skill_or_workflow_candidate)
            .unwrap_or(LearningCandidateType::WorkflowTemplate);

        let eligibility = procedure_skill_promotion_eligibility(procedure, &request);
        if !eligibility.eligible {
            return Ok(LearningProcedureSkillPromotionOutcome {
                procedure_id: procedure.id.clone(),
                procedure_path: Some(procedure_path),
                eligible: false,
                reason: eligibility.reason,
                target_skill: Some(target_skill),
                promotion_candidate_id: None,
                promotion_candidate_type: None,
                capability_backlog_path: None,
                eval_candidate_id: None,
                eval_backlog_candidate_id: None,
                eval_backlog_path: None,
                event_ids: Vec::new(),
                reused_existing_candidate: false,
            });
        }

        if let Some(existing) = find_existing_open_promotion_candidate(store, scope, procedure)? {
            let target_skill = candidate_target_skill(&existing).unwrap_or(target_skill);
            let existing_routing = self.existing_promotion_routing(
                store,
                scope,
                procedure,
                &existing,
                request.route_to_backlog,
                request.create_eval_candidate,
            )?;
            if existing_routing.is_complete(request.route_to_backlog, request.create_eval_candidate)
            {
                return Ok(LearningProcedureSkillPromotionOutcome {
                    procedure_id: procedure.id.clone(),
                    procedure_path: Some(procedure_path),
                    eligible: true,
                    reason: "existing_procedure_skill_promotion_candidate_reused_without_changes"
                        .to_string(),
                    target_skill: Some(target_skill),
                    promotion_candidate_id: Some(existing.id.clone()),
                    promotion_candidate_type: Some(existing.candidate_type.clone()),
                    capability_backlog_path: existing_routing.capability_backlog_path,
                    eval_candidate_id: None,
                    eval_backlog_candidate_id: existing_routing
                        .eval_backlog_path
                        .as_ref()
                        .map(|_| existing.id.clone()),
                    eval_backlog_path: existing_routing.eval_backlog_path,
                    event_ids: Vec::new(),
                    reused_existing_candidate: true,
                });
            }
            if !candidate_allows_automatic_reconciliation(&existing) {
                return Ok(LearningProcedureSkillPromotionOutcome {
                    procedure_id: procedure.id.clone(),
                    procedure_path: Some(procedure_path),
                    eligible: true,
                    reason:
                        "existing_progressed_procedure_skill_promotion_candidate_reused_read_only"
                            .to_string(),
                    target_skill: Some(target_skill),
                    promotion_candidate_id: Some(existing.id.clone()),
                    promotion_candidate_type: Some(existing.candidate_type.clone()),
                    capability_backlog_path: existing_routing.capability_backlog_path,
                    eval_candidate_id: None,
                    eval_backlog_candidate_id: existing_routing
                        .eval_backlog_path
                        .as_ref()
                        .map(|_| existing.id.clone()),
                    eval_backlog_path: existing_routing.eval_backlog_path,
                    event_ids: Vec::new(),
                    reused_existing_candidate: true,
                });
            }
            let capability_backlog_path =
                self.ensure_capability_backlog(store, scope, &existing, request.route_to_backlog)?;
            let eval_backlog_path = self.ensure_promotion_eval_backlog(
                store,
                scope,
                procedure,
                &procedure_path,
                &target_skill,
                &existing,
                request.create_eval_candidate,
            )?;
            let evidence_refs = promotion_evidence_refs(procedure, &procedure_path);
            let updated = store.update_procedure(
                scope,
                &procedure.id,
                request.actor.clone(),
                "phase_14_skill_promotion_candidate_reconciled",
                format!(
                    "Procedure-to-skill promotion candidate `{}` was reconciled for target skill `{}`.",
                    existing.id, target_skill
                ),
                evidence_refs.clone(),
                |procedure| {
                    append_promotion_payload(
                        procedure,
                        &existing,
                        None,
                        eval_backlog_path.as_ref().map(|_| existing.id.as_str()),
                        &target_skill,
                        capability_backlog_path.as_deref(),
                        eval_backlog_path.as_deref(),
                        request.force,
                    );
                },
            )?;
            let reconciled_event = store.append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_skill_promotion_reconciled".to_string(),
                    agent_id: updated.owner_agent.clone(),
                    task_id: updated.source_task_ids.last().cloned(),
                    execution_id: None,
                    chat_session_id: updated.source_chat_session_ids.last().cloned(),
                    summary: format!(
                        "Procedure `{}` reused and reconciled skill promotion candidate `{}`.",
                        updated.id, existing.id
                    ),
                    evidence_refs,
                    payload: json!({
                        "procedure_id": updated.id,
                        "procedure_path": self.procedure_path(&updated),
                        "promotion_candidate_id": existing.id.clone(),
                        "target_skill": target_skill.clone(),
                        "capability_backlog_path": capability_backlog_path.clone(),
                        "eval_backlog_candidate_id": eval_backlog_path.as_ref().map(|_| existing.id.clone()),
                        "eval_backlog_path": eval_backlog_path.clone(),
                        "actor": request.actor.clone(),
                    }),
                },
            )?;
            return Ok(LearningProcedureSkillPromotionOutcome {
                procedure_id: updated.id.clone(),
                procedure_path: Some(self.procedure_path(&updated)),
                eligible: true,
                reason: "existing_open_procedure_skill_promotion_candidate_reused".to_string(),
                target_skill: Some(target_skill),
                promotion_candidate_id: Some(existing.id.clone()),
                promotion_candidate_type: Some(existing.candidate_type.clone()),
                capability_backlog_path,
                eval_candidate_id: None,
                eval_backlog_candidate_id: eval_backlog_path.as_ref().map(|_| existing.id.clone()),
                eval_backlog_path,
                event_ids: vec![reconciled_event.id],
                reused_existing_candidate: true,
            });
        }
        if let Some(promoted) = find_existing_promoted_promotion_candidate(store, scope, procedure)?
        {
            let target_skill = candidate_target_skill(&promoted).unwrap_or(target_skill);
            let existing_routing = self.existing_promotion_routing(
                store,
                scope,
                procedure,
                &promoted,
                request.route_to_backlog,
                request.create_eval_candidate,
            )?;
            return Ok(LearningProcedureSkillPromotionOutcome {
                procedure_id: procedure.id.clone(),
                procedure_path: Some(procedure_path),
                eligible: true,
                reason: "existing_promoted_procedure_skill_promotion_candidate_reused_read_only"
                    .to_string(),
                target_skill: Some(target_skill),
                promotion_candidate_id: Some(promoted.id.clone()),
                promotion_candidate_type: Some(promoted.candidate_type.clone()),
                capability_backlog_path: existing_routing.capability_backlog_path,
                eval_candidate_id: None,
                eval_backlog_candidate_id: existing_routing
                    .eval_backlog_path
                    .as_ref()
                    .map(|_| promoted.id.clone()),
                eval_backlog_path: existing_routing.eval_backlog_path,
                event_ids: Vec::new(),
                reused_existing_candidate: true,
            });
        }

        let evidence_refs = promotion_evidence_refs(procedure, &procedure_path);
        let promotion_candidate = store.create_candidate(
            scope.clone(),
            CreateLearningCandidateRequest {
                principal: None,
                workspace: None,
                candidate_type: candidate_type.clone(),
                state: LearningCandidateState::Proposed,
                title: format!("Promote procedure to skill: {}", procedure.title),
                summary: format!(
                    "Procedure `{}` has enough repeated successful evidence to become review-gated skill guidance.",
                    procedure.id
                ),
                rationale: promotion_rationale(procedure, &eligibility.reason, &request),
                proposed_change: promotion_candidate_payload(
                    procedure,
                    &procedure_path,
                    &target_skill,
                    &candidate_type,
                    &request,
                ),
                proposed_target: Some(format!("skill:{target_skill}")),
                confidence: Some(promotion_confidence(procedure)),
                source_agent_id: procedure.owner_agent.clone(),
                source_task_id: procedure.source_task_ids.last().cloned(),
                source_execution_id: None,
                source_chat_session_id: procedure.source_chat_session_ids.last().cloned(),
                event_refs: Vec::new(),
                evidence_refs: evidence_refs.clone(),
                risk_level: LearningRiskLevel::Medium,
                review_required: true,
                review_reason: Some(
                    "Procedure-to-skill promotion must pass review, eval, validation, application, and promotion gates."
                        .to_string(),
                ),
                review_policy: json!({
                    "requires_review": true,
                    "source": "procedure_to_skill_promotion",
                    "source_procedure_id": procedure.id,
                    "minimum_success_count": request.min_success_count.unwrap_or(DEFAULT_MIN_SUCCESS_COUNT),
                    "minimum_evidence_count": request.min_evidence_count.unwrap_or(DEFAULT_MIN_EVIDENCE_COUNT),
                }),
                promotion_target: Some("skill_evolution".to_string()),
                promotion_policy: json!({
                    "eligible_for_auto_promotion": false,
                    "requires_eval_backlog": true,
                    "requires_validation": true,
                    "requires_application": true,
                    "source_procedure_id": procedure.id,
                }),
            },
        )?;

        let created_event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_procedure_skill_promotion_candidate_created".to_string(),
                agent_id: procedure.owner_agent.clone(),
                task_id: procedure.source_task_ids.last().cloned(),
                execution_id: None,
                chat_session_id: procedure.source_chat_session_ids.last().cloned(),
                summary: format!(
                    "Procedure `{}` created skill promotion candidate `{}`.",
                    procedure.id, promotion_candidate.id
                ),
                evidence_refs: evidence_refs.clone(),
                payload: json!({
                    "procedure_id": procedure.id,
                    "procedure_path": procedure_path,
                    "candidate_id": promotion_candidate.id,
                    "candidate_type": promotion_candidate.candidate_type.as_str(),
                    "target_skill": target_skill,
                    "eligibility_reason": eligibility.reason,
                    "actor": request.actor,
                    "force": request.force,
                }),
            },
        )?;
        let mut event_ids = vec![created_event.id.clone()];

        let capability_backlog_path = self.ensure_capability_backlog(
            store,
            scope,
            &promotion_candidate,
            request.route_to_backlog,
        )?;
        let eval_backlog_path = self.ensure_promotion_eval_backlog(
            store,
            scope,
            procedure,
            &procedure_path,
            &target_skill,
            &promotion_candidate,
            request.create_eval_candidate,
        )?;
        let eval_backlog_candidate_id = eval_backlog_path
            .as_ref()
            .map(|_| promotion_candidate.id.clone());

        let updated = store.update_procedure(
            scope,
            &procedure.id,
            request.actor.clone(),
            "phase_14_skill_promotion_candidate_created",
            format!(
                "Procedure-to-skill promotion candidate `{}` was created for target skill `{}`.",
                promotion_candidate.id, target_skill
            ),
            evidence_refs.clone(),
            |procedure| {
                append_promotion_payload(
                    procedure,
                    &promotion_candidate,
                    None,
                    eval_backlog_candidate_id.as_deref(),
                    &target_skill,
                    capability_backlog_path.as_deref(),
                    eval_backlog_path.as_deref(),
                    request.force,
                );
            },
        )?;

        let routed_event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_procedure_skill_promotion_routed".to_string(),
                agent_id: updated.owner_agent.clone(),
                task_id: updated.source_task_ids.last().cloned(),
                execution_id: None,
                chat_session_id: updated.source_chat_session_ids.last().cloned(),
                summary: format!(
                    "Procedure `{}` skill promotion was routed through review/eval backlogs.",
                    updated.id
                ),
                evidence_refs,
                payload: json!({
                    "procedure_id": updated.id,
                    "procedure_status": updated.status.as_str(),
                    "procedure_version": updated.version,
                    "target_skill": target_skill.clone(),
                    "promotion_candidate_id": promotion_candidate.id.clone(),
                    "capability_backlog_path": capability_backlog_path.clone(),
                    "eval_candidate_id": Option::<String>::None,
                    "eval_backlog_candidate_id": eval_backlog_candidate_id.clone(),
                    "eval_backlog_path": eval_backlog_path.clone(),
                    "actor": request.actor.clone(),
                }),
            },
        )?;
        event_ids.push(routed_event.id);

        Ok(LearningProcedureSkillPromotionOutcome {
            procedure_id: updated.id.clone(),
            procedure_path: Some(self.procedure_path(&updated)),
            eligible: true,
            reason: "procedure_skill_promotion_routed".to_string(),
            target_skill: Some(target_skill),
            promotion_candidate_id: Some(promotion_candidate.id),
            promotion_candidate_type: Some(promotion_candidate.candidate_type),
            capability_backlog_path,
            eval_candidate_id: None,
            eval_backlog_candidate_id,
            eval_backlog_path,
            event_ids,
            reused_existing_candidate: false,
        })
    }

    fn ensure_capability_backlog(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        route_to_backlog: bool,
    ) -> Result<Option<String>> {
        if !route_to_backlog {
            return Ok(None);
        }
        let route = LearningCapabilityEvolutionBridge::new(self.workspace_layout.clone())
            .route_candidate(store, scope, candidate)?;
        Ok(route.backlog_path)
    }

    fn ensure_promotion_eval_backlog(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        procedure: &LearningProcedure,
        procedure_path: &str,
        target_skill: &str,
        promotion_candidate: &LearningCandidate,
        create_eval_backlog: bool,
    ) -> Result<Option<String>> {
        if !create_eval_backlog {
            return Ok(None);
        }
        let existing = match store.read_evaluation_backlog_item(scope, &promotion_candidate.id) {
            Ok(value) => Some(value),
            Err(error) if store.error_is_not_found(&error) => None,
            Err(error) => return Err(error),
        };
        let item = build_promotion_evaluation_backlog_item(
            scope,
            procedure,
            procedure_path,
            target_skill,
            promotion_candidate,
            existing.as_ref(),
        );
        store.write_evaluation_backlog_item(&item)?;
        Ok(Some(
            self.workspace_layout
                .learning_evaluation_backlog_path(
                    &scope.principal,
                    &scope.workspace,
                    &promotion_candidate.id,
                )
                .to_string_lossy()
                .to_string(),
        ))
    }

    fn existing_promotion_routing(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        procedure: &LearningProcedure,
        candidate: &LearningCandidate,
        requires_capability_backlog: bool,
        requires_eval_backlog: bool,
    ) -> Result<ExistingPromotionRouting> {
        let capability_backlog_path = if requires_capability_backlog {
            match store.read_capability_evolution_backlog_item(scope, &candidate.id) {
                Ok(_) => {
                    let skill_path = self.workspace_layout.skill_evolution_backlog_path(
                        &scope.principal,
                        &scope.workspace,
                        &candidate.id,
                    );
                    let path = if skill_path.exists() {
                        skill_path
                    } else {
                        self.workspace_layout.capability_evolution_backlog_path(
                            &scope.principal,
                            &scope.workspace,
                            &candidate.id,
                        )
                    };
                    Some(path.to_string_lossy().to_string())
                },
                Err(error) if store.error_is_not_found(&error) => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let eval_backlog_path = if requires_eval_backlog {
            match store.read_evaluation_backlog_item(scope, &candidate.id) {
                Ok(_) => Some(
                    self.workspace_layout
                        .learning_evaluation_backlog_path(
                            &scope.principal,
                            &scope.workspace,
                            &candidate.id,
                        )
                        .to_string_lossy()
                        .to_string(),
                ),
                Err(error) if store.error_is_not_found(&error) => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };

        Ok(ExistingPromotionRouting {
            capability_backlog_path,
            eval_backlog_path,
            procedure_payload_recorded: procedure_payload_records_promotion(
                procedure,
                &candidate.id,
                requires_capability_backlog,
                requires_eval_backlog,
            ),
        })
    }

    fn procedure_path(&self, procedure: &LearningProcedure) -> String {
        self.workspace_layout
            .learning_procedure_path(
                &procedure.scope.principal,
                &procedure.scope.workspace,
                procedure.status.as_str(),
                &procedure.id,
            )
            .to_string_lossy()
            .to_string()
    }
}

#[derive(Debug)]
struct PromotionEligibility {
    eligible: bool,
    reason: String,
}

#[derive(Debug, Clone, Default)]
struct ExistingPromotionRouting {
    capability_backlog_path: Option<String>,
    eval_backlog_path: Option<String>,
    procedure_payload_recorded: bool,
}

impl ExistingPromotionRouting {
    fn is_complete(&self, requires_capability_backlog: bool, requires_eval_backlog: bool) -> bool {
        self.procedure_payload_recorded
            && (!requires_capability_backlog || self.capability_backlog_path.is_some())
            && (!requires_eval_backlog || self.eval_backlog_path.is_some())
    }
}

fn procedure_skill_promotion_eligibility(
    procedure: &LearningProcedure,
    request: &PromoteLearningProcedureToSkillRequest,
) -> PromotionEligibility {
    if procedure.status != LearningProcedureStatus::Active {
        return PromotionEligibility {
            eligible: false,
            reason: format!(
                "procedure_status_is_not_active:{}",
                procedure.status.as_str()
            ),
        };
    }
    if request.force {
        return PromotionEligibility {
            eligible: true,
            reason: "force_requested_review_gated_promotion".to_string(),
        };
    }
    let min_success_count = request
        .min_success_count
        .unwrap_or(DEFAULT_MIN_SUCCESS_COUNT)
        .max(2);
    if procedure.success_count < min_success_count {
        return PromotionEligibility {
            eligible: false,
            reason: format!(
                "success_count_below_threshold:{}<{}",
                procedure.success_count, min_success_count
            ),
        };
    }
    if procedure.failure_count >= procedure.success_count {
        return PromotionEligibility {
            eligible: false,
            reason: format!(
                "failure_pressure_too_high:{}>={}",
                procedure.failure_count, procedure.success_count
            ),
        };
    }
    let min_evidence_count = request
        .min_evidence_count
        .unwrap_or(DEFAULT_MIN_EVIDENCE_COUNT)
        .max(1);
    let evidence_count = procedure_evidence_count(procedure);
    if evidence_count < min_evidence_count {
        return PromotionEligibility {
            eligible: false,
            reason: format!("evidence_count_below_threshold:{evidence_count}<{min_evidence_count}"),
        };
    }
    if procedure.workflow.is_empty() {
        return PromotionEligibility {
            eligible: false,
            reason: "procedure_has_no_workflow_steps".to_string(),
        };
    }
    PromotionEligibility {
        eligible: true,
        reason: "repeated_successful_active_procedure".to_string(),
    }
}

fn procedure_evidence_count(procedure: &LearningProcedure) -> usize {
    procedure
        .evidence_refs
        .iter()
        .filter_map(|evidence| evidence.id.as_deref().or(evidence.path.as_deref()))
        .chain(procedure.source_task_ids.iter().map(String::as_str))
        .chain(procedure.source_chat_session_ids.iter().map(String::as_str))
        .collect::<std::collections::HashSet<_>>()
        .len()
}

fn promotion_candidate_payload(
    procedure: &LearningProcedure,
    procedure_path: &str,
    target_skill: &str,
    candidate_type: &LearningCandidateType,
    request: &PromoteLearningProcedureToSkillRequest,
) -> Value {
    let payload = json!({
        "source": "procedure_to_skill_promotion",
        "source_procedure_id": procedure.id,
        "source_procedure_path": procedure_path,
        "source_procedure_status": procedure.status.as_str(),
        "source_procedure_version": procedure.version,
        "source_procedure_success_count": procedure.success_count,
        "source_procedure_failure_count": procedure.failure_count,
        "source_procedure_last_used_at": procedure.last_used_at.as_ref().map(|value| value.to_rfc3339()),
        "skill_name": target_skill,
        "target_skill": target_skill,
        "workflow_signature": procedure.payload.get("workflow_signature").cloned(),
        "trigger_conditions": procedure.activation.use_when,
        "when_not_to_use": procedure.activation.avoid_when,
        "examples": procedure.activation.example_goals,
        "procedure_steps": procedure.workflow,
        "decision_points": procedure.decision_points,
        "verification_steps": procedure.verification,
        "common_failure_modes": procedure.failure_modes,
        "proposed_files": [format!("skills/{target_skill}/SKILL.md")],
        "required_eval": procedure_promotion_eval_plan(procedure, target_skill),
        "promotion_gate": procedure_promotion_gate(procedure),
        "expected_behavior": format!(
            "The `{target_skill}` skill captures the proven reusable procedure `{}` without weakening its activation boundaries or verification checks.",
            procedure.id
        ),
        "source_refs": procedure_source_refs(procedure, procedure_path),
        "operator_request": request.payload,
    });
    if *candidate_type == LearningCandidateType::SkillUpdate {
        json!({ "skill_update": payload })
    } else {
        json!({ "workflow_template": payload })
    }
}

fn build_promotion_evaluation_backlog_item(
    scope: &LearningScope,
    procedure: &LearningProcedure,
    procedure_path: &str,
    target_skill: &str,
    promotion_candidate: &LearningCandidate,
    existing: Option<&LearningEvaluationBacklogItem>,
) -> LearningEvaluationBacklogItem {
    let now = Utc::now();
    let created_at = existing.map(|item| item.created_at).unwrap_or(now);
    let status = existing
        .map(|item| item.status.clone())
        .unwrap_or(LearningEvaluationBacklogStatus::Queued);
    LearningEvaluationBacklogItem {
        id: format!(
            "leb_{}",
            promotion_candidate
                .id
                .strip_prefix("lc_")
                .unwrap_or(&promotion_candidate.id)
        ),
        scope: scope.clone(),
        candidate_id: promotion_candidate.id.clone(),
        status,
        title: format!("Evaluate skill promotion for {}", procedure.title),
        summary: format!(
            "Regression eval for promoting procedure `{}` into skill `{}`.",
            procedure.id, target_skill
        ),
        rationale: "Procedure-to-skill promotion needs meta-harness evidence before any skill file is applied."
            .to_string(),
        case_kind: "procedure_skill_promotion_regression".to_string(),
        priority: "normal".to_string(),
        target_agent_id: procedure.owner_agent.clone(),
        focus_area: Some(target_skill.to_string()),
        proposed_target: Some(format!("skill:{target_skill}")),
        source_agent_id: procedure.owner_agent.clone(),
        source_task_id: procedure.source_task_ids.last().cloned(),
        source_execution_id: None,
        source_chat_session_id: procedure.source_chat_session_ids.last().cloned(),
        evidence_refs: promotion_evidence_refs(procedure, procedure_path),
        case_spec: json!({
            "source": "procedure_to_skill_promotion",
            "candidate_id": promotion_candidate.id,
            "promotion_candidate_id": promotion_candidate.id,
            "source_procedure_id": procedure.id,
            "source_procedure_path": procedure_path,
            "source_procedure_version": procedure.version,
            "target_skill": target_skill,
            "case_kind": "procedure_skill_promotion_regression",
            "priority": "normal",
            "target_agent_id": procedure.owner_agent,
            "focus_area": target_skill,
            "goal": format!("Validate promoted skill `{target_skill}` preserves procedure `{}` behavior.", procedure.id),
            "failure_mode": "promoted skill over-applies, omits verification, or loses procedure boundaries",
            "expected_behavior": "The promoted skill should activate only for matching goals, follow the learned procedure steps, verify results, and avoid known failure modes.",
            "reproduction_steps": procedure.workflow,
            "success_criteria": if procedure.verification.is_empty() {
                vec![
                    "Skill guidance contains source-procedure provenance.".to_string(),
                    "Skill guidance includes activation boundaries and workflow steps.".to_string(),
                    "Reviewer records validation evidence before promotion.".to_string(),
                ]
            } else {
                procedure.verification.clone()
            },
            "eval_plan": procedure_promotion_eval_plan(procedure, target_skill),
            "promotion_gate": procedure_promotion_gate(procedure),
            "source_refs": procedure_source_refs(procedure, procedure_path),
            "meta_harness": {
                "required": true,
                "source": "procedure_to_skill_promotion",
                "review_goal": "Validate that the promoted skill preserves the learned procedure and does not over-apply."
            }
        }),
        created_at,
        updated_at: now,
    }
}

fn procedure_promotion_eval_plan(procedure: &LearningProcedure, target_skill: &str) -> Value {
    json!({
        "source": "procedure_to_skill_promotion",
        "case_kind": "procedure_skill_promotion_regression",
        "priority": "normal",
        "target_agent_id": procedure.owner_agent,
        "focus_area": target_skill,
        "goal": format!("Validate procedure `{}` can graduate into skill `{target_skill}`.", procedure.id),
        "expected_behavior": "The generated skill preserves activation, workflow, decision, verification, failure-mode, and provenance guidance from the procedure.",
        "reproduction_steps": procedure.workflow,
        "success_criteria": if procedure.verification.is_empty() {
            vec![
                "Promotion includes source procedure provenance.".to_string(),
                "Promotion remains review-gated until validation evidence exists.".to_string(),
            ]
        } else {
            procedure.verification.clone()
        },
    })
}

fn procedure_promotion_gate(procedure: &LearningProcedure) -> Value {
    json!({
        "source": "procedure_to_skill_promotion",
        "source_procedure_id": procedure.id,
        "human_review_required": true,
        "meta_harness_review_required": true,
        "local_validation_required": true,
        "regression_required": true,
        "application_required_for_scoped_skills": true,
        "minimum_success_count": DEFAULT_MIN_SUCCESS_COUNT,
        "current_success_count": procedure.success_count,
        "current_failure_count": procedure.failure_count,
    })
}

fn promotion_evidence_refs(
    procedure: &LearningProcedure,
    procedure_path: &str,
) -> Vec<LearningEvidenceRef> {
    let mut refs = vec![LearningEvidenceRef {
        kind: "learning_procedure".to_string(),
        id: Some(procedure.id.clone()),
        path: Some(procedure_path.to_string()),
        uri: None,
        summary: Some(procedure.summary.clone()),
    }];
    refs.extend(procedure.evidence_refs.iter().cloned());
    refs.truncate(24);
    refs
}

fn procedure_source_refs(procedure: &LearningProcedure, procedure_path: &str) -> Vec<Value> {
    let mut refs = vec![json!({
        "kind": "learning_procedure",
        "id": procedure.id,
        "path": procedure_path,
        "summary": procedure.summary,
    })];
    refs.extend(procedure.evidence_refs.iter().take(12).map(|evidence| {
        json!({
            "kind": evidence.kind,
            "id": evidence.id,
            "path": evidence.path,
            "uri": evidence.uri,
            "summary": evidence.summary,
        })
    }));
    refs
}

fn promotion_rationale(
    procedure: &LearningProcedure,
    eligibility_reason: &str,
    request: &PromoteLearningProcedureToSkillRequest,
) -> String {
    let operator_reason = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("No additional operator reason supplied.");
    format!(
        "{} Procedure has {} success(es), {} failure(s), {} evidence item(s), and {} workflow step(s). {}",
        eligibility_reason,
        procedure.success_count,
        procedure.failure_count,
        procedure_evidence_count(procedure),
        procedure.workflow.len(),
        operator_reason
    )
}

fn promotion_confidence(procedure: &LearningProcedure) -> f64 {
    let successes = procedure.success_count as f64;
    let failures = procedure.failure_count as f64;
    let total = (successes + failures).max(1.0);
    ((successes / total) * 0.4 + 0.55).clamp(0.55, 0.95)
}

fn find_existing_open_promotion_candidate(
    store: &LearningStore,
    scope: &LearningScope,
    procedure: &LearningProcedure,
) -> Result<Option<LearningCandidate>> {
    let candidates = store.list_candidates(
        scope,
        LearningCandidateFilters {
            candidate_type: None,
            state: None,
            source_agent_id: procedure.owner_agent.clone(),
            limit: Some(2_000),
        },
    )?;
    Ok(candidates.into_iter().find(|candidate| {
        candidate.candidate_type.is_skill_or_workflow_candidate()
            && !candidate.state.is_terminal()
            && candidate_references_procedure(candidate, &procedure.id)
    }))
}

fn find_existing_promoted_promotion_candidate(
    store: &LearningStore,
    scope: &LearningScope,
    procedure: &LearningProcedure,
) -> Result<Option<LearningCandidate>> {
    let candidates = store.list_candidates(
        scope,
        LearningCandidateFilters {
            candidate_type: None,
            state: Some(LearningCandidateState::Promoted.as_str().to_string()),
            source_agent_id: procedure.owner_agent.clone(),
            limit: Some(2_000),
        },
    )?;
    Ok(candidates.into_iter().find(|candidate| {
        candidate.candidate_type.is_skill_or_workflow_candidate()
            && candidate_references_procedure(candidate, &procedure.id)
    }))
}

fn candidate_allows_automatic_reconciliation(candidate: &LearningCandidate) -> bool {
    matches!(
        candidate.state,
        LearningCandidateState::Observed
            | LearningCandidateState::Proposed
            | LearningCandidateState::Triaged
    )
}

fn candidate_references_procedure(candidate: &LearningCandidate, procedure_id: &str) -> bool {
    let value = &candidate.proposed_change;
    string_value_matches(value.get("source_procedure_id"), procedure_id)
        || value
            .get("workflow_template")
            .is_some_and(|payload| nested_candidate_references_procedure(payload, procedure_id))
        || value
            .get("skill_update")
            .is_some_and(|payload| nested_candidate_references_procedure(payload, procedure_id))
}

fn nested_candidate_references_procedure(payload: &Value, procedure_id: &str) -> bool {
    string_value_matches(payload.get("source_procedure_id"), procedure_id)
        || string_value_matches(payload.get("procedure_id"), procedure_id)
        || payload
            .get("source_procedure")
            .is_some_and(|value| string_value_matches(value.get("id"), procedure_id))
}

fn string_value_matches(value: Option<&Value>, expected: &str) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| value.trim() == expected)
}

fn candidate_target_skill(candidate: &LearningCandidate) -> Option<String> {
    for payload in [
        candidate.proposed_change.get("workflow_template"),
        candidate.proposed_change.get("skill_update"),
        Some(&candidate.proposed_change),
    ]
    .into_iter()
    .flatten()
    {
        for key in ["target_skill", "skill_name", "skill"] {
            if let Some(skill) = payload
                .get(key)
                .and_then(Value::as_str)
                .and_then(skill_slug)
            {
                return Some(skill);
            }
        }
    }
    candidate
        .proposed_target
        .as_deref()
        .and_then(|target| skill_slug(target.strip_prefix("skill:").unwrap_or(target).trim()))
}

fn procedure_payload_records_promotion(
    procedure: &LearningProcedure,
    candidate_id: &str,
    requires_capability_backlog: bool,
    requires_eval_backlog: bool,
) -> bool {
    procedure
        .payload
        .get("phase_14_skill_promotions")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("promotion_candidate_id").and_then(Value::as_str) == Some(candidate_id)
                    && (!requires_capability_backlog
                        || item
                            .get("capability_backlog_path")
                            .and_then(Value::as_str)
                            .is_some_and(|value| !value.trim().is_empty()))
                    && (!requires_eval_backlog
                        || item
                            .get("eval_backlog_path")
                            .and_then(Value::as_str)
                            .is_some_and(|value| !value.trim().is_empty()))
                    && (!requires_eval_backlog
                        || item
                            .get("eval_backlog_candidate_id")
                            .and_then(Value::as_str)
                            == Some(candidate_id))
            })
        })
}

fn append_promotion_payload(
    procedure: &mut LearningProcedure,
    promotion_candidate: &LearningCandidate,
    eval_candidate_id: Option<&str>,
    eval_backlog_candidate_id: Option<&str>,
    target_skill: &str,
    capability_backlog_path: Option<&str>,
    eval_backlog_path: Option<&str>,
    force: bool,
) {
    if !procedure.payload.is_object() {
        let previous = std::mem::take(&mut procedure.payload);
        procedure.payload = json!({ "previous_payload": previous });
    }
    let Some(root) = procedure.payload.as_object_mut() else {
        return;
    };
    root.entry("phase_14_skill_promotions".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(items) = root
        .get_mut("phase_14_skill_promotions")
        .and_then(Value::as_array_mut)
    {
        let existing_index = items.iter().position(|item| {
            item.get("promotion_candidate_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == promotion_candidate.id)
        });
        let created_at = existing_index
            .and_then(|index| {
                items
                    .get(index)
                    .and_then(|item| item.get("created_at"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        let item = json!({
            "created_at": created_at,
            "updated_at": Utc::now().to_rfc3339(),
            "target_skill": target_skill,
            "promotion_candidate_id": promotion_candidate.id,
            "promotion_candidate_type": promotion_candidate.candidate_type.as_str(),
            "eval_candidate_id": eval_candidate_id,
            "eval_backlog_candidate_id": eval_backlog_candidate_id,
            "capability_backlog_path": capability_backlog_path,
            "eval_backlog_path": eval_backlog_path,
            "force": force,
        });
        if let Some(index) = existing_index {
            items[index] = item;
        } else {
            items.push(item);
        }
        if items.len() > MAX_PROMOTION_HISTORY_ITEMS {
            let excess = items.len().saturating_sub(MAX_PROMOTION_HISTORY_ITEMS);
            items.drain(0..excess);
        }
    }
}

fn skill_slug(value: &str) -> Option<String> {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.trim().chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch == '-' || ch == '_' || ch.is_whitespace() || ch == ':' || ch == '/' {
            Some('-')
        } else {
            None
        };
        let Some(ch) = normalized else {
            continue;
        };
        if ch == '-' {
            if last_dash || out.is_empty() {
                continue;
            }
            last_dash = true;
            out.push(ch);
        } else {
            last_dash = false;
            out.push(ch);
        }
    }
    let trimmed = out.trim_matches('-').chars().take(80).collect::<String>();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn default_skill_slug(procedure: &LearningProcedure) -> String {
    let base = procedure
        .owner_agent
        .as_deref()
        .map(|agent| format!("{agent}-{}", procedure.title))
        .unwrap_or_else(|| procedure.title.clone());
    skill_slug(&base).unwrap_or_else(|| format!("procedure-{}", procedure.id))
}

fn default_actor() -> String {
    PROCEDURE_SKILL_PROMOTION_SOURCE.to_string()
}

fn default_true() -> bool {
    true
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::learning::{
        CreateLearningProcedureRequest, LearningProcedureActivation,
    };

    #[test]
    fn ineligible_single_run_procedure_does_not_create_candidate() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, 1, 0);

        let outcome = LearningProcedureSkillPromotionBridge::new(workspace)
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest::default(),
            )
            .expect("promotion check");

        assert!(!outcome.eligible);
        assert!(outcome.reason.starts_with("success_count_below_threshold"));
        assert!(outcome.promotion_candidate_id.is_none());
    }

    #[test]
    fn eligible_procedure_routes_skill_and_eval_candidates() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, 3, 0);

        let outcome = LearningProcedureSkillPromotionBridge::new(workspace)
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest {
                    actor: "test".to_string(),
                    reason: Some("Repeated procedure is stable.".to_string()),
                    ..PromoteLearningProcedureToSkillRequest::default()
                },
            )
            .expect("promote procedure");

        assert!(outcome.eligible);
        let promotion_candidate_id = outcome
            .promotion_candidate_id
            .as_deref()
            .expect("promotion candidate");
        assert!(outcome.capability_backlog_path.is_some());
        assert!(outcome.eval_candidate_id.is_none());
        assert_eq!(
            outcome.eval_backlog_candidate_id.as_deref(),
            Some(promotion_candidate_id)
        );
        assert!(outcome.eval_backlog_path.is_some());

        let promotion_candidate = store
            .read_candidate(&scope, promotion_candidate_id)
            .expect("read promotion candidate");
        assert_eq!(
            promotion_candidate.candidate_type,
            LearningCandidateType::WorkflowTemplate
        );
        assert_eq!(promotion_candidate.state, LearningCandidateState::Triaged);
        assert_eq!(
            promotion_candidate
                .proposed_change
                .get("workflow_template")
                .and_then(|value| value.get("source_procedure_id"))
                .and_then(Value::as_str),
            Some(procedure.id.as_str())
        );
        let eval_backlog = store
            .read_evaluation_backlog_item(&scope, promotion_candidate_id)
            .expect("read eval backlog");
        assert_eq!(eval_backlog.candidate_id, promotion_candidate_id);
        assert_eq!(eval_backlog.status, LearningEvaluationBacklogStatus::Queued);
        assert_eq!(
            eval_backlog.case_kind,
            "procedure_skill_promotion_regression"
        );

        let backlog = store
            .read_capability_evolution_backlog_item(&scope, promotion_candidate_id)
            .expect("read capability backlog");
        assert_eq!(backlog.candidate_id, promotion_candidate_id);
        assert_eq!(
            backlog
                .fix_spec
                .get("source_procedure_id")
                .and_then(Value::as_str),
            Some(procedure.id.as_str())
        );

        let updated = store
            .read_procedure(&scope, &procedure.id)
            .expect("read updated procedure");
        assert_eq!(updated.status, LearningProcedureStatus::Active);
        assert!(updated
            .payload
            .get("phase_14_skill_promotions")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()));
    }

    #[test]
    fn existing_open_promotion_candidate_is_reused() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, 5, 0);
        let bridge = LearningProcedureSkillPromotionBridge::new(workspace);

        let first = bridge
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest::default(),
            )
            .expect("first promotion");
        let after_first = store
            .read_procedure(&scope, &procedure.id)
            .expect("read procedure after first promotion");
        let second = bridge
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest::default(),
            )
            .expect("second promotion");

        assert!(second.reused_existing_candidate);
        assert_eq!(first.promotion_candidate_id, second.promotion_candidate_id);
        assert!(second.event_ids.is_empty());
        let candidate_id = second
            .promotion_candidate_id
            .as_deref()
            .expect("promotion candidate");
        assert_eq!(
            second.eval_backlog_candidate_id.as_deref(),
            Some(candidate_id)
        );
        store
            .read_evaluation_backlog_item(&scope, candidate_id)
            .expect("eval backlog repaired on reuse");
        let updated = store
            .read_procedure(&scope, &procedure.id)
            .expect("read procedure");
        assert_eq!(updated.version, after_first.version);
        let promotion_items = updated
            .payload
            .get("phase_14_skill_promotions")
            .and_then(Value::as_array)
            .expect("promotion payload");
        assert_eq!(promotion_items.len(), 1);
    }

    #[test]
    fn progressed_existing_candidate_is_not_repaired_by_automatic_reuse() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, 4, 0);
        let procedure_path = LearningProcedureSkillPromotionBridge::new(workspace.clone())
            .procedure_path(&procedure);
        let existing = store
            .create_candidate(
                scope.clone(),
                CreateLearningCandidateRequest {
                    principal: None,
                    workspace: None,
                    candidate_type: LearningCandidateType::WorkflowTemplate,
                    state: LearningCandidateState::Proposed,
                    title: "Progressed procedure promotion".to_string(),
                    summary: "Candidate moved beyond automatic repair state.".to_string(),
                    rationale: "simulate reviewer progress before backlog repair".to_string(),
                    proposed_change: json!({
                        "workflow_template": {
                            "source": "procedure_to_skill_promotion",
                            "source_procedure_id": procedure.id,
                            "target_skill": "simple-data-analyst-dashboard-triage"
                        }
                    }),
                    proposed_target: Some("skill:simple-data-analyst-dashboard-triage".to_string()),
                    confidence: Some(0.8),
                    source_agent_id: procedure.owner_agent.clone(),
                    source_task_id: procedure.source_task_ids.last().cloned(),
                    source_execution_id: None,
                    source_chat_session_id: None,
                    event_refs: Vec::new(),
                    evidence_refs: promotion_evidence_refs(&procedure, &procedure_path),
                    risk_level: LearningRiskLevel::Medium,
                    review_required: true,
                    review_reason: Some("test".to_string()),
                    review_policy: json!({}),
                    promotion_target: Some("skill_evolution".to_string()),
                    promotion_policy: json!({}),
                },
            )
            .expect("create progressed candidate");
        store
            .transition_candidate(
                &scope,
                &existing.id,
                LearningCandidateState::Approved,
                "test",
                "approved_for_test",
                "simulate reviewer progress",
                existing.evidence_refs.clone(),
            )
            .expect("approve candidate");
        let before = store
            .read_procedure(&scope, &procedure.id)
            .expect("read procedure before reuse");

        let outcome = LearningProcedureSkillPromotionBridge::new(workspace)
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest::default(),
            )
            .expect("reuse progressed candidate");

        assert!(outcome.reused_existing_candidate);
        assert_eq!(
            outcome.promotion_candidate_id.as_deref(),
            Some(existing.id.as_str())
        );
        assert_eq!(
            outcome.reason,
            "existing_progressed_procedure_skill_promotion_candidate_reused_read_only"
        );
        assert!(outcome.event_ids.is_empty());
        assert!(outcome.capability_backlog_path.is_none());
        assert!(outcome.eval_backlog_path.is_none());
        let after = store
            .read_procedure(&scope, &procedure.id)
            .expect("read procedure after reuse");
        assert_eq!(after.version, before.version);
        assert!(after
            .payload
            .get("phase_14_skill_promotions")
            .and_then(Value::as_array)
            .map(|items| items.is_empty())
            .unwrap_or(true));
        assert!(store
            .read_capability_evolution_backlog_item(&scope, &existing.id)
            .is_err_and(|error| store.error_is_not_found(&error)));
        assert!(store
            .read_evaluation_backlog_item(&scope, &existing.id)
            .is_err_and(|error| store.error_is_not_found(&error)));
    }

    #[test]
    fn promoted_existing_candidate_blocks_duplicate_auto_promotion() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, 6, 0);
        let bridge = LearningProcedureSkillPromotionBridge::new(workspace);

        let first = bridge
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest::default(),
            )
            .expect("first promotion");
        let candidate_id = first
            .promotion_candidate_id
            .as_deref()
            .expect("promotion candidate")
            .to_string();
        let candidate = store
            .read_candidate(&scope, &candidate_id)
            .expect("read candidate");
        store
            .transition_candidate(
                &scope,
                &candidate_id,
                LearningCandidateState::Promoted,
                "test",
                "promoted_for_test",
                "simulate completed skill promotion",
                candidate.evidence_refs.clone(),
            )
            .expect("promote candidate");

        let second = bridge
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest::default(),
            )
            .expect("second promotion");

        assert!(second.reused_existing_candidate);
        assert_eq!(
            second.promotion_candidate_id.as_deref(),
            Some(candidate_id.as_str())
        );
        assert_eq!(
            second.reason,
            "existing_promoted_procedure_skill_promotion_candidate_reused_read_only"
        );
        assert!(second.event_ids.is_empty());
        let candidates = store
            .list_candidates(
                &scope,
                LearningCandidateFilters {
                    candidate_type: None,
                    state: None,
                    source_agent_id: procedure.owner_agent.clone(),
                    limit: Some(20),
                },
            )
            .expect("list candidates");
        let matching_count = candidates
            .iter()
            .filter(|candidate| {
                candidate.candidate_type.is_skill_or_workflow_candidate()
                    && candidate_references_procedure(candidate, &procedure.id)
            })
            .count();
        assert_eq!(matching_count, 1);
    }

    #[test]
    fn existing_open_candidate_does_not_bypass_current_procedure_eligibility() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, 5, 0);
        let bridge = LearningProcedureSkillPromotionBridge::new(workspace);

        let first = bridge
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest::default(),
            )
            .expect("first promotion");
        assert!(first.promotion_candidate_id.is_some());

        store
            .transition_procedure_status(
                &scope,
                &procedure.id,
                LearningProcedureStatus::Deprecated,
                "test",
                "deprecate",
                "simulate procedure becoming unsafe after promotion candidate creation",
                procedure.evidence_refs.clone(),
            )
            .expect("deprecate procedure");

        let second = bridge
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest::default(),
            )
            .expect("second promotion");

        assert!(!second.eligible);
        assert!(second
            .reason
            .starts_with("procedure_status_is_not_active:deprecated"));
        assert!(second.promotion_candidate_id.is_none());
        assert!(!second.reused_existing_candidate);
    }

    #[test]
    fn existing_partial_promotion_candidate_repairs_missing_backlogs_and_payload() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test", "workspace");
        let procedure = active_procedure(&store, &scope, 4, 0);
        let existing = store
            .create_candidate(
                scope.clone(),
                CreateLearningCandidateRequest {
                    principal: None,
                    workspace: None,
                    candidate_type: LearningCandidateType::WorkflowTemplate,
                    state: LearningCandidateState::Proposed,
                    title: "Existing partial procedure promotion".to_string(),
                    summary: "Candidate existed before backlog routing completed.".to_string(),
                    rationale: "simulate interrupted phase 14 run".to_string(),
                    proposed_change: json!({
                        "workflow_template": {
                            "source": "procedure_to_skill_promotion",
                            "source_procedure_id": procedure.id,
                            "target_skill": "simple-data-analyst-dashboard-triage"
                        }
                    }),
                    proposed_target: Some("skill:simple-data-analyst-dashboard-triage".to_string()),
                    confidence: Some(0.8),
                    source_agent_id: procedure.owner_agent.clone(),
                    source_task_id: procedure.source_task_ids.last().cloned(),
                    source_execution_id: None,
                    source_chat_session_id: None,
                    event_refs: Vec::new(),
                    evidence_refs: promotion_evidence_refs(
                        &procedure,
                        &LearningProcedureSkillPromotionBridge::new(workspace.clone())
                            .procedure_path(&procedure),
                    ),
                    risk_level: LearningRiskLevel::Medium,
                    review_required: true,
                    review_reason: Some("test".to_string()),
                    review_policy: json!({}),
                    promotion_target: Some("skill_evolution".to_string()),
                    promotion_policy: json!({}),
                },
            )
            .expect("create partial candidate");

        let outcome = LearningProcedureSkillPromotionBridge::new(workspace)
            .promote_procedure(
                &store,
                &scope,
                &procedure.id,
                PromoteLearningProcedureToSkillRequest::default(),
            )
            .expect("reconcile partial candidate");

        assert!(outcome.reused_existing_candidate);
        assert_eq!(
            outcome.promotion_candidate_id.as_deref(),
            Some(existing.id.as_str())
        );
        assert_eq!(
            outcome.eval_backlog_candidate_id.as_deref(),
            Some(existing.id.as_str())
        );
        store
            .read_capability_evolution_backlog_item(&scope, &existing.id)
            .expect("capability backlog repaired");
        store
            .read_evaluation_backlog_item(&scope, &existing.id)
            .expect("eval backlog repaired");
        let updated = store
            .read_procedure(&scope, &procedure.id)
            .expect("read updated procedure");
        assert!(updated
            .payload
            .get("phase_14_skill_promotions")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("promotion_candidate_id").and_then(Value::as_str)
                        == Some(existing.id.as_str())
                        && item
                            .get("eval_backlog_candidate_id")
                            .and_then(Value::as_str)
                            == Some(existing.id.as_str())
                })
            }));
    }

    fn active_procedure(
        store: &LearningStore,
        scope: &LearningScope,
        success_count: u64,
        failure_count: u64,
    ) -> LearningProcedure {
        let procedure = store
            .create_procedure(
                scope.clone(),
                CreateLearningProcedureRequest {
                    principal: None,
                    workspace: None,
                    id: Some(format!("proc_phase14_{success_count}_{failure_count}")),
                    actor: "test".to_string(),
                    reason: Some("seed".to_string()),
                    status: LearningProcedureStatus::Draft,
                    title: "Analyze recurring dashboard issue".to_string(),
                    summary: "Repeated workflow for diagnosing a dashboard discrepancy."
                        .to_string(),
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
                    decision_points: vec!["Use Metabase first when dashboard ids are known.".to_string()],
                    verification: vec!["The final totals reconcile or the discrepancy is explained.".to_string()],
                    failure_modes: vec!["Do not save a card before the query is validated.".to_string()],
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
                    source_candidate_id: Some("lc_proc_source".to_string()),
                    source_task_ids: vec!["task_one".to_string(), "task_two".to_string()],
                    source_chat_session_ids: Vec::new(),
                    success_count,
                    failure_count,
                    payload: json!({"workflow_signature": "agent:simple-data-analyst|tools:metabase>duckdb"}),
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
                "activate for test",
                procedure.evidence_refs.clone(),
            )
            .expect("activate")
    }
}
