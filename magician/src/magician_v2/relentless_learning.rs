//! Terminal run evidence enters the existing learning review queue. This
//! projection spends no model budget and grants no procedure or tool authority.
use super::{
    artifact_v2::workspace::ArtifactV2Workspace,
    learning::*,
    relentless::store::{RunState, RunStatus, RunStore},
    relentless_runtime::TextDraftSetup,
};
use anyhow::{ensure, Context, Result};
use serde_json::json;

pub(crate) fn project_lessons(
    workspace: ArtifactV2Workspace,
    store: &RunStore,
    state: &RunState,
    setup: &TextDraftSetup,
) -> Result<()> {
    if setup
        .guidance_origin
        .as_ref()
        .is_some_and(|origin| origin.is_trial())
        || !state.status.terminal()
        || state.status == RunStatus::BudgetExhausted
        || (state.status == RunStatus::Accepted
            && state.handoff.is_none()
            && state.owner_approved != Some(false))
    {
        return Ok(());
    }
    let learning = LearningStore::new(workspace);
    let scope = LearningScope::new(&state.scope.principal, &state.scope.workspace);
    let id = format!("lc_relentless_{}", state.id.simple());
    match learning.read_candidate(&scope, &id) {
        Ok(candidate) => {
            let hash = candidate.proposed_change["lesson_hash"]
                .as_str()
                .context("published lessons lost their content hash")?;
            let lesson: serde_json::Value =
                serde_json::from_slice(&store.read_blob(state.id, hash)?)?;
            ensure!(
                candidate.proposed_change["kind"] == "relentless_run_lessons"
                    && candidate.proposed_change["run_id"] == state.id.to_string()
                    && lesson["version"] == "relentless-lessons-v1"
                    && lesson["run_id"] == state.id.to_string()
                    && lesson["contract_hash"] == state.contract.hash()?
                    && candidate.source_task_id.as_deref()
                        == Some(state.admission.task_id.as_str()),
                "published learning record no longer binds this run's lessons"
            );
            // Billing proofs can arrive later. The original evidence snapshot
            // stays sealed; current costs live in the run journal.
            return Ok(());
        },
        Err(error) if learning.error_is_not_found(&error) => {},
        Err(error) => return Err(error),
    }
    let lesson = run_lessons(state)?;
    let hash = store.put_blob(state.id, &serde_json::to_vec(&lesson)?)?;
    learning.ensure_candidate_with_id(scope, CreateLearningCandidateRequest {
        principal:None,workspace:None,candidate_type:LearningCandidateType::Other,
        state:LearningCandidateState::Observed,
        title:format!("Relentless lessons: {}", state.contract.definition().goal),
        summary:format!("{} attempts; {:?}. Review the sealed scores, defects and decisions before proposing reusable guidance.", state.attempts.len(), state.status),
        rationale:"Observed run evidence is training feedback, not held-out qualification or outward trust.".into(),
        proposed_change:json!({"kind":"relentless_run_lessons","run_id":state.id,"lesson_hash":hash}),
        proposed_target:None,confidence:None,
        source_agent_id:setup.native_harness.as_ref().map(|recipe| recipe.agent_id.clone()),
        source_task_id:Some(state.admission.task_id.clone()),source_execution_id:setup.runtime_execution_id.clone(),
        source_chat_session_id:None,event_refs:vec![],
        evidence_refs:vec![LearningEvidenceRef {kind:"relentless_sealed_lessons".into(),id:Some(hash),
            path:None,uri:Some(format!("/api/magician/v2/relentless/runs/{}/lessons",state.id)),
            summary:Some("Frozen review configuration and exact attempt evidence; captured or local work supplies no confirmed delivery.".into())}],
        risk_level:LearningRiskLevel::Low,review_required:true,
        review_reason:Some("Procedure promotion requires independent qualification under an owner-approved policy or exact owner review.".into()),
        review_policy:json!({"require_owner_review":true}),promotion_target:None,
        promotion_policy:json!({"automatic_promotion":false}),
    }, &id)?;
    Ok(())
}

pub(crate) fn run_lessons(state: &RunState) -> Result<serde_json::Value> {
    let attempts: Vec<_> = state
        .attempts
        .iter()
        .map(|attempt| {
            json!({
                "number":attempt.number,"artifact":attempt.artifact,"verdicts":attempt.verdicts,
                "review":attempt.review,"decided":attempt.decided,
            })
        })
        .collect();
    Ok(json!({"version":"relentless-lessons-v1","run_id":state.id,
        "contract_hash":state.contract.hash()?,"source_epoch":state.source_epoch,
        "status":state.status,"reason":state.reason,"attempts":attempts,
        "decisions":state.decisions,"retained":state.retained,"spend":state.budget.spend()?,
        "budget_resets":state.budget_resets,
        "spend_is_publication_snapshot":true,
        "routing":state.routing,"owner_approved":state.owner_approved,"handoff":state.handoff,
        "outward_delivery_confirmed":false,
    }))
}
