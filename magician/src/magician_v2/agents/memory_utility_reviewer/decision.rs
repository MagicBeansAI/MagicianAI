//! Utility classification retains the review/storage owner and its compaction fallback.
use super::*;
use crate::magician_v2::{
    decision_host::classification::{self, PolicyLookup},
    decisions::{mixed, observation, reference, runner, telemetry::Reference},
    query_analysis::operation_llm_router::SimplifiedLLMResponse,
};
use decision_engine_contract::{
    batch::DecisionItem,
    request::{Answer, DecisionState},
};
use std::time::{Duration, Instant};

const DECISION_ID: &str = "memory_utility_review";
const PROJECTION: &str = "memory_utility_v1";
const BUDGET: Duration = Duration::from_secs(60);
type Reviews = BTreeMap<String, Vec<MemoryTemperatureUtilityReviewJudgement>>;
struct Reviewed {
    rows: Reviews,
    response: SimplifiedLLMResponse,
    elapsed_ms: u64,
}

/// All three production paths (single, logically chunked run, maintenance batch)
/// use this seam. Shadow retains their original prompt and physical call shape.
pub(super) async fn review(
    service: &AgentMemoryService,
    router: &OperationLlmRouter,
    runs: &[MemoryTemperatureUtilityReviewInput],
    combined: bool,
    system: &str,
    original_prompt: &str,
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> Result<Reviews> {
    let lookup = match service.scoped_memory_scope() {
        Some((p, w)) => classification::ready_policy(DECISION_ID, &p, &w).await,
        None => classification::unscoped_policy(),
    };
    let Some((principal, workspace)) = service.scoped_memory_scope() else {
        anyhow::ensure!(
            lookup.allows_incumbent(),
            "memory decision deferred: missing scope"
        );
        return incumbent(
            router,
            runs,
            combined,
            system,
            Some(original_prompt),
            telemetry,
        )
        .await
        .map(|r| r.rows);
    };
    let Some(reference_version) = reference::version(
        router,
        MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION,
        MEMORY_TEMPERATURE_UTILITY_REVIEW_CONTRACT,
    ) else {
        anyhow::ensure!(
            lookup.allows_incumbent(),
            "memory decision deferred: missing reference"
        );
        return incumbent(
            router,
            runs,
            combined,
            system,
            Some(original_prompt),
            telemetry,
        )
        .await
        .map(|r| r.rows);
    };
    if !matches!(lookup, PolicyLookup::Participating(_)) {
        anyhow::ensure!(
            lookup.allows_incumbent(),
            "memory decision deferred: engine unavailable"
        );
        return incumbent(
            router,
            runs,
            combined,
            system,
            Some(original_prompt),
            telemetry,
        )
        .await
        .map(|r| r.rows);
    }
    let started = Instant::now();
    let (identities, context, items) = project(runs);
    if items.is_empty()
        || items.len()
            != runs
                .iter()
                .map(|r| r.selected_candidates.len())
                .sum::<usize>()
    {
        anyhow::ensure!(
            lookup.allows_incumbent(),
            "memory decision deferred: invalid projection"
        );
        return incumbent(
            router,
            runs,
            combined,
            system,
            Some(original_prompt),
            telemetry,
        )
        .await
        .map(|r| r.rows);
    }
    let mut scope =
        router.classification_trace_context(magicllm::LlmScope::new(principal, workspace));
    if runs.len() == 1 {
        scope.task_id = runs[0].task_id.clone().or(scope.task_id);
        scope.execution_id = runs[0].execution_id.clone().or(scope.execution_id);
        scope.chat_session_id = runs[0].chat_session_id.clone().or(scope.chat_session_id);
    }
    let case_id = blake3::hash(&serde_json::to_vec(&(&context, &items))?)
        .to_hex()
        .to_string();
    let mut input = runner::Input {
        operation: DECISION_ID.into(),
        projection_version: PROJECTION.into(),
        reference_version: reference_version.clone(),
        case_id,
        context: Some(context),
        items,
        required_questions: vec!["utility".into()],
        scope,
        agent: runs.first().map(|r| r.agent_id.clone()),
        requires_completion: true,
        replay: None,
    };
    if let PolicyLookup::Participating(participation) = &lookup {
        if observation::selected(&input, participation) {
            type Snapshot = (
                Vec<MemoryTemperatureUtilityReviewInput>,
                bool,
                String,
                String,
            );
            if let Some((snapshot, bytes)) = observation::snapshot_bounded::<_, Snapshot>(&(
                runs,
                combined,
                system,
                original_prompt,
            )) {
                let snapshot = std::sync::Arc::new(snapshot);
                let check_snapshot = snapshot.clone();
                let check_service = service.clone();
                let check_router = router.clone();
                let check_revision = reference_version.clone();
                let check_principal = principal.to_owned();
                let check_workspace = workspace.to_owned();
                let run_snapshot = snapshot.clone();
                let run_router =
                    router.with_dispatch_priority(magicllm::dispatch::Priority::Background);
                input.replay = Some(Ok(runner::ReferenceReplay {
                    bytes,
                    cost_reservation_microusd: reference::observation_cost_upper_microusd(
                        router,
                        MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION,
                        bytes,
                    ),
                    current: Box::new(move || {
                        let snapshot = check_snapshot.clone();
                        let service = check_service.clone();
                        let router = check_router.clone();
                        let revision = check_revision.clone();
                        let principal = check_principal.clone();
                        let workspace = check_workspace.clone();
                        Box::pin(async move {
                            if service.scoped_memory_scope()
                                != Some((principal.as_str(), workspace.as_str()))
                                || !router.observation_dispatch_available()
                                || router.authoritative_trace_scope().as_ref()
                                    != Some(&magicllm::LlmScope::new(&principal, &workspace))
                                || reference::version(
                                    &router,
                                    MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION,
                                    MEMORY_TEMPERATURE_UTILITY_REVIEW_CONTRACT,
                                )
                                .as_ref()
                                    != Some(&revision)
                            {
                                return false;
                            }
                            let Ok(queue) = super::load_utility_review_queue(&service).await else {
                                return false;
                            };
                            snapshot.0.iter().all(|run| {
                                queue
                                    .entries
                                    .get(&run.run_id)
                                    .is_some_and(|entry| entry.input == *run)
                            })
                        })
                    }),
                    access_current: None,
                    run: Box::new(move || {
                        Box::pin(async move {
                            let started = Instant::now();
                            if !run_router.observation_dispatch_available() {
                                return runner::ReplayResult {
                                    status: "dispatch_unavailable",
                                    attempted: Some(false),
                                    reference: None,
                                };
                            }
                            let review = incumbent(
                                &run_router,
                                &run_snapshot.0,
                                run_snapshot.1,
                                &run_snapshot.2,
                                Some(&run_snapshot.3),
                                None,
                            )
                            .await;
                            let Ok(review) = review else {
                                return runner::ReplayResult {
                                    status: "failed",
                                    attempted: None,
                                    reference: None,
                                };
                            };
                            let (identities, _, _) = project(&run_snapshot.0);
                            let labels = identities
                                .iter()
                                .filter_map(|(id, (index, key))| {
                                    review
                                        .rows
                                        .get(&run_snapshot.0[*index].run_id)
                                        .and_then(|rows| {
                                            rows.iter().find(|row| &row.memory_candidate_key == key)
                                        })
                                        .map(|row| {
                                            (
                                                id.clone(),
                                                BTreeMap::from([(
                                                    "utility".into(),
                                                    json!(row.label.as_str()),
                                                )]),
                                            )
                                        })
                                })
                                .collect();
                            runner::ReplayResult {
                                status: "completed",
                                attempted: Some(true),
                                reference: Some(Reference::from_response(
                                    labels,
                                    &review.response,
                                    started.elapsed().as_millis() as u64,
                                )),
                            }
                        })
                    }),
                }));
            } else {
                input.replay = Some(Err("snapshot_oversize"));
            }
        }
    }
    let all_count = identities.len();
    let outcome = runner::run(
        input,
        lookup,
        BUDGET.saturating_sub(started.elapsed()),
        runner::text_reserve(BUDGET),
        |ids, _| {
            let identities = &identities;
            async move {
                if ids.len() == all_count {
                    Some(
                        incumbent(
                            router,
                            runs,
                            combined,
                            system,
                            Some(original_prompt),
                            telemetry,
                        )
                        .await,
                    )
                } else {
                    let selected = select_runs(runs, &identities, &ids);
                    Some(incumbent(router, &selected, combined, system, None, telemetry).await)
                }
            }
        },
        |result| match result {
            Ok(review) => {
                let mut labels = BTreeMap::new();
                for (id, (index, key)) in &identities {
                    if let Some(row) = review
                        .rows
                        .get(&runs[*index].run_id)
                        .and_then(|rows| rows.iter().find(|r| &r.memory_candidate_key == key))
                    {
                        labels.insert(
                            id.clone(),
                            BTreeMap::from([("utility".into(), json!(row.label.as_str()))]),
                        );
                    }
                }
                Reference::from_response(labels, &review.response, review.elapsed_ms)
            },
            Err(_) => BTreeMap::new().into(),
        },
    )
    .await;
    let current = || {
        outcome.authority.as_ref().is_some_and(|p| p.is_current())
            && reference::version(
                router,
                MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION,
                MEMORY_TEMPERATURE_UTILITY_REVIEW_CONTRACT,
            )
            .as_ref()
                == Some(&reference_version)
    };
    let mut accepted = BTreeMap::new();
    for (id, answers) in outcome.current_answers() {
        let Some(Answer::Choice {
            choice, confidence, ..
        }) = answers.get("utility")
        else {
            continue;
        };
        let Some((run_index, key)) = identities.get(&id) else {
            continue;
        };
        let label = MemoryTemperatureUtilityLabel::from_review_label(choice.as_str());
        // Unknown is an explicit label, never a coercion target for another enum.
        if label.as_str() != choice.as_str() {
            continue;
        }
        accepted.insert(
            id.clone(),
            (
                *run_index,
                MemoryTemperatureUtilityReviewJudgement {
                    memory_candidate_key: key.clone(),
                    label,
                    confidence: Some(*confidence),
                    reason: outcome.origins.get(&id).map(|origin| {
                        format!(
                            "decision_model={}; pack={}@{}; batch={}",
                            origin.model.model, origin.pack, origin.pack_version, origin.batch_id
                        )
                    }),
                    compact_text: None,
                },
            ),
        );
    }
    let mut compact_ids = Vec::new();
    for (id, (index, row)) in &accepted {
        if runs[*index]
            .selected_candidates
            .iter()
            .find(|c| c.memory_candidate_key == row.memory_candidate_key)
            .is_some_and(|c| should_create_hot_projection(c, row))
        {
            compact_ids.push(id.clone());
        }
    }
    // One text call for useful/long accepted items only. Its labels are ignored;
    // source identity, approved utility and confidence remain immutable.
    let text_attempted = !compact_ids.is_empty() && current();
    let text = if !text_attempted {
        None
    } else {
        let selected = select_runs(runs, &identities, &compact_ids);
        let fixed = accepted.values().map(|(index, row)| json!({"run_id": runs[*index].run_id, "memory_candidate_key": row.memory_candidate_key, "label": row.label.as_str()})).collect::<Vec<_>>();
        let prompt = format!("Generate compact_text only for these approved utility decisions: {}. Preserve each fixed label and memory_candidate_key. Return JSON runs:[{{run_id,memories:[{{memory_candidate_key,label,compact_text}}]}}]. Do not invent facts, instructions or source ids. Source runs: {}", json!(fixed), json!(selected.iter().map(review_prompt_payload).collect::<Vec<_>>()));
        tokio::time::timeout(
            BUDGET.saturating_sub(started.elapsed()),
            incumbent(router, &selected, true, "Compact the provided source memory without changing the approved utility decisions. Return strict JSON.", Some(&prompt), telemetry),
        )
        .await
        .ok()
        .and_then(Result::ok)
    };
    if let Some(authority) = outcome.authority.as_ref() {
        authority.revalidate().await;
    }
    let mut completed = Vec::new();
    for (id, (index, mut row)) in accepted {
        let compact = text
            .as_ref()
            .and_then(|review| review.rows.get(&runs[index].run_id))
            .and_then(|rows| {
                rows.iter()
                    .find(|r| r.memory_candidate_key == row.memory_candidate_key)
            })
            .filter(|r| r.label == row.label)
            .and_then(|r| r.compact_text.clone());
        let need = if compact_ids.contains(&id) {
            mixed::TextNeed::Optional
        } else {
            mixed::TextNeed::None
        };
        if let mixed::Completed::Ready { text, .. } = mixed::complete(
            row.label,
            need,
            compact,
            || async { None },
            |_, text| projection_text_is_useful(text),
            current,
        )
        .await
        {
            row.compact_text = text;
            completed.push((index, row));
        }
    }
    if let Some(observation) = &outcome.observation {
        observation.complete(
            text.as_ref()
                .map(|r| Reference::from_response(BTreeMap::new(), &r.response, r.elapsed_ms)),
            text_attempted,
            completed.len() == outcome.answers.len()
                && outcome.incumbent.as_ref().is_none_or(Result::is_ok),
            started.elapsed(),
        );
    }
    let mut rows = match outcome.incumbent {
        Some(Ok(review)) => review.rows,
        Some(Err(error)) => return Err(error),
        _ => BTreeMap::new(),
    };
    for (index, row) in completed {
        rows.entry(runs[index].run_id.clone())
            .or_default()
            .push(row);
    }
    if rows.is_empty() {
        anyhow::bail!("memory utility decision had no current validated result")
    }
    Ok(rows)
}

fn project(
    runs: &[MemoryTemperatureUtilityReviewInput],
) -> (
    BTreeMap<String, (usize, String)>,
    DecisionState,
    Vec<DecisionItem>,
) {
    let mut identities = BTreeMap::new();
    let mut contexts = Vec::new();
    let mut items = Vec::new();
    for (run_index, run) in runs.iter().enumerate() {
        let mut projection = review_prompt_payload(run);
        let memories = projection
            .as_object_mut()
            .and_then(|p| p.remove("injected_memories"));
        // Transport correlation uses ordinals; durable source ids stay at the owner.
        if let Some(context_run) = projection.get_mut("run").and_then(Value::as_object_mut) {
            for key in [
                "run_id",
                "agent_id",
                "task_id",
                "execution_id",
                "chat_session_id",
            ] {
                context_run.remove(key);
            }
        }
        contexts.push(projection);
        for (candidate_index, mut memory) in memories
            .and_then(|m| m.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            let Some(key) = memory
                .get("memory_candidate_key")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            let id = format!("{run_index}:{candidate_index}");
            if let Some(object) = memory.as_object_mut() {
                object.remove("memory_candidate_key");
                object.remove("source_key");
            }
            identities.insert(id.clone(), (run_index, key));
            items.push(DecisionItem {
                item_id: id,
                state: DecisionState::from_json(json!({"run_index": run_index, "memory": memory})),
                choice_candidates: Default::default(),
            });
        }
    }
    (
        identities,
        DecisionState::from_json(json!({"runs": contexts})),
        items,
    )
}

fn select_runs(
    runs: &[MemoryTemperatureUtilityReviewInput],
    identities: &BTreeMap<String, (usize, String)>,
    ids: &[String],
) -> Vec<MemoryTemperatureUtilityReviewInput> {
    let wanted: BTreeSet<_> = ids
        .iter()
        .filter_map(|id| identities.get(id))
        .cloned()
        .collect();
    runs.iter()
        .enumerate()
        .filter_map(|(index, run)| {
            let selected_candidates = run
                .selected_candidates
                .iter()
                .filter(|c| wanted.contains(&(index, c.memory_candidate_key.clone())))
                .cloned()
                .collect::<Vec<_>>();
            (!selected_candidates.is_empty()).then(|| MemoryTemperatureUtilityReviewInput {
                selected_candidates,
                run_id: run.run_id.clone(),
                agent_id: run.agent_id.clone(),
                task_id: run.task_id.clone(),
                execution_id: run.execution_id.clone(),
                chat_session_id: run.chat_session_id.clone(),
                goal: run.goal.clone(),
                outcome: run.outcome.clone(),
                final_answer: run.final_answer.clone(),
                action_trace: run.action_trace.clone(),
            })
        })
        .collect()
}

async fn incumbent(
    router: &OperationLlmRouter,
    runs: &[MemoryTemperatureUtilityReviewInput],
    combined: bool,
    system: &str,
    original_prompt: Option<&str>,
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> Result<Reviewed> {
    let operation = LLMOperation::Other(MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION.into());
    let payload = if combined {
        json!({"runs": runs.iter().map(review_prompt_payload).collect::<Vec<_>>()})
    } else {
        review_prompt_payload(&runs[0])
    };
    let prompt = original_prompt.map(str::to_owned).unwrap_or_else(|| format!(
        "Review only the offered injected memories against each completed run. Labels: load_bearing (necessary), useful (materially helped), referenced (mentioned only), irrelevant (did not help), stale (outdated), harmful (wrong or misleading), unknown (insufficient evidence). Return strict JSON with {}. Each memory has memory_candidate_key, label, confidence, reason and optional compact_text for useful/load_bearing items. Input:\n{}",
        if combined { "runs:[{run_id,memories:[...]}]" } else { "memories:[...]" }, payload));
    let started = Instant::now();
    let response = if combined {
        router
            .generate_for_operation_with_system(&operation, Some(system), &prompt)
            .await?
    } else {
        router
            .generate_for_chunkable_operation_with_system(
                &operation,
                Some(system),
                &prompt,
                serde_json::to_value(&runs[0])?,
                None,
            )
            .await?
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if telemetry.is_none() {
        if let Some(scope) = router.authoritative_trace_scope() {
            reference::record_response(
                &response,
                &scope.principal,
                &scope.workspace,
                MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION,
                started.elapsed(),
            );
        }
    }
    let allowed: BTreeMap<_, _> = runs
        .iter()
        .map(|r| {
            (
                r.run_id.clone(),
                r.selected_candidates
                    .iter()
                    .map(|c| c.memory_candidate_key.clone())
                    .collect(),
            )
        })
        .collect();
    let parsed = if combined {
        parse_memory_utility_batch_review_output(&response.content, &allowed)
    } else {
        parse_memory_utility_review_output(&response.content, &allowed[&runs[0].run_id])
            .map(|rows| BTreeMap::from([(runs[0].run_id.clone(), rows)]))
    };
    if let Some(telemetry) = telemetry {
        let attribution = if runs.len() == 1 {
            utility_review_attribution(&runs[0])
        } else {
            Default::default()
        };
        match &parsed {
            Ok(_) => telemetry.emit_validated_success(
                MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION,
                &response,
                elapsed_ms,
                attribution,
                "memory_utility_review",
            ),
            Err(error) => telemetry.emit_validation_failure(
                MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION,
                &response,
                elapsed_ms,
                attribution,
                "memory_utility_review",
                error,
            ),
        }
    }
    Ok(Reviewed {
        rows: parsed.map_err(|e| anyhow!("memory utility review output was not valid: {e}"))?,
        response,
        elapsed_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::queue_review_input;
    use super::*;

    #[test]
    fn memory_decision_utility_projection_excludes_raw_keys_and_bounds_large_source() {
        let mut run = queue_review_input("private-run-key", "completed");
        run.selected_candidates[0].text = "记".repeat(100_000);
        run.selected_candidates[0].source_text = "source".repeat(100_000);
        let (_, context, items) = project(&[run]);
        let raw = serde_json::to_string(&(context, &items)).unwrap();
        assert!(!raw.contains("private-run-key"));
        assert!(!raw.contains("source_text"));
        assert_eq!(items.len(), 1);
        assert!(raw.len() < 4_000);
        assert_eq!(
            items[0].state.0["memory"]["text"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            MAX_MEMORY_TEXT_CHARS
        );
    }

    #[test]
    fn memory_decision_utility_subset_cannot_introduce_foreign_keys_or_cross_runs() {
        let first = queue_review_input("first", "completed");
        let mut second = queue_review_input("second", "completed");
        second.selected_candidates[0].memory_candidate_key =
            first.selected_candidates[0].memory_candidate_key.clone();
        let runs = [first, second];
        let (identities, _, _) = project(&runs);
        let selected = select_runs(&runs, &identities, &["1:0".into(), "foreign".into()]);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].run_id, "second");
        assert_eq!(
            selected[0].selected_candidates[0].source_key,
            "episode-second"
        );
        assert!(select_runs(&runs, &identities, &["foreign".into()]).is_empty());
    }

    #[test]
    fn memory_decision_utility_local_only_never_offered_to_hosted_classifier() {
        let mut run = queue_review_input("local", "completed");
        run.selected_candidates[0].app_model_processing =
            Some(crate::magician_v2::apps::models::AppModelProcessing::LocalOnly);
        assert!(project(&[run]).2.is_empty());
    }
}
