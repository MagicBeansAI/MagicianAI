//! The live judgment lane (P8.2, P8.8): the bound reference judge over the
//! portable corpus, through the production call ([`judge::reference_call`]).
//!
//! An `#[ignore]` test, run only on purpose: it spends real model calls.
//! `make eval-preference-alignment-judgment-live` runs it and then scores it.
//! It reads no live config and writes none: the router tables, judge route
//! and budgets are this branch's repository seed (what it would ship, and what
//! this code validates), and only the provider key comes from the owner's
//! env file.
//!
//! Each call's raw answer is recorded with its latency and token use, under a
//! generous timeout. Scoring then reads one run three ways: through the host
//! validator (the governed judge), without it (the prompt on its own), and
//! with production's response budget applied to the recorded latency.
//!
//! Environment:
//!
//! | variable | meaning |
//! |---|---|
//! | `PA_LIVE_EVAL=1` | required; otherwise the test returns at once |
//! | `PA_LIVE_ENV_FILE` | env file with the provider key (default: `$MAGICIAN_ROOT_DIR/.env`, else `~/MagicianNotes/.env`) |
//! | `PA_LIVE_CASES` | case files, `:`- or space-separated (default: the public dev split) |
//! | `PA_LIVE_OUT_DIR` | where predictions and the manifest go (required) |
//! | `PA_LIVE_REPEATS` | independent passes (default 1) |
//! | `PA_LIVE_CONCURRENCY` | calls in flight (default 4) |
//! | `PA_LIVE_TIMEOUT_MS` | per-call timeout (default 120000) |
//! | `PA_LIVE_LIMIT` | only the first N cases, for a smoke run |
//! | `PA_LIVE_JUDGE_PROFILE` | bind the judge to this declared profile instead of the seed's route, to compare candidate routes |

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use futures_util::StreamExt;
    use serde_json::{json, Value};

    use crate::config::PREFERENCE_ALIGNMENT_JUDGE_OPERATION;
    use crate::magician_v2::preference_alignment::eval_case::parse_cases;
    use crate::magician_v2::preference_alignment::judge::reference_call;
    use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;

    fn env(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.trim().is_empty())
    }

    fn env_number(name: &str, default: u64) -> u64 {
        env(name)
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "spends real model calls; run through make eval-preference-alignment-judgment-live"]
    async fn live_judgment_lane() {
        if env("PA_LIVE_EVAL").as_deref() != Some("1") {
            eprintln!("PA_LIVE_EVAL is not 1; the live lane does not run");
            return;
        }
        // Provider setup reports why a profile was skipped only through
        // tracing; without a subscriber that reason is lost.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_writer(std::io::stderr)
            .try_init();
        // Router tables and budgets from this branch's repository seed: the
        // judge route this code would ship, validated by this code. Only the
        // provider key comes from the owner's env file; no live config file is
        // read or written.
        let env_file = env("PA_LIVE_ENV_FILE")
            .map(PathBuf::from)
            .or_else(|| env("MAGICIAN_ROOT_DIR").map(|root| PathBuf::from(root).join(".env")))
            .unwrap_or_else(|| {
                PathBuf::from(env("HOME").unwrap_or_default()).join("MagicianNotes/.env")
            });
        assert!(
            env_file.is_file(),
            "no env file with the provider key at {}",
            env_file.display()
        );
        // Existing variables win; the file only fills what is missing.
        dotenvy::from_path(&env_file).expect("the env file parses");
        let out_dir = PathBuf::from(env("PA_LIVE_OUT_DIR").expect("set PA_LIVE_OUT_DIR"));
        std::fs::create_dir_all(&out_dir).expect("output directory");

        let config: crate::config::MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("the repository config parses");
        let mut router_config = config
            .router_config()
            .cloned()
            .expect("the repository config has llm.router");
        // Only the judge's route: its operation, the profiles its selector
        // names, their fallbacks and the default. The rest of the seed is not
        // under test, and parts of it (logical-context chunking profiles)
        // need boot-time adapters a test binary cannot register.
        // A candidate route for comparison: rebind the judge to one declared
        // profile. The seed's binding stays the default.
        let profile_override = env("PA_LIVE_JUDGE_PROFILE");
        if let Some(profile) = &profile_override {
            assert!(
                router_config.profiles.contains_key(profile),
                "PA_LIVE_JUDGE_PROFILE names no declared profile: {profile}"
            );
            router_config.operation_mapping.insert(
                PREFERENCE_ALIGNMENT_JUDGE_OPERATION.to_string(),
                profile.as_str().into(),
            );
        }
        let selector = router_config
            .operation_mapping
            .get(PREFERENCE_ALIGNMENT_JUDGE_OPERATION)
            .cloned()
            .expect("the repository seed binds the judge");
        fn strings(value: &Value, out: &mut Vec<String>) {
            match value {
                Value::String(text) => out.push(text.clone()),
                Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
                Value::Object(map) => map.values().for_each(|item| strings(item, out)),
                _ => {},
            }
        }
        let mut named = Vec::new();
        strings(&serde_json::to_value(&selector).unwrap_or(Value::Null), &mut named);
        named.push(router_config.default_profile.clone());
        let mut keep: std::collections::BTreeSet<String> = named
            .into_iter()
            .filter(|name| router_config.profiles.contains_key(name))
            .collect();
        loop {
            let fallbacks: Vec<String> = keep
                .iter()
                .filter_map(|name| router_config.profiles.get(name))
                .filter_map(|profile| profile.metadata.as_ref())
                .filter_map(|meta| meta.get("fallback_profile").and_then(Value::as_str))
                .map(str::to_string)
                .filter(|name| !keep.contains(name) && router_config.profiles.contains_key(name))
                .collect();
            if fallbacks.is_empty() {
                break;
            }
            keep.extend(fallbacks);
        }
        router_config.profiles.retain(|name, _| keep.contains(name));
        router_config.adaptive_profiles.clear();
        router_config
            .operation_mapping
            .retain(|operation, _| operation == PREFERENCE_ALIGNMENT_JUDGE_OPERATION);
        let mut chunking_disabled = Vec::new();
        for (name, profile) in router_config.profiles.iter_mut() {
            if profile.chunking.take().is_some() {
                chunking_disabled.push(name.clone());
            }
        }
        chunking_disabled.sort();
        let route_profiles: Vec<String> = keep.into_iter().collect();
        assert!(
            router_config
                .operation_mapping
                .contains_key(PREFERENCE_ALIGNMENT_JUDGE_OPERATION),
            "the repository seed binds the judge"
        );
        if let Err(error) = magicllm::ConfiguredRouter::from_router_config(router_config.clone()) {
            panic!("the router cannot be built: {error}");
        }
        let mapping_source = if profile_override.is_some() {
            "PA_LIVE_JUDGE_PROFILE"
        } else {
            "repository_seed"
        };
        let router = Arc::new(OperationLlmRouter::new(Some(router_config)));
        let judge_ready = router
            .shared_configured_router()
            .is_some_and(|configured| {
                configured.has_provider_for_operation(PREFERENCE_ALIGNMENT_JUDGE_OPERATION)
            });
        assert!(
            judge_ready,
            "the judge's provider did not register; see the magicllm.bootstrap warning above"
        );

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
        let mut case_files = Vec::new();
        for path in &case_paths {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            case_files.push(json!({"path": path.display().to_string(), "sha256": sha256_hex(text.as_bytes())}));
            cases.extend(parse_cases(&text).expect("cases parse"));
        }
        if let Some(limit) = env("PA_LIVE_LIMIT").and_then(|value| value.parse::<usize>().ok()) {
            cases.truncate(limit);
        }
        let repeats = env_number("PA_LIVE_REPEATS", 1).max(1);
        let concurrency = env_number("PA_LIVE_CONCURRENCY", 4).max(1) as usize;
        let timeout = Duration::from_millis(env_number("PA_LIVE_TIMEOUT_MS", 120_000));

        let manifest = json!({
            "lane": "preference_alignment_judgment_live",
            "started_at": chrono::Utc::now().to_rfc3339(),
            "prompt": {
                "name": crate::magician_v2::prompts::constants::names::ACTION_ALIGNMENT_JUDGE,
                "version": crate::magician_v2::prompts::constants::versions::ACTION_ALIGNMENT_JUDGE,
            },
            "decision_schema": crate::magician_v2::preference_alignment::contract::ALIGNMENT_DECISION_SCHEMA_VERSION,
            "judge_mapping_source": mapping_source,
            "judge_profile": profile_override,
            "router_tables": "repository_seed",
            "route_profiles": route_profiles,
            "chunking_disabled_profiles": chunking_disabled,
            "case_files": case_files,
            "cases": cases.len(),
            "repeats": repeats,
            "concurrency": concurrency,
            "timeout_ms": timeout.as_millis() as u64,
            "production_budget_ms": config.preference_alignment.budgets.foreground_check_ms,
        });
        std::fs::write(
            out_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .expect("manifest");

        for repeat in 1..=repeats {
            let started = Instant::now();
            let records: Vec<Value> = futures_util::stream::iter(cases.iter())
                .map(|case| {
                    let router = Arc::clone(&router);
                    async move {
                        let input = case.to_alignment_input();
                        let call_started = Instant::now();
                        let answer = reference_call(&router, &input, timeout).await;
                        let latency_ms = call_started.elapsed().as_millis() as u64;
                        match answer {
                            Ok((reference_version, response)) => {
                                let decision =
                                    serde_json::from_str::<Value>(response.content.trim()).ok();
                                json!({
                                    "id": case.id,
                                    "repeat": repeat,
                                    "decision": decision,
                                    "raw": if decision.is_none() {
                                        Some(response.content.chars().take(2000).collect::<String>())
                                    } else {
                                        None
                                    },
                                    "latency_ms": latency_ms,
                                    "usage": response.usage.as_ref().map(|usage| json!({
                                        "prompt_tokens": usage.prompt_tokens,
                                        "completion_tokens": usage.completion_tokens,
                                    })),
                                    "reference_version": reference_version,
                                })
                            },
                            Err(cause) => json!({
                                "id": case.id,
                                "repeat": repeat,
                                "decision": null,
                                "unresolved": format!("{cause:?}"),
                                "latency_ms": latency_ms,
                            }),
                        }
                    }
                })
                .buffer_unordered(concurrency)
                .collect()
                .await;
            let mut sorted = records;
            sorted.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            let path = out_dir.join(format!("predictions-r{repeat}.jsonl"));
            let body: String = sorted
                .iter()
                .map(|record| format!("{record}\n"))
                .collect();
            std::fs::write(&path, body).expect("predictions");
            let unresolved = sorted
                .iter()
                .filter(|record| record.get("unresolved").is_some())
                .count();
            eprintln!(
                "repeat {repeat}: {} cases in {:.0}s, {unresolved} unresolved -> {}",
                sorted.len(),
                started.elapsed().as_secs_f64(),
                path.display()
            );
        }
    }
}
