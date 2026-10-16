//! Relentless-backed procedural qualification. This is a service integration,
//! not a second harness: every trial owns an ordinary metered controller run.
use super::{io, Control, RelentlessService};
use crate::magician_v2::{
    artifact_v2::{workspace::scope_hosts_user_subsystems, ScopeRef, V3ReadApi},
    learning::{
        relentless_promotion::*, LearningProcedure, LearningProcedureStatus, LearningScope,
        LearningStore,
    },
    relentless::store::{RunEvent, RunScope, RunState, RunStatus, RunStore},
    relentless_runtime::{PreparedTextDraft, TextDraftProposal, TextDraftSetup},
    task_run_factory::task_policy_context_hash,
};
use anyhow::{ensure, Context, Result};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

impl RelentlessService {
    fn guidance_store(&self) -> GuidancePolicyStore {
        GuidancePolicyStore::new(self.workspace.clone())
    }

    pub async fn preview_guidance_policy(
        &self,
        scope: RunScope,
        id: Uuid,
        spec: GuidancePromotionSpec,
    ) -> Result<FrozenGuidancePolicy> {
        ensure!(
            scope_hosts_user_subsystems(&scope.principal, &scope.workspace),
            "owner scope cannot host guidance qualification"
        );
        let policy = FrozenGuidancePolicy::resolve(
            self.workspace.clone(),
            &self.router,
            scope.clone(),
            id,
            spec,
        )?;
        let run_store = self.store(&scope);
        let frozen = policy.clone();
        io(move || {
            let mut training = std::collections::BTreeSet::new();
            for id in &frozen.spec.training_run_ids {
                ensure!(training.insert(*id), "duplicate training run");
                let run = run_store.load(*id)?;
                ensure!(
                    run.status.terminal(),
                    "training feedback must come from a terminal run"
                );
                let setup: TextDraftSetup =
                    serde_json::from_slice(&run_store.read_blob(*id, &run.admission.source_hash)?)?;
                ensure!(
                    !setup
                        .guidance_origin
                        .as_ref()
                        .is_some_and(|origin| origin.is_trial()),
                    "held-out qualification cannot be used as training provenance"
                );
                let hash = input_hash(&run.evaluation_contract()?)?;
                ensure!(
                    !frozen
                        .evaluation
                        .iter()
                        .chain(&frozen.canary)
                        .any(|case| case.input_hash == hash),
                    "qualification inputs overlap declared training runs"
                );
            }
            Ok(())
        })
        .await?;
        let artifact_scope =
            ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
        for target in &policy.spec.targets {
            let task = self
                .artifacts
                .get_task(&artifact_scope, &target.task_id)
                .await?;
            let task_policy = task.manifest.relentless.as_ref().context(
                "automatic guidance targets need a reviewed recurring Relentless task policy",
            )?;
            ensure!(task.manifest.agent_id == policy.spec.agent_id
                && task_policy_context_hash(&task.manifest).map_err(anyhow::Error::msg)? == target.task_context_hash
                && task_policy.contract_hash == target.baseline_contract_hash
                && task_policy.capture_handoff.is_none()
                && task_policy.proposal.actor_guidance == policy.spec.baseline_guidance,
                "guidance target differs from its exact reviewed task context/baseline, or has a capture handoff");
            let baseline = PreparedTextDraft::resolve(&self.router, &task_policy.proposal)?;
            ensure!(
                baseline.contract.hash()? == target.baseline_contract_hash,
                "guidance target model/adapter/source pins changed"
            );
            ensure!(
                baseline.contract.definition().actor.profile
                    == policy.evaluation[0]
                        .baseline_contract
                        .definition()
                        .actor
                        .profile
                    && baseline
                        .contract
                        .definition()
                        .critics
                        .iter()
                        .any(|c| c.model.is_some()),
                "adoption requires the qualified actor profile and independent production review"
            );
            let harness_hash = digest(&baseline.setup.native_harness)?;
            for case in policy.evaluation.iter().chain(&policy.canary) {
                ensure!(digest(&case.baseline.native_harness)? == harness_hash
                    && case.baseline_contract.definition().artifact_kind == baseline.contract.definition().artifact_kind
                    && case.baseline_contract.definition().actor.model == baseline.contract.definition().actor.model
                    && case.baseline_contract.definition().actor.family == baseline.contract.definition().actor.family,
                    "qualification must use the target's native actor recipe, artifact kind and model/family");
            }
        }
        Ok(policy)
    }

    pub async fn activate_guidance_policy(
        &self,
        scope: RunScope,
        id: Uuid,
        spec: GuidancePromotionSpec,
        expected_version_hash: &str,
    ) -> Result<GuidancePolicyState> {
        let policy = self
            .preview_guidance_policy(scope.clone(), id, spec)
            .await?;
        ensure!(
            policy.version_hash == expected_version_hash,
            "reviewed guidance policy/candidate/model pins changed"
        );
        let store = self.guidance_store();
        let service = self.clone();
        io(move || {
            let state = store.activate(policy)?;
            service.launch_guidance_policy(scope, id);
            Ok(state)
        })
        .await
    }
    pub async fn owner_guidance_policies(
        &self,
        scope: &RunScope,
    ) -> Result<Vec<GuidancePolicyState>> {
        let store = self.guidance_store();
        let scope = scope.clone();
        io(move || store.list(&scope)).await
    }

    pub async fn pending_guidance_proposals(
        &self,
        scope: RunScope,
    ) -> Result<Vec<serde_json::Value>> {
        let layout = self.workspace.clone();
        io(move || {
            let store = LearningStore::new(layout);
            let scope = LearningScope::new(&scope.principal, &scope.workspace);
            let candidates = store.list_candidates(&scope, crate::magician_v2::learning::LearningCandidateFilters {
                candidate_type:Some("memory_procedure".into()), ..Default::default()
            })?;
            let mut proposals = Vec::new();
            for candidate in candidates {
                if candidate.state.is_terminal() || candidate.review_policy["kind"] != "relentless_guidance_proposal" {
                    continue;
                }
                let guidance = crate::magician_v2::learning::relentless_promotion::candidate_guidance(candidate.proposed_change.clone())?;
                guidance.validate()?;
                let training_run_ids = candidate.evidence_refs.iter().filter(|evidence| evidence.kind == "relentless_training_run")
                    .map(|evidence| Ok(Uuid::parse_str(evidence.id.as_deref().context("training run reference missing")?)?))
                    .collect::<Result<Vec<_>>>()?;
                proposals.push(json!({"candidate_id":candidate.id,"guidance":guidance,"training_run_ids":training_run_ids,
                    "agent_id":candidate.source_agent_id,"task_id":candidate.source_task_id,"execution_id":candidate.source_execution_id}));
            }
            Ok(proposals)
        }).await
    }

    pub async fn owner_guidance_evidence(
        &self,
        scope: RunScope,
        id: Uuid,
        run_id: Uuid,
    ) -> Result<serde_json::Value> {
        let store = self.guidance_store();
        let runs = self.store(&scope);
        io(move || {
            let state = store.load(&scope, id)?.context("guidance policy missing")?;
            let (hash, origin, contract_hash, version) =
                if let Some(observation) = state.observations.get(&run_id) {
                    let reservation = state
                        .reservations
                        .get(&run_id)
                        .context("trial evidence has no reservation")?;
                    (
                        observation.evidence_hash.clone(),
                        GuidanceRunOrigin::Trial {
                            policy_id: id,
                            version_hash: state.policy.version_hash.clone(),
                            phase: reservation.phase,
                            case_id: reservation.case_id.clone(),
                            variant: reservation.variant,
                        },
                        reservation.contract_hash.clone(),
                        "relentless-guidance-trial-evidence-v1",
                    )
                } else {
                    let observation = state
                        .monitored
                        .get(&run_id)
                        .context("sealed guidance evidence is not available yet")?;
                    let reservation = state
                        .adoptions
                        .get(&run_id)
                        .context("monitor evidence has no adoption")?;
                    (
                        observation.evidence_hash.clone(),
                        reservation.origin.clone(),
                        reservation.guided_contract_hash.clone(),
                        "relentless-guidance-monitor-v1",
                    )
                };
            // read_blob verifies the selected content hash; callers cannot
            // supply a free-form blob hash or cross-policy run reference.
            let evidence: serde_json::Value =
                serde_json::from_slice(&runs.read_blob(run_id, &hash)?)?;
            let actual_origin: GuidanceRunOrigin =
                serde_json::from_value(evidence["origin"].clone())?;
            let contract: magician_relentless::contract::FrozenContract =
                serde_json::from_value(evidence["run"]["contract"].clone())?;
            ensure!(
                evidence["version"] == version
                    && actual_origin == origin
                    && evidence["run"]["id"] == run_id.to_string()
                    && evidence["run"]["scope"] == serde_json::to_value(&scope)?
                    && contract.hash()? == contract_hash,
                "sealed guidance evidence lost its policy/run/scope/contract binding"
            );
            Ok(json!({"evidence":evidence,"content_hash":hash,"sealed":true}))
        })
        .await
    }
    pub async fn revoke_guidance_policy(
        &self,
        scope: RunScope,
        id: Uuid,
        version: String,
    ) -> Result<GuidancePolicyState> {
        let store = self.guidance_store();
        let s = scope.clone();
        let service = self.clone();
        let state = io(move || {
            let state = store.revoke(&s, id, &version)?;
            service.launch_guidance_policy(scope, id);
            Ok(state)
        })
        .await?;
        self.stop_guidance_trials(&state).await?;
        self.sync_guidance_procedure(&state).await?;
        Ok(state)
    }
    pub async fn resume_guidance_policy(
        &self,
        scope: RunScope,
        id: Uuid,
        version: String,
    ) -> Result<GuidancePolicyState> {
        let store = self.guidance_store();
        let s = scope.clone();
        let service = self.clone();
        io(move || {
            let state = store.resume(&s, id, &version)?;
            service.launch_guidance_policy(scope, id);
            Ok(state)
        })
        .await
    }
    pub(super) async fn recover_guidance_policies(&self, scope: &RunScope) -> Result<()> {
        let store = self.guidance_store();
        let s = scope.clone();
        let ids = io(move || store.list_ids(&s)).await?;
        let mut failures = 0usize;
        for id in ids {
            let store = self.guidance_store();
            let s = scope.clone();
            let state = match io(move || store.load(&s, id)?.context("guidance policy missing"))
                .await
            {
                Ok(state) => state,
                Err(error) => {
                    failures += 1;
                    tracing::warn!(?scope, policy_id=%id, %error, "Relentless damaged guidance policy remains blocked");
                    continue;
                },
            };
            if state.status.phase().is_none() || state.policy.spec.expires_at <= Utc::now() {
                if let Err(error) = self.stop_guidance_trials(&state).await {
                    failures += 1;
                    tracing::warn!(policy_id=%id, %error, "Relentless guidance trial cleanup remains pending");
                }
            }
            self.launch_guidance_policy(scope.clone(), state.policy.id);
        }
        ensure!(
            failures == 0,
            "{failures} guidance records/cleanup operations remain blocked"
        );
        Ok(())
    }

    pub(super) fn validate_guidance_trial_admission(
        &self,
        scope: &RunScope,
        run_id: Uuid,
        proposal: &TextDraftProposal,
        contract_hash: &str,
        origin: &GuidanceRunOrigin,
    ) -> Result<chrono::DateTime<Utc>> {
        let GuidanceRunOrigin::Trial {
            policy_id,
            version_hash,
            phase,
            case_id,
            variant,
        } = origin
        else {
            anyhow::bail!("one-off admission cannot invent a production adoption")
        };
        let policy = self
            .guidance_store()
            .load(scope, *policy_id)?
            .context("trial policy missing")?;
        let reservation = policy
            .reservations
            .get(&run_id)
            .context("trial budget was not reserved before task exposure")?;
        ensure!(
            policy.policy.version_hash == *version_hash
                && policy.status.phase() == Some(*phase)
                && policy.policy.spec.expires_at > Utc::now()
                && reservation.case_id == *case_id
                && reservation.variant == *variant
                && reservation.contract_hash == contract_hash
                && reservation.proposal_hash == digest(proposal)?,
            "trial differs from its active owner policy/reservation"
        );
        Ok(policy.policy.spec.expires_at)
    }

    pub(super) fn guidance_trial_is_active(
        &self,
        run: &RunState,
        setup: &TextDraftSetup,
    ) -> Result<bool> {
        let Some(GuidanceRunOrigin::Trial {
            policy_id,
            version_hash,
            phase,
            case_id,
            variant,
        }) = &setup.guidance_origin
        else {
            return Ok(true);
        };
        let Some(policy) = self.guidance_store().load(&run.scope, *policy_id)? else {
            return Ok(false);
        };
        let reservation = policy
            .reservations
            .get(&run.id)
            .context("trial lost its durable reservation")?;
        ensure!(
            policy.policy.version_hash == *version_hash
                && policy.policy.trial_id(*phase, case_id, *variant)? == run.id
                && reservation.phase == *phase
                && reservation.case_id == *case_id
                && reservation.variant == *variant
                && reservation.contract_hash == run.contract.hash()?
                && reservation.proposal_hash == setup.proposal_hash,
            "recovering trial differs from its frozen policy/reservation"
        );
        ensure!(
            run.admission.not_after == Some(policy.policy.spec.expires_at),
            "trial lost its absolute parent policy deadline"
        );
        Ok(policy.status.phase() == Some(*phase) && policy.policy.spec.expires_at > Utc::now())
    }

    async fn stop_guidance_trials(&self, state: &GuidancePolicyState) -> Result<()> {
        let mut failures = 0usize;
        for run_id in state.reservations.keys() {
            if let Err(error) = self
                .cancel_internal_trial(&state.policy.scope, *run_id)
                .await
            {
                failures += 1;
                tracing::warn!(policy_id=%state.policy.id, %run_id, %error, "Guidance trial cancellation journal remains blocked");
            }
        }
        ensure!(
            failures == 0,
            "{failures} guidance trial cancellation journals remain blocked"
        );
        Ok(())
    }

    fn launch_guidance_policy(&self, scope: RunScope, id: Uuid) {
        let key = (scope.principal.clone(), scope.workspace.clone(), id);
        let control = Control::default();
        {
            let mut controls = self
                .guidance_controls
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(existing) = controls.get(&key) {
                existing.notify();
                return;
            }
            controls.insert(key.clone(), control.clone());
        }
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.run_guidance_policy(&scope, id, &control).await {
                tracing::warn!(policy_id=%id, %error, "Guidance qualification stopped; frozen journal/reservations retained");
                let store = service.guidance_store();
                let s = scope.clone();
                let reason = format!("Qualification needs attention: {error:#}");
                let paused = io(move || {
                    let state = store.load(&s, id)?.context("guidance policy missing")?;
                    if state.status.phase().is_some()
                        || state.status == GuidancePolicyStatus::Qualified
                    {
                        store.pause(&s, id, &state.policy.version_hash, reason)
                    } else if state.status == GuidancePolicyStatus::Promoted {
                        store.disable(&s, id, &state.policy.version_hash, reason)
                    } else {
                        Ok(state)
                    }
                })
                .await;
                if let Ok(state) = paused {
                    let _ = service.stop_guidance_trials(&state).await;
                    // Future admission is already disabled by the journal.
                    // Best-effort archival never overwrites edited guidance.
                    let _ = service.sync_guidance_procedure(&state).await;
                    let _ = service.project_guidance_notice(&state).await;
                }
            }
            service
                .guidance_controls
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&key);
            let store = service.guidance_store();
            let s = scope.clone();
            if io(move || {
                Ok(store.load(&s, id)?.is_some_and(|state| {
                    state.status.phase().is_some()
                        || state.status == GuidancePolicyStatus::Qualified
                }))
            })
            .await
            .unwrap_or(false)
            {
                // An owner resume arriving while the old worker exits must
                // retain its wake rather than leave a running journal idle.
                service.launch_guidance_policy(scope, id);
            }
        });
    }

    async fn run_guidance_policy(
        &self,
        scope: &RunScope,
        id: Uuid,
        control: &Control,
    ) -> Result<()> {
        'policy: loop {
            let policy_store = self.guidance_store();
            let s = scope.clone();
            let state = io(move || {
                policy_store
                    .load(&s, id)?
                    .context("guidance policy missing")
            })
            .await?;
            if state.policy.spec.expires_at <= Utc::now()
                && (state.status.phase().is_some()
                    || state.status == GuidancePolicyStatus::Qualified)
            {
                let store = self.guidance_store();
                let s = scope.clone();
                let version = state.policy.version_hash.clone();
                let state = io(move || {
                    store.pause(
                        &s,
                        id,
                        &version,
                        "Owner qualification policy expired".into(),
                    )
                })
                .await?;
                self.stop_guidance_trials(&state).await?;
                self.project_guidance_notice(&state).await?;
                return Ok(());
            }
            if let Some(phase) = state.status.phase() {
                // Alternate pair order deterministically to avoid always giving
                // one variant the first provider call. No fixture-specific logic.
                for (index, case) in state.policy.cases(phase).iter().enumerate() {
                    let variants = if index % 2 == 0 {
                        [GuidanceVariant::Baseline, GuidanceVariant::Candidate]
                    } else {
                        [GuidanceVariant::Candidate, GuidanceVariant::Baseline]
                    };
                    for variant in variants {
                        let run_id = state.policy.trial_id(phase, &case.id, variant)?;
                        if state.observations.contains_key(&run_id) {
                            continue;
                        }
                        let reservation = TrialReservation {
                            run_id,
                            phase,
                            case_id: case.id.clone(),
                            variant,
                            contract_hash: case.contract(variant).hash()?,
                            proposal_hash: digest(case.proposal(variant))?,
                            maximum_cost_microusd: case.proposal(variant).budget.max_cost_microusd,
                        };
                        let store = self.guidance_store();
                        let s = scope.clone();
                        let version = state.policy.version_hash.clone();
                        let current =
                            io(move || store.reserve(&s, id, &version, reservation)).await?;
                        if current.status.phase() != Some(phase) {
                            continue 'policy;
                        }
                        let origin = GuidanceRunOrigin::Trial {
                            policy_id: id,
                            version_hash: state.policy.version_hash.clone(),
                            phase,
                            case_id: case.id.clone(),
                            variant,
                        };
                        self.start_text_draft_with_origin(
                            scope.clone(),
                            run_id,
                            case.proposal(variant),
                            &state.policy.spec.ui_thread_id,
                            &state.policy.spec.agent_id,
                            &case.contract(variant).hash()?,
                            None,
                            None,
                            None,
                            Some(&origin),
                        )
                        .await?;
                        self.await_guidance_trial(&state.policy, run_id, control)
                            .await?;
                    }
                    let store = self.guidance_store();
                    let s = scope.clone();
                    let version = state.policy.version_hash.clone();
                    let next = io(move || {
                        let current = store.load(&s, id)?.context("guidance policy missing")?;
                        if current.status.phase() == Some(phase)
                            && current.assess(phase)?.is_some_and(|(passed, _)| !passed)
                        {
                            store.decide(&s, id, &version, phase)
                        } else {
                            Ok(current)
                        }
                    })
                    .await?;
                    if next.status.phase() != Some(phase) {
                        // A qualified must-pass defect cannot be rescued by
                        // spending on remaining cases of this frozen candidate.
                        continue 'policy;
                    }
                }
                let store = self.guidance_store();
                let s = scope.clone();
                let version = state.policy.version_hash.clone();
                io(move || store.decide(&s, id, &version, phase)).await?;
                continue;
            }
            if state.status == GuidancePolicyStatus::Qualified {
                self.sync_guidance_procedure(&state).await?;
                let store = self.guidance_store();
                let s = scope.clone();
                let version = state.policy.version_hash.clone();
                io(move || store.promote(&s, id, &version)).await?;
                continue;
            }
            if state.status == GuidancePolicyStatus::Promoted {
                if state.policy.spec.expires_at <= Utc::now() {
                    let store = self.guidance_store();
                    let s = scope.clone();
                    let version = state.policy.version_hash.clone();
                    io(move || store.expire(&s, id, &version)).await?;
                    continue;
                }
                self.sync_guidance_procedure(&state).await?;
                self.project_guidance_notice(&state).await?;
                // The owner expiry/revocation timer dispatches no models and
                // lives with the same recoverable policy worker.
                tokio::select! { _ = control.wake.notified() => {}, _ = control.cancel.cancelled() => {},
                _ = tokio::time::sleep((state.policy.spec.expires_at - Utc::now()).to_std().unwrap_or_default()) => {} }
                continue;
            }
            self.sync_guidance_procedure(&state).await?;
            self.stop_guidance_trials(&state).await?;
            self.project_guidance_notice(&state).await?;
            return Ok(());
        }
    }
    async fn await_guidance_trial(
        &self,
        policy: &FrozenGuidancePolicy,
        run_id: Uuid,
        control: &Control,
    ) -> Result<()> {
        loop {
            let store = self.guidance_store();
            let scope = policy.scope.clone();
            let id = policy.id;
            let current =
                io(move || store.load(&scope, id)?.context("guidance policy missing")).await?;
            if current.status.phase().is_none()
                || current.policy.spec.expires_at <= Utc::now()
                || control.cancel.is_cancelled()
            {
                self.cancel_internal_trial(&policy.scope, run_id).await?;
                anyhow::bail!("Owner qualification revoked, paused or expired");
            }
            let store = self.store(&policy.scope);
            let read = store.clone();
            let run = io(move || read.load(run_id)).await?;
            if run.status == magician_relentless::store::RunStatus::WaitingOwner {
                self.cancel_internal_trial(&policy.scope, run_id).await?;
                anyhow::bail!("Internal trial could not finish without additional input; no qualification pass");
            }
            if run.status.terminal() && run.lease.is_none() {
                let read = store.clone();
                let source_hash = run.admission.source_hash.clone();
                let setup: TextDraftSetup = io(move || {
                    Ok(serde_json::from_slice(
                        &read.read_blob(run_id, &source_hash)?,
                    )?)
                })
                .await?;
                ensure!(
                    self.project_guidance_trial_receipt(store, &run, &setup)
                        .await?,
                    "trial has unknown physical usage; full policy ceiling remains reserved"
                );
                return Ok(());
            }
            tokio::select! { _ = control.wake.notified() => {}, _ = control.cancel.cancelled() => {},
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {} }
        }
    }

    /// Receipt projection is non-productive and independent of qualification
    /// status. Revocation must not hide known costs or refund unknown usage.
    pub(super) async fn project_guidance_trial_receipt(
        &self,
        run_store: Arc<RunStore>,
        run: &RunState,
        setup: &TextDraftSetup,
    ) -> Result<bool> {
        let Some(GuidanceRunOrigin::Trial {
            policy_id,
            version_hash,
            phase,
            case_id,
            variant,
        }) = setup.guidance_origin.clone()
        else {
            return Ok(true);
        };
        if !run.status.terminal() || run.lease.is_some() || has_unknown_usage(run)? {
            return Ok(false);
        }
        let operator_redirects = if setup.native_harness.is_some() {
            let execution = setup
                .runtime_execution_id
                .as_deref()
                .context("native trial has no root binding")?;
            crate::magician_v2::execution::agentic::run_loop::steer_inbox::accepted_redirect_count(
                self.orchestrator.durable_artifact_workspace(),
                &run.scope.principal,
                &run.scope.workspace,
                execution,
            )
            .await
            .map_err(anyhow::Error::msg)?
        } else {
            0
        };
        let policy_store = self.guidance_store();
        let scope = run.scope.clone();
        let run_id = run.id;
        io(move || {
            let policy = policy_store
                .load(&scope, policy_id)?
                .context("trial policy missing")?;
            if policy.observations.contains_key(&run_id) {
                return Ok(true);
            }
            let run = run_store.load(run_id)?;
            let spend = run.budget.spend()?;
            if !run.status.terminal() || run.lease.is_some() || has_unknown_usage(&run)? {
                return Ok(false);
            }
            let reservation = policy
                .reservations
                .get(&run_id)
                .context("trial reservation missing")?;
            let setup: TextDraftSetup =
                serde_json::from_slice(&run_store.read_blob(run_id, &run.admission.source_hash)?)?;
            let origin = GuidanceRunOrigin::Trial {
                policy_id,
                version_hash: version_hash.clone(),
                phase,
                case_id,
                variant,
            };
            ensure!(
                policy.policy.version_hash == version_hash
                    && policy
                        .policy
                        .trial_id(phase, &reservation.case_id, variant)?
                        == run_id
                    && run.contract.hash()? == reservation.contract_hash
                    && setup.proposal_hash == reservation.proposal_hash
                    && run.source_epoch == 0
                    && setup.guidance_origin.as_ref() == Some(&origin),
                "trial evidence is outside its exact frozen policy"
            );
            let review = run.retained.artifact().or_else(|| {
                run.attempts
                    .last()
                    .and_then(|attempt| attempt.review.as_ref())
            });
            let evidence_hash = run_store.put_blob(
                run_id,
                &serde_json::to_vec(&json!({
                "version":"relentless-guidance-trial-evidence-v1", "policy_id":policy_id,
                "run":run, "origin":origin, "operator_redirects":operator_redirects}))?,
            )?;
            let observation = TrialObservation {
                run_id,
                contract_hash: run.contract.hash()?,
                source_epoch: run.source_epoch,
                status: run.status,
                complete: review.is_some_and(|r| r.complete),
                feasible: review.is_some_and(|r| r.feasible),
                quality: review.and_then(|r| r.quality_score),
                accepted: run.status == magician_relentless::store::RunStatus::Accepted,
                operator_redirects,
                actual_cost_microusd: spend.settled_microusd,
                evidence_hash,
            };
            policy_store.observe(&scope, policy_id, &version_hash, observation)?;
            Ok(true)
        })
        .await
    }

    async fn cancel_internal_trial(&self, scope: &RunScope, id: Uuid) -> Result<()> {
        // Physical cancellation must not depend on reading a damaged journal.
        self.cancel_worker(scope, id);
        let store = self.store(scope);
        io(move || {
            if !store.has_admission(id)? {
                // Reservation precedes the owning root and admission. Later
                // admission/recovery checks still refuse a disabled policy.
                return Ok(());
            }
            let state = store.load(id)?;
            if !state.status.terminal() {
                store.commit(id, state.generation + 1, RunEvent::Cancelled, Utc::now())?;
            }
            Ok(())
        })
        .await?;
        Ok(())
    }

    async fn sync_guidance_procedure(&self, state: &GuidancePolicyState) -> Result<()> {
        let workspace = self.workspace.clone();
        let state = state.clone();
        io(move || {
            let store = LearningStore::new(workspace.clone());
            crate::magician_v2::learning::LearningProcedureBridge::new(workspace)
                .sync_relentless_policy(&store, &state)
        })
        .await
    }

    /// Called synchronously while an ordinary task root is being prepared.
    /// Reservation precedes root exposure; interrupted roots retain their slot.
    pub(super) fn apply_qualified_guidance(
        &self,
        scope: &RunScope,
        task_id: &str,
        context_hash: &str,
        run_id: Uuid,
        proposal: &TextDraftProposal,
        prepared: &mut PreparedTextDraft,
    ) -> Result<()> {
        let store = self.guidance_store();
        let baseline_hash = prepared.contract.hash()?;
        let states = store.list(scope)?;
        ensure!(!states.iter().any(|state| state.adoptions.contains_key(&run_id)
            && (state.status != GuidancePolicyStatus::Promoted || state.policy.spec.expires_at <= Utc::now())),
            "interrupted guidance reservation was revoked/disabled; its root cannot be freshly exposed");
        let eligible: Vec<_> = states
            .into_iter()
            .filter(|state| {
                (state.adoption_enabled() || state.adoptions.contains_key(&run_id))
                    && state.policy.spec.targets.iter().any(|target| {
                        target.task_id == task_id
                            && target.task_context_hash == context_hash
                            && target.baseline_contract_hash == baseline_hash
                    })
            })
            .collect();
        ensure!(
            eligible.len() <= 1,
            "multiple active guidance policies cover this task; revoke the overlap"
        );
        let Some(state) = eligible.into_iter().next() else {
            return Ok(());
        };
        let validate_projection = || -> Result<()> {
            let learning = LearningStore::new(self.workspace.clone());
            let learning_scope = LearningScope::new(&scope.principal, &scope.workspace);
            let candidate =
                learning.read_candidate(&learning_scope, &state.policy.spec.candidate_id)?;
            ensure!(
                digest(&candidate.proposed_change)? == state.policy.candidate_proposal_hash
                    && (!candidate.state.is_terminal()
                        || candidate.state
                            == crate::magician_v2::learning::LearningCandidateState::Promoted),
                "qualified candidate was changed, rejected or retired; review a successor policy"
            );
            let procedure =
                learning.read_procedure(&learning_scope, &state.policy.procedure_id())?;
            ensure!(
                procedure.status == LearningProcedureStatus::Active
                    && procedure_guidance(&procedure) == state.policy.guidance
                    && procedure.payload == state.procedure_payload()?
                    && procedure.owner_agent.as_deref()
                        == Some(state.policy.spec.agent_id.as_str())
                    && procedure.source_candidate_id.as_deref()
                        == Some(state.policy.spec.candidate_id.as_str()),
                "qualified guidance procedure is not active or its pinned body/provenance changed"
            );
            Ok(())
        };
        if let Err(error) = validate_projection() {
            // A new root can use its independently reviewed baseline after
            // guidance is disabled. A reserved root cannot change identity.
            if state.adoptions.contains_key(&run_id) {
                return Err(
                    error.context("interrupted guided root requires its exact admission snapshot")
                );
            }
            let latest = store
                .load(scope, state.policy.id)?
                .context("guidance policy missing")?;
            if latest.status == GuidancePolicyStatus::Promoted {
                store.disable(
                    scope,
                    state.policy.id,
                    &state.policy.version_hash,
                    format!("Qualified guidance is unavailable: {error:#}"),
                )?;
            }
            self.launch_guidance_policy(scope.clone(), state.policy.id);
            tracing::warn!(policy_id=%state.policy.id, %error,
                "Disabled unavailable guidance; ordinary task retains its reviewed baseline");
            return Ok(());
        }
        let mut guided = proposal.clone();
        guided.actor_guidance = Some(state.policy.guidance.clone());
        let mut resolved = PreparedTextDraft::resolve(&self.router, &guided)?;
        let origin = GuidanceRunOrigin::Adoption {
            policy_id: state.policy.id,
            version_hash: state.policy.version_hash.clone(),
            task_id: task_id.into(),
            baseline_contract_hash: baseline_hash,
        };
        store.adopt(
            scope,
            state.policy.id,
            &state.policy.version_hash,
            AdoptionReservation {
                run_id,
                origin: origin.clone(),
                guided_contract_hash: resolved.contract.hash()?,
            },
        )?;
        resolved.setup.guidance_origin = Some(origin);
        *prepared = resolved;
        Ok(())
    }

    pub(super) async fn project_guidance_monitor(
        &self,
        run_store: Arc<RunStore>,
        run: &RunState,
        setup: &TextDraftSetup,
    ) -> Result<()> {
        let Some(GuidanceRunOrigin::Adoption {
            policy_id,
            version_hash,
            task_id,
            ..
        }) = &setup.guidance_origin
        else {
            return Ok(());
        };
        if !run.status.terminal() || run.status == RunStatus::BudgetExhausted {
            return Ok(());
        }
        let policy_store = self.guidance_store();
        let scope = run.scope.clone();
        let id = *policy_id;
        let version = version_hash.clone();
        let task_id = task_id.clone();
        let snapshot = run.clone();
        let state = io(move || {
            let policy = policy_store.load(&scope, id)?.context("adoption policy missing")?;
            if policy.monitored.contains_key(&snapshot.id) { return Ok(policy) }
            let admission = policy.adoptions.get(&snapshot.id).context("adoption has no reserved slot")?;
            ensure!(policy.policy.version_hash == version && admission.guided_contract_hash == snapshot.contract.hash()?,
                "monitored run lost its qualified guidance/contract binding");
            let target = policy.policy.spec.targets.iter().find(|t| t.task_id == task_id).context("monitored target missing")?;
            let review = snapshot.retained.artifact().or_else(|| snapshot.attempts.last().and_then(|a| a.review.as_ref()));
            let unknown = has_unknown_usage(&snapshot)? || !review.is_some_and(|r| r.complete)
                || target.minimum_quality.is_some() && review.and_then(|r| r.quality_score).is_none();
            let regressed = policy.policy.spec.monitor_nonacceptance_is_regression
                && snapshot.status != magician_relentless::store::RunStatus::Accepted
                || review.is_some_and(|r| r.complete && !r.feasible)
                || target.minimum_quality.zip(review.and_then(|r| r.quality_score)).is_some_and(|(floor, score)| score < floor);
            let evidence_hash = run_store.put_blob(snapshot.id, &serde_json::to_vec(&json!({
                "version":"relentless-guidance-monitor-v1","run":snapshot,"origin":admission.origin,"target":target}))?)?;
            policy_store.monitor(&scope, id, &version, AdoptionObservation { run_id:snapshot.id, regressed, unknown, evidence_hash })
        }).await?;
        self.sync_guidance_procedure(&state).await?;
        if state.status == GuidancePolicyStatus::Regressed {
            self.project_guidance_notice(&state).await?;
        }
        Ok(())
    }

    async fn project_guidance_notice(&self, state: &GuidancePolicyState) -> Result<()> {
        if state.status.phase().is_some()
            || matches!(
                state.status,
                GuidancePolicyStatus::Qualified | GuidancePolicyStatus::Expired
            )
        {
            return Ok(());
        }
        let Some(requests) = &self.user_requests else {
            return Ok(());
        };
        use crate::magician_v2::user_requests::{RequestOption, UserRequest};
        let notice_id =
            format!("relentless-guidance-{}-{:?}", state.policy.id, state.status).to_lowercase();
        requests.submit_nonblocking_durable(UserRequest {
            id:notice_id, request_type:"relentless.guidance_policy_status".into(),
            question:format!("Guidance policy {} is {:?}. {} Inspect it in /relentless; dismissing this notice changes no policy.",
                state.policy.spec.name, state.status, state.reason.as_deref().unwrap_or("Held-out evaluation and canary passed.")),
            options:vec![RequestOption { id:"dismiss".into(), label:"Dismiss notice".into(), requires_input:false }],
            principal:state.policy.scope.principal.clone(), workspace:state.policy.scope.workspace.clone(),
            context:json!({"policy_id":state.policy.id,"version_hash":state.policy.version_hash,"status":state.status}),
            source:"relentless".into(), execution_id:None, task_id:None,
            timeout_secs:86_400, default_on_timeout:"dismiss".into(), created_at:0, sensitive:None,
        }).await?;
        Ok(())
    }
}
fn has_unknown_usage(run: &RunState) -> Result<bool> {
    use magician_relentless::budget::Settlement;
    Ok(run.budget.spend()?.reserved_microusd > 0
        || run.budget.operations().values().any(|operation| {
            matches!(
                operation.settlement,
                Settlement::Reserved | Settlement::OutcomeUnknown
            )
        }))
}
fn procedure_guidance(
    procedure: &LearningProcedure,
) -> magician_relentless::contract::ProceduralGuidance {
    magician_relentless::contract::ProceduralGuidance {
        title: procedure.title.clone(),
        workflow: procedure.workflow.clone(),
        decision_points: procedure.decision_points.clone(),
        verification: procedure.verification.clone(),
        failure_modes: procedure.failure_modes.clone(),
    }
}
