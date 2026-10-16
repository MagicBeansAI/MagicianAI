use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::{
    CreateLearningEventRequest, LearningCandidate, LearningCandidateState,
    LearningEvaluationBacklogItem, LearningEvaluationBacklogStatus, LearningRiskLevel,
    LearningScope, LearningStore,
};

const DEFAULT_EVAL_CASE_KIND: &str = "regression";
const LEARNING_EVAL_BRIDGE_ACTOR: &str = "learning_eval_bridge";

#[derive(Debug, Clone, Serialize)]
pub struct LearningEvalRouteOutcome {
    pub candidate_id: String,
    pub routed: bool,
    pub backlog_path: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct LearningEvalBridge {
    workspace_layout: ArtifactV2Workspace,
}

#[derive(Debug, Clone)]
struct EvalBacklogSpec {
    case_kind: String,
    priority: String,
    target_agent_id: Option<String>,
    focus_area: Option<String>,
    case_spec: Value,
}

impl LearningEvalBridge {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub fn route_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
    ) -> Result<LearningEvalRouteOutcome> {
        if !candidate.candidate_type.is_evaluation_candidate() {
            return Ok(LearningEvalRouteOutcome {
                candidate_id: candidate.id.clone(),
                routed: false,
                backlog_path: None,
                reason: "not_an_evaluation_candidate".to_string(),
            });
        }
        let stored_candidate = match store.read_candidate(scope, &candidate.id) {
            Ok(value) => Some(value),
            Err(error) if store.error_is_not_found(&error) => None,
            Err(error) => return Err(error),
        };
        let candidate = stored_candidate.as_ref().unwrap_or(candidate);
        if candidate.state.is_terminal() {
            return Ok(LearningEvalRouteOutcome {
                candidate_id: candidate.id.clone(),
                routed: false,
                backlog_path: None,
                reason: format!("candidate_state_is_terminal: {}", candidate.state.as_str()),
            });
        }

        let spec = EvalBacklogSpec::from_candidate(candidate);
        let now = Utc::now();
        let existing = store
            .read_evaluation_backlog_item(scope, &candidate.id)
            .ok();
        let created_at = existing.as_ref().map(|item| item.created_at).unwrap_or(now);
        let status = existing
            .as_ref()
            .map(|item| item.status.clone())
            .unwrap_or(LearningEvaluationBacklogStatus::Queued);
        let item = LearningEvaluationBacklogItem {
            id: format!(
                "leb_{}",
                candidate.id.strip_prefix("lc_").unwrap_or(&candidate.id)
            ),
            scope: scope.clone(),
            candidate_id: candidate.id.clone(),
            status,
            title: candidate.title.clone(),
            summary: candidate.summary.clone(),
            rationale: candidate.rationale.clone(),
            case_kind: spec.case_kind.clone(),
            priority: spec.priority.clone(),
            target_agent_id: spec.target_agent_id.clone(),
            focus_area: spec.focus_area.clone(),
            proposed_target: candidate.proposed_target.clone(),
            source_agent_id: candidate.source_agent_id.clone(),
            source_task_id: candidate.source_task_id.clone(),
            source_execution_id: candidate.source_execution_id.clone(),
            source_chat_session_id: candidate.source_chat_session_id.clone(),
            evidence_refs: candidate.evidence_refs.clone(),
            case_spec: spec.case_spec.clone(),
            created_at,
            updated_at: now,
        };
        store.write_evaluation_backlog_item(&item)?;

        let backlog_path = self.workspace_layout.learning_evaluation_backlog_path(
            &scope.principal,
            &scope.workspace,
            &candidate.id,
        );
        let event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_eval_candidate_routed".to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Learning evaluation candidate `{}` routed to evaluation backlog.",
                    candidate.id
                ),
                evidence_refs: candidate.evidence_refs.clone(),
                payload: json!({
                    "candidate_id": candidate.id,
                    "backlog_id": item.id,
                    "backlog_path": backlog_path.display().to_string(),
                    "case_kind": item.case_kind,
                    "priority": item.priority,
                    "target_agent_id": item.target_agent_id,
                    "focus_area": item.focus_area,
                    "existing_item_updated": existing.is_some(),
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
                LEARNING_EVAL_BRIDGE_ACTOR,
                "routed_to_eval_backlog",
                format!(
                    "Evaluation candidate was routed to the meta-harness backlog; learning event {} records the backlog item.",
                    event.id
                ),
                candidate.evidence_refs.clone(),
            )?;
        }

        Ok(LearningEvalRouteOutcome {
            candidate_id: candidate.id.clone(),
            routed: true,
            backlog_path: Some(backlog_path.display().to_string()),
            reason: "routed_to_eval_backlog".to_string(),
        })
    }
}

impl EvalBacklogSpec {
    fn from_candidate(candidate: &LearningCandidate) -> Self {
        let payload = candidate
            .proposed_change
            .get("evaluation")
            .or_else(|| candidate.proposed_change.get("eval"))
            .cloned()
            .unwrap_or_else(|| candidate.proposed_change.clone());
        let case_kind = read_string_any(
            &payload,
            &["case_kind", "kind", "eval_kind", "evaluation_kind", "type"],
        )
        .map(|value| normalize_token(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_EVAL_CASE_KIND.to_string());
        let priority = read_string_any(&payload, &["priority", "severity"])
            .map(|value| normalize_token(&value))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| default_priority(&candidate.risk_level).to_string());
        let target_agent_id = read_string_any(
            &payload,
            &[
                "target_agent_id",
                "agent_id",
                "target_agent",
                "owner_agent_id",
            ],
        );
        let focus_area = read_string_any(&payload, &["focus_area", "goal_id", "suite"]);
        let diagnosis = candidate
            .proposed_change
            .get("meta_harness_diagnosis")
            .or_else(|| candidate.proposed_change.get("diagnosis"))
            .or_else(|| payload.get("meta_harness_diagnosis"))
            .or_else(|| payload.get("diagnosis"))
            .cloned();
        let mut case_spec = if payload.is_object() {
            payload
        } else {
            json!({
                "value": payload
            })
        };
        if let (Some(diagnosis), Some(map)) = (diagnosis, case_spec.as_object_mut()) {
            map.entry("meta_harness_diagnosis".to_string())
                .or_insert(diagnosis);
        }

        Self {
            case_kind,
            priority,
            target_agent_id,
            focus_area,
            case_spec,
        }
    }
}

fn default_priority(risk: &LearningRiskLevel) -> &'static str {
    match risk {
        LearningRiskLevel::Low => "low",
        LearningRiskLevel::Medium => "normal",
        LearningRiskLevel::High | LearningRiskLevel::Critical => "high",
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

fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

pub fn log_eval_route_error(candidate_id: &str, error: &anyhow::Error) {
    warn!(
        candidate_id = %candidate_id,
        error = %error,
        "Learning evaluation candidate routing failed"
    );
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use crate::magician_v2::learning::{
        CreateLearningCandidateRequest, LearningCandidateState, LearningCandidateType,
        LearningEvidenceRef,
    };

    use super::*;

    #[test]
    fn route_eval_candidate_writes_backlog_and_triages_source_candidate() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test-principal", "test-workspace");
        let candidate = store
            .create_candidate(scope.clone(), eval_candidate_request())
            .expect("create candidate");

        let outcome = LearningEvalBridge::new(workspace)
            .route_candidate(&store, &scope, &candidate)
            .expect("route eval candidate");

        assert!(outcome.routed);
        assert_eq!(outcome.candidate_id, candidate.id);
        assert_eq!(outcome.reason, "routed_to_eval_backlog");
        assert!(outcome
            .backlog_path
            .as_deref()
            .expect("backlog path")
            .ends_with(&format!("{}.json", candidate.id)));

        let item = store
            .read_evaluation_backlog_item(&scope, &candidate.id)
            .expect("read backlog item");
        assert_eq!(item.candidate_id, candidate.id);
        assert_eq!(item.status, LearningEvaluationBacklogStatus::Queued);
        assert_eq!(item.case_kind, "capability_eval");
        assert_eq!(item.priority, "high");
        assert_eq!(item.target_agent_id.as_deref(), Some("personal-assistant"));
        assert_eq!(item.focus_area.as_deref(), Some("browser-sota"));
        assert_eq!(item.source_task_id.as_deref(), Some("task_eval"));
        assert_eq!(
            item.case_spec.get("goal").and_then(Value::as_str),
            Some("complete the slider case without false pass marking")
        );

        let reread_candidate = store
            .read_candidate(&scope, &candidate.id)
            .expect("read candidate");
        assert_eq!(reread_candidate.state, LearningCandidateState::Triaged);

        let decisions = store
            .read_decisions(&scope, &candidate.id)
            .expect("read decisions");
        assert!(decisions
            .iter()
            .any(|decision| decision.decision == "routed_to_eval_backlog"));

        let events = store.list_events(&scope, 10).expect("list events");
        assert!(events
            .iter()
            .any(|event| event.event_type == "learning_eval_candidate_routed"));
    }

    #[test]
    fn route_non_eval_candidate_is_noop() {
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let store = LearningStore::new(workspace.clone());
        let scope = LearningScope::new("test-principal", "test-workspace");
        let mut request = eval_candidate_request();
        request.candidate_type = LearningCandidateType::BugReport;
        let candidate = store
            .create_candidate(scope.clone(), request)
            .expect("create candidate");

        let outcome = LearningEvalBridge::new(workspace)
            .route_candidate(&store, &scope, &candidate)
            .expect("route non-eval candidate");

        assert!(!outcome.routed);
        assert_eq!(outcome.reason, "not_an_evaluation_candidate");
        assert!(store
            .read_evaluation_backlog_item(&scope, &candidate.id)
            .is_err());
        let reread_candidate = store
            .read_candidate(&scope, &candidate.id)
            .expect("read candidate");
        assert_eq!(reread_candidate.state, LearningCandidateState::Proposed);
    }

    fn eval_candidate_request() -> CreateLearningCandidateRequest {
        CreateLearningCandidateRequest {
            principal: None,
            workspace: None,
            candidate_type: LearningCandidateType::EvaluationCase,
            state: LearningCandidateState::Proposed,
            title: "Browser slider regression eval".to_string(),
            summary: "A browser SoTA slider run needs a durable regression case.".to_string(),
            rationale: "The agent marked a case pass before the slider value changed.".to_string(),
            proposed_change: json!({
                "evaluation": {
                    "case_kind": "capability-eval",
                    "priority": "high",
                    "target_agent_id": "personal-assistant",
                    "focus_area": "browser-sota",
                    "goal": "complete the slider case without false pass marking",
                    "failure_mode": "false pass radio selection",
                    "expected_behavior": "slider value changes before Pass is selected",
                    "reproduction_steps": ["open the SoTA slider page", "complete case 1"],
                    "success_criteria": ["value changed", "pass selected only after verification"],
                    "source_refs": ["execution:exec_eval"]
                }
            }),
            proposed_target: Some("meta-harness:browser-sota".to_string()),
            confidence: Some(0.88),
            source_agent_id: Some("personal-assistant".to_string()),
            source_task_id: Some("task_eval".to_string()),
            source_execution_id: Some("exec_eval".to_string()),
            source_chat_session_id: None,
            event_refs: Vec::new(),
            evidence_refs: vec![LearningEvidenceRef {
                kind: "execution".to_string(),
                id: Some("exec_eval".to_string()),
                path: Some("tasks/task_eval/executions/exec_eval/events.jsonl".to_string()),
                uri: None,
                summary: Some("Slider was not changed before Pass was selected.".to_string()),
            }],
            risk_level: LearningRiskLevel::Medium,
            review_required: true,
            review_reason: Some(
                "Evaluation candidates are reviewed by the meta-harness.".to_string(),
            ),
            review_policy: json!({"requires_meta_harness_review": true}),
            promotion_target: None,
            promotion_policy: json!({"eligible_for_auto_promotion": false}),
        }
    }
}
