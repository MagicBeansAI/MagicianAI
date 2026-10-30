//! Shared task-backed admission, recovery and output lifecycle for Relentless.
//! HTTP and compiled capabilities reuse this service; proposals alone grant no
//! budget, model access or delivery authority.
use super::{
    artifact_v2::{
        models::ExecutionOutcomeSnapshot,
        service::{WriteUserOutputBody, WriteUserOutputDirectInput},
        workspace::{scope_hosts_user_subsystems, ArtifactV2Workspace},
        ArtifactV2Service, ScopeRef, V3ReadApi,
    },
    dispatch_glue::CancelBridge,
    orchestrator::MagicianV2Orchestrator,
    query_analysis::operation_llm_router::OperationLlmRouter,
    relentless::{
        controller::Controller,
        report::RunReport,
        routing::ReviewRoute,
        store::{RunEvent, RunScope, RunState, RunStatus, RunStore},
    },
    relentless_runtime::{
        ExistingTaskAdmission, PreparedTextDraft, ProposedTextDraft, TextAdapters,
        TextDraftProposal, TextDraftSetup, TEXT_PROPOSAL_REQUEST_TYPE,
    },
    storage::WaitingState,
    task_run_factory::{
        create_existing_task_direct_root_run_with_output_owner,
        create_task_backed_direct_root_run_with_output_owner, task_policy_context_hash,
    },
};
use anyhow::{Context, Result};
use chrono::Utc;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// The exact-review ask's answers. Either one is bound to the run and the
/// artifact hash in the ask's id, so it decides only the draft that was asked.
const REVIEW_APPROVE: &str = "approve";
const REVIEW_REJECT: &str = "reject";
const REVIEW_ASK_MARKER: &str = ":relentless-review:";

/// `{execution}:relentless-review:{run}:{artifact_hash}` → run and hash.
fn review_ask_target(correlation_id: &str) -> Option<(Uuid, &str)> {
    let (_, rest) = correlation_id.split_once(REVIEW_ASK_MARKER)?;
    let (run, hash) = rest.split_once(':')?;
    if hash.is_empty() {
        return None;
    }
    Some((Uuid::parse_str(run).ok()?, hash))
}

#[derive(Clone, Default)]
struct Control {
    cancel: CancellationToken,
    wake: Arc<tokio::sync::Notify>,
    wake_revision: Arc<AtomicU64>,
}
impl Control {
    fn notify(&self) {
        self.wake_revision.fetch_add(1, Ordering::AcqRel);
        self.wake.notify_one();
    }
    fn revision(&self) -> u64 {
        self.wake_revision.load(Ordering::Acquire)
    }
}
type Key = (String, String, Uuid);

#[path = "relentless_guidance.rs"]
mod guidance;

#[path = "relentless_budget.rs"]
mod budget_continuation;

#[derive(serde::Serialize)]
pub struct OwnerTaskContext {
    pub task_id: String,
    pub context_hash: String,
    pub title: String,
    pub goal: String,
    pub agent_id: String,
    pub ui_thread_id: String,
    pub can_start: bool,
    pub reason: Option<String>,
    pub pending_run_id: Option<Uuid>,
    pub pending_proposal_hash: Option<String>,
    pub policy: Option<super::relentless_task_policy::TaskRelentlessPolicy>,
    pub policy_runs: u32,
}

#[cfg(test)]
#[path = "relentless_service_tests.rs"]
mod tests;

#[derive(Clone)]
pub struct RelentlessService {
    workspace: ArtifactV2Workspace,
    router: Arc<OperationLlmRouter>,
    artifacts: Arc<ArtifactV2Service>,
    orchestrator: Arc<MagicianV2Orchestrator>,
    controls: Arc<Mutex<BTreeMap<Key, Control>>>,
    guidance_controls: Arc<Mutex<BTreeMap<Key, Control>>>,
    admission_lock: Arc<tokio::sync::Mutex<()>>,
    cancel_bridge: Option<Arc<CancelBridge>>,
    user_requests: Option<Arc<super::user_requests::UserRequestService>>,
}

async fn io<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .context("Relentless storage worker")?
}

impl RelentlessService {
    /// Preview/admission pin the installed typed skill under this owner's
    /// scope. Unspecified pins are resolved here; changed pins fail closed.
    pub fn prepare_owner_draft(
        &self,
        scope: &RunScope,
        proposal: &TextDraftProposal,
    ) -> Result<PreparedTextDraft> {
        let mut resolved = proposal.clone();
        if let Some(recipe) = &mut resolved.native_harness {
            if !recipe.metered_tools.is_empty() {
                let executors = self.orchestrator.build_action_executors();
                let resolver = executors
                    .capability_scope_resolver
                    .as_ref()
                    .context("metered tools require the scoped skill catalog")?;
                let snapshot =
                    resolver.capability_snapshot_for_scope(&scope.principal, &scope.workspace)?;
                for policy in &mut recipe.metered_tools {
                    let pack = snapshot
                        .registry
                        .get_pack_definition(&policy.capability)
                        .context("metered tool is not installed in this scope")?;
                    let pin = super::relentless_tools::capability_pin(&pack, &policy.action)?;
                    anyhow::ensure!(
                        policy
                            .capability_hash
                            .as_ref()
                            .is_none_or(|expected| expected == &pin),
                        "metered tool pin changed; preview a successor contract"
                    );
                    policy.capability_hash = Some(pin);
                }
            }
        }
        let mut prepared = PreparedTextDraft::resolve(&self.router, &resolved)?;
        // Admission provenance retains the original owner proposal; resolved
        // runtime pins belong to the frozen recipe and contract instead.
        prepared.setup.proposal_hash = blake3::hash(&serde_json::to_vec(proposal)?)
            .to_hex()
            .to_string();
        Ok(prepared)
    }

    pub(crate) fn prepare_task_policy_launch(
        &self,
        scope: &ScopeRef,
        task: &super::artifact_v2::models::TaskRecord,
        execution_id: &str,
    ) -> Result<super::relentless_task_policy::TaskPolicyLaunch> {
        let policy = task
            .manifest
            .relentless
            .as_ref()
            .context("task has no Relentless policy")?;
        policy.validate()?;
        let context_hash = task_policy_context_hash(&task.manifest).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            scope_hosts_user_subsystems(scope.principal(), scope.workspace())
                && policy.approved_by == scope.principal()
                && policy.expires_at > Utc::now()
                && policy.task_context_hash == context_hash
                && task.manifest.approved
                && task.manifest.monitor_spec.is_none()
                && task.state.synthesis_pending_executions.is_empty()
                && task
                    .state
                    .relentless_policy_runs
                    .get(&policy.id.to_string())
                    .copied()
                    .unwrap_or(0)
                    < policy.max_runs
                && policy.proposal.goal == task.manifest.description
                && policy
                    .proposal
                    .native_harness
                    .as_ref()
                    .map(|recipe| recipe.agent_id.as_str())
                    == Some(task.manifest.agent_id.as_str()),
            "task policy expired, exhausted or its reviewed task context changed"
        );
        let mut prepared = self.prepare_owner_draft(
            &RunScope {
                principal: scope.principal().into(),
                workspace: scope.workspace().into(),
            },
            &policy.proposal,
        )?;
        anyhow::ensure!(
            prepared.contract.hash()? == policy.contract_hash,
            "task policy model, adapter or source pins changed; review a successor policy"
        );
        let scope = RunScope {
            principal: scope.principal().into(),
            workspace: scope.workspace().into(),
        };
        let run_id =
            super::relentless_task_policy::run_id(&scope, &task.manifest.task_id, execution_id)?;
        if policy.capture_handoff.is_none() {
            self.apply_qualified_guidance(
                &scope,
                &task.manifest.task_id,
                &context_hash,
                run_id,
                &policy.proposal,
                &mut prepared,
            )?;
        }
        prepared.setup.task_policy_origin = Some(ExistingTaskAdmission {
            task_id: task.manifest.task_id.clone(),
            context_hash,
        });
        if let Some(target) = &policy.capture_handoff {
            self.validate_capture_handoff(
                &scope,
                target,
                &task.manifest.task_id,
                &policy.task_context_hash,
                &policy.contract_hash,
            )?;
            prepared.setup.capture_handoff = Some(target.clone());
        }
        prepared.setup.task_execution_id = Some(execution_id.to_owned());
        prepared.setup.runtime_execution_id = Some(execution_id.to_owned());
        Ok(super::relentless_task_policy::TaskPolicyLaunch {
            scope,
            task_id: task.manifest.task_id.clone(),
            execution_id: execution_id.into(),
            policy_id: policy.id,
            policy_hash: policy.hash()?,
            run_id,
            prepared,
        })
    }

    pub(crate) async fn launch_task_policy(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> Result<bool> {
        super::runtime_writer_floor::enforce_runtime_writer_floor(
            self.artifacts.workspace().base_root(),
            false,
        )?;
        let Some(launch) = self
            .artifacts
            .read_relentless_launch(scope, task_id, execution_id)
            .await?
        else {
            return Ok(false);
        };
        anyhow::ensure!(
            super::relentless_task_policy::run_id(&launch.scope, task_id, execution_id)?
                == launch.run_id
                && launch.prepared.setup.task_execution_id.as_deref() == Some(execution_id)
                && launch.prepared.setup.runtime_execution_id.as_deref() == Some(execution_id)
                && launch
                    .prepared
                    .setup
                    .task_policy_origin
                    .as_ref()
                    .is_some_and(|origin| origin.task_id == task_id),
            "task policy launch identity changed"
        );
        // The reducer's committed root, not a lone sidecar, proves admission.
        let execution = self
            .artifacts
            .get_execution(scope, task_id, execution_id)
            .await?;
        let task = self.artifacts.get_task(scope, task_id).await?;
        anyhow::ensure!(
            execution.state.parent_execution_id.is_none()
                && execution.state.root_execution_id.as_deref() == Some(execution_id)
                && task
                    .state
                    .relentless_policy_runs
                    .get(&launch.policy_id.to_string())
                    .copied()
                    .unwrap_or(0)
                    > 0,
            "task policy root acceptance is not committed"
        );
        let _admission = self.admission_lock.lock().await;
        let store = self.store(&launch.scope);
        let s = store.clone();
        let id = launch.run_id;
        if !io(move || s.has_admission(id)).await? {
            anyhow::ensure!(
                execution.state.status == "running",
                "unadmitted policy root is no longer running"
            );
            anyhow::ensure!(
                task.state.latest_root_execution_id.as_deref() == Some(execution_id),
                "a newer task root superseded the interrupted policy launch"
            );
            self.artifacts
                .ensure_external_root_runtime(scope, task_id, execution_id, &launch.owner_id())
                .await?;
            let s = store.clone();
            let owner = launch.scope.principal.clone();
            let task_id = task_id.to_owned();
            io(move || launch.prepared.admit(&s, id, owner, task_id)).await?;
        }
        if matches!(task.state.status.as_str(), "cancelled" | "canceled") {
            let s = store.clone();
            io(move || {
                let state = s.load(id)?;
                if !state.status.terminal() {
                    s.commit(id, state.generation + 1, RunEvent::Cancelled, Utc::now())?;
                }
                Ok(())
            })
            .await?;
        }
        self.launch(
            RunScope {
                principal: scope.principal().into(),
                workspace: scope.workspace().into(),
            },
            id,
        );
        Ok(true)
    }

    pub async fn configure_owner_task_policy(
        &self,
        scope: RunScope,
        task_id: &str,
        expected_context_hash: &str,
        policy_id: Uuid,
        expected_policy_id: Option<Uuid>,
        proposal: Option<TextDraftProposal>,
        expected_contract_hash: &str,
        max_runs: u32,
        expires_at: chrono::DateTime<Utc>,
    ) -> Result<super::artifact_v2::models::TaskRecord> {
        self.configure_owner_task_policy_with_capture(
            scope,
            task_id,
            expected_context_hash,
            policy_id,
            expected_policy_id,
            proposal,
            expected_contract_hash,
            max_runs,
            expires_at,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn configure_owner_task_policy_with_capture(
        &self,
        scope: RunScope,
        task_id: &str,
        expected_context_hash: &str,
        policy_id: Uuid,
        expected_policy_id: Option<Uuid>,
        proposal: Option<TextDraftProposal>,
        expected_contract_hash: &str,
        max_runs: u32,
        expires_at: chrono::DateTime<Utc>,
        capture_handoff: Option<super::approval_envelopes::campaign::CaptureHandoffTarget>,
    ) -> Result<super::artifact_v2::models::TaskRecord> {
        anyhow::ensure!(
            scope_hosts_user_subsystems(&scope.principal, &scope.workspace),
            "owner scope cannot host Relentless"
        );
        let policy = proposal
            .map(|mut proposal| -> Result<_> {
                let prepared = self.prepare_owner_draft(&scope, &proposal)?;
                if proposal
                    .native_harness
                    .as_ref()
                    .is_some_and(|recipe| !recipe.metered_tools.is_empty())
                {
                    proposal.native_harness = prepared.setup.native_harness.clone();
                }
                anyhow::ensure!(
                    prepared.contract.hash()? == expected_contract_hash && expires_at > Utc::now(),
                    "reviewed contract changed or policy expired"
                );
                let policy = super::relentless_task_policy::TaskRelentlessPolicy {
                    id: policy_id,
                    proposal,
                    contract_hash: expected_contract_hash.into(),
                    task_context_hash: expected_context_hash.into(),
                    capture_handoff,
                    max_runs,
                    expires_at,
                    approved_by: scope.principal.clone(),
                    approved_at: Utc::now(),
                };
                policy.validate()?;
                if let Some(target) = &policy.capture_handoff {
                    self.validate_capture_handoff(
                        &scope,
                        target,
                        task_id,
                        expected_context_hash,
                        expected_contract_hash,
                    )?;
                }
                Ok(policy)
            })
            .transpose()?;
        self.artifacts
            .set_owner_relentless_policy(
                &ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace),
                task_id,
                expected_context_hash,
                policy,
                expected_policy_id,
            )
            .await
            .map_err(anyhow::Error::from)
    }

    pub fn new(
        workspace: ArtifactV2Workspace,
        router: Arc<OperationLlmRouter>,
        artifacts: Arc<ArtifactV2Service>,
        orchestrator: Arc<MagicianV2Orchestrator>,
        cancel_bridge: Option<Arc<CancelBridge>>,
    ) -> Self {
        Self {
            workspace,
            router,
            artifacts,
            orchestrator,
            controls: Default::default(),
            guidance_controls: Default::default(),
            admission_lock: Default::default(),
            cancel_bridge,
            user_requests: None,
        }
    }

    pub fn store(&self, scope: &RunScope) -> Arc<RunStore> {
        Arc::new(RunStore::new(
            self.workspace
                .scope_root(&scope.principal, &scope.workspace),
            scope.clone(),
        ))
    }

    fn validate_capture_handoff(
        &self,
        scope: &RunScope,
        target: &super::approval_envelopes::campaign::CaptureHandoffTarget,
        task_id: &str,
        context_hash: &str,
        contract_hash: &str,
    ) -> Result<()> {
        let campaigns =
            super::approval_envelopes::campaign::CampaignStore::new(self.workspace.clone());
        let campaign_scope =
            super::approval_envelopes::EnvelopeStoreScope::new(&scope.principal, &scope.workspace);
        campaigns.validate_handoff_target(
            &campaign_scope,
            target,
            task_id,
            context_hash,
            contract_hash,
        )
    }

    pub async fn owner_campaigns(
        &self,
        scope: &RunScope,
        page: &runtime_core::storage::PaginationParams,
        query: &str,
    ) -> Result<serde_json::Value> {
        let store = super::approval_envelopes::campaign::CampaignStore::new(self.workspace.clone());
        let campaign_scope =
            super::approval_envelopes::EnvelopeStoreScope::new(&scope.principal, &scope.workspace);
        let mut campaigns = io(move || store.list(&campaign_scope)).await?;
        campaigns.retain(|c| {
            catalog_matches(query, &[&c.campaign.id.to_string(), &c.campaign.spec.name])
        });
        campaigns.sort_by(|a, b| {
            a.campaign
                .spec
                .name
                .to_lowercase()
                .cmp(&b.campaign.spec.name.to_lowercase())
                .then_with(|| a.campaign.id.cmp(&b.campaign.id))
        });
        let pagination = catalog_pagination(campaigns.len(), page);
        let campaigns = campaigns.into_iter().skip(page.offset).take(page.limit);
        let task_scope =
            ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
        let mut views = Vec::new();
        for mut campaign in campaigns {
            let mut targets = Vec::new();
            for target in &campaign.campaign.spec.targets {
                let task = self.artifacts.get_task(&task_scope, &target.task_id).await;
                targets.push(match task {
                    Ok(task) => serde_json::json!({"task_id":target.task_id,"identity_id":target.identity_id,
                        "title":task.manifest.title,"due_date":task.manifest.due_date,"status":task.state.status,
                        "context_changed":task_policy_context_hash(&task.manifest).map_err(anyhow::Error::msg)?!=target.task_context_hash}),
                    Err(error) => serde_json::json!({"task_id":target.task_id,"identity_id":target.identity_id,"unavailable":error.to_string()}),
                });
            }
            let capture_count = campaign.captures.len();
            // Captures have their own bounded endpoint; policy pages never carry artifact history.
            campaign.captures.clear();
            views.push(serde_json::json!({"state":campaign,"targets":targets,"capture_count":capture_count}));
        }
        let counterparties = super::counterparties::CounterpartyStore::new(self.workspace.clone());
        let identity_scope =
            super::counterparties::CounterpartyScope::new(&scope.principal, &scope.workspace);
        let identities=io(move || {
            let mut identities=Vec::new();
            for counterparty in counterparties.list(&identity_scope)? {
                for identity in counterparties.identities_for(&identity_scope,&counterparty.counterparty_id)? {
                    identities.push(serde_json::json!({"id":identity.identity_id,"recipient":identity.normalised,"counterparty":counterparty.display_name,"verified":identity.is_verified()}));
                }
            }
            Ok(identities)
        }).await?;
        Ok(
            serde_json::json!({"campaigns":views,"pagination":pagination,"identities":identities,"communication":"capture_only"}),
        )
    }

    pub async fn owner_campaign_captures(
        &self,
        scope: &RunScope,
        id: Uuid,
        page: &runtime_core::storage::PaginationParams,
        status: &str,
    ) -> Result<serde_json::Value> {
        use super::approval_envelopes::campaign::{CampaignStore, CaptureStatus};
        anyhow::ensure!(
            ["all", "awaiting_review", "captured", "rejected"].contains(&status),
            "unknown capture status"
        );
        let store = CampaignStore::new(self.workspace.clone());
        let capture_scope =
            super::approval_envelopes::EnvelopeStoreScope::new(&scope.principal, &scope.workspace);
        let state = io(move || store.load(&capture_scope, id))
            .await?
            .context("campaign missing")?;
        let mut captures: Vec<_> = state
            .captures
            .into_values()
            .filter(|c| match status {
                "awaiting_review" => c.status == CaptureStatus::AwaitingReview,
                "captured" => c.status == CaptureStatus::Captured,
                "rejected" => c.status == CaptureStatus::Rejected,
                _ => true,
            })
            .collect();
        captures.sort_by(|a, b| {
            b.presented_at
                .cmp(&a.presented_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let pagination = catalog_pagination(captures.len(), page);
        let captures: Vec<_> = captures
            .into_iter()
            .skip(page.offset)
            .take(page.limit)
            .map(|mut capture| {
                // The immutable binding hash still identifies the complete stored binding.
                // Exact bytes are fetched from the run, never from a queue row.
                capture.binding.artifact_files.clear();
                capture
            })
            .collect();
        Ok(serde_json::json!({"captures":captures,"pagination":pagination}))
    }

    pub async fn configure_owner_campaign(
        &self,
        scope: RunScope,
        id: Uuid,
        expected_version: Option<String>,
        spec: Option<super::approval_envelopes::campaign::CampaignSpec>,
    ) -> Result<super::approval_envelopes::campaign::CampaignState> {
        let store = super::approval_envelopes::campaign::CampaignStore::new(self.workspace.clone());
        let campaign_scope =
            super::approval_envelopes::EnvelopeStoreScope::new(&scope.principal, &scope.workspace);
        let Some(spec) = spec else {
            return io(move || {
                store.revoke(
                    &campaign_scope,
                    id,
                    expected_version
                        .as_deref()
                        .context("revocation needs the exact approved campaign version")?,
                )
            })
            .await;
        };
        spec.validate()?;
        let task_scope =
            ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
        for target in &spec.targets {
            let task = self
                .artifacts
                .get_task(&task_scope, &target.task_id)
                .await?;
            anyhow::ensure!(
                task_policy_context_hash(&task.manifest).map_err(anyhow::Error::msg)?
                    == target.task_context_hash
                    && spec
                        .goal_id
                        .as_ref()
                        .is_none_or(|id| task.manifest.goal_id.as_ref() == Some(id)),
                "campaign target task context changed"
            );
        }
        let counterparties = super::counterparties::CounterpartyStore::new(self.workspace.clone());
        let identity_scope =
            super::counterparties::CounterpartyScope::new(&scope.principal, &scope.workspace);
        io(move || {
            for target in &spec.targets {
                // Local capture may retain an unverified research target. The
                // existing identity record is still mandatory; this does not
                // assert that it is safe to contact or disclose anything.
                let identity = counterparties
                    .load_identity(&identity_scope, &target.identity_id)?
                    .context("campaign identity is absent from the scoped counterparty register")?;
                anyhow::ensure!(
                    target
                        .recipient
                        .as_ref()
                        .is_none_or(|recipient| recipient == &identity.normalised),
                    "campaign recipient differs from the scoped identity record"
                );
            }
            store.activate(&campaign_scope, id, expected_version.as_deref(), spec)
        })
        .await
    }

    async fn project_campaign_review_notice(
        &self,
        scope: &RunScope,
        act: &super::approval_envelopes::campaign::CapturedAct,
    ) -> Result<()> {
        use super::{
            approval_envelopes::campaign::CaptureStatus,
            user_requests::{RequestOption, UserRequest, UserResponse},
        };
        let Some(requests) = &self.user_requests else {
            return Ok(());
        };
        let request_id = format!("relentless-review-{}", act.id);
        if act.status == CaptureStatus::AwaitingReview {
            requests.submit_nonblocking_durable(UserRequest {
                id:request_id, request_type:"relentless.capture_review".into(),
                question:format!("A local campaign capture for {} needs exact review. Inspect the bound artifact in /relentless?run={}&campaign={} before approving or rejecting it. Dismissing this notice does not approve it.", act.binding.recipient, act.binding.run_id, act.binding.campaign_id),
                options:vec![RequestOption { id:"dismiss".into(), label:"Dismiss notice".into(), requires_input:false }],
                principal:scope.principal.clone(),workspace:scope.workspace.clone(),
                context:serde_json::json!({"campaign_id":act.binding.campaign_id,"capture_id":act.id,"binding_hash":act.binding_hash,
                    "run_id":act.binding.run_id,"artifact_hash":act.binding.artifact_hash,"reason":act.reason}),
                source:"relentless".into(), execution_id:None,task_id:Some(act.binding.task_id.clone()),
                timeout_secs:86_400,default_on_timeout:"dismiss".into(),created_at:0,sensitive:None,
            }).await?;
        } else if act.owner_reviewed_by.is_some() {
            if requests
                .pending_request_snapshot(
                    &request_id,
                    Some(&scope.principal),
                    Some(&scope.workspace),
                )
                .await
                .is_none()
            {
                return Ok(());
            }
            let result = requests
                .respond_scoped(
                    UserResponse {
                        request_id,
                        decision: "dismiss".into(),
                        input: None,
                        channel: "relentless_review".into(),
                        sensitive: vec![],
                    },
                    Some(&scope.principal),
                    Some(&scope.workspace),
                )
                .await;
            anyhow::ensure!(
                !matches!(
                    result,
                    super::user_requests::ScopedResponseResult::PersistenceUnavailable
                ),
                "capture decision committed; review notice resolution remains pending"
            );
        }
        Ok(())
    }

    pub async fn capture_owner_campaign_run(
        &self,
        scope: RunScope,
        campaign_id: Uuid,
        expected_version: String,
        run_id: Uuid,
        artifact_hash: String,
        identity_id: String,
    ) -> Result<super::approval_envelopes::campaign::CapturedAct> {
        use super::approval_envelopes::campaign::*;
        let campaigns = CampaignStore::new(self.workspace.clone());
        let campaign_scope =
            super::approval_envelopes::EnvelopeStoreScope::new(&scope.principal, &scope.workspace);
        let reader = campaigns.clone();
        let read_scope = campaign_scope.clone();
        let campaign = io(move || {
            reader
                .load(&read_scope, campaign_id)?
                .context("campaign missing")
        })
        .await?;
        anyhow::ensure!(
            campaign.campaign.version_hash == expected_version,
            "campaign changed before capture binding"
        );
        let run_store = self.store(&scope);
        let reader = run_store.clone();
        let run = io(move || reader.load(run_id)).await?;
        let handoff = run
            .handoff
            .as_ref()
            .context("run has no completed capture handoff")?;
        anyhow::ensure!(
            run.status == RunStatus::Accepted
                && handoff.kind == super::relentless::routing::HandoffKind::Capture
                && handoff.artifact_hash == artifact_hash
                && run.owner_approved != Some(false),
            "campaign requires the exact accepted capture artifact"
        );
        let artifact = run
            .attempts
            .iter()
            .filter_map(|attempt| attempt.artifact.as_ref())
            .find(|artifact| artifact.hash == artifact_hash)
            .context("accepted artifact missing from its run")?
            .clone();
        let target = campaign
            .campaign
            .spec
            .targets
            .iter()
            .find(|target| {
                target.task_id == run.admission.task_id && target.identity_id == identity_id
            })
            .context("run/identity is outside the approved campaign")?;
        let task_scope =
            ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
        let task = self
            .artifacts
            .get_task(&task_scope, &target.task_id)
            .await?;
        let context_hash = task_policy_context_hash(&task.manifest).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            context_hash == target.task_context_hash
                && campaign.campaign.spec.goal_id.as_ref().is_none_or(|id| task
                    .manifest
                    .goal_id
                    .as_ref()
                    == Some(id)),
            "campaign target task context changed"
        );
        let counterparties = super::counterparties::CounterpartyStore::new(self.workspace.clone());
        let identity_scope =
            super::counterparties::CounterpartyScope::new(&scope.principal, &scope.workspace);
        let notice_scope = scope.clone();
        let captured = io(move || {
            let identity = counterparties
                .load_identity(&identity_scope, &identity_id)?
                .context("campaign identity no longer exists")?;
            // Loading the journal verified the sealed manifest and all its
            // content blobs. The manifest digest is not itself a content blob.
            let files = artifact
                .files
                .iter()
                .map(|(file, content)| Ok((file.clone(), run_store.read_blob(run_id, content)?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            let spec = &campaign.campaign.spec;
            let (content_validation, content_review_reason) =
                if let Some(template) = &spec.content_template {
                    let sources = run
                        .evaluation_contract()?
                        .definition()
                        .sources
                        .iter()
                        .map(|source| {
                            Ok((
                                source.id.clone(),
                                run_store.read_blob(run_id, &source.content_hash)?,
                            ))
                        })
                        .collect::<Result<BTreeMap<_, _>>>()?;
                    match template.validate_artifact(&files, &sources) {
                        Ok(proof) => (Some(proof), None),
                        Err(error) => (
                            None,
                            Some(format!("Exact content review required: {error:#}")),
                        ),
                    }
                } else {
                    (
                        None,
                        Some("No approved bounded template; exact content review required".into()),
                    )
                };
            campaigns.capture(
                &campaign_scope,
                CaptureBinding {
                    scope: magicllm::LlmScope::new(&scope.principal, &scope.workspace),
                    campaign_id,
                    version_hash: expected_version,
                    run_id,
                    task_id: run.admission.task_id.clone(),
                    task_context_hash: context_hash,
                    identity_id,
                    recipient: identity.normalised,
                    sender_identity: spec.sender_identity.clone(),
                    channel: spec.channel.clone(),
                    message_kind: spec.message_kind.clone(),
                    content_contract_hash: spec.content_contract_hash.clone(),
                    contract_hash: run.contract.hash()?,
                    artifact_hash,
                    artifact_files: artifact.files,
                    content_validation,
                    content_review_reason,
                },
                run.owner_approved == Some(true),
            )
        })
        .await?;
        self.project_campaign_review_notice(&notice_scope, &captured)
            .await?;
        Ok(captured)
    }

    pub async fn review_owner_campaign_capture(
        &self,
        scope: RunScope,
        campaign_id: Uuid,
        id: String,
        binding_hash: String,
        approved: bool,
    ) -> Result<super::approval_envelopes::campaign::CapturedAct> {
        let store = super::approval_envelopes::campaign::CampaignStore::new(self.workspace.clone());
        let campaign_scope =
            super::approval_envelopes::EnvelopeStoreScope::new(&scope.principal, &scope.workspace);
        if approved {
            let reader = store.clone();
            let read_scope = campaign_scope.clone();
            let capture_id = id.clone();
            let capture = io(move || {
                reader
                    .load(&read_scope, campaign_id)?
                    .context("campaign missing")?
                    .captures
                    .get(&capture_id)
                    .cloned()
                    .context("capture missing")
            })
            .await?;
            let checked = self
                .capture_owner_campaign_run(
                    scope.clone(),
                    campaign_id,
                    capture.binding.version_hash.clone(),
                    capture.binding.run_id,
                    capture.binding.artifact_hash.clone(),
                    capture.binding.identity_id.clone(),
                )
                .await?;
            anyhow::ensure!(
                checked.binding_hash == binding_hash,
                "capture content changed before exact owner review"
            );
        }
        let service = self.clone();
        let wake_scope = scope.clone();
        let act = io(move || {
            let act = store.review(&campaign_scope, campaign_id, &id, &binding_hash, approved)?;
            // A durable capture decision owns its model-free projection wake,
            // even if the response or notice projection is interrupted.
            service.launch(wake_scope, act.binding.run_id);
            Ok(act)
        })
        .await?;
        self.project_campaign_review_notice(&scope, &act).await?;
        Ok(act)
    }

    async fn scoped_execution_output_owner(
        &self,
        scope: &RunScope,
        execution_id: &str,
    ) -> Result<
        Option<(
            String,
            super::artifact_v2::models::ExternalExecutionOutputOwner,
        )>,
    > {
        let execution = self
            .orchestrator
            .get_execution(execution_id)
            .await
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            execution.principal == scope.principal && execution.workspace == scope.workspace,
            "owner answer is outside the execution scope"
        );
        let Some(task_id) = execution.task_id else {
            return Ok(None);
        };
        let artifact_scope =
            ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
        Ok(self
            .artifacts
            .external_execution_output_owner(&artifact_scope, &task_id, execution_id)
            .await?
            .map(|owner| (task_id, owner)))
    }

    /// Read the existing ownership record without claiming a pause, recording
    /// an answer or dispatching work. Ordinary tasks keep their own HITL route.
    pub async fn owns_execution(&self, scope: &RunScope, execution_id: &str) -> Result<bool> {
        Ok(self
            .scoped_execution_output_owner(scope, execution_id)
            .await?
            .is_some_and(|(_, owner)| owner.owner_id.starts_with("relentless:")))
    }

    /// Authenticated owner boundary for an existing native question. Read-only
    /// controller identification happens before ordinary pause consumption.
    /// The existing lifecycle exclusion serializes answer custody and receipts.
    pub async fn respond_owner_input(
        &self,
        scope: RunScope,
        execution_id: &str,
        attention_id: &str,
        value: super::execution::agentic::UserInputValue,
    ) -> Result<Option<RunState>> {
        // A disconnected HTTP request must not abandon receipt publication or
        // the wake after it commits. Dropping the join handle leaves settlement
        // running, just as ordinary resume settlement does.
        let api = self.clone();
        let execution_id = execution_id.to_owned();
        let attention_id = attention_id.to_owned();
        tokio::spawn(async move {
            api.respond_owner_input_inner(scope, &execution_id, &attention_id, value)
                .await
        })
        .await
        .context("Relentless owner-answer settlement task")?
    }

    async fn respond_owner_input_inner(
        &self,
        scope: RunScope,
        execution_id: &str,
        attention_id: &str,
        value: super::execution::agentic::UserInputValue,
    ) -> Result<Option<RunState>> {
        use super::{
            analytics::llm_trace_content::scoped_content_fingerprint,
            execution::agentic::{vault_durable_pause_response, UserInputResponse},
            relentless_actor::{HarnessActor, RecordedOwnerInput},
        };
        anyhow::ensure!(
            scope_hosts_user_subsystems(&scope.principal, &scope.workspace),
            "owner scope cannot host Relentless"
        );
        let Some((task_id, owner)) = self
            .scoped_execution_output_owner(&scope, execution_id)
            .await?
        else {
            return Ok(None);
        };
        let Some(named) = owner.owner_id.strip_prefix("relentless:") else {
            return Ok(None);
        };
        let (run, proposal_hash) = named
            .split_once(':')
            .context("invalid Relentless output owner")?;
        let id = Uuid::parse_str(run)?;
        let _admission = self.admission_lock.lock().await;
        let pause_store = self
            .orchestrator
            .full_pause_store_handle()
            .context("owner question requires durable pause storage")?;
        let _lifecycle = pause_store
            .acquire_stateless_execution_lifecycle_exclusion_scoped(
                &scope.principal,
                &scope.workspace,
                execution_id,
            )
            .await
            .map_err(anyhow::Error::msg)?;
        let store = self.store(&scope);
        let s = store.clone();
        let (state, setup) = io(move || {
            let state = s.load(id)?;
            let setup: TextDraftSetup =
                serde_json::from_slice(&s.read_blob(id, &state.admission.source_hash)?)?;
            Ok((state, setup))
        })
        .await?;
        anyhow::ensure!(
            state.admission.task_id == task_id
                && setup.runtime_execution_id.as_deref() == Some(execution_id)
                && setup.admission_key() == proposal_hash
                && setup.native_harness.is_some(),
            "owner answer lost its native task binding"
        );
        let attention = attention_id.to_owned();
        let fingerprint_for =
            |input_type: &super::execution::agentic::UserInputType| -> Result<String> {
                let bytes = zeroize::Zeroizing::new(serde_json::to_vec(&(
                    "relentless.owner-input.v1",
                    id,
                    execution_id,
                    attention_id,
                    input_type,
                    &value,
                ))?);
                scoped_content_fingerprint(
                    &self.workspace,
                    &magicllm::LlmScope::new(&scope.principal, &scope.workspace),
                    &bytes,
                )
                .map_err(anyhow::Error::msg)
            };
        if let Some(hash) = state.owner_inputs.get(attention_id) {
            let s = store.clone();
            let hash = hash.clone();
            let recorded: RecordedOwnerInput =
                io(move || Ok(serde_json::from_slice(&s.read_blob(id, &hash)?)?)).await?;
            anyhow::ensure!(
                value.matches_input_type(&recorded.response.input_type)
                    && fingerprint_for(&recorded.response.input_type)? == recorded.fingerprint,
                "answered request cannot receive conflicting evidence"
            );
            if let Err(error) =
                self.emit_owner_input_resolved(&state, execution_id, attention_id, &recorded)
            {
                tracing::warn!(run_id=%id, %error, "Relentless answered-question projection pending");
            }
            self.launch(scope, id);
            return Ok(Some(state));
        }
        anyhow::ensure!(
            state.status == RunStatus::WaitingOwner
                && state.attention_id.as_deref() == Some(attention_id),
            "owner input does not match the pending exact request"
        );
        let attempt = state
            .attempts
            .last()
            .context("owner question has no actor attempt")?;
        let checkpoint = attempt
            .checkpoint
            .as_ref()
            .context("owner question has no sealed native checkpoint")?;
        let operation = &state.budget.operations()[&attempt.actor_operation_id].intent;
        let adapters = Arc::new(TextAdapters::recover_run(
            &self.router,
            store.clone(),
            &state,
            "owner-input-inspection".into(),
        )?);
        let actor = HarnessActor {
            recipe: setup
                .native_harness
                .context("owner question has no native recipe")?,
            reviews: adapters,
            orchestrator: self.orchestrator.clone(),
        };
        let paused = actor.inspect_pinned_pause(operation, checkpoint).await?;
        let fingerprint = fingerprint_for(&paused.body.input_type)?;
        let response = UserInputResponse::new(paused.body.input_type.clone(), value);
        let secret_store = self
            .orchestrator
            .secret_store_for_scope(&scope.principal, &scope.workspace);
        let response =
            vault_durable_pause_response(&paused.body, response, secret_store.as_deref())?;
        let recorded = RecordedOwnerInput {
            response,
            fingerprint,
            request: self.orchestrator.event_broadcaster().and_then(|bus| {
                bus.pending_hitl_request(&scope.principal, &scope.workspace, attention_id)
                    .map(Box::new)
            }),
        };
        let bytes = serde_json::to_vec(&recorded)?;
        let s = store.clone();
        let generation = state.generation;
        let answered = io(move || {
            let response_hash = s.put_blob(id, &bytes)?;
            s.commit(
                id,
                generation,
                RunEvent::OwnerInputRecorded {
                    attention_id: attention,
                    response_hash,
                },
                Utc::now(),
            )
        })
        .await?;
        if let Err(error) =
            self.emit_owner_input_resolved(&answered, execution_id, attention_id, &recorded)
        {
            tracing::warn!(run_id=%id, %error, "Relentless answered-question projection pending");
        }
        self.launch(scope, id);
        Ok(Some(answered))
    }

    fn emit_owner_input_resolved(
        &self,
        state: &RunState,
        execution_id: &str,
        attention_id: &str,
        recorded: &super::relentless_actor::RecordedOwnerInput,
    ) -> Result<()> {
        if let Some(bus) = self.orchestrator.event_broadcaster() {
            let event = super::realtime_events::RuntimeTransportEvent::HitlResolved {
                correlation_id: attention_id.into(),
                source: "agentic".into(),
                outcome: "answered".into(),
                decision: None,
                task_id: Some(state.admission.task_id.clone()),
                execution_id: Some(execution_id.into()),
                agent_id: None,
                principal: Some(state.scope.principal.clone()),
                workspace: Some(state.scope.workspace.clone()),
                timestamp: recorded.response.timestamp.timestamp_millis(),
            };
            let matches = || {
                bus.resolved_hitl_lifecycle_matches(
                    &state.scope.principal,
                    &state.scope.workspace,
                    attention_id,
                    &event,
                )
            };
            if !matches() {
                if let Some(request) = recorded.request.as_deref() {
                    anyhow::ensure!(
                        matches!(request, super::realtime_events::RuntimeTransportEvent::HitlRequested {
                            correlation_id, source, task_id: Some(task), execution_id: Some(execution),
                            principal: Some(principal), workspace: Some(workspace), timestamp, ..
                        } if correlation_id == attention_id && source == "agentic"
                            && task == &state.admission.task_id && execution == execution_id
                            && principal == &state.scope.principal && workspace == &state.scope.workspace
                            && *timestamp <= recorded.response.timestamp.timestamp_millis()),
                        "sealed owner question lost its exact task/scope binding"
                    );
                    bus.emit(request.clone());
                }
                bus.emit(event.clone());
            }
            anyhow::ensure!(
                matches(),
                "sealed owner answer has no exact resolved-question receipt"
            );
        }
        Ok(())
    }

    /// Existing tasks own lifecycle and output. Relentless's journal remains
    /// authority for reviewed bytes and spend; projections can be retried.
    pub async fn capture_handoff_view(&self, state: &RunState) -> Result<serde_json::Value> {
        let store = self.store(&state.scope);
        let snapshot = state.clone();
        let campaigns =
            super::approval_envelopes::campaign::CampaignStore::new(self.workspace.clone());
        io(move || {
            let setup: TextDraftSetup = serde_json::from_slice(
                &store.read_blob(snapshot.id, &snapshot.admission.source_hash)?,
            )?;
            let Some(target) = setup.capture_handoff else {
                return Ok(serde_json::Value::Null);
            };
            let scope = super::approval_envelopes::EnvelopeStoreScope::new(
                &snapshot.scope.principal,
                &snapshot.scope.workspace,
            );
            let campaign = campaigns.load(&scope, target.campaign_id)?;
            let receipt = match (&campaign, &snapshot.handoff) {
                (Some(campaign), Some(handoff)) => {
                    campaign.capture_receipt(&target, snapshot.id, &handoff.artifact_hash)?
                },
                _ => None,
            };
            let coverage = campaigns.validate_handoff_target(
                &scope,
                &target,
                &snapshot.admission.task_id,
                &setup
                    .task_policy_origin
                    .as_ref()
                    .context("capture handoff lost its task binding")?
                    .context_hash,
                &snapshot.contract.hash()?,
            );
            Ok(
                serde_json::json!({"target":target,"capture":receipt,"available":coverage.is_ok(),
                "unavailable_reason":coverage.err().map(|error|format!("{error:#}"))}),
            )
        })
        .await
    }

    async fn project_capture_handoff_pending_notice(
        &self,
        state: &RunState,
        target: &super::approval_envelopes::campaign::CaptureHandoffTarget,
    ) -> Result<()> {
        use super::user_requests::{RequestOption, UserRequest};
        let Some(requests) = &self.user_requests else {
            return Ok(());
        };
        requests.submit_nonblocking_durable(UserRequest {
            id:format!("relentless-handoff-{}", state.id),request_type:"relentless.capture_handoff_pending".into(),
            question:format!("The accepted local artifact is available, but its campaign capture is pending. Review current campaign scope and capacity in /relentless?run={}&campaign={}. Dismissing this notice grants no capture or delivery permission.", state.id, target.campaign_id),
            options:vec![RequestOption {id:"dismiss".into(),label:"Dismiss notice".into(),requires_input:false}],
            principal:state.scope.principal.clone(),workspace:state.scope.workspace.clone(),
            context:serde_json::json!({"run_id":state.id,"target":target,"artifact_hash":state.handoff.as_ref().map(|handoff|&handoff.artifact_hash)}),
            source:"relentless".into(),execution_id:None,task_id:Some(state.admission.task_id.clone()),
            timeout_secs:86_400,default_on_timeout:"dismiss".into(),created_at:0,sensitive:None,
        }).await?;
        Ok(())
    }

    async fn project_capture_handoff(
        &self,
        state: &RunState,
        target: &super::approval_envelopes::campaign::CaptureHandoffTarget,
        artifact_hash: &str,
    ) -> Result<super::approval_envelopes::campaign::CapturedAct> {
        let campaigns =
            super::approval_envelopes::campaign::CampaignStore::new(self.workspace.clone());
        let scope = super::approval_envelopes::EnvelopeStoreScope::new(
            &state.scope.principal,
            &state.scope.workspace,
        );
        let target_copy = target.clone();
        let run_id = state.id;
        let hash = artifact_hash.to_owned();
        let receipt = io(move || {
            let Some(campaign) = campaigns.load(&scope, target_copy.campaign_id)? else {
                return Ok(None);
            };
            Ok(campaign
                .capture_receipt(&target_copy, run_id, &hash)?
                .cloned())
        })
        .await?;
        if let Some(receipt) = receipt {
            self.project_campaign_review_notice(&state.scope, &receipt)
                .await?;
            return Ok(receipt);
        }
        self.capture_owner_campaign_run(
            state.scope.clone(),
            target.campaign_id,
            target.version_hash.clone(),
            state.id,
            artifact_hash.into(),
            target.identity_id.clone(),
        )
        .await
    }

    async fn close_capture_handoff_pending_notice(&self, state: &RunState) -> Result<()> {
        use super::user_requests::{ScopedResponseResult, UserResponse};
        let Some(requests) = &self.user_requests else {
            return Ok(());
        };
        let request_id = format!("relentless-handoff-{}", state.id);
        if requests
            .pending_request_snapshot(
                &request_id,
                Some(&state.scope.principal),
                Some(&state.scope.workspace),
            )
            .await
            .is_none()
        {
            return Ok(());
        };
        let result = requests
            .respond_scoped(
                UserResponse {
                    request_id,
                    decision: "dismiss".into(),
                    input: None,
                    channel: "relentless_handoff".into(),
                    sensitive: vec![],
                },
                Some(&state.scope.principal),
                Some(&state.scope.workspace),
            )
            .await;
        anyhow::ensure!(
            !matches!(result, ScopedResponseResult::PersistenceUnavailable),
            "capture completed; pending notice resolution needs retry"
        );
        Ok(())
    }

    /// Apply the owner's answer to the exact-review ask: `approve` / `reject`
    /// record the review on the run (the same transition as the Quality
    /// page's review) and resume it. Any other decision (`dismiss`, a
    /// timeout) decides nothing. Returns whether the review was recorded; an
    /// answer for a draft that is no longer the pending one is ignored.
    pub async fn apply_review_answer(
        &self,
        scope: &RunScope,
        run_id: Uuid,
        artifact_hash: &str,
        decision: &str,
    ) -> Result<bool> {
        let approved = match decision {
            REVIEW_APPROVE => true,
            REVIEW_REJECT => false,
            _ => return Ok(false),
        };
        let store = self.store(scope);
        let artifact_hash = artifact_hash.to_owned();
        let recorded = io(move || {
            let state = store.load(run_id)?;
            let pending = state.status == RunStatus::Accepted
                && state.handoff.is_none()
                && state.owner_approved.is_none()
                && state.routing.as_ref().is_some_and(|r| {
                    r.route == ReviewRoute::HumanReview && r.artifact_hash == artifact_hash
                });
            if !pending {
                return Ok(false);
            }
            store.commit(
                run_id,
                state.generation,
                RunEvent::OwnerReviewed {
                    artifact_hash,
                    approved,
                },
                Utc::now(),
            )?;
            Ok(true)
        })
        .await?;
        if recorded {
            self.launch(scope.clone(), run_id);
        }
        Ok(recorded)
    }

    /// Answer the exact-review ask from wherever it was answered — the
    /// composer dock, the task panel, Attention or a push — by following its
    /// resolution on the runtime bus. A missed resolution (lag, restart) is
    /// picked up from request history at the run's next projection.
    pub fn start_review_answers(self: Arc<Self>) {
        let Some(bus) = self.orchestrator.event_broadcaster() else {
            return;
        };
        let mut events = bus.subscribe();
        tokio::spawn(async move {
            use super::realtime_events::RuntimeTransportEvent;
            use tokio::sync::broadcast::error::RecvError;
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(RecvError::Lagged(count)) => {
                        tracing::warn!(count, "[RELENTLESS] review answer subscriber lagged; runs reconcile from request history");
                        continue;
                    },
                    Err(RecvError::Closed) => break,
                };
                let RuntimeTransportEvent::HitlResolved {
                    correlation_id,
                    decision: Some(decision),
                    principal: Some(principal),
                    workspace: Some(workspace),
                    ..
                } = event
                else {
                    continue;
                };
                let Some((run_id, artifact_hash)) = review_ask_target(&correlation_id) else {
                    continue;
                };
                let scope = RunScope {
                    principal,
                    workspace,
                };
                if let Err(error) = self
                    .apply_review_answer(&scope, run_id, artifact_hash, &decision)
                    .await
                {
                    tracing::warn!(%run_id, %error, "[RELENTLESS] review answer not yet recorded; the run's next projection retries");
                }
            }
        });
    }

    /// Route exact-artifact review through the existing durable request owner
    /// as an Approve / Reject ask. An answer decides the handoff only through
    /// `apply_review_answer`, which re-checks the run, its route and the exact
    /// artifact hash. Recovery reuses its stable identity without reopening it.
    async fn project_accepted_review_notice(
        &self,
        state: &RunState,
        setup: &TextDraftSetup,
        owning_cancelled: bool,
    ) -> Result<()> {
        use super::user_requests::{
            RequestOption, ScopedResponseResult, UserRequest, UserResponse,
        };
        let Some(requests) = &self.user_requests else {
            return Ok(());
        };
        let Some(artifact_hash) = state
            .decisions
            .iter()
            .rev()
            .find_map(|d| d.accepted_hash.as_ref())
        else {
            return Ok(());
        };
        let Some(execution_id) = setup.runtime_execution_id.as_ref() else {
            return Ok(());
        };
        let request_id = format!(
            "{execution_id}:relentless-review:{}:{artifact_hash}",
            state.id
        );
        let awaiting_review = !owning_cancelled
            && state.status == RunStatus::Accepted
            && state.handoff.is_none()
            && state.owner_approved.is_none()
            && state
                .routing
                .as_ref()
                .is_some_and(|r| r.route == ReviewRoute::HumanReview)
            && !setup
                .guidance_origin
                .as_ref()
                .is_some_and(|origin| origin.is_trial());
        if awaiting_review {
            // An answer that reached history but not the run (the process
            // stopped between the two) is applied here, not asked again.
            let answered = requests
                .list_history_for_scope(&state.scope.principal, &state.scope.workspace, None)
                .into_iter()
                .find(|record| record.request.id == request_id)
                .and_then(|record| record.response)
                .map(|response| response.decision);
            if let Some(decision) = answered {
                self.apply_review_answer(&state.scope, state.id, artifact_hash, &decision)
                    .await?;
                return Ok(());
            }
            requests.submit_nonblocking_durable(UserRequest {
                id: request_id,
                request_type: "relentless.accepted_review".into(),
                question: "A draft passed its checks and needs your review. Approve hands off this exact draft; Reject declines it.".into(),
                options: vec![
                    RequestOption { id: REVIEW_APPROVE.into(), label: "Approve draft".into(), requires_input: false },
                    RequestOption { id: REVIEW_REJECT.into(), label: "Reject draft".into(), requires_input: false },
                ],
                principal: state.scope.principal.clone(), workspace: state.scope.workspace.clone(),
                context: serde_json::json!({"run_id": state.id, "artifact_hash": artifact_hash, "task_id": state.admission.task_id,
                    "review_href": format!("/work?view=quality&run={}&task={}", state.id, state.admission.task_id), "review_label": "Review draft"}),
                source: "relentless".into(), execution_id: Some(execution_id.clone()), task_id: Some(state.admission.task_id.clone()),
                timeout_secs: 86_400, default_on_timeout: "dismiss".into(), created_at: 0, sensitive: None,
            }).await?;
        } else if requests
            .pending_request_snapshot(
                &request_id,
                Some(&state.scope.principal),
                Some(&state.scope.workspace),
            )
            .await
            .is_some()
        {
            let result = requests
                .respond_scoped(
                    UserResponse {
                        request_id,
                        decision: "dismiss".into(),
                        input: None,
                        channel: "relentless_review".into(),
                        sensitive: vec![],
                    },
                    Some(&state.scope.principal),
                    Some(&state.scope.workspace),
                )
                .await;
            anyhow::ensure!(
                !matches!(result, ScopedResponseResult::PersistenceUnavailable),
                "exact review notice resolution needs retry"
            );
        }
        Ok(())
    }

    async fn publish_pending_native_question(
        &self,
        store: Arc<RunStore>,
        state: &RunState,
        setup: &TextDraftSetup,
    ) -> Result<()> {
        if state.status != RunStatus::WaitingOwner || setup.native_harness.is_none() {
            return Ok(());
        }
        // WaitingOwner also represents blocked/unknown attempts. Those have no
        // resumable question; their ordinary Work status still needs projection.
        if state
            .attempts
            .last()
            .is_none_or(|attempt| attempt.checkpoint.is_none())
        {
            return Ok(());
        }
        let execution_id = setup
            .runtime_execution_id
            .as_deref()
            .context("native question has no runtime")?;
        let pauses = self
            .orchestrator
            .full_pause_store_handle()
            .context("native question has no pause store")?;
        let _lifecycle = pauses
            .acquire_stateless_execution_lifecycle_exclusion_scoped(
                &state.scope.principal,
                &state.scope.workspace,
                execution_id,
            )
            .await
            .map_err(anyhow::Error::msg)?;
        let reader = store.clone();
        let id = state.id;
        let current = io(move || Ok(reader.load(id)?)).await?;
        if current.generation != state.generation
            || current.status != RunStatus::WaitingOwner
            || current.attention_id != state.attention_id
            || current.source_epoch != state.source_epoch
        {
            return Ok(());
        }
        let runtime = self
            .orchestrator
            .get_execution(execution_id)
            .await
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            runtime.principal == state.scope.principal
                && runtime.workspace == state.scope.workspace
                && runtime.task_id.as_deref() == Some(state.admission.task_id.as_str()),
            "native question lost its runtime scope"
        );
        if runtime.waiting_state.is_terminal() {
            return Ok(());
        }
        let scope = ScopeRef::system_internal_unauthenticated(
            &state.scope.principal,
            &state.scope.workspace,
        );
        if self
            .artifacts
            .get_task(&scope, &state.admission.task_id)
            .await?
            .manifest
            .lifecycle
            != super::artifact_v2::models::TaskLifecycle::Persistent
        {
            return Ok(());
        }
        let attempt = current
            .attempts
            .last()
            .context("native question has no attempt")?;
        let checkpoint = attempt
            .checkpoint
            .as_ref()
            .context("native question has no checkpoint")?;
        anyhow::ensure!(
            current.attention_id.as_deref() == Some(checkpoint.attention_id.as_str()),
            "native question differs from current attention"
        );
        let operation = &current.budget.operations()[&attempt.actor_operation_id].intent;
        let actor = super::relentless_actor::HarnessActor {
            recipe: setup
                .native_harness
                .clone()
                .context("native question recipe missing")?,
            reviews: Arc::new(super::relentless_runtime::TextAdapters::recover_run(
                &self.router,
                store,
                &current,
                "native-question-projection".into(),
            )?),
            orchestrator: self.orchestrator.clone(),
        };
        // Receipt-backed first questions belong to the ordinary terminal
        // publisher. Only validation replacements have a pinned request here.
        if let Some(request) = actor
            .inspect_pinned_pause(operation, checkpoint)
            .await?
            .request
        {
            let bus = self
                .orchestrator
                .event_broadcaster()
                .context("native question transport missing")?;
            bus.emit(*request.clone());
            let published = bus
                .pending_hitl_request(
                    &state.scope.principal,
                    &state.scope.workspace,
                    &checkpoint.attention_id,
                )
                .context("sealed replacement question publication needs retry")?;
            anyhow::ensure!(
                serde_json::to_value(published)? == serde_json::to_value(request.as_ref())?,
                "published replacement question differs from its checkpoint"
            );
        }
        Ok(())
    }

    /// Close obsolete native asks using this terminal run's sealed actor
    /// identities. Legacy inner-limit pauses grant no new continuation; the
    /// existing lifecycle receipt and exact-generation CAS retire them.
    async fn retire_terminal_native_questions(
        &self,
        state: &RunState,
        setup: &TextDraftSetup,
        preserve_budget_pause: bool,
    ) -> Result<()> {
        use super::execution::agentic::{
            app_pause_serializable_continuation_bytes, ExecutionPauseKind,
        };
        use super::relentless_actor::HarnessActor;
        if !state.status.terminal()
            || setup.native_harness.is_none()
            || (state.status == RunStatus::BudgetExhausted && preserve_budget_pause)
        {
            return Ok(());
        }
        let execution_id = setup
            .runtime_execution_id
            .as_deref()
            .context("terminal native run has no root")?;
        let pauses = self
            .orchestrator
            .full_pause_store_handle()
            .context("terminal native run has no pause store")?;
        let _lifecycle = pauses
            .acquire_stateless_execution_lifecycle_exclusion_scoped(
                &state.scope.principal,
                &state.scope.workspace,
                execution_id,
            )
            .await
            .map_err(anyhow::Error::msg)?;
        let attempts = state
            .budget
            .operations()
            .values()
            .filter(|operation| {
                operation.intent.class == super::relentless::budget::OperationClass::Actor
            })
            .map(|operation| HarnessActor::operation_attempt_hash(state.id, &operation.intent))
            .collect::<Result<std::collections::BTreeSet<_>>>()?;
        for pending in pauses.get_pending_for_execution(execution_id) {
            if pending.principal.as_deref() != Some(&state.scope.principal)
                || pending.workspace.as_deref() != Some(&state.scope.workspace)
                || pending.task_id.as_deref() != Some(&state.admission.task_id)
                || !matches!(
                    pending.pause_kind,
                    Some(
                        ExecutionPauseKind::UserInput
                            | ExecutionPauseKind::Escalation
                            | ExecutionPauseKind::MaxIterations
                            | ExecutionPauseKind::Budget
                    )
                )
            {
                continue;
            }
            let Some(body) = pauses
                .load_serializable_by_key_from_disk_scoped_on_execution_runtime(
                    &pending.key,
                    &state.scope.principal,
                    &state.scope.workspace,
                )
                .await?
            else {
                continue;
            };
            let Some(attempt) = body
                .pause_state
                .controller_attempt_hash
                .as_ref()
                .filter(|hash| attempts.contains(*hash))
            else {
                continue;
            };
            if body.pause_state.task_id.as_deref() != Some(&state.admission.task_id)
                || body.pause_state.execution_id.as_deref() != Some(execution_id)
            {
                continue;
            }
            let correlation = body.pause_state.hitl_correlation_id();
            let expected = blake3::hash(&app_pause_serializable_continuation_bytes(&body)?)
                .to_hex()
                .to_string();
            if let Some(bus) = self.orchestrator.event_broadcaster() {
                let lifecycle = bus.hitl_lifecycle_state(
                    &state.scope.principal,
                    &state.scope.workspace,
                    &correlation,
                );
                if !matches!(
                    lifecycle,
                    super::realtime_events::HitlLifecycleState::Resolved
                ) {
                    // A durable closure precedes pause removal, so a failed
                    // publication leaves the exact body available for retry.
                    let resolved = super::realtime_events::RuntimeTransportEvent::HitlResolved {
                        correlation_id: correlation.clone(),
                        source: "agentic".into(),
                        outcome: "cancelled".into(),
                        decision: None,
                        task_id: Some(state.admission.task_id.clone()),
                        execution_id: Some(execution_id.into()),
                        agent_id: None,
                        principal: Some(state.scope.principal.clone()),
                        workspace: Some(state.scope.workspace.clone()),
                        timestamp: Utc::now().timestamp_millis(),
                    };
                    anyhow::ensure!(
                        bus.emit_hitl_lifecycle_if_accepted(resolved),
                        "terminal native question closure needs retry"
                    );
                }
            }
            let store = pauses.clone();
            let key = pending.key;
            let principal = state.scope.principal.clone();
            let workspace = state.scope.workspace.clone();
            let execution = execution_id.to_owned();
            let attempt = attempt.clone();
            super::execution::runtime_boundary::run_execution_job(move || async move {
                store.retire_controller_checkpoint_pause_scoped(
                    &key, &principal, &workspace, &execution, &attempt, &expected,
                )
            })
            .await
            .context("terminal native pause retirement failed to join")??;
        }
        if let Some(bus) = self.orchestrator.event_broadcaster() {
            // Older bounded conclusions could announce a canonical request
            // without persisting a resumable pause. The sealed actor identity
            // still proves ownership; prompt text and hints confer no authority.
            let keys: std::collections::BTreeSet<_> = attempts
                .iter()
                .map(|attempt| format!("{execution_id}:controller:{attempt}"))
                .collect();
            let mut cursor = None;
            loop {
                let (requests, next) = bus.pending_hitl_for_execution(
                    &state.scope.principal,
                    &state.scope.workspace,
                    execution_id,
                    cursor.as_deref(),
                )?;
                for request in requests {
                    let super::realtime_events::RuntimeTransportEvent::HitlRequested {
                        correlation_id,
                        source,
                        task_id,
                        principal,
                        workspace,
                        ..
                    } = request
                    else {
                        continue;
                    };
                    if source != "agentic"
                        || task_id.as_deref() != Some(&state.admission.task_id)
                        || principal.as_deref() != Some(&state.scope.principal)
                        || workspace.as_deref() != Some(&state.scope.workspace)
                        || !keys.contains(
                            super::execution::agentic::split_hitl_correlation_id(&correlation_id).0,
                        )
                    {
                        continue;
                    }
                    anyhow::ensure!(
                        bus.emit_hitl_lifecycle_if_accepted(
                            super::realtime_events::RuntimeTransportEvent::HitlResolved {
                                correlation_id,
                                source,
                                outcome: "cancelled".into(),
                                decision: None,
                                task_id,
                                execution_id: Some(execution_id.into()),
                                agent_id: None,
                                principal,
                                workspace,
                                timestamp: Utc::now().timestamp_millis(),
                            }
                        ),
                        "terminal native orphan request closure needs retry"
                    );
                }
                let Some(next) = next else {
                    break;
                };
                cursor = Some(next);
            }
        }
        Ok(())
    }

    /// Check a report or draft against the bound execution's tool results
    /// before it is published to the owner; unchanged when it verifies or
    /// when there is no tool evidence to check it against. Relentless content
    /// already passed its own adversarial critics, so this adds the
    /// deterministic figure grounding (figures only, no second judge call):
    /// a stated figure no tool result contains goes out under the caveat.
    async fn checked_owner_report(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        text: String,
    ) -> String {
        use super::artifact_v2::synthesis::{
            check_owner_text_detailed, load_execution_grounding_corpus, record_owner_text_check,
            OwnerTextReview,
        };
        let corpus =
            load_execution_grounding_corpus(&self.workspace, scope, task_id, execution_id).await;
        let check = check_owner_text_detailed(
            Arc::new(corpus),
            &text,
            "",
            OwnerTextReview::Figures,
            "relentless_report_grounding",
        )
        .await;
        record_owner_text_check(
            &self.workspace,
            scope,
            task_id,
            execution_id,
            "relentless_report",
            None,
            &check,
        )
        .await;
        check.text.unwrap_or(text)
    }

    async fn project(&self, store: Arc<RunStore>, state: &RunState) -> Result<()> {
        let scope = ScopeRef::system_internal_unauthenticated(
            &state.scope.principal,
            &state.scope.workspace,
        );
        if Box::pin(self.artifacts.get_task(&scope, &state.admission.task_id))
            .await?
            .state
            .status
            == "archived"
        {
            // Archived work retains its last report, output and review history.
            // Startup replay must not publish a fresh elapsed-time report or
            // reopen capture, question, learning or review projections.
            return Ok(());
        }
        let snapshot = state.clone();
        let read_store = store.clone();
        let (setup, report, draft, answers) = io(move || {
            let store = read_store;
            let setup: TextDraftSetup = serde_json::from_slice(
                &store.read_blob(snapshot.id, &snapshot.admission.source_hash)?,
            )?;
            let report = RunReport::from_state(&snapshot)?;
            let draft = if let Some(output) = &report.output {
                if let Some(recipe) = &setup.artifact_review {
                    let mut summary =
                        format!("[Review artifact files](/relentless?run={})\n", snapshot.id);
                    for name in recipe.files.keys() {
                        anyhow::ensure!(
                            output.files.contains_key(name),
                            "report artifact declaration missing"
                        );
                        summary.push_str(&format!("\n- {}", name.replace(['\n', '\r'], " ")));
                    }
                    Some(summary)
                } else {
                    Some(String::from_utf8(
                        store.read_blob(
                            snapshot.id,
                            output
                                .files
                                .get(&setup.artifact_file)
                                .context("report draft missing")?,
                        )?,
                    )?)
                }
            } else {
                None
            };
            let answers = if setup.native_harness.is_some() {
                snapshot
                    .owner_inputs
                    .iter()
                    .map(|(attention, hash)| {
                        let recorded: super::relentless_actor::RecordedOwnerInput =
                            serde_json::from_slice(&store.read_blob(snapshot.id, hash)?)?;
                        Ok((attention.clone(), recorded))
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                Vec::new()
            };
            Ok((setup, report, draft, answers))
        })
        .await?;
        let internal_trial = setup
            .guidance_origin
            .as_ref()
            .is_some_and(|origin| origin.is_trial());
        let continuation_available = state.status == RunStatus::BudgetExhausted
            && self
                .budget_continuation_reason(state, &setup)
                .await?
                .is_none();
        if !internal_trial {
            self.publish_pending_native_question(store.clone(), state, &setup)
                .await?;
        }
        self.retire_terminal_native_questions(state, &setup, continuation_available)
            .await?;
        let owning_cancelled = if let Some(runtime) = &setup.runtime_execution_id {
            self.orchestrator
                .get_execution_status(runtime)
                .await
                .map_err(anyhow::Error::msg)?
                == WaitingState::Cancelled
        } else {
            false
        };
        let mut capture_open_item = None;
        if let (Some(target), Some(handoff)) = (&setup.capture_handoff, &state.handoff) {
            if !owning_cancelled
                && state.status == RunStatus::Accepted
                && state.owner_approved != Some(false)
            {
                match self
                    .project_capture_handoff(state, target, &handoff.artifact_hash)
                    .await
                {
                    Ok(capture) => {
                        use super::approval_envelopes::campaign::CaptureStatus;
                        capture_open_item = match capture.status {
                            CaptureStatus::Captured => None,
                            CaptureStatus::AwaitingReview => {
                                Some("Campaign capture awaits exact owner review".into())
                            },
                            CaptureStatus::Rejected => {
                                Some("Owner rejected the exact campaign capture".into())
                            },
                        };
                        self.close_capture_handoff_pending_notice(state).await?;
                    },
                    Err(error) => {
                        tracing::warn!(run_id=%state.id, %error, "Relentless accepted local artifact retained; campaign capture pending");
                        self.project_capture_handoff_pending_notice(state, target)
                            .await?;
                        capture_open_item = Some(format!(
                            "Accepted local artifact retained; campaign capture pending: {error:#}"
                        ));
                    },
                }
            }
        }
        if !answers.is_empty() {
            let execution_id = setup
                .runtime_execution_id
                .as_deref()
                .context("sealed native answers have no runtime execution binding")?;
            for (attention, recorded) in answers {
                self.emit_owner_input_resolved(state, execution_id, &attention, &recorded)?;
            }
        }
        if let Some(execution) = setup.task_execution_id.as_ref() {
            let status = match state.status {
                _ if owning_cancelled => "cancelled",
                RunStatus::Accepted if internal_trial => "completed",
                RunStatus::Accepted if state.handoff.is_some() => "completed",
                RunStatus::Accepted if state.owner_approved == Some(false) => "failed",
                RunStatus::Accepted => "waiting_for_user",
                RunStatus::Cancelled => "cancelled",
                RunStatus::WaitingOwner => "waiting_for_user",
                RunStatus::BudgetExhausted if continuation_available => "waiting_for_user",
                s if s.terminal() => "failed",
                _ => "running",
            };
            let outcome = ExecutionOutcomeSnapshot {
                execution_status: status.into(),
                task_status: status.into(),
                outcome_type: format!("relentless_{status}"),
                outcome_summary: if state.owner_approved == Some(false) {
                    "Owner declined the exact accepted draft handoff".into()
                } else {
                    state
                        .routing
                        .as_ref()
                        .map(|r| r.reason.clone())
                        .or_else(|| state.reason.clone())
                        .unwrap_or_else(|| "Local draft improvement is running".into())
                },
                iterations_used: Some(state.attempts.len()),
                is_terminal: owning_cancelled
                    || state.status.terminal()
                        && !continuation_available
                        && (state.status != RunStatus::Accepted
                            || internal_trial
                            || state.handoff.is_some()
                            || state.owner_approved == Some(false)),
                completion_kind: None,
                open_items: report
                    .open_items
                    .iter()
                    .cloned()
                    .chain(capture_open_item)
                    .collect(),
            };
            if outcome.is_terminal {
                let text = if internal_trial {
                    "Internal guidance qualification finished. Evidence remains in its owner-scoped policy and run journal.".into()
                } else if state.status == RunStatus::Accepted
                    && state.handoff.is_some()
                    && !owning_cancelled
                {
                    // The accepted text handoff remains the exact reviewed bytes.
                    draft.unwrap_or_else(|| outcome.outcome_summary.clone())
                } else {
                    let mut text = report.markdown();
                    text.push_str(&format!(
                        "\n[Inspect this run and its sealed files](/relentless?run={})\n",
                        state.id
                    ));
                    if let Some(draft) = draft {
                        text.push_str("\n## Available artifact\n\n");
                        text.push_str(&draft);
                    }
                    text
                };
                // A report reaches the owner as the run's claims: it is
                // checked against the execution's tool results first, unless
                // the owner approved these exact bytes.
                let text = if state.owner_approved == Some(true) {
                    text
                } else {
                    self.checked_owner_report(&scope, &state.admission.task_id, execution, text)
                        .await
                };
                Box::pin(self.artifacts.persist_precomputed_execution_outcome(
                    &scope,
                    &state.admission.task_id,
                    execution,
                    &format!("relentless:{}:{}", state.id, setup.admission_key()),
                    outcome,
                    text,
                ))
                .await?;
            } else {
                if let Some(draft) = draft.filter(|_| state.status == RunStatus::Accepted) {
                    let (media_type, draft) = if state.owner_approved == Some(true) {
                        ("text/plain", draft)
                    } else {
                        let checked = self
                            .checked_owner_report(
                                &scope,
                                &state.admission.task_id,
                                execution,
                                draft.clone(),
                            )
                            .await;
                        if checked == draft {
                            ("text/plain", draft)
                        } else {
                            ("text/markdown", checked)
                        }
                    };
                    self.artifacts
                        .write_user_output_direct(
                            &scope,
                            &state.admission.task_id,
                            WriteUserOutputDirectInput {
                                media_type: media_type.into(),
                                body: WriteUserOutputBody::Text(draft),
                                dashboard_theme: None,
                            },
                        )
                        .await?;
                }
                self.artifacts
                    .persist_external_execution_outcome(
                        &scope,
                        &state.admission.task_id,
                        execution,
                        outcome,
                    )
                    .await?;
                if let Some(runtime) = &setup.runtime_execution_id {
                    let current = self
                        .orchestrator
                        .get_execution_status(runtime)
                        .await
                        .map_err(anyhow::Error::msg)?;
                    anyhow::ensure!(
                        current != WaitingState::Cancelled,
                        "owning task was cancelled during nonterminal projection"
                    );
                    let target = if status == "waiting_for_user" {
                        WaitingState::WaitingUser
                    } else {
                        WaitingState::Executing
                    };
                    self.orchestrator
                        .set_ordinary_execution_status_if_current(
                            runtime,
                            &current,
                            target,
                            "Relentless journal projection",
                        )
                        .await
                        .map_err(anyhow::Error::msg)?;
                }
            }
        }
        self.project_guidance_trial_receipt(store.clone(), state, &setup)
            .await?;
        self.project_guidance_monitor(store.clone(), state, &setup)
            .await?;
        let workspace = self.workspace.clone();
        let snapshot = state.clone();
        let lesson_setup = setup.clone();
        io(move || {
            super::relentless_learning::project_lessons(workspace, &store, &snapshot, &lesson_setup)
        })
        .await?;
        self.project_accepted_review_notice(state, &setup, owning_cancelled)
            .await?;
        self.project_budget_review_notice(
            state,
            &setup,
            continuation_available && !owning_cancelled,
        )
        .await?;
        Ok(())
    }

    pub async fn run_report(&self, scope: &RunScope, id: Uuid) -> Result<serde_json::Value> {
        let store = self.store(scope);
        let (state, report) = io(move || {
            let state = store.load(id)?;
            let report = RunReport::from_state(&state)?;
            Ok((state, report))
        })
        .await?;
        let capture_handoff = match self.capture_handoff_view(&state).await {
            Ok(view) => view,
            Err(error) => {
                serde_json::json!({"available":false,"unavailable_reason":format!("{error:#}")})
            },
        };
        Ok(
            serde_json::json!({"report":report,"content_hash":report.hash()?,
            "markdown":report.markdown(),"capture_handoff":capture_handoff,"derived_from_journal":true}),
        )
    }

    pub async fn run_lessons(&self, scope: &RunScope, id: Uuid) -> Result<serde_json::Value> {
        let store = self.store(scope);
        let workspace = self.workspace.clone();
        io(move || {
            let state = store.load(id)?;
            let learning = super::learning::LearningStore::new(workspace);
            let candidate_id = format!("lc_relentless_{}", id.simple());
            let learning_scope = super::learning::LearningScope::new(&state.scope.principal, &state.scope.workspace);
            match learning.read_candidate(&learning_scope, &candidate_id) {
                Ok(candidate) => {
                    anyhow::ensure!(candidate.source_task_id.as_deref() == Some(state.admission.task_id.as_str())
                        && candidate.proposed_change["run_id"] == id.to_string(), "learning evidence lost its run binding");
                    let hash = candidate.proposed_change["lesson_hash"].as_str().context("sealed lesson hash missing")?;
                    let lesson: serde_json::Value = serde_json::from_slice(&store.read_blob(id, hash)?)?;
                    anyhow::ensure!(lesson["version"] == "relentless-lessons-v1" && lesson["run_id"] == id.to_string(), "sealed lessons identity changed");
                    Ok(serde_json::json!({"lessons":lesson,"content_hash":hash,"sealed":true,"current_spend":state.budget.spend()?}))
                },
                Err(error) if learning.error_is_not_found(&error) => Ok(serde_json::json!({"lessons":super::relentless_learning::run_lessons(&state)?,"sealed":false,"current_spend":state.budget.spend()?})),
                Err(error) => Err(error),
            }
        }).await
    }

    /// Read-only pipeline over existing task/goal identities. Local acceptance
    /// is reported separately from any external outcome owned by other modules.
    pub async fn owner_work_view(
        &self,
        scope: &RunScope,
        goal_id: Option<&str>,
        page: &runtime_core::storage::PaginationParams,
        query: &str,
    ) -> Result<serde_json::Value> {
        anyhow::ensure!(
            goal_id.is_none_or(|id| !id.trim().is_empty() && id.trim() == id),
            "goal identity must be exact and nonempty"
        );
        let store = self.store(scope);
        let runs = io(move || store.list_projected(|run| Ok((run.admission.task_id.clone(), serde_json::json!({
            "id":run.id,"status":run.status,"reason":run.reason,"source_epoch":run.source_epoch,
            "deadline_at":run.deadline_at,"contract_hash":run.contract.hash()?,"accepted_artifact":run.decisions.iter().rev().find_map(|decision|decision.accepted_hash.clone()),
            "routing":run.routing,"handoff":run.handoff,"owner_approved":run.owner_approved,"spend":run.budget.spend()?,
        }))))).await?;
        let artifact_scope =
            ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
        let mut eligible = Vec::new();
        for item in self.artifacts.list_tasks(&artifact_scope).await? {
            let task = self.artifacts.get_task(&artifact_scope, &item.id).await?;
            let task_runs: Vec<_> = runs.iter().filter(|run| run.0 == item.id).collect();
            if goal_id.is_some_and(|id| task.manifest.goal_id.as_deref() != Some(id))
                || (goal_id.is_none() && task_runs.is_empty() && task.manifest.relentless.is_none())
            {
                continue;
            }
            if !catalog_matches(
                query,
                &[
                    &task.manifest.title,
                    &item.id,
                    task.manifest.goal_id.as_deref().unwrap_or(""),
                    &task.manifest.agent_id,
                ],
            ) {
                continue;
            }
            eligible.push((item.id, task));
        }
        eligible.sort_by(|a, b| {
            a.1.manifest
                .title
                .to_lowercase()
                .cmp(&b.1.manifest.title.to_lowercase())
                .then_with(|| a.0.cmp(&b.0))
        });
        let pagination = catalog_pagination(eligible.len(), page);
        let mut targets = Vec::new();
        for (id, task) in eligible.into_iter().skip(page.offset).take(page.limit) {
            let task_runs: Vec<_> = runs.iter().filter(|run| run.0 == id).collect();
            let run_count = task_runs.len();
            let context_hash =
                task_policy_context_hash(&task.manifest).map_err(anyhow::Error::msg)?;
            let policy = task.manifest.relentless.as_ref().map(|policy| -> Result<_> { Ok(serde_json::json!({
                "id":policy.id,"max_runs":policy.max_runs,"runs_accepted":task.state.relentless_policy_runs.get(&policy.id.to_string()).copied().unwrap_or(0),
                "expires_at":policy.expires_at,"expired":policy.expires_at<=Utc::now(),
                "context_changed":policy.task_context_hash!=context_hash,
                "maximum_cost_microusd":policy.maximum_cost_microusd()?,
            })) }).transpose()?;
            let run_views: Vec<_> = task_runs
                .into_iter()
                .take(1)
                .map(|(_, view)| view.clone())
                .collect();
            targets.push(serde_json::json!({"task_id":id,"title":task.manifest.title,"context_hash":context_hash,
                "goal_id":task.manifest.goal_id,"agent_id":task.manifest.agent_id,"due_date":task.manifest.due_date,
                "task_status":task.state.status,"policy":policy,"runs":run_views,"run_count":run_count}));
        }
        Ok(
            serde_json::json!({"goal_id":goal_id,"targets":targets,"pagination":pagination,"external_outcome_owned_elsewhere":true}),
        )
    }

    /// Local billing proof only; this never creates an actor/critic adapter or
    /// reopens a terminal ordinary task. Projection uses the current journal.
    pub async fn reconcile_owner_costs(&self, scope: RunScope, id: Uuid) -> Result<RunState> {
        let store = self.store(&scope);
        let state = io(move || {
            store.reconcile_terminal_receipts(id, format!("relentless-billing-{}", Uuid::new_v4()))
        })
        .await?;
        self.launch(scope, id);
        Ok(state)
    }

    async fn reconcile_terminal_billing(
        &self,
        store: Arc<RunStore>,
        state: RunState,
        worker: String,
        preserve_attempt: bool,
    ) -> Result<RunState> {
        let id = state.id;
        let s = store.clone();
        match io(move || {
            s.reconcile_terminal_receipts_with_continuation(id, worker, preserve_attempt)
        })
        .await
        {
            Ok(state) => Ok(state),
            Err(error) => {
                tracing::warn!(run_id=%id, %error, "Relentless billing proof pending; original ceiling retained");
                // An earlier operation may already have settled. Never project
                // the stale snapshot after a partial reconciliation.
                io(move || store.load(id)).await
            },
        }
    }

    async fn finalize(
        &self,
        store: Arc<RunStore>,
        state: RunState,
        worker: String,
    ) -> Result<RunState> {
        if state.status != RunStatus::Accepted
            || state.handoff.is_some()
            || state.routing.as_ref().is_some_and(|r| {
                r.route == ReviewRoute::HumanReview && state.owner_approved != Some(true)
            })
        {
            return Ok(state);
        }
        io(move || {
            let mut state = store.claim(state.id, worker, Utc::now())?;
            if state.routing.is_none() {
                state = store.commit(
                    state.id,
                    state.generation,
                    RunEvent::ReviewRouted,
                    Utc::now(),
                )?;
            }
            if state.handoff.is_none()
                && (state.owner_approved == Some(true)
                    || state
                        .routing
                        .as_ref()
                        .is_some_and(|r| r.route != ReviewRoute::HumanReview))
            {
                state = store.commit(
                    state.id,
                    state.generation,
                    RunEvent::HandoffRecorded,
                    Utc::now(),
                )?;
            }
            store.commit(
                state.id,
                state.generation,
                RunEvent::LeaseReleased,
                Utc::now(),
            )
        })
        .await
    }

    pub fn launch(&self, scope: RunScope, id: Uuid) {
        let key = (scope.principal.clone(), scope.workspace.clone(), id);
        let control = Control::default();
        {
            let mut controls = self.controls.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(active) = controls.get(&key) {
                active.notify();
                return;
            }
            controls.insert(key.clone(), control.clone());
        }
        let api = self.clone();
        tokio::spawn(async move {
            let store = api.store(&scope);
            let worker = format!("relentless-worker-{}", Uuid::new_v4());
            let mut observed_wake = control.revision();
            let result: Result<()> = async {
                let s = store.clone();
                let initial = io(move || s.load(id)).await?;
                let s = store.clone(); let source_hash = initial.admission.source_hash.clone();
                let setup: TextDraftSetup = io(move || Ok(serde_json::from_slice(&s.read_blob(id, &source_hash)?)?)).await?;
                let mut task = magicllm::TaskRef::task(initial.admission.task_id.clone());
                task.root_execution_id = setup.runtime_execution_id.clone();
                task.execution_id = setup.runtime_execution_id.clone();
                let external_cancel = api.cancel_bridge.as_ref().map(|b| b.subscribe_cancel(&task)).unwrap_or_default();
                let s = store.clone(); let cancellation = control.cancel.clone();
                let relay = tokio::spawn(async move {
                    external_cancel.cancelled().await;
                    let cancelled = io(move || {
                        let state = s.load(id)?;
                        if !state.status.terminal() || state.status == RunStatus::BudgetExhausted || (state.status == RunStatus::Accepted && state.handoff.is_none() && state.owner_approved != Some(false)) {
                            s.commit(id, state.generation + 1, RunEvent::Cancelled, Utc::now())?;
                        }
                        Ok(())
                    }).await;
                    if let Err(error) = cancelled { tracing::error!(run_id=%id, %error, "Owning task cancellation journal failed"); }
                    cancellation.cancel();
                });
                struct Relay(tokio::task::JoinHandle<()>);
                impl Drop for Relay { fn drop(&mut self) { self.0.abort(); } }
                let _relay = Relay(relay);
                loop {
                    // Capture before loading the journal. A later owner wake
                    // cannot be lost if this worker exits after projection.
                    observed_wake = control.revision();
                    let s = store.clone(); let state = io(move || s.load(id)).await?;
                    if !state.status.terminal() && setup.guidance_origin.as_ref().is_some_and(|origin| origin.is_trial()) {
                        let service = api.clone(); let run = state.clone(); let setup = setup.clone();
                        if !io(move || service.guidance_trial_is_active(&run, &setup)).await? {
                            let s = store.clone(); let generation = state.generation + 1;
                            io(move || s.commit(id, generation, RunEvent::Cancelled, Utc::now())).await?;
                            control.cancel.cancel();
                            continue;
                        }
                    }
                    if !state.status.terminal() || state.status == RunStatus::BudgetExhausted || (state.status == RunStatus::Accepted && state.handoff.is_none() && state.owner_approved != Some(false)) {
                        if let Some(runtime) = &setup.runtime_execution_id {
                            if api.orchestrator.get_execution_status(runtime).await.map_err(anyhow::Error::msg)? == WaitingState::Cancelled {
                                let s = store.clone(); let generation = state.generation + 1;
                                io(move || s.commit(id, generation, RunEvent::Cancelled, Utc::now())).await?;
                                control.cancel.cancel();
                                continue;
                            }
                        }
                    }
                    if let Some(lease) = &state.lease {
                        if lease.expires_at > Utc::now() {
                            tokio::select! { _ = control.cancel.cancelled() => {}, _ = tokio::time::sleep((lease.expires_at - Utc::now()).to_std().unwrap_or_default()) => {} }
                            continue;
                        }
                    }
                    if state.status.terminal() {
                        let state = api.reconcile_terminal_billing(store.clone(), state, worker.clone(), !setup.guidance_origin.as_ref().is_some_and(|origin| origin.is_trial())).await?;
                        let state = if setup.guidance_origin.as_ref().is_some_and(|origin| origin.is_trial()) {
                            state
                        } else { api.finalize(store.clone(), state, worker.clone()).await? };
                        match api.project(store.clone(), &state).await {
                            Ok(()) if !setup.guidance_origin.as_ref().is_some_and(|origin| origin.is_trial()) && ((state.status == RunStatus::Accepted && state.handoff.is_none() && state.owner_approved.is_none())
                                || (state.status == RunStatus::BudgetExhausted && api.budget_continuation_reason(&state, &setup).await?.is_none())) => {
                                // Keep the owning task's cancellation subscription
                                // alive while an accepted draft awaits human review.
                                // This wait dispatches no productive/model work.
                                tokio::select! { _ = control.cancel.cancelled() => {}, _ = control.wake.notified() => {} }
                                continue;
                            }
                            Ok(()) => break,
                            Err(error) => {
                                // The journal is already authoritative. Retry only
                                // its stable task projection; never another actor
                                // or critic operation. Reload before each retry so
                                // an owner cancellation/review remains authoritative.
                                tracing::warn!(run_id=%id, %error, "Relentless terminal projection pending; retrying sealed output");
                                tokio::select! { _ = control.wake.notified() => {}, _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {} }
                                continue;
                            }
                        }
                    }
                    if state.status == RunStatus::WaitingOwner && state.deadline_at > Utc::now() {
                        if let Err(error) = api.project(store.clone(), &state).await { tracing::warn!(run_id=%id, %error, "Relentless task projection pending; deadline remains active"); }
                        tokio::select! { _ = control.cancel.cancelled() => {}, _ = control.wake.notified() => {}, _ = magician_relentless::controller::wait_for_deadline(state.deadline_at) => {} }
                        continue;
                    }
                    if setup.native_harness.is_some() && state.deadline_at > Utc::now() {
                        // The normal root shell starts in Planning. Publish
                        // this controller's running state through the existing
                        // lifecycle writer before native phase/steer admission.
                        // Failure retries projection only, never an actor call.
                        // Once the deadline expires, let the existing controller
                        // stop the run even if its UI projection is still pending.
                        if let Err(error) = api.project(store.clone(), &state).await {
                            tracing::warn!(run_id=%id, %error, "Relentless native task launch projection pending");
                            tokio::select! { _ = control.cancel.cancelled() => {}, _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {} }
                            continue;
                        }
                    }
                    let s = store.clone(); let router = api.router.clone(); let worker_id = worker.clone();
                    let adapters = io(move || TextAdapters::recover_run(&router, s, &state, worker_id)).await;
                    let state = match adapters {
                        Ok(adapters) => {
                            let adapters = Arc::new(adapters);
                            let actor: Arc<dyn magician_relentless::controller::ActorAdapter> = match &adapters.native_harness {
                                Some(recipe) => Arc::new(super::relentless_actor::HarnessActor { reviews: adapters.clone(), orchestrator: api.orchestrator.clone(), recipe: recipe.clone() }),
                                None => adapters.clone(),
                            };
                            Controller { store: store.clone(), actor, critic: adapters, worker_id: worker.clone() }.run_pass(id, control.cancel.clone()).await?
                        }
                        Err(error) => {
                            let s = store.clone(); let w = worker.clone();
                            io(move || {
                                let claimed = s.claim(id, w, Utc::now())?;
                                let event = if Utc::now() >= claimed.deadline_at { RunEvent::DeadlineExpired } else { RunEvent::Blocked { reason: format!("Frozen draft adapter unavailable: {error:#}"), attention_id: format!("relentless-{id}-{}", claimed.attempts.len()) } };
                                let state = s.commit(id, claimed.generation, event, Utc::now())?;
                                s.commit(id, claimed.generation, RunEvent::LeaseReleased, Utc::now())?;
                                Ok(state)
                            }).await?
                        }
                    };
                    if state.status == RunStatus::BudgetExhausted {
                        // A pass can return its pre-release snapshot. Finish
                        // terminal billing in the next loop before publishing
                        // an owner continuation notice for the reviewed generation.
                        continue;
                    }
                    if let Err(error) = api.project(store.clone(), &state).await { tracing::warn!(run_id=%id, %error, "Relentless task projection pending; run journal retained"); }
                }
                Ok(())
            }.await;
            if let Err(error) = result {
                tracing::error!(run_id = %id, error = %error, "Relentless owner worker stopped; durable state retained");
            }
            let pending_wake = {
                let mut controls = api.controls.lock().unwrap_or_else(|p| p.into_inner());
                controls.remove(&key);
                control.revision() != observed_wake
            };
            if pending_wake {
                // Only an unobserved external wake retries a stopped worker;
                // errors alone must not create an uncontrolled restart loop.
                api.launch(scope, id);
            }
        });
    }

    pub async fn recover(&self) -> Result<()> {
        let workspace = self.workspace.clone();
        let scopes = io(move || Ok(workspace.list_scopes())).await?;
        let mut failures = 0usize;
        for (principal, workspace) in scopes {
            if !scope_hosts_user_subsystems(&principal, &workspace) {
                continue;
            }
            let scope = RunScope {
                principal,
                workspace,
            };
            let campaigns =
                super::approval_envelopes::campaign::CampaignStore::new(self.workspace.clone());
            let campaign_scope = super::approval_envelopes::EnvelopeStoreScope::new(
                &scope.principal,
                &scope.workspace,
            );
            let discovery = campaigns.clone();
            let s = campaign_scope.clone();
            let campaign_ids = match io(move || discovery.list_ids(&s)).await {
                Ok(ids) => ids,
                Err(error) => {
                    failures += 1;
                    tracing::warn!(?scope, %error, "Relentless campaign discovery remains blocked");
                    vec![]
                },
            };
            for id in campaign_ids {
                let store = campaigns.clone();
                let s = campaign_scope.clone();
                let campaign = match io(move || {
                    store.load(&s, id)?.context("listed campaign missing")
                })
                .await
                {
                    Ok(state) => state,
                    Err(error) => {
                        failures += 1;
                        tracing::warn!(?scope, campaign_id=%id, %error, "Relentless campaign recovery remains blocked");
                        continue;
                    },
                };
                for act in campaign.captures.values() {
                    if act.status
                        != super::approval_envelopes::campaign::CaptureStatus::AwaitingReview
                        || (campaign.campaign.revoked_at.is_none()
                            && campaign.campaign.spec.expires_at > Utc::now()
                            && campaign.campaign.version_hash == act.binding.version_hash)
                    {
                        if let Err(error) = self.project_campaign_review_notice(&scope, act).await {
                            failures += 1;
                            tracing::warn!(capture_id=%act.id, %error, "Relentless capture review notice projection remains pending");
                        }
                    }
                }
            }
            let artifact_scope =
                ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
            // Discovery is not a full task feed: a damaged sibling manifest
            // must not prevent recovery of a separately committed launch.
            let tasks: Vec<String> = match self
                .workspace
                .read_dir_path_or_empty(
                    self.workspace
                        .tasks_root(&scope.principal, &scope.workspace),
                )
                .await
            {
                Ok(entries) => entries
                    .into_iter()
                    .filter(|entry| entry.is_dir)
                    .map(|entry| entry.file_name)
                    .collect(),
                Err(error) => {
                    failures += 1;
                    tracing::warn!(?scope, %error, "Relentless task discovery remains blocked");
                    vec![]
                },
            };
            for task_id in tasks {
                if let Err(error) = ArtifactV2Workspace::validate_task_id(&task_id) {
                    failures += 1;
                    tracing::warn!(?scope, %error, "Relentless recovery found an invalid task directory");
                    continue;
                }
                let executions = match self
                    .artifacts
                    .list_executions(&artifact_scope, &task_id)
                    .await
                {
                    Ok(executions) => executions,
                    Err(error) => {
                        failures += 1;
                        tracing::warn!(%task_id, %error, "Relentless execution discovery remains blocked");
                        continue;
                    },
                };
                for execution in executions {
                    let result: Result<()> = async {
                        if self
                            .artifacts
                            .read_relentless_launch(
                                &artifact_scope,
                                &task_id,
                                &execution.execution_id,
                            )
                            .await?
                            .is_some()
                        {
                            self.launch_task_policy(
                                &artifact_scope,
                                &task_id,
                                &execution.execution_id,
                            )
                            .await?;
                        }
                        Ok(())
                    }
                    .await;
                    if let Err(error) = result {
                        failures += 1;
                        tracing::warn!(%task_id, execution_id=%execution.execution_id, %error, "Relentless task policy recovery remains pending");
                    }
                }
            }
            let s = self.store(&scope);
            match io(move || s.list_projected_results(|state| Ok(state.id))).await {
                Ok(records) => {
                    for (id, result) in records {
                        match result {
                            Ok(_) => self.launch(scope.clone(), id),
                            Err(error) => {
                                failures += 1;
                                tracing::warn!(?scope, run_id=%id, %error, "Relentless damaged run remains blocked; healthy runs recover independently");
                            },
                        }
                    }
                },
                Err(error) => {
                    failures += 1;
                    tracing::warn!(?scope, %error, "Relentless run discovery remains blocked");
                },
            }
            if let Err(error) = self.recover_guidance_policies(&scope).await {
                failures += 1;
                tracing::warn!(?scope, %error, "Relentless guidance recovery has blocked records");
            }
        }
        anyhow::ensure!(failures == 0, "Relentless recovery retained {failures} blocked records or discovery/projection failures; verified records recovered independently");
        Ok(())
    }

    pub fn router(&self) -> &OperationLlmRouter {
        &self.router
    }

    pub fn with_user_requests(
        mut self,
        requests: Arc<super::user_requests::UserRequestService>,
    ) -> Self {
        self.user_requests = Some(requests);
        self
    }

    pub async fn pending_proposals(&self, scope: &RunScope) -> Result<Vec<ProposedTextDraft>> {
        let Some(requests) = &self.user_requests else {
            return Ok(vec![]);
        };
        requests
            .list_pending_for_scope(&scope.principal, &scope.workspace)
            .await
            .into_iter()
            .filter(|r| r.request_type == TEXT_PROPOSAL_REQUEST_TYPE)
            .map(|r| Self::proposal_from_request(r))
            .collect()
    }

    fn proposal_from_request(
        request: super::user_requests::UserRequest,
    ) -> Result<ProposedTextDraft> {
        anyhow::ensure!(
            request.request_type == TEXT_PROPOSAL_REQUEST_TYPE,
            "request is not a Relentless proposal"
        );
        let proposed: ProposedTextDraft = serde_json::from_value(request.context)?;
        proposed.validate()?;
        anyhow::ensure!(
            proposed.origin.request_id == request.id
                && Some(&proposed.origin.task_id) == request.task_id.as_ref()
                && Some(&proposed.origin.execution_id) == request.execution_id.as_ref(),
            "proposal request provenance mismatch"
        );
        Ok(proposed)
    }

    /// Called only by a verified owner admission boundary. Agent-created
    /// proposal data cannot invoke this method through a compiled capability.
    pub async fn start_owner_text_draft(
        &self,
        scope: RunScope,
        id: Uuid,
        proposal: &TextDraftProposal,
        ui_thread_id: &str,
        agent_id: &str,
        expected_contract_hash: &str,
        proposal_request_id: Option<&str>,
    ) -> Result<(RunState, bool)> {
        self.start_owner_text_draft_with_task_policy(
            scope,
            id,
            proposal,
            ui_thread_id,
            agent_id,
            expected_contract_hash,
            proposal_request_id,
            None,
        )
        .await
    }

    pub async fn owner_task_context(
        &self,
        scope: &RunScope,
        task_id: &str,
    ) -> Result<OwnerTaskContext> {
        use super::artifact_v2::V3ReadApi;
        let artifact_scope =
            ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
        let task = self.artifacts.get_task(&artifact_scope, task_id).await?;
        let mut reason = if !task.manifest.approved {
            Some("Approve the task before starting its policy.".to_string())
        } else if task.state.active_root_execution_id.is_some() {
            Some("Finish or stop the current task execution first.".to_string())
        } else if !task.state.synthesis_pending_executions.is_empty() {
            Some("The previous task output is still being prepared.".to_string())
        } else {
            None
        };
        if reason.is_none() {
            if let Some(prior) = task.state.latest_root_execution_id.as_deref() {
                let prior = self
                    .artifacts
                    .get_execution(&artifact_scope, task_id, prior)
                    .await?;
                if !matches!(
                    prior.state.status.as_str(),
                    "completed" | "failed" | "cancelled"
                ) || prior.state.synthesis_pending
                {
                    reason = Some("The previous task execution has not settled.".into());
                }
            }
        }
        let mut pending_run_id = None;
        let mut pending_proposal_hash = None;
        if let Some(root) = task.state.latest_root_execution_id.as_deref() {
            if let Some(owner) = self
                .artifacts
                .external_execution_output_owner(&artifact_scope, task_id, root)
                .await?
            {
                if let Some((run_id, proposal_hash)) = owner
                    .owner_id
                    .strip_prefix("relentless:")
                    .and_then(|value| value.split_once(':'))
                {
                    if let Ok(id) = Uuid::parse_str(run_id) {
                        let store = self.store(scope);
                        if !io(move || store.has_admission(id)).await? {
                            pending_run_id = Some(id);
                            pending_proposal_hash = Some(proposal_hash.to_owned());
                            reason = Some("Retry the exact reviewed brief to finish the interrupted admission.".into());
                        }
                    }
                }
            }
        }
        Ok(OwnerTaskContext {
            task_id: task_id.to_owned(),
            context_hash: task_policy_context_hash(&task.manifest).map_err(anyhow::Error::msg)?,
            title: task.manifest.title,
            goal: task.manifest.description,
            agent_id: task.manifest.agent_id,
            ui_thread_id: task.manifest.ui_thread_id,
            can_start: reason.is_none() || pending_run_id.is_some(),
            reason,
            pending_run_id,
            pending_proposal_hash,
            policy_runs: task
                .manifest
                .relentless
                .as_ref()
                .and_then(|policy| {
                    task.state
                        .relentless_policy_runs
                        .get(&policy.id.to_string())
                        .copied()
                })
                .unwrap_or(0),
            policy: task.manifest.relentless,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_owner_text_draft_with_task_policy(
        &self,
        scope: RunScope,
        id: Uuid,
        proposal: &TextDraftProposal,
        ui_thread_id: &str,
        agent_id: &str,
        expected_contract_hash: &str,
        proposal_request_id: Option<&str>,
        existing_task: Option<&ExistingTaskAdmission>,
    ) -> Result<(RunState, bool)> {
        self.start_owner_text_draft_with_capture(
            scope,
            id,
            proposal,
            ui_thread_id,
            agent_id,
            expected_contract_hash,
            proposal_request_id,
            existing_task,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_owner_text_draft_with_capture(
        &self,
        scope: RunScope,
        id: Uuid,
        proposal: &TextDraftProposal,
        ui_thread_id: &str,
        agent_id: &str,
        expected_contract_hash: &str,
        proposal_request_id: Option<&str>,
        existing_task: Option<&ExistingTaskAdmission>,
        capture_handoff: Option<&super::approval_envelopes::campaign::CaptureHandoffTarget>,
    ) -> Result<(RunState, bool)> {
        // Admission spans durable task/root writes. Its owner outlives an HTTP
        // disconnect and always finishes the committed admission + worker wake.
        let service = self.clone();
        let proposal = proposal.clone();
        let ui_thread_id = ui_thread_id.to_owned();
        let agent_id = agent_id.to_owned();
        let expected_contract_hash = expected_contract_hash.to_owned();
        let proposal_request_id = proposal_request_id.map(str::to_owned);
        let existing_task = existing_task.cloned();
        let capture_handoff = capture_handoff.cloned();
        tokio::spawn(async move {
            service
                .start_text_draft_with_origin(
                    scope,
                    id,
                    &proposal,
                    &ui_thread_id,
                    &agent_id,
                    &expected_contract_hash,
                    proposal_request_id.as_deref(),
                    existing_task.as_ref(),
                    capture_handoff.as_ref(),
                    None,
                )
                .await
        })
        .await
        .context("Relentless owner admission worker")?
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_text_draft_with_origin(
        &self,
        scope: RunScope,
        id: Uuid,
        proposal: &TextDraftProposal,
        ui_thread_id: &str,
        agent_id: &str,
        expected_contract_hash: &str,
        proposal_request_id: Option<&str>,
        existing_task: Option<&ExistingTaskAdmission>,
        capture_handoff: Option<&super::approval_envelopes::campaign::CaptureHandoffTarget>,
        guidance_origin: Option<&super::learning::relentless_promotion::GuidanceRunOrigin>,
    ) -> Result<(RunState, bool)> {
        anyhow::ensure!(
            scope_hosts_user_subsystems(&scope.principal, &scope.workspace),
            "owner scope cannot host Relentless"
        );
        anyhow::ensure!(
            proposal
                .native_harness
                .as_ref()
                .is_none_or(|recipe| recipe.agent_id == agent_id),
            "native actor identity must match the owning task"
        );
        let _admission = self.admission_lock.lock().await;
        let store = self.store(&scope);

        let s = store.clone();
        let existing = match io(move || {
            if s.has_admission(id)? {
                Ok(Some(s.load(id)?))
            } else {
                Ok(None)
            }
        })
        .await
        {
            Ok(existing) => existing,
            Err(error) => return Err(error),
        };
        if let Some(existing) = existing {
            let hash = blake3::hash(&serde_json::to_vec(&proposal).expect("serializable proposal"))
                .to_hex()
                .to_string();
            let s = store.clone();
            let source_hash = existing.admission.source_hash.clone();
            let setup = io(move || {
                Ok(serde_json::from_slice::<TextDraftSetup>(
                    &s.read_blob(id, &source_hash)?,
                )?)
            })
            .await;
            if !setup.is_ok_and(|s| {
                s.proposal_hash == hash
                    && s.proposal_origin.as_ref().map(|o| o.request_id.as_str())
                        == proposal_request_id
                    && s.task_policy_origin.as_ref() == existing_task
                    && s.capture_handoff.as_ref() == capture_handoff
                    && s.guidance_origin.as_ref() == guidance_origin
            }) || existing.contract.hash()? != expected_contract_hash
            {
                return Err(anyhow::anyhow!(
                    "Run ID already belongs to another admitted proposal"
                ));
            }
            if let Some(origin) = guidance_origin {
                let deadline = self.validate_guidance_trial_admission(
                    &scope,
                    id,
                    proposal,
                    expected_contract_hash,
                    origin,
                )?;
                anyhow::ensure!(
                    existing.admission.not_after == Some(deadline),
                    "trial lost its absolute parent policy deadline"
                );
            }
            self.launch(scope, id);
            return Ok((existing, false));
        }
        let mut prepared = match self.prepare_owner_draft(&scope, &proposal) {
            Ok(p) => p,
            Err(e) => return Err(e),
        };
        anyhow::ensure!(
            prepared.contract.hash()? == expected_contract_hash,
            "Reviewed contract changed; review the current contract before starting"
        );
        let guidance_deadline = if let Some(origin) = guidance_origin {
            let deadline = self.validate_guidance_trial_admission(
                &scope,
                id,
                proposal,
                expected_contract_hash,
                origin,
            )?;
            prepared.setup.guidance_origin = Some(origin.clone());
            Some(deadline)
        } else {
            None
        };
        if let Some(target) = capture_handoff {
            let task = existing_task
                .context("campaign capture handoff requires an existing reviewed task")?;
            anyhow::ensure!(
                proposal.handoff_kind == super::relentless::routing::HandoffKind::Capture,
                "campaign target requires a capture handoff"
            );
            self.validate_capture_handoff(
                &scope,
                target,
                &task.task_id,
                &task.context_hash,
                expected_contract_hash,
            )?;
            prepared.setup.capture_handoff = Some(target.clone());
        }
        if let Some(request_id) = proposal_request_id {
            let requests = self
                .user_requests
                .as_ref()
                .context("owner requests unavailable")?;
            let request = requests
                .pending_request_snapshot(
                    request_id,
                    Some(&scope.principal),
                    Some(&scope.workspace),
                )
                .await
                .context("proposal expired, dismissed or outside owner scope")?;
            let proposed = Self::proposal_from_request(request)?;
            anyhow::ensure!(
                proposed.proposal_hash == prepared.setup.proposal_hash,
                "proposal bytes changed; create and review a new proposal"
            );
            anyhow::ensure!(
                proposed.origin.agent_id == agent_id,
                "proposal agent does not match owning task"
            );
            prepared.setup.proposal_origin = Some(proposed.origin);
        }
        let owner_key = prepared.setup.admission_key();
        let owner_id = format!("relentless:{id}:{owner_key}");
        let task = if let Some(existing) = existing_task {
            let context = self.owner_task_context(&scope, &existing.task_id).await?;
            anyhow::ensure!(
                context.context_hash == existing.context_hash
                    && context.goal == proposal.goal
                    && context.agent_id == agent_id
                    && context.ui_thread_id == ui_thread_id
                    && proposal.native_harness.is_some(),
                "Reviewed task intent or native actor changed; review its current context"
            );
            if let Some(pending_id) = context.pending_run_id {
                anyhow::ensure!(pending_id == id
                    && context.pending_proposal_hash.as_deref() == Some(owner_key.as_str()),
                    "Interrupted task admission requires its original run ID and exact reviewed brief");
            }
            prepared.setup.task_policy_origin = Some(existing.clone());
            Box::pin(create_existing_task_direct_root_run_with_output_owner(
                &self.artifacts,
                &self.orchestrator,
                &scope.principal,
                &scope.workspace,
                &existing.task_id,
                &existing.context_hash,
                &owner_id,
                id,
            ))
            .await
        } else {
            Box::pin(create_task_backed_direct_root_run_with_output_owner(
                &self.artifacts,
                &self.orchestrator,
                &scope.principal,
                &scope.workspace,
                &ui_thread_id,
                Some(&proposal.goal),
                &proposal.goal,
                &agent_id,
                super::storage::TaskCreatedBy::User,
                false,
                guidance_origin.is_some_and(|origin| origin.is_trial()),
                &owner_id,
                id,
            ))
            .await
        };
        let (task_id, task_execution_id, runtime_execution_id) = match task {
            Ok(t) => t,
            Err(e) => return Err(anyhow::anyhow!(e)),
        };
        prepared.setup.task_execution_id = Some(task_execution_id);
        prepared.setup.runtime_execution_id = Some(runtime_execution_id);
        let s = store.clone();
        let owner = scope.principal.clone();
        match io(move || prepared.admit_until(&s, id, owner, task_id, guidance_deadline)).await {
            Ok(state) => {
                self.launch(scope, id);
                Ok((state, true))
            },
            Err(e) => Err(e),
        }
    }

    pub fn cancel_worker(&self, scope: &RunScope, id: Uuid) {
        if let Some(control) = self
            .controls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(scope.principal.clone(), scope.workspace.clone(), id))
        {
            control.cancel.cancel();
        }
    }
}

fn catalog_matches(query: &str, fields: &[&str]) -> bool {
    let haystack = fields.join(" ").to_lowercase();
    query
        .split_whitespace()
        .all(|term| haystack.contains(&term.to_lowercase()))
}
fn catalog_pagination(
    total: usize,
    page: &runtime_core::storage::PaginationParams,
) -> runtime_core::storage::PaginationInfo {
    runtime_core::storage::PaginationInfo {
        total,
        limit: page.limit,
        offset: page.offset,
        has_more: page.offset.saturating_add(page.limit) < total,
    }
}
