//! Closed thread judgments on the shared rail. The caller owns LLM fallback.
use super::{
    classify::{self, Classification, ClassifyLlm},
    store::{RecentHandledFollowUp, ThreadClassifyRow},
    types::{ChannelTemporalKind, ClassifiedBy},
};
use anyhow::Result;
use async_trait::async_trait;
use decision_engine_contract::{
    batch::DecisionItem,
    request::{Answer, DecisionState},
};
use futures_util::{stream, StreamExt};
use magician::magician_v2::{
    decisions::{self, runner, telemetry::Reference, Participation},
    query_analysis::operation_llm_router::OperationLlmRouter,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

pub const PROJECTION_VERSION: &str = "channel_classify_projection/1";
pub const REQUIRED_QUESTIONS: [&str; 5] = [
    "lane",
    "follow_up_kind",
    "action_owner",
    "urgency",
    "repeat_of_recently_handled",
];
const PROJECTION_FIELDS: [&str; 17] = [
    "channel",
    "lane",
    "subject",
    "sender",
    "recipient_domains",
    "label_ids",
    "message_count",
    "age",
    "latest_message_id",
    "latest_message_age",
    "latest_direction",
    "latest_intent",
    "needs_reply_hint",
    "follow_up_hint",
    "recent_handled_followups",
    "summary",
    "safe_brief",
];
fn validate_case_projection(case: &serde_json::Value) -> Result<()> {
    anyhow::ensure!(
        case["projection_version"] == PROJECTION_VERSION,
        "incompatible classifier projection"
    );
    let state = case["input"]["state"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("missing classifier state"))?;
    anyhow::ensure!(
        state.len() == PROJECTION_FIELDS.len()
            && PROJECTION_FIELDS
                .iter()
                .all(|key| state.get(*key).is_some_and(serde_json::Value::is_string)),
        "classifier state must contain exactly the body-blind projection fields"
    );
    anyhow::ensure!(
        state["summary"].as_str().unwrap().chars().count() <= 900,
        "classifier summary exceeds projection cap"
    );
    Ok(())
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ClassifyInput {
    pub row: ThreadClassifyRow,
    pub recent_handled: Vec<RecentHandledFollowUp>,
    pub now: i64,
}
async fn sources_current(
    store: &super::store::MailAssistStore,
    scope: &magicllm::LlmScope,
    inputs: &[ClassifyInput],
) -> bool {
    for input in inputs {
        let row = &input.row;
        let Ok(threads) = store
            .get_threads_by_ids(
                &scope.principal,
                &scope.workspace,
                &row.provider,
                &row.account_alias,
                std::slice::from_ref(&row.thread_id),
            )
            .await
        else {
            return false;
        };
        if threads
            .first()
            .is_none_or(|thread| thread.sensitive_suppressed)
        {
            return false;
        }
        let Ok(Some(current)) = store
            .distilled_message_for_thread(
                &scope.principal,
                &scope.workspace,
                &row.provider,
                &row.account_alias,
                &row.thread_id,
            )
            .await
        else {
            return false;
        };
        if current.sensitive_suppressed
            || current.message_id != row.latest_message_id
            || current.distill_revision != Some(row.distill_revision)
        {
            return false;
        }
        let Ok(handled) = store
            .recent_handled_follow_ups(
                &scope.principal,
                &scope.workspace,
                &row.provider,
                &row.account_alias,
                &row.thread_id,
                input
                    .now
                    .saturating_sub(classify::RECENT_HANDLED_FOLLOW_UP_SUPPRESS_MS),
                3,
            )
            .await
        else {
            return false;
        };
        if handled != input.recent_handled {
            return false;
        }
    }
    true
}
pub fn item_id(row: &ThreadClassifyRow) -> String {
    blake3::hash(&serde_json::to_vec(&(&row.provider, &row.account_alias, &row.thread_id)).unwrap())
        .to_hex()
        .to_string()
}
pub fn decision_item(input: &ClassifyInput) -> DecisionItem {
    let mut vars = classify::user_vars(&input.row, input.now, &input.recent_handled);
    vars.insert(
        "summary".into(),
        vars["summary"].chars().take(900).collect(),
    );
    DecisionItem {
        item_id: item_id(&input.row),
        state: DecisionState::from_json(serde_json::json!(vars)),
        choice_candidates: BTreeMap::new(),
    }
}
pub fn decision_context(owner: &str, now: i64) -> DecisionState {
    DecisionState::from_json(
        serde_json::json!({"owner": owner, "today": chrono::DateTime::from_timestamp_millis(now).map(|d| d.date_naive().to_string())}),
    )
}

/// Read-only baseline evaluation using the real configured classifier and
/// managed prompts. The returned receipt contains no message content.
pub async fn baseline_case(
    llm: &dyn ClassifyLlm,
    case: &serde_json::Value,
) -> Result<serde_json::Value> {
    validate_case_projection(case)?;
    fn canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted: BTreeMap<_, _> = map.into_iter().collect();
                serde_json::Value::Object(
                    sorted.into_iter().map(|(k, v)| (k, canonical(v))).collect(),
                )
            },
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonical).collect())
            },
            other => other,
        }
    }
    use sha2::{Digest, Sha256};
    let input_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical(case["input"].clone()))?)
    );
    anyhow::ensure!(
        case["projection_version"] == PROJECTION_VERSION,
        "incompatible baseline projection"
    );
    let id = case["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing case identity"))?;
    let vars = case["input"]["state"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("missing classifier state"))?
        .iter()
        .map(|(key, value)| {
            Ok((
                key.clone(),
                value
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("classifier field must be a string"))?
                    .to_owned(),
            ))
        })
        .collect::<Result<std::collections::HashMap<_, _>>>()?;
    let system = classify::classify_system_prompt().await?;
    let user = magician::magician_v2::prompts::rendered_prompt(
        magician::magician_v2::prompts::names::CHANNEL_CLASSIFY_USER,
        magician::magician_v2::prompts::versions::CHANNEL_CLASSIFY,
        vars,
    )
    .await?;
    let started = std::time::Instant::now();
    let (raw, call) = llm.complete_observed(&system, &user).await?;
    let c = classify::parse_classification(&raw)?;
    let reference = labels(&BTreeMap::from([(
        id.to_owned(),
        Ok((
            c,
            Reference {
                labels: Default::default(),
                call,
                calls: vec![],
                latency_ms: started.elapsed().as_millis() as u64,
            },
        )),
    )]));
    Ok(
        serde_json::json!({"id":id,"labels":reference.labels.get(id),"reference":reference,"projection_version":PROJECTION_VERSION,"input_sha256":input_sha256}),
    )
}

/// Read-only paired evaluation through the same scoped runner as the worker.
/// Inputs are freshly exported real projections; this does not write annotations.
/// The caller captures the runner's transport events into a private ledger.
pub async fn shadow_case(
    router: &OperationLlmRouter,
    scope: magicllm::LlmScope,
    llm: &dyn ClassifyLlm,
    case: &serde_json::Value,
) -> Result<serde_json::Value> {
    validate_case_projection(case)?;
    anyhow::ensure!(
        case["projection_version"] == PROJECTION_VERSION,
        "incompatible shadow projection"
    );
    anyhow::ensure!(
        case["source"]["principal"] == scope.principal
            && case["source"]["workspace"] == scope.workspace,
        "shadow source scope mismatch"
    );
    let captured = case["captured_at_ms"]
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("missing source capture time"))?;
    let age = chrono::Utc::now()
        .timestamp_millis()
        .saturating_sub(captured);
    anyhow::ensure!(
        (0..=3_600_000).contains(&age),
        "shadow source must have been exported within one hour"
    );
    anyhow::ensure!(
        case["source"]["message_id"].as_str().is_some()
            && case["source"]["message_id"] == case["input"]["state"]["latest_message_id"],
        "shadow source message mismatch"
    );
    anyhow::ensure!(
        case["input"]["state"].as_object().is_some_and(
            |state| !state.is_empty() && state.values().all(serde_json::Value::is_string)
        ),
        "invalid classifier fields"
    );
    anyhow::ensure!(
        case["input"]["context"]["owner"] == scope.principal,
        "shadow context owner differs from source scope"
    );
    let id = case["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing shadow identity"))?;
    let lookup = decisions::policy_lookup("channel_classify", &scope).await;
    anyhow::ensure!(
        matches!(&lookup, decisions::PolicyLookup::Participating(p) if p.policy.shadow && !p.policy.gate),
        "read-only comparisons require an active shadow-only policy"
    );
    let reference_version = decisions::reference::version(
        router,
        "channel_classify",
        magician::magician_v2::prompts::versions::CHANNEL_CLASSIFY,
    )
    .ok_or_else(|| anyhow::anyhow!("classifier reference is unbound"))?;
    let input = runner::Input {
        operation: "channel_classify".into(),
        projection_version: PROJECTION_VERSION.into(),
        reference_version,
        case_id: id.into(),
        context: Some(DecisionState::from_json(case["input"]["context"].clone())),
        items: vec![DecisionItem {
            item_id: id.into(),
            state: DecisionState::from_json(case["input"]["state"].clone()),
            choice_candidates: BTreeMap::new(),
        }],
        required_questions: REQUIRED_QUESTIONS.map(str::to_owned).to_vec(),
        scope: decisions::trace_context(router, scope),
        agent: None,
        requires_completion: false,
        replay: None,
    };
    let outcome = runner::run(
        input,
        lookup,
        Duration::from_secs(60),
        Duration::ZERO,
        |_, _| async move { Some(baseline_case(llm, case).await) },
        |result| {
            result
                .as_ref()
                .ok()
                .and_then(|row| serde_json::from_value(row["reference"].clone()).ok())
                .unwrap_or_else(|| Reference::from(BTreeMap::new()))
        },
    )
    .await;
    // Shadow deliberately exports its comparison asynchronously. The collector
    // waits on its subscribed transport; production callers remain nonblocking.
    outcome
        .incumbent
        .ok_or_else(|| anyhow::anyhow!("incumbent comparison did not complete"))?
}

/// All private outputs are constrained to the runtime eval directory, including
/// when an existing parent is a symlink. Resolve before creating any directory.
pub fn private_output(
    path: &std::path::Path,
    runtime_root: &std::path::Path,
) -> Result<std::path::PathBuf> {
    use std::path::Component;
    let root = runtime_root.canonicalize()?;
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .canonicalize()?;
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    anyhow::ensure!(
        !path.components().any(|c| matches!(c, Component::ParentDir)),
        "private output cannot traverse parents"
    );
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("private output needs a parent"))?;
    let mut existing = parent;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| anyhow::anyhow!("missing private parent"))?;
    }
    let resolved = existing
        .canonicalize()?
        .join(parent.strip_prefix(existing)?)
        .join(path.file_name().unwrap());
    anyhow::ensure!(
        resolved.starts_with(root.join("evals/channel_classify")) && !resolved.starts_with(repo),
        "private output must be outside git under runtime/evals/channel_classify"
    );
    Ok(resolved)
}
fn choice<'a>(answers: &'a BTreeMap<String, Answer>, id: &str) -> Option<&'a str> {
    answers.get(id)?.choice_option().map(|o| o.as_str())
}
pub fn classification_from_answers(
    input: &ClassifyInput,
    answers: &BTreeMap<String, Answer>,
    model: &str,
    pack_version: &str,
) -> Option<Classification> {
    let row = &input.row;
    let lane = choice(answers, "lane")?;
    let follow_up = choice(answers, "follow_up_kind")?;
    let actor = choice(answers, "action_owner")?;
    let urgency = choice(answers, "urgency")?;
    // The engine returns only qualified heads. Validate the closed vocabulary
    // again before composing a persisted product annotation.
    if !classify::LABELS.contains(&lane)
        || ![
            "needs_reply",
            "owner_owes",
            "other_owes",
            "waiting_on",
            "check_back",
            "schedule",
            "none",
        ]
        .contains(&follow_up)
        || !["owner", "counterparty", "agent", "unknown"].contains(&actor)
        || !["low", "normal", "high"].contains(&urgency)
    {
        return None;
    }
    let repeat = answers.get("repeat_of_recently_handled")?.noul_value()?;
    let due = row
        .follow_up_hint
        .as_ref()
        .and_then(|h| h.due_text.clone())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            row.distill_brief
                .as_ref()?
                .temporal_facts
                .iter()
                .find(|f| f.kind == ChannelTemporalKind::Due)
                .map(|f| f.text.clone())
        });
    let details = row
        .follow_up_hint
        .as_ref()
        .map(|h| h.key_details.clone())
        .filter(|d| !d.is_empty())
        .or_else(|| row.distill_brief.as_ref().map(|b| b.key_facts.clone()))
        .unwrap_or_default();
    let projected = decision_item(input);
    let evidence = [
        "subject",
        "sender",
        "summary",
        "safe_brief",
        "follow_up_hint",
        "latest_direction",
        "latest_intent",
        "recent_handled_followups",
    ]
    .into_iter()
    .filter(|k| {
        projected.state.as_json()[*k].as_str().is_some_and(|s| {
            !s.is_empty() && !["none", "unknown", "(none)", "(unknown)"].contains(&s)
        })
    })
    .collect::<Vec<_>>();
    let probability = |id: &str| answers.get(id).and_then(Answer::noul_value).unwrap_or(0.5);
    let semantic = serde_json::json!({
        "communication_type": choice(answers,"communication_type").unwrap_or("unknown"),
        "requested_action": choice(answers,"requested_action").unwrap_or("unknown"),
        "action_owner": choice(answers,"obligation_owner").unwrap_or("unknown"),
        "direct_request_probability": probability("direct_request"), "broadcast_probability": probability("broadcast"),
        "personal_obligation_probability": probability("personal_obligation"), "information_value_probability": probability("information_value"),
        "deadline": {"kind":choice(answers,"deadline_kind").unwrap_or(if due.is_some() {"explicit"} else {"none"}), "value":due},
        "campaign_or_event_identity":null,"evidence_refs":evidence
    });
    let confidence = answers.get("lane")?.choice_confidence()?;
    if !confidence.is_finite()
        || !(0.0..=1.0).contains(&confidence)
        || !repeat.is_finite()
        || !(0.0..=1.0).contains(&repeat)
    {
        return None;
    }
    let mut c = classify::parse_classification(&serde_json::json!({
        "label":lane,"confidence":confidence,"needs_reply":lane=="needs_reply",
        "follow_up_kind":follow_up,"action_owner":actor,"urgency":urgency,
        "due_text":due,"key_details":details,"reason":format!("{lane} · {follow_up} · {actor} · {urgency}"),
        "repeat_of_recently_handled":repeat>=0.5,"semantic_features":semantic
    }).to_string()).ok()?;
    c.semantic_features.input_revision = row.distill_revision;
    c.semantic_features.prompt_version = format!("channel_classify/{pack_version}");
    c.semantic_features.model = Some(model.into());
    c.semantic_features.profile = Some(format!(
        "decision-engine:{model}:channel_classify/{pack_version}"
    ));
    classify::normalize_classification_for_row(row, &mut c);
    Some(c)
}

#[derive(Default)]
pub struct DeciderOutcome {
    pub resolved: BTreeMap<String, (Classification, ClassifiedBy)>,
    pub incumbent: BTreeMap<String, Result<Classification>>,
    pub authority: Option<Participation>,
    pub observation: Option<decisions::telemetry::Observation>,
    pub incumbent_receipts: BTreeMap<String, Reference>,
}
#[async_trait]
pub trait ClassifyDecider: Send + Sync {
    async fn max_items(&self) -> usize {
        usize::MAX
    }
    async fn available(&self) -> bool {
        false
    }
    async fn decide(
        &self,
        inputs: &[ClassifyInput],
        llm: &dyn ClassifyLlm,
        system: &str,
        concurrency: usize,
    ) -> Result<DeciderOutcome>;
}
pub struct NoDecider;
#[async_trait]
impl ClassifyDecider for NoDecider {
    async fn decide(
        &self,
        _: &[ClassifyInput],
        _: &dyn ClassifyLlm,
        _: &str,
        _: usize,
    ) -> Result<DeciderOutcome> {
        Ok(DeciderOutcome::default())
    }
}
pub struct EngineClassifyDecider {
    pub router: Arc<OperationLlmRouter>,
    pub scope: magicllm::LlmScope,
    pub store: super::store::MailAssistStore,
    pub reference_llm: Arc<classify::RouterClassifyLlm>,
}
impl EngineClassifyDecider {
    fn replay(
        &self,
        input: &runner::Input,
        policy: &Participation,
        inputs: &[ClassifyInput],
        system: &str,
    ) -> Option<Result<runner::ReferenceReplay, &'static str>> {
        use decisions::{observation, reference};
        if !observation::selected(input, policy) {
            return None;
        }
        let Some((snapshot, bytes)) =
            observation::snapshot_bounded::<_, (Vec<ClassifyInput>, String)>(&(inputs, system))
        else {
            return Some(Err("snapshot_oversize"));
        };
        let snapshot = Arc::new(snapshot);
        let source = snapshot.clone();
        let router = self.router.clone();
        let version = input.reference_version.clone();
        let scope = self.scope.clone();
        let store = self.store.clone();
        let llm = self.reference_llm.clone();
        let reservation =
            reference::observation_cost_upper_microusd(&self.router, "channel_classify", bytes)
                .and_then(|cost| cost.checked_mul(inputs.len() as u64).ok_or("cost_overflow"));
        Some(Ok(runner::ReferenceReplay {
            bytes,
            cost_reservation_microusd: reservation,
            current: Box::new(move || {
                let (source, router, version, scope, store) = (
                    source.clone(),
                    router.clone(),
                    version.clone(),
                    scope.clone(),
                    store.clone(),
                );
                Box::pin(async move {
                    if !reference::scope_current(&router, &scope)
                        || reference::version(
                            &router,
                            "channel_classify",
                            magician::magician_v2::prompts::versions::CHANNEL_CLASSIFY,
                        )
                        .as_ref()
                            != Some(&version)
                    {
                        return false;
                    }
                    sources_current(&store, &scope, &source.0).await
                })
            }),
            access_current: None,
            run: Box::new(move || {
                Box::pin(async move {
                    let started = std::time::Instant::now();
                    let mut results = BTreeMap::new();
                    for i in &snapshot.0 {
                        results.insert(
                            item_id(&i.row),
                            classify::classify_with_system_observed_at(
                                llm.as_ref(),
                                &snapshot.1,
                                &i.row,
                                &i.recent_handled,
                                i.now,
                            )
                            .await,
                        );
                    }
                    let mut reference = labels(&results);
                    reference.latency_ms = started.elapsed().as_millis() as u64;
                    runner::ReplayResult {
                        status: if results.values().all(Result::is_ok) {
                            "completed"
                        } else {
                            "failed"
                        },
                        attempted: Some(true),
                        reference: Some(reference),
                    }
                })
            }),
        }))
    }
}
pub(super) fn classification_labels(c: &Classification) -> BTreeMap<String, serde_json::Value> {
    let mut heads = BTreeMap::from([
        ("lane".into(), serde_json::json!(c.label)),
        (
            "repeat_of_recently_handled".into(),
            serde_json::json!(c.repeat_of_recently_handled),
        ),
    ]);
    for (key, default) in [
        ("follow_up_kind", "none"),
        ("action_owner", "unknown"),
        ("urgency", "normal"),
    ] {
        heads.insert(
            key.into(),
            c.proposed_action
                .as_ref()
                .and_then(|v| v.get(key))
                .cloned()
                .unwrap_or_else(|| serde_json::json!(default)),
        );
    }
    heads
}
fn labels(classifications: &BTreeMap<String, Result<(Classification, Reference)>>) -> Reference {
    let mut reference: Reference = classifications
        .iter()
        .filter_map(|(id, result)| {
            let (c, receipt) = result.as_ref().ok()?;
            // Compare producer judgments before common lifecycle overrides.
            // The applied classification still uses the incumbent normalizer.
            let heads = receipt
                .labels
                .get("")
                .cloned()
                .unwrap_or_else(|| classification_labels(c));
            Some((id.clone(), heads))
        })
        .collect::<BTreeMap<_, _>>()
        .into();
    reference.calls = classifications
        .values()
        .filter_map(|r| r.as_ref().ok()?.1.call.clone())
        .collect();
    reference.latency_ms = classifications
        .values()
        .filter_map(|r| r.as_ref().ok().map(|v| v.1.latency_ms))
        .sum();
    reference
}
#[async_trait]
impl ClassifyDecider for EngineClassifyDecider {
    async fn max_items(&self) -> usize {
        match decisions::policy_lookup("channel_classify", &self.scope).await {
            decisions::PolicyLookup::Participating(policy) => {
                policy.policy.classification.limits.max_items.min(8)
            },
            // Preserve the incumbent pass size while the engine is disabled
            // or unavailable; only an active engine owns decision admission.
            _ => usize::MAX,
        }
    }
    async fn available(&self) -> bool {
        matches!(
            decisions::policy_lookup("channel_classify", &self.scope).await,
            decisions::PolicyLookup::Participating(_)
        )
    }
    async fn decide(
        &self,
        inputs: &[ClassifyInput],
        llm: &dyn ClassifyLlm,
        system: &str,
        concurrency: usize,
    ) -> Result<DeciderOutcome> {
        if inputs.is_empty() {
            return Ok(DeciderOutcome::default());
        }
        let lookup = decisions::policy_lookup("channel_classify", &self.scope).await;
        if !matches!(lookup, decisions::PolicyLookup::Participating(_)) {
            // The caller owns the ordinary fallback pass. In particular, Off
            // must not place a large LLM-only batch inside the rail's total
            // deadline and then discard completed rows when that deadline ends.
            return Ok(DeciderOutcome::default());
        }
        let reference_version = decisions::reference::version(
            &self.router,
            "channel_classify",
            magician::magician_v2::prompts::versions::CHANNEL_CLASSIFY,
        )
        .unwrap_or_else(|| "channel_classify:unbound".into());
        let items = inputs.iter().map(decision_item).collect::<Vec<_>>();
        let revisions = inputs
            .iter()
            .map(|i| (&i.row.latest_message_id, i.row.distill_revision))
            .collect::<Vec<_>>();
        let case_id = blake3::hash(&serde_json::to_vec(&(&items, revisions))?)
            .to_hex()
            .to_string();
        let mut input = runner::Input {
            operation: "channel_classify".into(),
            projection_version: PROJECTION_VERSION.into(),
            reference_version,
            case_id,
            context: Some(decision_context(&self.scope.principal, inputs[0].now)),
            items,
            required_questions: REQUIRED_QUESTIONS.map(str::to_owned).to_vec(),
            scope: decisions::trace_context(&self.router, self.scope.clone()),
            agent: None,
            requires_completion: false,
            replay: None,
        };
        if let decisions::PolicyLookup::Participating(p) = &lookup {
            input.replay = self.replay(&input, p, inputs, system);
        }
        let outcome = runner::run(
            input,
            lookup,
            Duration::from_secs(120),
            Duration::ZERO,
            |ids, _| async move {
                if !llm.bound() {
                    return None;
                }
                let mut selected = Vec::new();
                for i in inputs {
                    if ids.contains(&item_id(&i.row)) {
                        selected.push(i.clone());
                    }
                }
                let started = std::time::Instant::now();
                let results = stream::iter(selected)
                    .map(|i| async move {
                        (
                            item_id(&i.row),
                            classify::classify_with_system_observed_at(
                                llm,
                                system,
                                &i.row,
                                &i.recent_handled,
                                i.now,
                            )
                            .await,
                        )
                    })
                    .buffer_unordered(concurrency.max(1))
                    .collect::<BTreeMap<_, _>>()
                    .await;
                Some((results, started.elapsed().as_millis() as u64))
            },
            |(results, elapsed)| {
                let mut reference = labels(results);
                reference.latency_ms = *elapsed;
                reference
            },
        )
        .await;
        let mut resolved = BTreeMap::new();
        for (id, answers) in outcome.current_answers() {
            if let (Some(input), Some(origin)) = (
                inputs.iter().find(|i| item_id(&i.row) == id),
                outcome.origins.get(&id),
            ) {
                let model = &origin.model.model;
                if let Some(c) =
                    classification_from_answers(input, &answers, model, &origin.pack_version)
                {
                    resolved.insert(
                        id,
                        (
                            c,
                            ClassifiedBy {
                                origin: "decision_engine".into(),
                                model: Some(model.clone()),
                                pack_version: Some(origin.pack_version.clone()),
                                projection_version: Some(PROJECTION_VERSION.into()),
                            },
                        ),
                    );
                }
            }
        }
        let mut incumbent_receipts = BTreeMap::new();
        Ok(DeciderOutcome {
            resolved,
            incumbent: outcome
                .incumbent
                .unwrap_or_default()
                .0
                .into_iter()
                .map(|(id, r)| {
                    let result = r.map(|(c, receipt)| {
                        incumbent_receipts.insert(id.clone(), receipt);
                        c
                    });
                    (id, result)
                })
                .collect(),
            incumbent_receipts,
            authority: outcome.authority,
            observation: outcome.observation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decision_engine_contract::primitives::OptionId;
    fn input() -> ClassifyInput {
        ClassifyInput {
            row: classify::tests::row(),
            recent_handled: vec![],
            now: 1_783_209_600_000,
        }
    }
    fn answers() -> BTreeMap<String, Answer> {
        let mut answers = BTreeMap::new();
        for (id, value) in [
            ("lane", "needs_reply"),
            ("follow_up_kind", "needs_reply"),
            ("action_owner", "owner"),
            ("urgency", "normal"),
        ] {
            answers.insert(
                id.into(),
                Answer::Choice {
                    choice: OptionId::new(value),
                    confidence: 0.95,
                    probabilities: BTreeMap::from([(OptionId::new(value), 0.95)]),
                },
            );
        }
        answers.insert(
            "repeat_of_recently_handled".into(),
            Answer::Noul { noul: 0.01 },
        );
        answers
    }
    #[test]
    fn projection_matches_prompt_keys_and_caps_summary_without_splitting_unicode() {
        let mut i = input();
        i.row.latest_summary = Some("é".repeat(1200));
        let item = decision_item(&i);
        let case = serde_json::json!({"projection_version":PROJECTION_VERSION,
            "input":{"state":item.state.as_json()}});
        validate_case_projection(&case).unwrap();
        let mut with_body = case.clone();
        with_body["input"]["state"]["body"] = serde_json::json!("must not be sent");
        assert!(validate_case_projection(&with_body).is_err());
        for key in classify::user_vars(&i.row, i.now, &i.recent_handled).keys() {
            assert!(item.state.as_json().get(key).is_some(), "{key}");
        }
        assert_eq!(
            item.state.as_json()["summary"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            900
        );
        let mut other = i.row.clone();
        other.account_alias = "other".into();
        assert_ne!(item_id(&i.row), item_id(&other));
    }
    #[test]
    fn all_required_heads_are_mandatory_and_invalid_vocabulary_is_unresolved() {
        for head in REQUIRED_QUESTIONS {
            let mut a = answers();
            a.remove(head);
            assert!(
                classification_from_answers(&input(), &a, "jev", "1.0.0").is_none(),
                "{head}"
            );
        }
        let mut a = answers();
        a.insert(
            "lane".into(),
            Answer::Choice {
                choice: OptionId::new("invalid"),
                confidence: 1.0,
                probabilities: BTreeMap::new(),
            },
        );
        assert!(classification_from_answers(&input(), &a, "jev", "1.0.0").is_none());
    }
    #[test]
    fn derivation_preserves_hint_due_and_sanitizes_details_and_reason() {
        let mut i = input();
        i.row.follow_up_hint.as_mut().unwrap().key_details = vec!["X".repeat(200); 9];
        let c = classification_from_answers(&i, &answers(), "jev", "1.0.0").unwrap();
        let action = c.proposed_action.unwrap();
        assert_eq!(action["due_text"], "soon");
        assert_eq!(action["needs_reply"], true);
        assert!(action["key_details"].as_array().unwrap().len() <= 6);
        assert!(action["key_details"].as_array().unwrap().iter().all(|v| v
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 120));
        assert!(!c.reason.unwrap().contains(i.row.subject.as_ref().unwrap()));
        let features = serde_json::to_value(c.semantic_features).unwrap();
        assert_eq!(features["features"]["communication_type"], "unknown");
        assert_eq!(features["features"]["deadline"]["kind"], "explicit");
        assert_eq!(features["features"]["direct_request_probability"], 0.5);
    }

    #[test]
    fn semantic_provenance_uses_actual_pack_version() {
        let c = classification_from_answers(&input(), &answers(), "jev", "1.1.0").unwrap();
        assert_eq!(c.semantic_features.prompt_version, "channel_classify/1.1.0");
        assert_eq!(
            c.semantic_features.profile.as_deref(),
            Some("decision-engine:jev:channel_classify/1.1.0")
        );
    }
    #[test]
    fn outbound_schedule_uses_the_incumbent_normalizer() {
        let mut i = input();
        i.row.latest_direction = Some("outbound".into());
        let mut a = answers();
        a.insert(
            "follow_up_kind".into(),
            Answer::Choice {
                choice: OptionId::new("schedule"),
                confidence: 0.95,
                probabilities: BTreeMap::new(),
            },
        );
        let c = classification_from_answers(&i, &a, "jev", "1.0.0").unwrap();
        assert_eq!(c.proposed_action.unwrap()["follow_up_kind"], "owner_owes");
    }
    #[test]
    fn legacy_annotation_deserialization_defaults_to_llm() {
        let old = serde_json::json!({"id":"a","provider":"gmail","account_alias":"a","thread_id":"t","state":"classified","created_at":1,"updated_at":1});
        let annotation: super::super::types::MailThreadAnnotation =
            serde_json::from_value(old).unwrap();
        assert_eq!(annotation.classified_by.origin, "llm");
    }

    struct CountingLlm(std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl ClassifyLlm for CountingLlm {
        fn bound(&self) -> bool {
            true
        }
        async fn complete(&self, _: &str, _: &str) -> Result<String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(serde_json::json!({"label":"fyi","confidence":0.95,"follow_up_kind":"none","action_owner":"unknown","urgency":"normal"}).to_string())
        }
    }
    struct ScriptedDecider {
        resolved: usize,
        failed: bool,
    }
    struct ShadowDecider;
    struct BoundedDecider;
    #[async_trait]
    impl ClassifyDecider for BoundedDecider {
        async fn max_items(&self) -> usize {
            2
        }
        async fn decide(
            &self,
            inputs: &[ClassifyInput],
            llm: &dyn ClassifyLlm,
            system: &str,
            concurrency: usize,
        ) -> Result<DeciderOutcome> {
            assert!(inputs.len() <= 2, "caller exceeded the active engine limit");
            ScriptedDecider {
                resolved: inputs.len(),
                failed: false,
            }
            .decide(inputs, llm, system, concurrency)
            .await
        }
    }
    #[tokio::test]
    async fn active_decision_batch_limit_leaves_excess_threads_for_next_pass() {
        let temp = tempfile::tempdir().unwrap();
        let store = seeded_store(temp.path()).await;
        let llm = CountingLlm(Default::default());
        let first = classify::run_classify_pass_with_decider(
            &store,
            "anonymous",
            "default",
            &llm,
            &BoundedDecider,
            8,
            1,
            None,
        )
        .await
        .unwrap();
        assert_eq!(first.classified, 2);
        assert_eq!(
            store
                .list_threads_to_classify("anonymous", "default", 8)
                .await
                .unwrap()
                .len(),
            1
        );
        let second = classify::run_classify_pass_with_decider(
            &store,
            "anonymous",
            "default",
            &llm,
            &BoundedDecider,
            8,
            1,
            None,
        )
        .await
        .unwrap();
        assert_eq!(second.classified, 1);
        assert_eq!(llm.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    #[async_trait]
    impl ClassifyDecider for ShadowDecider {
        async fn decide(
            &self,
            inputs: &[ClassifyInput],
            llm: &dyn ClassifyLlm,
            system: &str,
            _: usize,
        ) -> Result<DeciderOutcome> {
            let mut incumbent = BTreeMap::new();
            for i in inputs {
                incumbent.insert(
                    item_id(&i.row),
                    classify::classify_with_system(llm, system, &i.row, &i.recent_handled).await,
                );
            }
            Ok(DeciderOutcome {
                incumbent,
                ..Default::default()
            })
        }
    }
    #[tokio::test]
    async fn shadow_reuses_reference_classifications_without_duplicate_llm_calls() {
        let temp = tempfile::tempdir().unwrap();
        let store = seeded_store(temp.path()).await;
        let llm = CountingLlm(Default::default());
        let out = classify::run_classify_pass_with_decider(
            &store,
            "anonymous",
            "default",
            &llm,
            &ShadowDecider,
            8,
            1,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out.classified, 3);
        assert_eq!(llm.0.load(std::sync::atomic::Ordering::SeqCst), 3);
        let rows = store
            .list_annotations_by_thread_ids(
                "anonymous",
                "default",
                "gmail",
                "acct",
                &["one".into(), "two".into(), "three".into()],
            )
            .await
            .unwrap();
        assert!(rows
            .iter()
            .all(|r| r.classified_by.origin == "llm" && r.label.as_deref() == Some("fyi")));
    }
    #[test]
    fn reference_labels_restore_the_closed_none_action() {
        let c = classify::parse_classification(r#"{"label":"fyi","confidence":0.95,"follow_up_kind":"none","action_owner":"unknown","urgency":"normal"}"#).unwrap();
        let reference = labels(&BTreeMap::from([(
            "id".into(),
            Ok((c, Reference::from(BTreeMap::new()))),
        )]));
        assert_eq!(reference.labels["id"]["follow_up_kind"], "none");
        assert_eq!(reference.labels["id"].len(), 5);
    }
    #[async_trait]
    impl ClassifyDecider for ScriptedDecider {
        async fn available(&self) -> bool {
            true
        }
        async fn decide(
            &self,
            inputs: &[ClassifyInput],
            _: &dyn ClassifyLlm,
            _: &str,
            _: usize,
        ) -> Result<DeciderOutcome> {
            if self.failed {
                anyhow::bail!("scripted transport failure");
            }
            let resolved = inputs
                .iter()
                .take(self.resolved)
                .map(|i| {
                    (
                        item_id(&i.row),
                        (
                            classification_from_answers(i, &answers(), "test-jev", "1.0.0")
                                .unwrap(),
                            ClassifiedBy {
                                origin: "decision_engine".into(),
                                model: Some("test-jev".into()),
                                pack_version: Some("1.0.0".into()),
                                projection_version: Some(PROJECTION_VERSION.into()),
                            },
                        ),
                    )
                })
                .collect();
            Ok(DeciderOutcome {
                resolved,
                ..Default::default()
            })
        }
    }
    async fn seeded_store(root: &std::path::Path) -> super::super::store::MailAssistStore {
        use magician::magician_v2::prompts::{
            json_storage::JsonStorageConfig, set_global_prompt_manager, JsonPromptStorage,
            PromptManager,
        };
        let storage = JsonPromptStorage::new(JsonStorageConfig {
            storage_dir: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../data/magician_v2/prompts"),
            ..Default::default()
        })
        .unwrap();
        set_global_prompt_manager(Arc::new(PromptManager::new(Arc::new(storage))));
        use crate::channel_assist::store::tests::{sample_message, sample_thread};
        let store = super::super::store::MailAssistStore::open(root).unwrap();
        for id in ["one", "two", "three"] {
            store
                .upsert_thread("anonymous", "default", sample_thread("acct", id, 1000))
                .await
                .unwrap();
            store
                .append_messages("anonymous", "default", vec![sample_message("acct", id, id)])
                .await
                .unwrap();
            store
                .set_distill_result(
                    "anonymous",
                    "default",
                    "gmail",
                    "acct",
                    id,
                    "A useful informational update with no obligation.",
                    "fyi",
                    false,
                    None,
                )
                .await
                .unwrap();
        }
        store
    }
    #[tokio::test]
    async fn observation_source_guard_rejects_changed_revision_and_parent_suppression() {
        let temp = tempfile::tempdir().unwrap();
        let store = seeded_store(temp.path()).await;
        let scope = magicllm::LlmScope {
            principal: "anonymous".into(),
            workspace: "default".into(),
        };
        let inputs: Vec<_> = store
            .list_threads_to_classify("anonymous", "default", 8)
            .await
            .unwrap()
            .into_iter()
            .map(|row| ClassifyInput {
                row,
                recent_handled: vec![],
                now: 1_790_000_000_000,
            })
            .collect();
        assert!(sources_current(&store, &scope, &inputs).await);
        store
            .set_distill_result(
                "anonymous",
                "default",
                "gmail",
                "acct",
                "one",
                "A changed informational summary.",
                "fyi",
                false,
                None,
            )
            .await
            .unwrap();
        let first = inputs
            .iter()
            .find(|input| input.row.thread_id == "one")
            .unwrap();
        assert!(!sources_current(&store, &scope, std::slice::from_ref(first)).await);
        let second = inputs
            .iter()
            .find(|input| input.row.thread_id == "two")
            .unwrap();
        assert!(sources_current(&store, &scope, std::slice::from_ref(second)).await);
        let mut suppressed = super::super::store::tests::sample_thread("acct", "two", second.now);
        suppressed.sensitive_suppressed = true;
        store
            .upsert_thread("anonymous", "default", suppressed)
            .await
            .unwrap();
        assert!(!sources_current(&store, &scope, std::slice::from_ref(second)).await);
    }

    #[tokio::test]
    async fn unresolved_decisions_with_an_unbound_llm_remain_idle_and_queued() {
        struct Unbound;
        #[async_trait]
        impl ClassifyLlm for Unbound {
            fn bound(&self) -> bool {
                false
            }
            async fn complete(&self, _: &str, _: &str) -> Result<String> {
                panic!("an unbound classifier must never dispatch");
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let store = seeded_store(temp.path()).await;
        let out = classify::run_classify_pass_with_decider(
            &store,
            "anonymous",
            "default",
            &Unbound,
            &ScriptedDecider {
                resolved: 0,
                failed: true,
            },
            8,
            1,
            None,
        )
        .await
        .unwrap();
        assert!(out.idle);
        assert_eq!(out.drained, 0);
        assert_eq!(
            store
                .list_threads_to_classify("anonymous", "default", 8)
                .await
                .unwrap()
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn partial_decisions_call_llm_only_for_unresolved_rows_and_round_trip_provenance() {
        let temp = tempfile::tempdir().unwrap();
        let store = seeded_store(temp.path()).await;
        let llm = CountingLlm(Default::default());
        let out = classify::run_classify_pass_with_decider(
            &store,
            "anonymous",
            "default",
            &llm,
            &ScriptedDecider {
                resolved: 2,
                failed: false,
            },
            8,
            1,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out.classified, 3);
        assert_eq!(llm.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        let rows = store
            .list_annotations_by_thread_ids(
                "anonymous",
                "default",
                "gmail",
                "acct",
                &["one".into(), "two".into(), "three".into()],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter()
                .filter(|r| r.classified_by.origin == "decision_engine")
                .count(),
            2
        );
        assert_eq!(
            rows.iter()
                .filter(|r| r.classified_by.origin == "llm")
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn decision_transport_failure_preserves_the_entire_llm_path() {
        let temp = tempfile::tempdir().unwrap();
        let store = seeded_store(temp.path()).await;
        let llm = CountingLlm(Default::default());
        let out = classify::run_classify_pass_with_decider(
            &store,
            "anonymous",
            "default",
            &llm,
            &ScriptedDecider {
                resolved: 0,
                failed: true,
            },
            8,
            1,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out.classified, 3);
        assert_eq!(llm.0.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    #[ignore = "real classify model calls; isolated engine socket and private output required"]
    async fn channel_classify_live_round_trip() -> Result<()> {
        magician_chunking::register_builtin_chunk_adapters()?;
        use magician::{
            config::{load_magician_config_from_path, DecisionHostConfig, DecisionMode},
            magician_v2::{
                decision_host,
                realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
            },
        };
        let socket = std::env::var("CLASSIFY_LIVE_SOCKET")?;
        anyhow::ensure!(
            socket.starts_with("/tmp/channel-classify-"),
            "live fixture needs an isolated test socket"
        );
        let mode = std::env::var("CLASSIFY_LIVE_MODE")?;
        anyhow::ensure!(
            ["shadow", "gate", "off", "outage"].contains(&mode.as_str()),
            "invalid fixture mode"
        );
        // The ignored fixture gets an explicit test root from its Make target;
        // satellite production code never resolves the ambient runtime root.
        let root = std::path::PathBuf::from(std::env::var("CLASSIFY_LIVE_ROOT")?);
        let output = private_output(
            &std::path::PathBuf::from(std::env::var("CLASSIFY_LIVE_OUTPUT")?).join("report.json"),
            &root,
        )?
        .parent()
        .unwrap()
        .to_path_buf();
        anyhow::ensure!(!output.exists(), "fixture output must be fresh");
        std::fs::create_dir_all(&output)?;
        for name in [".env.development", ".env"] {
            let _ = dotenvy::from_path(root.join(name));
        }
        let config = load_magician_config_from_path(&root.join("magician-config.yaml"))?;
        let router = Arc::new(OperationLlmRouter::new(config.router_config().cloned()));
        let bus = Arc::new(RuntimeTransportBroadcaster::new(4096));
        let mut events = bus.subscribe();
        router.set_event_broadcaster(bus.clone());
        let queue = magicllm::LlmDispatchQueue::start(
            router
                .shared_configured_router()
                .ok_or_else(|| anyhow::anyhow!("classifier router unavailable"))?,
            Arc::new(magicllm::dispatch::NoopTaskStateView),
            Arc::new(magicllm::dispatch::NoopTaskLedgerSink),
            config.llm.dispatch.clone(),
        );
        router.set_dispatch_queue(queue);
        let activation =
            magician::magician_v2::analytics::llm_trace_activation::LlmTraceActivation::start(
                bus.clone(),
                magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
                    output.join("ledger"),
                ),
                Default::default(),
            )?;
        decision_host::set_health_broadcaster(&bus);
        decision_host::configure(&DecisionHostConfig {
            mode: if mode == "off" {
                DecisionMode::Off
            } else {
                DecisionMode::AllEngines
            },
            socket: Some(if mode == "outage" {
                format!("{socket}.missing")
            } else {
                socket
            }),
            ..Default::default()
        });
        if mode == "shadow" || mode == "gate" {
            anyhow::ensure!(
                decision_host::prime_classification_for_eval(
                    "channel_classify",
                    "anonymous",
                    "default"
                )
                .await,
                "classify policy unavailable"
            );
            let decisions::PolicyLookup::Participating(policy) = decisions::policy_lookup(
                "channel_classify",
                &magicllm::LlmScope::new("anonymous", "default"),
            )
            .await
            else {
                anyhow::bail!("classify policy unavailable")
            };
            anyhow::ensure!(
                if mode == "shadow" {
                    policy.policy.shadow && !policy.policy.gate
                } else {
                    policy.policy.gate
                        && !policy.policy.shadow
                        && policy.policy.classification.allow_unqualified_gate
                },
                "unexpected test policy"
            );
        }
        struct LiveLlm {
            inner: classify::RouterClassifyLlm,
            calls: std::sync::atomic::AtomicUsize,
        }
        #[async_trait]
        impl ClassifyLlm for LiveLlm {
            fn bound(&self) -> bool {
                self.inner.bound()
            }
            fn semantic_extractor_identity(
                &self,
            ) -> magician::magician_v2::attention::learning::SemanticExtractorIdentity {
                self.inner.semantic_extractor_identity()
            }
            async fn complete(&self, system: &str, user: &str) -> Result<String> {
                Ok(self.complete_observed(system, user).await?.0)
            }
            async fn complete_observed(
                &self,
                system: &str,
                user: &str,
            ) -> Result<(String, Option<decisions::telemetry::ReferenceCall>)> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.inner.complete_observed(system, user).await
            }
        }
        let llm = LiveLlm {
            inner: classify::RouterClassifyLlm::new(
                router.clone(),
                Some(bus.clone()),
                "anonymous",
                "default",
            ),
            calls: Default::default(),
        };
        anyhow::ensure!(llm.bound(), "incumbent classifier is unbound");
        let store = seeded_store(&output.join("channel-store")).await;
        let decider = EngineClassifyDecider {
            router: router.clone(),
            scope: magicllm::LlmScope::new("anonymous", "default"),
            store: store.clone(),
            reference_llm: Arc::new(classify::RouterClassifyLlm::new(
                router,
                Some(bus),
                "anonymous",
                "default",
            )),
        };
        let batch_limit = decider.max_items().await;
        let expected_threads = 3.min(batch_limit);
        let started = std::time::Instant::now();
        let result = classify::run_classify_pass_with_decider(
            &store,
            "anonymous",
            "default",
            &llm,
            &decider,
            8,
            1,
            None,
        )
        .await?;
        let annotations = store
            .list_annotations_by_thread_ids(
                "anonymous",
                "default",
                "gmail",
                "acct",
                &["one".into(), "two".into(), "three".into()],
            )
            .await?;
        let engine_rows = annotations
            .iter()
            .filter(|a| a.classified_by.origin == "decision_engine")
            .count();
        let llm_calls = llm.calls.load(std::sync::atomic::Ordering::SeqCst);
        let pending = store
            .list_threads_to_classify("anonymous", "default", 8)
            .await?
            .len();
        let mut observations = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let RuntimeTransportEvent::DecisionShadowAgreement { record, .. } = event {
                observations.push(*record);
            }
        }
        let shutdown = activation.shutdown().await;
        let report = serde_json::json!({"mode":mode,"batch_limit":batch_limit,"pending":pending,"classified":result.classified,"failed":result.failed,
            "engine_annotations":engine_rows,"llm_calls":llm_calls,"latency_ms":started.elapsed().as_millis(),
            "observations":observations,"ledger":shutdown});
        std::fs::write(
            output.join("report.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        anyhow::ensure!(
            result.classified == expected_threads
                && result.failed == 0
                && annotations.len() == expected_threads
                && pending == 3 - expected_threads,
            "real classification/persistence failed"
        );
        if mode == "gate" {
            anyhow::ensure!(
                engine_rows > 0 && llm_calls == expected_threads - engine_rows,
                "gated annotations did not reduce LLM calls"
            );
        } else {
            anyhow::ensure!(
                engine_rows == 0 && llm_calls == expected_threads,
                "incumbent path changed"
            );
        }
        if mode == "shadow" || mode == "gate" {
            anyhow::ensure!(
                !report["observations"].as_array().unwrap().is_empty(),
                "missing decision comparison observations"
            );
        }
        println!(
            "{mode}: classified={expected_threads} engine={engine_rows} llm_calls={llm_calls} pending={pending} -> {}",
            output.display()
        );
        Ok(())
    }
}
