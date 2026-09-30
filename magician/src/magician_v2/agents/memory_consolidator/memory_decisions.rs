//! Classification adapters retain the consolidator's source and storage owner.
use super::*;
use crate::magician_v2::{
    decision_host::{
        self,
        classification::{self, PolicyLookup},
    },
    decisions::{mixed, observation, reference, runner, telemetry::Reference},
};
use decision_engine_contract::{
    batch::DecisionItem,
    request::{Answer, DecisionState},
};
use std::time::{Duration as Budget, Instant};

const QUALITY_DECISION: &str = "memory_episode_quality";
const QUALITY_PROJECTION: &str = "episode_quality_v1_score_round_0_6";
const QUALITY_BUDGET: Budget = Budget::from_secs(45);

pub(super) struct ConflictReplaySource<'a> {
    service: &'a AgentMemoryService,
    agent_id: &'a str,
    tier: &'a MemoryTierDefinition,
    field: &'a str,
    cases: &'a [MemoryContradictionSweepCase],
}

impl<'a> ConflictReplaySource<'a> {
    pub(super) fn new(
        service: &'a AgentMemoryService,
        agent_id: &'a str,
        tier: &'a MemoryTierDefinition,
        field: &'a str,
        cases: &'a [MemoryContradictionSweepCase],
    ) -> Self {
        Self {
            service,
            agent_id,
            tier,
            field,
            cases,
        }
    }

    fn identity(&self) -> Value {
        json!((
            self.agent_id,
            &self.tier.name,
            self.field,
            self.cases
                .iter()
                .map(|case| (
                    case.existing_index,
                    case.incoming_index,
                    &case.existing_hash,
                    &case.incoming_hash
                ))
                .collect::<Vec<_>>()
        ))
    }
}

#[derive(Serialize)]
struct ConflictReplaySnapshotRef<'a> {
    target: &'a str,
    rule: &'a str,
    conflicts: &'a [MemoryConflictReviewCase],
    agent_id: &'a str,
    tier: &'a MemoryTierDefinition,
    field: &'a str,
    pair_indices: Vec<(usize, usize)>,
}

#[derive(Deserialize)]
struct ConflictReplaySnapshot {
    target: String,
    rule: String,
    conflicts: Vec<MemoryConflictReviewCase>,
    agent_id: String,
    tier: MemoryTierDefinition,
    field: String,
    pair_indices: Vec<(usize, usize)>,
}

async fn conflict_source_current(
    service: &AgentMemoryService,
    snapshot: &ConflictReplaySnapshot,
) -> bool {
    let Some((principal, workspace)) = service.scoped_memory_scope() else {
        return false;
    };
    let Ok(Some(tier)) = service
        .load_native_tier(&snapshot.agent_id, &snapshot.tier, None)
        .await
    else {
        return false;
    };
    if tier.principal.as_deref() != Some(principal)
        || tier.workspace.as_deref() != Some(workspace)
        || tier.agent_id.as_deref() != Some(snapshot.agent_id.as_str())
        || tier.tier_name != snapshot.tier.name
        || tier.tier_scope != snapshot.tier.scope
        || tier.goal_id.is_some()
    {
        return false;
    }
    let Some(items) = tier.fields.get(&snapshot.field).and_then(Value::as_array) else {
        return false;
    };
    snapshot.pair_indices.len() == snapshot.conflicts.len()
        && snapshot.pair_indices.iter().zip(&snapshot.conflicts).all(
            |(&(existing_index, incoming_index), case)| {
                let (Some(existing), Some(incoming)) =
                    (items.get(existing_index), items.get(incoming_index))
                else {
                    return false;
                };
                !memory_item_is_superseded(existing)
                    && !memory_item_is_superseded(incoming)
                    && memory_conflict_review_identity_hash(existing)
                        == memory_conflict_review_identity_hash(&case.existing_item)
                    && memory_conflict_review_identity_hash(incoming)
                        == memory_conflict_review_identity_hash(&case.incoming_item)
            },
        )
}
pub(super) struct QualityReview {
    pub signals: HashMap<String, EpisodeMemorySignal>,
    pub response: SimplifiedLLMResponse,
    pub elapsed_ms: u64,
}

/// One captured invocation policy covers lookup, inference and both caches.
/// Discovering a gate while the cache read awaits never changes this invocation.
pub(super) struct QualityPolicy {
    pub key: String,
    lookup: PolicyLookup,
    reference: Option<String>,
    generation: u64,
}
impl QualityPolicy {
    pub fn current(&self, router: &OperationLlmRouter) -> bool {
        if self.generation != decision_host::generation()
            || self.reference
                != reference::version(
                    router,
                    LLMOperation::MemoryEpisodeQualityClassification.as_str(),
                    EPISODE_QUALITY_REVIEW_CONTRACT,
                )
        {
            return false;
        }
        match &self.lookup {
            PolicyLookup::Participating(p) if p.policy.gate => p.is_current(),
            _ => true,
        }
    }
}
pub(super) async fn quality_cache_identity(
    owner: &MemoryConsolidator,
    router: &OperationLlmRouter,
) -> QualityPolicy {
    let reference = reference::version(
        router,
        LLMOperation::MemoryEpisodeQualityClassification.as_str(),
        EPISODE_QUALITY_REVIEW_CONTRACT,
    );
    let lookup = match owner.memory_service.scoped_memory_scope() {
        Some((p, w)) => classification::ready_policy(QUALITY_DECISION, &p, &w).await,
        None => classification::unscoped_policy(),
    };
    let generation = decision_host::generation();
    let origin = match &lookup {
        PolicyLookup::Participating(p) if p.policy.gate => {
            format!("engine:{}:{}", p.engine_instance, p.revision)
        },
        _ => "incumbent".into(),
    };
    let key = blake3::hash(
        format!("{generation}:{origin}:{reference:?}:{QUALITY_PROJECTION}").as_bytes(),
    )
    .to_hex()
    .to_string();
    QualityPolicy {
        key,
        lookup,
        reference,
        generation,
    }
}

pub(super) async fn quality(
    owner: &MemoryConsolidator,
    router: &Arc<OperationLlmRouter>,
    episodes: &[V3EpisodeRecord],
    policy: &QualityPolicy,
) -> Result<HashMap<String, EpisodeMemorySignal>> {
    let Some((principal, workspace)) = owner.memory_service.scoped_memory_scope() else {
        anyhow::ensure!(
            policy.lookup.allows_incumbent(),
            "memory quality deferred: missing scope"
        );
        return owner
            .classify_episode_memory_quality_incumbent(router, &episodes.iter().collect::<Vec<_>>())
            .await
            .map(|r| r.signals);
    };
    let Some(revision) = policy.reference.clone() else {
        anyhow::ensure!(
            policy.lookup.allows_incumbent(),
            "memory quality deferred: missing reference"
        );
        return owner
            .classify_episode_memory_quality_incumbent(router, &episodes.iter().collect::<Vec<_>>())
            .await
            .map(|r| r.signals);
    };
    let lookup = policy.lookup.clone();
    if !matches!(lookup, PolicyLookup::Participating(_))
        || episodes.is_empty()
        || episodes.len() > 64
    {
        anyhow::ensure!(
            lookup.allows_incumbent(),
            "memory quality deferred: engine unavailable or invalid projection"
        );
        return owner
            .classify_episode_memory_quality_incumbent(router, &episodes.iter().collect::<Vec<_>>())
            .await
            .map(|r| r.signals);
    }
    let started = Instant::now();
    let items = episodes
        .iter()
        .enumerate()
        .map(|(index, episode)| {
            let mut projection = episode_memory_quality_classifier_value(episode);
            if let Some(object) = projection.as_object_mut() {
                for key in ["episode_id", "goal_key", "completed_at"] {
                    object.remove(key);
                }
            }
            DecisionItem {
                item_id: index.to_string(),
                state: DecisionState::from_json(projection),
                choice_candidates: Default::default(),
            }
        })
        .collect::<Vec<_>>();
    let mut input = runner::Input {
        operation: QUALITY_DECISION.into(),
        projection_version: QUALITY_PROJECTION.into(),
        reference_version: revision.clone(),
        case_id: episode_quality_cache_key(episodes),
        context: None,
        items,
        required_questions: vec!["classification".into(), "priority".into(), "score".into()],
        scope: router.classification_trace_context(magicllm::LlmScope::new(principal, workspace)),
        agent: None,
        requires_completion: true,
        replay: None,
    };
    if let PolicyLookup::Participating(participation) = &lookup {
        if observation::selected(&input, participation) {
            if let Some((snapshot, bytes)) =
                observation::snapshot_bounded::<_, Vec<V3EpisodeRecord>>(&episodes)
            {
                let snapshot = Arc::new(snapshot);
                let check_snapshot = snapshot.clone();
                let check_service = owner.memory_service.clone();
                let check_router = router.clone();
                let check_revision = revision.clone();
                let check_principal = principal.to_owned();
                let check_workspace = workspace.to_owned();
                let run_snapshot = snapshot.clone();
                let run_owner = owner.clone();
                let run_router = Arc::new(
                    router.with_dispatch_priority(magicllm::dispatch::Priority::Background),
                );
                input.replay = Some(Ok(runner::ReferenceReplay {
                    bytes,
                    cost_reservation_microusd: reference::observation_cost_upper_microusd(
                        router,
                        LLMOperation::MemoryEpisodeQualityClassification.as_str(),
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
                                    LLMOperation::MemoryEpisodeQualityClassification.as_str(),
                                    EPISODE_QUALITY_REVIEW_CONTRACT,
                                )
                                .as_ref()
                                    != Some(&revision)
                            {
                                return false;
                            }
                            for episode in snapshot.iter() {
                                let Ok(Some(current)) = service
                                    .load_native_episode_by_id(
                                        &episode.agent_id,
                                        &episode.episode_id,
                                    )
                                    .await
                                else {
                                    return false;
                                };
                                let Some((current, _)) =
                                    observation::snapshot_bounded::<_, V3EpisodeRecord>(&current)
                                else {
                                    return false;
                                };
                                if serde_json::to_vec(&current).ok()
                                    != serde_json::to_vec(episode).ok()
                                {
                                    return false;
                                }
                            }
                            true
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
                            let result = run_owner
                                .classify_episode_memory_quality_incumbent(
                                    &run_router,
                                    &run_snapshot.iter().collect::<Vec<_>>(),
                                )
                                .await;
                            let Ok(review) = result else {
                                return runner::ReplayResult {
                                    status: "failed",
                                    attempted: None,
                                    reference: None,
                                };
                            };
                            let labels = run_snapshot
                                .iter()
                                .enumerate()
                                .filter_map(|(i, episode)| {
                                    review.signals.get(&episode.episode_id).map(|signal| {
                                        (
                                            i.to_string(),
                                            BTreeMap::from([
                                                (
                                                    "classification".into(),
                                                    json!(signal.classification),
                                                ),
                                                (
                                                    "priority".into(),
                                                    json!(signal.extraction_priority),
                                                ),
                                                ("score".into(), json!(signal.score)),
                                            ]),
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
                                    review.elapsed_ms,
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
    let outcome = runner::run(
        input,
        lookup,
        QUALITY_BUDGET.saturating_sub(started.elapsed()),
        runner::text_reserve(QUALITY_BUDGET),
        |ids, _| async move {
            if ids.len() == episodes.len() {
                Some(
                    owner
                        .classify_episode_memory_quality_incumbent(
                            router,
                            &episodes.iter().collect::<Vec<_>>(),
                        )
                        .await,
                )
            } else {
                let selected = ids
                    .iter()
                    .filter_map(|id| id.parse::<usize>().ok())
                    .filter_map(|i| episodes.get(i))
                    .collect::<Vec<_>>();
                Some(
                    owner
                        .classify_episode_memory_quality_incumbent(router, &selected)
                        .await,
                )
            }
        },
        |result| match result {
            Ok(review) => {
                let labels = episodes
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| {
                        review.signals.get(&e.episode_id).map(|s| {
                            (
                                i.to_string(),
                                BTreeMap::from([
                                    ("classification".into(), json!(s.classification)),
                                    ("priority".into(), json!(s.extraction_priority)),
                                    ("score".into(), json!(s.score)),
                                ]),
                            )
                        })
                    })
                    .collect();
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
                LLMOperation::MemoryEpisodeQualityClassification.as_str(),
                EPISODE_QUALITY_REVIEW_CONTRACT,
            )
            .as_ref()
                == Some(&revision)
    };
    let mut accepted = BTreeMap::new();
    for (id, answers) in outcome.current_answers() {
        let Some(index) = id.parse::<usize>().ok().filter(|i| *i < episodes.len()) else {
            continue;
        };
        if let Some(mut signal) = signal(&answers, &episodes[index]) {
            if let Some(origin) = outcome.origins.get(&id) {
                signal.reviewer = format!(
                    "decision_model:{}:{}@{}:{}",
                    origin.model.model, origin.pack, origin.pack_version, origin.batch_id
                );
            }
            accepted.insert(index, signal);
        }
    }
    // Reasons feed the subsequent extraction prompt, so positive decisions retain
    // their evidence-based prose stage. Negative decisions need no new prose.
    let text_episodes = accepted
        .iter()
        .filter(|(_, s)| s.classification != "progress_only_or_low_signal")
        .map(|(i, _)| &episodes[*i])
        .collect::<Vec<_>>();
    let text_attempted = !text_episodes.is_empty() && current();
    let text = if text_attempted {
        tokio::time::timeout(
            QUALITY_BUDGET.saturating_sub(started.elapsed()),
            owner.classify_episode_memory_quality_incumbent(router, &text_episodes),
        )
        .await
        .ok()
        .and_then(Result::ok)
    } else {
        None
    };
    if let Some(authority) = outcome.authority.as_ref() {
        authority.revalidate().await;
    }
    let mut completed = HashMap::new();
    for (index, mut signal) in accepted {
        let need = if signal.classification == "progress_only_or_low_signal" {
            mixed::TextNeed::None
        } else {
            mixed::TextNeed::Required
        };
        let reasons = text
            .as_ref()
            .and_then(|r| r.signals.get(&episodes[index].episode_id))
            .map(|s| s.reasons.clone());
        match mixed::complete(
            (),
            need,
            reasons,
            || async { None },
            |_, reasons| {
                !reasons.is_empty()
                    && reasons
                        .iter()
                        .all(|r| !r.trim().is_empty() && r.len() <= 2400)
            },
            current,
        )
        .await
        {
            mixed::Completed::Ready { text, .. } => {
                if let Some(reasons) = text {
                    signal.reasons = reasons;
                }
                completed.insert(episodes[index].episode_id.clone(), signal);
            },
            mixed::Completed::MissingRequiredText => {
                // The existing deterministic signal is the operation's safe fallback.
                completed.insert(
                    episodes[index].episode_id.clone(),
                    episode_memory_signal(&episodes[index]),
                );
            },
            mixed::Completed::StalePolicy => {},
        }
    }
    if let Some(observation) = &outcome.observation {
        observation.complete(
            text.as_ref()
                .map(|r| Reference::from_response(BTreeMap::new(), &r.response, r.elapsed_ms)),
            text_attempted,
            current(),
            started.elapsed(),
        );
    }
    let mut signals = match outcome.incumbent {
        Some(Ok(review)) => review.signals,
        Some(Err(error)) if completed.is_empty() => return Err(error),
        _ => HashMap::new(),
    };
    signals.extend(completed);
    if signals.is_empty() {
        anyhow::bail!("episode quality decision had no current result");
    }
    Ok(signals)
}

fn signal(
    answers: &BTreeMap<String, Answer>,
    episode: &V3EpisodeRecord,
) -> Option<EpisodeMemorySignal> {
    let Answer::Choice {
        choice: classification,
        confidence: c1,
        ..
    } = answers.get("classification")?
    else {
        return None;
    };
    let Answer::Choice {
        choice: priority,
        confidence: c2,
        ..
    } = answers.get("priority")?
    else {
        return None;
    };
    let Answer::Score {
        score,
        confidence: c3,
        ..
    } = answers.get("score")?
    else {
        return None;
    };
    if !score.is_finite() || !(0.0..=6.0).contains(score) {
        return None;
    }
    let classification = normalize_memory_quality_classification(classification.as_str())?;
    let priority = normalize_memory_quality_priority(priority.as_str())?;
    let mut signal = episode_memory_signal(episode);
    signal.classification = classification;
    signal.extraction_priority = priority;
    signal.score = score.round() as i32;
    signal.confidence = Some(c1.min(*c2).min(*c3));
    signal.reviewer = "decision_model".into();
    Some(signal)
}

pub(super) struct ConflictReview {
    pub decisions: HashMap<String, MemoryConflictDecision>,
    pub response: Option<SimplifiedLLMResponse>,
    pub elapsed_ms: u64,
}
impl ConflictReview {
    pub fn empty() -> Self {
        Self {
            decisions: HashMap::new(),
            response: None,
            elapsed_ms: 0,
        }
    }
}
#[derive(Debug, Clone)]
pub(super) struct ConflictDecision {
    pub decision: MemoryConflictDecision,
    pub guard: Option<Arc<reference::ApplyGuard>>,
    pub origin: Option<decision_engine_contract::classification::ClassificationOrigin>,
}
impl ConflictDecision {
    pub fn incumbent(decision: MemoryConflictDecision) -> Self {
        Self {
            decision,
            guard: None,
            origin: None,
        }
    }
    pub fn current(&self) -> bool {
        self.guard.as_ref().is_none_or(|guard| guard.current())
    }
}
const CONFLICT_OPERATION: &str = "memory_conflict_review";
const CONFLICT_PROJECTION: &str = "memory_conflict_pair_v1";

pub(super) struct ConflictPolicy {
    pub key: String,
    lookup: PolicyLookup,
    reference: Option<String>,
}
pub(super) async fn conflict_policy(
    owner: &MemoryConsolidator,
    router: &OperationLlmRouter,
    target: &str,
) -> ConflictPolicy {
    let mapping = if memory_conflict_target_is_high_risk(target) {
        LLMOperation::MemoryConflictReviewHighRisk
    } else {
        LLMOperation::MemoryConflictReview
    };
    let reference = reference::version(router, mapping.as_str(), CONFLICT_PROJECTION);
    let lookup = match owner.memory_service.scoped_memory_scope() {
        Some((p, w)) => classification::ready_policy(CONFLICT_OPERATION, &p, &w).await,
        None => classification::unscoped_policy(),
    };
    let origin = match &lookup {
        PolicyLookup::Participating(p) if p.policy.gate => {
            format!("engine:{}:{}", p.engine_instance, p.revision)
        },
        _ => "incumbent".into(),
    };
    let key = blake3::hash(
        format!(
            "{}:{origin}:{reference:?}:{CONFLICT_PROJECTION}",
            decision_host::generation()
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string();
    ConflictPolicy {
        key,
        lookup,
        reference,
    }
}

pub(super) async fn conflict(
    owner: &MemoryConsolidator,
    router: &OperationLlmRouter,
    rule: &MemoryConsolidationRule,
    target: &str,
    conflicts: &[MemoryConflictReviewCase],
    policy: &ConflictPolicy,
    source: Option<&ConflictReplaySource<'_>>,
) -> HashMap<String, ConflictDecision> {
    if conflicts.is_empty() {
        return HashMap::new();
    }
    let mapping = if memory_conflict_target_is_high_risk(target) {
        LLMOperation::MemoryConflictReviewHighRisk
    } else {
        LLMOperation::MemoryConflictReview
    };
    let fallback = || async {
        if !policy.lookup.allows_incumbent() {
            return HashMap::new();
        }
        owner
            .review_memory_conflict_incumbent(
                router,
                rule,
                target,
                &conflicts.iter().collect::<Vec<_>>(),
            )
            .await
            .decisions
            .into_iter()
            .map(|(id, decision)| (id, ConflictDecision::incumbent(decision)))
            .collect()
    };
    let Some((principal, workspace)) = owner.memory_service.scoped_memory_scope() else {
        return fallback().await;
    };
    let Some(revision) = policy.reference.clone() else {
        return fallback().await;
    };
    let lookup = policy.lookup.clone();
    if !matches!(lookup, PolicyLookup::Participating(_)) || conflicts.len() > 64 {
        return fallback().await;
    }
    let started = Instant::now();
    let budget = Budget::from_secs(mapping.timeout_seconds());
    let mut items = Vec::new();
    for (index, case) in conflicts.iter().enumerate() {
        // Require a complete bounded pair. A truncated old/incoming fact cannot
        // authorize supersession, even when a provider is confident about it.
        let raw = memory_conflict_review_payload_excerpt(
            target,
            &rule.name,
            std::slice::from_ref(case),
            12000,
        );
        let Ok(mut value) = serde_json::from_str::<Value>(&raw) else {
            return fallback().await;
        };
        if let Some(pair) = value
            .get_mut("conflicts")
            .and_then(Value::as_array_mut)
            .and_then(|a| a.first_mut())
            .and_then(Value::as_object_mut)
        {
            pair.remove("conflict_id");
        }
        items.push(DecisionItem {
            item_id: index.to_string(),
            state: DecisionState::from_json(value),
            choice_candidates: Default::default(),
        });
    }
    let case_id = blake3::hash(
        serde_json::to_vec(&(&items, source.map(ConflictReplaySource::identity)))
            .unwrap_or_default()
            .as_slice(),
    )
    .to_hex()
    .to_string();
    let mut input = runner::Input {
        operation: CONFLICT_OPERATION.into(),
        projection_version: CONFLICT_PROJECTION.into(),
        reference_version: revision.clone(),
        case_id,
        context: None,
        items,
        required_questions: vec!["resolution".into()],
        scope: router.classification_trace_context(magicllm::LlmScope::new(principal, workspace)),
        agent: None,
        requires_completion: false,
        replay: None,
    };
    if let PolicyLookup::Participating(participation) = &lookup {
        if observation::selected(&input, participation) {
            if let Some(source) = source {
                let snapshot_source = ConflictReplaySnapshotRef {
                    target,
                    rule: &rule.name,
                    conflicts,
                    agent_id: source.agent_id,
                    tier: source.tier,
                    field: source.field,
                    pair_indices: source
                        .cases
                        .iter()
                        .map(|case| (case.existing_index, case.incoming_index))
                        .collect(),
                };
                match observation::snapshot_bounded::<_, ConflictReplaySnapshot>(&snapshot_source) {
                    Some((snapshot, bytes)) => {
                        let snapshot = Arc::new(snapshot);
                        let check_snapshot = snapshot.clone();
                        let check_service = source.service.clone();
                        let run_router = router.clone();
                        let check_router = router.clone();
                        let check_revision = revision.clone();
                        let check_mapping = mapping.as_str().to_owned();
                        let run_mapping = check_mapping.clone();
                        let check_principal = principal.to_owned();
                        let check_workspace = workspace.to_owned();
                        let run_principal = check_principal.clone();
                        let run_workspace = check_workspace.clone();
                        input.replay = Some(Ok(runner::ReferenceReplay {
                            bytes,
                            cost_reservation_microusd: reference::observation_cost_upper_microusd(
                                router,
                                mapping.as_str(),
                                bytes,
                            ),
                            current: Box::new(move || {
                                let snapshot = check_snapshot.clone();
                                let service = check_service.clone();
                                let router = check_router.clone();
                                let revision = check_revision.clone();
                                let mapping = check_mapping.clone();
                                let principal = check_principal.clone();
                                let workspace = check_workspace.clone();
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
                                            &mapping,
                                            CONFLICT_PROJECTION,
                                        )
                                        .as_ref()
                                            == Some(&revision)
                                        && conflict_source_current(&service, &snapshot).await
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
                                    let prompt = memory_conflict_reference_prompt(
                                        &snapshot.target,
                                        &snapshot.rule,
                                        &snapshot.conflicts.iter().collect::<Vec<_>>(),
                                    );
                                    let started = Instant::now();
                                    let Ok(response) = reference::pinned_json_observation(
                                        &run_router,
                                        &run_mapping,
                                        &run_principal,
                                        &run_workspace,
                                        MEMORY_CONFLICT_SYSTEM_PROMPT,
                                        &prompt,
                                    )
                                    .await
                                    else {
                                        return runner::ReplayResult {
                                            status: "failed",
                                            attempted: None,
                                            reference: None,
                                        };
                                    };
                                    let labels = conflict_reference_labels(
                                        &response.content,
                                        &snapshot.conflicts,
                                    );
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
            } else {
                // Incoming transform items are not yet persisted. They need a
                // separate immutable source binding before comparison is safe.
                input.replay = Some(Err("source_unbound"));
            }
        }
    }
    let outcome = runner::run(
        input,
        lookup,
        budget.saturating_sub(started.elapsed()),
        budget - Budget::from_secs(1),
        |ids, _| async move {
            let selected = ids
                .iter()
                .filter_map(|id| id.parse::<usize>().ok())
                .filter_map(|i| conflicts.get(i))
                .collect::<Vec<_>>();
            Some(
                owner
                    .review_memory_conflict_incumbent(router, rule, target, &selected)
                    .await,
            )
        },
        |review| {
            let labels: crate::magician_v2::decisions::telemetry::Labels = conflicts
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    review.decisions.get(&c.conflict_id).map(|d| {
                        (
                            i.to_string(),
                            BTreeMap::from([("resolution".into(), json!(conflict_label(*d)))]),
                        )
                    })
                })
                .collect();
            match &review.response {
                Some(response) => Reference::from_response(labels, response, review.elapsed_ms),
                None => labels.into(),
            }
        },
    )
    .await;
    let guard = outcome.authority.as_ref().map(|authority| {
        Arc::new(reference::ApplyGuard::new(
            authority.clone(),
            router,
            mapping.as_str(),
            CONFLICT_PROJECTION,
            &revision,
        ))
    });
    let mut decisions = HashMap::new();
    if reference::version(router, mapping.as_str(), CONFLICT_PROJECTION).as_ref() == Some(&revision)
    {
        for (id, answers) in outcome.current_answers() {
            let Some(case) = id.parse::<usize>().ok().and_then(|i| conflicts.get(i)) else {
                continue;
            };
            if let Some(Answer::Choice { choice, .. }) = answers.get("resolution") {
                if let Some(decision) = MemoryConflictDecision::from_str(choice.as_str()) {
                    if conflict_label(decision) == choice.as_str() {
                        decisions.insert(
                            case.conflict_id.clone(),
                            ConflictDecision {
                                decision,
                                guard: guard.clone(),
                                origin: outcome.origins.get(&id).cloned(),
                            },
                        );
                    }
                }
            }
        }
    }
    if let Some(incumbent) = outcome.incumbent {
        decisions.extend(
            incumbent
                .decisions
                .into_iter()
                .map(|(id, decision)| (id, ConflictDecision::incumbent(decision))),
        );
    }
    decisions
}

fn conflict_reference_labels(
    response: &str,
    conflicts: &[MemoryConflictReviewCase],
) -> Option<crate::magician_v2::decisions::telemetry::Labels> {
    let value = parse_json_from_llm_response(response).ok()?;
    let rows = value.get("conflicts")?.as_array()?;
    if rows.len() != conflicts.len() {
        return None;
    }
    let expected = conflicts
        .iter()
        .map(|case| case.conflict_id.as_str())
        .collect::<HashSet<_>>();
    let mut labels = BTreeMap::new();
    for row in rows {
        let id = row.get("conflict_id")?.as_str()?;
        if !expected.contains(id) || labels.contains_key(id) {
            return None;
        }
        let decision = MemoryConflictDecision::from_value(row)?;
        let item_id = conflicts.iter().position(|case| case.conflict_id == id)?;
        labels.insert(
            item_id.to_string(),
            BTreeMap::from([("resolution".into(), json!(conflict_label(decision)))]),
        );
    }
    (labels.len() == conflicts.len()).then_some(labels)
}

#[cfg(test)]
mod memory_conflict_replay_tests {
    use super::*;

    fn pair(id: &str) -> MemoryConflictReviewCase {
        MemoryConflictReviewCase {
            conflict_id: id.into(),
            existing_item: json!({"key":"code","value":"draft"}),
            incoming_item: json!({"key":"code","value":"signed"}),
            similarity: 0.2,
            match_reason: MemoryConflictMatchReason::SameDurableKey,
        }
    }

    #[test]
    fn conflict_observation_requires_exactly_one_valid_label_per_offered_pair() {
        let cases = [pair("first"), pair("second")];
        let valid = r#"{"conflicts":[{"conflict_id":"second","decision":"keep_both"},{"conflict_id":"first","decision":"replace_existing"}]}"#;
        let labels = conflict_reference_labels(valid, &cases).unwrap();
        assert_eq!(labels["0"]["resolution"], "replace_existing");
        assert_eq!(labels["1"]["resolution"], "keep_both");
        for invalid in [
            r#"{"conflicts":[{"conflict_id":"first","decision":"replace_existing"}]}"#,
            r#"{"conflicts":[{"conflict_id":"first","decision":"replace_existing"},{"conflict_id":"first","decision":"keep_both"}]}"#,
            r#"{"conflicts":[{"conflict_id":"first","decision":"replace_existing"},{"conflict_id":"foreign","decision":"keep_both"}]}"#,
            r#"{"conflicts":[{"conflict_id":"first","decision":"replace_existing"},{"conflict_id":"second","decision":"invalid"}]}"#,
        ] {
            assert!(
                conflict_reference_labels(invalid, &cases).is_none(),
                "{invalid}"
            );
        }
    }
}
fn conflict_label(decision: MemoryConflictDecision) -> &'static str {
    match decision {
        MemoryConflictDecision::ReplaceExisting => "replace_existing",
        MemoryConflictDecision::KeepExisting => "keep_existing",
        MemoryConflictDecision::KeepBoth => "keep_both",
    }
}
