//! Proposal-only bridge from ordinary task execution to owner admission.
use super::shared::require_scope_str;
use crate::magician_v2::{
    artifact_v2::{ScopeRef, V3ReadApi},
    execution::{agent_resources::AgentResources, error::ExecutionError},
    relentless_runtime::{
        text_profile_catalog, PreparedTextDraft, ProposedTextDraft, TextDraftProposal,
        TextProposalOrigin, TEXT_PROPOSAL_REQUEST_TYPE,
    },
    user_requests::{RequestOption, UserRequest},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let fail = |e: String| ExecutionError::Step(format!("propose_relentless: {e}"));
    let principal = require_scope_str(&args, "__principal", "propose_relentless")?;
    let workspace = require_scope_str(&args, "__workspace", "propose_relentless")?;
    let agent_id = require_scope_str(&args, "__agent_id", "propose_relentless")?;
    let task_id = require_scope_str(&args, "__task_id", "propose_relentless")?;
    let execution_id = require_scope_str(&args, "__execution_id", "propose_relentless")?;
    let artifacts = resources
        .artifact_v2_service
        .as_ref()
        .ok_or_else(|| fail("task service unavailable".into()))?;
    let requests = resources
        .user_request_service
        .as_ref()
        .ok_or_else(|| fail("owner requests unavailable".into()))?;
    let router = resources
        .operation_llm_router
        .as_ref()
        .ok_or_else(|| fail("model router unavailable".into()))?;
    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
    let execution = artifacts
        .get_execution(&scope, &task_id, &execution_id)
        .await
        .map_err(|e| fail(e.to_string()))?;
    if execution.state.agent_id != agent_id
        || execution.state.task_id != task_id
        || !matches!(
            execution.state.status.as_str(),
            "running" | "waiting_for_user"
        )
    {
        return Err(fail(
            "proposal requires the active same-scope task execution and agent".into(),
        ));
    }
    match args
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("propose")
    {
        "profiles" => return text_profile_catalog(router).map_err(|e| fail(format!("{e:#}"))),
        "propose_guidance" => {
            return propose_guidance(
                resources.clone(),
                &args,
                &principal,
                &workspace,
                &agent_id,
                &task_id,
                &execution_id,
            )
            .await
        },
        "propose" => {},
        _ => return Err(fail("unsupported operation".into())),
    }
    let mut proposal: TextDraftProposal = serde_json::from_value(
        args.get("proposal")
            .cloned()
            .ok_or_else(|| fail("proposal required".into()))?,
    )
    .map_err(|e| fail(e.to_string()))?;
    if let Some(recipe) = &mut proposal.native_harness {
        if !recipe.metered_tools.is_empty() {
            let manager =
                crate::magician_v2::artifact_v2::capabilities::CapabilityWorkspaceManager::new(
                    resources.artifact_workspace.clone(),
                    std::env::current_dir().map_err(|e| fail(e.to_string()))?,
                );
            let packs = manager.load_pack_defs_for_scope(&principal, &workspace);
            for policy in &mut recipe.metered_tools {
                let pack = packs
                    .iter()
                    .find(|pack| pack.name == policy.capability)
                    .ok_or_else(|| fail("metered tool is not installed in this scope".into()))?;
                let pin =
                    crate::magician_v2::relentless_tools::capability_pin(pack, &policy.action)
                        .map_err(|e| fail(format!("{e:#}")))?;
                if policy
                    .capability_hash
                    .as_ref()
                    .is_some_and(|expected| expected != &pin)
                {
                    return Err(fail(
                        "metered tool changed; propose a successor contract".into(),
                    ));
                }
                policy.capability_hash = Some(pin);
            }
        }
    }
    let prepared =
        PreparedTextDraft::resolve(router, &proposal).map_err(|e| fail(format!("{e:#}")))?;
    let maximum = PreparedTextDraft::maximum_attempt_microusd(router, &proposal)
        .map_err(|e| fail(format!("{e:#}")))?;
    let proposal_hash = prepared.setup.proposal_hash;
    let request_id = format!(
        "relentless-proposal-{}",
        blake3::hash(
            &serde_json::to_vec(&json!([
                principal,
                workspace,
                agent_id,
                task_id,
                execution_id,
                proposal_hash
            ]))
            .map_err(|e| fail(e.to_string()))?
        )
        .to_hex()
    );
    let proposed = ProposedTextDraft {
        proposal,
        proposal_hash,
        origin: TextProposalOrigin {
            request_id: request_id.clone(),
            agent_id: agent_id.clone(),
            task_id: task_id.clone(),
            execution_id: execution_id.clone(),
        },
    };
    proposed.validate().map_err(|e| fail(format!("{e:#}")))?;
    let review_url = format!("/work?view=quality&proposal={request_id}");
    // Keep the durable notice body byte-stable for exact replay of its
    // pre-Work request id. Attention and in-app navigation resolve old links.
    let notice_review_url = format!("/relentless?proposal={request_id}");
    let request = UserRequest {
        id: request_id.clone(), request_type: TEXT_PROPOSAL_REQUEST_TYPE.into(),
        question: format!("{agent_id} proposes bounded local draft improvement: {}\n\nReview the requirements, source notes, models and budget in Relentless before starting. {notice_review_url}", proposed.proposal.goal),
        options: vec![RequestOption { id: "dismiss".into(), label: "Dismiss notice".into(), requires_input: false }],
        principal, workspace, context: serde_json::to_value(&proposed).map_err(|e| fail(e.to_string()))?,
        source: "relentless".into(), execution_id: Some(execution_id), task_id: Some(task_id),
        timeout_secs: 86_400, default_on_timeout: "dismiss".into(), created_at: 0, sensitive: None,
    };
    let request_principal = request.principal.clone();
    let request_workspace = request.workspace.clone();
    let submission = requests
        .submit_nonblocking_durable(request)
        .await
        .map_err(|e| fail(e.to_string()))?;
    let pending = requests
        .pending_request_snapshot(
            submission.request_id(),
            Some(&request_principal),
            Some(&request_workspace),
        )
        .await
        .is_some();
    Ok(
        json!({"status":if pending { "proposed" } else { "closed" }, "request_id":submission.request_id(), "review_url":review_url,
        "proposal_hash":proposed.proposal_hash, "contract":prepared.contract,
        "maximum_attempt_microusd":maximum, "budget_can_admit_attempt":maximum<=proposed.proposal.budget.cost_ceiling(),
        "admitted":false, "awaiting_owner_review":pending,
        "next_step":if pending { "Open review_url to review the exact contract and start the run. Budget validity alone does not start a run." } else { "This proposal is no longer pending. Do not report it as awaiting approval or started; inspect its outcome before proposing a successor." },
        "communication":"capture_only"}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuidanceProposal {
    guidance: magician_relentless::contract::ProceduralGuidance,
    training_run_ids: Vec<uuid::Uuid>,
}

/// Agent-authored procedural text enters the existing review queue. This
/// creates no procedure, trial, policy or productive/model operation.
async fn propose_guidance(
    resources: Arc<AgentResources>,
    args: &Value,
    principal: &str,
    workspace: &str,
    agent_id: &str,
    task_id: &str,
    execution_id: &str,
) -> Result<Value, ExecutionError> {
    use crate::magician_v2::{
        learning::*,
        relentless::store::{RunScope, RunStore},
        relentless_runtime::TextDraftSetup,
    };
    let fail = |e: String| ExecutionError::Step(format!("propose_relentless guidance: {e}"));
    let mut proposal: GuidanceProposal = serde_json::from_value(
        args.get("guidance_proposal")
            .cloned()
            .ok_or_else(|| fail("guidance_proposal required".into()))?,
    )
    .map_err(|e| fail(e.to_string()))?;
    proposal
        .guidance
        .validate()
        .map_err(|e| fail(format!("{e:#}")))?;
    proposal.training_run_ids.sort();
    if proposal.training_run_ids.is_empty()
        || proposal
            .training_run_ids
            .windows(2)
            .any(|pair| pair[0] == pair[1])
    {
        return Err(fail(
            "guidance needs distinct terminal training run IDs".into(),
        ));
    }
    let change = serde_json::to_value(&proposal.guidance).map_err(|e| fail(e.to_string()))?;
    let encoded = serde_json::to_vec(
        &json!({"guidance":change,"training_run_ids":proposal.training_run_ids}),
    )
    .map_err(|e| fail(e.to_string()))?;
    if encoded.len() > 1024 * 1024 {
        return Err(fail(
            "guidance proposal exceeds the shared 1 MiB proposal limit".into(),
        ));
    }
    let layout = resources.artifact_workspace.clone();
    let run_scope = RunScope {
        principal: principal.into(),
        workspace: workspace.into(),
    };
    let training_ids = proposal.training_run_ids.clone();
    let training = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let runs = RunStore::new(
            layout.scope_root(&run_scope.principal, &run_scope.workspace),
            run_scope,
        );
        let mut training = Vec::new();
        for id in training_ids {
            let run = runs.load(id)?;
            anyhow::ensure!(
                run.status.terminal(),
                "training feedback requires terminal runs"
            );
            let setup: TextDraftSetup =
                serde_json::from_slice(&runs.read_blob(id, &run.admission.source_hash)?)?;
            anyhow::ensure!(
                !setup
                    .guidance_origin
                    .as_ref()
                    .is_some_and(|origin| origin.is_trial()),
                "qualification evidence cannot be reused as training"
            );
            training.push((id, run.admission.task_id.clone(), run.contract.hash()?));
        }
        Ok(training)
    })
    .await
    .map_err(|e| fail(e.to_string()))?
    .map_err(|e| fail(format!("{e:#}")))?;
    let scope = ScopeRef::system_internal_unauthenticated(principal, workspace);
    let artifacts = resources
        .artifact_v2_service
        .as_ref()
        .ok_or_else(|| fail("task service unavailable".into()))?;
    for (_, source_task, _) in &training {
        let task = artifacts
            .get_task(&scope, source_task)
            .await
            .map_err(|e| fail(e.to_string()))?;
        if task.manifest.agent_id != agent_id {
            return Err(fail(
                "agent-authored guidance may cite only this agent's own scoped task feedback"
                    .into(),
            ));
        }
    }
    let id = format!(
        "lc_relentless_guidance_{}",
        blake3::hash(
            &serde_json::to_vec(&json!([
                principal,
                workspace,
                agent_id,
                task_id,
                execution_id,
                change,
                proposal.training_run_ids
            ]))
            .map_err(|e| fail(e.to_string()))?
        )
        .to_hex()
    );
    let scope = LearningScope::new(principal, workspace);
    let candidate_request = CreateLearningCandidateRequest {
        principal:None,workspace:None,candidate_type:LearningCandidateType::MemoryProcedure,
        state:LearningCandidateState::Proposed,title:proposal.guidance.title.clone(),
        summary:"Agent-proposed procedural text; owner evaluation/canary policy or exact review is required.".into(),
        rationale:"Reusable workflow proposed from terminal scoped task feedback; no held-out evidence was supplied as training.".into(),
        proposed_change:change.clone(),proposed_target:None,confidence:None,
        source_agent_id:Some(agent_id.into()),source_task_id:Some(task_id.into()),source_execution_id:Some(execution_id.into()),
        source_chat_session_id:None,event_refs:vec![],evidence_refs:training.into_iter().map(|(run_id,_,contract_hash)| LearningEvidenceRef {
            kind:"relentless_training_run".into(),id:Some(run_id.to_string()),path:None,
            uri:Some(format!("/api/magician/v2/relentless/runs/{run_id}/lessons")),
            summary:Some(format!("Terminal training feedback under frozen contract {contract_hash}; no qualification claim.")),
        }).collect(),risk_level:LearningRiskLevel::Low,review_required:true,
        review_reason:Some("Procedural text requires exact review or an explicit held-out evaluation/canary policy.".into()),
        review_policy:json!({"require_owner_review":true,"kind":"relentless_guidance_proposal"}),promotion_target:None,
        promotion_policy:json!({"automatic_promotion":false}),
    };
    let layout = resources.artifact_workspace.clone();
    let candidate_id = id.clone();
    let candidate = tokio::task::spawn_blocking(move || {
        LearningStore::new(layout).ensure_candidate_with_id(scope, candidate_request, &candidate_id)
    })
    .await
    .map_err(|e| fail(e.to_string()))?
    .map_err(|e| fail(format!("{e:#}")))?;
    if candidate.proposed_change != change
        || candidate.source_agent_id.as_deref() != Some(agent_id)
        || candidate.source_task_id.as_deref() != Some(task_id)
        || candidate.source_execution_id.as_deref() != Some(execution_id)
    {
        return Err(fail(
            "existing guidance candidate changed; propose a new candidate".into(),
        ));
    }
    let requests = resources
        .user_request_service
        .as_ref()
        .ok_or_else(|| fail("owner requests unavailable".into()))?;
    let request_id = format!("relentless-guidance-proposal-{id}");
    requests.submit_nonblocking_durable(UserRequest {
        id:request_id.clone(),request_type:"relentless.guidance_proposal".into(),
        question:format!("{agent_id} proposes reusable guidance: {}. Inspect candidate {id} and its training evidence in /relentless. An explicit evaluation/canary policy or exact owner review is required before use; dismissing this notice activates nothing.", proposal.guidance.title),
        options:vec![RequestOption {id:"dismiss".into(),label:"Dismiss notice".into(),requires_input:false}],
        principal:principal.into(),workspace:workspace.into(),context:json!({"candidate_id":id,"training_run_ids":proposal.training_run_ids,"guidance":proposal.guidance}),
        source:"relentless".into(),execution_id:Some(execution_id.into()),task_id:Some(task_id.into()),
        timeout_secs:86_400,default_on_timeout:"dismiss".into(),created_at:0,sensitive:None,
    }).await.map_err(|e| fail(e.to_string()))?;
    Ok(
        json!({"status":"proposed","candidate_id":id,"request_id":request_id,"review_url":"/work?view=quality",
        "training_run_ids":proposal.training_run_ids,"authority_activated":false}),
    )
}
