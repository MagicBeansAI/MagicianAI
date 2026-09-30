use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::{
    CreateLearningCandidateRequest, CreateLearningEventRequest, LearningCandidate,
    LearningCandidateState, LearningCandidateType, LearningCapabilityEvolutionBridge,
    LearningEvalBridge, LearningEvent, LearningEventRef, LearningEvidenceRef, LearningMemoryBridge,
    LearningProcedureBridge, LearningRiskLevel, LearningScope, LearningStore,
};

const MAX_TEACHING_CONTENT_CHARS: usize = 12_000;
const TEACHING_ACTOR: &str = "user_teaching";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningTeachingAction {
    Remember,
    Forget,
    Correct,
    MakeReusable,
    ImproveTool,
    NeverDoThis,
    ThisWasUseful,
    ThisWasWrong,
}

impl LearningTeachingAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Remember => "remember",
            Self::Forget => "forget",
            Self::Correct => "correct",
            Self::MakeReusable => "make_reusable",
            Self::ImproveTool => "improve_tool",
            Self::NeverDoThis => "never_do_this",
            Self::ThisWasUseful => "this_was_useful",
            Self::ThisWasWrong => "this_was_wrong",
        }
    }

    fn event_summary_prefix(&self) -> &'static str {
        match self {
            Self::Remember => "Remember",
            Self::Forget => "Forget",
            Self::Correct => "Correct",
            Self::MakeReusable => "Make reusable",
            Self::ImproveTool => "Improve tool",
            Self::NeverDoThis => "Never do this",
            Self::ThisWasUseful => "This was useful",
            Self::ThisWasWrong => "This was wrong",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LearningTeachingTarget {
    pub kind: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub path: Option<String>,
    pub uri: Option<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLearningTeachingFeedbackRequest {
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub action: LearningTeachingAction,
    pub content: String,
    pub correction: Option<String>,
    pub target: Option<LearningTeachingTarget>,
    pub source_agent_id: Option<String>,
    pub source_task_id: Option<String>,
    pub source_execution_id: Option<String>,
    pub source_chat_session_id: Option<String>,
    pub confidence: Option<f64>,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningTeachingRouteOutcome {
    pub candidate_id: String,
    pub routed: bool,
    pub promoted: bool,
    pub route_kind: String,
    pub reason: String,
    pub detail: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningTeachingFeedbackResponse {
    pub scope: LearningScope,
    pub event: LearningEvent,
    pub candidates: Vec<LearningCandidate>,
    pub route_outcomes: Vec<LearningTeachingRouteOutcome>,
    pub durable_change_count: usize,
    pub review_required_count: usize,
}

pub async fn record_teaching_feedback(
    store: &LearningStore,
    workspace_layout: &ArtifactV2Workspace,
    scope: LearningScope,
    request: CreateLearningTeachingFeedbackRequest,
) -> Result<LearningTeachingFeedbackResponse> {
    let content = bounded_required_text(&request.content, "content")?;
    let correction = request
        .correction
        .as_deref()
        .map(|value| bounded_required_text(value, "correction"))
        .transpose()?;
    let evidence_refs = teaching_evidence_refs(&request);
    let event = store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_user_teaching_recorded".to_string(),
            agent_id: request.source_agent_id.clone(),
            task_id: request.source_task_id.clone(),
            execution_id: request.source_execution_id.clone(),
            chat_session_id: request.source_chat_session_id.clone(),
            summary: format!(
                "{}: {}",
                request.action.event_summary_prefix(),
                one_line_excerpt(&content, 180)
            ),
            evidence_refs: evidence_refs.clone(),
            payload: json!({
                "source": TEACHING_ACTOR,
                "action": request.action.as_str(),
                "content": content.clone(),
                "correction": correction.clone(),
                "target": request.target.clone(),
                "payload": request.payload.clone(),
            }),
        },
    )?;

    let event_ref = LearningEventRef {
        event_id: event.id.clone(),
        event_type: event.event_type.clone(),
    };
    let candidate_requests = build_candidate_requests(
        &request,
        &content,
        correction.as_deref(),
        &event_ref,
        &evidence_refs,
    );
    let mut candidates = Vec::new();
    let mut route_outcomes = Vec::new();
    for candidate_request in candidate_requests {
        let candidate = store.create_candidate(scope.clone(), candidate_request)?;
        let outcome =
            match route_teaching_candidate(store, workspace_layout, &scope, &candidate).await {
                Ok(outcome) => outcome,
                Err(error) => LearningTeachingRouteOutcome {
                    candidate_id: candidate.id.clone(),
                    routed: false,
                    promoted: false,
                    route_kind: "route_error".to_string(),
                    reason: format!("route_failed: {error}"),
                    detail: json!({
                        "candidate_type": candidate.candidate_type.as_str(),
                        "error": error.to_string(),
                    }),
                },
            };
        let latest = store
            .read_candidate(&scope, &candidate.id)
            .unwrap_or(candidate);
        candidates.push(latest);
        route_outcomes.push(outcome);
    }
    let durable_change_count = route_outcomes
        .iter()
        .filter(|outcome| outcome.promoted)
        .count();
    let review_required_count = candidates
        .iter()
        .filter(|candidate| {
            candidate.review_required
                || matches!(
                    candidate.state,
                    LearningCandidateState::Triaged
                        | LearningCandidateState::Proposed
                        | LearningCandidateState::Observed
                )
        })
        .count();

    Ok(LearningTeachingFeedbackResponse {
        scope,
        event,
        candidates,
        route_outcomes,
        durable_change_count,
        review_required_count,
    })
}

fn build_candidate_requests(
    request: &CreateLearningTeachingFeedbackRequest,
    content: &str,
    correction: Option<&str>,
    event_ref: &LearningEventRef,
    evidence_refs: &[LearningEvidenceRef],
) -> Vec<CreateLearningCandidateRequest> {
    match request.action {
        LearningTeachingAction::Remember => vec![memory_candidate(
            request,
            LearningCandidateType::MemoryFact,
            "Remember",
            "knowledge",
            "upsert",
            teaching_key(request, content, "remember"),
            Value::String(content.to_string()),
            false,
            false,
            event_ref,
            evidence_refs,
        )],
        LearningTeachingAction::Forget => vec![memory_candidate(
            request,
            LearningCandidateType::MemoryFact,
            "Forget",
            teaching_tier(request, "knowledge").as_str(),
            "remove",
            teaching_key(request, content, "forget"),
            Value::Null,
            true,
            false,
            event_ref,
            evidence_refs,
        )],
        LearningTeachingAction::Correct => vec![memory_candidate(
            request,
            memory_type_from_payload(request, LearningCandidateType::MemoryFact),
            "Correct",
            teaching_tier(request, "knowledge").as_str(),
            "replace",
            teaching_key(request, content, "correct"),
            Value::String(correction.unwrap_or(content).to_string()),
            false,
            true,
            event_ref,
            evidence_refs,
        )],
        LearningTeachingAction::NeverDoThis => vec![memory_candidate(
            request,
            LearningCandidateType::MemoryPreference,
            "Never do this",
            "preferences",
            "upsert",
            teaching_key(request, content, "never"),
            Value::String(format!("Never do this: {content}")),
            false,
            false,
            event_ref,
            evidence_refs,
        )],
        LearningTeachingAction::ThisWasUseful => vec![procedure_candidate(
            request,
            "Useful pattern",
            content,
            event_ref,
            evidence_refs,
        )],
        LearningTeachingAction::MakeReusable => vec![procedure_candidate(
            request,
            "Reusable workflow",
            content,
            event_ref,
            evidence_refs,
        )],
        LearningTeachingAction::ImproveTool => vec![capability_candidate(
            request,
            content,
            event_ref,
            evidence_refs,
        )],
        LearningTeachingAction::ThisWasWrong => vec![evaluation_candidate(
            request,
            content,
            correction,
            event_ref,
            evidence_refs,
        )],
    }
}

fn memory_candidate(
    request: &CreateLearningTeachingFeedbackRequest,
    candidate_type: LearningCandidateType,
    label: &str,
    target_tier: &str,
    operation: &str,
    key: String,
    value: Value,
    explicit_forget: bool,
    explicit_correction: bool,
    event_ref: &LearningEventRef,
    evidence_refs: &[LearningEvidenceRef],
) -> CreateLearningCandidateRequest {
    let promotion_target = format!("user.{target_tier}");
    CreateLearningCandidateRequest {
        principal: None,
        workspace: None,
        candidate_type,
        state: LearningCandidateState::Observed,
        title: format!("{label}: {}", target_label(request)),
        summary: one_line_excerpt(&request.content, 500),
        rationale: "Explicit user teaching/correction was recorded outside the transcript so the durable learning path can review or apply it.".to_string(),
        proposed_change: json!({
            "memory": {
                "scope": "user",
                "target_tier": target_tier,
                "operation": operation,
                "key": key,
                "value": value,
                "explicit_user_request": true,
                "explicit_user_correction": explicit_correction,
                "teaching_action": request.action.as_str(),
                "target": request.target,
                "source": TEACHING_ACTOR,
                "forget_request": explicit_forget,
            }
        }),
        proposed_target: Some(promotion_target.clone()),
        confidence: Some(request.confidence.unwrap_or(1.0).clamp(0.0, 1.0)),
        source_agent_id: request.source_agent_id.clone(),
        source_task_id: request.source_task_id.clone(),
        source_execution_id: request.source_execution_id.clone(),
        source_chat_session_id: request.source_chat_session_id.clone(),
        event_refs: vec![event_ref.clone()],
        evidence_refs: evidence_refs.to_vec(),
        risk_level: LearningRiskLevel::Low,
        review_required: false,
        review_reason: None,
        review_policy: json!({
            "source": TEACHING_ACTOR,
            "auto_promote_if_explicit_low_risk_user_memory": true,
        }),
        promotion_target: Some(promotion_target),
        promotion_policy: json!({
            "bridge": "learning_memory_bridge",
            "requires_explicit_user_request": true,
        }),
    }
}

fn procedure_candidate(
    request: &CreateLearningTeachingFeedbackRequest,
    label: &str,
    content: &str,
    event_ref: &LearningEventRef,
    evidence_refs: &[LearningEvidenceRef],
) -> CreateLearningCandidateRequest {
    let target = target_name(request);
    let procedure_id = read_payload_string(request, "procedure_id")
        .or_else(|| read_payload_string(request, "id"))
        .map(|value| normalize_memory_key(&value))
        .filter(|value| !value.is_empty());
    let workflow_signature = read_payload_string(request, "workflow_signature")
        .or_else(|| target.clone())
        .map(|value| normalize_memory_key(&value))
        .filter(|value| !value.is_empty());
    CreateLearningCandidateRequest {
        principal: None,
        workspace: None,
        candidate_type: LearningCandidateType::MemoryProcedure,
        state: LearningCandidateState::Observed,
        title: format!("{label}: {}", target_label(request)),
        summary: one_line_excerpt(content, 500),
        rationale: "User explicitly identified reusable procedural knowledge; route it into the procedure registry as a draft for review before future retrieval.".to_string(),
        proposed_change: json!({
            "procedure": {
                "procedure_id": procedure_id,
                "title": format!("{label}: {}", target_label(request)),
                "summary": content,
                "activation": {
                    "use_when": [content],
                    "avoid_when": [],
                    "example_goals": target.clone().into_iter().collect::<Vec<_>>()
                },
                "workflow": [content],
                "decision_points": [],
                "verification": [],
                "failure_modes": [],
                "workflow_signature": workflow_signature,
                "success_count": 1,
                "target": request.target,
                "source": TEACHING_ACTOR,
                "teaching_action": request.action.as_str()
            }
        }),
        proposed_target: Some("learning.procedures.draft".to_string()),
        confidence: Some(request.confidence.unwrap_or(0.9).clamp(0.0, 1.0)),
        source_agent_id: request.source_agent_id.clone(),
        source_task_id: request.source_task_id.clone(),
        source_execution_id: request.source_execution_id.clone(),
        source_chat_session_id: request.source_chat_session_id.clone(),
        event_refs: vec![event_ref.clone()],
        evidence_refs: evidence_refs.to_vec(),
        risk_level: LearningRiskLevel::Medium,
        review_required: true,
        review_reason: Some(
            "Reusable procedures can change future behavior and require review before activation."
                .to_string(),
        ),
        review_policy: json!({ "source": TEACHING_ACTOR, "requires_review": true }),
        promotion_target: Some("learning.procedures.draft".to_string()),
        promotion_policy: json!({ "bridge": "learning_procedure_bridge" }),
    }
}

fn capability_candidate(
    request: &CreateLearningTeachingFeedbackRequest,
    content: &str,
    event_ref: &LearningEventRef,
    evidence_refs: &[LearningEvidenceRef],
) -> CreateLearningCandidateRequest {
    let capability_id = target_name(request);
    CreateLearningCandidateRequest {
        principal: None,
        workspace: None,
        candidate_type: LearningCandidateType::CapabilityUpdate,
        state: LearningCandidateState::Observed,
        title: format!("Tool improvement: {}", capability_id.as_deref().unwrap_or("unspecified tool")),
        summary: one_line_excerpt(content, 500),
        rationale: "User explicitly described a tool or skill improvement; route it to the capability-evolution backlog for review.".to_string(),
        proposed_change: json!({
            "capability_evolution": {
                "capability_id": capability_id,
                "problem": content,
                "proposed_fix_type": read_payload_string(request, "proposed_fix_type").unwrap_or_else(|| "tool_or_skill_improvement".to_string()),
                "target": request.target,
                "source": TEACHING_ACTOR,
            }
        }),
        proposed_target: target_name(request),
        confidence: Some(request.confidence.unwrap_or(0.85).clamp(0.0, 1.0)),
        source_agent_id: request.source_agent_id.clone(),
        source_task_id: request.source_task_id.clone(),
        source_execution_id: request.source_execution_id.clone(),
        source_chat_session_id: request.source_chat_session_id.clone(),
        event_refs: vec![event_ref.clone()],
        evidence_refs: evidence_refs.to_vec(),
        risk_level: LearningRiskLevel::Medium,
        review_required: true,
        review_reason: Some("Tool/schema changes require review and validation before promotion.".to_string()),
        review_policy: json!({ "source": TEACHING_ACTOR, "requires_review": true }),
        promotion_target: target_name(request),
        promotion_policy: json!({ "bridge": "learning_capability_evolution_bridge" }),
    }
}

fn evaluation_candidate(
    request: &CreateLearningTeachingFeedbackRequest,
    content: &str,
    correction: Option<&str>,
    event_ref: &LearningEventRef,
    evidence_refs: &[LearningEvidenceRef],
) -> CreateLearningCandidateRequest {
    CreateLearningCandidateRequest {
        principal: None,
        workspace: None,
        candidate_type: LearningCandidateType::EvaluationCase,
        state: LearningCandidateState::Observed,
        title: format!("Regression from user correction: {}", target_label(request)),
        summary: one_line_excerpt(content, 500),
        rationale: "User marked an outcome as wrong; preserve it as a regression/evaluation candidate instead of burying it in chat history.".to_string(),
        proposed_change: json!({
            "evaluation": {
                "case_kind": read_payload_string(request, "case_kind").unwrap_or_else(|| "regression".to_string()),
                "priority": read_payload_string(request, "priority").unwrap_or_else(|| "high".to_string()),
                "failure_observed": content,
                "expected_behavior": correction,
                "target": request.target,
                "source": TEACHING_ACTOR,
            }
        }),
        proposed_target: target_name(request),
        confidence: Some(request.confidence.unwrap_or(0.9).clamp(0.0, 1.0)),
        source_agent_id: request.source_agent_id.clone(),
        source_task_id: request.source_task_id.clone(),
        source_execution_id: request.source_execution_id.clone(),
        source_chat_session_id: request.source_chat_session_id.clone(),
        event_refs: vec![event_ref.clone()],
        evidence_refs: evidence_refs.to_vec(),
        risk_level: LearningRiskLevel::Medium,
        review_required: false,
        review_reason: None,
        review_policy: json!({ "source": TEACHING_ACTOR, "route_to_eval_backlog": true }),
        promotion_target: target_name(request),
        promotion_policy: json!({ "bridge": "learning_eval_bridge" }),
    }
}

async fn route_teaching_candidate(
    store: &LearningStore,
    workspace_layout: &ArtifactV2Workspace,
    scope: &LearningScope,
    candidate: &LearningCandidate,
) -> Result<LearningTeachingRouteOutcome> {
    if candidate.candidate_type.is_procedure_candidate() {
        let outcome = LearningProcedureBridge::new(workspace_layout.clone())
            .route_candidate(store, scope, candidate)?;
        let detail = serde_json::to_value(&outcome)?;
        return Ok(LearningTeachingRouteOutcome {
            candidate_id: outcome.candidate_id.clone(),
            routed: outcome.routed,
            promoted: outcome.promoted,
            route_kind: "procedure".to_string(),
            reason: outcome.reason.clone(),
            detail,
        });
    }
    if candidate.candidate_type.is_memory_candidate() {
        let outcome = LearningMemoryBridge::new(workspace_layout.clone())
            .route_candidate(store, scope, candidate)
            .await?;
        let detail = serde_json::to_value(&outcome)?;
        return Ok(LearningTeachingRouteOutcome {
            candidate_id: outcome.candidate_id.clone(),
            routed: outcome.routed,
            promoted: outcome.promoted,
            route_kind: "memory".to_string(),
            reason: outcome.reason.clone(),
            detail,
        });
    }
    if candidate.candidate_type.is_evaluation_candidate() {
        let outcome = LearningEvalBridge::new(workspace_layout.clone())
            .route_candidate(store, scope, candidate)?;
        let detail = serde_json::to_value(&outcome)?;
        return Ok(LearningTeachingRouteOutcome {
            candidate_id: outcome.candidate_id.clone(),
            routed: outcome.routed,
            promoted: false,
            route_kind: "evaluation".to_string(),
            reason: outcome.reason.clone(),
            detail,
        });
    }
    if candidate
        .candidate_type
        .is_skill_or_capability_evolution_candidate()
    {
        let outcome = LearningCapabilityEvolutionBridge::new(workspace_layout.clone())
            .route_candidate(store, scope, candidate)?;
        let detail = serde_json::to_value(&outcome)?;
        return Ok(LearningTeachingRouteOutcome {
            candidate_id: outcome.candidate_id.clone(),
            routed: outcome.routed,
            promoted: false,
            route_kind: "capability_evolution".to_string(),
            reason: outcome.reason.clone(),
            detail,
        });
    }
    Ok(LearningTeachingRouteOutcome {
        candidate_id: candidate.id.clone(),
        routed: false,
        promoted: false,
        route_kind: "none".to_string(),
        reason: "no_bridge_for_candidate_type".to_string(),
        detail: json!({ "candidate_type": candidate.candidate_type.as_str() }),
    })
}

fn teaching_evidence_refs(
    request: &CreateLearningTeachingFeedbackRequest,
) -> Vec<LearningEvidenceRef> {
    let mut refs = request.evidence_refs.clone();
    if let Some(target) = request.target.as_ref() {
        refs.push(LearningEvidenceRef {
            kind: target
                .kind
                .clone()
                .unwrap_or_else(|| "teaching_target".to_string()),
            id: target.id.clone().or_else(|| target.name.clone()),
            path: target.path.clone(),
            uri: target.uri.clone(),
            summary: target.summary.clone(),
        });
    }
    refs
}

fn bounded_required_text(raw: &str, field: &str) -> Result<String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(anyhow!("{field} is required"));
    }
    let mut out = String::new();
    for ch in value.chars().take(MAX_TEACHING_CONTENT_CHARS) {
        out.push(ch);
    }
    Ok(out)
}

fn target_label(request: &CreateLearningTeachingFeedbackRequest) -> String {
    target_name(request).unwrap_or_else(|| one_line_excerpt(&request.content, 80))
}

fn target_name(request: &CreateLearningTeachingFeedbackRequest) -> Option<String> {
    let target = request.target.as_ref()?;
    target
        .name
        .clone()
        .or_else(|| target.id.clone())
        .or_else(|| target.path.clone())
        .or_else(|| target.uri.clone())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn teaching_key(
    request: &CreateLearningTeachingFeedbackRequest,
    content: &str,
    fallback_prefix: &str,
) -> String {
    if let Some(key) = read_payload_string(request, "key")
        .or_else(|| read_payload_string(request, "memory_key"))
        .or_else(|| target_name(request))
    {
        return normalize_memory_key(&key);
    }
    let slug = normalize_memory_key(content);
    if slug.is_empty() {
        fallback_prefix.to_string()
    } else {
        slug.chars().take(80).collect()
    }
}

fn teaching_tier(request: &CreateLearningTeachingFeedbackRequest, fallback: &str) -> String {
    read_payload_string(request, "target_tier")
        .or_else(|| read_payload_string(request, "tier"))
        .map(|value| normalize_memory_key(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn memory_type_from_payload(
    request: &CreateLearningTeachingFeedbackRequest,
    fallback: LearningCandidateType,
) -> LearningCandidateType {
    match read_payload_string(request, "memory_type")
        .map(|value| normalize_memory_key(&value))
        .as_deref()
    {
        Some("preference") | Some("preferences") => LearningCandidateType::MemoryPreference,
        Some("procedure") | Some("workflow") | Some("workflows") => {
            LearningCandidateType::MemoryProcedure
        },
        Some("fact") | Some("knowledge") => LearningCandidateType::MemoryFact,
        _ => fallback,
    }
}

fn read_payload_string(
    request: &CreateLearningTeachingFeedbackRequest,
    key: &str,
) -> Option<String> {
    request.payload.get(key).and_then(|value| match value {
        Value::String(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    })
}

fn normalize_memory_key(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn one_line_excerpt(raw: &str, max_chars: usize) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for ch in collapsed.chars().take(max_chars) {
        out.push(ch);
    }
    if collapsed.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}
