//! Reconciliation dispatch within the existing workflow owner. This child
//! module can consume that owner's guarded reads, effect receipts and terminal
//! transaction; it does not have an independent execution lifecycle.
use super::*;
use crate::magician_v2::apps::reconciliation::{
    plan_reconciliation_with_sources, AppReconciliation,
};
use crate::magician_v2::execution::compiled_dispatch::{
    compiled_pack_result_to_value, prepare_attested_local_compiled_dispatch,
};

impl AppWorkflowService {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn reconcile_recipe_records<'a>(
        &'a self,
        scope: &'a ScopeRef,
        task: &'a AppWorkflowTaskBinding,
        run: &'a AppRecipeAttachedRun,
        material: &'a AppRecipeRuntimeMaterial,
        node: &'a AppName,
        permit: &'a AppRecipeCanonicalNodePermit,
        owner: &'a dyn AppRecipeCanonicalNodeOwner,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<(AppValidatedWorkflowValue, AppDigest), AppWorkflowError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let binding = run
                .plan()
                .node(node)
                .ok_or(AppWorkflowError::CorruptBinding)?;
            let AppRecipeNodeKind::Reconcile { declaration } = binding.operation() else {
                return Err(AppWorkflowError::CorruptBinding);
            };
            let execution_id = run.execution_id();
            let agent_id = task.resolved_agent_id.as_str();
            let input = workflow_value_to_json(&material.input_schema, run.input())?;
            let source = self
                .recipe_host_read(
                    scope,
                    task,
                    run,
                    &declaration.tool,
                    &declaration.action,
                    declaration
                        .bound_parameters(&input)
                        .map_err(|error| AppWorkflowError::Reconciliation(error.0))?,
                    binding
                        .authority()
                        .primitive_ref
                        .as_ref()
                        .ok_or(AppWorkflowError::RecipeBindingUnavailable)?,
                    binding
                        .authority()
                        .action_ref
                        .as_ref()
                        .ok_or(AppWorkflowError::RecipeBindingUnavailable)?,
                    "primary",
                    binding,
                    owner,
                )
                .await?;
            let mut sources = BTreeMap::new();
            for (name, read) in &declaration.sources {
                if !read.enabled(&input) {
                    continue;
                }
                // A prior read may have yielded long enough for cancellation
                // or claim loss. Revalidate the canonical owner before the
                // next separately receipted source operation.
                owner
                    .fence_recipe_node_output(scope, &task.task_id, run, node, permit)
                    .await?;
                let result = self
                    .recipe_host_read(
                        scope,
                        task,
                        run,
                        &read.tool,
                        &read.action,
                        read.bound_parameters(&input)
                            .map_err(|error| AppWorkflowError::Reconciliation(error.0))?,
                        &read.primitive_ref,
                        &read.action_ref,
                        &format!("named-{name}"),
                        binding,
                        owner,
                    )
                    .await?;
                sources.insert(name.clone(), result.value().clone());
            }
            let mut existing = BTreeMap::new();
            for target in &declaration.targets {
                if target
                    .source
                    .as_ref()
                    .is_some_and(|name| !sources.contains_key(name))
                {
                    continue;
                }
                let mut rows = Vec::new();
                let mut cursor = None;
                let mut seen_cursors = BTreeSet::new();
                let mut page_number = 0usize;
                // Cursor identity includes limit; keep it fixed on every page.
                let page_size = usize::from(declaration.max_existing_rows).min(100);
                loop {
                    owner
                        .fence_recipe_node_output(scope, &task.task_id, run, node, permit)
                        .await?;
                    let remaining = usize::from(declaration.max_existing_rows)
                        .checked_sub(rows.len())
                        .filter(|remaining| *remaining > 0)
                        .ok_or(AppWorkflowError::Reconciliation(
                            "store scan exceeds its reviewed per-run budget",
                        ))?;
                    let mut parameters = HashMap::from([
                        ("entity".to_owned(), json!(target.entity)),
                        (
                            "select".to_owned(),
                            json!(AppReconciliation::selected_fields(target)),
                        ),
                        ("limit".to_owned(), json!(page_size)),
                    ]);
                    if let Some(cursor) = &cursor {
                        parameters.insert("cursor".to_owned(), json!(cursor));
                    }
                    let record = Box::pin(self.query_own_store(
                        scope,
                        &task.task_id,
                        execution_id,
                        agent_id,
                        &format!(
                            "recipe-reconcile-{}-{}-{}",
                            node, target.entity, page_number
                        ),
                        &parameters,
                        Utc::now(),
                    ))
                    .await?;
                    let page: AppQueryPage = serde_json::from_value(record.value().clone())?;
                    if page.envelope.value.len() > remaining.min(page_size)
                        || (page.envelope.value.is_empty() && page.next_cursor.is_some())
                    {
                        return Err(AppWorkflowError::Reconciliation("invalid store page"));
                    }
                    rows.extend(page.envelope.value);
                    let Some(next) = page.next_cursor else { break };
                    if !seen_cursors.insert(next.clone()) {
                        return Err(AppWorkflowError::Reconciliation("repeated store cursor"));
                    }
                    cursor = Some(next);
                    page_number += 1;
                }
                existing.insert(target.entity.clone(), rows);
            }
            let plan = plan_reconciliation_with_sources(
                declaration,
                &input,
                source.value(),
                &sources,
                &existing,
                Utc::now(),
            )
            .map_err(|error| AppWorkflowError::Reconciliation(error.0))?;
            // The canonical claim, cancellation, live grants and resource authority
            // are independent fences. The commit owner checks its own fences again
            // immediately before the all-or-nothing store transaction.
            owner
                .fence_recipe_node_output(scope, &task.task_id, run, node, permit)
                .await?;
            let unchanged = plan.operations.is_empty();
            let mut parameters = HashMap::from([
                (
                    "operations".to_owned(),
                    serde_json::to_value(plan.operations)?,
                ),
                (
                    "expected_record_revisions".to_owned(),
                    serde_json::to_value(plan.expected_record_revisions)?,
                ),
                (
                    "user_visible_summary".to_owned(),
                    json!("Reconciled app records from the approved source."),
                ),
            ]);
            if unchanged {
                parameters.insert(
                    "output".to_owned(),
                    crate::magician_v2::apps::reconciliation::unchanged_result(),
                );
            }
            Box::pin(self.commit_terminal_inner(
                scope,
                &task.task_id,
                execution_id,
                agent_id,
                &parameters,
                Utc::now(),
                unchanged,
            ))
            .await?;
            let state = self
                .require_run_state(scope, task, execution_id, agent_id)
                .await?;
            let result = state
                .result
                .as_ref()
                .ok_or(AppWorkflowError::CorruptBinding)?;
            let receipt_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(result)?)?;
            let envelope = result
                .output
                .as_ref()
                .ok_or(AppWorkflowError::CorruptBinding)?;
            let schema = material
                .bundle
                .schema(&binding.output().schema_ref)
                .ok_or(AppWorkflowError::RecipeBindingUnavailable)?;
            let value = validate_json_workflow_value(
                schema,
                envelope.value.clone(),
                envelope.handling_labels.clone(),
                recipe_provenance_from_source_refs(
                    &envelope.source_refs,
                    &envelope.handling_labels,
                    &envelope.content_digest,
                )?,
            )
            .map_err(AppWorkflowError::from)?;
            Ok((value, receipt_digest))
        })
    }

    /// No model loop: the same exact provider witness, parameter proof,
    /// disclosure, package lock, resource reservation and receipt owners used
    /// by agentic Apps calls remain mandatory here.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn recipe_host_read<'a>(
        &'a self,
        scope: &'a ScopeRef,
        task: &'a AppWorkflowTaskBinding,
        run: &'a AppRecipeAttachedRun,
        tool_name: &'a AppName,
        action_name: &'a AppName,
        parameters: BTreeMap<AppName, Value>,
        primitive_ref: &'a AppReference,
        action_ref: &'a AppReference,
        source_name: &'a str,
        binding: &'a crate::magician_v2::apps::recipe_lowering::AppRecipeLoweredNodeBinding,
        owner: &'a dyn AppRecipeCanonicalNodeOwner,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<AppLabeledToolResultRecord, AppWorkflowError>>
                + Send
                + 'a,
        >,
    > {
        use futures_util::FutureExt;
        // These reads nest beneath the recipe and effect owners. Keep their
        // future storage indirect on the ordinary executor thread stack.
        Box::pin(async move {
            let execution_id = run.execution_id();
            let agent_id = task.resolved_agent_id.as_str();
            let tool = tool_name.as_str();
            let invocation = format!(
                "recipe-reconcile-source-{}-{}",
                binding.binding_digest(),
                source_name
            );
            let plan = super::super::app_tool_bind::plan_app_tool_call(
                tool,
                Some(action_name.as_str()),
                super::super::app_tool_bind::AppToolContainProfile::InProcessCompiled,
            );
            if plan.io_kind != super::super::app_tool_bind::AppToolIoKind::BoundHostRead
                || !super::super::app_tool_bind::app_effect_owner_supported(&plan)
            {
                return Err(AppWorkflowError::ToolDenied(tool.to_owned()));
            }
            let tool_ref = AppReference::parse(format!("capability:{tool}"))?;
            let state = self
                .require_run_state(scope, task, execution_id, agent_id)
                .boxed()
                .await?;
            let (authenticated, _) = self
                .ensure_live_binding(scope, task, &state, Utc::now())
                .boxed()
                .await?;
            let immutable = self
                .immutable_workflow_material(&authenticated, task, Utc::now())
                .boxed()
                .await?;
            let locked = immutable
                .package_lock
                .capability(&tool_ref)
                .and_then(|capability| capability.primitive_binding())
                .ok_or(AppWorkflowError::RecipeBindingUnavailable)?;
            let action_binding = locked
                .action_named(action_name.as_str())
                .ok_or(AppWorkflowError::RecipeBindingUnavailable)?;
            if primitive_ref != locked.primitive_ref() || action_ref != action_binding.action_ref()
            {
                return Err(AppWorkflowError::RecipeBindingUnavailable);
            }
            let mut parameters: HashMap<String, Value> = parameters
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect();
            parameters.insert("__action_name".to_owned(), json!(action_name));
            parameters.insert("__principal".to_owned(), json!(scope.principal()));
            parameters.insert("__workspace".to_owned(), json!(scope.workspace()));
            let timeout_secs = (binding.resources().max_active_millis / 1000)
                .min(30)
                .max(1);
            let witness = owner.recipe_builtin_provider(scope, tool).await?;
            let mut dispatch = prepare_attested_local_compiled_dispatch(
                witness,
                tool,
                parameters,
                timeout_secs,
                Some(execution_id.to_owned()),
                None,
                Some(invocation.clone()),
            )
            .boxed()
            .await
            .map_err(|_| AppWorkflowError::ToolDenied(tool.to_owned()))?;
            if let AppRecipeNodeKind::Reconcile { declaration } = binding.operation() {
                let source = if source_name == "primary" {
                    None
                } else {
                    Some(AppName::parse(
                        source_name
                            .strip_prefix("named-")
                            .ok_or(AppWorkflowError::CorruptBinding)?,
                    )?)
                };
                // The source selection and field mapping are already sealed
                // into this compiled node's binding/invocation identity.
                let projection = declaration
                    .source_projection(source.as_ref())
                    .map_err(|error| AppWorkflowError::Reconciliation(error.0))?;
                dispatch = dispatch
                    .with_reconciliation_projection(projection)
                    .map_err(|_| AppWorkflowError::CorruptBinding)?;
            }
            let parameters = dispatch
                .exact_parameters()
                .ok_or(AppWorkflowError::CorruptBinding)?
                .clone();
            let bytes =
                canonical_json_bytes(&json!({"capability_name": tool, "parameters": parameters}))?;
            let target = dispatch
                .attest_app_tool_target(tool_ref)
                .ok_or(AppWorkflowError::CorruptBinding)?;
            let input_ceiling = dispatch
                .reviewed_transport_input_byte_ceiling()
                .ok_or(AppWorkflowError::ReviewedTransportCeilingUnavailable)?;
            let result_ceiling = dispatch
                .reviewed_transport_result_byte_ceiling()
                .ok_or(AppWorkflowError::ReviewedTransportCeilingUnavailable)?;
            let action = ExecutableAction::Pack {
                capability_name: tool.to_owned(),
                implementation:
                    crate::magician_v2::execution::capability::ImplementationType::Compiled {
                        provider_name: tool.to_owned(),
                    },
                resolved_params: parameters,
            };
            let disclosure = self
                .authorize_tool_disclosure(
                    scope,
                    &task.task_id,
                    execution_id,
                    agent_id,
                    &invocation,
                    target,
                    &bytes,
                    input_ceiling,
                    result_ceiling,
                    Utc::now(),
                )
                .boxed()
                .await?;
            let mut operation = self
                .reserve_action_resources(
                    scope,
                    &task.task_id,
                    execution_id,
                    agent_id,
                    &invocation,
                    bytes.clone(),
                    AppResourceQuantity::default(),
                    Some(tool_name.clone()),
                    false,
                    Some(result_ceiling),
                    timeout_secs,
                    Utc::now(),
                )
                .boxed()
                .await?
                .ok_or(AppWorkflowError::ResourceRuntimeUnavailable)?;
            let attempted = Box::pin(self.execute_compiled_effect(
                scope,
                &task.task_id,
                execution_id,
                agent_id,
                &invocation,
                &action,
                dispatch,
                &mut operation,
                disclosure,
                None,
                &bytes,
            ))
            .await;
            let mut execution = match attempted {
                AppWorkflowEffectAttempt::RequiresSettlement(execution) => execution,
                AppWorkflowEffectAttempt::DispatchStartUnresolved
                | AppWorkflowEffectAttempt::AbortBeforeIoUnresolved => {
                    self.retain_unresolved_effect_operation(operation);
                    return Err(AppWorkflowError::RecipeOutcomeUncertain);
                },
                AppWorkflowEffectAttempt::AbortedBeforeIo => {
                    return Err(AppWorkflowError::RecipeCancelled)
                },
                AppWorkflowEffectAttempt::AdmissionExpiredBeforeIo => {
                    return Err(AppWorkflowError::EffectAdmissionExpiredBeforeIo)
                },
                AppWorkflowEffectAttempt::RejectedBeforeDispatchStart => {
                    self.release_pre_io_operation(
                        scope,
                        &task.task_id,
                        execution_id,
                        agent_id,
                        operation,
                        Utc::now(),
                    )
                    .boxed()
                    .await?;
                    return Err(AppWorkflowError::ToolDenied(tool.to_owned()));
                },
            };
            self.settle_action_resources(
                scope,
                &task.task_id,
                execution_id,
                agent_id,
                operation,
                &mut execution.result,
                None,
                Some(execution.receipt.clone()),
                Utc::now(),
            )
            .boxed()
            .await?;
            let result = execution.result.map_err(|error| {
                if matches!(
                    error.downcast_ref::<AppWorkflowError>(),
                    Some(AppWorkflowError::ToolResultTooLarge)
                ) {
                    AppWorkflowError::ToolResultTooLarge
                } else {
                    AppWorkflowError::Reconciliation("approved source read failed")
                }
            })?;
            let value =
                compiled_pack_result_to_value(&result).ok_or(AppWorkflowError::CorruptBinding)?;
            self.label_tool_result(
                scope,
                &task.task_id,
                execution_id,
                agent_id,
                execution
                    .disclosure_permit
                    .take()
                    .ok_or(AppWorkflowError::CorruptBinding)?,
                execution.receipt,
                value,
                Utc::now(),
            )
            .boxed()
            .await
        })
    }
}
