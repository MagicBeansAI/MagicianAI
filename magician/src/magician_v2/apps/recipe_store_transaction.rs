//! Deterministic own-store operations within the existing App workflow owner.
//! Host reads retain their exact locked capability; mutations retain the normal
//! all-or-nothing revision, permission, resource and receipt boundaries.
use super::*;
use crate::magician_v2::apps::contextual_round_program::AppRoundValueEnvironment;
use crate::magician_v2::apps::reconciliation;
use workflow_commits::AppNativeProgressContext;

fn program_error(
    _: crate::magician_v2::apps::contextual_round_program::AppRoundProgramError,
) -> AppWorkflowError {
    AppWorkflowError::Reconciliation("invalid store transaction value")
}

/// Recovery reuses exactly the durable command, never newly read values or a
/// replacement timestamp. The ordinary commit owner resolves its receipt and
/// resource reservation before repeating any entity I/O.
fn retained_parameters(
    intent: &AppWorkflowCommitIntent,
) -> Result<HashMap<String, Value>, AppWorkflowError> {
    let mut parameters = HashMap::from([
        (
            "user_visible_summary".to_owned(),
            json!(intent.user_visible_summary),
        ),
        (
            "source_artifact_refs".to_owned(),
            json!(intent.source_artifact_refs),
        ),
    ]);
    match &intent.effect {
        AppWorkflowTerminalEffect::ReadOnly { output } => {
            parameters.insert("output".to_owned(), output.clone());
        },
        AppWorkflowTerminalEffect::Mutation { command } => {
            parameters.insert(
                "operations".to_owned(),
                serde_json::to_value(&command.operations)?,
            );
            parameters.insert(
                "expected_record_revisions".to_owned(),
                serde_json::to_value(&command.expected_record_revisions)?,
            );
        },
    }
    Ok(parameters)
}

impl AppWorkflowService {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn execute_store_transaction<'a>(
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
            let AppRecipeNodeKind::StoreTransaction { sources, program } = binding.operation()
            else {
                return Err(AppWorkflowError::CorruptBinding);
            };
            let context = AppNativeProgressContext {
                run,
                node,
                permit,
                owner,
            };
            context.fence(scope, &task.task_id).await?;
            let state = self
                .require_run_state(
                    scope,
                    task,
                    run.execution_id(),
                    task.resolved_agent_id.as_str(),
                )
                .await?;
            let mut parameters = if let Some(result) = state.result.as_ref() {
                return recipe_contextual_round::validated_round_result(material, binding, result);
            } else if let Some(intent) = state.commit_intent.as_ref() {
                retained_parameters(intent)?
            } else {
                let input = workflow_value_to_json(&material.input_schema, run.input())?;
                let prepared_at = Utc::now();
                let mut values = json!({});
                let input_values = program
                    .values
                    .evaluate(&AppRoundValueEnvironment {
                        input: &input,
                        participant: &Value::Null,
                        context: &values,
                        model: None,
                        item: None,
                        now: prepared_at,
                        participant_id: "",
                        run_id: run.execution_id(),
                    })
                    .map_err(program_error)?;
                for (name, source) in sources {
                    if !source.enabled(&input) {
                        continue;
                    }
                    context.fence(scope, &task.task_id).await?;
                    let mut parameters = source
                        .bound_parameters(&input)
                        .map_err(|error| AppWorkflowError::Reconciliation(error.0))?;
                    if let Some(overrides) = program.source_parameters.get(name.as_str()) {
                        for (parameter, id) in overrides {
                            parameters.insert(
                                AppName::parse(parameter)?,
                                input_values.get(*id).map_err(program_error)?.clone(),
                            );
                        }
                        if !reconciliation::valid_source_parameters(&parameters, &BTreeMap::new()) {
                            return Err(AppWorkflowError::Reconciliation(
                                "invalid prepared host parameters",
                            ));
                        }
                    }
                    let record = self
                        .recipe_host_read(
                            scope,
                            task,
                            run,
                            &source.tool,
                            &source.action,
                            parameters,
                            &source.primitive_ref,
                            &source.action_ref,
                            &format!("store-{name}"),
                            binding,
                            owner,
                        )
                        .await?;
                    values[name.as_str()] = record.value().clone();
                }
                for query in &program.queries {
                    let prepared = program
                        .values
                        .evaluate(&AppRoundValueEnvironment {
                            input: &input,
                            participant: &Value::Null,
                            context: &values,
                            model: None,
                            item: None,
                            now: prepared_at,
                            participant_id: "",
                            run_id: run.execution_id(),
                        })
                        .map_err(program_error)?;
                    if !program
                        .query_enabled(&query.name, &prepared)
                        .map_err(program_error)?
                    {
                        values[&query.name] = json!([]);
                        continue;
                    }
                    let base = program
                        .query_parameters(query, &prepared)
                        .map_err(program_error)?;
                    let page_size = base
                        .get("limit")
                        .and_then(Value::as_u64)
                        .ok_or(AppWorkflowError::CorruptBinding)?
                        as usize;
                    let scan = program.scan_queries.contains(&query.name);
                    let mut cursor = None;
                    let mut seen = BTreeSet::new();
                    let mut rows = vec![];
                    for page_index in 0..program.max_query_pages {
                        context.fence(scope, &task.task_id).await?;
                        let mut parameters: HashMap<_, _> = base.clone().into_iter().collect();
                        if let Some(cursor) = &cursor {
                            parameters.insert("cursor".to_owned(), json!(cursor));
                        }
                        let record = Box::pin(self.query_own_store(
                            scope,
                            &task.task_id,
                            run.execution_id(),
                            task.resolved_agent_id.as_str(),
                            &format!(
                                "store-query-{}-{}-{page_index}",
                                binding.binding_digest(),
                                query.name
                            ),
                            &parameters,
                            Utc::now(),
                        ))
                        .await?;
                        let page: AppQueryPage = serde_json::from_value(record.value().clone())?;
                        page.validate_app_contract(&AppContractLimits::default())?;
                        if page.envelope.value.len() > page_size
                            || (page.envelope.value.is_empty() && page.next_cursor.is_some())
                        {
                            return Err(AppWorkflowError::Reconciliation("invalid store page"));
                        }
                        rows.extend(page.envelope.value);
                        if !scan {
                            break;
                        }
                        let Some(next) = page.next_cursor else {
                            break;
                        };
                        if !seen.insert(next.clone()) || page_index + 1 == program.max_query_pages {
                            return Err(AppWorkflowError::Reconciliation("incomplete store scan"));
                        }
                        cursor = Some(next);
                    }
                    values[&query.name] = serde_json::to_value(rows)?;
                }
                let plan = program
                    .plan(&AppRoundValueEnvironment {
                        input: &input,
                        participant: &Value::Null,
                        context: &values,
                        model: None,
                        item: None,
                        now: prepared_at,
                        participant_id: "",
                        run_id: run.execution_id(),
                    })
                    .map_err(program_error)?;
                HashMap::from([
                    ("operations".to_owned(), json!(plan.operations)),
                    (
                        "expected_record_revisions".to_owned(),
                        json!(plan.expected_record_revisions),
                    ),
                    (
                        "user_visible_summary".to_owned(),
                        json!(program
                            .summary(
                                &program
                                    .values
                                    .evaluate(&AppRoundValueEnvironment {
                                        input: &input,
                                        participant: &Value::Null,
                                        context: &values,
                                        model: None,
                                        item: None,
                                        now: prepared_at,
                                        participant_id: "",
                                        run_id: run.execution_id(),
                                    })
                                    .map_err(program_error)?
                            )
                            .map_err(program_error)?),
                    ),
                ])
            };
            // Empty plans use the receipt-free owner result. Nonempty plans
            // must enter the ordinary mutation owner, not its no-change branch.
            let unchanged = parameters
                .get("operations")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty);
            if unchanged {
                let summary = parameters
                    .get("user_visible_summary")
                    .and_then(Value::as_str)
                    .unwrap_or("App records already match the approved source.");
                parameters.insert(
                    "output".to_owned(),
                    reconciliation::unchanged_result_with_summary(summary),
                );
            }
            context.fence(scope, &task.task_id).await?;
            let result = Box::pin(self.commit_workflow_effect_inner(
                scope,
                &task.task_id,
                run.execution_id(),
                task.resolved_agent_id.as_str(),
                &parameters,
                Utc::now(),
                unchanged,
                AppWorkflowCommitTarget::Terminal,
                Some(&context),
            ))
            .await?;
            recipe_contextual_round::validated_round_result(material, binding, &result)
        })
    }
}
