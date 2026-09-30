use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::magician_v2::{
    agents::AgentDefinitionStore,
    artifact_v2::workspace::ArtifactV2Workspace,
    harness::{ProgramLoader, ProgramRuntimeState},
};

use super::{
    CreateLearningEventRequest, LearningCandidate, LearningCandidateState, LearningCandidateType,
    LearningRiskLevel, LearningScope, LearningStore,
};

const LEARNING_PROGRAM_STATE_BRIDGE_ACTOR: &str = "learning_program_state_bridge";

/// Provenance actor stamped on the inverse update written by an owner revert.
const LEARNING_PROGRAM_STATE_REVERT_ACTOR: &str = "owner_revert";

/// Default program document path used when a harness agent's focus area does
/// not override `program` (mirrors `harness::program`'s private default).
const DEFAULT_PROGRAM_DOC: &str = "program.md";

/// Owner-notification event emitted whenever the harness self-management lane
/// auto-applies a bookkeeping/control update. Carries the changed fields'
/// before/after values plus the metadata needed to revert.
pub const HARNESS_PROGRAM_STATE_AUTO_APPLIED_EVENT: &str = "harness_program_state_auto_applied";

/// Event emitted when an owner reverts a previously auto-applied bookkeeping
/// update (the inverse is applied as a new provenance-stamped update, never a
/// history deletion).
pub const HARNESS_PROGRAM_STATE_REVERTED_EVENT: &str = "harness_program_state_reverted";

/// Bookkeeping/control fields the harness self-management lane may auto-apply
/// without owner review. MUST stay a strict subset of the fields
/// `ProgramRuntimeState::apply_update` accepts; `goal_id` and every
/// immutable/structural field are deliberately excluded so this lane can only
/// advance a program's own bookkeeping, never rebind or restructure it.
const HARNESS_AUTO_APPLY_FIELDS: &[&str] = &[
    "current_phase",
    "current_step",
    "open_loops",
    "next_action_hints",
    "last_run_summary",
    "stop_conditions",
    "stop_condition_status",
    "blocked",
    "blocked_status",
    "escalation_status",
    "recent_learning_candidates",
    "learning_candidates",
    "last_meta_harness_verdict",
    "meta_harness_verdict",
    // Boundary C. Bookkeeping in the same sense as the rest of this list: it
    // records the continuation a cycle settled on and its budget, and confers
    // no authority. It is deliberately NOT a control field — a cycle saying
    // "I am waiting on approval" describes itself; it does not pause the lane,
    // which stays the owner's to do.
    "last_cycle_decision",
];

/// Control-semantics fields whose auto-apply must raise an ELEVATED-priority
/// owner notification — a self-pause / self-block must be immediately visible.
const HARNESS_AUTO_APPLY_CONTROL_FIELDS: &[&str] = &[
    "stop_conditions",
    "stop_condition_status",
    "blocked",
    "blocked_status",
    "escalation_status",
];

#[derive(Debug, Clone, Serialize)]
pub struct LearningProgramStateRouteOutcome {
    pub candidate_id: String,
    pub applied: bool,
    pub state_path: Option<String>,
    pub reason: String,
}

/// Classification of a `program_state_update` candidate against the harness
/// self-management auto-apply lane (Phase 4.1).
#[derive(Debug, Clone, Copy)]
struct HarnessAutoApplyClassification {
    /// True when the candidate is harness-self-originated, whitelist-only
    /// bookkeeping that may auto-apply with no human review.
    eligible: bool,
    /// True when the patch touches a control-semantics field
    /// (`stop_conditions`/`blocked`) and so must raise an elevated-priority
    /// owner notification.
    touches_control_field: bool,
}

/// Outcome of an owner revert of a previously auto-applied bookkeeping update.
#[derive(Debug, Clone, Serialize)]
pub struct LearningProgramStateRevertOutcome {
    pub reverted_event_id: String,
    pub state_path: Option<String>,
    pub reverted_event_log_id: String,
    pub update_history_len: usize,
    pub history_growth: usize,
}

#[derive(Debug, Clone)]
pub struct LearningProgramStateBridge {
    workspace_layout: ArtifactV2Workspace,
}

#[derive(Debug, Clone)]
struct ProgramStateCandidateSpec {
    program_relative_path: String,
    program_section: Option<String>,
    goal_id: Option<String>,
    patch: Value,
    reason: String,
}

impl LearningProgramStateBridge {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub async fn route_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
    ) -> Result<LearningProgramStateRouteOutcome> {
        if candidate.candidate_type != LearningCandidateType::ProgramStateUpdate {
            return Ok(LearningProgramStateRouteOutcome {
                candidate_id: candidate.id.clone(),
                applied: false,
                state_path: None,
                reason: "not_a_program_state_update_candidate".to_string(),
            });
        }

        let stored_candidate = match store.read_candidate(scope, &candidate.id) {
            Ok(value) => Some(value),
            Err(error) if store.error_is_not_found(&error) => None,
            Err(error) => return Err(error),
        };
        let candidate = stored_candidate.as_ref().unwrap_or(candidate);
        if candidate.state.is_terminal() {
            return Ok(LearningProgramStateRouteOutcome {
                candidate_id: candidate.id.clone(),
                applied: false,
                state_path: None,
                reason: format!("candidate_state_is_terminal: {}", candidate.state.as_str()),
            });
        }

        // Phase 4.1: harness self-originated bookkeeping updates auto-apply with
        // no human review when the patch stays inside the bookkeeping whitelist
        // and the provenance is the focus-area owner agent. This never weakens
        // the owner-approval diode for definition/skill/persona/delegation
        // changes — the lane only ever writes the agent's own bookkeeping fields.
        let auto_apply = self
            .classify_harness_bookkeeping_auto_apply(scope, candidate)
            .await;

        let can_apply = candidate.state == LearningCandidateState::Approved
            || (!candidate.review_required && candidate.risk_level == LearningRiskLevel::Low)
            || auto_apply.eligible;
        if !can_apply {
            let event = store.append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_program_state_candidate_review_gated".to_string(),
                    agent_id: candidate.source_agent_id.clone(),
                    task_id: candidate.source_task_id.clone(),
                    execution_id: candidate.source_execution_id.clone(),
                    chat_session_id: candidate.source_chat_session_id.clone(),
                    summary: format!(
                        "Learning program-state candidate `{}` requires review before application.",
                        candidate.id
                    ),
                    evidence_refs: candidate.evidence_refs.clone(),
                    payload: json!({
                        "candidate_id": candidate.id,
                        "risk_level": candidate.risk_level.as_str(),
                        "review_required": candidate.review_required,
                        "review_reason": candidate.review_reason,
                    }),
                },
            )?;
            if matches!(
                candidate.state,
                LearningCandidateState::Observed | LearningCandidateState::Proposed
            ) {
                let _ = store.transition_candidate(
                    scope,
                    &candidate.id,
                    LearningCandidateState::Triaged,
                    LEARNING_PROGRAM_STATE_BRIDGE_ACTOR,
                    "review_required",
                    format!(
                        "Program-state candidate is review-gated; learning event {} records the hold.",
                        event.id
                    ),
                    candidate.evidence_refs.clone(),
                )?;
            }
            return Ok(LearningProgramStateRouteOutcome {
                candidate_id: candidate.id.clone(),
                applied: false,
                state_path: None,
                reason: "review_required".to_string(),
            });
        }

        let Some(spec) = ProgramStateCandidateSpec::from_candidate(candidate)? else {
            // No focus-area program document path resolved. Rather than writing
            // to a wrong default document (the old `program.md` fallback), treat
            // this candidate as a no-op for the program-state lane.
            return Ok(LearningProgramStateRouteOutcome {
                candidate_id: candidate.id.clone(),
                applied: false,
                state_path: None,
                reason: "not_a_program_state_update_candidate".to_string(),
            });
        };
        let spec = self
            .canonicalize_candidate_spec(scope, candidate, spec)
            .await?;
        let loader = ProgramLoader::new(self.workspace_layout.clone());
        let loaded = loader
            .load_by_reference(
                &scope.principal,
                &scope.workspace,
                &spec.program_relative_path,
                spec.program_section.as_deref(),
            )
            .await
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| {
                anyhow!(
                    "program document `{}` was not found for program-state update",
                    spec.program_relative_path
                )
            })?;
        let mut state = loader
            .load_or_create_runtime_state(
                &scope.principal,
                &scope.workspace,
                &loaded,
                spec.goal_id.as_deref(),
            )
            .await
            .map_err(anyhow::Error::msg)?;
        // Snapshot the touched bookkeeping fields BEFORE the update so an
        // auto-apply notification can carry before/after and a revert can apply
        // the inverse. Only captured for the auto-apply lane.
        let before_snapshot = auto_apply
            .eligible
            .then(|| snapshot_state_fields(&state, &spec.patch));
        state
            .apply_update(
                spec.patch.clone(),
                LEARNING_PROGRAM_STATE_BRIDGE_ACTOR,
                spec.reason.clone(),
                Some(candidate.id.clone()),
                candidate.source_task_id.clone(),
                candidate.source_execution_id.clone(),
            )
            .map_err(anyhow::Error::msg)?;
        loader
            .write_runtime_state(
                &scope.principal,
                &scope.workspace,
                &loaded,
                spec.goal_id.as_deref(),
                &state,
            )
            .await
            .map_err(anyhow::Error::msg)?;
        let after_snapshot = before_snapshot
            .as_ref()
            .map(|_| snapshot_state_fields(&state, &spec.patch));
        let state_path = state.program.state_relative_path.clone();

        let event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_program_state_candidate_applied".to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Learning program-state candidate `{}` updated runtime state `{}`.",
                    candidate.id, state_path
                ),
                evidence_refs: candidate.evidence_refs.clone(),
                payload: json!({
                    "candidate_id": candidate.id,
                    "program_relative_path": loaded.relative_path,
                    "program_section": loaded.section,
                    "goal_id": spec.goal_id,
                    "state_path": state_path,
                    "patch": spec.patch,
                }),
            },
        )?;

        // Phase 4.1: every auto-apply emits a durable, revertible owner
        // notification carrying the changed fields' before/after + the
        // update_history entry id. Control-field auto-applies are elevated.
        if let (Some(before), Some(after)) = (before_snapshot, after_snapshot) {
            let priority = if auto_apply.touches_control_field {
                "elevated"
            } else {
                "normal"
            };
            let update_history_index = state.update_history.len().saturating_sub(1);
            let _ = store.append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: HARNESS_PROGRAM_STATE_AUTO_APPLIED_EVENT.to_string(),
                    agent_id: candidate.source_agent_id.clone(),
                    task_id: candidate.source_task_id.clone(),
                    execution_id: candidate.source_execution_id.clone(),
                    chat_session_id: candidate.source_chat_session_id.clone(),
                    summary: format!(
                        "Harness bookkeeping auto-applied to `{}`{}.",
                        state_path,
                        if auto_apply.touches_control_field {
                            " (control field — elevated)"
                        } else {
                            ""
                        }
                    ),
                    evidence_refs: candidate.evidence_refs.clone(),
                    payload: json!({
                        "candidate_id": candidate.id,
                        "source_agent_id": candidate.source_agent_id,
                        "program_relative_path": loaded.relative_path,
                        "program_section": loaded.section,
                        "goal_id": spec.goal_id,
                        "state_path": state_path,
                        "applied_event_id": event.id,
                        "update_history_index": update_history_index,
                        "updated_at": state.updated_at,
                        "before": Value::Object(before),
                        "after": Value::Object(after),
                        "touches_control_field": auto_apply.touches_control_field,
                        "elevated": auto_apply.touches_control_field,
                        "priority": priority,
                        "revertible": true,
                    }),
                },
            )?;
        }

        let target_state = if candidate.state == LearningCandidateState::Approved {
            LearningCandidateState::Promoted
        } else {
            LearningCandidateState::Promoted
        };
        let _ = store.transition_candidate(
            scope,
            &candidate.id,
            target_state,
            LEARNING_PROGRAM_STATE_BRIDGE_ACTOR,
            "program_state_applied",
            format!(
                "Program-state update was applied to `{}`; learning event {} records the write.",
                state_path, event.id
            ),
            candidate.evidence_refs.clone(),
        )?;

        Ok(LearningProgramStateRouteOutcome {
            candidate_id: candidate.id.clone(),
            applied: true,
            state_path: Some(state_path),
            reason: "program_state_applied".to_string(),
        })
    }

    pub async fn promote_reviewed_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        actor: &str,
        reason: &str,
    ) -> Result<LearningCandidate> {
        let mut approved = candidate.clone();
        if approved.state != LearningCandidateState::Approved {
            approved = store.transition_candidate(
                scope,
                &candidate.id,
                LearningCandidateState::Approved,
                actor,
                "approved_program_state_update",
                reason,
                candidate.evidence_refs.clone(),
            )?;
        }
        let outcome = self.route_candidate(store, scope, &approved).await?;
        if !outcome.applied {
            return Err(anyhow!(
                "program-state candidate `{}` was not applied: {}",
                candidate.id,
                outcome.reason
            ));
        }
        store.read_candidate(scope, &candidate.id)
    }

    /// Classify a `program_state_update` candidate against the harness
    /// self-management auto-apply lane (Phase 4.1). Eligible iff ALL hold:
    /// (a) provenance is the harness agent that owns the targeted focus area,
    /// (b) the patch touches only the bookkeeping whitelist, and (c) it asserts
    /// no structural/definition change (guaranteed by (b)). Any ambiguity
    /// (no spec, no patch, non-whitelist key, non-harness provenance) leaves the
    /// candidate on its existing owner-gated governance path.
    async fn classify_harness_bookkeeping_auto_apply(
        &self,
        scope: &LearningScope,
        candidate: &LearningCandidate,
    ) -> HarnessAutoApplyClassification {
        let not_eligible = HarnessAutoApplyClassification {
            eligible: false,
            touches_control_field: false,
        };
        if candidate.candidate_type != LearningCandidateType::ProgramStateUpdate {
            return not_eligible;
        }
        let spec = match ProgramStateCandidateSpec::from_candidate(candidate) {
            Ok(Some(spec)) => spec,
            _ => return not_eligible,
        };
        let spec = match self
            .canonicalize_candidate_spec(scope, candidate, spec)
            .await
        {
            Ok(spec) => spec,
            Err(_) => return not_eligible,
        };
        // (b)+(c): the patch may touch ONLY whitelisted bookkeeping/control
        // fields. apply_update would reject anything else, but auto-apply
        // (skipping review) must additionally never fire for a patch that mixes
        // in a structural or unsupported field.
        if !patch_is_bookkeeping_whitelisted(&spec.patch) {
            return not_eligible;
        }
        // (a): provenance must be the harness agent that owns this focus area.
        if !self
            .candidate_is_harness_self_owned(scope, candidate, &spec.program_relative_path)
            .await
        {
            return not_eligible;
        }
        HarnessAutoApplyClassification {
            eligible: true,
            touches_control_field: patch_touches_control_field(&spec.patch),
        }
    }

    /// True when the candidate's `source_agent_id` resolves to a harness-enabled
    /// agent that owns a focus area targeting `program_relative_path` (the
    /// "focus-area owner agent"). Fails closed on any load error or mismatch so
    /// the auto-apply lane can never be entered by a foreign or worker agent.
    async fn candidate_is_harness_self_owned(
        &self,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        program_relative_path: &str,
    ) -> bool {
        let Some(agent_id) = candidate
            .source_agent_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return false;
        };
        let store = AgentDefinitionStore::with_workspace_layout(self.workspace_layout.clone())
            .for_scope(&scope.principal, &scope.workspace);
        let record = match store.get_definition(agent_id).await {
            Ok(Some(record)) => record,
            _ => return false,
        };
        let definition = &record.definition;
        if definition.harness.is_none() {
            return false;
        }
        let Some(autonomous) = definition.autonomous_config.as_ref() else {
            return false;
        };
        let target = normalize_program_doc_key(program_relative_path);
        autonomous.focus_areas.iter().any(|focus_area| {
            match focus_area
                .program
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some(program) => normalize_program_doc_key(program) == target,
                None => target == normalize_program_doc_key(DEFAULT_PROGRAM_DOC),
            }
        })
    }

    /// Replace model-echoed path/section fields with the authoritative program
    /// reference owned by the source harness agent. A model may identify the
    /// intended document, but it cannot invent a section or escape the source
    /// agent's declared focus areas.
    async fn canonicalize_candidate_spec(
        &self,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        spec: ProgramStateCandidateSpec,
    ) -> Result<ProgramStateCandidateSpec> {
        let source_agent_id = candidate
            .source_agent_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("program-state candidate has no source agent"))?;
        let definition_store =
            AgentDefinitionStore::with_workspace_layout(self.workspace_layout.clone())
                .for_scope(&scope.principal, &scope.workspace);
        let record = definition_store
            .get_definition(source_agent_id)
            .await?
            .ok_or_else(|| anyhow!("source agent `{source_agent_id}` was not found"))?;
        let definition = &record.definition;
        if definition.harness.is_none() {
            return Err(anyhow!(
                "source agent `{source_agent_id}` is not harness-enabled"
            ));
        }

        let requested_path = normalize_program_doc_key(&spec.program_relative_path);
        let loader = ProgramLoader::new(self.workspace_layout.clone());
        let loaded = if let Some(goal_id) = spec.goal_id.as_deref() {
            loader
                .load_for_goal(
                    &scope.principal,
                    &scope.workspace,
                    definition,
                    Some(goal_id),
                )
                .await
                .map_err(anyhow::Error::msg)?
        } else {
            let matching_focus_area = definition.autonomous_config.as_ref().and_then(|config| {
                config.focus_areas.iter().find(|focus_area| {
                    focus_area
                        .program
                        .as_deref()
                        .map(normalize_program_doc_key)
                        .is_some_and(|path| path == requested_path)
                })
            });
            if matching_focus_area.is_some() {
                loader
                    .load_for_focus_area(
                        &scope.principal,
                        &scope.workspace,
                        definition,
                        matching_focus_area,
                    )
                    .await
                    .map_err(anyhow::Error::msg)?
            } else if requested_path == normalize_program_doc_key(DEFAULT_PROGRAM_DOC) {
                loader
                    .load_for_focus_area(&scope.principal, &scope.workspace, definition, None)
                    .await
                    .map_err(anyhow::Error::msg)?
            } else {
                return Err(anyhow!(
                    "program `{}` is not owned by source agent `{source_agent_id}`",
                    spec.program_relative_path
                ));
            }
        }
        .ok_or_else(|| {
            anyhow!(
                "authoritative program document for source agent `{source_agent_id}` was not found"
            )
        })?;

        canonicalize_spec_from_loaded(spec, loaded)
    }

    /// Revert a previously auto-applied bookkeeping update by applying the
    /// inverse (`before` snapshot captured in the auto-apply event) as a NEW
    /// provenance-stamped update (`actor=owner_revert`). Never deletes history
    /// and never escapes the bookkeeping whitelist.
    pub async fn revert_auto_applied(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        auto_applied_event_id: &str,
        reason: &str,
        scan: usize,
    ) -> Result<LearningProgramStateRevertOutcome> {
        let events = store.list_events(scope, scan.clamp(1, 50_000))?;
        let event = events
            .into_iter()
            .find(|event| {
                event.id == auto_applied_event_id
                    && event.event_type == HARNESS_PROGRAM_STATE_AUTO_APPLIED_EVENT
            })
            .ok_or_else(|| {
                anyhow!(
                    "harness auto-apply event `{}` was not found in scope",
                    auto_applied_event_id
                )
            })?;
        let payload = event.payload.clone();
        let program_relative_path = payload
            .get("program_relative_path")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("auto-apply event is missing `program_relative_path`"))?;
        let program_section = payload
            .get("program_section")
            .and_then(Value::as_str)
            .map(str::to_string);
        let goal_id = payload
            .get("goal_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let before = payload
            .get("before")
            .cloned()
            .filter(Value::is_object)
            .ok_or_else(|| anyhow!("auto-apply event is missing a `before` snapshot"))?;
        // Defense-in-depth: the inverse patch must itself stay within the
        // bookkeeping whitelist — a revert can never escape the lane it undoes.
        if !patch_is_bookkeeping_whitelisted(&before) {
            return Err(anyhow!(
                "auto-apply `before` snapshot contains a non-whitelisted field; refusing to revert"
            ));
        }
        let loader = ProgramLoader::new(self.workspace_layout.clone());
        let loaded = loader
            .load_by_reference(
                &scope.principal,
                &scope.workspace,
                &program_relative_path,
                program_section.as_deref(),
            )
            .await
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| {
                anyhow!(
                    "program document `{}` was not found for revert",
                    program_relative_path
                )
            })?;
        let mut state = loader
            .load_or_create_runtime_state(
                &scope.principal,
                &scope.workspace,
                &loaded,
                goal_id.as_deref(),
            )
            .await
            .map_err(anyhow::Error::msg)?;
        // Optimistic concurrency: refuse to revert if a later cycle advanced the
        // bookkeeping past this auto-apply. If the `after` snapshot the auto-apply
        // recorded no longer matches current state, a newer cycle changed these
        // fields and reverting would clobber it — so we refuse and let the owner
        // reconcile first rather than silently overwrite newer bookkeeping.
        if let Some(after) = payload.get("after").cloned().filter(Value::is_object) {
            let current = Value::Object(snapshot_state_fields(&state, &after));
            if current != after {
                return Err(anyhow!(
                    "program state advanced since auto-apply `{}` (a later cycle changed these bookkeeping fields); refusing to revert to avoid clobbering newer state",
                    auto_applied_event_id
                ));
            }
        }
        let history_before = state.update_history.len();
        state
            .apply_update(
                before.clone(),
                LEARNING_PROGRAM_STATE_REVERT_ACTOR,
                reason.to_string(),
                None,
                None,
                None,
            )
            .map_err(anyhow::Error::msg)?;
        loader
            .write_runtime_state(
                &scope.principal,
                &scope.workspace,
                &loaded,
                goal_id.as_deref(),
                &state,
            )
            .await
            .map_err(anyhow::Error::msg)?;
        let state_path = state.program.state_relative_path.clone();
        let reverted = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: HARNESS_PROGRAM_STATE_REVERTED_EVENT.to_string(),
                agent_id: event.agent_id.clone(),
                task_id: None,
                execution_id: None,
                chat_session_id: None,
                summary: format!(
                    "Owner reverted harness bookkeeping auto-apply `{}` on `{}`.",
                    auto_applied_event_id, state_path
                ),
                evidence_refs: Vec::new(),
                payload: json!({
                    "reverted_event_id": auto_applied_event_id,
                    "candidate_id": payload.get("candidate_id").cloned().unwrap_or(Value::Null),
                    "program_relative_path": loaded.relative_path,
                    "program_section": loaded.section,
                    "goal_id": goal_id,
                    "state_path": state_path,
                    "restored": before,
                    "actor": LEARNING_PROGRAM_STATE_REVERT_ACTOR,
                    "reason": reason,
                    "update_history_len": state.update_history.len(),
                }),
            },
        )?;
        Ok(LearningProgramStateRevertOutcome {
            reverted_event_id: auto_applied_event_id.to_string(),
            state_path: Some(state_path),
            reverted_event_log_id: reverted.id,
            update_history_len: state.update_history.len(),
            history_growth: state.update_history.len().saturating_sub(history_before),
        })
    }
}

impl ProgramStateCandidateSpec {
    fn from_candidate(candidate: &LearningCandidate) -> Result<Option<Self>> {
        let payload = candidate
            .proposed_change
            .get("program_state_update")
            .or_else(|| candidate.proposed_change.get("program_state"))
            .cloned()
            .unwrap_or_else(|| candidate.proposed_change.clone());
        let Some(program_relative_path) = read_string_any(
            &payload,
            &[
                "program_relative_path",
                "relative_path",
                "program_path",
                "program",
            ],
        )
        .or_else(|| {
            candidate
                .proposed_target
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        }) else {
            // No focus-area program document path on the candidate. The caller
            // routes this to the program-state no-op instead of defaulting to a
            // wrong document.
            return Ok(None);
        };
        let program_section = read_string_any(
            &payload,
            &["program_section", "section", "program_section_name"],
        );
        let goal_id = read_string_any(&payload, &["goal_id", "goal", "focus_goal_id"]);
        let patch = payload
            .get("patch")
            .or_else(|| payload.get("state_patch"))
            .or_else(|| payload.get("update"))
            .cloned()
            .ok_or_else(|| anyhow!("program-state candidate is missing `patch`"))?;
        if !patch.is_object() {
            return Err(anyhow!("program-state candidate `patch` must be an object"));
        }
        let reason = read_string_any(&payload, &["reason", "update_reason"])
            .unwrap_or_else(|| candidate.summary.clone());
        Ok(Some(Self {
            program_relative_path,
            program_section,
            goal_id,
            patch,
            reason,
        }))
    }
}

fn canonicalize_spec_from_loaded(
    mut spec: ProgramStateCandidateSpec,
    loaded: crate::magician_v2::harness::LoadedProgram,
) -> Result<ProgramStateCandidateSpec> {
    if normalize_program_doc_key(&loaded.relative_path)
        != normalize_program_doc_key(&spec.program_relative_path)
    {
        return Err(anyhow!(
            "candidate program `{}` does not match authoritative program `{}`",
            spec.program_relative_path,
            loaded.relative_path
        ));
    }
    spec.program_relative_path = loaded.relative_path;
    spec.program_section = loaded.section;
    Ok(spec)
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

/// True when `patch` is a non-empty JSON object whose keys are ALL in the
/// harness bookkeeping/control whitelist (the strict subset of
/// `ProgramRuntimeState::apply_update` the auto-apply lane is allowed to write).
fn patch_is_bookkeeping_whitelisted(patch: &Value) -> bool {
    match patch {
        Value::Object(map) if !map.is_empty() => map
            .keys()
            .all(|key| HARNESS_AUTO_APPLY_FIELDS.contains(&key.as_str())),
        _ => false,
    }
}

/// True when `patch` touches a control-semantics field (`stop_conditions` /
/// `blocked`, including aliases) — those auto-applies are elevated-priority.
fn patch_touches_control_field(patch: &Value) -> bool {
    match patch {
        Value::Object(map) => map
            .keys()
            .any(|key| HARNESS_AUTO_APPLY_CONTROL_FIELDS.contains(&key.as_str())),
        _ => false,
    }
}

/// Map a whitelisted patch key (including its aliases) to the canonical
/// `ProgramRuntimeState` field name read for before/after snapshots and revert.
fn canonical_state_field(key: &str) -> Option<&'static str> {
    match key {
        "current_phase" => Some("current_phase"),
        "current_step" => Some("current_step"),
        "open_loops" => Some("open_loops"),
        "next_action_hints" => Some("next_action_hints"),
        "last_run_summary" => Some("last_run_summary"),
        "stop_conditions" | "stop_condition_status" => Some("stop_conditions"),
        "blocked" | "blocked_status" | "escalation_status" => Some("blocked"),
        "recent_learning_candidates" | "learning_candidates" => Some("recent_learning_candidates"),
        "last_meta_harness_verdict" | "meta_harness_verdict" => Some("last_meta_harness_verdict"),
        _ => None,
    }
}

/// Read the current value of one canonical bookkeeping field as JSON (null /
/// array / object as appropriate). The shape round-trips through
/// `apply_update`, so a captured before-snapshot can be re-applied verbatim as
/// the inverse on revert.
fn read_canonical_field_value(state: &ProgramRuntimeState, field: &str) -> Value {
    match field {
        "current_phase" => state
            .current_phase
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
        "current_step" => state
            .current_step
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
        "open_loops" => Value::Array(state.open_loops.clone()),
        "next_action_hints" => Value::Array(
            state
                .next_action_hints
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
        "last_run_summary" => state.last_run_summary.clone().unwrap_or(Value::Null),
        "stop_conditions" => Value::Array(state.stop_conditions.clone()),
        "blocked" => state.blocked.clone().unwrap_or(Value::Null),
        "recent_learning_candidates" => Value::Array(state.recent_learning_candidates.clone()),
        "last_meta_harness_verdict" => state
            .last_meta_harness_verdict
            .clone()
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// Snapshot the canonical bookkeeping fields a patch touches, keyed by the
/// canonical field name. Used to capture before/after values for the owner
/// notification and to compute the inverse patch for revert.
fn snapshot_state_fields(
    state: &ProgramRuntimeState,
    patch: &Value,
) -> serde_json::Map<String, Value> {
    let mut snapshot = serde_json::Map::new();
    if let Value::Object(map) = patch {
        for key in map.keys() {
            if let Some(field) = canonical_state_field(key) {
                snapshot
                    .entry(field.to_string())
                    .or_insert_with(|| read_canonical_field_value(state, field));
            }
        }
    }
    snapshot
}

/// Normalize a program document path for owner-agent matching: trims, drops a
/// leading `./`, and strips a leading `programs/` so a focus-area `program`
/// value and a candidate path compare equal regardless of prefix style.
fn normalize_program_doc_key(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches("./");
    trimmed
        .strip_prefix("programs/")
        .unwrap_or(trimmed)
        .to_string()
}

pub fn log_program_state_route_error(candidate_id: &str, error: &anyhow::Error) {
    warn!(
        target: "magician::learning",
        candidate_id,
        error = %error,
        "failed to route learning program-state candidate"
    );
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::harness::LoadedProgram;

    fn seed_state() -> ProgramRuntimeState {
        let loaded = LoadedProgram {
            relative_path: "revenue_strategy.md".to_string(),
            section: None,
            title: Some("Revenue".to_string()),
            content: "x".to_string(),
        };
        ProgramRuntimeState::new(
            "anonymous",
            "default",
            &loaded,
            Some("harness:cro:revenue"),
            "programs/state/revenue.json".to_string(),
            "seed",
            "seed-init",
            json!({}),
        )
    }

    /// The `can_apply` truth table is driven by the whitelist + provenance gate.
    /// The provenance half needs a real definition store, so here we lock the
    /// PURE predicates: only harness bookkeeping/control patches qualify for the
    /// auto-apply lane; anything structural/definition-shaped stays owner-gated.
    #[test]
    fn bookkeeping_whitelist_truth_table() {
        // Pure progress-note bookkeeping → whitelisted, NOT a control field.
        let progress = json!({"current_phase": "review", "next_action_hints": ["a"], "last_run_summary": {"ok": true}});
        assert!(patch_is_bookkeeping_whitelisted(&progress));
        assert!(!patch_touches_control_field(&progress));

        // Control-semantics fields → whitelisted AND control (elevated).
        let stop = json!({"stop_conditions": [{"id": "owner_pause", "status": "open"}]});
        assert!(patch_is_bookkeeping_whitelisted(&stop));
        assert!(patch_touches_control_field(&stop));
        let blocked = json!({"blocked": {"status": "paused"}});
        assert!(patch_is_bookkeeping_whitelisted(&blocked));
        assert!(patch_touches_control_field(&blocked));
        // Aliases are recognized too.
        assert!(patch_is_bookkeeping_whitelisted(
            &json!({"blocked_status": {"x": 1}})
        ));
        assert!(patch_touches_control_field(
            &json!({"stop_condition_status": []})
        ));

        // Structural / definition fields → NOT whitelisted (stay owner-gated).
        assert!(!patch_is_bookkeeping_whitelisted(
            &json!({"persona": "new"})
        ));
        assert!(!patch_is_bookkeeping_whitelisted(&json!({"tools": ["x"]})));
        assert!(!patch_is_bookkeeping_whitelisted(
            &json!({"program": "other.md"})
        ));
        // goal_id is accepted by apply_update but deliberately excluded here so
        // the lane can never rebind a program's focus.
        assert!(!patch_is_bookkeeping_whitelisted(&json!({"goal_id": "g2"})));
        // Mixed (whitelist + structural) → NOT eligible.
        assert!(!patch_is_bookkeeping_whitelisted(
            &json!({"current_phase": "x", "persona": "y"})
        ));
        // Empty / non-object → NOT eligible.
        assert!(!patch_is_bookkeeping_whitelisted(&json!({})));
        assert!(!patch_is_bookkeeping_whitelisted(&json!("nope")));
        assert!(!patch_is_bookkeeping_whitelisted(&Value::Null));
    }

    /// Boundary C's decision is bookkeeping, not control.
    ///
    /// A cycle recording "I am waiting on approval" describes itself; it must
    /// not acquire the elevated semantics of `blocked` or `stop_conditions`,
    /// because pausing a lane stays the owner's act. If this ever starts
    /// reading as a control field, a harness gained the power to stop itself
    /// by narrating.
    #[test]
    fn the_cycle_continuation_decision_is_bookkeeping_and_never_control() {
        let decision = json!({
            "last_cycle_decision": {
                "decision": "request_approval",
                "terminal_for_cycle": true,
                "budget": {"elapsed_ms": 120}
            }
        });
        assert!(patch_is_bookkeeping_whitelisted(&decision));
        assert!(!patch_touches_control_field(&decision));

        // And it cannot smuggle a structural change alongside itself.
        assert!(!patch_is_bookkeeping_whitelisted(&json!({
            "last_cycle_decision": {"decision": "stop"},
            "persona": "rewritten"
        })));
    }

    /// apply -> revert -> equals the pre-apply snapshot; history grows by 2
    /// (one apply + one inverse); the inverse never escapes the whitelist.
    #[test]
    fn revert_restores_pre_apply_snapshot_without_whitelist_escape() {
        let mut state = seed_state();
        // Establish a baseline so the pre-apply values are non-trivial.
        state
            .apply_update(
                json!({"current_phase": "phase-a"}),
                "seed",
                "baseline",
                None,
                None,
                None,
            )
            .unwrap();
        let history_after_baseline = state.update_history.len();
        let pre_apply_phase = state.current_phase.clone();
        let pre_apply_blocked = state.blocked.clone();
        let pre_apply_stop = state.stop_conditions.clone();

        // The harness bookkeeping/control auto-apply patch.
        let patch = json!({
            "current_phase": "phase-b",
            "blocked": {"status": "paused"},
            "stop_conditions": [{"id": "owner_pause", "status": "open"}],
        });
        assert!(patch_is_bookkeeping_whitelisted(&patch));
        assert!(patch_touches_control_field(&patch));

        // Capture the inverse snapshot BEFORE applying, exactly as the bridge does.
        let before = snapshot_state_fields(&state, &patch);
        state
            .apply_update(patch.clone(), "auto", "auto-apply", None, None, None)
            .unwrap();
        assert_eq!(state.current_phase.as_deref(), Some("phase-b"));
        assert!(state.blocked.is_some());
        assert_eq!(state.stop_conditions.len(), 1);

        // The inverse patch must itself stay inside the whitelist (no escape).
        let inverse = Value::Object(before);
        assert!(patch_is_bookkeeping_whitelisted(&inverse));

        // Revert = apply the inverse as a NEW update.
        state
            .apply_update(
                inverse,
                LEARNING_PROGRAM_STATE_REVERT_ACTOR,
                "revert",
                None,
                None,
                None,
            )
            .unwrap();

        // State equals the pre-apply snapshot.
        assert_eq!(state.current_phase, pre_apply_phase);
        assert_eq!(state.blocked, pre_apply_blocked);
        assert_eq!(state.stop_conditions, pre_apply_stop);
        assert_eq!(state.updated_by, LEARNING_PROGRAM_STATE_REVERT_ACTOR);
        // History grew by exactly 2 (apply + revert) — nothing was deleted.
        assert_eq!(state.update_history.len(), history_after_baseline + 2);
    }

    #[test]
    fn authoritative_program_reference_removes_hallucinated_focus_area_section() {
        let spec = ProgramStateCandidateSpec {
            program_relative_path: "product_ops.md".to_string(),
            program_section: Some("runtime state".to_string()),
            goal_id: Some("harness:cpo:product-ops".to_string()),
            patch: json!({"current_phase": "review"}),
            reason: "advance".to_string(),
        };
        let loaded = LoadedProgram {
            relative_path: "product_ops.md".to_string(),
            section: None,
            title: Some("Product Ops".to_string()),
            content: "Program".to_string(),
        };

        let canonical = canonicalize_spec_from_loaded(spec, loaded).unwrap();
        assert_eq!(canonical.program_relative_path, "product_ops.md");
        assert!(canonical.program_section.is_none());
    }

    #[test]
    fn authoritative_program_reference_rejects_foreign_path() {
        let spec = ProgramStateCandidateSpec {
            program_relative_path: "foreign.md".to_string(),
            program_section: None,
            goal_id: None,
            patch: json!({"current_phase": "review"}),
            reason: "advance".to_string(),
        };
        let loaded = LoadedProgram {
            relative_path: "program.md".to_string(),
            section: Some("product".to_string()),
            title: None,
            content: "Program".to_string(),
        };

        assert!(canonicalize_spec_from_loaded(spec, loaded).is_err());
    }
}
