//! Context-first native rounds. Selection and mutations are deterministic;
//! only the reviewed semantic step enters the normal App model dispatcher.
use super::*;
use crate::magician_v2::apps::{
    contextual_round::{AppRoundCandidate, AppRoundParticipantState},
    contextual_round_declaration::{
        AppRoundContextMode, AppRoundProgramDeclaration, AppRoundSemanticOutcome,
    },
    contextual_round_program::{AppRoundValueEnvironment, AppRoundValueSource},
    reconciliation::AppReconcileSourceRows,
};
use workflow_commits::{AppNativeCommitKey, AppNativeProgressContext};
use workflow_rounds::{AppNativeContextReference, AppNativeRoundModelClaim, AppNativeRoundOutcome};

fn program_error(
    _: crate::magician_v2::apps::contextual_round_program::AppRoundProgramError,
) -> AppWorkflowError {
    AppWorkflowError::Reconciliation("invalid contextual round value")
}

fn projected_context(
    state: &AppWorkflowRunState,
    references: &BTreeMap<AppName, AppNativeContextReference>,
) -> Result<Value, AppWorkflowError> {
    let mut values = serde_json::Map::new();
    for (name, reference) in references {
        let record = state
            .labeled_tool_results
            .iter()
            .find(|record| record.checkpoint() == &reference.checkpoint)
            .ok_or(AppWorkflowError::ToolResultCheckpointMismatch)?;
        let value = record
            .value()
            .pointer(&reference.pointer)
            .ok_or(AppWorkflowError::ToolResultCheckpointMismatch)?;
        values.insert(name.to_string(), value.clone());
    }
    Ok(Value::Object(values))
}

impl AppWorkflowService {
    async fn reserve_round_context_slots(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &AppNativeProgressContext<'_>,
        program: &AppRoundProgramDeclaration,
        source_rows: usize,
        source_pages: u16,
    ) -> Result<(), AppWorkflowError> {
        let slots = program
            .retained_query_slots(source_rows, source_pages)
            .map_err(program_error)?
            .max(MAX_APP_LABELED_TOOL_RESULTS);
        if slots > MAX_RUN_STATE_BYTES as usize {
            return Err(AppWorkflowError::ToolResultTooLarge);
        }
        let recipe_binding_digest = task
            .recipe_binding
            .as_ref()
            .ok_or(AppWorkflowError::RecipeBindingUnavailable)?
            .binding_digest()
            .clone();
        context.fence(scope, &task.task_id).await?;
        let guard = self.acquire_task_guard(scope, &task.task_id).await?;
        context
            .fence_under_task_guard(scope, &task.task_id, &guard)
            .await?;
        let mut state = self
            .require_run_state_unlocked(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        if state
            .native_tool_result_capacity
            .as_ref()
            .is_some_and(|capacity| {
                capacity.recipe_binding_digest == recipe_binding_digest && capacity.slots >= slots
            })
        {
            return Ok(());
        }
        state.native_tool_result_capacity = Some(AppNativeToolResultCapacity {
            recipe_binding_digest,
            slots,
        });
        self.persist_run_state_unlocked(scope, &task.task_id, context.run.execution_id(), &state)
            .await
    }

    async fn round_queries(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &AppNativeProgressContext<'_>,
        program: &AppRoundProgramDeclaration,
        input: &Value,
        participant: &Value,
        participant_id: &str,
        per_participant: bool,
        phase: &str,
        now: DateTime<Utc>,
    ) -> Result<BTreeMap<AppName, AppNativeContextReference>, AppWorkflowError> {
        let empty = json!({});
        let values = program
            .values
            .evaluate(&AppRoundValueEnvironment {
                input,
                participant,
                context: &empty,
                model: None,
                item: None,
                now,
                run_id: context.run.execution_id(),
                participant_id,
            })
            .map_err(program_error)?;
        // These queries consume only the current first page. Choose the shared
        // indexed path where the schema supports it, before resource/authority
        // binding. Reconciliation and other frozen scans keep snapshot semantics.
        let authenticated = task_execution_scope(
            scope,
            &task.accepted_authority.scope_binding_ref,
            context.run.execution_id(),
            now,
        )?;
        let active = self
            .entity_store
            .active_schema(&authenticated, &task.installation_id, now)
            .await?
            .ok_or(AppWorkflowError::StaleRuntimeAuthority)?;
        let mut references = BTreeMap::new();
        for query in program
            .queries
            .iter()
            .filter(|query| query.per_participant == per_participant)
            .filter(|query| {
                phase != "dispatch" || query.per_participant || query.refresh_before_dispatch
            })
        {
            context.fence(scope, &task.task_id).await?;
            let mut parameters: HashMap<String, Value> = program
                .query_parameters(query, &values)
                .map_err(program_error)?
                .into_iter()
                .collect();
            if active.supports_keyset_query(&workflow_store_query_request(task, &parameters)?) {
                parameters.insert("pagination".to_owned(), json!("keyset"));
            }
            let invocation = AppDigest::blake3_canonical_json(&json!([
                context.permit.binding_digest(),
                phase,
                participant_id,
                query.name,
            ]))?;
            let record = Box::pin(self.query_own_store_inner(
                scope,
                &task.task_id,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
                &format!("round-window-query-{invocation}"),
                &parameters,
                Utc::now(),
                true,
            ))
            .await?;
            references.insert(
                AppName::parse(&query.name)?,
                AppNativeContextReference {
                    checkpoint: record.checkpoint().clone(),
                    pointer: "/envelope/value".to_owned(),
                },
            );
        }
        Ok(references)
    }

    pub(super) fn execute_contextual_round<'a>(
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
        use futures_util::FutureExt;
        Box::pin(async move {
            let binding = run
                .plan()
                .node(node)
                .ok_or(AppWorkflowError::CorruptBinding)?;
            let AppRecipeNodeKind::ContextualRound {
                source,
                cursor_parameter,
                max_source_pages,
                program,
            } = binding.operation()
            else {
                return Err(AppWorkflowError::CorruptBinding);
            };
            let context = AppNativeProgressContext {
                run,
                node,
                permit,
                owner,
            };
            let input = workflow_value_to_json(&material.input_schema, run.input())?;
            let round_key = workflow_rounds::round_ref(binding.binding_digest())?;
            let state = self
                .require_run_state(
                    scope,
                    task,
                    run.execution_id(),
                    task.resolved_agent_id.as_str(),
                )
                .boxed()
                .await?;
            if let Some(result) = state.result.as_ref() {
                return validated_round_result(material, binding, result);
            }
            if !state.native_rounds.contains_key(&round_key) {
                let prepared_at = Utc::now();
                self.reserve_round_context_slots(
                    scope,
                    task,
                    &context,
                    program,
                    0,
                    *max_source_pages,
                )
                .boxed()
                .await?;
                let shared = Box::pin(self.round_queries(
                    scope,
                    task,
                    &context,
                    program,
                    &input,
                    &Value::Null,
                    "",
                    false,
                    "prepare",
                    prepared_at,
                ))
                .await?;
                let state = self
                    .require_run_state(
                        scope,
                        task,
                        run.execution_id(),
                        task.resolved_agent_id.as_str(),
                    )
                    .boxed()
                    .await?;
                let shared_values = projected_context(&state, &shared)?;
                let prepared = program
                    .values
                    .evaluate(&AppRoundValueEnvironment {
                        input: &input,
                        participant: &Value::Null,
                        context: &shared_values,
                        model: None,
                        item: None,
                        now: prepared_at,
                        run_id: run.execution_id(),
                        participant_id: "",
                    })
                    .map_err(program_error)?;
                let after = program
                    .resume_after
                    .map(|id| prepared.get(id))
                    .transpose()
                    .map_err(program_error)?
                    .filter(|value| !value.is_null())
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or(AppWorkflowError::CorruptBinding)
                    })
                    .transpose()?;
                let AppReconcileSourceRows::Page {
                    rows_field,
                    next_cursor_field,
                    truncated_field,
                } = &source.rows
                else {
                    return Err(AppWorkflowError::CorruptBinding);
                };
                let mut parameters = source
                    .bound_parameters(&input)
                    .map_err(|error| AppWorkflowError::Reconciliation(error.0))?;
                let mut seen_cursors = BTreeSet::new();
                let mut candidates = Vec::new();
                for page in 0..*max_source_pages {
                    context.fence(scope, &task.task_id).await?;
                    let record = self
                        .recipe_host_read(
                            scope,
                            task,
                            run,
                            &source.tool,
                            &source.action,
                            parameters.clone(),
                            &source.primitive_ref,
                            &source.action_ref,
                            &format!("round-participants-{page}"),
                            binding,
                            owner,
                        )
                        .await?;
                    let rows = record
                        .value()
                        .get(rows_field.as_str())
                        .and_then(Value::as_array)
                        .ok_or(AppWorkflowError::Reconciliation("invalid participant page"))?;
                    let discovered = candidates
                        .len()
                        .checked_add(rows.len())
                        .ok_or(AppWorkflowError::ToolResultTooLarge)?;
                    self.reserve_round_context_slots(
                        scope,
                        task,
                        &context,
                        program,
                        discovered,
                        *max_source_pages,
                    )
                    .boxed()
                    .await?;
                    for (index, participant) in rows.iter().enumerate() {
                        let id = participant
                            .pointer(&program.participant_id_pointer)
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .ok_or(AppWorkflowError::CorruptBinding)?
                            .to_owned();
                        let mut references = shared.clone();
                        references.insert(
                            AppName::parse("participant")?,
                            AppNativeContextReference {
                                checkpoint: record.checkpoint().clone(),
                                pointer: format!("/{rows_field}/{index}"),
                            },
                        );
                        let preliminary = program
                            .values
                            .evaluate(&AppRoundValueEnvironment {
                                input: &input,
                                participant,
                                context: &shared_values,
                                model: None,
                                item: None,
                                now: prepared_at,
                                run_id: run.execution_id(),
                                participant_id: &id,
                            })
                            .map_err(program_error)?;
                        let early_exclusion = program
                            .eligibility
                            .iter()
                            .find(|rule| {
                                program.values.sources([rule.require]).is_ok_and(|sources| {
                                    !sources.contains(&AppRoundValueSource::Context)
                                }) && !preliminary.condition(rule.require)
                            })
                            .map(|rule| rule.reason.clone());
                        let mut preflight_failure = None;
                        if early_exclusion.is_none()
                            && program.context_mode == AppRoundContextMode::Snapshot
                        {
                            match Box::pin(self.round_queries(
                                scope,
                                task,
                                &context,
                                program,
                                &input,
                                participant,
                                &id,
                                true,
                                "prepare",
                                prepared_at,
                            ))
                            .await
                            {
                                Ok(found) => references.extend(found),
                                Err(error) => {
                                    tracing::warn!(task_id = %task.task_id, participant_id = %id, %error,
                                        "participant context unavailable; other candidates continue");
                                    preflight_failure = Some("context_unavailable".to_owned());
                                },
                            }
                        }
                        let state = self
                            .require_run_state(
                                scope,
                                task,
                                run.execution_id(),
                                task.resolved_agent_id.as_str(),
                            )
                            .boxed()
                            .await?;
                        let values = projected_context(&state, &references)?;
                        let prepared = program
                            .values
                            .evaluate(&AppRoundValueEnvironment {
                                input: &input,
                                participant,
                                context: &values,
                                model: None,
                                item: None,
                                now: prepared_at,
                                run_id: run.execution_id(),
                                participant_id: &id,
                            })
                            .map_err(program_error)?;
                        candidates.push(AppRoundCandidate {
                            exclusion: early_exclusion.or_else(|| {
                                if preflight_failure.is_none()
                                    && program.context_mode == AppRoundContextMode::Snapshot
                                {
                                    program.exclusion(&prepared)
                                } else {
                                    None
                                }
                            }),
                            preflight_failure,
                            participant_id: id,
                            context: serde_json::to_value(references)?,
                        });
                    }
                    let next = record
                        .value()
                        .get(next_cursor_field.as_str())
                        .filter(|value| !value.is_null());
                    let truncated = record
                        .value()
                        .get(truncated_field.as_str())
                        .and_then(Value::as_bool)
                        .ok_or(AppWorkflowError::Reconciliation(
                            "invalid participant paging flag",
                        ))?;
                    match next {
                        None if !truncated => break,
                        Some(Value::String(cursor))
                            if !rows.is_empty() && seen_cursors.insert(cursor.clone()) =>
                        {
                            if page + 1 == *max_source_pages {
                                return Err(AppWorkflowError::Reconciliation(
                                    "participant scan exceeds its reviewed page budget",
                                ));
                            }
                            parameters.insert(cursor_parameter.clone(), json!(cursor));
                        },
                        _ => {
                            return Err(AppWorkflowError::Reconciliation(
                                "invalid participant cursor",
                            ))
                        },
                    }
                }
                self.begin_native_round(
                    scope,
                    task,
                    &context,
                    candidates,
                    program.limits.clone(),
                    after.as_deref(),
                    shared,
                    prepared_at,
                )
                .await?;
            }
            // Recover charged attempts and retained results before issuing any new call.
            let (_, abandoned) = self
                .native_round_snapshot(scope, task, &context)
                .boxed()
                .await?;
            for claim in abandoned {
                let _ = self
                    .settle_native_round_participant(
                        scope,
                        task,
                        &context,
                        &claim,
                        AppNativeRoundOutcome::Failed {
                            code: AppName::parse("model_result_interrupted")?,
                            retryable: true,
                        },
                        None,
                    )
                    .await;
            }
            let (round, _) = self
                .native_round_snapshot(scope, task, &context)
                .boxed()
                .await?;
            for participant in round.participants() {
                if matches!(
                    participant.state(),
                    AppRoundParticipantState::Prepared { .. }
                ) {
                    if let Err(error) = self
                        .commit_round_participant(
                            scope,
                            task,
                            &context,
                            program,
                            &input,
                            participant.participant_id(),
                        )
                        .await
                    {
                        tracing::warn!(task_id = %task.task_id, participant_id = participant.participant_id(), %error, "native round retained commit remains pending");
                    }
                }
            }
            let mut running = futures_util::stream::FuturesUnordered::new();
            loop {
                while running.len() < usize::from(program.limits.max_concurrent) {
                    if program.context_mode == AppRoundContextMode::Progressive {
                        self.refresh_next_round_context(scope, task, &context, program, &input)
                            .await?;
                    }
                    let (claim, completed) = concurrent_progress::await_with_progress(
                        self.claim_native_round_participant(scope, task, &context),
                        &mut running,
                    )
                    .await;
                    for result in completed {
                        if let Err(error) = result {
                            tracing::warn!(task_id = %task.task_id, %error, "native round participant did not complete; other participants continue");
                        }
                    }
                    let claim = match claim {
                        Ok(Some(claim)) => claim,
                        Ok(None) => break,
                        Err(error) => {
                            let (cancelled, _) = concurrent_progress::await_with_progress(
                                self.cancel_native_round(scope, task, &context),
                                &mut running,
                            )
                            .await;
                            use futures_util::StreamExt;
                            // Accepted physical calls drain through their normal
                            // dispatcher/resource owner before this frame exits.
                            while running.next().await.is_some() {}
                            cancelled?;
                            return Err(error);
                        },
                    };
                    running.push(Box::pin(self.run_round_participant(
                        scope, task, &context, program, &input, claim,
                    )));
                }
                use futures_util::StreamExt;
                let Some(result) = running.next().await else {
                    break;
                };
                if let Err(error) = result {
                    tracing::warn!(task_id = %task.task_id, %error, "native round participant did not complete; other participants continue");
                }
            }
            let (round, _) = self
                .native_round_snapshot(scope, task, &context)
                .boxed()
                .await?;
            if !round.is_complete() {
                return Err(AppWorkflowError::EffectSettlementUnresolved);
            }
            context.fence(scope, &task.task_id).await?;
            let final_context = Box::pin(self.round_queries(
                scope,
                task,
                &context,
                program,
                &input,
                &Value::Null,
                "",
                false,
                "final",
                Utc::now(),
            ))
            .await?;
            let state = self
                .require_run_state(
                    scope,
                    task,
                    run.execution_id(),
                    task.resolved_agent_id.as_str(),
                )
                .boxed()
                .await?;
            let mut values = projected_context(&state, &final_context)?;
            values["round"] = json!({"next_cursor": round.next_cursor(), "summary": round.summary().map_err(|_| AppWorkflowError::CorruptBinding)?});
            let plan = program
                .plan_mutations(
                    true,
                    &AppRoundValueEnvironment {
                        input: &input,
                        participant: &Value::Null,
                        context: &values,
                        model: None,
                        item: None,
                        now: Utc::now(),
                        run_id: run.execution_id(),
                        participant_id: "",
                    },
                )
                .map_err(program_error)?;
            let parameters = HashMap::from([
                ("operations".to_owned(), json!(plan.operations)),
                (
                    "expected_record_revisions".to_owned(),
                    json!(plan.expected_record_revisions),
                ),
            ]);
            Box::pin(self.commit_workflow_effect_inner(
                scope,
                &task.task_id,
                run.execution_id(),
                task.resolved_agent_id.as_str(),
                &parameters,
                Utc::now(),
                false,
                AppWorkflowCommitTarget::Terminal,
                Some(&context),
            ))
            .await?;
            let state = self
                .require_run_state(
                    scope,
                    task,
                    run.execution_id(),
                    task.resolved_agent_id.as_str(),
                )
                .boxed()
                .await?;
            let result = state
                .result
                .as_ref()
                .ok_or(AppWorkflowError::CorruptBinding)?;
            validated_round_result(material, binding, result)
        })
    }

    async fn run_round_participant(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &AppNativeProgressContext<'_>,
        program: &AppRoundProgramDeclaration,
        input: &Value,
        claim: AppNativeRoundModelClaim,
    ) -> Result<(), AppWorkflowError> {
        let prepared = self
            .prepare_native_semantic_input(
                scope,
                task,
                context,
                &AppName::parse(&program.semantic_step)?,
                program.max_output_tokens,
                &claim,
            )
            .await;
        let prepared = match prepared {
            Ok(value) => value,
            Err(error) => {
                let completion = workflow_rounds::AppNativeModelCompletion::new(
                    &task.task_id,
                    context.run.execution_id(),
                    &claim,
                );
                completion.mark_finished(); // preparation has no provider I/O
                self.settle_native_round_participant(
                    scope,
                    task,
                    context,
                    &claim,
                    AppNativeRoundOutcome::Failed {
                        code: AppName::parse("context_admission_failed")?,
                        retryable: false,
                    },
                    Some(&completion),
                )
                .await?;
                return Err(error);
            },
        };
        let completion = prepared.completion();
        let result = context
            .owner
            .dispatch_recipe_semantic(
                scope,
                &task.task_id,
                context.run,
                context.node,
                context.permit,
                prepared,
            )
            .await;
        match result {
            Ok(result) => {
                self.settle_native_round_participant(
                    scope,
                    task,
                    context,
                    &claim,
                    AppNativeRoundOutcome::Prepared {
                        output: result.output,
                    },
                    Some(&completion),
                )
                .await?;
                self.commit_round_participant(
                    scope,
                    task,
                    context,
                    program,
                    input,
                    &claim.model().participant_id,
                )
                .await
            },
            Err(error) => {
                self.settle_native_round_participant(
                    scope,
                    task,
                    context,
                    &claim,
                    AppNativeRoundOutcome::Failed {
                        code: AppName::parse("semantic_dispatch_failed")?,
                        retryable: false,
                    },
                    Some(&completion),
                )
                .await?;
                Err(error)
            },
        }
    }

    /// A conversation observes earlier committed contributions. Independent
    /// snapshot rounds never enter this path. The refreshed view is retained
    /// before dispatch and reused verbatim across recovery and retries.
    async fn refresh_next_round_context(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &AppNativeProgressContext<'_>,
        program: &AppRoundProgramDeclaration,
        input: &Value,
    ) -> Result<(), AppWorkflowError> {
        loop {
            let (round, _) = self.native_round_snapshot(scope, task, context).await?;
            if round
                .participants()
                .iter()
                .any(|p| matches!(p.state(), AppRoundParticipantState::Prepared { .. }))
            {
                // An uncertain commit must settle before later participants
                // can observe it. Do not turn its draft into discussion data.
                return Err(AppWorkflowError::EffectSettlementUnresolved);
            }
            let Some(participant) = round
                .next_context_refresh()
                .map_err(|_| AppWorkflowError::CorruptBinding)?
            else {
                return Ok(());
            };
            let id = participant.participant_id();
            let mut references: BTreeMap<AppName, AppNativeContextReference> =
                serde_json::from_value(participant.context().clone())?;
            let state = self
                .require_run_state(
                    scope,
                    task,
                    context.run.execution_id(),
                    task.resolved_agent_id.as_str(),
                )
                .await?;
            let initial = projected_context(&state, &references)?;
            let person = initial
                .get("participant")
                .ok_or(AppWorkflowError::CorruptBinding)?;
            let prepared_at = Utc::now();
            let mut deferred = None;
            for per_participant in [false, true] {
                match Box::pin(self.round_queries(
                    scope,
                    task,
                    context,
                    program,
                    input,
                    person,
                    id,
                    per_participant,
                    "dispatch",
                    prepared_at,
                ))
                .await
                {
                    Ok(found) => references.extend(found),
                    Err(error) => {
                        tracing::warn!(task_id = %task.task_id, participant_id = id, %error,
                            "participant discussion context refresh failed; no model call");
                        deferred = Some("context_unavailable".to_owned());
                        break;
                    },
                }
            }
            if deferred.is_none() {
                let state = self
                    .require_run_state(
                        scope,
                        task,
                        context.run.execution_id(),
                        task.resolved_agent_id.as_str(),
                    )
                    .await?;
                let values = projected_context(&state, &references)?;
                let evaluated = program
                    .values
                    .evaluate(&AppRoundValueEnvironment {
                        input,
                        participant: person,
                        context: &values,
                        model: None,
                        item: None,
                        now: prepared_at,
                        run_id: context.run.execution_id(),
                        participant_id: id,
                    })
                    .map_err(program_error)?;
                deferred = program.exclusion(&evaluated);
            }
            let skipped = deferred.is_some();
            self.refresh_native_round_participant(
                scope,
                task,
                context,
                id,
                references,
                prepared_at,
                deferred,
            )
            .await?;
            if !skipped {
                return Ok(());
            }
        }
    }

    async fn commit_round_participant(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &AppNativeProgressContext<'_>,
        program: &AppRoundProgramDeclaration,
        input: &Value,
        participant_id: &str,
    ) -> Result<(), AppWorkflowError> {
        context.fence(scope, &task.task_id).await?;
        let state = self
            .require_run_state(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        let retained = state
            .native_rounds
            .get(&workflow_rounds::round_ref(
                context.permit.binding_digest(),
            )?)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let participant = retained
            .round
            .participants()
            .iter()
            .find(|item| item.participant_id() == participant_id)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let AppRoundParticipantState::Prepared { draft, .. } = participant.state() else {
            return Err(AppWorkflowError::CorruptBinding);
        };
        let references = serde_json::from_value(participant.context().clone())?;
        let values = projected_context(&state, &references)?;
        let person = values
            .get("participant")
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let environment = AppRoundValueEnvironment {
            input,
            participant: person,
            context: &values,
            model: Some(draft),
            item: None,
            now: participant
                .dispatch_prepared_at_ms()
                .and_then(DateTime::<Utc>::from_timestamp_millis)
                .unwrap_or(retained.prepared_at),
            run_id: context.run.execution_id(),
            participant_id,
        };
        let planned = (|| -> Result<_, AppWorkflowError> {
            let prepared = program
                .values
                .evaluate(&environment)
                .map_err(program_error)?;
            let semantic = program.semantic_outcome(&prepared).map_err(program_error)?;
            let plan = program
                .plan_mutations(false, &environment)
                .map_err(program_error)?;
            let quiet = match semantic {
                AppRoundSemanticOutcome::Draft if !plan.semantic_record_ids.is_empty() => None,
                AppRoundSemanticOutcome::Quiet(reason) if plan.semantic_record_ids.is_empty() => {
                    Some(reason)
                },
                _ => {
                    return Err(AppWorkflowError::Reconciliation(
                        "semantic result has no matching mutation",
                    ))
                },
            };
            Ok((plan, quiet))
        })();
        let (plan, quiet) = match planned {
            Ok(value) => value,
            Err(error) => {
                self.fail_prepared_native_participant(
                    scope,
                    task,
                    context,
                    participant_id,
                    AppName::parse("invalid_semantic_result")?,
                )
                .await?;
                return Err(error);
            },
        };
        let binding = context
            .run
            .plan()
            .node(context.node)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let participant_ref =
            workflow_rounds::participant_reference(binding.binding_digest(), participant_id)?;
        let key = AppNativeCommitKey::for_participant(binding, participant_ref.clone())?;
        let existing = AppWorkflowCommitTarget::native(&key)
            .retained_result(&state)?
            .cloned();
        let committed = if let Some(result) = existing {
            Some(result)
        } else if plan.operations.is_empty() {
            None
        } else {
            let parameters = HashMap::from([
                ("operations".to_owned(), json!(plan.operations)),
                (
                    "expected_record_revisions".to_owned(),
                    json!(plan.expected_record_revisions),
                ),
                (
                    "user_visible_summary".to_owned(),
                    json!(format!("Participant {participant_id} round progress")),
                ),
            ]);
            Some(
                self.commit_native_progress(scope, task, context, participant_ref, &parameters)
                    .await?,
            )
        };
        self.finish_native_round_participant(
            scope,
            task,
            context,
            participant_id,
            plan.semantic_record_ids,
            quiet,
            committed.as_ref(),
        )
        .await
    }
}

pub(super) fn validated_round_result(
    material: &AppRecipeRuntimeMaterial,
    binding: &crate::magician_v2::apps::recipe_lowering::AppRecipeLoweredNodeBinding,
    result: &AppActionResult<Value>,
) -> Result<(AppValidatedWorkflowValue, AppDigest), AppWorkflowError> {
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
    )?;
    Ok((
        value,
        AppDigest::blake3_canonical_json(&serde_json::to_value(result)?)?,
    ))
}
