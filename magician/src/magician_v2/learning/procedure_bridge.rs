use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::{
    CreateLearningEventRequest, CreateLearningProcedureRequest, LearningCandidate,
    LearningCandidateState, LearningCandidateType, LearningEvidenceRef, LearningProcedure,
    LearningProcedureActivation, LearningProcedureFilters, LearningProcedureStatus, LearningScope,
    LearningStore,
};

const LEARNING_PROCEDURE_BRIDGE_SOURCE: &str = "learning_procedure_bridge";

#[derive(Debug, Clone, Serialize)]
pub struct LearningProcedureRouteOutcome {
    pub candidate_id: String,
    pub routed: bool,
    pub promoted: bool,
    pub procedure_id: Option<String>,
    pub procedure_path: Option<String>,
    pub existing_procedure_reused: bool,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct LearningProcedureBridge {
    workspace_layout: ArtifactV2Workspace,
}

#[derive(Debug, Clone)]
struct ProcedurePromotionSpec {
    id: String,
    existing_procedure_id: Option<String>,
    title: String,
    summary: String,
    owner_agent: Option<String>,
    activation: LearningProcedureActivation,
    workflow: Vec<String>,
    decision_points: Vec<String>,
    verification: Vec<String>,
    failure_modes: Vec<String>,
    source_task_ids: Vec<String>,
    source_chat_session_ids: Vec<String>,
    success_count: u64,
    failure_count: u64,
    workflow_signature: Option<String>,
    deprecation_recommended: bool,
    deprecation_reason: Option<String>,
    payload: Value,
}

impl LearningProcedureBridge {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub fn route_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
    ) -> Result<LearningProcedureRouteOutcome> {
        if candidate.candidate_type != LearningCandidateType::MemoryProcedure {
            return Ok(LearningProcedureRouteOutcome {
                candidate_id: candidate.id.clone(),
                routed: false,
                promoted: false,
                procedure_id: None,
                procedure_path: None,
                existing_procedure_reused: false,
                reason: "not_a_procedure_candidate".to_string(),
            });
        }
        if candidate.state.is_terminal() {
            return Ok(LearningProcedureRouteOutcome {
                candidate_id: candidate.id.clone(),
                routed: false,
                promoted: false,
                procedure_id: None,
                procedure_path: None,
                existing_procedure_reused: false,
                reason: format!("candidate_already_terminal:{}", candidate.state.as_str()),
            });
        }

        let spec = ProcedurePromotionSpec::from_candidate(candidate)?;
        let (procedure, existing_procedure_reused, reason) =
            self.create_or_reuse_draft_procedure(store, scope, candidate, &spec, false, None)?;
        let procedure_path = self.procedure_path(&procedure);
        let event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_procedure_candidate_routed".to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: if spec.deprecation_recommended {
                    format!(
                        "Learning procedure deprecation candidate `{}` routed to existing procedure `{}` for review.",
                        candidate.id, procedure.id
                    )
                } else {
                    format!(
                        "Learning procedure candidate `{}` routed to draft procedure `{}`.",
                        candidate.id, procedure.id
                    )
                },
                evidence_refs: candidate.evidence_refs.clone(),
                payload: json!({
                    "candidate_id": candidate.id,
                    "candidate_type": candidate.candidate_type.as_str(),
                    "procedure_id": procedure.id,
                    "procedure_path": procedure_path,
                    "existing_procedure_reused": existing_procedure_reused,
                    "workflow_signature": spec.workflow_signature,
                    "deprecation_recommended": spec.deprecation_recommended,
                    "deprecation_reason": spec.deprecation_reason,
                    "reason": reason,
                }),
            },
        )?;

        if matches!(
            candidate.state,
            LearningCandidateState::Observed | LearningCandidateState::Proposed
        ) {
            let decision = if existing_procedure_reused {
                "matched_existing_procedure"
            } else {
                "routed_to_draft_procedure"
            };
            let _ = store.transition_candidate(
                scope,
                &candidate.id,
                LearningCandidateState::Triaged,
                LEARNING_PROCEDURE_BRIDGE_SOURCE,
                decision,
                format!(
                    "Procedure candidate was routed for review through procedure `{}`. Route event: {}.",
                    procedure.id, event.id
                ),
                candidate.evidence_refs.clone(),
            )?;
        }

        Ok(LearningProcedureRouteOutcome {
            candidate_id: candidate.id.clone(),
            routed: true,
            promoted: false,
            procedure_id: Some(procedure.id),
            procedure_path: Some(procedure_path),
            existing_procedure_reused,
            reason,
        })
    }

    pub fn promote_reviewed_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        actor: &str,
        reason: &str,
    ) -> Result<LearningCandidate> {
        if candidate.candidate_type != LearningCandidateType::MemoryProcedure {
            return Err(anyhow!(
                "candidate `{}` is `{}`; only memory_procedure candidates can be promoted through the procedure bridge",
                candidate.id,
                candidate.candidate_type.as_str()
            ));
        }
        if candidate.state.is_terminal() {
            return Err(anyhow!(
                "candidate `{}` is terminal in state `{}` and cannot be promoted through the procedure bridge",
                candidate.id,
                candidate.state.as_str()
            ));
        }

        let spec = ProcedurePromotionSpec::from_candidate(candidate)?;
        if spec.deprecation_recommended {
            return self.promote_reviewed_deprecation_candidate(
                store, scope, candidate, &spec, actor, reason,
            );
        }
        let (procedure, existing_procedure_reused, route_reason) = self
            .create_or_reuse_draft_procedure(
                store,
                scope,
                candidate,
                &spec,
                true,
                Some((actor, reason)),
            )?;
        let (procedure, activation_reason) =
            self.activate_reviewed_procedure_if_needed(store, scope, procedure, actor, reason)?;
        let procedure_path = self.procedure_path(&procedure);
        let evidence = LearningEvidenceRef {
            kind: "learning_procedure".to_string(),
            id: Some(procedure.id.clone()),
            path: Some(procedure_path.clone()),
            uri: None,
            summary: Some(procedure.summary.clone()),
        };
        let event_id = match store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_procedure_candidate_review_promoted".to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Reviewed learning procedure candidate `{}` promoted to {} procedure `{}` by {}.",
                    candidate.id,
                    procedure.status.as_str(),
                    procedure.id,
                    actor
                ),
                evidence_refs: {
                    let mut refs = candidate.evidence_refs.clone();
                    refs.push(evidence.clone());
                    refs
                },
                payload: json!({
                    "candidate_id": candidate.id,
                    "candidate_type": candidate.candidate_type.as_str(),
                    "procedure_id": procedure.id,
                    "procedure_path": procedure_path,
                    "existing_procedure_reused": existing_procedure_reused,
                    "procedure_status": procedure.status.as_str(),
                    "actor": actor,
                    "reason": reason,
                    "route_reason": route_reason,
                    "activation_reason": activation_reason,
                }),
            },
        ) {
            Ok(event) => Some(event.id),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    candidate_id = %candidate.id,
                    procedure_id = %procedure.id,
                    procedure_status = procedure.status.as_str(),
                    "failed to append learning procedure promotion event after procedure was updated"
                );
                None
            },
        };

        store.transition_candidate(
            scope,
            &candidate.id,
            LearningCandidateState::Promoted,
            actor,
            "reviewed_promoted_to_procedure",
            promotion_decision_reason(reason, event_id.as_deref(), &procedure),
            vec![evidence],
        )
    }

    fn promote_reviewed_deprecation_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        spec: &ProcedurePromotionSpec,
        actor: &str,
        reason: &str,
    ) -> Result<LearningCandidate> {
        let Some(existing) =
            find_existing_procedure_for_deprecation(store, scope, candidate, spec)?
        else {
            return Err(anyhow!(
                "procedure deprecation candidate `{}` must reference an existing procedure",
                candidate.id
            ));
        };
        let deprecation_reason = spec
            .deprecation_reason
            .clone()
            .unwrap_or_else(|| "Reviewed procedure candidate recommended deprecation.".to_string());
        let deprecated = if matches!(
            existing.status,
            LearningProcedureStatus::Deprecated | LearningProcedureStatus::Archived
        ) {
            existing
        } else {
            store.transition_procedure_status(
                scope,
                &existing.id,
                LearningProcedureStatus::Deprecated,
                actor,
                "reviewed_procedure_candidate_deprecated",
                format!(
                    "{} Candidate `{}` recommended deprecating this reusable procedure. {}",
                    reason, candidate.id, deprecation_reason
                ),
                candidate.evidence_refs.clone(),
            )?
        };
        let procedure_path = self.procedure_path(&deprecated);
        let evidence = LearningEvidenceRef {
            kind: "learning_procedure".to_string(),
            id: Some(deprecated.id.clone()),
            path: Some(procedure_path.clone()),
            uri: None,
            summary: Some(deprecated.summary.clone()),
        };
        let event_id = match store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_procedure_deprecated".to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Reviewed learning procedure candidate `{}` deprecated procedure `{}` by {}.",
                    candidate.id, deprecated.id, actor
                ),
                evidence_refs: {
                    let mut refs = candidate.evidence_refs.clone();
                    refs.push(evidence.clone());
                    refs
                },
                payload: json!({
                    "candidate_id": &candidate.id,
                    "candidate_type": candidate.candidate_type.as_str(),
                    "procedure_id": &deprecated.id,
                    "procedure_path": &procedure_path,
                    "procedure_status": deprecated.status.as_str(),
                    "actor": actor,
                    "reason": reason,
                    "deprecation_reason": deprecation_reason,
                    "deprecation_recommended": true,
                }),
            },
        ) {
            Ok(event) => Some(event.id),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    candidate_id = %candidate.id,
                    procedure_id = %deprecated.id,
                    "failed to append learning procedure deprecation event after procedure was deprecated"
                );
                None
            },
        };

        store.transition_candidate(
            scope,
            &candidate.id,
            LearningCandidateState::Promoted,
            actor,
            "reviewed_deprecated_procedure",
            match event_id {
                Some(event_id) => format!(
                    "{} Learning event {} records the reviewed procedure deprecation.",
                    reason, event_id
                ),
                None => format!(
                    "{} Procedure `{}` was deprecated, but the deprecation event append failed; use the procedure decision log for durable audit.",
                    reason, deprecated.id
                ),
            },
            vec![evidence],
        )
    }

    fn create_or_reuse_draft_procedure(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        spec: &ProcedurePromotionSpec,
        reviewed: bool,
        reviewed_context: Option<(&str, &str)>,
    ) -> Result<(LearningProcedure, bool, String)> {
        if spec.deprecation_recommended {
            let Some(existing) =
                find_existing_procedure_for_deprecation(store, scope, candidate, spec)?
            else {
                return Err(anyhow!(
                    "procedure deprecation candidate `{}` must reference an existing procedure",
                    candidate.id
                ));
            };
            return Ok((
                existing,
                true,
                "deprecation_candidate_requires_review_or_is_already_retired".to_string(),
            ));
        }
        if let Some(existing) = find_existing_procedure(store, scope, candidate, spec)? {
            let same_source_candidate =
                existing.source_candidate_id.as_deref() == Some(candidate.id.as_str());
            if reviewed && !same_source_candidate {
                let actor = reviewed_context
                    .map(|(actor, _)| actor.to_string())
                    .unwrap_or_else(|| LEARNING_PROCEDURE_BRIDGE_SOURCE.to_string());
                let reason = reviewed_context
                    .map(|(_, reason)| reason.to_string())
                    .unwrap_or_else(|| {
                        "Reviewed reusable procedure candidate updated an existing procedure."
                            .to_string()
                    });
                let updated = store.update_procedure(
                    scope,
                    &existing.id,
                    actor,
                    "reviewed_updated_existing_procedure",
                    format!(
                        "{} Candidate `{}` supplied reusable procedure improvements.",
                        reason, candidate.id
                    ),
                    candidate.evidence_refs.clone(),
                    |procedure| merge_procedure_from_spec(procedure, candidate, spec),
                )?;
                return Ok((updated, true, "updated_existing_procedure".to_string()));
            }
            let reason = format!("reused_existing_procedure:{}", existing.status.as_str());
            return Ok((existing, true, reason));
        }

        let procedure_id = available_new_procedure_id(store, scope, &spec.id, candidate)?;
        let actor = reviewed_context
            .map(|(actor, _)| actor.to_string())
            .unwrap_or_else(|| LEARNING_PROCEDURE_BRIDGE_SOURCE.to_string());
        let create_reason = reviewed_context
            .map(|(_, reason)| reason.to_string())
            .unwrap_or_else(|| {
                if reviewed {
                    "Reviewed reusable procedure candidate was converted to a draft procedure."
                        .to_string()
                } else {
                    "Reusable procedure candidate was extracted from teaching/reflection and recorded as a draft procedure."
                        .to_string()
                }
            });
        let procedure = store.create_procedure(
            scope.clone(),
            CreateLearningProcedureRequest {
                principal: None,
                workspace: None,
                id: Some(procedure_id),
                actor,
                reason: Some(create_reason),
                status: LearningProcedureStatus::Draft,
                title: spec.title.clone(),
                summary: spec.summary.clone(),
                owner_agent: spec.owner_agent.clone(),
                activation: spec.activation.clone(),
                workflow: spec.workflow.clone(),
                decision_points: spec.decision_points.clone(),
                verification: spec.verification.clone(),
                failure_modes: spec.failure_modes.clone(),
                evidence_refs: candidate.evidence_refs.clone(),
                source_candidate_id: Some(candidate.id.clone()),
                source_task_ids: spec.source_task_ids.clone(),
                source_chat_session_ids: spec.source_chat_session_ids.clone(),
                success_count: spec.success_count,
                failure_count: spec.failure_count,
                payload: spec.payload.clone(),
            },
        )?;
        Ok((procedure, false, "created_draft_procedure".to_string()))
    }

    fn activate_reviewed_procedure_if_needed(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        procedure: LearningProcedure,
        actor: &str,
        reason: &str,
    ) -> Result<(LearningProcedure, String)> {
        if procedure.status == LearningProcedureStatus::Active {
            return Ok((procedure, "already_active".to_string()));
        }
        if procedure.status != LearningProcedureStatus::Draft {
            return Ok((procedure, "not_activated_non_draft_procedure".to_string()));
        }
        let activated = store.transition_procedure_status(
            scope,
            &procedure.id,
            LearningProcedureStatus::Active,
            actor,
            "reviewed_procedure_candidate_activated",
            format!(
                "{} Reviewed learning candidate approval activated this procedure for retrieval.",
                reason
            ),
            procedure.evidence_refs.clone(),
        )?;
        Ok((activated, "activated_draft_procedure".to_string()))
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

impl ProcedurePromotionSpec {
    fn from_candidate(candidate: &LearningCandidate) -> Result<Self> {
        let payload = procedure_payload(candidate);
        let title = read_string_any(&payload, &["title", "name", "procedure_title"])
            .unwrap_or_else(|| candidate.title.clone());
        let title = truncate_text(title.trim(), 220);
        if title.is_empty() {
            return Err(anyhow!("procedure candidate is missing a title"));
        }
        let summary = read_string_any(&payload, &["summary", "description", "pattern"])
            .unwrap_or_else(|| candidate.summary.clone());
        let summary = truncate_text(summary.trim(), 2_000);
        let owner_agent = read_string_any(
            &payload,
            &[
                "owner_agent",
                "agent_id",
                "target_agent_id",
                "source_agent_id",
            ],
        )
        .or_else(|| candidate.source_agent_id.clone())
        .map(|agent| agent.trim().to_string())
        .filter(|agent| !agent.is_empty());
        let workflow_signature = read_string_any(
            &payload,
            &["workflow_signature", "signature", "workflow_id"],
        )
        .map(|signature| normalize_signature(&signature))
        .filter(|signature| !signature.is_empty());
        let activation = LearningProcedureActivation {
            use_when: read_string_list_nested(
                &payload,
                &[
                    "activation.use_when",
                    "use_when",
                    "trigger_conditions",
                    "when_to_use",
                ],
            )
            .or_else(|| {
                let fallback = one_line(&summary);
                (!fallback.is_empty()).then(|| vec![fallback])
            })
            .unwrap_or_default(),
            avoid_when: read_string_list_nested(
                &payload,
                &[
                    "activation.avoid_when",
                    "avoid_when",
                    "when_not_to_use",
                    "do_not_use_when",
                ],
            )
            .unwrap_or_default(),
            example_goals: read_string_list_nested(
                &payload,
                &["activation.example_goals", "example_goals", "examples"],
            )
            .unwrap_or_default(),
        };
        let workflow = read_string_list_nested(
            &payload,
            &[
                "workflow",
                "procedure_steps",
                "steps",
                "procedure.steps",
                "workflow.steps",
            ],
        )
        .or_else(|| {
            let fallback = one_line(&summary);
            (!fallback.is_empty()).then(|| vec![fallback])
        })
        .unwrap_or_default();
        let decision_points = read_string_list_nested(
            &payload,
            &[
                "decision_points",
                "branching_rules",
                "tool_choice_guidance",
                "decisions",
            ],
        )
        .unwrap_or_default();
        let verification = read_string_list_nested(
            &payload,
            &["verification", "verification_steps", "success_criteria"],
        )
        .unwrap_or_default();
        let failure_modes = read_string_list_nested(
            &payload,
            &[
                "failure_modes",
                "common_failure_modes",
                "when_not_to_use",
                "risks",
            ],
        )
        .unwrap_or_default();
        let source_task_ids = candidate
            .source_task_id
            .clone()
            .map_or_else(Vec::new, |task_id| vec![task_id]);
        let source_chat_session_ids = candidate
            .source_chat_session_id
            .clone()
            .map_or_else(Vec::new, |chat_session_id| vec![chat_session_id]);
        let success_count = read_u64_any(&payload, &["success_count", "successful_count"])
            .unwrap_or_else(|| default_success_count(candidate));
        let failure_count = read_u64_any(&payload, &["failure_count", "failed_count"])
            .unwrap_or_else(|| default_failure_count(candidate));
        let existing_procedure_id = read_string_any(&payload, &["existing_procedure_id"])
            .map(|id| sanitize_procedure_id(&id))
            .filter(|id| !id.is_empty());
        let deprecation_reason = read_string_any(
            &payload,
            &[
                "deprecation_reason",
                "deprecated_reason",
                "retirement_reason",
                "superseded_reason",
            ],
        );
        let deprecation_recommended = read_bool_any(
            &payload,
            &[
                "deprecate",
                "deprecated",
                "deprecation_recommended",
                "should_deprecate",
                "retire",
            ],
        )
        .unwrap_or(false)
            || deprecation_reason.is_some()
            || read_string_any(&payload, &["status", "action", "recommendation"])
                .is_some_and(|value| deprecation_action_string(&value));
        let id = read_string_any(&payload, &["procedure_id", "id"])
            .map(|id| sanitize_procedure_id(&id))
            .filter(|id| !id.is_empty())
            .or_else(|| existing_procedure_id.clone())
            .unwrap_or_else(|| {
                deterministic_procedure_id(
                    owner_agent.as_deref(),
                    workflow_signature.as_deref(),
                    &title,
                    candidate,
                )
            });

        let mut normalized_payload = match payload {
            Value::Object(map) => Value::Object(map),
            other => json!({ "source_payload": other }),
        };
        if let Value::Object(map) = &mut normalized_payload {
            if let Some(signature) = workflow_signature.as_ref() {
                map.insert(
                    "workflow_signature".to_string(),
                    Value::String(signature.clone()),
                );
            }
            map.insert(
                "source_candidate_id".to_string(),
                Value::String(candidate.id.clone()),
            );
            map.insert(
                "candidate_type".to_string(),
                Value::String(candidate.candidate_type.as_str().to_string()),
            );
        }

        Ok(Self {
            id,
            existing_procedure_id,
            title,
            summary,
            owner_agent,
            activation,
            workflow,
            decision_points,
            verification,
            failure_modes,
            source_task_ids,
            source_chat_session_ids,
            success_count,
            failure_count,
            workflow_signature,
            deprecation_recommended,
            deprecation_reason,
            payload: normalized_payload,
        })
    }
}

fn find_existing_procedure(
    store: &LearningStore,
    scope: &LearningScope,
    candidate: &LearningCandidate,
    spec: &ProcedurePromotionSpec,
) -> Result<Option<LearningProcedure>> {
    if let Some(existing_id) = spec.existing_procedure_id.as_deref() {
        match store.read_procedure(scope, existing_id) {
            Ok(procedure) if procedure_is_reusable_target(&procedure) => {
                return Ok(Some(procedure));
            },
            Ok(_) => return Ok(None),
            Err(error) if store.error_is_not_found(&error) => {},
            Err(error) => return Err(error),
        }
    }
    let procedures = store.list_procedures(
        scope,
        LearningProcedureFilters {
            status: None,
            owner_agent: None,
            limit: None,
        },
    )?;
    for procedure in procedures {
        if !procedure_is_reusable_target(&procedure) {
            continue;
        }
        if procedure.source_candidate_id.as_deref() == Some(candidate.id.as_str()) {
            return Ok(Some(procedure));
        }
        if procedure.id == spec.id {
            return Ok(Some(procedure));
        }
        if let Some(signature) = spec.workflow_signature.as_deref() {
            if procedure_workflow_signature(&procedure).as_deref() == Some(signature) {
                return Ok(Some(procedure));
            }
        }
    }
    Ok(None)
}

fn find_existing_procedure_for_deprecation(
    store: &LearningStore,
    scope: &LearningScope,
    candidate: &LearningCandidate,
    spec: &ProcedurePromotionSpec,
) -> Result<Option<LearningProcedure>> {
    if let Some(existing_id) = spec.existing_procedure_id.as_deref() {
        match store.read_procedure(scope, existing_id) {
            Ok(procedure)
                if matches!(
                    procedure.status,
                    LearningProcedureStatus::Draft
                        | LearningProcedureStatus::Active
                        | LearningProcedureStatus::Deprecated
                        | LearningProcedureStatus::Archived
                ) =>
            {
                return Ok(Some(procedure));
            },
            Ok(_) => return Ok(None),
            Err(error) if store.error_is_not_found(&error) => {},
            Err(error) => return Err(error),
        }
    }
    find_existing_procedure(store, scope, candidate, spec)
}

fn promotion_decision_reason(
    reason: &str,
    event_id: Option<&str>,
    procedure: &LearningProcedure,
) -> String {
    match event_id {
        Some(event_id) => format!(
            "{} Learning event {} records the reviewed procedure routing.",
            reason, event_id
        ),
        None => format!(
            "{} Procedure `{}` was updated to `{}`, but the promotion event append failed; use the procedure decision log for durable audit.",
            reason,
            procedure.id,
            procedure.status.as_str()
        ),
    }
}

fn procedure_is_reusable_target(procedure: &LearningProcedure) -> bool {
    matches!(
        procedure.status,
        LearningProcedureStatus::Draft | LearningProcedureStatus::Active
    )
}

fn available_new_procedure_id(
    store: &LearningStore,
    scope: &LearningScope,
    requested_id: &str,
    candidate: &LearningCandidate,
) -> Result<String> {
    match store.read_procedure(scope, requested_id) {
        Ok(procedure) if !procedure_is_reusable_target(&procedure) => {
            let suffix = candidate
                .id
                .trim_start_matches("lc_")
                .chars()
                .take(10)
                .collect::<String>();
            let base_len = requested_id.len().min(140);
            let base = requested_id.chars().take(base_len).collect::<String>();
            Ok(format!("{base}-new-{suffix}"))
        },
        Ok(_) => Ok(requested_id.to_string()),
        Err(error) if store.error_is_not_found(&error) => Ok(requested_id.to_string()),
        Err(error) => Err(error),
    }
}

fn merge_procedure_from_spec(
    procedure: &mut LearningProcedure,
    candidate: &LearningCandidate,
    spec: &ProcedurePromotionSpec,
) {
    if procedure.summary.trim().is_empty() && !spec.summary.trim().is_empty() {
        procedure.summary = spec.summary.clone();
    }
    if procedure.owner_agent.is_none() {
        procedure.owner_agent = spec.owner_agent.clone();
    }
    append_unique_strings(
        &mut procedure.activation.use_when,
        &spec.activation.use_when,
    );
    append_unique_strings(
        &mut procedure.activation.avoid_when,
        &spec.activation.avoid_when,
    );
    append_unique_strings(
        &mut procedure.activation.example_goals,
        &spec.activation.example_goals,
    );
    append_unique_strings(&mut procedure.workflow, &spec.workflow);
    append_unique_strings(&mut procedure.decision_points, &spec.decision_points);
    append_unique_strings(&mut procedure.verification, &spec.verification);
    append_unique_strings(&mut procedure.failure_modes, &spec.failure_modes);
    append_unique_evidence_refs(&mut procedure.evidence_refs, &candidate.evidence_refs);
    append_unique_strings(&mut procedure.source_task_ids, &spec.source_task_ids);
    append_unique_strings(
        &mut procedure.source_chat_session_ids,
        &spec.source_chat_session_ids,
    );
    if procedure.source_candidate_id.is_none() {
        procedure.source_candidate_id = Some(candidate.id.clone());
    }
    procedure.success_count = procedure.success_count.saturating_add(spec.success_count);
    procedure.failure_count = procedure.failure_count.saturating_add(spec.failure_count);
    merge_procedure_payload(procedure, candidate, spec);
}

fn merge_procedure_payload(
    procedure: &mut LearningProcedure,
    candidate: &LearningCandidate,
    spec: &ProcedurePromotionSpec,
) {
    if !procedure.payload.is_object() {
        let previous = std::mem::take(&mut procedure.payload);
        procedure.payload = json!({ "previous_payload": previous });
    }
    let Some(root) = procedure.payload.as_object_mut() else {
        return;
    };
    if let Some(signature) = spec.workflow_signature.as_ref() {
        root.entry("workflow_signature".to_string())
            .or_insert_with(|| Value::String(signature.clone()));
    }
    append_json_string(root, "source_candidate_ids", &candidate.id);
    for task_id in &spec.source_task_ids {
        append_json_string(root, "source_task_ids", task_id);
    }
    for chat_session_id in &spec.source_chat_session_ids {
        append_json_string(root, "source_chat_session_ids", chat_session_id);
    }
    let update = json!({
        "candidate_id": candidate.id,
        "summary": spec.summary,
        "workflow_signature": spec.workflow_signature,
        "success_count": spec.success_count,
        "failure_count": spec.failure_count,
    });
    root.entry("phase_11_updates".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(updates) = root
        .get_mut("phase_11_updates")
        .and_then(Value::as_array_mut)
    {
        updates.push(update);
    }
}

fn append_unique_strings(target: &mut Vec<String>, additions: &[String]) {
    for value in additions {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if !target.iter().any(|existing| existing == value) {
            target.push(truncate_text(value, 1_000));
        }
    }
}

fn append_unique_evidence_refs(
    target: &mut Vec<LearningEvidenceRef>,
    additions: &[LearningEvidenceRef],
) {
    for value in additions {
        if !target.contains(value) {
            target.push(value.clone());
        }
    }
}

fn append_json_string(root: &mut serde_json::Map<String, Value>, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    root.entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(items) = root.get_mut(key).and_then(Value::as_array_mut) {
        if !items.iter().any(|item| item.as_str() == Some(value)) {
            items.push(Value::String(value.to_string()));
        }
    }
}

fn procedure_payload(candidate: &LearningCandidate) -> Value {
    if let Some(value) = candidate.proposed_change.get("procedure") {
        return value.clone();
    }
    if let Some(value) = candidate.proposed_change.get("memory") {
        if let Some(procedure) = value.get("procedure") {
            return procedure.clone();
        }
        if let Some(memory_value) = value.get("value") {
            return match memory_value {
                Value::Object(map) => {
                    let mut out = map.clone();
                    for key in [
                        "key",
                        "target",
                        "teaching_action",
                        "explicit_user_request",
                        "explicit_user_correction",
                    ] {
                        if let Some(inherited) = value.get(key) {
                            out.entry(key.to_string())
                                .or_insert_with(|| inherited.clone());
                        }
                    }
                    Value::Object(out)
                },
                Value::String(text) => json!({
                    "title": candidate.title,
                    "summary": candidate.summary,
                    "procedure_steps": [text],
                }),
                other => json!({
                    "title": candidate.title,
                    "summary": candidate.summary,
                    "procedure": other,
                }),
            };
        }
        return value.clone();
    }
    if let Some(value) = candidate.proposed_change.get("workflow_template") {
        return value.clone();
    }
    candidate.proposed_change.clone()
}

fn procedure_workflow_signature(procedure: &LearningProcedure) -> Option<String> {
    procedure
        .payload
        .get("workflow_signature")
        .and_then(Value::as_str)
        .map(normalize_signature)
        .filter(|signature| !signature.is_empty())
}

fn deterministic_procedure_id(
    owner_agent: Option<&str>,
    workflow_signature: Option<&str>,
    title: &str,
    candidate: &LearningCandidate,
) -> String {
    let seed = format!(
        "{}|{}|{}|{}|{}",
        owner_agent.unwrap_or("agent"),
        workflow_signature.unwrap_or(""),
        title,
        candidate.source_task_id.as_deref().unwrap_or(""),
        candidate.source_chat_session_id.as_deref().unwrap_or("")
    );
    let digest = fnv1a64(seed.as_bytes());
    let slug = sanitize_procedure_id(title);
    let slug = if slug.is_empty() {
        "procedure".to_string()
    } else {
        slug.chars().take(56).collect()
    };
    format!("proc_{slug}_{digest:016x}")
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn sanitize_procedure_id(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.trim().to_ascii_lowercase().chars() {
        let next = if ch.is_ascii_alphanumeric() || ch == '_' {
            Some(ch)
        } else if ch == '-' || ch.is_ascii_whitespace() {
            Some('-')
        } else {
            None
        };
        if let Some(ch) = next {
            if ch == '-' {
                if !last_dash && !out.is_empty() {
                    out.push(ch);
                    last_dash = true;
                }
            } else {
                out.push(ch);
                last_dash = false;
            }
        }
        if out.len() >= 96 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn read_string_any(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    })
}

fn read_u64_any(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
        })
    })
}

fn read_bool_any(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|value| {
            value.as_bool().or_else(|| {
                value
                    .as_str()
                    .map(str::trim)
                    .map(|text| matches!(text.to_ascii_lowercase().as_str(), "true" | "yes" | "1"))
            })
        })
    })
}

fn deprecation_action_string(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "deprecate"
            | "deprecated"
            | "retire"
            | "retired"
            | "archive"
            | "archived"
            | "supersede"
            | "superseded"
    )
}

fn read_string_list_nested(value: &Value, paths: &[&str]) -> Option<Vec<String>> {
    paths
        .iter()
        .filter_map(|path| path_value(value, path))
        .find_map(string_list_from_value)
}

fn path_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn string_list_from_value(value: &Value) -> Option<Vec<String>> {
    let values = match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(|text| truncate_text(text, 1_000))
                    .or_else(|| {
                        item.as_object().and_then(|object| {
                            object
                                .get("text")
                                .or_else(|| object.get("summary"))
                                .or_else(|| object.get("description"))
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|text| !text.is_empty())
                                .map(|text| truncate_text(text, 1_000))
                        })
                    })
            })
            .collect::<Vec<_>>(),
        Value::String(text) => text
            .lines()
            .map(|line| {
                line.trim()
                    .trim_start_matches(|ch: char| {
                        ch == '-' || ch == '*' || ch.is_ascii_digit() || ch == '.'
                    })
                    .trim()
            })
            .filter(|line| !line.is_empty())
            .map(|line| truncate_text(line, 1_000))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    (!values.is_empty()).then_some(values)
}

fn default_success_count(candidate: &LearningCandidate) -> u64 {
    if matches!(candidate.risk_level, super::LearningRiskLevel::Low) {
        1
    } else {
        0
    }
}

fn default_failure_count(candidate: &LearningCandidate) -> u64 {
    if matches!(
        candidate.risk_level,
        super::LearningRiskLevel::High | super::LearningRiskLevel::Critical
    ) {
        1
    } else {
        0
    }
}

fn normalize_signature(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
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
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::learning::{CreateLearningCandidateRequest, LearningRiskLevel};

    #[test]
    fn reviewed_promotion_activates_existing_routed_draft_without_double_counting() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        let candidate = store
            .create_candidate(
                scope.clone(),
                procedure_candidate("sig_browser", None, None),
            )
            .expect("create candidate");
        let bridge = LearningProcedureBridge::new(workspace);

        let route = bridge
            .route_candidate(&store, &scope, &candidate)
            .expect("route procedure candidate");
        assert!(route.routed);
        let draft = store
            .read_procedure(&scope, route.procedure_id.as_deref().expect("procedure id"))
            .expect("read draft");
        assert_eq!(draft.status, LearningProcedureStatus::Draft);
        assert_eq!(draft.success_count, 1);

        let latest_candidate = store
            .read_candidate(&scope, &candidate.id)
            .expect("read candidate");
        let promoted = bridge
            .promote_reviewed_candidate(
                &store,
                &scope,
                &latest_candidate,
                "reviewer",
                "approved reusable browser procedure",
            )
            .expect("promote candidate");
        assert_eq!(promoted.state, LearningCandidateState::Promoted);

        let active = store
            .read_procedure(&scope, route.procedure_id.as_deref().expect("procedure id"))
            .expect("read active");
        assert_eq!(active.status, LearningProcedureStatus::Active);
        assert_eq!(active.success_count, 1);
    }

    #[test]
    fn reviewed_update_candidate_merges_into_existing_active_procedure() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        let existing = store
            .create_procedure(
                scope.clone(),
                CreateLearningProcedureRequest {
                    principal: None,
                    workspace: None,
                    id: Some("proc_existing".to_string()),
                    actor: "test".to_string(),
                    reason: Some("seed".to_string()),
                    status: LearningProcedureStatus::Draft,
                    title: "Existing procedure".to_string(),
                    summary: "Existing summary".to_string(),
                    owner_agent: Some("agent-a".to_string()),
                    activation: LearningProcedureActivation::default(),
                    workflow: vec!["Open source artifact".to_string()],
                    decision_points: Vec::new(),
                    verification: vec!["Verify original output".to_string()],
                    failure_modes: Vec::new(),
                    evidence_refs: Vec::new(),
                    source_candidate_id: None,
                    source_task_ids: Vec::new(),
                    source_chat_session_ids: Vec::new(),
                    success_count: 2,
                    failure_count: 0,
                    payload: json!({ "workflow_signature": "sig_existing" }),
                },
            )
            .expect("create existing procedure");
        store
            .transition_procedure_status(
                &scope,
                &existing.id,
                LearningProcedureStatus::Active,
                "test",
                "activate_seed",
                "seed active procedure",
                Vec::new(),
            )
            .expect("activate existing");
        let candidate = store
            .create_candidate(
                scope.clone(),
                procedure_candidate(
                    "sig_existing",
                    Some("proc_existing"),
                    Some("Verify new table output"),
                ),
            )
            .expect("create update candidate");
        let bridge = LearningProcedureBridge::new(workspace);

        bridge
            .promote_reviewed_candidate(
                &store,
                &scope,
                &candidate,
                "reviewer",
                "approved procedure update",
            )
            .expect("promote update candidate");

        let updated = store
            .read_procedure(&scope, "proc_existing")
            .expect("read updated procedure");
        assert_eq!(updated.status, LearningProcedureStatus::Active);
        assert!(updated
            .verification
            .iter()
            .any(|step| step == "Verify new table output"));
        assert_eq!(updated.success_count, 3);
        assert!(updated
            .payload
            .get("phase_11_updates")
            .and_then(Value::as_array)
            .is_some_and(|updates| updates.len() == 1));
    }

    #[test]
    fn archived_procedure_id_is_not_reused_as_live_dedupe_target() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        store
            .create_procedure(
                scope.clone(),
                CreateLearningProcedureRequest {
                    principal: None,
                    workspace: None,
                    id: Some("proc_retired".to_string()),
                    actor: "test".to_string(),
                    reason: Some("seed".to_string()),
                    status: LearningProcedureStatus::Draft,
                    title: "Retired procedure".to_string(),
                    summary: "Retired summary".to_string(),
                    owner_agent: None,
                    activation: LearningProcedureActivation::default(),
                    workflow: Vec::new(),
                    decision_points: Vec::new(),
                    verification: Vec::new(),
                    failure_modes: Vec::new(),
                    evidence_refs: Vec::new(),
                    source_candidate_id: None,
                    source_task_ids: Vec::new(),
                    source_chat_session_ids: Vec::new(),
                    success_count: 0,
                    failure_count: 0,
                    payload: json!({ "workflow_signature": "sig_retired" }),
                },
            )
            .expect("create retired procedure");
        store
            .transition_procedure_status(
                &scope,
                "proc_retired",
                LearningProcedureStatus::Archived,
                "test",
                "archive_seed",
                "archive retired procedure",
                Vec::new(),
            )
            .expect("archive procedure");
        let candidate = store
            .create_candidate(
                scope.clone(),
                procedure_candidate("sig_new", None, Some("Verify new flow"))
                    .with_procedure_id("proc_retired"),
            )
            .expect("create candidate");
        let bridge = LearningProcedureBridge::new(workspace);

        let route = bridge
            .route_candidate(&store, &scope, &candidate)
            .expect("route candidate");
        let new_id = route.procedure_id.expect("new procedure id");
        assert_ne!(new_id, "proc_retired");
        assert!(new_id.starts_with("proc_retired-new-"));
        let draft = store
            .read_procedure(&scope, &new_id)
            .expect("read new draft");
        assert_eq!(draft.status, LearningProcedureStatus::Draft);
    }

    #[test]
    fn reviewed_candidate_does_not_merge_by_title_and_owner_only() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        let existing = store
            .create_procedure(
                scope.clone(),
                CreateLearningProcedureRequest {
                    principal: None,
                    workspace: None,
                    id: Some("proc_title_only".to_string()),
                    actor: "test".to_string(),
                    reason: Some("seed".to_string()),
                    status: LearningProcedureStatus::Draft,
                    title: "Reusable browser procedure".to_string(),
                    summary: "Existing summary".to_string(),
                    owner_agent: Some("agent-a".to_string()),
                    activation: LearningProcedureActivation::default(),
                    workflow: vec!["Existing step".to_string()],
                    decision_points: Vec::new(),
                    verification: Vec::new(),
                    failure_modes: Vec::new(),
                    evidence_refs: Vec::new(),
                    source_candidate_id: None,
                    source_task_ids: Vec::new(),
                    source_chat_session_ids: Vec::new(),
                    success_count: 7,
                    failure_count: 0,
                    payload: json!({}),
                },
            )
            .expect("create existing procedure");
        store
            .transition_procedure_status(
                &scope,
                &existing.id,
                LearningProcedureStatus::Active,
                "test",
                "activate_seed",
                "seed active procedure",
                Vec::new(),
            )
            .expect("activate existing");
        let candidate = store
            .create_candidate(
                scope.clone(),
                procedure_candidate("", None, Some("Verify unrelated title-only flow")),
            )
            .expect("create title-only candidate");
        let bridge = LearningProcedureBridge::new(workspace);

        bridge
            .promote_reviewed_candidate(
                &store,
                &scope,
                &candidate,
                "reviewer",
                "approved separate procedure",
            )
            .expect("promote candidate");

        let unchanged = store
            .read_procedure(&scope, "proc_title_only")
            .expect("read unchanged existing procedure");
        assert_eq!(unchanged.success_count, 7);
        assert!(!unchanged
            .verification
            .iter()
            .any(|step| step == "Verify unrelated title-only flow"));
        let active = store
            .list_procedures(
                &scope,
                LearningProcedureFilters {
                    status: Some("active".to_string()),
                    owner_agent: Some("agent-a".to_string()),
                    limit: None,
                },
            )
            .expect("list active procedures");
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn reviewed_deprecation_candidate_deprecates_existing_active_procedure() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("principal", "workspace");
        let existing = store
            .create_procedure(
                scope.clone(),
                CreateLearningProcedureRequest {
                    principal: None,
                    workspace: None,
                    id: Some("proc_stale".to_string()),
                    actor: "test".to_string(),
                    reason: Some("seed".to_string()),
                    status: LearningProcedureStatus::Draft,
                    title: "Stale browser procedure".to_string(),
                    summary: "Old flow that no longer works.".to_string(),
                    owner_agent: Some("agent-a".to_string()),
                    activation: LearningProcedureActivation::default(),
                    workflow: vec!["Use stale flow".to_string()],
                    decision_points: Vec::new(),
                    verification: Vec::new(),
                    failure_modes: Vec::new(),
                    evidence_refs: Vec::new(),
                    source_candidate_id: None,
                    source_task_ids: Vec::new(),
                    source_chat_session_ids: Vec::new(),
                    success_count: 0,
                    failure_count: 3,
                    payload: json!({ "workflow_signature": "sig_stale" }),
                },
            )
            .expect("create stale procedure");
        store
            .transition_procedure_status(
                &scope,
                &existing.id,
                LearningProcedureStatus::Active,
                "test",
                "activate_seed",
                "seed active procedure",
                Vec::new(),
            )
            .expect("activate existing");
        let candidate = store
            .create_candidate(
                scope.clone(),
                procedure_candidate("sig_stale", Some("proc_stale"), None).with_deprecation(
                    "The page changed and this procedure repeatedly misleads the agent.",
                ),
            )
            .expect("create deprecation candidate");
        let bridge = LearningProcedureBridge::new(workspace);

        let route = bridge
            .route_candidate(&store, &scope, &candidate)
            .expect("route deprecation candidate");
        assert!(route.routed);
        assert_eq!(route.procedure_id.as_deref(), Some("proc_stale"));
        assert!(route.existing_procedure_reused);
        let routed_candidate = store
            .read_candidate(&scope, &candidate.id)
            .expect("read routed candidate");

        let promoted = bridge
            .promote_reviewed_candidate(
                &store,
                &scope,
                &routed_candidate,
                "reviewer",
                "approved procedure deprecation",
            )
            .expect("promote deprecation candidate");
        assert_eq!(promoted.state, LearningCandidateState::Promoted);

        let deprecated = store
            .read_procedure(&scope, "proc_stale")
            .expect("read deprecated procedure");
        assert_eq!(deprecated.status, LearningProcedureStatus::Deprecated);
    }

    trait ProcedureCandidateRequestExt {
        fn with_procedure_id(self, procedure_id: &str) -> Self;
        fn with_deprecation(self, reason: &str) -> Self;
    }

    impl ProcedureCandidateRequestExt for CreateLearningCandidateRequest {
        fn with_procedure_id(mut self, procedure_id: &str) -> Self {
            if let Some(object) = self
                .proposed_change
                .get_mut("procedure")
                .and_then(Value::as_object_mut)
            {
                object.insert(
                    "procedure_id".to_string(),
                    Value::String(procedure_id.to_string()),
                );
            }
            self
        }

        fn with_deprecation(mut self, reason: &str) -> Self {
            if let Some(object) = self
                .proposed_change
                .get_mut("procedure")
                .and_then(Value::as_object_mut)
            {
                object.insert("deprecation_recommended".to_string(), Value::Bool(true));
                object.insert(
                    "deprecation_reason".to_string(),
                    Value::String(reason.to_string()),
                );
            }
            self
        }
    }

    fn procedure_candidate(
        signature: &str,
        existing_procedure_id: Option<&str>,
        verification: Option<&str>,
    ) -> CreateLearningCandidateRequest {
        CreateLearningCandidateRequest {
            principal: None,
            workspace: None,
            candidate_type: LearningCandidateType::MemoryProcedure,
            state: LearningCandidateState::Proposed,
            title: "Reusable browser procedure".to_string(),
            summary: "Use this flow for repeated browser work.".to_string(),
            rationale: "Repeated workflow evidence.".to_string(),
            proposed_change: json!({
                "procedure": {
                    "title": "Reusable browser procedure",
                    "summary": "Use this flow for repeated browser work.",
                    "existing_procedure_id": existing_procedure_id,
                    "activation": {
                        "use_when": ["browser work repeats"],
                        "avoid_when": [],
                        "example_goals": ["complete browser test"]
                    },
                    "workflow": ["Open page", "Act", "Verify"],
                    "verification": verification.into_iter().collect::<Vec<_>>(),
                    "failure_modes": ["Do not skip verification"],
                    "workflow_signature": signature,
                    "success_count": 1
                }
            }),
            proposed_target: Some("learning.procedures.draft".to_string()),
            confidence: Some(0.9),
            source_agent_id: Some("agent-a".to_string()),
            source_task_id: Some("task-a".to_string()),
            source_execution_id: Some("exec-a".to_string()),
            source_chat_session_id: None,
            event_refs: Vec::new(),
            evidence_refs: Vec::new(),
            risk_level: LearningRiskLevel::Medium,
            review_required: true,
            review_reason: Some("procedure requires review".to_string()),
            review_policy: json!({ "requires_review": true }),
            promotion_target: Some("learning.procedures.draft".to_string()),
            promotion_policy: json!({ "bridge": "learning_procedure_bridge" }),
        }
    }
}
