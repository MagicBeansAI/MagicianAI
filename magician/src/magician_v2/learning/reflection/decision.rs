//! Only used procedures enter Jev; the complete reflection still owns candidates.
use super::*;
use crate::magician_v2::{
    decision_host::classification::{self},
    decisions::{observation, reference, runner, telemetry::Reference, text},
};
use decision_engine_contract::{batch::DecisionItem, request::DecisionState};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
const ENGINE: &str = "procedure_feedback";
const PROJECTION: &str = "procedure_feedback_used_v1";
const PROMPT: &str = "learning_reflection_procedure_feedback_v1";

pub(super) async fn review(
    runtime: &LearningReflectionRuntime,
    router: &OperationLlmRouter,
    system: &str,
    prompt: &str,
    schema: &str,
    usage: &LearningProcedureUsageContext,
    episode: &V3EpisodeRecord,
    store: &LearningStore,
    scope_owner: &LearningScope,
    related_episodes: &[V3EpisodeRecord],
    existing_candidates: &[serde_json::Value],
    existing_procedures: &[serde_json::Value],
) -> Result<text::Reviewed<Result<ReflectionOutput>>> {
    let scope = router.authoritative_trace_scope();
    let lookup = match scope.as_ref() {
        Some(s) => classification::ready_policy(ENGINE, &s.principal, &s.workspace).await,
        None => classification::unscoped_policy(),
    };
    let revision = reference::version(router, LEARNING_REFLECTION_OPERATION, PROMPT);
    let generate = |heads: text::Answers| async move {
        let prompt = if heads.is_empty() {
            prompt.to_owned()
        } else {
            text::locked_prompt(
                &format!(
                    "{prompt}\nDecision item indices correspond to used procedures: {}",
                    serde_json::to_string(
                        &usage
                            .used_procedures
                            .iter()
                            .map(|p| &p.procedure_id)
                            .collect::<Vec<_>>()
                    )?
                ),
                &heads,
            )
        };
        router
            .generate_for_operation_with_system_and_tool_schema(
                &LLMOperation::Other(LEARNING_REFLECTION_OPERATION.into()),
                Some(system),
                &prompt,
                Some(schema),
                None,
            )
            .await
    };
    let Some((scope, revision)) = scope
        .zip(revision)
        .filter(|_| !usage.is_empty() && usage.used_procedures.len() <= 64)
    else {
        anyhow::ensure!(
            usage.is_empty() || lookup.allows_incumbent(),
            "procedure decision deferred: missing scope/reference or input bounds"
        );
        let response = generate(BTreeMap::new()).await?;
        return Ok(text::Reviewed {
            value: parse_reflection_output(&response.content),
            response,
            guard: None,
            origins: BTreeMap::new(),
        });
    };
    let episode_json = bounded_reflection_json(
        episode_summary_value(episode),
        MAX_REFLECTION_EPISODE_JSON_CHARS,
    )?;
    let sources = usage
        .used_procedures
        .iter()
        .map(|p| store.read_procedure(scope_owner, &p.procedure_id))
        .collect::<Result<Vec<_>>>();
    let sources = sources.unwrap_or_default();
    let projection = |p: &super::super::LearningProcedure| {
        json!({"title":p.title,"summary":p.summary,
        "activation":p.activation,"workflow":p.workflow,"decision_points":p.decision_points,
        "verification":p.verification,"failure_modes":p.failure_modes})
    };
    let source_complete = sources.len() == usage.used_procedures.len()
        && sources
            .iter()
            .all(|p| serde_json::to_vec(&projection(p)).is_ok_and(|v| v.len() <= 12000));
    let items = sources
        .iter()
        .enumerate()
        .map(|(i, p)| DecisionItem {
            item_id: i.to_string(),
            state: DecisionState::from_json(projection(p)),
            choice_candidates: Default::default(),
        })
        .collect();
    anyhow::ensure!(
        source_complete || lookup.allows_incumbent(),
        "procedure decision deferred: incomplete sources"
    );
    let lookup = if source_complete {
        lookup
    } else {
        classification::PolicyLookup::Disabled
    };
    let mut input = runner::Input {
        operation: ENGINE.into(),
        projection_version: PROJECTION.into(),
        reference_version: revision,
        // The full reflection prompt includes related episodes and learning
        // summaries. A replay of a changed prompt must have a new case ID even
        // when its episode and used-procedure IDs are unchanged.
        case_id: blake3::hash(&serde_json::to_vec(&(
            system,
            prompt,
            schema,
            &usage.used_procedures,
            &episode_json,
        ))?)
        .to_hex()
        .to_string(),
        context: Some(DecisionState::from_text(episode_json)),
        items,
        required_questions: vec!["verdict".into(), "deprecation_recommended".into()],
        scope: router.classification_trace_context(scope),
        agent: Some(episode.agent_id.clone()),
        requires_completion: true,
        replay: None,
    };
    if let classification::PolicyLookup::Participating(participation) = &lookup {
        if observation::selected(&input, participation) {
            type Snapshot = (
                String,
                String,
                String,
                Vec<String>,
                String,
                String,
                String,
                Vec<(String, String, String)>,
                Vec<serde_json::Value>,
                Vec<serde_json::Value>,
                Vec<(String, String)>,
            );
            let identity = (|| {
                let episode_digest = source_digest(episode)?;
                let related = related_episodes
                    .iter()
                    .map(|related| {
                        Some((
                            related.agent_id.clone(),
                            related.episode_id.clone(),
                            source_digest(related)?,
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?;
                let procedures = sources
                    .iter()
                    .map(|procedure| Some((procedure.id.clone(), source_digest(procedure)?)))
                    .collect::<Option<Vec<_>>>()?;
                Some((episode_digest, related, procedures))
            })();
            if let Some((episode_digest, related, procedures)) = identity {
                let source = (
                    system,
                    prompt,
                    schema,
                    usage
                        .used_procedures
                        .iter()
                        .map(|p| p.procedure_id.clone())
                        .collect::<Vec<_>>(),
                    episode.agent_id.as_str(),
                    episode.episode_id.as_str(),
                    episode_digest.as_str(),
                    related,
                    existing_candidates,
                    existing_procedures,
                    procedures,
                );
                if let Some((snapshot, bytes)) =
                    observation::snapshot_bounded::<_, Snapshot>(&source)
                {
                    let snapshot = Arc::new(snapshot);
                    let check_snapshot = snapshot.clone();
                    let check_runtime = runtime.clone();
                    let check_store = store.clone();
                    let check_scope = scope_owner.clone();
                    let check_router = router.clone();
                    let check_trace_scope = input.scope.scope.clone();
                    let check_revision = input.reference_version.clone();
                    let access_snapshot = snapshot.clone();
                    let access_runtime = runtime.clone();
                    let access_store = store.clone();
                    let access_scope = scope_owner.clone();
                    let access_router = router.clone();
                    let access_trace_scope = input.scope.scope.clone();
                    let access_revision = input.reference_version.clone();
                    let run_scope = input.scope.scope.clone();
                    let run_router =
                        router.with_dispatch_priority(magicllm::dispatch::Priority::Background);
                    input.replay = Some(Ok(runner::ReferenceReplay {
                        bytes,
                        cost_reservation_microusd: reference::observation_cost_upper_microusd(
                            router,
                            LEARNING_REFLECTION_OPERATION,
                            bytes,
                        ),
                        current: Box::new(move || {
                            let snapshot = check_snapshot.clone();
                            let runtime = check_runtime.clone();
                            let store = check_store.clone();
                            let scope = check_scope.clone();
                            let router = check_router.clone();
                            let trace_scope = check_trace_scope.clone();
                            let revision = check_revision.clone();
                            Box::pin(async move {
                                if !router.observation_dispatch_available()
                                    || router.authoritative_trace_scope().as_ref()
                                        != Some(&trace_scope)
                                    || reference::version(
                                        &router,
                                        LEARNING_REFLECTION_OPERATION,
                                        PROMPT,
                                    )
                                    .as_ref()
                                        != Some(&revision)
                                {
                                    return false;
                                }
                                let service = AgentMemoryService::with_scoped_memory_scope(
                                    runtime
                                        .workspace_layout
                                        .memory_root(&scope.principal, &scope.workspace),
                                    &scope.principal,
                                    &scope.workspace,
                                );
                                let Ok(Some(episode)) = service
                                    .load_native_episode_by_id(&snapshot.4, &snapshot.5)
                                    .await
                                else {
                                    return false;
                                };
                                if source_digest(&episode).as_deref() != Some(snapshot.6.as_str()) {
                                    return false;
                                }
                                for (agent_id, episode_id, digest) in &snapshot.7 {
                                    let Ok(Some(related)) = service
                                        .load_native_episode_by_id(agent_id, episode_id)
                                        .await
                                    else {
                                        return false;
                                    };
                                    if source_digest(&related).as_deref() != Some(digest.as_str()) {
                                        return false;
                                    }
                                }
                                for (id, digest) in &snapshot.10 {
                                    let Ok(procedure) = store.read_procedure(&scope, id) else {
                                        return false;
                                    };
                                    if source_digest(&procedure).as_deref() != Some(digest.as_str())
                                    {
                                        return false;
                                    }
                                }
                                let Ok(candidates) =
                                    runtime.existing_candidate_summaries(&store, &scope)
                                else {
                                    return false;
                                };
                                let Ok(procedures) =
                                    runtime.existing_procedure_summaries(&store, &scope)
                                else {
                                    return false;
                                };
                                snapshot.8.iter().all(|item| candidates.contains(item))
                                    && snapshot.9.iter().all(|item| procedures.contains(item))
                            })
                        }),
                        access_current: Some(Box::new(move || {
                            let snapshot = access_snapshot.clone();
                            let runtime = access_runtime.clone();
                            let store = access_store.clone();
                            let scope = access_scope.clone();
                            let router = access_router.clone();
                            let trace_scope = access_trace_scope.clone();
                            let revision = access_revision.clone();
                            Box::pin(async move {
                                if !router.observation_dispatch_available()
                                    || router.authoritative_trace_scope().as_ref()
                                        != Some(&trace_scope)
                                    || reference::version(
                                        &router,
                                        LEARNING_REFLECTION_OPERATION,
                                        PROMPT,
                                    )
                                    .as_ref()
                                        != Some(&revision)
                                {
                                    return false;
                                }
                                let service = AgentMemoryService::with_scoped_memory_scope(
                                    runtime
                                        .workspace_layout
                                        .memory_root(&scope.principal, &scope.workspace),
                                    &scope.principal,
                                    &scope.workspace,
                                );
                                if !matches!(
                                    service
                                        .load_native_episode_by_id(&snapshot.4, &snapshot.5)
                                        .await,
                                    Ok(Some(_))
                                ) {
                                    return false;
                                }
                                for (agent_id, episode_id, _) in &snapshot.7 {
                                    if !matches!(
                                        service
                                            .load_native_episode_by_id(agent_id, episode_id)
                                            .await,
                                        Ok(Some(_))
                                    ) {
                                        return false;
                                    }
                                }
                                for (id, _) in &snapshot.10 {
                                    if store.read_procedure(&scope, id).is_err() {
                                        return false;
                                    }
                                }
                                for candidate in &snapshot.8 {
                                    let Some(id) =
                                        candidate.get("id").and_then(serde_json::Value::as_str)
                                    else {
                                        return false;
                                    };
                                    if store.read_candidate(&scope, id).is_err() {
                                        return false;
                                    }
                                }
                                for procedure in &snapshot.9 {
                                    let Some(id) =
                                        procedure.get("id").and_then(serde_json::Value::as_str)
                                    else {
                                        return false;
                                    };
                                    if store.read_procedure(&scope, id).is_err() {
                                        return false;
                                    }
                                }
                                true
                            })
                        })),
                        run: Box::new(move || {
                            Box::pin(async move {
                                let started = Instant::now();
                                let response = run_router
                                    .generate_for_operation_with_system_and_tool_schema(
                                        &LLMOperation::Other(LEARNING_REFLECTION_OPERATION.into()),
                                        Some(&snapshot.0),
                                        &snapshot.1,
                                        Some(&snapshot.2),
                                        None,
                                    )
                                    .await;
                                let Ok(response) = response else {
                                    return runner::ReplayResult {
                                        status: "failed",
                                        attempted: None,
                                        reference: None,
                                    };
                                };
                                reference::record_response(
                                    &response,
                                    &run_scope.principal,
                                    &run_scope.workspace,
                                    LEARNING_REFLECTION_OPERATION,
                                    started.elapsed(),
                                );
                                let parsed = parse_reflection_output(&response.content);
                                let labels = parsed
                                    .as_ref()
                                    .ok()
                                    .map(|review| feedback_labels(review, &snapshot.3))
                                    .unwrap_or_default();
                                let complete = parsed.as_ref().is_ok_and(|review| {
                                    review.procedure_feedback.len() == snapshot.3.len()
                                        && labels.len() == snapshot.3.len()
                                        && snapshot
                                            .3
                                            .iter()
                                            .collect::<std::collections::BTreeSet<_>>()
                                            .len()
                                            == snapshot.3.len()
                                });
                                runner::ReplayResult {
                                    status: if complete { "completed" } else { "failed" },
                                    attempted: Some(true),
                                    reference: Some(Reference::from_response(
                                        labels,
                                        &response,
                                        started.elapsed().as_millis() as u64,
                                    )),
                                }
                            })
                        }),
                    }));
                } else {
                    input.replay = Some(Err("snapshot_oversize"));
                }
            } else {
                input.replay = Some(Err("source_oversize"));
            }
        }
    }
    let mut reviewed = text::review(
        input,
        lookup,
        router,
        LEARNING_REFLECTION_OPERATION,
        PROMPT,
        Duration::from_secs(60),
        generate,
        |raw| Ok(parse_reflection_output(raw)),
        |r| {
            r.as_ref()
                .map(|r| {
                    usage
                        .used_procedures
                        .iter()
                        .enumerate()
                        .filter_map(|(i, p)| {
                            let f = r
                                .procedure_feedback
                                .iter()
                                .find(|f| f.procedure_id == p.procedure_id)?;
                            Some((
                                i.to_string(),
                                BTreeMap::from([
                                    (
                                        "verdict".into(),
                                        json!(LearningProcedureRunFeedbackVerdict::parse(
                                            &f.verdict
                                        )),
                                    ),
                                    (
                                        "deprecation_recommended".into(),
                                        json!(f.deprecation_recommended),
                                    ),
                                ]),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        },
        |r, heads| {
            let r = r
                .as_mut()
                .map_err(|e| anyhow!("invalid required reflection: {e}"))?;
            for (id, answers) in heads {
                let p = id
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| usage.used_procedures.get(i))
                    .ok_or_else(|| anyhow!("foreign procedure"))?;
                let f = r
                    .procedure_feedback
                    .iter_mut()
                    .find(|f| f.procedure_id == p.procedure_id)
                    .ok_or_else(|| anyhow!("required procedure feedback missing"))?;
                let (verdict, confidence) =
                    text::choice(answers, "verdict").ok_or_else(|| anyhow!("missing verdict"))?;
                let (deprecate, dc) = text::boolean(answers, "deprecation_recommended")
                    .ok_or_else(|| anyhow!("missing deprecation head"))?;
                anyhow::ensure!(
                    !f.rationale.trim().is_empty(),
                    "required feedback rationale missing"
                );
                // Reusing an incumbent text body is valid only for matching heads.
                // Contradictory rationale cannot support a destructive decision.
                anyhow::ensure!(
                    LearningProcedureRunFeedbackVerdict::parse(&f.verdict)
                        == LearningProcedureRunFeedbackVerdict::parse(&verdict)
                        && f.deprecation_recommended == deprecate,
                    "generated feedback reversed qualified heads"
                );
                f.verdict = verdict;
                f.confidence = Some(confidence.min(dc));
                f.deprecation_recommended = deprecate;
            }
            Ok(())
        },
    )
    .await?;
    if let Some(guard) = reviewed.guard.take() {
        let store = store.clone();
        let scope = scope_owner.clone();
        let versions = sources
            .iter()
            .map(|p| (p.id.clone(), p.version, p.updated_at))
            .collect::<Vec<_>>();
        reviewed.guard = Some(Arc::new((*guard).clone().with_source_check(move || {
            versions.iter().all(|(id, version, at)| {
                store
                    .read_procedure(&scope, id)
                    .is_ok_and(|p| p.version == *version && p.updated_at == *at)
            })
        })));
        anyhow::ensure!(reviewed.current(), "procedure source changed during review");
    }
    Ok(reviewed)
}

fn source_digest<T: serde::Serialize>(source: &T) -> Option<String> {
    let bytes = serde_json::to_vec(source).ok()?;
    (bytes.len() <= 256 * 1024).then(|| blake3::hash(&bytes).to_hex().to_string())
}

fn feedback_labels(
    review: &ReflectionOutput,
    used_ids: &[String],
) -> crate::magician_v2::decisions::telemetry::Labels {
    used_ids
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let feedback = review
                .procedure_feedback
                .iter()
                .find(|feedback| feedback.procedure_id == *id)?;
            Some((
                i.to_string(),
                BTreeMap::from([
                    (
                        "verdict".into(),
                        json!(LearningProcedureRunFeedbackVerdict::parse(
                            &feedback.verdict
                        )),
                    ),
                    (
                        "deprecation_recommended".into(),
                        json!(feedback.deprecation_recommended),
                    ),
                ]),
            ))
        })
        .collect()
}
