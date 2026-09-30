//! Real-provider connection publication and stale-source withdrawal, isolated on SSD.
use magician::{
    config::{load_magician_config_from_path, DecisionHostConfig, DecisionMode},
    magician_v2::{
        agents::AgentMemoryResolver,
        artifact_v2::workspace::ArtifactV2Workspace,
        attention::resurfacing::{
            memory_connections::{ConnectionState, ConnectionSurface},
            store::ResurfacingStore,
            types::{Candidate, CandidateState, SourceKind},
        },
        attention_funnel::AttentionScope,
        attention_funnel_store::AttentionFunnelStore,
        decision_host,
        feed::FeedStore,
        query_analysis::operation_llm_router::OperationLlmRouter,
        realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
        user_requests::UserRequestService,
    },
};
use magician_comms::channel_assist::resurfacing::{
    interaction::{MemoryInteractionAdapter, ResurfacingInteractionRegistry},
    memory_connections::ConnectionRuntime,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[tokio::test]
#[ignore = "real Jev/LLM calls, isolated engine and external output required"]
async fn memory_connection_publication_live() -> anyhow::Result<()> {
    magician_chunking::register_builtin_chunk_adapters()?;
    let config_path = PathBuf::from(std::env::var("MAGICIAN_MEMORY_EVAL_CONFIG")?);
    let output = PathBuf::from(std::env::var("MAGICIAN_MEMORY_EVAL_OUTPUT")?);
    anyhow::ensure!(!output.exists(), "fresh output directory required");
    let socket = std::env::var("DECISION_ENGINE_SOCKET")?;
    anyhow::ensure!(
        socket.starts_with("/tmp/memory-decision-"),
        "isolated test socket required"
    );
    let root = config_path.parent().unwrap();
    for file in [".env.development", ".env"] {
        let _ = dotenvy::from_path(root.join(file));
    }
    magician::magician_v2::query_analysis::operation_llm_router::install_llm_routing_overrides(
        root,
    );
    let config = load_magician_config_from_path(&config_path)?;
    let principal = "connection-publication-eval";
    let workspace = uuid::Uuid::new_v4().to_string();
    let scope = AttentionScope {
        principal: principal.into(),
        workspace: workspace.clone(),
    };
    let ledger_workspace = ArtifactV2Workspace::new(output.join("runtime"));
    let resolver = AgentMemoryResolver::with_workspace_layout(ledger_workspace.clone());
    let memory = resolver.resolve_for_scope(principal, &workspace)?;
    let text="Registration for the climate founders networking dinner closes tomorrow. Five climate startup founders will attend. The owner has not registered.";
    let mut knowledge = json!({"user_preferences":[
        {"key":"goal","value":"The owner wants to meet climate startup founders this month to find collaborators for the new research project.","source_type":"explicit_user_statement","confidence":1.0},
        {"key":"event","value":text,"source_type":"explicit_user_statement","confidence":1.0}
    ]});
    memory.persist_user_knowledge(&knowledge).await?;
    let bus = Arc::new(RuntimeTransportBroadcaster::new(4096));
    let mut rx = bus.subscribe();
    decision_host::set_health_broadcaster(&bus);
    let router = OperationLlmRouter::new(config.router_config().cloned())
        .with_scope_context(Some(magicllm::LlmScope::new(principal, &workspace)));
    router.set_event_broadcaster(bus.clone());
    let activation =
        magician::magician_v2::analytics::llm_trace_activation::LlmTraceActivation::start(
            bus.clone(),
            ledger_workspace.clone(),
            Default::default(),
        )?;
    decision_host::configure(&DecisionHostConfig {
        mode: DecisionMode::AllEngines,
        socket: Some(socket),
        ..Default::default()
    });
    let policy = decision_host::decision_backend_for("magician")
        .unwrap()
        .operations()
        .await?;
    anyhow::ensure!(
        policy
            .operations
            .iter()
            .any(|p| p.name == "memory_connection_gate" && p.shadow && !p.gate),
        "shadow-only connection binding required"
    );
    let requests = Arc::new(
        UserRequestService::new(bus.clone())
            .with_workspace_layout(ledger_workspace.clone())
            .with_history_persist_path(output.join("request-history.json"))
            .with_pending_persist_path(output.join("request-pending.json"))
            .await,
    );
    let runtime = ConnectionRuntime {
        store: ResurfacingStore::open(&output.join("runtime"))?,
        resolver: resolver.clone(),
        interactions: ResurfacingInteractionRegistry::from_adapters(vec![Arc::new(
            MemoryInteractionAdapter::new(resolver),
        )]),
        feed: FeedStore::open_workspace(ledger_workspace.clone())?,
        requests: Some(requests.clone()),
        router: Some(Arc::new(router)),
        attention: Some(AttentionFunnelStore::open(&output.join("runtime"))?),
        taste: None,
    };
    let now = chrono::Utc::now().timestamp();
    let candidate = Candidate {
        candidate_id: "climate-event".into(),
        source_kind: SourceKind::Memory,
        source_ref: "user_preferences#event".into(),
        title: "Climate founders networking dinner".into(),
        content_digest: text.into(),
        content_revision: None,
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
        .upsert_candidate(principal, &workspace, &candidate)
        .await?;
    anyhow::ensure!(
        decision_host::prime_classification_for_eval(
            "memory_connection_gate",
            principal,
            &workspace
        )
        .await,
        "policy unavailable"
    );
    // Keep policy warm while optional semantic recall initializes. This tests
    // steady-state publication; separate cases cover cold-policy fallback.
    let warm_workspace = workspace.clone();
    let warmer = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = decision_host::prime_classification_for_eval(
                "memory_connection_gate",
                principal,
                &warm_workspace,
            )
            .await;
        }
    });
    let started = Instant::now();
    let outcome:anyhow::Result<Value>=async {
        let mut cursor=None;
        let reviewed=runtime.pass(&scope,&mut cursor,now).await?;
        let published=runtime.store.get_connection(principal,&workspace,&candidate.candidate_id).await?.ok_or_else(||anyhow::anyhow!("connection not stored"))?;
        anyhow::ensure!(reviewed==1 && published.state==ConnectionState::Published,"connection not published: {:?}",published.state);
        let surface=published.connection.as_ref().ok_or_else(||anyhow::anyhow!("connection absent"))?.surface;
        match surface {
            ConnectionSurface::ForYou=>anyhow::ensure!(runtime.feed.get_item(principal,&workspace,&published.feed_id).await?.is_some(),"feed item missing"),
            ConnectionSurface::WorthALook=>anyhow::ensure!(runtime.store.get_phrasing(principal,&workspace,&candidate.candidate_id).await?.is_some(),"phrasing missing"),
            ConnectionSurface::Hitl=>anyhow::ensure!(requests.pending_request_snapshot(&published.request_id,Some(principal),Some(&workspace)).await.is_some(),"HITL request missing"),
        }
        let publication_ms=started.elapsed().as_millis();
        knowledge["user_preferences"][1]["value"]=json!("The climate dinner was cancelled. Registration is closed and the event will not take place.");
        memory.persist_user_knowledge(&knowledge).await?;
        let additional=runtime.pass(&scope,&mut cursor,now+1).await?;
        let withdrawn=runtime.store.get_connection(principal,&workspace,&candidate.candidate_id).await?.ok_or_else(||anyhow::anyhow!("connection audit missing"))?;
        anyhow::ensure!(additional==0 && withdrawn.state==ConnectionState::Withdrawn,"stale source did not withdraw without another review");
        anyhow::ensure!(runtime.feed.get_item(principal,&workspace,&published.feed_id).await?.is_none(),"stale feed item retained");
        anyhow::ensure!(requests.pending_request_snapshot(&published.request_id,Some(principal),Some(&workspace)).await.is_none(),"stale HITL retained");
        Ok(json!({"published":true,"surface":surface,"publication_ms":publication_ms,"withdrawn":true,"review_calls":reviewed,"additional_calls_on_withdrawal":additional}))
    }.await;
    warmer.abort();
    let _ = warmer.await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    let mut records = Vec::new();
    let mut calls = BTreeMap::new();
    let mut duplicate_events = 0;
    let mut gaps = 0;
    while let Ok(event) = rx.try_recv() {
        match event {
            RuntimeTransportEvent::DecisionShadowAgreement {
                principal: p,
                workspace: w,
                record,
                ..
            } if p == principal && w == workspace => {
                records.push(json!({"principal":p,"workspace":w,"record":record}))
            },
            RuntimeTransportEvent::DecisionAccountingGap {
                principal: p,
                workspace: w,
                ..
            } if p == principal && w == workspace => gaps += 1,
            RuntimeTransportEvent::LLMResponseReceived {
                principal: Some(p),
                workspace: Some(w),
                correlation: Some(c),
                provider,
                model,
                operation,
                latency_ms,
                cost,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                success,
                timestamp,
                ..
            } if p == principal && w == workspace => {
                let id = c.llm_call_id.clone();
                duplicate_events += usize::from(calls.contains_key(&id));
                let usage = c.usage_availability;
                calls.insert(id.clone(),json!({"llm_call_id":id,"principal":p,"workspace":w,"provider":provider,"model":model,"operation":operation,
                    "provider_attempt_count":c.provider_attempt_count,"response_reused":c.response_reused,
                    "latency_ms":latency_ms,"completed_at_ms":timestamp,"success":success,
                    "cost_usd":usage.filter(|a|a.cost).map(|_|cost),"input_tokens":usage.filter(|a|a.tokens).map(|_|input_tokens),
                    "output_tokens":usage.filter(|a|a.tokens).map(|_|output_tokens),"cache_read_tokens":usage.filter(|a|a.cache_read).map(|_|cache_read_tokens),
                    "cache_creation_tokens":usage.filter(|a|a.cache_write).map(|_|cache_creation_tokens)}));
            },
            _ => {},
        }
    }
    let physical: BTreeSet<String> = records
        .iter()
        .flat_map(|r| r["record"]["calls"].as_array().into_iter().flatten())
        .filter_map(|c| c["call_id"].as_str().map(str::to_owned))
        .collect();
    let event_receipt_match = physical.iter().all(|id| calls.contains_key(id));
    let shutdown = activation.shutdown().await;
    let reader =
        magician::magician_v2::analytics::llm_analytics_read_service::LlmAnalyticsReadService::new(
            ledger_workspace,
        );
    let scope = magicllm::LlmScope::new(principal, &workspace);
    let read = |relation| -> anyhow::Result<Vec<serde_json::Map<String, Value>>> {
        let mut query =
            magician::magician_v2::analytics::llm_analytics_read_service::LlmFactQuery::for_relation(
                relation,
            );
        query.limit = Some(1000);
        query.order_by = None;
        let page = reader.query_facts(&scope, query)?;
        anyhow::ensure!(
            page.total <= 1000,
            "eval ledger export exceeded bounded page"
        );
        let allow = [
            "principal",
            "workspace",
            "llm_call_id",
            "task_id",
            "execution_id",
            "response_kind",
            "provider_attempt_id",
            "provider_attempt_index",
            "provider_attempt_count",
            "response_reused",
            "provider",
            "model",
            "operation",
            "success",
            "transport_success",
            "completed_at_ms",
            "latency_ms",
            "cost_usd",
            "pricing_version",
            "cost_source",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
        ];
        Ok(page
            .rows
            .into_iter()
            .map(|mut row| {
                row.retain(|k, _| allow.contains(&k.as_str()));
                row
            })
            .collect())
    };
    let ledger_calls =
        read(magician::magician_v2::analytics::llm_fact_registry::LlmFactRelation::Calls)?;
    let attempts = read(
        magician::magician_v2::analytics::llm_fact_registry::LlmFactRelation::ProviderAttempts,
    )?;
    let reference_ids: BTreeSet<String> = records
        .iter()
        .filter_map(|r| {
            r["record"]["reference"]["call"]["receipt"]["context"]["llm_call_id"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    let receipt_match = event_receipt_match
        && duplicate_events == 0
        && reference_ids.iter().all(|id| calls.contains_key(id))
        && calls.keys().all(|id| {
            ledger_calls
                .iter()
                .filter(|row| row.get("llm_call_id").and_then(Value::as_str) == Some(id))
                .count()
                == 1
        });

    std::fs::create_dir_all(&output)?;
    let passed = outcome.is_ok();
    let result = json!({"passed":passed,"result":outcome.as_ref().ok(),"error":outcome.err().map(|e|e.to_string())});
    for (name, value) in [
        ("results.json", result.clone()),
        ("comparisons.json", json!(records)),
        ("receipts.json", json!(calls.values().collect::<Vec<_>>())),
        (
            "ledger.json",
            json!({"calls":ledger_calls,"attempts":attempts}),
        ),
        ("capture-shutdown.json", json!(shutdown)),
    ] {
        std::fs::write(output.join(name), serde_json::to_vec_pretty(&value)?)?;
    }
    anyhow::ensure!(passed, "publication/withdrawal failed: {result}");
    anyhow::ensure!(
        gaps == 0
            && receipt_match
            && serde_json::to_value(&shutdown)?["activation"]["gap_records_emitted"] == 0,
        "receipt reconciliation failed"
    );
    anyhow::ensure!(
        records
            .iter()
            .any(|r| r["record"]["operation"] == "memory_connection_gate"
                && r["record"]["calls"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())),
        "no real Jev review recorded"
    );
    Ok(())
}
