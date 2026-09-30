//! Bridge: user-owned (ambient) work evidence → review-gated memory candidate.
//!
//! Distilled ambient browsing observations live in the user-owned evidence lane
//! (`work_evidence.json`). They are a *sibling* of memory, not memory — so an
//! agent cannot retrieve them when reasoning (see `evidence/mod.rs`). This
//! bridge promotes a salient ambient evidence record into a **review-gated**
//! `MemoryFact` candidate targeting `user.knowledge`, then routes it through the
//! existing learning→memory bridge.
//!
//! Consent posture: ambient observations are captured passively from the user's
//! own browsing, so the candidate is **always** `review_required = true` — it is
//! never auto-promoted into agent-visible memory. Once a human approves it in
//! the memory review surface, the standard promotion path writes it to the user
//! knowledge tier, where the memory retrieval index picks it up and it becomes
//! retrievable in prompts. (Signals only exist when ambient capture consent is
//! on, so the upstream consent gate already covers the capture step.)
//!
//! Failures are fail-soft: the caller logs and continues distillation.

use anyhow::Result;
use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::evidence::EvidenceRecord;

use super::{
    CreateLearningCandidateRequest, CreateLearningEventRequest, LearningCandidateState,
    LearningCandidateType, LearningEvidenceRef, LearningMemoryBridge, LearningRiskLevel,
    LearningScope, LearningStore,
};

/// Promote one user-owned (ambient) evidence record into a review-gated
/// `user.knowledge` memory candidate and route it through the learning→memory
/// bridge. Returns the created candidate id.
pub async fn route_user_evidence_to_memory(
    workspace_layout: &ArtifactV2Workspace,
    scope: &LearningScope,
    record: &EvidenceRecord,
) -> Result<String> {
    // Privacy gate: affirmatively-sensitive ambient observations (financial,
    // health, credentials, private comms, …) are never promoted into agent-visible
    // memory — not even queued for review. The review gate is the backstop for
    // `unknown`/`work`; this keeps sensitive categories out entirely.
    if crate::magician_v2::evidence::is_sensitive(&record.sensitivity) {
        tracing::debug!(
            evidence_id = %record.evidence_id,
            sensitivity = %record.sensitivity,
            "ambient evidence is sensitive; skipping memory promotion"
        );
        return Ok(String::new());
    }

    let store = LearningStore::new(workspace_layout.clone());

    let summary = record.summary.trim().to_string();
    let title = summary.chars().take(80).collect::<String>();
    let facet_labels: Vec<String> = record
        .facets
        .iter()
        .map(|facet| facet.label.clone())
        .filter(|label| !label.trim().is_empty())
        .collect();

    // Target `user.knowledge` explicitly: `MemoryFact` defaults to agent scope,
    // but the memory bridge only promotes user-scope tiers. The evidence id is
    // the stable upsert key, so a re-distilled host/day updates one knowledge
    // entry rather than fanning out.
    let proposed_change = json!({
        "memory": {
            "scope": "user",
            "target_tier": "knowledge",
            "operation": "upsert",
            "key": record.evidence_id.clone(),
            "value": summary.clone(),
            "facets": facet_labels,
        }
    });

    let evidence_ref = LearningEvidenceRef {
        kind: "work_evidence".to_string(),
        id: Some(record.evidence_id.clone()),
        path: None,
        uri: None,
        summary: Some(summary.clone()),
    };

    let request = CreateLearningCandidateRequest {
        principal: None,
        workspace: None,
        candidate_type: LearningCandidateType::MemoryFact,
        state: LearningCandidateState::Proposed,
        title,
        summary: summary.clone(),
        rationale: format!(
            "Distilled from ambient browsing evidence `{}` (producer `{}`).",
            record.evidence_id, record.producer
        ),
        proposed_change,
        proposed_target: Some("user.knowledge".to_string()),
        confidence: Some(record.confidence),
        source_agent_id: None,
        source_task_id: None,
        source_execution_id: None,
        source_chat_session_id: None,
        event_refs: Vec::new(),
        evidence_refs: vec![evidence_ref],
        risk_level: LearningRiskLevel::Medium,
        review_required: true,
        review_reason: Some(
            "Ambient browsing observation requires review before becoming agent-visible memory."
                .to_string(),
        ),
        review_policy: Value::Null,
        promotion_target: None,
        promotion_policy: Value::Null,
    };

    let candidate = store.create_candidate(scope.clone(), request)?;

    // Mirror the reflection pipeline's created-event for cross-surface audit.
    let _ = store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_candidate_created".to_string(),
            agent_id: None,
            task_id: None,
            execution_id: None,
            chat_session_id: None,
            summary: format!(
                "Memory candidate created from ambient evidence `{}`.",
                record.evidence_id
            ),
            evidence_refs: candidate.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate.id.clone(),
                "candidate_type": candidate.candidate_type.as_str(),
                "evidence_id": record.evidence_id.clone(),
                "producer": record.producer.clone(),
            }),
        },
    );

    // review_required = true → the bridge triages this for human review rather
    // than auto-promoting it.
    LearningMemoryBridge::new(workspace_layout.clone())
        .route_candidate(&store, scope, &candidate)
        .await?;

    Ok(candidate.id)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::evidence::EvidenceStatus;

    fn ambient_evidence(evidence_id: &str, sensitivity: &str) -> EvidenceRecord {
        EvidenceRecord {
            evidence_id: evidence_id.to_string(),
            summary: "visited a page".to_string(),
            evidence_kind: "activity".to_string(),
            observed_actions: Vec::new(),
            entity_keys: Vec::new(),
            people_keys: Vec::new(),
            artifact_refs: Vec::new(),
            source_refs: vec!["episode:test".to_string()],
            facets: Vec::new(),
            importance: 0.5,
            confidence: 0.5,
            sensitivity: sensitivity.to_string(),
            first_seen_at: "2026-06-13T00:00:00Z".to_string(),
            last_seen_at: "2026-06-13T00:00:00Z".to_string(),
            status: EvidenceStatus::Active,
            last_corrected_at: None,
            producer: "ambient_browser".to_string(),
            metadata: serde_json::Value::Null,
        }
    }

    /// Affirmatively-sensitive ambient evidence must never reach the memory
    /// candidate path — the privacy gate returns an empty candidate id and writes
    /// nothing (it short-circuits before constructing the learning store).
    #[tokio::test]
    async fn sensitive_evidence_is_not_promoted_to_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let scope = LearningScope::new("anonymous", "default");

        let candidate_id = route_user_evidence_to_memory(
            &workspace,
            &scope,
            &ambient_evidence("evd:fin-1", "financial"),
        )
        .await
        .unwrap();

        assert!(
            candidate_id.is_empty(),
            "sensitive evidence must not create a memory candidate"
        );
    }
}
