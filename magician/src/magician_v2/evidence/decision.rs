//! Promote/importance classification; source owners still validate and stamp records.
use super::{
    parse_evidence_proposal,
    screen_distill::{
        cluster_screen_observations, render_screen_prompt, ScreenObservationCluster,
        ScreenObservationRow,
    },
    tier_distill::{cluster_tier_entries, producer_spec, TierCluster},
    EvidenceDecisionGuard, EvidenceProposal,
};
use crate::magician_v2::{
    agents::AgentMemoryService,
    artifact_v2::memory::V3EpisodeRecord,
    decision_host::classification::{self, PolicyLookup},
    decisions::{mixed, observation, reference, runner, telemetry::Reference},
    prompts::PromptManager,
    query_analysis::operation_llm_router::{OperationLlmRouter, SimplifiedLLMResponse},
};
use decision_engine_contract::{
    batch::DecisionItem,
    request::{Answer, DecisionState},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    io::{self, Write},
    sync::Arc,
    time::{Duration, Instant},
};

const DECISION_ID: &str = "evidence_promote";
const PROJECTION: &str = "evidence_promote_v1_importance_quarters";
const BUDGET: Duration = Duration::from_secs(60);
pub(super) struct Reviewed {
    pub proposal: EvidenceProposal,
    pub response: SimplifiedLLMResponse,
    elapsed_ms: u64,
}

pub(super) enum EvidenceReplaySource<'a> {
    Episode {
        service: &'a AgentMemoryService,
        episode: &'a V3EpisodeRecord,
        prompts: &'a Arc<PromptManager>,
    },
    Tier {
        service: &'a AgentMemoryService,
        cluster: &'a TierCluster,
        producer: &'a str,
        prompts: &'a Arc<PromptManager>,
    },
    Screen {
        service: &'a AgentMemoryService,
        cluster: &'a ScreenObservationCluster,
        prompts: &'a Arc<PromptManager>,
        source_digest: &'a str,
    },
}

#[derive(Serialize)]
struct EpisodeReplaySnapshotRef<'a> {
    mapping: &'a str,
    system: &'a str,
    user: &'a str,
    episode: &'a V3EpisodeRecord,
}

#[derive(Deserialize)]
struct EpisodeReplaySnapshot {
    mapping: String,
    system: String,
    user: String,
    episode: V3EpisodeRecord,
}

#[derive(Serialize)]
struct TierReplaySnapshotRef<'a> {
    mapping: &'a str,
    system: &'a str,
    user: &'a str,
    cluster: &'a TierCluster,
    producer: &'a str,
}

#[derive(Deserialize)]
struct TierReplaySnapshot {
    mapping: String,
    system: String,
    user: String,
    cluster: TierCluster,
    producer: String,
}

#[derive(Serialize)]
struct ScreenReplaySnapshotRef<'a> {
    mapping: &'a str,
    system: &'a str,
    user: &'a str,
    cluster: &'a ScreenObservationCluster,
    source_digest: &'a str,
}

#[derive(Deserialize)]
struct ScreenReplaySnapshot {
    mapping: String,
    system: String,
    user: String,
    cluster: ScreenObservationCluster,
    source_digest: String,
}

struct DigestWriter<'a>(&'a mut blake3::Hasher);

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn screen_tier_digest(source_tier: &Value) -> Option<String> {
    source_tier.as_array()?;
    let mut hasher = blake3::Hasher::new();
    serde_json::to_writer(DigestWriter(&mut hasher), source_tier).ok()?;
    Some(hasher.finalize().to_hex().to_string())
}

async fn screen_replay_current(
    service: &AgentMemoryService,
    prompts: &PromptManager,
    snapshot: &ScreenReplaySnapshot,
) -> bool {
    if snapshot.mapping != "screen_evidence_distill" {
        return false;
    }
    let Ok(knowledge) = service.load_user_knowledge().await else {
        return false;
    };
    let Some(tier) =
        crate::magician_v2::chat::service::normalized_user_memory_tier_name("screen_observations")
    else {
        return false;
    };
    let Some(source_tier) = knowledge.get(tier.as_str()) else {
        return false;
    };
    if screen_tier_digest(source_tier).as_deref() != Some(snapshot.source_digest.as_str()) {
        return false;
    }
    let Some(entries) = source_tier.as_array() else {
        return false;
    };
    let rows: Vec<ScreenObservationRow> = entries
        .iter()
        .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
        .collect();
    let Some(current) = cluster_screen_observations(&rows)
        .into_iter()
        .find(|cluster| {
            cluster.purpose == snapshot.cluster.purpose && cluster.day == snapshot.cluster.day
        })
    else {
        return false;
    };
    let Some((current, _)) = observation::snapshot_bounded::<_, ScreenObservationCluster>(&current)
    else {
        return false;
    };
    if serde_json::to_vec(&current).ok() != serde_json::to_vec(&snapshot.cluster).ok() {
        return false;
    }
    matches!(
        render_screen_prompt(&snapshot.cluster, prompts).await,
        Ok((system, user)) if system == snapshot.system && user == snapshot.user
    )
}

async fn episode_replay_current(
    service: &AgentMemoryService,
    prompts: &PromptManager,
    snapshot: &EpisodeReplaySnapshot,
) -> bool {
    let Ok(Some(current)) = service
        .load_native_episode_by_id(&snapshot.episode.agent_id, &snapshot.episode.episode_id)
        .await
    else {
        return false;
    };
    let Some((current, _)) = observation::snapshot_bounded::<_, V3EpisodeRecord>(&current) else {
        return false;
    };
    if serde_json::to_vec(&current).ok() != serde_json::to_vec(&snapshot.episode).ok() {
        return false;
    }
    let Ok(system) = prompts
        .get_rendered_prompt("evidence_distill_system", "1.0.0", HashMap::new())
        .await
    else {
        return false;
    };
    let Ok(episode_json) = serde_json::to_string_pretty(&snapshot.episode) else {
        return false;
    };
    let Ok(user) = prompts
        .get_rendered_prompt(
            "evidence_distill_user",
            "1.0.0",
            HashMap::from([("episode_json".to_string(), episode_json)]),
        )
        .await
    else {
        return false;
    };
    system == snapshot.system && user == snapshot.user
}

async fn tier_replay_current(
    service: &AgentMemoryService,
    prompts: &PromptManager,
    snapshot: &TierReplaySnapshot,
) -> bool {
    let Some(spec) = producer_spec(&snapshot.producer) else {
        return false;
    };
    if snapshot.mapping != "tier_evidence_distill" || spec.producer != snapshot.producer {
        return false;
    }
    let Ok(knowledge) = service.load_user_knowledge().await else {
        return false;
    };
    let Some(tier) = crate::magician_v2::chat::service::normalized_user_memory_tier_name(spec.tier)
    else {
        return false;
    };
    let Some(entries) = knowledge.get(tier.as_str()).and_then(Value::as_array) else {
        return false;
    };
    let Some(current) = cluster_tier_entries(entries, &spec)
        .into_iter()
        .find(|cluster| {
            cluster.slug == snapshot.cluster.slug && cluster.day == snapshot.cluster.day
        })
    else {
        return false;
    };
    let Some((current, _)) = observation::snapshot_bounded::<_, TierCluster>(&current) else {
        return false;
    };
    if serde_json::to_vec(&current).ok() != serde_json::to_vec(&snapshot.cluster).ok() {
        return false;
    }
    let payload = if snapshot.cluster.rows.len() == 1 {
        snapshot.cluster.rows[0].clone()
    } else {
        Value::Array(snapshot.cluster.rows.clone())
    };
    tier_prompt_current(prompts, snapshot, &payload).await
}

async fn tier_prompt_current(
    prompts: &PromptManager,
    snapshot: &TierReplaySnapshot,
    payload: &Value,
) -> bool {
    let Some(spec) = producer_spec(&snapshot.producer) else {
        return false;
    };
    let Ok(system) = prompts
        .get_rendered_prompt("tier_evidence_distill_system", "1.0.0", HashMap::new())
        .await
    else {
        return false;
    };
    let Ok(record_json) = serde_json::to_string_pretty(payload) else {
        return false;
    };
    let Ok(user) = prompts
        .get_rendered_prompt(
            "tier_evidence_distill_user",
            "1.0.0",
            HashMap::from([
                ("domain".to_string(), spec.domain.to_string()),
                ("record_json".to_string(), record_json),
            ]),
        )
        .await
    else {
        return false;
    };
    system == snapshot.system && user == snapshot.user
}

fn evidence_reference_labels(
    content: &str,
) -> Option<crate::magician_v2::decisions::telemetry::Labels> {
    let value: Value = serde_json::from_str(content.trim()).ok()?;
    let promote = value.get("promote")?.as_bool()?;
    let importance = value.get("importance")?.as_f64()?;
    if !importance.is_finite() || !(0.0..=1.0).contains(&importance) {
        return None;
    }
    Some(BTreeMap::from([(
        "0".into(),
        BTreeMap::from([
            ("promote".into(), json!(promote)),
            ("importance".into(), json!((importance * 4.0).round() / 4.0)),
        ]),
    )]))
}

/// A pinned provider is a stronger content boundary than the engine's general
/// locality opt-in. Such calls retain that provider; hosted Jev never sees them.
pub(super) async fn review<G, F>(
    router: &OperationLlmRouter,
    mapping: &str,
    system: &str,
    source: &str,
    provider_pinned: bool,
    replay_source: Option<EvidenceReplaySource<'_>>,
    generate: G,
) -> anyhow::Result<Reviewed>
where
    G: Fn() -> F,
    F: Future<Output = anyhow::Result<SimplifiedLLMResponse>>,
{
    let invoke = || async {
        let started = Instant::now();
        let response = generate().await?;
        let mut proposal = parse_evidence_proposal(&response.content)?;
        // Authority can only be attached by this host after engine qualification.
        proposal.decision_origin = None;
        proposal.decision_guard = None;
        Ok::<_, anyhow::Error>(Reviewed {
            proposal,
            response,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    };
    let Some(scope) = router.authoritative_trace_scope() else {
        anyhow::ensure!(
            classification::unscoped_policy().allows_incumbent(),
            "evidence decision deferred: missing scope"
        );
        return invoke().await;
    };
    let lookup =
        classification::ready_policy(DECISION_ID, &scope.principal, &scope.workspace).await;
    let prompt_version = format!(
        "evidence_proposal_v1:{mapping}:{}",
        blake3::hash(system.as_bytes()).to_hex()
    );
    let Some(revision) = reference::version(router, mapping, &prompt_version) else {
        anyhow::ensure!(
            lookup.allows_incumbent(),
            "evidence decision deferred: missing reference"
        );
        return invoke().await;
    };
    if provider_pinned || source.len() > 12000 {
        anyhow::ensure!(
            lookup.allows_incumbent(),
            "evidence decision deferred: provider boundary or input bounds"
        );
        return invoke().await;
    }
    if !matches!(lookup, PolicyLookup::Participating(_)) {
        anyhow::ensure!(
            lookup.allows_incumbent(),
            "evidence decision deferred: engine unavailable"
        );
        return invoke().await;
    }
    let started = Instant::now();
    let mut input = runner::Input {
        operation: DECISION_ID.into(),
        projection_version: PROJECTION.into(),
        reference_version: revision.clone(),
        case_id: blake3::hash(source.as_bytes()).to_hex().to_string(),
        context: None,
        items: vec![DecisionItem {
            item_id: "0".into(),
            state: DecisionState::from_json(json!({"source_kind": mapping, "source": source})),
            choice_candidates: Default::default(),
        }],
        required_questions: vec!["promote".into(), "importance".into()],
        scope: router.classification_trace_context(scope),
        agent: None,
        requires_completion: true,
        replay: None,
    };
    if let PolicyLookup::Participating(participation) = &lookup {
        if observation::selected(&input, participation) {
            match replay_source {
                Some(EvidenceReplaySource::Episode {
                    service,
                    episode,
                    prompts,
                }) => {
                    let snapshot_source = EpisodeReplaySnapshotRef {
                        mapping,
                        system,
                        user: source,
                        episode,
                    };
                    match observation::snapshot_bounded::<_, EpisodeReplaySnapshot>(
                        &snapshot_source,
                    ) {
                        Some((snapshot, bytes)) => {
                            let snapshot = Arc::new(snapshot);
                            let check_snapshot = snapshot.clone();
                            let check_service = service.clone();
                            let check_prompts = prompts.clone();
                            let check_router = router.clone();
                            let run_router = router.clone();
                            let check_revision = revision.clone();
                            let check_prompt_version = prompt_version.clone();
                            let principal = input.scope.scope.principal.clone();
                            let workspace = input.scope.scope.workspace.clone();
                            let run_principal = principal.clone();
                            let run_workspace = workspace.clone();
                            input.replay = Some(Ok(runner::ReferenceReplay {
                                bytes,
                                cost_reservation_microusd:
                                    reference::observation_cost_upper_microusd(
                                        router, mapping, bytes,
                                    ),
                                current: Box::new(move || {
                                    let snapshot = check_snapshot.clone();
                                    let service = check_service.clone();
                                    let prompts = check_prompts.clone();
                                    let router = check_router.clone();
                                    let revision = check_revision.clone();
                                    let prompt_version = check_prompt_version.clone();
                                    let principal = principal.clone();
                                    let workspace = workspace.clone();
                                    Box::pin(async move {
                                        service.scoped_memory_scope()
                                            == Some((principal.as_str(), workspace.as_str()))
                                            && router.authoritative_trace_scope().as_ref()
                                                == Some(&magicllm::LlmScope::new(
                                                    &principal, &workspace,
                                                ))
                                            && router.observation_dispatch_available()
                                            && reference::version(
                                                &router,
                                                &snapshot.mapping,
                                                &prompt_version,
                                            )
                                            .as_ref()
                                                == Some(&revision)
                                            && episode_replay_current(&service, &prompts, &snapshot)
                                                .await
                                    })
                                }),
                                access_current: None,
                                run: Box::new(move || {
                                    Box::pin(async move {
                                        if !run_router.observation_dispatch_available() {
                                            return runner::ReplayResult {
                                                status: "dispatch_unavailable",
                                                attempted: Some(false),
                                                reference: None,
                                            };
                                        }
                                        let started = Instant::now();
                                        let Ok(response) = reference::pinned_json_observation(
                                            &run_router,
                                            &snapshot.mapping,
                                            &run_principal,
                                            &run_workspace,
                                            &snapshot.system,
                                            &snapshot.user,
                                        )
                                        .await
                                        else {
                                            return runner::ReplayResult {
                                                status: "failed",
                                                attempted: None,
                                                reference: None,
                                            };
                                        };
                                        let labels = evidence_reference_labels(&response.content);
                                        runner::ReplayResult {
                                            status: if labels.is_some() {
                                                "completed"
                                            } else {
                                                "failed"
                                            },
                                            attempted: Some(true),
                                            reference: Some(Reference::from_response(
                                                labels.unwrap_or_default(),
                                                &response,
                                                started.elapsed().as_millis() as u64,
                                            )),
                                        }
                                    })
                                }),
                            }));
                        },
                        None => input.replay = Some(Err("snapshot_oversize")),
                    }
                },
                Some(EvidenceReplaySource::Tier {
                    service,
                    cluster,
                    producer,
                    prompts,
                }) => {
                    let snapshot_source = TierReplaySnapshotRef {
                        mapping,
                        system,
                        user: source,
                        cluster,
                        producer,
                    };
                    match observation::snapshot_bounded::<_, TierReplaySnapshot>(&snapshot_source) {
                        Some((snapshot, bytes)) => {
                            let snapshot = Arc::new(snapshot);
                            let check_snapshot = snapshot.clone();
                            let check_service = service.clone();
                            let check_prompts = prompts.clone();
                            let check_router = router.clone();
                            let run_router = router.clone();
                            let check_revision = revision.clone();
                            let check_prompt_version = prompt_version.clone();
                            let principal = input.scope.scope.principal.clone();
                            let workspace = input.scope.scope.workspace.clone();
                            let run_principal = principal.clone();
                            let run_workspace = workspace.clone();
                            input.replay = Some(Ok(runner::ReferenceReplay {
                                bytes,
                                cost_reservation_microusd:
                                    reference::observation_cost_upper_microusd(
                                        router, mapping, bytes,
                                    ),
                                current: Box::new(move || {
                                    let snapshot = check_snapshot.clone();
                                    let service = check_service.clone();
                                    let prompts = check_prompts.clone();
                                    let router = check_router.clone();
                                    let revision = check_revision.clone();
                                    let prompt_version = check_prompt_version.clone();
                                    let principal = principal.clone();
                                    let workspace = workspace.clone();
                                    Box::pin(async move {
                                        service.scoped_memory_scope()
                                            == Some((principal.as_str(), workspace.as_str()))
                                            && router.authoritative_trace_scope().as_ref()
                                                == Some(&magicllm::LlmScope::new(
                                                    &principal, &workspace,
                                                ))
                                            && router.observation_dispatch_available()
                                            && reference::version(
                                                &router,
                                                &snapshot.mapping,
                                                &prompt_version,
                                            )
                                            .as_ref()
                                                == Some(&revision)
                                            && tier_replay_current(&service, &prompts, &snapshot)
                                                .await
                                    })
                                }),
                                access_current: None,
                                run: Box::new(move || {
                                    Box::pin(async move {
                                        if !run_router.observation_dispatch_available() {
                                            return runner::ReplayResult {
                                                status: "dispatch_unavailable",
                                                attempted: Some(false),
                                                reference: None,
                                            };
                                        }
                                        let started = Instant::now();
                                        let Ok(response) = reference::pinned_json_observation(
                                            &run_router,
                                            &snapshot.mapping,
                                            &run_principal,
                                            &run_workspace,
                                            &snapshot.system,
                                            &snapshot.user,
                                        )
                                        .await
                                        else {
                                            return runner::ReplayResult {
                                                status: "failed",
                                                attempted: None,
                                                reference: None,
                                            };
                                        };
                                        let labels = evidence_reference_labels(&response.content);
                                        runner::ReplayResult {
                                            status: if labels.is_some() {
                                                "completed"
                                            } else {
                                                "failed"
                                            },
                                            attempted: Some(true),
                                            reference: Some(Reference::from_response(
                                                labels.unwrap_or_default(),
                                                &response,
                                                started.elapsed().as_millis() as u64,
                                            )),
                                        }
                                    })
                                }),
                            }));
                        },
                        None => input.replay = Some(Err("snapshot_oversize")),
                    }
                },
                Some(EvidenceReplaySource::Screen {
                    service,
                    cluster,
                    prompts,
                    source_digest,
                }) => {
                    let snapshot_source = ScreenReplaySnapshotRef {
                        mapping,
                        system,
                        user: source,
                        cluster,
                        source_digest,
                    };
                    match observation::snapshot_bounded::<_, ScreenReplaySnapshot>(&snapshot_source)
                    {
                        Some((snapshot, bytes)) => {
                            let snapshot = Arc::new(snapshot);
                            let check_snapshot = snapshot.clone();
                            let check_service = service.clone();
                            let check_prompts = prompts.clone();
                            let check_router = router.clone();
                            let run_router = router.clone();
                            let check_revision = revision.clone();
                            let check_prompt_version = prompt_version.clone();
                            let principal = input.scope.scope.principal.clone();
                            let workspace = input.scope.scope.workspace.clone();
                            let run_principal = principal.clone();
                            let run_workspace = workspace.clone();
                            input.replay = Some(Ok(runner::ReferenceReplay {
                                bytes,
                                cost_reservation_microusd:
                                    reference::observation_cost_upper_microusd(
                                        router, mapping, bytes,
                                    ),
                                current: Box::new(move || {
                                    let snapshot = check_snapshot.clone();
                                    let service = check_service.clone();
                                    let prompts = check_prompts.clone();
                                    let router = check_router.clone();
                                    let revision = check_revision.clone();
                                    let prompt_version = check_prompt_version.clone();
                                    let principal = principal.clone();
                                    let workspace = workspace.clone();
                                    Box::pin(async move {
                                        service.scoped_memory_scope()
                                            == Some((principal.as_str(), workspace.as_str()))
                                            && router.authoritative_trace_scope().as_ref()
                                                == Some(&magicllm::LlmScope::new(
                                                    &principal, &workspace,
                                                ))
                                            && router.observation_dispatch_available()
                                            && reference::version(
                                                &router,
                                                &snapshot.mapping,
                                                &prompt_version,
                                            )
                                            .as_ref()
                                                == Some(&revision)
                                            && screen_replay_current(&service, &prompts, &snapshot)
                                                .await
                                    })
                                }),
                                access_current: None,
                                run: Box::new(move || {
                                    Box::pin(async move {
                                        if !run_router.observation_dispatch_available() {
                                            return runner::ReplayResult {
                                                status: "dispatch_unavailable",
                                                attempted: Some(false),
                                                reference: None,
                                            };
                                        }
                                        let started = Instant::now();
                                        let Ok(response) = reference::pinned_json_observation(
                                            &run_router,
                                            &snapshot.mapping,
                                            &run_principal,
                                            &run_workspace,
                                            &snapshot.system,
                                            &snapshot.user,
                                        )
                                        .await
                                        else {
                                            return runner::ReplayResult {
                                                status: "failed",
                                                attempted: None,
                                                reference: None,
                                            };
                                        };
                                        let labels = evidence_reference_labels(&response.content);
                                        runner::ReplayResult {
                                            status: if labels.is_some() {
                                                "completed"
                                            } else {
                                                "failed"
                                            },
                                            attempted: Some(true),
                                            reference: Some(Reference::from_response(
                                                labels.unwrap_or_default(),
                                                &response,
                                                started.elapsed().as_millis() as u64,
                                            )),
                                        }
                                    })
                                }),
                            }));
                        },
                        None => input.replay = Some(Err("snapshot_oversize")),
                    }
                },
                None => input.replay = Some(Err("source_unbound")),
            }
        }
    }
    let outcome = runner::run(
        input,
        lookup,
        BUDGET,
        runner::text_reserve(BUDGET),
        |_, _| async { Some(invoke().await) },
        |review| match review {
            Ok(review) => {
                let mut labels =
                    BTreeMap::from([("promote".into(), json!(review.proposal.promote))]);
                if let Some(value) = review
                    .proposal
                    .importance
                    .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                {
                    labels.insert("importance".into(), json!((value * 4.0).round() / 4.0));
                }
                Reference::from_response(
                    BTreeMap::from([("0".into(), labels)]),
                    &review.response,
                    review.elapsed_ms,
                )
            },
            Err(_) => BTreeMap::new().into(),
        },
    )
    .await;
    let current = || {
        outcome.authority.as_ref().is_some_and(|p| p.is_current())
            && reference::version(router, mapping, &prompt_version).as_ref() == Some(&revision)
    };
    let answers = outcome.current_answers();
    let accepted = answers.get("0").and_then(|a| {
        let Answer::Noul { noul } = a.get("promote")? else {
            return None;
        };
        let Answer::Score {
            score, confidence, ..
        } = a.get("importance")?
        else {
            return None;
        };
        (score.is_finite() && (0.0..=4.0).contains(score)).then_some((
            *noul >= 0.5,
            score.round() / 4.0,
            noul.max(1.0 - noul).min(*confidence),
        ))
    });
    let Some((promote, importance, confidence)) = accepted.filter(|_| current()) else {
        if let Some(observation) = &outcome.observation {
            observation.complete(
                None,
                false,
                outcome.incumbent.as_ref().is_some_and(Result::is_ok),
                started.elapsed(),
            );
        }
        return outcome.incumbent.unwrap_or_else(|| {
            Err(anyhow::anyhow!(
                "evidence classification has no current answer"
            ))
        });
    };
    let existing = outcome.incumbent.as_ref().and_then(|r| r.as_ref().ok());
    let text_attempted = promote && existing.is_none();
    let generated = if text_attempted {
        tokio::time::timeout(BUDGET.saturating_sub(started.elapsed()), invoke())
            .await
            .ok()
            .and_then(Result::ok)
    } else {
        None
    };
    if let Some(authority) = outcome.authority.as_ref() {
        authority.revalidate().await;
    }
    let text_reference = generated
        .as_ref()
        .map(|r| Reference::from_response(BTreeMap::new(), &r.response, r.elapsed_ms));
    let text = generated.or_else(|| {
        outcome
            .incumbent
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|r| Reviewed {
                proposal: r.proposal.clone(),
                response: r.response.clone(),
                elapsed_ms: r.elapsed_ms,
            })
    });
    let completed = mixed::complete(
        (promote, importance, confidence),
        if promote {
            mixed::TextNeed::Required
        } else {
            mixed::TextNeed::None
        },
        text,
        || async { None },
        |_, r| valid_body(&r.proposal),
        current,
    )
    .await;
    let valid = matches!(&completed, mixed::Completed::Ready { .. });
    if let Some(observation) = &outcome.observation {
        observation.complete(text_reference, text_attempted, valid, started.elapsed());
    }
    let mixed::Completed::Ready { text, .. } = completed else {
        anyhow::bail!("evidence decision requires current policy and a valid evidence body");
    };
    let mut result = text.unwrap_or_else(|| Reviewed {
        proposal: EvidenceProposal {
            promote: false,
            skip_reason: Some("qualified_decision_skip".into()),
            ..Default::default()
        },
        // Host-normalized output, with no fabricated LLM usage/receipt.
        response: SimplifiedLLMResponse::default(),
        elapsed_ms: 0,
    });
    result.proposal.promote = promote;
    result.proposal.importance = Some(importance);
    result.proposal.confidence = Some(confidence);
    result.proposal.decision_origin = outcome.origins.get("0").cloned();
    result.proposal.decision_guard = outcome.authority.as_ref().map(|authority| {
        EvidenceDecisionGuard(Arc::new(reference::ApplyGuard::new(
            authority.clone(),
            router,
            mapping,
            &prompt_version,
            &revision,
        )))
    });
    if result.response.telemetry.is_none() {
        result.response.content = serde_json::to_string(&result.proposal)?;
    }
    Ok(result)
}

fn valid_body(proposal: &EvidenceProposal) -> bool {
    proposal
        .summary
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty() && s.len() <= 4096)
        && proposal.observed_actions.len() <= 32
        && proposal.entity_keys.len() <= 64
        && proposal.people_keys.len() <= 64
        && proposal.entities.len() <= 64
        && proposal.facets.len() <= 32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn screen_replay_rejects_changed_deleted_or_reprompted_source() {
        use crate::magician_v2::{
            artifact_v2::workspace::ArtifactV2Workspace, prompts::json_storage::JsonPromptStorage,
        };

        let temp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_scoped_memory_scope_in_workspace(
            ArtifactV2Workspace::new(temp.path()),
            "screen-evidence-test",
            "workspace",
        );
        let tier = crate::magician_v2::chat::service::normalized_user_memory_tier_name(
            "screen_observations",
        )
        .unwrap();
        let knowledge = |rows: Vec<Value>| {
            let mut value = serde_json::Map::new();
            value.insert(tier.clone(), Value::Array(rows));
            Value::Object(value)
        };
        let row = json!({
            "key":"observe:launch",
            "purpose":"Release review",
            "mode":"watch",
            "date":"2026-09-29",
            "summary":"Mira approved the release.",
            "notes":2,
            "alerts":1
        });
        let row2 = json!({
            "key":"observe:followup",
            "purpose":"Release review",
            "mode":"watch",
            "date":"2026-09-29",
            "summary":"Mira closed the action items.",
            "notes":3,
            "alerts":0
        });
        let source = knowledge(vec![row.clone(), row2.clone()]);
        service
            .save_user_knowledge(&source)
            .await
            .unwrap();
        let cluster = cluster_screen_observations(&[
            serde_json::from_value(row.clone()).unwrap(),
            serde_json::from_value(row2.clone()).unwrap(),
        ])
        .remove(0);
        let prompts =
            PromptManager::new(Arc::new(JsonPromptStorage::with_default_config().unwrap()));
        let (system, user) = render_screen_prompt(&cluster, &prompts).await.unwrap();
        let mut snapshot = ScreenReplaySnapshot {
            mapping: "screen_evidence_distill".into(),
            system,
            user,
            cluster,
            source_digest: screen_tier_digest(&source[tier.as_str()]).unwrap(),
        };
        assert!(screen_replay_current(&service, &prompts, &snapshot).await);
        snapshot.user.push_str("changed");
        assert!(!screen_replay_current(&service, &prompts, &snapshot).await);
        snapshot
            .user
            .truncate(snapshot.user.len() - "changed".len());
        let mut changed = row;
        changed["summary"] = json!("Release was held for review.");
        service
            .save_user_knowledge(&knowledge(vec![changed.clone(), row2.clone()]))
            .await
            .unwrap();
        assert!(!screen_replay_current(&service, &prompts, &snapshot).await);
        changed["summary"] = json!("Mira approved the release.");
        changed["notes"] = json!(3);
        let mut redistributed = row2;
        redistributed["notes"] = json!(2);
        let same_cluster = cluster_screen_observations(&[
            serde_json::from_value(changed.clone()).unwrap(),
            serde_json::from_value(redistributed.clone()).unwrap(),
        ])
        .remove(0);
        assert_eq!(
            serde_json::to_value(&same_cluster).unwrap(),
            serde_json::to_value(&snapshot.cluster).unwrap()
        );
        service
            .save_user_knowledge(&knowledge(vec![changed, redistributed]))
            .await
            .unwrap();
        assert!(!screen_replay_current(&service, &prompts, &snapshot).await);
        service
            .save_user_knowledge(&knowledge(Vec::new()))
            .await
            .unwrap();
        assert!(!screen_replay_current(&service, &prompts, &snapshot).await);
    }

    #[tokio::test]
    async fn tier_replay_rejects_changed_or_deleted_source_rows() {
        use crate::magician_v2::{
            artifact_v2::workspace::ArtifactV2Workspace, prompts::json_storage::JsonPromptStorage,
        };

        let temp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_scoped_memory_scope_in_workspace(
            ArtifactV2Workspace::new(temp.path()),
            "evidence-test",
            "workspace",
        );
        let spec = producer_spec("meeting").unwrap();
        let tier =
            crate::magician_v2::chat::service::normalized_user_memory_tier_name(spec.tier).unwrap();
        let knowledge = |rows: Vec<Value>| {
            let mut value = serde_json::Map::new();
            value.insert(tier.clone(), Value::Array(rows));
            Value::Object(value)
        };
        let row = json!({
            "key":"meeting:launch",
            "source_type":spec.source_type,
            "account":"launch",
            "day":"2026-09-29",
            "summary":"Mira owns the release review."
        });
        service
            .save_user_knowledge(&knowledge(vec![row.clone()]))
            .await
            .unwrap();
        let cluster = cluster_tier_entries(&[row.clone()], &spec).remove(0);
        let prompts =
            PromptManager::new(Arc::new(JsonPromptStorage::with_default_config().unwrap()));
        let system = prompts
            .get_rendered_prompt("tier_evidence_distill_system", "1.0.0", HashMap::new())
            .await
            .unwrap();
        let user = prompts
            .get_rendered_prompt(
                "tier_evidence_distill_user",
                "1.0.0",
                HashMap::from([
                    ("domain".to_string(), spec.domain.to_string()),
                    (
                        "record_json".to_string(),
                        serde_json::to_string_pretty(&row).unwrap(),
                    ),
                ]),
            )
            .await
            .unwrap();
        let snapshot = TierReplaySnapshot {
            mapping: "tier_evidence_distill".into(),
            system,
            user,
            cluster,
            producer: spec.producer.into(),
        };
        assert!(tier_replay_current(&service, &prompts, &snapshot).await);
        let mut changed = row;
        changed["summary"] = json!("Release review ownership changed to Dev.");
        service
            .save_user_knowledge(&knowledge(vec![changed]))
            .await
            .unwrap();
        assert!(!tier_replay_current(&service, &prompts, &snapshot).await);
        service
            .save_user_knowledge(&knowledge(Vec::new()))
            .await
            .unwrap();
        assert!(!tier_replay_current(&service, &prompts, &snapshot).await);
    }

    #[test]
    fn memory_decision_evidence_observation_requires_explicit_valid_heads() {
        let labels = evidence_reference_labels(r#"{"promote":true,"importance":0.74}"#).unwrap();
        assert_eq!(labels["0"]["promote"], true);
        assert_eq!(labels["0"]["importance"], 0.75);
        for invalid in [
            r#"{"importance":0.75}"#,
            r#"{"promote":true}"#,
            r#"{"promote":"true","importance":0.75}"#,
            r#"{"promote":true,"importance":1.5}"#,
        ] {
            assert!(evidence_reference_labels(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn memory_decision_evidence_requires_body_and_bounded_fields() {
        let mut proposal = EvidenceProposal::default();
        proposal.promote = true;
        assert!(!valid_body(&proposal));
        proposal.summary = Some("Completed a concrete investigation with reusable findings".into());
        assert!(valid_body(&proposal));
        proposal.entity_keys = vec!["extra".into(); 65];
        assert!(!valid_body(&proposal));
    }
}
