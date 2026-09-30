//! Native semantic calls use the same App operation dispatcher and physical
//! resource owner as reviewed behavior steps, with an additional node claim.
use super::*;
use crate::magician_v2::apps::{
    llm_dispatch::{
        resolve_recipe_step_operation, AppBehaviorLlmDispatchRequest, AppLlmDispatchBudget,
        AppLlmDispatchResult, AppLlmOperationDispatcher,
    },
    models::AppName,
    processing_boundary::admit_app_native_semantic_input,
    workflows::AppNativeSemanticInput,
};

struct AppRecipeModelClaimAuthorizer {
    reducer: Arc<dyn ArtifactV2Reducer>,
    scope: ScopeRef,
    task_id: String,
    execution_id: String,
    step_id: String,
    binding_digest: String,
    input_digest: String,
    claim_owner_id: String,
    claim_epoch: u64,
    deadline_at_ms: i64,
}

impl std::fmt::Debug for AppRecipeModelClaimAuthorizer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppRecipeModelClaimAuthorizer")
            .field("task_id", &self.task_id)
            .field("step_id", &self.step_id)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl magicllm::LlmDisclosureAuthorizer for AppRecipeModelClaimAuthorizer {
    async fn revalidate(
        &self,
        _profile: &str,
        _provider: &magicllm::LLMProviderKind,
        _model: &str,
        _api_base_url: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now();
        if now.timestamp_millis() >= self.deadline_at_ms {
            return Err("native recipe model claim expired".to_owned());
        }
        match self
            .reducer
            .reduce_app_recipe_step_claim_renewed(
                &self.scope,
                &self.task_id,
                &self.execution_id,
                &self.step_id,
                &self.binding_digest,
                &self.input_digest,
                &self.claim_owner_id,
                self.claim_epoch,
                now.timestamp_millis(),
                &now.to_rfc3339(),
            )
            .await
            .map_err(|_| "native recipe model claim could not be revalidated".to_owned())?
        {
            AppRecipeStepReducerAdmission::Reserved {
                claim_epoch,
                deadline_at_ms,
            } if claim_epoch == self.claim_epoch && deadline_at_ms >= self.deadline_at_ms => Ok(()),
            _ => Err("native recipe model claim is no longer active".to_owned()),
        }
    }
}

impl ArtifactV2Service {
    pub(super) async fn dispatch_native_recipe_semantic(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        run: &AppRecipeAttachedRun,
        node_id: &AppName,
        permit: &AppRecipeCanonicalNodePermit,
        input: AppNativeSemanticInput,
    ) -> Result<AppLlmDispatchResult, AppWorkflowError> {
        let result = async {
            self.fence_recipe_node_output(scope, task_id, run, node_id, permit)
                .await?;
            if input.task_id() != task_id
                || input.input().execution_ref().as_str() != run.execution_id()
                || !input.matches_node_claim(permit)
            {
                return Err(AppWorkflowError::CorruptBinding);
            }
            let router = self
                .orchestrator
                .operation_llm_router()
                .ok_or(AppWorkflowError::ProcessingContextUnavailable)?;
            let router_config = router
                .router_config_snapshot()
                .ok_or(AppWorkflowError::ProcessingContextUnavailable)?;
            let config_authority = self
                .app_llm_config_authority
                .read()
                .ok()
                .and_then(|slot| slot.clone())
                .ok_or(AppWorkflowError::ProcessingContextUnavailable)?;
            let app_workflows = self.app_workflow_service();
            let admitted = admit_app_native_semantic_input(
                app_workflows.registry_service(),
                &input,
                Arc::clone(&self.app_processing_trust_settings),
                &router_config,
            )
            .map_err(|error| {
                warn!(
                    task_id,
                    execution_id = run.execution_id(),
                    error_code = error.diagnostic_code(),
                    "native recipe semantic context refused"
                );
                AppWorkflowError::ProcessingContextUnavailable
            })?
            .with_additional_authorizer(Arc::new(AppRecipeModelClaimAuthorizer {
                reducer: Arc::clone(&self.reducer),
                scope: scope.clone(),
                task_id: task_id.to_owned(),
                execution_id: run.execution_id().to_owned(),
                step_id: permit.step_id().to_owned(),
                binding_digest: permit.binding_digest().to_string(),
                input_digest: permit.input_digest().to_string(),
                claim_owner_id: permit.claim_owner_id().to_owned(),
                claim_epoch: permit.claim_epoch(),
                deadline_at_ms: permit.deadline_at_ms(),
            }));
            app_workflows
                .record_model_admission(
                    scope,
                    task_id,
                    run.execution_id(),
                    input.agent_id(),
                    admitted.receipt().clone(),
                )
                .await?;
            let step = input.step();
            let operation = {
                let config = config_authority
                    .read()
                    .map_err(|_| AppWorkflowError::ProcessingContextUnavailable)?;
                let live_router = config
                    .router_config()
                    .ok_or(AppWorkflowError::ProcessingContextUnavailable)?;
                resolve_recipe_step_operation(
                    &config.app_platform,
                    live_router,
                    step.verified_manifest(),
                    step.operation(),
                )
                .map_err(|_| AppWorkflowError::ProcessingContextUnavailable)?
            };
            let budget = AppLlmDispatchBudget::from_behavior_authority(
                0,
                step.resources().max_causation_depth,
                step.remaining_output_tokens(),
            )
            .map_err(|_| AppWorkflowError::ProcessingContextUnavailable)?;
            let task_ref = magicllm::dispatch::TaskRef::task(task_id.to_owned())
                .with_agent(input.participant_id().to_owned())
                .with_scope(scope.principal().to_owned(), scope.workspace().to_owned())
                .with_execution(run.execution_id().to_owned(), run.execution_id().to_owned())
                .with_plan_step(None, Some(format!("{}:{}", node_id, step.step_id())));
            AppLlmOperationDispatcher::new(router, config_authority)
                .dispatch(AppBehaviorLlmDispatchRequest {
                    step,
                    operation: &operation,
                    admitted_context: &admitted,
                    task_ref,
                    budget,
                })
                .await
                .map_err(|error| {
                    warn!(
                        task_id,
                        execution_id = run.execution_id(),
                        error_code = error.diagnostic_code(),
                        "native recipe semantic dispatch failed"
                    );
                    AppWorkflowError::ProcessingContextUnavailable
                })
        }
        .await;
        // Protected App jobs drain through the physical settlement owner
        // before their queue receiver returns. A dropped/cancelled future does
        // not reach this marker and therefore cannot prove zero usage.
        input.mark_dispatch_completed();
        result
    }
}
