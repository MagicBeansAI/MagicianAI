//! Owner continuation on the existing run journal and durable request inbox.
use super::*;
use magician_relentless::budget_round::BudgetReset;

impl RelentlessService {
    pub(super) async fn budget_continuation_reason(
        &self,
        state: &RunState,
        setup: &TextDraftSetup,
    ) -> Result<Option<String>> {
        if setup
            .guidance_origin
            .as_ref()
            .is_some_and(|origin| origin.is_trial())
        {
            return Ok(Some(
                "Qualification trials retain their bounded evaluation policy".into(),
            ));
        }
        let Some(root) = setup.task_execution_id.as_deref() else {
            return Ok(Some("Run has no owning task execution".into()));
        };
        let scope = ScopeRef::system_internal_unauthenticated(
            &state.scope.principal,
            &state.scope.workspace,
        );
        let task = self
            .artifacts
            .get_task(&scope, &state.admission.task_id)
            .await?;
        if matches!(
            task.state.status.as_str(),
            "archived" | "cancelled" | "completed"
        ) || task.state.active_root_execution_id.as_deref() != Some(root)
            || task.state.latest_root_execution_id.as_deref() != Some(root)
        {
            return Ok(Some("This run no longer owns an active task; archived or superseded work cannot restart".into()));
        }
        let execution = self
            .artifacts
            .get_execution(&scope, &state.admission.task_id, root)
            .await?;
        if matches!(
            execution.state.status.as_str(),
            "failed" | "completed" | "cancelled"
        ) {
            return Ok(Some("This legacy task execution has already closed".into()));
        }
        if let Some(runtime) = &setup.runtime_execution_id {
            if matches!(
                self.orchestrator
                    .get_execution_status(runtime)
                    .await
                    .map_err(anyhow::Error::msg)?,
                WaitingState::Cancelled | WaitingState::Completed | WaitingState::Failed
            ) {
                return Ok(Some("The owning runtime execution has closed".into()));
            }
        }
        Ok(None)
    }

    pub async fn budget_continuation_view(&self, state: &RunState) -> Result<serde_json::Value> {
        let store = self.store(&state.scope);
        let id = state.id;
        let hash = state.admission.source_hash.clone();
        let setup: TextDraftSetup =
            io(move || Ok(serde_json::from_slice(&store.read_blob(id, &hash)?)?)).await?;
        let reason = self.budget_continuation_reason(state, &setup).await?;
        Ok(
            serde_json::json!({"available":state.status == RunStatus::BudgetExhausted && reason.is_none(),"reason":reason}),
        )
    }

    /// Verified owner endpoint only. Answers, reloads and actor proposals
    /// never enter this boundary. The exact reviewed generation fences leases.
    #[allow(clippy::too_many_arguments)]
    pub async fn approve_owner_budget_reset(
        &self,
        scope: RunScope,
        id: Uuid,
        reset_id: Uuid,
        expected_generation: u64,
        expected_contract_hash: String,
        reset: BudgetReset,
    ) -> Result<RunState> {
        let _guard = self.admission_lock.lock().await;
        let store = self.store(&scope);
        let reader = store.clone();
        let (state, setup) = io(move || {
            let state = reader.load(id)?;
            let setup: TextDraftSetup =
                serde_json::from_slice(&reader.read_blob(id, &state.admission.source_hash)?)?;
            Ok((state, setup))
        })
        .await?;
        // A lost HTTP response may retry the exact approval after productive
        // work has moved on. Never append another grant or reset its clock.
        if let Some(prior) = state
            .budget_resets
            .iter()
            .find(|grant| grant.id == reset_id)
        {
            anyhow::ensure!(
                prior.approved_by == scope.principal
                    && prior.reset == reset
                    && expected_contract_hash == state.contract.hash()?,
                "reset identity was already used for a different approval"
            );
            self.launch(scope, id);
            return Ok(state);
        }
        anyhow::ensure!(
            state.status == RunStatus::BudgetExhausted,
            "run is not waiting for budget continuation"
        );
        anyhow::ensure!(self.budget_continuation_reason(&state, &setup).await?.is_none(),
            "only the current active task can continue; closed, archived and qualification work cannot reopen");
        let principal = scope.principal.clone();
        let next = io(move || {
            store.commit(
                id,
                expected_generation
                    .checked_add(1)
                    .context("generation overflow")?,
                RunEvent::BudgetResetApproved {
                    id: reset_id,
                    approved_by: principal,
                    contract_hash: expected_contract_hash,
                    reset,
                },
                Utc::now(),
            )
        })
        .await?;
        self.launch(scope, id);
        Ok(next)
    }

    pub(super) async fn project_budget_review_notice(
        &self,
        state: &RunState,
        setup: &TextDraftSetup,
        awaiting: bool,
    ) -> Result<()> {
        use super::super::user_requests::{
            RequestOption, ScopedResponseResult, UserRequest, UserResponse,
        };
        let (Some(requests), Some(execution)) = (&self.user_requests, &setup.runtime_execution_id)
        else {
            return Ok(());
        };
        // Stable identity per stop. A dismissed/expired notice is never
        // re-created by polling, recovery or a repeated worker projection.
        for round in 0..=state.budget_resets.len() {
            let request_id = format!("{execution}:quality-budget:{}:{round}", state.id);
            if awaiting && round == state.budget_resets.len() {
                let limits = state
                    .budget_pause
                    .as_ref()
                    .context("budget stop context missing")?
                    .limits;
                let names = [
                    (limits.cost, "cost"),
                    (limits.time, "time"),
                    (limits.attempts, "attempts"),
                ]
                .into_iter()
                .filter_map(|(hit, name)| hit.then_some(name))
                .collect::<Vec<_>>()
                .join(", ");
                requests.submit_nonblocking_durable(UserRequest {
                    id:request_id,request_type:"relentless.budget_extension".into(),
                    question:format!("Work paused at its {names} allowance. Review whether to continue; approval resets only reached allowances and keeps cumulative usage. Dismissing this notice leaves work paused."),
                    options:vec![RequestOption{id:"dismiss".into(),label:"Dismiss notice".into(),requires_input:false}],
                    principal:state.scope.principal.clone(),workspace:state.scope.workspace.clone(),
                    context:serde_json::json!({"run_id":state.id,"task_id":state.admission.task_id,"budget_stop":round,
                        "review_href":format!("/work?view=quality&run={}&task={}",state.id,state.admission.task_id),"review_label":"Review continuation"}),
                    source:"relentless".into(),execution_id:Some(execution.clone()),task_id:Some(state.admission.task_id.clone()),
                    timeout_secs:604_800,default_on_timeout:"dismiss".into(),created_at:0,sensitive:None,
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
                            channel: "quality_budget_review".into(),
                            sensitive: vec![],
                        },
                        Some(&state.scope.principal),
                        Some(&state.scope.workspace),
                    )
                    .await;
                anyhow::ensure!(
                    !matches!(result, ScopedResponseResult::PersistenceUnavailable),
                    "budget notice resolution needs retry"
                );
            }
        }
        Ok(())
    }
}
