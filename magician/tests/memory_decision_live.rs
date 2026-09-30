//! Opt-in real-provider replay through production applicability/attachment paths.
//! Outputs contain opaque cases, decisions, receipt metadata and costs; no bodies.
use magician::config::{
    load_magician_config_from_path, DecisionHostConfig, DecisionMode, MagicianConfig,
};
use magician::magician_v2::{
    agents::AgentMemoryService,
    attention::resurfacing::{
        memory_context::{memories_from_knowledge, MemoryApplication, ScopedMemory},
        memory_effects::MemoryJudgement,
        memory_stage2::{refine_judgement_with_llm, Stage2Budget},
        store::ResurfacingStore,
        types::{Candidate, CandidateState, SourceKind},
    },
    decision_host, memory_applicability as applicability,
    query_analysis::operation_llm_router::OperationLlmRouter,
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Fixture {
    id: String,
    goal: String,
    relevant: String,
    irrelevant: String,
}

async fn prime(operation: &str, principal: &str, workspace: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        decision_host::prime_classification_for_eval(operation, principal, workspace).await,
        "memory engine policy unavailable for {operation}"
    );
    Ok(())
}

fn install_eval_dispatch(
    router: &OperationLlmRouter,
    config: &MagicianConfig,
) -> anyhow::Result<()> {
    let configured = router
        .shared_configured_router()
        .ok_or_else(|| anyhow::anyhow!("incumbent router did not initialize"))?;
    let queue = magicllm::LlmDispatchQueue::start(
        configured,
        Arc::new(magicllm::dispatch::NoopTaskStateView),
        Arc::new(magicllm::dispatch::NoopTaskLedgerSink),
        config.llm.dispatch.clone(),
    );
    router.set_dispatch_queue(queue);
    Ok(())
}

#[tokio::test]
#[ignore = "real Jev/incumbent calls; requires explicit config, socket and external output directory"]
async fn memory_decision_live_replay() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();
    magician_chunking::register_builtin_chunk_adapters()?;
    let config_path = PathBuf::from(std::env::var("MAGICIAN_MEMORY_EVAL_CONFIG")?);
    let output = PathBuf::from(std::env::var("MAGICIAN_MEMORY_EVAL_OUTPUT")?);
    anyhow::ensure!(!output.exists(), "use a fresh eval output directory");
    let socket = std::env::var("DECISION_ENGINE_SOCKET")?;
    let mode = std::env::var("MAGICIAN_MEMORY_EVAL_MODE").unwrap_or_else(|_| "shadow".into());
    anyhow::ensure!(
        matches!(mode.as_str(), "shadow" | "fixture_gate" | "operator_gate"),
        "invalid evaluation mode"
    );
    if mode != "shadow" {
        anyhow::ensure!(
            socket.starts_with("/tmp/memory-decision-"),
            "fixture gate requires an isolated test socket"
        );
    }
    let root = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config root"))?;
    for file in [".env.development", ".env"] {
        let _ = dotenvy::from_path(root.join(file));
    }
    magician::magician_v2::query_analysis::operation_llm_router::install_llm_routing_overrides(
        root,
    );
    let config = load_magician_config_from_path(&config_path)?;
    let router = OperationLlmRouter::new(config.router_config().cloned());
    install_eval_dispatch(&router, &config)?;
    anyhow::ensure!(
        router.shared_configured_router().is_some(),
        "incumbent router did not initialize"
    );
    for op in ["memory_applicability_judge", "memory_attach_stage2"] {
        anyhow::ensure!(
            router.explicit_binding_for_operation(op).is_some(),
            "unbound incumbent {op}"
        );
    }
    let principal = "memory-decision-eval";
    let workspace = ulid::Ulid::new().to_string();
    let router = router.with_scope_context(Some(magicllm::LlmScope::new(principal, &workspace)));
    let bus = Arc::new(RuntimeTransportBroadcaster::new(4096));
    router.set_event_broadcaster(bus.clone());
    decision_host::set_health_broadcaster(&bus);
    let mut rx = bus.subscribe();
    let ledger_workspace = magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
        output.join("ledger"),
    );
    let activation =
        magician::magician_v2::analytics::llm_trace_activation::LlmTraceActivation::start(
            bus.clone(),
            ledger_workspace.clone(),
            Default::default(),
        )?;
    let host = DecisionHostConfig {
        mode: DecisionMode::AllEngines,
        socket: Some(socket),
        ..Default::default()
    };
    decision_host::configure(&host);
    let policy = decision_host::decision_backend_for("magician")
        .unwrap()
        .operations()
        .await?;
    anyhow::ensure!(
        policy.contract_version == decision_engine_contract::CONTRACT_VERSION,
        "coordinated v5 deployment required"
    );
    for op in ["memory_applicability", "memory_attach"] {
        let binding = policy
            .operations
            .iter()
            .find(|p| p.name == op)
            .ok_or_else(|| anyhow::anyhow!("missing {op}"))?;
        if mode == "shadow" {
            anyhow::ensure!(
                binding.shadow && !binding.gate,
                "live smoke requires shadow-only memory binding {op}"
            );
        } else if mode == "operator_gate" {
            anyhow::ensure!(
                binding.gate && !binding.shadow && binding.classification.allow_unqualified_gate,
                "operator gate missing for {op}"
            );
        } else {
            anyhow::ensure!(
                binding.gate
                    && !binding.shadow
                    && !binding.classification.qualifications.is_empty()
                    && binding
                        .classification
                        .qualifications
                        .iter()
                        .all(|q| q.human_review_ref.starts_with("unreviewed-synthetic-test:")),
                "fixture gate requires explicit unreviewed synthetic-test qualifications for {op}"
            );
        }
    }
    let suite: Value = serde_json::from_str(include_str!(
        "../../data/magician_v2/evals/memory_decisions/smoke-v2.json"
    ))?;
    let fixtures: Vec<Fixture> = serde_json::from_value(suite["cases"].clone())?;
    anyhow::ensure!(!fixtures.is_empty(), "memory replay fixture set is empty");
    let case_limit = std::env::var("MAGICIAN_MEMORY_EVAL_CASE_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(fixtures.len())
        .clamp(1, fixtures.len());
    let fixtures = fixtures.into_iter().take(case_limit).collect::<Vec<_>>();
    let minimum_success_rate = suite["minimum_success_rate_per_operation"]
        .as_f64()
        .ok_or_else(|| {
            anyhow::anyhow!("fixture set needs an explicit semantic acceptance threshold")
        })?;
    anyhow::ensure!(
        (0.9..=1.0).contains(&minimum_success_rate),
        "semantic threshold is below the plan's 90% floor"
    );
    let mut results = Vec::new();
    let mut verified_cache_hits = 0;
    let attach_store = ResurfacingStore::open(&output.join("resurfacing"))?;
    let attach_memory = AgentMemoryService::with_scoped_memory_scope_in_workspace(
        ledger_workspace.clone(),
        principal,
        &workspace,
    );
    let entries = fixtures
        .iter()
        .flat_map(|fixture| {
            [
                (
                    format!("{}:irrelevant", fixture.id),
                    fixture.irrelevant.clone(),
                ),
                (format!("{}:relevant", fixture.id), fixture.relevant.clone()),
            ]
        })
        .map(|(key, value)| {
            json!({
                "key":key,"value":value,"source_type":"user_stated",
                "scope":{"topics":["memory-decision-eval"]}
            })
        })
        .collect::<Vec<_>>();
    attach_memory
        .save_user_knowledge(&json!({"preferences":entries}))
        .await?;
    let stored_memories = memories_from_knowledge(&attach_memory.load_user_knowledge().await?);
    for fixture in &fixtures {
        let router = router.with_task_context(Some(
            magicllm::dispatch::TaskRef::task(fixture.id.clone()).with_scope(principal, &workspace),
        ));
        prime("memory_applicability", principal, &workspace).await?;
        let offered = applicability::narrow(
            vec![
                applicability::Candidate {
                    item_key: format!("{}:irrelevant", fixture.id),
                    text: fixture.irrelevant.clone(),
                },
                applicability::Candidate {
                    item_key: format!("{}:relevant", fixture.id),
                    text: fixture.relevant.clone(),
                },
            ],
            12,
        );
        let memories = offered
            .iter()
            .map(|n| {
                let key = format!("preferences: {}", n.candidate.item_key);
                stored_memories
                    .iter()
                    .find(|memory| memory.key == key)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing persisted memory {key}"))
            })
            .collect::<anyhow::Result<Vec<ScopedMemory>>>()?;
        let source_memories = memories.clone();
        let source_service = attach_memory.clone();
        let source_principal = principal.to_owned();
        let source_workspace = workspace.clone();
        let source_check_factory = move || -> applicability::ApplicabilitySourceCheck {
            let service = source_service.clone();
            let expected = source_memories.clone();
            let principal = source_principal.clone();
            let workspace = source_workspace.clone();
            Arc::new(move || {
                let service = service.clone();
                let expected = expected.clone();
                let principal = principal.clone();
                let workspace = workspace.clone();
                Box::pin(async move {
                    if service.scoped_memory_scope()
                        != Some((principal.as_str(), workspace.as_str()))
                    {
                        return false;
                    }
                    let Ok(knowledge) = service.load_user_knowledge().await else {
                        return false;
                    };
                    let current = memories_from_knowledge(&knowledge);
                    expected.iter().all(|source| current.contains(source))
                })
            })
        };
        let started = Instant::now();
        let ranked = applicability::judge_or_fall_back_with_source_check(
            offered.clone(),
            &fixture.goal,
            Some(&router),
            principal,
            &workspace,
            Some(&source_check_factory),
        )
        .await;
        let elapsed = started.elapsed().as_millis();
        let preserved = ranked.len() == offered.len()
            && ranked
                .iter()
                .map(|v| &v.candidate.item_key)
                .collect::<BTreeSet<_>>()
                == offered.iter().map(|v| &v.candidate.item_key).collect();
        let expected = ranked
            .first()
            .is_some_and(|r| r.candidate.item_key.ends_with(":relevant"));
        results.push(json!({"case_id":fixture.id,"operation":"memory_applicability","foreground_ms":elapsed,"expected_outcome_passed":expected,"mutation_invariant":preserved}));
        // Warm application-cache parity: identical reads cannot change order.
        let mut cache_events = bus.subscribe();
        let cached = applicability::judge_or_fall_back_with_source_check(
            offered.clone(),
            &fixture.goal,
            Some(&router),
            principal,
            &workspace,
            Some(&source_check_factory),
        )
        .await;
        // A failed first call or expired policy is a legitimate cache miss and
        // may recover on the second call. Assert parity only for actual hits.
        while let Ok(event) = cache_events.try_recv() {
            if let RuntimeTransportEvent::DecisionShadowAgreement { record, .. } = event {
                if record["stage"] == "invocation" && record["status"] == "application_cache_hit" {
                    verified_cache_hits += 1;
                    anyhow::ensure!(ranked == cached, "warm verdict cache changed result");
                }
            }
        }
        prime("memory_attach", principal, &workspace).await?;
        let candidate = Candidate {
            candidate_id: fixture.id.clone(),
            source_kind: SourceKind::Task,
            source_ref: fixture.id.clone(),
            title: fixture.goal.clone(),
            content_digest: fixture.goal.clone(),
            content_details: None,
            content_revision: Some(format!("{}-v1", fixture.id)),
            semantic_features: None,
            salience_score: 0.0,
            signals: Default::default(),
            temporal_anchor_at: None,
            embedding_id: None,
            state: CandidateState::Candidate,
            first_seen_at: 0,
            last_scored_at: 0,
            last_surfaced_at: None,
            cooldown_until: 0,
            surface_count: 0,
            dismiss_count: 0,
        };
        attach_store
            .upsert_candidate(principal, &workspace, &candidate)
            .await?;
        let mut judgement = MemoryJudgement::default();
        judgement.applications.would_apply = memories
            .iter()
            .map(|m| MemoryApplication {
                memory_key: m.key.clone(),
                memory_revision: None,
                direction: "explain".into(),
                rationale: "fixture stage-one scope match".into(),
                strength: None,
            })
            .collect();
        let mut budget = Stage2Budget::per_pass();
        let started = Instant::now();
        refine_judgement_with_llm(
            Some(&router),
            Some(&attach_memory),
            &attach_store,
            principal,
            &workspace,
            &fixture.id,
            candidate.content_revision.as_deref(),
            &fixture.goal,
            &fixture.goal,
            "task",
            &memories,
            &mut judgement,
            &mut budget,
        )
        .await;
        let keys = judgement
            .applications
            .would_apply
            .iter()
            .map(|a| a.memory_key.clone())
            .collect::<BTreeSet<_>>();
        results.push(json!({"case_id":fixture.id,"operation":"memory_attach","foreground_ms":started.elapsed().as_millis(),
            "expected_outcome_passed":keys.len()==1&&keys.iter().all(|k|k.ends_with(":relevant")),
            "mutation_invariant":keys.iter().all(|k|memories.iter().any(|m|&m.key==k))&&budget.remaining()==7}));
    }
    // Complete queued reference work before the rollback probe changes the
    // host policy. Its latency must not be charged to any foreground call.
    let settle_seconds = std::env::var("MAGICIAN_MEMORY_EVAL_WAIT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(4)
        .min(45);
    tokio::time::sleep(Duration::from_secs(settle_seconds)).await;
    let mut live_invariants = json!({"rollback":false,"cache_invalidation":false});
    if mode != "shadow" {
        let fixture = &fixtures[0];
        let probe = router.with_task_context(Some(
            magicllm::dispatch::TaskRef::task("__rollback_probe__")
                .with_scope(principal, &workspace),
        ));
        let offered = applicability::narrow(
            vec![
                applicability::Candidate {
                    item_key: format!("{}:irrelevant", fixture.id),
                    text: fixture.irrelevant.clone(),
                },
                applicability::Candidate {
                    item_key: format!("{}:relevant", fixture.id),
                    text: fixture.relevant.clone(),
                },
            ],
            12,
        );
        prime("memory_applicability", principal, &workspace).await?;
        let _ = applicability::judge_or_fall_back(
            offered.clone(),
            &fixture.goal,
            Some(&probe),
            principal,
            &workspace,
        )
        .await;
        let mut warm_events = bus.subscribe();
        let _ = applicability::judge_or_fall_back(
            offered.clone(),
            &fixture.goal,
            Some(&probe),
            principal,
            &workspace,
        )
        .await;
        let mut warm_hit = false;
        while let Ok(event) = warm_events.try_recv() {
            if let RuntimeTransportEvent::DecisionShadowAgreement { record, .. } = event {
                warm_hit |=
                    record["stage"] == "invocation" && record["status"] == "application_cache_hit";
            }
        }
        decision_host::configure(&DecisionHostConfig {
            mode: DecisionMode::Off,
            ..host.clone()
        });
        let mut off_events = bus.subscribe();
        let result = applicability::judge_or_fall_back(
            offered.clone(),
            &fixture.goal,
            Some(&probe),
            principal,
            &workspace,
        )
        .await;
        let (mut incumbent_calls, mut decision_calls) = (0, 0);
        let mut rollback_events = Vec::new();
        while let Ok(event) = off_events.try_recv() {
            if let RuntimeTransportEvent::LLMResponseReceived {
                correlation: Some(c),
                provider,
                success,
                error,
                ..
            } = event
            {
                if c.task_id.as_deref() == Some("__rollback_probe__") {
                    rollback_events.push(json!({"provider":provider.clone(),"success":success,"error":error}));
                    if provider.starts_with("decision:") {
                        decision_calls += 1;
                    } else {
                        incumbent_calls += 1;
                    }
                }
            }
        }
        anyhow::ensure!(
            result.len() == offered.len() && incumbent_calls > 0 && decision_calls == 0,
            "Off must invalidate the cached verdict and dispatch only the incumbent: result={} offered={} incumbent_calls={} decision_calls={} events={rollback_events:?}",
            result.len(), offered.len(), incumbent_calls, decision_calls
        );
        live_invariants = json!({"rollback":true,"cache_invalidation":warm_hit});
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
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
    let qualified_items: BTreeMap<String, usize> = ["memory_applicability", "memory_attach"]
        .into_iter()
        .map(|operation| {
            let count = records
                .iter()
                .filter(|r| r["record"]["operation"] == operation)
                .flat_map(|r| r["record"]["items"].as_array().into_iter().flatten())
                .filter(|i| {
                    i["eligible_answers"]
                        .as_object()
                        .is_some_and(|q| !q.is_empty())
                })
                .count();
            (operation.into(), count)
        })
        .collect();
    let ledger = json!({"calls":ledger_calls,"attempts":attempts});
    std::fs::create_dir_all(&output)?;
    for (name, value) in [
        ("ledger.json", ledger),
        ("capture-shutdown.json", json!(shutdown)),
        ("comparisons.json", json!(records)),
        ("receipts.json", json!(calls.values().collect::<Vec<_>>())),
        (
            "results.json",
            json!({"scope":{"principal":principal,"workspace":workspace},"dataset_id":suite["dataset_id"],"qualification":"INCOMPLETE","mode":mode,"provider_path":"direct_profile_replay","live_invariants":live_invariants,"semantic_minimum_success_rate":minimum_success_rate,"qualified_items":qualified_items,"cases":results,"accounting_gaps":gaps,"receipts_reconciled":receipt_match,"duplicate_events":duplicate_events,"reference_calls":reference_ids.len(),"verified_cache_hits":verified_cache_hits,"decision_calls":physical.len()}),
        ),
    ] {
        std::fs::write(output.join(name), serde_json::to_vec_pretty(&value)?)?;
    }
    decision_host::configure(&DecisionHostConfig {
        mode: DecisionMode::Off,
        ..host
    });
    anyhow::ensure!(
        !shutdown.durable_pipeline.timed_out && shutdown.durable_pipeline.flush_error.is_none(),
        "ledger drain failed"
    );
    anyhow::ensure!(
        gaps == 0 && receipt_match && !physical.is_empty(),
        "real decision receipts incomplete; inspect artifacts"
    );
    if mode == "operator_gate" {
        anyhow::ensure!(
            records
                .iter()
                .all(|r| r["record"]["mode"] != "gate"
                    || r["record"]["reference_attempted"] == false),
            "gated classification attempted an incumbent LLM fallback"
        );
    }
    anyhow::ensure!(
        mode == "shadow" || qualified_items.values().all(|count| *count > 0),
        "fixture gate did not exercise qualified decisions in both consumers"
    );
    anyhow::ensure!(
        results.iter().all(|r| r["mutation_invariant"] == true),
        "memory mutation invariant failed"
    );
    anyhow::ensure!(
        verified_cache_hits > 0,
        "live replay did not exercise a warm cache hit"
    );
    for operation in ["memory_applicability", "memory_attach"] {
        let cases = results
            .iter()
            .filter(|r| r["operation"] == operation)
            .collect::<Vec<_>>();
        let passed = cases
            .iter()
            .filter(|r| r["expected_outcome_passed"] == true)
            .count();
        let failed = cases
            .iter()
            .filter(|r| r["expected_outcome_passed"] != true)
            .map(|r| r["case_id"].clone())
            .collect::<Vec<_>>();
        println!(
            "{operation}: {passed}/{} semantic outcomes; failed cases {failed:?}",
            cases.len()
        );
        anyhow::ensure!(
            !cases.is_empty() && passed as f64 / cases.len() as f64 >= minimum_success_rate,
            "{operation} is below the declared semantic threshold; inspect results.json"
        );
    }
    println!(
        "memory live replay: {} workflows; {} physical decision calls; artifacts {}",
        results.len(),
        physical.len(),
        output.display()
    );
    Ok(())
}

/// Synthetic-only schema diagnostic; kept separate from scored replay evidence.
#[tokio::test]
#[ignore = "real incumbent call; requires the live evaluation config and a fresh external output"]
async fn memory_decision_incumbent_diagnostic() -> anyhow::Result<()> {
    magician_chunking::register_builtin_chunk_adapters()?;
    let config_path = PathBuf::from(std::env::var("MAGICIAN_MEMORY_EVAL_CONFIG")?);
    let output = PathBuf::from(std::env::var("MAGICIAN_MEMORY_EVAL_OUTPUT")?);
    anyhow::ensure!(!output.exists(), "use a fresh diagnostic output directory");
    let root = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config root"))?;
    for file in [".env.development", ".env"] {
        let _ = dotenvy::from_path(root.join(file));
    }
    magician::magician_v2::query_analysis::operation_llm_router::install_llm_routing_overrides(
        root,
    );
    let config = load_magician_config_from_path(&config_path)?;
    let router = OperationLlmRouter::new(config.router_config().cloned()).with_scope_context(Some(
        magicllm::LlmScope::new("memory-decision-eval", "synthetic-diagnostic"),
    ));
    install_eval_dispatch(&router, &config)?;
    let suite: Value = serde_json::from_str(include_str!(
        "../../data/magician_v2/evals/memory_decisions/smoke-v2.json"
    ))?;
    let fixture: Fixture = serde_json::from_value(
        suite["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == "similar-words")
            .unwrap()
            .clone(),
    )?;
    let candidates = applicability::narrow(
        vec![
            applicability::Candidate {
                item_key: format!("{}:irrelevant", fixture.id),
                text: fixture.irrelevant,
            },
            applicability::Candidate {
                item_key: format!("{}:relevant", fixture.id),
                text: fixture.relevant,
            },
        ],
        12,
    );
    let prompt = magician::magician_v2::prompts::rendered_prompt(
        magician::magician_v2::prompts::constants::names::MEMORY_APPLICABILITY_JUDGE,
        magician::magician_v2::prompts::constants::versions::MEMORY_APPLICABILITY_JUDGE,
        std::collections::HashMap::from([
            ("goal".into(), fixture.goal.clone()),
            (
                "candidates".into(),
                applicability::render_candidates_for_judge(&candidates),
            ),
        ]),
    )
    .await?;
    let op = "memory_applicability_judge";
    let (profile, kind) = router
        .explicit_binding_for_operation(op)
        .ok_or_else(|| anyhow::anyhow!("unbound incumbent"))?;
    let response = router
        .generate_for_operation_with_system_pinned_and_response_format(
            &magician::magician_v2::query_analysis::operation_llm_router::LLMOperation::Other(
                op.into(),
            ),
            Some(&prompt),
            &fixture.goal,
            &profile,
            Some(kind),
            magicllm::LLMResponseFormat::JsonObject,
        )
        .await?;
    let tel = response.telemetry.as_ref();
    std::fs::create_dir_all(&output)?;
    std::fs::write(
        output.join("synthetic-response.json"),
        serde_json::to_vec_pretty(&json!({
            "case_id":fixture.id,"content":response.content,"parsed_verdicts":applicability::parse_verdicts(&response.content,&candidates),
            "model":tel.map(|t|&t.model),"provider":tel.map(|t|&t.provider),
            "input_tokens":tel.filter(|t|t.usage_reported).map(|t|t.input_tokens),
            "output_tokens":tel.filter(|t|t.usage_reported).map(|t|t.output_tokens),
            "cost_usd":tel.and_then(|t|t.usage_availability.filter(|a|a.cost).map(|_|t.cost_usd)),
            "receipt":tel.and_then(|t|t.trace_receipt.as_ref()),"qualification":"diagnostic_only"
        }))?,
    )?;
    println!(
        "synthetic schema diagnostic written to {}",
        output.display()
    );
    Ok(())
}

/// Live owner entry points beyond the two foreground memory selectors. This is
/// synthetic smoke evidence, never a substitute for held-out human qualification.
#[tokio::test]
#[ignore = "real provider owner replay; requires isolated socket and fresh external output"]
async fn memory_decision_owner_live_replay() -> anyhow::Result<()> {
    use magician::magician_v2::{
        agents::{
            self, AgentMemoryService, MemoryPromptSelectedCandidate, MemoryTemperatureTier,
            MemoryTemperatureUtilityReviewInput, SemanticMemoryType,
        },
        artifact_v2::workspace::ArtifactV2Workspace,
        attention::resurfacing::memory_connections::{
            self, ConnectionDecisionPolicy, ConnectionSource,
        },
        evidence::{self as tier_distill},
        prompts::{json_storage::JsonPromptStorage, PromptManager},
    };
    magician_chunking::register_builtin_chunk_adapters()?;
    let config_path = PathBuf::from(std::env::var("MAGICIAN_MEMORY_EVAL_CONFIG")?);
    let output = PathBuf::from(std::env::var("MAGICIAN_MEMORY_EVAL_OUTPUT")?);
    anyhow::ensure!(!output.exists(), "use a fresh output directory");
    let socket = std::env::var("DECISION_ENGINE_SOCKET")?;
    let mode = std::env::var("MAGICIAN_MEMORY_EVAL_MODE").unwrap_or_else(|_| "shadow".into());
    let qualified_conflict_probe =
        std::env::var("MAGICIAN_MEMORY_EVAL_QUALIFIED_CONFLICT").as_deref() == Ok("1");
    anyhow::ensure!(
        matches!(mode.as_str(), "shadow" | "fixture_gate" | "operator_gate"),
        "invalid replay mode"
    );
    anyhow::ensure!(
        socket.starts_with("/tmp/memory-decision-"),
        "isolated socket required"
    );
    let root = config_path.parent().unwrap();
    for file in [".env.development", ".env"] {
        let _ = dotenvy::from_path(root.join(file));
    }
    magician::magician_v2::query_analysis::operation_llm_router::install_llm_routing_overrides(
        root,
    );
    let config = load_magician_config_from_path(&config_path)?;
    let principal = "memory-owner-eval";
    let workspace = ulid::Ulid::new().to_string();
    let router = OperationLlmRouter::new(config.router_config().cloned())
        .with_scope_context(Some(magicllm::LlmScope::new(principal, &workspace)));
    anyhow::ensure!(
        router.processing_locality() == magicllm::ProcessingLocality::Cloud,
        "this replay requires the configured cloud locality arm"
    );
    install_eval_dispatch(&router, &config)?;
    let bus = Arc::new(RuntimeTransportBroadcaster::new(4096));
    router.set_event_broadcaster(bus.clone());
    decision_host::set_health_broadcaster(&bus);
    let mut rx = bus.subscribe();
    let ledger_workspace = ArtifactV2Workspace::new(output.join("runtime"));
    let activation =
        magician::magician_v2::analytics::llm_trace_activation::LlmTraceActivation::start(
            bus.clone(),
            ledger_workspace.clone(),
            Default::default(),
        )?;
    decision_host::configure(&DecisionHostConfig {
        mode: DecisionMode::AllEngines,
        socket: Some(socket.clone()),
        ..Default::default()
    });
    let operations = [
        "memory_utility_review",
        "memory_lifecycle_relation",
        "evidence_promote",
        "memory_connection_gate",
        "procedure_feedback",
        "memory_episode_quality",
        "memory_conflict_review",
    ];
    let selected = std::env::var("MAGICIAN_MEMORY_EVAL_OPERATIONS").ok();
    let selected: Option<BTreeSet<&str>> = selected.as_deref().map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|op| !op.is_empty())
            .collect()
    });
    if let Some(selected) = &selected {
        anyhow::ensure!(
            !selected.is_empty() && selected.iter().all(|op| operations.contains(op)),
            "evaluation operation selection must name supported memory owners"
        );
    }
    let operations: Vec<&str> = operations
        .into_iter()
        .filter(|op| {
            selected
                .as_ref()
                .is_none_or(|selected| selected.contains(op))
        })
        .collect();
    anyhow::ensure!(
        !qualified_conflict_probe
            || (mode == "operator_gate" && operations == ["memory_conflict_review"]),
        "qualified conflict probe requires isolated operator_gate conflict selection"
    );
    let policy = decision_host::decision_backend_for("magician")
        .unwrap()
        .operations()
        .await?;
    anyhow::ensure!(
        policy.contract_version == decision_engine_contract::CONTRACT_VERSION,
        "wire mismatch"
    );
    for &op in &operations {
        anyhow::ensure!(
            policy.operations.iter().any(|p| p.name == op
                && if mode == "shadow" {
                    p.shadow && !p.gate
                } else if mode == "operator_gate" {
                    p.gate && !p.shadow && p.classification.allow_unqualified_gate
                } else {
                    p.gate
                        && !p.shadow
                        && !p.classification.qualifications.is_empty()
                        && p.classification
                            .qualifications
                            .iter()
                            .all(|q| q.human_review_ref.starts_with("unreviewed-synthetic-test:"))
                }),
            "isolated {mode} binding missing: {op}"
        );
    }
    if qualified_conflict_probe {
        let binding = policy
            .operations
            .iter()
            .find(|binding| binding.name == "memory_conflict_review")
            .unwrap();
        for output in ["replace_existing", "keep_existing"] {
            anyhow::ensure!(
                binding
                    .classification
                    .qualifications
                    .iter()
                    .any(|qualification| {
                        qualification.question == "resolution"
                            && qualification.output == output
                            && qualification
                                .human_review_ref
                                .starts_with("unreviewed-synthetic-test:")
                    }),
                "isolated test-only qualification missing for {output}"
            );
        }
    }
    let memory = AgentMemoryService::with_scoped_memory_scope_in_workspace(
        ledger_workspace.clone(),
        principal,
        &workspace,
    );
    let prompts = Arc::new(PromptManager::new(Arc::new(
        JsonPromptStorage::with_default_config()?,
    )));
    let mut results = Vec::new();
    for &operation in &operations {
        prime(operation, principal, &workspace).await?;
        let scoped = router.with_task_context(Some(
            magicllm::dispatch::TaskRef::task(operation)
                .with_scope(principal, &workspace)
                .with_execution(format!("{operation}-exec"), format!("{operation}-exec")),
        ));
        let started = Instant::now();
        let result: anyhow::Result<Value> = async {
            match operation {
                "memory_utility_review" => {
                    let source = "The release verification code for project Amber is AMBER-741. Use this exact code when reporting the verified release.";
                    let candidate: agents::MemoryCandidateDocument = serde_json::from_value(json!({
                        "principal":principal,"workspace":workspace,"agent_id":"eval-agent","scope":"agent",
                        "tier_name":"episodes.items","semantic_memory_type":"episode","item_key":"amber","json_pointer":"/items/0",
                        "content_hash":agents::source_text_hash(source),"last_updated":chrono::Utc::now(),"text":source,"metadata_json":{}
                    }))?;
                    let overlay = agents::sync_memory_temperature_overlay(memory.storage(), &[candidate]).await?;
                    let candidate_key = overlay.entries.keys().next().ok_or_else(||anyhow::anyhow!("candidate overlay empty"))?.clone();
                    let input = MemoryTemperatureUtilityReviewInput {
                        run_id:"utility-owner-run".into(), agent_id:"eval-agent".into(), task_id:Some(operation.into()), execution_id:None, chat_session_id:None,
                        goal:"Report the release verification code for project Amber".into(), outcome:"goal_achieved".into(), final_answer:"The release verification code is AMBER-741.".into(), action_trace:vec![],
                        selected_candidates:vec![MemoryPromptSelectedCandidate {
                            memory_candidate_key:candidate_key, semantic_memory_type:SemanticMemoryType::Episode,
                            temperature_tier:MemoryTemperatureTier::T2, tier_name:"episodes.items".into(),source_key:"amber".into(),source_ids:vec!["episodes.items#amber".into()],
                            source_text_hash:agents::source_text_hash(source),source_text:source.into(),text:source.into(),projection_used:false,app_model_processing:None,
                        }],
                    };
                    let summary = agents::review_memory_temperature_utility(memory.clone(), Arc::new(scoped.clone()), input).await?;
                    let persisted = agents::memory_temperature_utility_review_was_applied(memory.storage(), "utility-owner-run").await?;
                    anyhow::ensure!(persisted && summary.reviewed == 1, "utility did not persist its review: {summary:?}");
                    Ok(json!({"summary":summary,"persisted":persisted}))
                },
                "memory_lifecycle_relation" => {
                    let now = chrono::Utc::now();
                    let original = json!({"preferences":agents::memory_lifecycle::merge_items(&[], &[json!({"key":"drink","value":"I prefer unsweetened tea in the morning."}),json!({"key":"units","value":"Use Celsius for temperature reports."})], now)});
                    memory.save_user_knowledge(&original).await?;
                    prime(operation, principal, &workspace).await?;
                    let outcome = agents::memory_lifecycle::runtime::pass(&memory, Some(&scoped), None, now, None).await?;
                    let saved = memory.load_user_knowledge().await?;
                    anyhow::ensure!(outcome.reviewed == 1 && outcome.applied == 1 && outcome.error.is_none(), "lifecycle did not apply: {outcome:?}");
                    anyhow::ensure!(saved["preferences"][0]["value"] == original["preferences"][0]["value"], "lifecycle changed preference evidence");
                    Ok(json!({"summary":outcome,"source_preserved":true}))
                },
                "evidence_promote" => {
                    let spec = tier_distill::producer_spec("meeting").unwrap();
                    let source_key = "meeting:amber-launch";
                    let row = json!({"key":source_key,"source_type":spec.source_type,"account":"amber-launch","day":chrono::Utc::now().format("%Y-%m-%d").to_string(),
                        "summary":"Amber launch planning meeting: Mira owns the security review due September 30. Deployment is blocked until the review passes. Team agreed to launch October 2 after approval.",
                        "decisions":["Security review is a release prerequisite"],"action_items":["Mira: finish security review by September 30"]});
                    let tier = magician::magician_v2::chat::service::normalized_user_memory_tier_name(spec.tier).unwrap();
                    let mut source = serde_json::Map::new();
                    source.insert(tier, json!([row]));
                    memory.save_user_knowledge(&Value::Object(source)).await?;
                    let resolver = agents::memory::AgentMemoryResolver::with_workspace_layout(ledger_workspace.clone());
                    let summary = tier_distill::distill_tier_producer(&resolver, &ledger_workspace, principal, &workspace, "meeting", 7, &scoped, &prompts).await.map_err(anyhow::Error::msg)?;
                    anyhow::ensure!(summary["distilled"] == 1, "substantive meeting was not promoted: {summary}");
                    let stored = memory.load_user_work_evidence().await?;
                    anyhow::ensure!(stored.iter().any(|r|r.source_refs == vec![source_key.to_string()] && !r.summary.trim().is_empty()), "evidence not durable");
                    let now = chrono::Utc::now().to_rfc3339();
                    let episode = serde_json::from_value(json!({
                        "principal":principal,"workspace":workspace,"agent_id":"eval-agent","episode_id":"evidence-episode","goal_key":"amber-release","consolidation_key":"amber-release",
                        "trigger_type":"manual","trigger_seq":1,"trigger_timestamp":now,"started_at":now,"completed_at":now,
                        "outcome_kind":"succeeded","execution_status":"completed","outcome_summary":"Project Amber release review confirmed that Mira owns the security review due September 30 and launch remains blocked until approval.",
                        "task_id":operation,"execution_id":format!("{operation}-exec"),"root_execution_id":format!("{operation}-exec"),
                        "observations":["The release planning record states Mira must finish the security review before the October 2 launch."],"origin_surface":"owner"
                    }))?;
                    memory.append_native_episode("eval-agent", &episode).await?;
                    let episode_started = Instant::now();
                    let episode_review = tier_distill::distill_episode_with_response(
                        &episode, &scoped, &prompts, Some((&memory, &prompts))
                    ).await?;
                    magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
                        bus.clone(), principal, &workspace, "memory_consolidation"
                    ).emit_validated_success(
                        "distill_evidence", &episode_review.response,
                        episode_started.elapsed().as_millis() as u64,
                        magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution {
                            execution_id: episode.execution_id.clone(),
                            task_id: episode.task_id.clone(),
                            agent_id: Some(episode.agent_id.clone()),
                            ..Default::default()
                        },
                        "memory_evidence_proposal",
                    );
                    anyhow::ensure!(episode_review.proposal.importance.is_some(), "episode evidence review missing importance");
                    Ok(json!({"promoted":true,"persisted":true,"source_refs_preserved":true,"episode_source_persisted":true}))
                },
                "memory_connection_gate" => {
                    let sources = vec![
                        ConnectionSource{id:"activity".into(), key:"event:climate".into(), revision:"1".into(),text:"Registration for the climate founders networking dinner closes tomorrow. Five climate startup founders will attend. The owner has not registered.".into()},
                        ConnectionSource{id:"m0".into(), key:"goal:founders".into(),revision:"1".into(),text:"The owner wants to meet climate startup founders this month to find collaborators for the new research project.".into()},
                    ];
                    let policy = ConnectionDecisionPolicy::capture(&scoped).await;
                    let review = memory_connections::review_connection(&scoped, &sources, &policy).await?;
                    anyhow::ensure!(review.current(), "connection source/policy stale");
                    let connection = review.connection.ok_or_else(||anyhow::anyhow!("concrete unmet opportunity not surfaced"))?;
                    anyhow::ensure!(connection.evidence.len() >= 2 && !connection.summary.trim().is_empty(), "connection lacks grounded evidence");
                    Ok(json!({"grounded":true,"citation_count":connection.evidence.len(),"surface":connection.surface}))
                },
                "procedure_feedback" => {
                    use magician::magician_v2::learning::*;
                    let store = LearningStore::new(ledger_workspace.clone());
                    let scope = LearningScope::new(principal, &workspace);
                    let procedure = store.create_procedure(scope.clone(), serde_json::from_value(json!({
                        "title":"Verify release code", "summary":"Read the project release record and report its exact verification code.",
                        "workflow":["Read the project release record","Extract the verification code without modifying it","Report the exact code"],
                        "verification":["Compare reported code with the source record"],"owner_agent":"eval-agent"
                    }))?)?;
                    store.append_event(scope.clone(), serde_json::from_value(json!({
                        "event_type":"learning_procedure_retrieval_rendered","agent_id":"eval-agent","task_id":operation,"execution_id":format!("{operation}-exec"),"root_execution_id":format!("{operation}-exec"),
                        "summary":"Rendered verification procedure for this run", "payload":{"selected":[{"procedure_id":procedure.id,"title":procedure.title,"success_count":0,"failure_count":0}]}
                    }))?)?;
                    let now = chrono::Utc::now().to_rfc3339();
                    let episode = serde_json::from_value(json!({
                        "principal":principal,"workspace":workspace,"agent_id":"eval-agent","episode_id":"procedure-episode","goal_key":"verify-code","consolidation_key":"verify-code",
                        "trigger_type":"test","trigger_seq":1,"trigger_timestamp":now,"started_at":now,"completed_at":now,
                        "outcome_kind":"succeeded","execution_status":"completed","outcome_summary":"Followed the retrieved verification procedure: read the Amber release record, extracted AMBER-741, checked it against the source, then reported exactly AMBER-741.",
                        "task_id":operation,"execution_id":format!("{operation}-exec"),"root_execution_id":format!("{operation}-exec"),"observations":["Source record verification code was AMBER-741; final answer matched exactly."],
                        "context_at_start":"Report the exact release verification code using the retrieved verification procedure."
                    }))?;
                    // Observation replay rechecks the immutable episode from
                    // this isolated owner store before using its captured text.
                    memory.append_native_episode("eval-agent", &episode).await?;
                    let runtime = LearningReflectionRuntime::new(ledger_workspace.clone(), Some(Arc::new(scoped.clone())), Arc::new(PromptManager::new(Arc::new(JsonPromptStorage::with_default_config()?))))
                        .with_event_broadcaster(Some(bus.clone()));
                    let run = runtime.reflect_episode(LearningReflectionInput {boundary:"execution_completed".into(),episode,extra_context:json!({})}).await?;
                    let after = store.read_procedure(&scope, &procedure.id)?;
                    let feedback = store.list_events(&scope, 100)?.into_iter().any(|e|e.event_type == "learning_procedure_feedback_recorded" && e.payload["procedure_id"] == procedure.id);
                    anyhow::ensure!(run.event_id.is_some() && run.skipped_reason.is_none() && feedback && after.success_count == 1 && after.failure_count == 0,
                        "procedure feedback did not persist successful use: success={} failure={} feedback={feedback} run={run:?}", after.success_count, after.failure_count);
                    Ok(json!({"run":run,"feedback_persisted":feedback,"success_count":after.success_count,"failure_count":after.failure_count}))
                },
                "memory_episode_quality" | "memory_conflict_review" => {
                    let mut definition: agents::AgentDefinition = serde_json::from_value(json!({"agent_id":"eval-agent","name":"Evaluation Agent","persona":"Extract source-grounded facts only","tools":[]}))?;
                    let tier: agents::MemoryTierDefinition = serde_json::from_value(json!({
                        "name":"knowledge","scope":"agent","description":"Verified project facts",
                        "schema":{"facts":{"type":"collection","max_items":50,"item_schema":{"key":{"type":"text"},"value":{"type":"text"}}}},
                        "render":{"format":"text","template":"{facts}"},"retention":"forever"
                    }))?;
                    definition.memory_tiers.push(tier.clone());
                    let consolidator = agents::memory_consolidator::MemoryConsolidator::new(memory.clone(), Some(Arc::new(scoped.clone())), Some(Arc::new(PromptManager::new(Arc::new(JsonPromptStorage::with_default_config()?)))))
                        .with_llm_telemetry_broadcaster(bus.clone());
                    if operation == "memory_conflict_review" {
                        let mut native = magician::magician_v2::artifact_v2::memory::V3MemoryTierRecord::new("knowledge", agents::TierScope::Agent, None, Some(principal), Some(&workspace), Some("eval-agent"));
                        native.fields.insert("facts".into(), json!([
                            {"key":"amber_runtime","value":"Project Amber uses Rust for the backend service."},
                            {"key":"amber_runtime","value":"Project Amber uses TypeScript for the browser user interface."}
                        ]));
                        memory.save_native_tier("eval-agent", &tier, None, &native).await?;
                        // Keep this agent-only fixture independent of lifecycle's hourly review state.
                        memory.save_user_knowledge(&json!({})).await?;
                        prime(operation, principal, &workspace).await?;
                        let result = consolidator.run_contradiction_sweep_for_agent(&definition, "eval-agent", chrono::Utc::now()).await?;
                        let stored = memory.load_native_tier("eval-agent", &tier, None).await?.ok_or_else(||anyhow::anyhow!("conflict tier missing"))?;
                        anyhow::ensure!(result.reviewed == 1 && result.keep_both == 1 && result.superseded == 0 && result.missing_decisions == 0, "compatible facts mishandled: {result:?}");
                        anyhow::ensure!(stored.fields["facts"].as_array().is_some_and(|v|v.len()==2), "conflict review lost source facts");
                        let mut destructive_cases = Vec::new();
                        let mut qualification_revocation = None;
                        if mode == "operator_gate" {
                            for (case_agent, expected, existing, incoming) in [
                                (
                                    "eval-agent-replace",
                                    "replace_existing",
                                    "Unverified Project Amber release draft used placeholder verification code AMBER-000. This draft is obsolete.",
                                    "The signed, current Project Amber release record supersedes the draft and verifies code AMBER-741.",
                                ),
                                (
                                    "eval-agent-keep",
                                    "keep_existing",
                                    "The signed, current Project Amber release record verifies code AMBER-741.",
                                    "An obsolete, unverified Project Amber draft used placeholder verification code AMBER-000.",
                                ),
                            ] {
                                let mut case_native = magician::magician_v2::artifact_v2::memory::V3MemoryTierRecord::new(
                                    "knowledge", agents::TierScope::Agent, None, Some(principal), Some(&workspace), Some(case_agent)
                                );
                                case_native.fields.insert("facts".into(), json!([
                                    {"key":"amber_release_verification_code","value":existing},
                                    {"key":"amber_release_verification_code","value":incoming}
                                ]));
                                memory.save_native_tier(case_agent, &tier, None, &case_native).await?;
                                let before = memory.load_native_tier(case_agent, &tier, None).await?.ok_or_else(||anyhow::anyhow!("conflict tier missing before sweep"))?;
                                let mut case_events = bus.subscribe();
                                let outcome = consolidator.run_contradiction_sweep_for_agent(&definition, case_agent, chrono::Utc::now()).await?;
                                let after = memory.load_native_tier(case_agent, &tier, None).await?.ok_or_else(||anyhow::anyhow!("conflict tier missing"))?;
                                let facts = after.fields["facts"].as_array().ok_or_else(||anyhow::anyhow!("conflict facts missing"))?;
                                let qualified = qualified_conflict_probe;
                                anyhow::ensure!(outcome.reviewed == 1 && outcome.superseded == usize::from(qualified) && outcome.missing_decisions == usize::from(!qualified),
                                    "restricted {expected} owner outcome mismatch: {outcome:?}");
                                let superseded_index = if expected == "replace_existing" { 0 } else { 1 };
                                anyhow::ensure!(facts.len() == 2 && facts[0]["value"] == existing && facts[1]["value"] == incoming
                                    && facts.iter().enumerate().all(|(index, fact)|
                                        (fact["memory_lifecycle"] == "superseded") == (qualified && index == superseded_index)),
                                    "restricted {expected} decision changed the wrong stored fact");
                                let primary = std::iter::from_fn(|| case_events.try_recv().ok())
                                    .filter_map(|event| match event {
                                        RuntimeTransportEvent::DecisionShadowAgreement {principal:p,workspace:w,record,..}
                                            if p == principal && w == workspace => serde_json::to_value(record).ok(),
                                        _ => None,
                                    })
                                    .find(|record| record["operation"] == "memory_conflict_review" && record["mode"] == "gate")
                                    .ok_or_else(||anyhow::anyhow!("missing gated {expected} conflict record"))?;
                                anyhow::ensure!(primary["items"][0]["response"]["answers"]["resolution"]["choice"] == expected
                                    && primary["items"][0]["eligible_answers"]["resolution"].is_string() == qualified,
                                    "restricted {expected} eligibility mismatch: {primary}");
                                destructive_cases.push(json!({"raw_choice":expected,"eligible":qualified,"superseded_index":if qualified {Some(superseded_index)} else {None},"before":before.fields["facts"],"after":after.fields["facts"],"summary":outcome}));
                            }
                            if let Ok(path) = std::env::var("MAGICIAN_MEMORY_EVAL_REVOKE_ENGINE_CONFIG") {
                                anyhow::ensure!(qualified_conflict_probe, "qualification revocation requires the qualified conflict probe");
                                let path = PathBuf::from(path);
                                anyhow::ensure!(path.parent().is_some_and(|parent|
                                        parent.parent() == Some(std::path::Path::new("/Volumes/build/magician/tmp"))
                                            && parent.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("memory-hardening-")))
                                    && path.file_name().and_then(|name| name.to_str()) == Some("decision-engine.yaml")
                                    && socket.starts_with("/tmp/memory-decision-"),
                                    "qualification revocation can only edit an isolated memory-hardening engine config");
                                let mut settings: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(&path)?)?;
                                settings["operations"]["memory_conflict_review"]["qualifications"] = serde_yaml::Value::Sequence(Vec::new());
                                let temporary = path.with_extension("yaml.eval-revoke.tmp");
                                std::fs::write(&temporary, serde_yaml::to_string(&settings)?)?;
                                std::fs::rename(&temporary, &path)?;
                                let backend = decision_host::decision_backend_for("magician").ok_or_else(||anyhow::anyhow!("missing decision backend"))?;
                                let mut new_policy = None;
                                for _ in 0..40 {
                                    tokio::time::sleep(Duration::from_millis(250)).await;
                                    let discovery = backend.operations().await?;
                                    let conflict = discovery.operations.iter().find(|op|op.name=="memory_conflict_review");
                                    if discovery.policy_revision != policy.policy_revision
                                        && conflict.is_some_and(|op|op.classification.qualifications.is_empty()) {
                                        new_policy = Some(discovery);
                                        break;
                                    }
                                }
                                let new_policy = new_policy.ok_or_else(||anyhow::anyhow!("engine did not publish revoked qualification policy"))?;
                                // Force a new owner invocation with the same source pair and
                                // process-local caches after the engine changes authority.
                                tokio::time::sleep(Duration::from_secs(3)).await;
                                let agent = "eval-agent-replace";
                                let mut reset = magician::magician_v2::artifact_v2::memory::V3MemoryTierRecord::new(
                                    "knowledge", agents::TierScope::Agent, None, Some(principal), Some(&workspace), Some(agent)
                                );
                                reset.fields.insert("facts".into(), destructive_cases[0]["before"].clone());
                                memory.save_native_tier(agent, &tier, None, &reset).await?;
                                let mut revocation_events = bus.subscribe();
                                let outcome = consolidator.run_contradiction_sweep_for_agent(&definition, agent, chrono::Utc::now()).await?;
                                let after = memory.load_native_tier(agent, &tier, None).await?.ok_or_else(||anyhow::anyhow!("revocation tier missing"))?;
                                anyhow::ensure!(outcome.reviewed == 1 && outcome.superseded == 0 && outcome.missing_decisions == 1
                                    && after.fields["facts"] == destructive_cases[0]["before"],
                                    "revoked authority applied cached supersession: {outcome:?}");
                                let record = std::iter::from_fn(|| revocation_events.try_recv().ok())
                                    .filter_map(|event|match event {
                                        RuntimeTransportEvent::DecisionShadowAgreement {principal:p,workspace:w,record,..}
                                            if p==principal && w==workspace => serde_json::to_value(record).ok(),
                                        _ => None,
                                    })
                                    .find(|record|record["operation"]=="memory_conflict_review" && record["mode"]=="gate")
                                    .ok_or_else(||anyhow::anyhow!("missing revoked conflict decision record"))?;
                                anyhow::ensure!(record["items"][0]["response"]["answers"]["resolution"]["choice"]=="replace_existing"
                                    && record["items"][0]["eligible_answers"]["resolution"].is_null(),
                                    "revoked decision retained eligibility: {record}");
                                qualification_revocation = Some(json!({"old_policy_revision":policy.policy_revision,"new_policy_revision":new_policy.policy_revision,"before":destructive_cases[0]["before"],"after":after.fields["facts"],"summary":outcome,"decision":record}));
                            }
                        }
                        Ok(json!({"summary":result,"persisted":true,"both_sources_preserved":true,"destructive_cases":destructive_cases,"qualification_revocation":qualification_revocation}))
                    } else {
                        definition.memory_consolidation.push(serde_json::from_value(json!({
                            "name":"quality-owner-extract","trigger":"step_completed","source":"episodes(amber-goal)","target":"knowledge",
                            "transform":{"type":"llm","prompt":"Extract durable project facts from the source episodes. Preserve the exact project name and verification code. Return JSON matching the target tier schema.","operation":"memory_insight_distillation"}
                        }))?);
                        let now = chrono::Utc::now().to_rfc3339();
                        let episode = serde_json::from_value(json!({
                            "principal":principal,"workspace":workspace,"agent_id":"eval-agent","episode_id":"quality-episode","goal_key":"amber-goal","consolidation_key":"amber-goal",
                            "trigger_type":"manual","trigger_seq":1,"trigger_timestamp":now,"started_at":now,"completed_at":now,
                            "outcome_kind":"succeeded","execution_status":"completed","outcome_summary":"Verified the Amber release record. Its release verification code is AMBER-741. This code is needed to validate the release package.",
                            "task_id":operation,"execution_id":format!("{operation}-exec"),"root_execution_id":format!("{operation}-exec"),"observations":["The source release record explicitly states verification code AMBER-741 for Project Amber."],"origin_surface":"owner"
                        }))?;
                        memory.append_native_episode("eval-agent", &episode).await?;
                        prime(operation, principal, &workspace).await?;
                        let result = consolidator.run_step_rules_for_v3_episodes(&definition, "eval-agent", "amber-goal", &[episode]).await?;
                        let stored = memory.load_native_tier("eval-agent", &tier, None).await?;
                        anyhow::ensure!(result.skipped_rules.is_empty() && !result.updated_targets.is_empty() && stored.as_ref().is_some_and(|r|serde_json::to_string(&r.fields).is_ok_and(|s|s.contains("AMBER-741"))), "quality-backed extraction did not persist grounded fact: {result:?}");
                        Ok(json!({"updated_targets":result.updated_targets,"skipped_rules":result.skipped_rules,"grounded_fact_persisted":true}))
                    }
                },
                _ => unreachable!(),
            }
        }.await;
        let passed = result.is_ok();
        if let Err(error) = &result {
            eprintln!("{operation}: {error:#}");
        }
        results.push(json!({"operation":operation,"passed":passed,"latency_ms":started.elapsed().as_millis(),"result":result.as_ref().ok(),"error":result.err().map(|e|e.to_string())}));
        eprintln!("owner replay {operation}: {passed}");
    }
    let settle_seconds = std::env::var("MAGICIAN_MEMORY_EVAL_WAIT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(4)
        .min(45);
    tokio::time::sleep(Duration::from_secs(settle_seconds)).await;
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
    let mut physical: BTreeSet<String> = records
        .iter()
        .flat_map(|r| r["record"]["calls"].as_array().into_iter().flatten())
        .filter_map(|c| c["call_id"].as_str().map(str::to_owned))
        .collect();
    physical.extend(records.iter().filter_map(|r| {
        r["record"]["reference"]["call"]["receipt"]["context"]["llm_call_id"]
            .as_str()
            .map(str::to_owned)
    }));
    physical.extend(records.iter().filter_map(|r| {
        r["record"]["text_call"]["receipt"]["context"]["llm_call_id"]
            .as_str()
            .map(str::to_owned)
    }));
    let event_receipt_match = physical.iter().all(|id| calls.contains_key(id));
    let shutdown = activation.shutdown().await;
    // Preserve semantic outcomes and physical receipts even if the governed
    // ledger query fails: a failed accounting check is still evaluation evidence.
    std::fs::create_dir_all(&output)?;
    for (name, value) in [
        ("policy.json", serde_json::to_value(&policy)?),
        ("results.json", json!(results)),
        ("comparisons.json", json!(records)),
        ("receipts.json", json!(calls.values().collect::<Vec<_>>())),
        ("capture-shutdown.json", json!(shutdown)),
    ] {
        std::fs::write(output.join(name), serde_json::to_vec_pretty(&value)?)?;
    }
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
    let receipt_match = event_receipt_match
        && duplicate_events == 0
        && physical.iter().all(|id| calls.contains_key(id))
        && calls.keys().all(|id| {
            ledger_calls
                .iter()
                .filter(|row| row.get("llm_call_id").and_then(Value::as_str) == Some(id))
                .count()
                == 1
        });

    std::fs::write(
        output.join("ledger.json"),
        serde_json::to_vec_pretty(&json!({"calls":ledger_calls,"attempts":attempts}))?,
    )?;
    anyhow::ensure!(
        serde_json::to_value(&shutdown)?["activation"]["gap_records_emitted"] == 0,
        "canonical trace capture reported gaps"
    );
    anyhow::ensure!(
        gaps == 0 && receipt_match,
        "receipt reconciliation failed: gaps={gaps} duplicates={duplicate_events}"
    );
    for &operation in &operations {
        anyhow::ensure!(
            records.iter().any(|r| r["record"]["operation"] == operation
                && r["record"]["calls"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())),
            "no real Decision Model owner call: {operation}"
        );
    }
    if mode == "operator_gate" {
        anyhow::ensure!(
            records
                .iter()
                .all(|r| r["record"]["mode"] != "gate"
                    || r["record"]["reference_attempted"] == false),
            "gated classification attempted an incumbent LLM fallback"
        );
    }
    if mode != "shadow" && operations.contains(&"memory_conflict_review") {
        // This benign pair has high-confidence keep_both support at the real
        // threshold. Other low-confidence answers must retain normal fallback.
        anyhow::ensure!(
            records
                .iter()
                .any(|r| r["record"]["operation"] == "memory_conflict_review"
                    && r["record"]["items"].as_array().is_some_and(|items| items
                        .iter()
                        .any(|item| item["eligible_answers"]["resolution"].is_string()))),
            "fixture gate did not exercise the qualified compatible-pair decision"
        );
    }
    if std::env::var("MAGICIAN_MEMORY_EVAL_REQUIRE_CONFLICT_SAMPLE").as_deref() == Ok("1") {
        anyhow::ensure!(
            operations == ["memory_conflict_review"],
            "conflict sample requires the isolated conflict owner selection"
        );
        let primary = records
            .iter()
            .filter(|r| {
                r["record"]["operation"] == "memory_conflict_review"
                    && r["record"]["mode"] == "gate"
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            primary.len() == 3,
            "expected benign and both restricted conflict owner cases"
        );
        for row in primary {
            let comparison_id = &row["record"]["comparison_id"];
            anyhow::ensure!(
                records
                    .iter()
                    .any(|stage| stage["record"]["comparison_id"] == *comparison_id
                        && stage["record"]["stage"] == "reference"
                        && stage["record"]["status"] == "completed"
                        && stage["record"]["reference_attempted"] == true
                        && stage["record"]["reference"]["labels"]["0"]["resolution"].is_string()
                        && stage["record"]["reference"]["call"]["receipt"]["context"]
                            ["llm_call_id"]
                            .is_string()),
                "missing source-bound completed conflict reference for {comparison_id}"
            );
        }
    }
    if std::env::var("MAGICIAN_MEMORY_EVAL_REQUIRE_EVIDENCE_SAMPLE").as_deref() == Ok("1") {
        anyhow::ensure!(
            operations == ["evidence_promote"],
            "evidence sample requires the isolated evidence owner selection"
        );
        let primary = records
            .iter()
            .filter(|r| {
                r["record"]["operation"] == "evidence_promote" && r["record"]["mode"] == "gate"
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            primary.len() == 2,
            "expected tier and persisted-episode evidence owners"
        );
        anyhow::ensure!(
            records
                .iter()
                .filter(|stage| {
                    stage["record"]["stage"] == "reference"
                        && stage["record"]["status"] == "completed"
                        && stage["record"]["reference_attempted"] == true
                        && primary.iter().any(|row| {
                            row["record"]["comparison_id"] == stage["record"]["comparison_id"]
                        })
                })
                .count()
                == 2,
            "persisted tier and episode references did not both complete"
        );
        anyhow::ensure!(
            records
                .iter()
                .any(|stage| stage["record"]["stage"] == "reference"
                    && stage["record"]["status"] == "completed"
                    && stage["record"]["reference_attempted"] == true
                    && stage["record"]["reference"]["labels"]["0"]["promote"].is_boolean()
                    && stage["record"]["reference"]["labels"]["0"]["importance"].is_number()
                    && stage["record"]["reference"]["call"]["receipt"]["context"]["llm_call_id"]
                        .is_string()
                    && primary
                        .iter()
                        .any(|row| row["record"]["comparison_id"]
                            == stage["record"]["comparison_id"])),
            "persisted episode evidence reference did not complete"
        );
    }
    anyhow::ensure!(
        results.iter().all(|r| r["passed"] == true),
        "owner semantics failed; see retained results"
    );
    Ok(())
}
