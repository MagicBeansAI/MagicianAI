//! Native semantic input resolved from this execution's protected checkpoints.
//! No wire value can construct this admission or append bytes after disclosure.
use super::*;

/// A read result and a projection inside it. This is runtime-only data; the
/// checkpoint must be found byte-for-byte in the exact execution before use.
pub(super) struct AppNativeContextProjection {
    pub record: AppLabeledToolResultRecord,
    pub pointer: String,
}

/// Owns the exact derived envelope, retained source evidence and reviewed
/// semantic step. It deliberately implements neither Debug nor Serialize.
pub struct AppNativeSemanticInput {
    input: AppWorkflowModelInput,
    step: AppAdmittedRecipeStep,
    records: Vec<AppLabeledToolResultRecord>,
    prepared_at: DateTime<Utc>,
    node_binding_digest: AppDigest,
    claim_owner_id: String,
    claim_epoch: u64,
    participant_id: String,
    completion: workflow_rounds::AppNativeModelCompletion,
    resource_owner: Arc<dyn LlmPhysicalResourceAuthorizer>,
}

impl AppNativeSemanticInput {
    pub(crate) fn input(&self) -> &AppWorkflowModelInput {
        &self.input
    }
    pub(crate) fn step(&self) -> &AppAdmittedRecipeStep {
        &self.step
    }
    pub(crate) fn records(&self) -> &[AppLabeledToolResultRecord] {
        &self.records
    }
    pub(crate) fn prepared_at(&self) -> DateTime<Utc> {
        self.prepared_at
    }
    pub(crate) fn task_id(&self) -> &str {
        &self.input.llm_resource_owner.task_id
    }
    pub(crate) fn agent_id(&self) -> &str {
        &self.input.llm_resource_owner.agent_id
    }
    pub(super) fn completion(&self) -> workflow_rounds::AppNativeModelCompletion {
        self.completion.clone()
    }
    pub(crate) fn mark_dispatch_completed(&self) {
        self.completion.mark_finished();
    }
    pub(crate) fn participant_id(&self) -> &str {
        &self.participant_id
    }
    pub(crate) fn resource_owner(&self) -> Arc<dyn LlmPhysicalResourceAuthorizer> {
        Arc::clone(&self.resource_owner)
    }
    pub(crate) fn matches_node_claim(&self, permit: &AppRecipeCanonicalNodePermit) -> bool {
        permit.binding_digest() == &self.node_binding_digest
            && permit.claim_owner_id() == self.claim_owner_id
            && permit.claim_epoch() == self.claim_epoch
    }
}

impl AppWorkflowService {
    /// Resolve once for the whole context, rather than reopening/authenticating
    /// the task, manifest and authority once per source record. The caller's
    /// canonical node claim is independently checked before this preparation.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn prepare_native_semantic_input(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &workflow_commits::AppNativeProgressContext<'_>,
        step_id: &AppName,
        remaining_output_tokens: u32,
        claim: &workflow_rounds::AppNativeRoundModelClaim,
    ) -> Result<AppNativeSemanticInput, AppWorkflowError> {
        context.fence(scope, &task.task_id).await?;
        let node = context
            .run
            .plan()
            .node(context.node)
            .ok_or(AppWorkflowError::RecipeBindingUnavailable)?;
        if !node.permits_workflow_mutation()
            || remaining_output_tokens == 0
            || claim.node_binding_digest() != node.binding_digest()
            || u64::from(remaining_output_tokens) > claim.model().maximum_usage.tokens
        {
            return Err(AppWorkflowError::RecipeBindingUnavailable);
        }
        let now = Utc::now();
        let state = self
            .require_run_state(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        let projections = self.native_round_context(&state, claim)?;
        let mut input = self
            .model_input_from_state(
                scope,
                &task.task_id,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
                task,
                &state,
                now,
            )
            .await?;
        if !matches!(input.recipe_admission(), AppRecipeAdmission::Deterministic) {
            return Err(AppWorkflowError::RecipeBindingUnavailable);
        }
        let material = self
            .immutable_workflow_material(input.authenticated_scope(), task, now)
            .await?;
        let manifest = material.staged.candidate().manifest().manifest();
        let step = native_semantic_step(task, manifest, step_id, remaining_output_tokens)?;
        let mut records = Vec::new();
        let mut values = serde_json::Map::new();
        let mut sources = Vec::new();
        let mut labels = input.input.handling_labels.clone();
        let mut context_bytes = 0usize;
        for (name, projection) in &projections {
            let record = &projection.record;
            let checkpoint = record.checkpoint();
            record.validate_exact_bytes()?;
            if projection.pointer.len() > 4096
                || (!projection.pointer.is_empty() && !projection.pointer.starts_with('/'))
                || checkpoint.execution_ref().as_str() != context.run.execution_id()
                || !state.labeled_tool_results.iter().any(|stored| {
                    stored.checkpoint() == checkpoint
                        && json_values_equal_iteratively(stored.value(), record.value())
                })
            {
                return Err(AppWorkflowError::ToolResultCheckpointMismatch);
            }
            let store_query =
                checkpoint.tool_ref().as_str() == format!("capability:{APP_STORE_QUERY_TOOL}");
            if checkpoint.authority_digest() != &input.authority().authority_digest
                || (!store_query
                    && (!input.authority().permits_tool(checkpoint.tool_ref())
                        || !input
                            .procedure_invocation()
                            .effective_tools()
                            .contains(checkpoint.tool_ref())))
            {
                return Err(AppWorkflowError::ToolResultContinuationDenied);
            }
            if !store_query
                && material
                    .package_lock
                    .capability(checkpoint.tool_ref())
                    .and_then(|dependency| dependency.primitive_binding())
                    .is_some()
                && (checkpoint.effect_binding_digest().is_none()
                    || checkpoint.raw_effect_result_digest().is_none()
                    || checkpoint.raw_effect_result_bytes().is_none())
            {
                return Err(AppWorkflowError::ToolResultCheckpointMismatch);
            }
            let value = record
                .value()
                .pointer(&projection.pointer)
                .ok_or(AppWorkflowError::ToolResultCheckpointMismatch)?;
            context_bytes = context_bytes
                .checked_add(exact_json_encoded_len(value))
                .filter(|bytes| *bytes <= MAX_RUN_STATE_BYTES as usize)
                .ok_or(AppWorkflowError::TerminalPayloadTooLarge)?;
            values.insert(name.to_string(), value.clone());
            labels.classification = labels
                .classification
                .max(record.handling_labels().classification);
            labels.model_processing = labels
                .model_processing
                .min(record.handling_labels().model_processing);
            sources
                .push(json!({"name":name, "pointer":projection.pointer, "checkpoint":checkpoint}));
            if !records
                .iter()
                .any(|stored: &AppLabeledToolResultRecord| stored.checkpoint() == checkpoint)
            {
                records.push(record.clone());
            }
        }
        let value = json!({"request":input.input.value, "context":values});
        let metrics = inspect_json_bounded(&value, MAX_JSON_NODES)
            .ok_or(AppWorkflowError::TerminalPayloadTooLarge)?;
        if metrics.max_depth > MAX_JSON_DEPTH
            || exact_json_encoded_len(&value) > MAX_RUN_STATE_BYTES as usize
        {
            return Err(AppWorkflowError::TerminalPayloadTooLarge);
        }
        labels.provenance_digest = AppDigest::blake3_canonical_json(&json!({
            "input":input.input.handling_labels.provenance_digest,
            "node_binding":node.binding_digest(), "step":step_id, "sources":sources,
        }))?;
        input.input.source = AppDataSource::AppAction;
        input.input.value_schema_ref =
            AppReference::parse("schema:app-native-semantic-context:v1")?;
        input.input.value = value;
        input.input.content_digest = AppDigest::blake3_canonical_json(&input.input.value)?;
        input.input.handling_labels = labels.clone();
        input.handling_labels = ResolvedAppHandlingLabels::from_trusted_policy(labels);
        // The base stays Deterministic, so the generic agentic admission path
        // still cannot dispatch it. Only the native disclosure entry consumes
        // the reviewed semantic step and reauthorizes these exact records for
        // its concrete physical model target.
        let resource_owner = Arc::new(workflow_rounds::AppNativeRoundResourceOwner::new(
            input.llm_resource_owner.clone(),
            claim.clone(),
        ));
        Ok(AppNativeSemanticInput {
            completion: workflow_rounds::AppNativeModelCompletion::new(
                &task.task_id,
                context.run.execution_id(),
                claim,
            ),
            participant_id: claim.model().participant_id.clone(),
            resource_owner,
            input,
            step,
            records,
            prepared_at: now,
            node_binding_digest: node.binding_digest().clone(),
            claim_owner_id: context.permit.claim_owner_id().to_owned(),
            claim_epoch: context.permit.claim_epoch(),
        })
    }
}

pub(super) fn native_semantic_step(
    task: &AppWorkflowTaskBinding,
    manifest: &super::super::manifest::AppPackageManifest,
    step_id: &AppName,
    remaining_output_tokens: u32,
) -> Result<AppAdmittedRecipeStep, AppWorkflowError> {
    let (behavior_id, purpose, resources, grant_digest, granted_operations, operations, steps) =
        match (
            &task.background_behavior_binding,
            &task.event_behavior_binding,
        ) {
            (Some(binding), None) => {
                let behavior = manifest
                    .app
                    .behaviors
                    .iter()
                    .find(|item| item.id == binding.grant.behavior_id)
                    .ok_or(AppWorkflowError::StaleBehaviorRecipeBinding)?;
                (
                    &binding.grant.behavior_id,
                    &binding.grant.purpose,
                    &binding.grant.resources,
                    &binding.grant.steps_digest,
                    &binding.grant.operations,
                    &behavior.operations,
                    &behavior.steps,
                )
            },
            (None, Some(binding)) => {
                let behavior = manifest
                    .app
                    .event_behaviors
                    .iter()
                    .find(|item| item.id == binding.grant.event_behavior_id)
                    .ok_or(AppWorkflowError::StaleBehaviorRecipeBinding)?;
                (
                    &binding.grant.event_behavior_id,
                    &binding.grant.purpose,
                    &binding.grant.resources,
                    &binding.grant.steps_digest,
                    &binding.grant.operations,
                    &behavior.operations,
                    &behavior.steps,
                )
            },
            _ => return Err(AppWorkflowError::BackgroundBehaviorOperationStepBindingRequired),
        };
    if &super::super::manifest::app_behavior_steps_digest(steps)? != grant_digest {
        return Err(AppWorkflowError::StaleBehaviorRecipeBinding);
    }
    let step = steps
        .iter()
        .find(|step| &step.id == step_id)
        .filter(|step| {
            step.when.is_none()
                && operations.contains(&step.operation)
                && granted_operations.contains(&step.operation)
        })
        .ok_or(AppWorkflowError::StaleBehaviorRecipeBinding)?;
    // The durable grant uses u64 counters; the dispatcher accepts a u32
    // output limit. Narrow only after intersecting with the caller's u32 cap.
    let remaining_output_tokens =
        u32::try_from(u64::from(remaining_output_tokens).min(resources.max_tokens_per_run))
            .map_err(|_| AppWorkflowError::BackgroundBehaviorRunTokensExhausted)?;
    if remaining_output_tokens == 0 {
        return Err(AppWorkflowError::BackgroundBehaviorRunTokensExhausted);
    }
    Ok(AppAdmittedRecipeStep {
        behavior_id: behavior_id.clone(),
        behavior_purpose: purpose.clone(),
        resources: resources.clone(),
        remaining_output_tokens,
        verified_manifest: Arc::new(manifest.clone()),
        step_id: step.id.clone(),
        operation: step.operation.clone(),
        output_schema: step.output_schema.clone(),
    })
}
