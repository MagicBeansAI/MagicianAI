//! Focused real-model acceptance using the production connection worker.
//! Synthetic scoped state only; raw failures are retained alongside successes.
use anyhow::{ensure, Context, Result};
use magician::{
    config::load_magician_config_from_path,
    magician_v2::{
        agents::AgentMemoryResolver,
        artifact_v2::workspace::ArtifactV2Workspace,
        attention::resurfacing::{memory_connections::*, store::ResurfacingStore, types::*},
        attention_funnel::AttentionScope,
        attention_funnel_store::AttentionFunnelStore,
        feed::FeedStore,
        query_analysis::operation_llm_router::OperationLlmRouter,
        realtime_events::RuntimeTransportBroadcaster,
        user_requests::UserRequestService,
    },
};
use magician_comms::channel_assist::resurfacing::{
    interaction::{MemoryInteractionAdapter, ResurfacingInteractionRegistry},
    memory_connections::{ConnectionReviewObservation, ConnectionRuntime},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, fs, path::PathBuf, sync::Arc, time::Instant};

#[derive(Clone, Deserialize)]
struct Memory {
    key: String,
    text: String,
    #[serde(default)]
    lifecycle: Option<String>,
    #[serde(default)]
    foreign: bool,
}

#[derive(Clone, Deserialize)]
struct Expected {
    kind: String,
    surfaces: Vec<ConnectionSurface>,
    required_memory_keys: Vec<String>,
    rationale: String,
}

#[derive(Clone, Deserialize)]
struct Case {
    id: String,
    partition: String,
    category: String,
    title: String,
    activity: String,
    memories: Vec<Memory>,
    expected: Expected,
    #[serde(default)]
    smoke: bool,
}

fn observation(o: &ConnectionReviewObservation) -> Value {
    let response = o.response.as_ref();
    let telemetry = response.and_then(|r| r.telemetry.as_ref());
    let usage = telemetry
        .filter(|t| t.usage_reported)
        .and_then(|_| response.and_then(|r| r.usage.as_ref()));
    json!({
        "candidate_id":o.candidate_id,"sources":o.sources,
        "raw_response":response.map(|r| &r.content),"error":o.error,
        "latency_ms":o.elapsed_ms,
        "usage":usage.map(|u|json!({"input_tokens":u.prompt_tokens,"output_tokens":u.completion_tokens,"total_tokens":u.total_tokens})),
        "provider":telemetry.map(|t| &t.provider),"model":telemetry.map(|t| &t.model),
        "profile":telemetry.and_then(|t|t.profile.as_ref()),
        "reasoning_tokens":telemetry.filter(|t|t.usage_reported).map(|t|t.reasoning_tokens),
        "router_estimated_cost_usd":telemetry.filter(|t|t.usage_reported && t.cost_usd.is_finite() && t.cost_usd>0.0).map(|t|t.cost_usd),
    })
}

#[test]
fn memory_eval_usage_requires_a_provider_usage_object() {
    use magician::magician_v2::{
        query_analysis::operation_llm_router::{SimplifiedLLMResponse, SimplifiedTokenUsage},
        slot_graph::extraction::LlmCallTelemetry,
    };
    let mut observed = ConnectionReviewObservation {
        candidate_id: "test".into(),
        sources: vec![],
        error: None,
        elapsed_ms: 1,
        response: Some(SimplifiedLLMResponse {
            content: "{\"connection\":null}".into(),
            usage: Some(SimplifiedTokenUsage {
                prompt_tokens: 100,
                completion_tokens: 23,
                total_tokens: 123,
            }),
            ..Default::default()
        }),
    };
    assert!(observation(&observed)["usage"].is_null());
    observed.response.as_mut().unwrap().telemetry = Some(LlmCallTelemetry::default());
    assert!(observation(&observed)["usage"].is_null());
    observed
        .response
        .as_mut()
        .unwrap()
        .telemetry
        .as_mut()
        .unwrap()
        .usage_reported = true;
    assert_eq!(observation(&observed)["usage"]["total_tokens"], 123);
}

async fn evaluate(
    case: &Case,
    repeat: usize,
    router: Arc<OperationLlmRouter>,
    root: PathBuf,
) -> Result<Value> {
    fs::create_dir(&root)?;
    let layout = ArtifactV2Workspace::new(&root);
    let resolver = AgentMemoryResolver::with_workspace_layout(layout.clone());
    let scope = AttentionScope {
        principal: "memory-eval".into(),
        workspace: format!("{}-{repeat}", case.id),
    };
    let memory = resolver.resolve_for_scope(&scope.principal, &scope.workspace)?;
    let entries: Vec<_> = case.memories.iter().filter(|m| !m.foreign).map(|m|json!({"key":m.key,"value":m.text,"source_type":"explicit_user_statement","confidence":1.0,"memory_lifecycle":m.lifecycle})).collect();
    let activity =
        json!({"key":"current_activity","value":case.activity,"updated_at":"2026-09-12T00:00:00Z"});
    memory
        .persist_user_knowledge(&json!({"user_preferences":entries,"user.knowledge":[activity]}))
        .await?;
    let foreign: Vec<_> = case
        .memories
        .iter()
        .filter(|m| m.foreign)
        .map(|m| json!({"key":m.key,"value":m.text}))
        .collect();
    if !foreign.is_empty() {
        resolver
            .resolve_for_scope("foreign-owner", &scope.workspace)?
            .persist_user_knowledge(&json!({"user_preferences":foreign}))
            .await?;
    }
    let requests = Arc::new(
        UserRequestService::new(Arc::new(RuntimeTransportBroadcaster::new(64)))
            .with_workspace_layout(layout.clone())
            .with_history_persist_path(root.join("request-history.json"))
            .with_pending_persist_path(root.join("request-pending.json"))
            .await,
    );
    let runtime = ConnectionRuntime {
        store: ResurfacingStore::open(&root)?,
        resolver: resolver.clone(),
        interactions: ResurfacingInteractionRegistry::from_adapters(vec![Arc::new(
            MemoryInteractionAdapter::new(resolver),
        )]),
        feed: FeedStore::open_workspace(layout)?,
        requests: Some(requests.clone()),
        router: Some(router),
        attention: Some(AttentionFunnelStore::open(&root)?),
        taste: None,
    };
    let now = chrono::Utc::now().timestamp();
    let candidate = Candidate {
        candidate_id: case.id.clone(),
        source_kind: SourceKind::Memory,
        source_ref: "user.knowledge#current_activity".into(),
        title: case.title.clone(),
        content_digest: case.activity.clone(),
        content_revision: Some("2026-09-12T00:00:00Z".into()),
        content_details: None,
        semantic_features: None,
        salience_score: 0.9,
        signals: Default::default(),
        temporal_anchor_at: None,
        embedding_id: None,
        state: CandidateState::Candidate,
        first_seen_at: now,
        last_scored_at: now,
        last_surfaced_at: None,
        cooldown_until: 0,
        surface_count: 0,
        dismiss_count: 0,
    };
    runtime
        .store
        .upsert_candidate(&scope.principal, &scope.workspace, &candidate)
        .await?;
    let before = memory.load_user_knowledge().await?;
    let mut observations = Vec::new();
    let started = Instant::now();
    let reviewed = runtime
        .pass_with_observer(&scope, &mut None, now, &mut |o| observations.push(o))
        .await?;
    let record = runtime
        .store
        .get_connection(&scope.principal, &scope.workspace, &case.id)
        .await?;
    let connection = record.as_ref().and_then(|r| r.connection.as_ref());
    let sources = observations
        .first()
        .map(|o| o.sources.as_slice())
        .unwrap_or(&[]);
    let required: Vec<_> = case
        .expected
        .required_memory_keys
        .iter()
        .map(|key| {
            case.memories
                .iter()
                .find(|m| &m.key == key)
                .context("unknown expected memory")
        })
        .collect::<Result<_>>()?;
    let recalled = required.iter().all(|m| {
        sources
            .iter()
            .any(|s| s.id != "activity" && s.text.contains(&m.text))
    });
    let cited = connection.is_some_and(|c| {
        required.iter().all(|m| {
            c.evidence.iter().any(|cite| {
                sources
                    .iter()
                    .any(|s| s.id == cite.id && s.text.contains(&m.text))
            })
        })
    });
    let provider_ok = observations.len() == 1
        && observations[0].error.is_none()
        && observations[0].response.is_some();
    let valid_empty = record
        .as_ref()
        .is_some_and(|r| r.state == ConnectionState::Empty);
    let published = record
        .as_ref()
        .is_some_and(|r| r.state == ConnectionState::Published);
    let surface_ok = connection.is_some_and(|c| case.expected.surfaces.contains(&c.surface));
    let feed = if let Some(r) = &record {
        runtime
            .feed
            .get_item(&scope.principal, &scope.workspace, &r.feed_id)
            .await?
    } else {
        None
    };
    let pending = requests
        .list_pending_for_scope(&scope.principal, &scope.workspace)
        .await;
    let phrasing = runtime
        .store
        .get_phrasing(&scope.principal, &scope.workspace, &case.id)
        .await?;
    let delivery_ok = match connection.map(|c| c.surface) {
        Some(ConnectionSurface::ForYou) => feed.is_some(),
        Some(ConnectionSurface::Hitl) => !pending.is_empty(),
        Some(ConnectionSurface::WorthALook) => phrasing.is_some(),
        None => feed.is_none() && pending.is_empty() && phrasing.is_none(),
    };
    let no_write = before == memory.load_user_knowledge().await?;
    let cross_scope_clear = case
        .memories
        .iter()
        .filter(|m| m.foreign)
        .all(|m| !sources.iter().any(|s| s.text.contains(&m.text)));
    let behavioural_match = if case.expected.kind == "silent" {
        valid_empty
    } else {
        published && surface_ok && recalled && cited
    };
    let passed = provider_ok && behavioural_match && delivery_ok && no_write && cross_scope_clear;
    Ok(
        json!({"id":case.id,"repeat":repeat,"partition":case.partition,"category":case.category,
            "expected":{"kind":case.expected.kind,"surfaces":case.expected.surfaces,"required_memory_keys":case.expected.required_memory_keys,"rationale":case.expected.rationale},
            "activity":{"title":case.title,"text":case.activity},"record":record,
            "observations":observations.iter().map(observation).collect::<Vec<_>>(),
            "checks":{"provider_ok":provider_ok,"retrieval_ok":recalled,"citations_ok":cited,"surface_ok":surface_ok,"delivery_ok":delivery_ok,"no_unrequested_memory_write":no_write,"cross_scope_clear":cross_scope_clear,"behavioural_match":behavioural_match},
            "automated_passed":passed,"usefulness_review":"pending","owner_review":"pending",
            "reviews_reserved":reviewed,"total_latency_ms":started.elapsed().as_millis(),"fixture_root":root,
            "delivery":{"feed":feed,"requests":pending,"phrasing":phrasing},
        }),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    magician_chunking::register_builtin_chunk_adapters()?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(args.len() % 2 == 0, "arguments must be --name value pairs");
    let opts: HashMap<_, _> = args
        .chunks_exact(2)
        .map(|p| (p[0].as_str(), p[1].as_str()))
        .collect();
    let required = |key| {
        opts.get(key)
            .copied()
            .with_context(|| format!("missing {key}"))
    };
    let suite = PathBuf::from(required("--suite")?);
    let output = PathBuf::from(required("--output-dir")?);
    ensure!(
        !output.exists(),
        "use a fresh output directory; previous evidence is immutable"
    );
    let repeats: usize = opts.get("--repeats").unwrap_or(&"1").parse()?;
    let max_calls: usize = opts.get("--max-calls").unwrap_or(&"12").parse()?;
    let max_tokens: u64 = opts
        .get("--max-reported-tokens")
        .unwrap_or(&"100000")
        .parse()?;
    ensure!(
        (1..=3).contains(&repeats) && max_calls > 0 && max_calls <= 180,
        "invalid bounds"
    );
    let bytes = fs::read(&suite)?;
    let all: Vec<Case> = serde_json::from_slice(&bytes)?;
    let partition = opts.get("--partition").copied().unwrap_or("smoke");
    let cases: Vec<_> = all
        .into_iter()
        .filter(|c| {
            partition == "all" || (partition == "smoke" && c.smoke) || c.partition == partition
        })
        .collect();
    ensure!(
        !cases.is_empty() && cases.len() * repeats <= max_calls,
        "case selection exceeds call ceiling"
    );
    let config_path = PathBuf::from(required("--config")?);
    let config_root = config_path
        .parent()
        .context("config needs a parent directory")?;
    for name in [".env.development", ".env"] {
        let env_path = config_root.join(name);
        if env_path.exists() {
            // Never print dotenv parser errors: they can include secret-bearing lines.
            ensure!(
                dotenvy::from_path(&env_path).is_ok(),
                "could not parse runtime environment file {}",
                env_path.display()
            );
        }
    }
    magician::magician_v2::query_analysis::operation_llm_router::install_llm_routing_overrides(
        config_path
            .parent()
            .context("config needs a parent directory")?,
    );
    let config = load_magician_config_from_path(&config_path)?;
    let router = Arc::new(OperationLlmRouter::new(Some(
        config
            .router_config()
            .cloned()
            .context("missing router config")?,
    )));
    if router.shared_configured_router().is_none() {
        let diagnostics = load_magician_config_from_path(&config_path)?;
        magician::magician_v2::llm_chunking::validate_router_chunking_config(
            diagnostics.router_config().context("missing router")?,
        )?;
        magicllm::ConfiguredRouter::from_router_config(
            diagnostics
                .router_config()
                .cloned()
                .context("missing router")?,
        )?;
        anyhow::bail!("operation router initialization failed validation");
    }
    let binding = router
        .explicit_binding_for_operation(OPERATION)
        .context("memory connection operation is unbound")?;
    fs::create_dir_all(output.join("fixtures"))?;
    let mut rows = Vec::new();
    let mut tokens = 0u64;
    let mut stop_reason: Option<String> = None;
    for repeat in 0..repeats {
        for case in &cases {
            if stop_reason.is_some() {
                break;
            }
            if tokens >= max_tokens {
                stop_reason = Some("reported token ceiling reached".into());
                break;
            }
            let result = evaluate(
                case,
                repeat,
                router.clone(),
                output
                    .join("fixtures")
                    .join(format!("{}-{repeat}", case.id)),
            )
            .await;
            let row = match result {
                Ok(row) => row,
                Err(e) => {
                    json!({"id":case.id,"repeat":repeat,"partition":case.partition,"automated_passed":false,"error":format!("{e:#}")})
                },
            };
            let usage = row
                .pointer("/observations/0/usage/total_tokens")
                .and_then(Value::as_u64);
            tokens += usage.unwrap_or(0);
            if row.pointer("/checks/provider_ok") != Some(&json!(true)) {
                stop_reason =
                    Some("provider or production-path failure; remaining cases unrun".into());
            } else if usage.is_none() {
                stop_reason = Some("provider usage unavailable; stopping further spend".into());
            }
            println!(
                "{} repeat={} automated_passed={} tokens={tokens}",
                case.id, repeat, row["automated_passed"]
            );
            rows.push(row);
            let report = json!({"schema_version":1,"kind":"real_model_production_worker","operation":OPERATION,
                "profile":binding.0,"partition":partition,"suite_sha256":null,"suite_blake3":blake3::hash(&bytes).to_hex().to_string(),
                "system_blake3":blake3::hash(SYSTEM.as_bytes()).to_hex().to_string(),"expected_cases":cases.len()*repeats,
                "max_calls":max_calls,"reported_tokens":tokens,"token_ceiling_checked_between_calls":max_tokens,
                "stop_reason":stop_reason,"cases":rows,"owner_review":"pending"});
            fs::write(
                output.join("report.json"),
                serde_json::to_vec_pretty(&report)?,
            )?;
        }
    }
    ensure!(
        stop_reason.is_none(),
        "evaluation inconclusive: {}",
        stop_reason.unwrap_or_default()
    );
    ensure!(
        rows.iter().all(|r| r["automated_passed"] == true),
        "one or more scenario expectations failed; see report.json"
    );
    Ok(())
}
