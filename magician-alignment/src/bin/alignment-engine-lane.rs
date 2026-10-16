//! The structured-decision judgment lane: pack `action_alignment` at
//! [`ENGINE_PACK_VERSION`] through a running Decision Engine over the portable
//! corpus, composed by [`magician_alignment::composer`] into
//! `alignment_decision.v2` predictions the preference-alignment scorer reads.
//!
//! Run on purpose only: it spends real model calls or local model time. Point
//! it at a disposable engine (its own config and socket) whose
//! `action_alignment` route pins one model; it never reads or writes live
//! config. Built behind the `engine-lane` feature, so it needs no `magician`
//! build: `make eval-preference-alignment-engine-live`.
//!
//! Each case is one `/v1/decide` batch: the shared context plus one item per
//! offered intention. Raw answers are recorded once; predictions are composed
//! at each threshold, so one run serves a threshold sweep. A composition that
//! does not resolve is recorded as `unresolved` (held), the way the host
//! would treat it before any fallback.
//!
//! | variable | meaning |
//! |---|---|
//! | `PA_ENGINE_SOCKET` | the disposable engine's socket (required) |
//! | `PA_LIVE_CASES` | case files, `:`- or space-separated (default: the public dev split) |
//! | `PA_LIVE_OUT_DIR` | where answers, predictions and the manifest go (required) |
//! | `PA_LIVE_CONCURRENCY` | cases in flight (default 2) |
//! | `PA_LIVE_LIMIT` | only the first N cases, for a smoke run |
//! | `PA_ENGINE_THRESHOLDS` | comma-separated confidence thresholds (default `0.5,0.6,0.7,0.8,0.9`) |
//! | `PA_ENGINE_LOCALITY` | `cloud` (default) or `local` |
//!
//! To recompose a recorded run with a changed composer, use `alignment-replay`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use decision_engine_contract::batch::{BatchRequest, ClassificationMode, DecisionItem, ItemStatus};
use decision_engine_contract::client::EngineClient;
use decision_engine_contract::request::DecisionState;
use decision_engine_contract::wire::{DecideRequest, Locality, CONTRACT_VERSION};
use futures_util::StreamExt;
use serde_json::{json, Value};

use magician_alignment::composer::{
    heads_from_answers, project, ComposerBars, IntentionHeads, ENGINE_OPERATION,
    ENGINE_PACK_VERSION, PROJECTION_VERSION,
};
use magician_alignment::eval_case::parse_cases;
use magician_alignment::replay::write_predictions;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let socket = PathBuf::from(env("PA_ENGINE_SOCKET").expect("set PA_ENGINE_SOCKET"));
    let out_dir = PathBuf::from(env("PA_LIVE_OUT_DIR").expect("set PA_LIVE_OUT_DIR"));
    std::fs::create_dir_all(&out_dir).expect("output directory");
    let client = EngineClient::new(&socket, Duration::from_secs(5));
    let operations = client.operations().await.expect("the engine answers discovery");
    let policy = operations
        .operations
        .iter()
        .find(|operation| operation.name == ENGINE_OPERATION)
        .cloned()
        .expect("the engine binds action_alignment");
    assert_eq!(
        policy.classification.pack_version, ENGINE_PACK_VERSION,
        "the engine must serve the per-intention pack"
    );
    let budget_ms = policy.classification.limits.shadow_budget_ms;
    let client = client.with_timeout(Duration::from_millis(budget_ms + 5_000));
    let locality = match env("PA_ENGINE_LOCALITY").as_deref() {
        Some("local") => Locality::Local,
        _ => Locality::Cloud,
    };
    let thresholds: Vec<f64> = env("PA_ENGINE_THRESHOLDS")
        .unwrap_or_else(|| "0.5,0.6,0.7,0.8,0.9".to_string())
        .split(',')
        .filter_map(|value| value.trim().parse().ok())
        .collect();

    let case_paths: Vec<PathBuf> = env("PA_LIVE_CASES")
        .map(|paths| {
            paths
                .split(|c: char| c == ':' || c.is_whitespace())
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_else(|| {
            vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../data/magician_v2/evals/preference_alignment/dev.jsonl")]
        });
    let mut cases = Vec::new();
    for path in &case_paths {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        cases.extend(parse_cases(&text).expect("cases parse"));
    }
    if let Some(limit) = env("PA_LIVE_LIMIT").and_then(|value| value.parse::<usize>().ok()) {
        cases.truncate(limit);
    }
    let concurrency = env("PA_LIVE_CONCURRENCY")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2)
        .max(1);

    std::fs::write(
        out_dir.join("manifest.json"),
        serde_json::to_string_pretty(&json!({
            "lane": "preference_alignment_engine",
            "started_at": chrono::Utc::now().to_rfc3339(),
            "operation": ENGINE_OPERATION,
            "pack_version": ENGINE_PACK_VERSION,
            "projection_version": PROJECTION_VERSION,
            "engine_instance": operations.engine_instance,
            "policy_revision": operations.policy_revision,
            "route_local": policy.route_local,
            "route_cloud": policy.route_cloud,
            "locality": format!("{locality:?}"),
            "case_files": case_paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            "cases": cases.len(),
            "concurrency": concurrency,
            "thresholds": thresholds,
        }))
        .unwrap(),
    )
    .expect("manifest");

    let revision = operations.policy_revision.clone();
    let started = Instant::now();
    let records: Vec<(String, Value, Option<BTreeMap<String, IntentionHeads>>)> =
        futures_util::stream::iter(cases.iter())
            .map(|case| {
                let client = client.clone();
                let revision = revision.clone();
                async move {
                    let input = case.to_alignment_input();
                    let (context, items) = project(&input);
                    let request = DecideRequest {
                        batch: BatchRequest {
                            mode: ClassificationMode::Shadow,
                            reference_version: "action_alignment_offline.v1".to_string(),
                            request_id: uuid::Uuid::new_v4().simple().to_string(),
                            expected_policy_revision: revision,
                            projection_version: PROJECTION_VERSION.to_string(),
                            execution_budget_ms: budget_ms,
                            context: Some(DecisionState::from_json(context)),
                            items: items
                                .into_iter()
                                .map(|(item_id, state)| DecisionItem {
                                    item_id,
                                    state: DecisionState::from_json(state),
                                    choice_candidates: BTreeMap::new(),
                                })
                                .collect(),
                        },
                        contract_version: CONTRACT_VERSION,
                        operation: ENGINE_OPERATION.to_string(),
                        state: DecisionState::from_text(""),
                        choice_candidates: BTreeMap::new(),
                        locality,
                    };
                    let call_started = Instant::now();
                    let reply = client.decide(&request).await;
                    let latency_ms = call_started.elapsed().as_millis() as u64;
                    let mut raw = json!({"id": case.id, "latency_ms": latency_ms});
                    let heads = match reply {
                        Err(error) => {
                            raw["error"] = json!(format!("{error:?}"));
                            None
                        },
                        Ok(reply) => {
                            let mut heads = BTreeMap::new();
                            let mut complete = true;
                            let mut items = Vec::new();
                            let (mut input_tokens, mut output_tokens) = (0u64, 0u64);
                            for item in &reply.batch.items {
                                let parsed = (item.status == ItemStatus::Answered)
                                    .then(|| item.response.as_ref())
                                    .flatten()
                                    .and_then(|response| {
                                        input_tokens += response.usage.input_tokens;
                                        output_tokens += response.usage.output_tokens;
                                        heads_from_answers(&response.answers)
                                    });
                                items.push(json!({
                                    "item_id": item.item_id,
                                    "status": format!("{:?}", item.status),
                                    "model": item.response.as_ref().map(|response| &response.model.model),
                                    "heads": parsed,
                                    "error": item.error,
                                    "latency_ms": item.latency_ms,
                                }));
                                match parsed {
                                    Some(parsed) => {
                                        heads.insert(item.item_id.clone(), parsed);
                                    },
                                    None => complete = false,
                                }
                            }
                            raw["status"] = json!(format!("{:?}", reply.status));
                            raw["error"] = json!(reply.error);
                            raw["items"] = json!(items);
                            raw["usage"] = json!({
                                "prompt_tokens": input_tokens,
                                "completion_tokens": output_tokens,
                            });
                            complete.then_some(heads)
                        },
                    };
                    (case.id.clone(), raw, heads)
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;

    let count = records.len();
    for (path, unresolved) in
        write_predictions(&out_dir, &cases, records, &ComposerBars::default(), &thresholds)
            .expect("predictions")
    {
        eprintln!("{unresolved} unresolved -> {}", path.display());
    }
    eprintln!(
        "{} cases in {:.0}s",
        count,
        started.elapsed().as_secs_f64()
    );
}
