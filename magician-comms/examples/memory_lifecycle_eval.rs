//! Frozen, real-model memory journeys through production ingress, review,
//! persistence, candidate rendering and durable owner responses. Synthetic data.
use anyhow::{ensure, Context, Result};
use magician::{
    config::load_magician_config_from_path,
    magician_v2::{
        agents::{
            memory_candidates::{load_memory_candidate_documents, MemoryCandidateRequest},
            memory_lifecycle::{self as lifecycle, runtime},
            memory_temperature::memory_candidate_has_superseded_lifecycle,
            memory_tiers::TierScope,
            retrieval_scope::RetrievalScope,
            AgentMemoryResolver,
        },
        artifact_v2::workspace::ArtifactV2Workspace,
        attention::resurfacing::{
            memory_connections::ConnectionState, store::ResurfacingStore, types::*,
        },
        attention_funnel::AttentionScope,
        chat::service::merge_user_memory_tier_fields,
        feed::FeedStore,
        query_analysis::operation_llm_router::OperationLlmRouter,
        realtime_events::RuntimeTransportBroadcaster,
        user_requests::{ScopedResponseResult, UserRequestService, UserResponse},
    },
};
use magician_comms::channel_assist::resurfacing::{
    interaction::{MemoryInteractionAdapter, ResurfacingInteractionRegistry},
    memory_connections::ConnectionRuntime,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
#[path = "memory_lifecycle_eval/answers.rs"]
mod answers;
#[path = "memory_lifecycle_eval/capture.rs"]
mod capture;

#[derive(Deserialize)]
struct Case {
    id: String,
    partition: String,
    initial: Vec<Value>,
    steps: Vec<Value>,
}

fn keys(document: &Value, wanted: &str) -> Vec<String> {
    let mut result: Vec<_> = document["preferences"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            if wanted == "retired" {
                lifecycle::retired(item)
            } else {
                lifecycle::state(item) == wanted
            }
        })
        .filter_map(|item| item.get("key").and_then(Value::as_str).map(str::to_owned))
        .collect();
    result.sort();
    result
}

fn expected(step: &Value, key: &str) -> Vec<String> {
    let mut values: Vec<String> = serde_json::from_value(step[key].clone()).unwrap_or_default();
    values.sort();
    values
}

fn observe(observed: runtime::Observation) -> Value {
    let response = observed.response.as_ref();
    let telemetry = response.and_then(|r| r.telemetry.as_ref());
    let usage = telemetry
        .filter(|t| t.usage_reported)
        .and_then(|_| response.and_then(|r| r.usage.as_ref()));
    json!({"incoming":observed.incoming,"offered":observed.offered,"error":observed.error,
        "raw_response":response.map(|r|&r.content),"model":telemetry.map(|t|&t.model),
        "provider":telemetry.map(|t|&t.provider),"profile":telemetry.and_then(|t|t.profile.as_ref()),
        "usage":usage.map(|u|json!({"input_tokens":u.prompt_tokens,"output_tokens":u.completion_tokens,"total_tokens":u.total_tokens})),
        "router_estimated_cost_usd":telemetry.filter(|t|t.usage_reported&&t.cost_usd.is_finite()&&t.cost_usd>0.0).map(|t|t.cost_usd)})
}

async fn request_service(root: &Path, layout: ArtifactV2Workspace) -> UserRequestService {
    UserRequestService::new(Arc::new(RuntimeTransportBroadcaster::new(64)))
        .with_workspace_layout(layout)
        .with_history_persist_path(root.join("request-history.json"))
        .with_pending_persist_path(root.join("request-pending.json"))
        .await
}

async fn evaluate(
    case: &Case,
    repeat: usize,
    root: &Path,
    router: &OperationLlmRouter,
    help: Option<&Value>,
) -> Result<Value> {
    fs::create_dir(root)?;
    let layout = ArtifactV2Workspace::new(root);
    let resolver = AgentMemoryResolver::with_workspace_layout(layout.clone());
    let principal = "memory-lifecycle-eval";
    let workspace = format!("{}-{repeat}", case.id);
    let service = resolver.resolve_for_scope(principal, &workspace)?;
    let now = chrono::Utc::now();
    let initial: Vec<_> = case
        .initial
        .iter()
        .map(|item| {
            let mut item = item.clone();
            item["memory_review_version"] = json!(lifecycle::POLICY_VERSION);
            item["updated_at"] = json!((now - chrono::Duration::days(30)).to_rfc3339());
            item
        })
        .collect();
    service
        .save_user_knowledge(&json!({"preferences":initial}))
        .await?;
    // Warm production recall before changes to exercise index invalidation.
    let warm_query = case
        .initial
        .iter()
        .map(lifecycle::text)
        .collect::<Vec<_>>()
        .join(" ");
    let warm_recall = magician::magician_v2::attention::resurfacing::memory_connections::recall(
        &service,
        &warm_query,
    )
    .await?;
    let requests = Arc::new(request_service(root, layout.clone()).await);
    let mut events = Vec::new();
    let mut all_passed = true;
    for (index, step) in case.steps.iter().enumerate() {
        let at = now + chrono::Duration::days(index as i64);
        let mut item = step.clone();
        for name in [
            "expected_active",
            "expected_retired",
            "questions",
            "answer",
            "after_answer_active",
            "after_answer_retired",
        ] {
            item.as_object_mut().unwrap().remove(name);
        }
        item["updated_at"] = json!(at.to_rfc3339());
        let key = item["key"].as_str().context("missing key")?.to_owned();
        let fields = serde_json::Map::from_iter([(key, item)]);
        let saved =
            merge_user_memory_tier_fields(&resolver, principal, &workspace, "preferences", &fields)
                .await;
        ensure!(saved["status"] == "ok", "ingress failed: {saved}");
        let mut observations = Vec::new();
        let pass = runtime::pass(
            &service,
            Some(router),
            Some(&requests),
            at,
            Some(&mut |o| observations.push(observe(o))),
        )
        .await;
        let document = service.load_user_knowledge().await?;
        let active = keys(&document, "active");
        let retired = keys(&document, "retired");
        let questions: Vec<lifecycle::Conflict> = document[lifecycle::JOURNAL]["conflicts"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .filter(|c: &lifecycle::Conflict| c.state == "pending")
            .collect();
        let candidates = load_memory_candidate_documents(
            service.storage(),
            "owner",
            &[],
            &MemoryCandidateRequest {
                scope: TierScope::User,
                goal_id: None,
                recency_cutoff: None,
                include_environment_knowledge: false,
                retrieval_scope: RetrievalScope::Unbound,
            },
        )
        .await?;
        let recalled: Vec<_> = candidates
            .iter()
            .filter(|c| !memory_candidate_has_superseded_lifecycle(c))
            .collect();
        let recalled_pointers: Vec<_> = recalled.iter().map(|c| c.json_pointer.clone()).collect();
        let retirement_excluded = candidates.iter().all(|c| {
            let source = document.pointer(&c.json_pointer);
            if source
                .is_some_and(|s| lifecycle::retired(s) || lifecycle::state(s) == "pending_review")
            {
                !recalled_pointers.contains(&c.json_pointer)
            } else {
                true
            }
        });
        let passed = pass
            .as_ref()
            .is_ok_and(|p| p.error.is_none() && p.applied == 1)
            && active == expected(step, "expected_active")
            && retired == expected(step, "expected_retired")
            && questions.len() == step["questions"].as_u64().unwrap_or(0) as usize
            && retirement_excluded
            && observations.len() == 1
            && observations[0]["raw_response"].is_string();
        let mut event = json!({"step":index,"passed":passed,"active":active,"retired":retired,
            "expected":step,"questions":questions,"document":document,"recall":recalled,
            "retirement_excluded":retirement_excluded,"observations":observations,
            "pass":match pass {Ok(p)=>json!(p),Err(e)=>json!({"error":e.to_string()})}});
        all_passed &= passed;
        if let Some(choice) = step.get("answer").and_then(Value::as_str) {
            if let Some(conflict) = questions.first() {
                let id = runtime::request_id(principal, &workspace, &conflict.id);
                let response = requests
                    .respond_scoped(
                        UserResponse {
                            request_id: id,
                            decision: choice.into(),
                            input: None,
                            channel: "evaluation".into(),
                            sensitive: Vec::new(),
                        },
                        Some(principal),
                        Some(&workspace),
                    )
                    .await;
                let answered = matches!(response, ScopedResponseResult::Accepted);
                runtime::reconcile_questions(&service, &requests, at).await?;
                let after = service.load_user_knowledge().await?;
                let answer_passed = answered
                    && keys(&after, "active") == expected(step, "after_answer_active")
                    && keys(&after, "retired") == expected(step, "after_answer_retired");
                let before_replay = lifecycle::digest(&after);
                runtime::reconcile_questions(&service, &requests, at).await?;
                let replay_safe =
                    lifecycle::digest(&service.load_user_knowledge().await?) == before_replay;
                event["answer"] = json!({"passed":answer_passed&&replay_safe,"document":after,"replay_safe":replay_safe});
                all_passed &= answer_passed && replay_safe;
            } else {
                all_passed = false;
            }
        }
        events.push(event);
        fs::write(
            root.join("journey.json"),
            serde_json::to_vec_pretty(&json!({"id":case.id,"events":events}))?,
        )?;
    }
    let help_result = if let Some(help) = help {
        let activity = help["activity"].as_str().context("missing help activity")?;
        let at = chrono::Utc::now();
        service
            .update_user_knowledge(|document| {
                document["user.knowledge"] = json!([{"key":"current_activity","value":activity,
                "updated_at":at.to_rfc3339(),"memory_review_version":lifecycle::POLICY_VERSION}]);
                Ok(true)
            })
            .await?;
        let scope = AttentionScope {
            principal: principal.into(),
            workspace: workspace.clone(),
        };
        let runtime = ConnectionRuntime {
            store: ResurfacingStore::open(root)?,
            resolver: resolver.clone(),
            interactions: ResurfacingInteractionRegistry::from_adapters(vec![Arc::new(
                MemoryInteractionAdapter::new(resolver.clone()),
            )]),
            feed: FeedStore::open_workspace(layout.clone())?,
            requests: Some(requests.clone()),
            router: Some(Arc::new(router.clone())),
            attention: None,
            taste: None,
        };
        let candidate = Candidate {
            candidate_id: "lifecycle-help".into(),
            source_kind: SourceKind::Memory,
            source_ref: "user.knowledge#current_activity".into(),
            title: help["title"].as_str().unwrap().into(),
            content_digest: activity.into(),
            content_revision: Some(at.to_rfc3339()),
            content_details: None,
            semantic_features: None,
            salience_score: 0.9,
            signals: Default::default(),
            temporal_anchor_at: None,
            embedding_id: None,
            state: CandidateState::Candidate,
            first_seen_at: at.timestamp(),
            last_scored_at: at.timestamp(),
            last_surfaced_at: None,
            cooldown_until: 0,
            surface_count: 0,
            dismiss_count: 0,
        };
        runtime
            .store
            .upsert_candidate(principal, &workspace, &candidate)
            .await?;
        let mut observations = Vec::new();
        runtime
            .pass_with_observer(&scope, &mut None, at.timestamp(), &mut |o| {
                observations.push(json!({
            "sources":o.sources,"error":o.error,"response":o.response.as_ref().map(|r|&r.content),
                    "telemetry":o.response.as_ref().and_then(|r|r.telemetry.as_ref()).map(|t|json!({
                        "model":t.model,"provider":t.provider,"usage_reported":t.usage_reported,
                        "input_tokens":if t.usage_reported {Some(t.input_tokens)} else {None},
                        "output_tokens":if t.usage_reported {Some(t.output_tokens)} else {None},
                        "router_estimated_cost_usd":if t.usage_reported && t.cost_usd>0.0 {Some(t.cost_usd)} else {None}
                    })),
        }))
            })
            .await?;
        let record = runtime
            .store
            .get_connection(principal, &workspace, &candidate.candidate_id)
            .await?;
        let state_matches = record
            .as_ref()
            .is_some_and(|r| match help["expected"].as_str() {
                Some("published") => {
                    r.state == ConnectionState::Published
                        && r.connection.as_ref().is_some_and(|c| {
                            help["allowed_surfaces"]
                                .as_array()
                                .is_some_and(|allowed| allowed.contains(&json!(c.surface)))
                        })
                },
                Some("empty") => r.state == ConnectionState::Empty,
                _ => false,
            });
        let recalled: Vec<_> = observations
            .iter()
            .flat_map(|o| o["sources"].as_array().into_iter().flatten())
            .filter(|s| s["id"] != "activity")
            .filter_map(|s| s["text"].as_str())
            .collect();
        let current_recalled = help["must_recall"]
            .as_str()
            .is_some_and(|needle| recalled.iter().any(|t| t.contains(needle)));
        let retired_excluded = help["must_not_recall"]
            .as_str()
            .is_none_or(|needle| !recalled.iter().any(|t| t.contains(needle)));
        let passed =
            state_matches && current_recalled && retired_excluded && observations.len() == 1;
        all_passed &= passed;
        Some(
            json!({"passed":passed,"expected":help,"record":record,"observations":observations,
            "current_recalled":current_recalled,"retired_excluded":retired_excluded}),
        )
    } else {
        None
    };
    Ok(
        json!({"id":case.id,"partition":case.partition,"repeat":repeat,"passed":all_passed,
        "root":root,"events":events,"warm_recall_before_changes":warm_recall,"downstream_help":help_result}),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    magician_chunking::register_builtin_chunk_adapters()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(args.len() % 2 == 0, "expected --name value arguments");
    let opts: HashMap<_, _> = args
        .chunks_exact(2)
        .map(|p| (p[0].as_str(), p[1].as_str()))
        .collect();
    let config_path = PathBuf::from(*opts.get("--config").context("--config required")?);
    let output = PathBuf::from(*opts.get("--output-dir").context("--output-dir required")?);
    let suite = PathBuf::from(*opts.get("--suite").context("--suite required")?);
    let partition = opts.get("--partition").copied().unwrap_or("development");
    ensure!(
        matches!(partition, "development" | "validation" | "all"),
        "unknown fixture partition"
    );
    let repeats: usize = opts.get("--repeats").unwrap_or(&"1").parse()?;
    ensure!((1..=3).contains(&repeats), "repeats must be 1..3");
    ensure!(
        !output.exists(),
        "use a fresh output directory; preserve earlier failures"
    );
    let root = config_path.parent().context("config must have parent")?;
    for name in [".env.development", ".env"] {
        if root.join(name).exists() {
            ensure!(
                dotenvy::from_path(root.join(name)).is_ok(),
                "runtime environment could not be loaded"
            );
        }
    }
    magician::magician_v2::query_analysis::operation_llm_router::install_llm_routing_overrides(
        root,
    );
    let config = load_magician_config_from_path(&config_path)?;
    let router = OperationLlmRouter::new(Some(
        config.router_config().cloned().context("missing router")?,
    ));
    if router.shared_configured_router().is_none() {
        magician::magician_v2::llm_chunking::validate_router_chunking_config(
            config.router_config().context("missing router config")?,
        )?;
        magicllm::ConfiguredRouter::from_router_config(
            config
                .router_config()
                .cloned()
                .context("missing router config")?,
        )?;
        anyhow::bail!("operation router initialization failed validation");
    }
    ensure!(
        router
            .explicit_binding_for_operation(lifecycle::OPERATION)
            .is_some(),
        "lifecycle operation unbound"
    );
    let suite_bytes = fs::read(&suite)?;
    let help_bytes = fs::read(suite.with_file_name("help_journeys.json"))?;
    let help: Value = serde_json::from_slice(&help_bytes)?;
    let capture_bytes = fs::read(suite.with_file_name("capture_journey.json"))?;
    let capture_fixture: Value = serde_json::from_slice(&capture_bytes)?;
    let answer_bytes = fs::read(suite.with_file_name("clarification_journey.json"))?;
    let answer_fixture: Value = serde_json::from_slice(&answer_bytes)?;
    let cases: Vec<Case> = serde_json::from_slice::<Vec<Case>>(&suite_bytes)?
        .into_iter()
        .filter(|c| partition == "all" || c.partition == partition)
        .collect();
    // Natural extraction may make one schema-repair call per utterance.
    let calls = (8 + cases
        .iter()
        .map(|c| c.steps.len() + usize::from(help.get(&c.id).is_some()))
        .sum::<usize>())
        * repeats;
    ensure!(
        calls > 0 && calls <= 100,
        "expected 1..100 bounded model calls"
    );
    fs::create_dir_all(&output)?;
    let mut rows = Vec::new();
    for repeat in 0..repeats {
        for case in &cases {
            let root = output.join(format!("{}-{repeat}", case.id));
            let row = match evaluate(case, repeat, &root, &router, help.get(&case.id)).await {
                Ok(row) => row,
                Err(error) => {
                    json!({"id":case.id,"repeat":repeat,"passed":false,"error":error.to_string(),"root":root})
                },
            };
            println!(
                "{} repeat {}: {}",
                case.id,
                repeat + 1,
                if row["passed"] == true {
                    "passed"
                } else {
                    "FAILED"
                }
            );
            rows.push(row);
            fs::write(
                output.join("report.json"),
                serde_json::to_vec_pretty(&json!({
                    "kind":"production_memory_lifecycle_journeys","suite":suite,
                    "suite_hash":blake3::hash(&suite_bytes).to_hex().to_string(),"partition":partition,
                    "help_suite_hash":blake3::hash(&help_bytes).to_hex().to_string(),
                    "repeats":repeats,"cases":rows,"input_boundary":"structured memory tool fields",
                    "ui_boundary":"production durable user-request service; no browser/device interaction"
                }))?,
            )?;
        }
        let capture_root = output.join(format!("natural-capture-{repeat}"));
        let captured = match capture::run(&capture_root, &router, &capture_fixture).await {
            Ok(row) => row,
            Err(error) => json!({"passed":false,"error":format!("{error:#}")}),
        };
        fs::write(
            capture_root.join("report.json"),
            serde_json::to_vec_pretty(&captured)?,
        )?;
        rows.push(
            json!({"id":"natural-capture","repeat":repeat,"passed":captured["passed"],
            "root":capture_root,"capture":captured}),
        );
        let answer_root = output.join(format!("free-text-clarification-{repeat}"));
        let answered = match answers::run(&answer_root, &router, &answer_fixture).await {
            Ok(row) => row,
            Err(error) => json!({"passed":false,"error":format!("{error:#}")}),
        };
        fs::write(
            answer_root.join("report.json"),
            serde_json::to_vec_pretty(&answered)?,
        )?;
        rows.push(
            json!({"id":"free-text-clarification","repeat":repeat,"passed":answered["passed"],
            "root":answer_root,"clarification":answered}),
        );
        fs::write(
            output.join("report.json"),
            serde_json::to_vec_pretty(&json!({
                "kind":"production_memory_lifecycle_journeys","suite":suite,
                "suite_hash":blake3::hash(&suite_bytes).to_hex().to_string(),"partition":partition,
                "help_suite_hash":blake3::hash(&help_bytes).to_hex().to_string(),
                "capture_suite_hash":blake3::hash(&capture_bytes).to_hex().to_string(),
                "clarification_suite_hash":blake3::hash(&answer_bytes).to_hex().to_string(),
                "repeats":repeats,"cases":rows,"max_model_calls":calls,
                "input_boundary":"structured fields plus persisted natural chat turns through production consolidation",
                "ui_boundary":"production durable user-request and attention delivery services; no browser/device interaction"
            }))?,
        )?;
    }
    ensure!(
        rows.iter().all(|r| r["passed"] == true),
        "one or more journeys failed; evidence retained at {}",
        output.display()
    );
    Ok(())
}
