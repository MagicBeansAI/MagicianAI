//! Cross-provider live conformance lane for interrupted tool-history replay.
//!
//! The lane deliberately enters through [`ChatLlmService`], not provider
//! adapters or hand-built HTTP payloads. That keeps it sensitive to the exact
//! production repair, native/flattened replay choice, and OpenAI Responses
//! continuation-anchor behavior used by chat.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use magicllm::{
    capability::LLMProviderKind,
    config::{LLMProfile, LLMRouterConfig, OperationProfileSelector},
    types::LLMToolSpec,
};
use serde::Serialize;
use serde_json::{json, Value};

use magician::config::{load_default_magician_config, load_magician_config_from_path};
use magician::magician_v2::{
    chat::{
        llm_service::{ChatLlmClient, ChatLlmService, ProviderReplayRequestAudit},
        models::{AssistantProviderState, ChatLlmTranscriptEntry, StoredToolCall},
    },
    query_analysis::multi_llm_service::MultiLLMService,
};

const SCHEMA_VERSION: &str = "provider_replay_live_eval.v1";
const COMPLETE_CALL_ID: &str = "call-provider-replay-complete";
const ORPHAN_CALL_ID: &str = "call-provider-replay-orphan";
const NEW_ORPHAN_CALL_ID: &str = "call-provider-replay-new-orphan";
const FIRST_SENTINEL: &str = "PROVIDER_REPLAY_OK";
const CONTINUATION_SENTINEL: &str = "PROVIDER_REPLAY_CONTINUED";
const SUPPORTED_FAMILIES: &[&str] = &[
    "openai_responses",
    "openai_chat",
    "anthropic",
    "gemini",
    "minimax",
    "deepseek",
    "openrouter",
    "ollama",
    "yutori",
];

#[derive(Debug, Parser)]
#[command(about = "Live cross-provider interrupted tool replay conformance lane")]
struct Args {
    /// Active Magician configuration. Defaults to the normal runtime resolver.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Run only these exact source profile names. Repeat for multiple profiles.
    #[arg(long = "profile")]
    profiles: Vec<String>,
    #[arg(long, default_value = "coverage/evals/provider-replay/live/latest")]
    output_dir: PathBuf,
    /// Resolve and report the provider matrix without making provider calls.
    #[arg(long)]
    dry_run: bool,
    /// Provider-free report/rendering smoke test.
    #[arg(long)]
    self_test: bool,
}

#[derive(Debug, Clone)]
struct SelectedProfile {
    source_name: String,
    family: String,
    profile: LLMProfile,
}

#[derive(Debug, Clone, Serialize)]
struct FamilyCoverage {
    family: String,
    configured: bool,
    selected_profile: Option<String>,
    status: String,
}

#[derive(Debug, Clone, Serialize)]
struct UsageSummary {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    reasoning_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
}

#[derive(Debug, Clone, Serialize)]
struct ProviderReplayRow {
    family: String,
    provider: String,
    api_mode: Option<String>,
    profile: String,
    model: String,
    credential_env: Option<String>,
    status: String,
    latency_ms: Option<u128>,
    continuation_latency_ms: Option<u128>,
    audit: Option<ProviderReplayRequestAudit>,
    continuation_anchor_reused: Option<bool>,
    newer_interruption_invalidated_anchor: Option<bool>,
    usage: Option<UsageSummary>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct EvalReport {
    schema_version: &'static str,
    generated_at: DateTime<Utc>,
    mode: String,
    status: String,
    gate: String,
    configured_family_count: usize,
    exercised_family_count: usize,
    passed_family_count: usize,
    failed_family_count: usize,
    family_coverage: Vec<FamilyCoverage>,
    results: Vec<ProviderReplayRow>,
}

pub async fn run_cli() -> Result<()> {
    let args = Args::parse();
    if args.self_test {
        let report = self_test_report();
        write_report(&args.output_dir, &report)?;
        println!(
            "Provider replay self-test report: {}",
            args.output_dir.join("report.html").display()
        );
        return Ok(());
    }

    load_runtime_env_files();
    let config = match args.config.as_deref() {
        Some(path) => load_magician_config_from_path(path)
            .with_context(|| format!("loading config {}", path.display()))?,
        None => load_default_magician_config().context("loading active Magician config")?,
    };
    let router = config
        .llm
        .router
        .as_ref()
        .ok_or_else(|| anyhow!("active config has no llm.router"))?;
    let selected = select_profiles(router, &args.profiles)?;
    if selected.is_empty() {
        bail!("no tool-capable provider profiles were selected");
    }

    let mut rows = Vec::with_capacity(selected.len());
    for selected_profile in &selected {
        if args.dry_run {
            rows.push(planned_row(selected_profile));
        } else {
            rows.push(run_provider(selected_profile).await);
        }
    }
    let report = build_report(router, &selected, rows, args.dry_run);
    write_report(&args.output_dir, &report)?;
    println!(
        "Provider replay report: {}",
        args.output_dir.join("report.html").display()
    );
    if !args.dry_run && report.failed_family_count > 0 {
        bail!(
            "{} configured provider replay family/families failed",
            report.failed_family_count
        );
    }
    Ok(())
}

fn load_runtime_env_files() {
    for file_name in [".env.development", ".env"] {
        let path = magician::magician_v2::artifact_v2::workspace::runtime_config_path(
            file_name, file_name,
        );
        let _ = dotenvy::from_path(path);
    }
}

fn provider_family(profile: &LLMProfile) -> String {
    match &profile.provider {
        LLMProviderKind::OpenAI => match profile
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("openai_api_mode"))
            .and_then(Value::as_str)
            .unwrap_or("responses")
        {
            "chat" => "openai_chat".to_string(),
            _ => "openai_responses".to_string(),
        },
        LLMProviderKind::Anthropic => "anthropic".to_string(),
        LLMProviderKind::Gemini => "gemini".to_string(),
        LLMProviderKind::Minimax => "minimax".to_string(),
        LLMProviderKind::DeepSeek => "deepseek".to_string(),
        LLMProviderKind::OpenRouter => "openrouter".to_string(),
        LLMProviderKind::Ollama => "ollama".to_string(),
        LLMProviderKind::Yutori => "yutori".to_string(),
        LLMProviderKind::Xai => "xai_responses".to_string(),
        LLMProviderKind::Sarvam => "sarvam".to_string(),
        LLMProviderKind::Custom(name) => format!("custom_{}", safe_slug(name)),
    }
}

fn tool_choice(profile: &LLMProfile) -> Option<&str> {
    let value = profile.metadata.as_ref()?.get("tool_choice")?;
    value
        .as_str()
        .or_else(|| value.get("type").and_then(Value::as_str))
}

fn is_tool_capable(profile: &LLMProfile) -> bool {
    profile.supports_tool_calling.unwrap_or(false) || tool_choice(profile).is_some()
}

fn profile_rank(name: &str, profile: &LLMProfile) -> (u8, u8, u8, u8, String) {
    let lowered = format!("{} {}", name, profile.model).to_lowercase();
    let chat_rank = if name.starts_with("chat-") { 0 } else { 1 };
    let auto_rank = if tool_choice(profile) == Some("auto") {
        0
    } else {
        1
    };
    let reasoning_rank = if profile.reasoning.is_none() { 0 } else { 1 };
    let size_rank = if lowered.contains("nano")
        || lowered.contains("flash-lite")
        || lowered.contains("haiku")
    {
        0
    } else if lowered.contains("mini")
        || lowered.contains("flash")
        || lowered.contains("m2.7")
        || lowered.contains("m27")
    {
        1
    } else {
        2
    };
    (
        chat_rank,
        auto_rank,
        reasoning_rank,
        size_rank,
        name.to_string(),
    )
}

fn select_profiles(router: &LLMRouterConfig, explicit: &[String]) -> Result<Vec<SelectedProfile>> {
    if !explicit.is_empty() {
        let mut selected = Vec::with_capacity(explicit.len());
        let mut seen = BTreeSet::new();
        for name in explicit {
            if !seen.insert(name.clone()) {
                bail!("duplicate --profile {name}");
            }
            let profile = router
                .profiles
                .get(name)
                .with_context(|| format!("unknown provider replay profile {name}"))?;
            if !is_tool_capable(profile) {
                bail!("profile {name} is not configured for tool calling");
            }
            selected.push(SelectedProfile {
                source_name: name.clone(),
                family: provider_family(profile),
                profile: profile.clone(),
            });
        }
        return Ok(selected);
    }

    let mut candidates: BTreeMap<String, Vec<(&String, &LLMProfile)>> = BTreeMap::new();
    for (name, profile) in &router.profiles {
        if is_tool_capable(profile) {
            candidates
                .entry(provider_family(profile))
                .or_default()
                .push((name, profile));
        }
    }
    let mut selected = Vec::with_capacity(candidates.len());
    for (family, mut profiles) in candidates {
        profiles.sort_by_key(|(name, profile)| profile_rank(name, profile));
        let (name, profile) = profiles
            .into_iter()
            .next()
            .expect("provider family candidate cannot be empty");
        selected.push(SelectedProfile {
            source_name: name.clone(),
            family,
            profile: profile.clone(),
        });
    }
    Ok(selected)
}

fn eval_router(selected: &SelectedProfile) -> (LLMRouterConfig, String) {
    let eval_name = format!("provider-replay-live-{}", safe_slug(&selected.family));
    let mut profile = selected.profile.clone();
    let metadata = profile.metadata.get_or_insert_with(HashMap::new);
    for output_constraint in [
        "format",
        "response_format",
        "json_schema",
        "output_config",
        "thinking",
        "verbosity",
        "streaming",
    ] {
        metadata.remove(output_constraint);
    }
    metadata.insert("tool_choice".to_string(), Value::String("auto".to_string()));
    profile.max_output_tokens = Some(64);
    profile.reasoning = None;
    // The lane evaluates provider replay, not logical-context adapters. A
    // selected local background profile may be chunk-enabled for its owning
    // operation; carrying that policy into an ordinary chat call would test a
    // different entrypoint and can fail before provider replay is exercised.
    profile.chunking = None;
    profile.timeout_secs = Some(profile.timeout_secs.unwrap_or(120).min(120));

    let mut router = LLMRouterConfig::default();
    router.default_profile = eval_name.clone();
    router.profiles.insert(eval_name.clone(), profile);
    router.operation_mapping.insert(
        "chat_completion".to_string(),
        OperationProfileSelector::Simple(eval_name.clone()),
    );
    (router, eval_name)
}

fn synthetic_provider_state(family: &str) -> Option<AssistantProviderState> {
    match family {
        "openai_responses" => Some(AssistantProviderState::OpenaiResponses {
            response_id: "resp_poisoned_eval_anchor".to_string(),
            tool_protocol_repair_checkpoint: false,
        }),
        "gemini" => Some(AssistantProviderState::Gemini {
            parts: vec![json!({"functionCall": {"name": "provider_replay_lookup"}})],
        }),
        "anthropic" | "minimax" | "deepseek" => Some(AssistantProviderState::AnthropicMessages {
            content: vec![json!({"type": "tool_use", "id": ORPHAN_CALL_ID})],
        }),
        _ => None,
    }
}

fn interrupted_history(family: &str) -> Vec<ChatLlmTranscriptEntry> {
    vec![
        ChatLlmTranscriptEntry::UserText {
            text: "Run the synthetic provider replay lookup.".to_string(),
        },
        ChatLlmTranscriptEntry::AssistantTurn {
            text: Some("The first lookup completed; a second lookup was interrupted.".to_string()),
            tool_calls: vec![
                StoredToolCall {
                    id: COMPLETE_CALL_ID.to_string(),
                    name: "provider_replay_lookup".to_string(),
                    arguments: json!({"record": "complete"}),
                },
                StoredToolCall {
                    id: ORPHAN_CALL_ID.to_string(),
                    name: "provider_replay_lookup".to_string(),
                    arguments: json!({"record": "interrupted"}),
                },
            ],
            provider_state: synthetic_provider_state(family),
        },
        ChatLlmTranscriptEntry::ToolResult {
            tool_call_id: COMPLETE_CALL_ID.to_string(),
            tool_name: Some("provider_replay_lookup".to_string()),
            content: json!({"status": "ok", "marker": "complete-pair-retained"}).to_string(),
        },
        ChatLlmTranscriptEntry::UserText {
            text: format!("The transport check is complete. Reply exactly {FIRST_SENTINEL}."),
        },
    ]
}

fn system_prompt(sentinel: &str) -> String {
    format!(
        "This is a bounded provider transport conformance check. A synthetic tool result in history is already complete. Do not request any tool. Reply with exactly {sentinel} and no other text."
    )
}

fn conformance_tool() -> LLMToolSpec {
    LLMToolSpec {
        name: "provider_replay_lookup".to_string(),
        description: "Synthetic no-side-effect provider replay fixture; do not call it for the final conformance response.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": { "record": { "type": "string" } },
            "required": ["record"],
            "additionalProperties": false,
        }),
    }
}

async fn generate_conformance_response(
    service: &ChatLlmService,
    prompt: &str,
    history: &[ChatLlmTranscriptEntry],
    profile: &str,
    native_tool_replay: bool,
) -> Result<magician::magician_v2::chat::llm_service::ChatLlmResponse> {
    if native_tool_replay {
        service
            .generate_response_with_tools(
                prompt,
                history,
                vec![conformance_tool()],
                None,
                Some(profile),
            )
            .await
    } else {
        // Flattened replay providers never receive a native historical call or
        // result. Some local profiles advertise tool capability only for the
        // config invariant while their physical endpoint intentionally rejects
        // tool-bearing requests, so the correct production contract here is a
        // tool-free final response over the paired flattened evidence.
        service
            .generate_response(prompt, history, None, Some(profile))
            .await
    }
}

fn validate_initial_audit(audit: &ProviderReplayRequestAudit) -> Result<()> {
    anyhow::ensure!(audit.repaired, "production request was not marked repaired");
    anyhow::ensure!(
        audit.checkpoint_required,
        "repair checkpoint was not required"
    );
    anyhow::ensure!(
        audit.removed_tool_calls == 1,
        "expected one removed orphan call"
    );
    anyhow::ensure!(
        audit.removed_tool_results == 0,
        "unexpected tool result removal"
    );
    anyhow::ensure!(
        audit.retained_provider_state_count == 0,
        "provider-native state survived the interrupted turn"
    );
    anyhow::ensure!(
        audit.retained_tool_call_ids == [COMPLETE_CALL_ID],
        "complete call was not retained exactly once"
    );
    anyhow::ensure!(
        audit.retained_tool_result_ids == [COMPLETE_CALL_ID],
        "complete result was not retained exactly once"
    );
    anyhow::ensure!(
        !audit
            .retained_tool_call_ids
            .iter()
            .any(|id| id == ORPHAN_CALL_ID),
        "orphan call survived repair"
    );
    anyhow::ensure!(
        audit.previous_response_id.is_none(),
        "poisoned native continuation anchor survived repair"
    );
    // A repaired Gemini turn cannot reuse its invalidated thought signature.
    // Production intentionally flattens both sides together in that case;
    // the real provider acceptance call below is the conformance gate.
    if audit.native_tool_replay && audit.replay_protocol != "gemini" {
        anyhow::ensure!(
            audit.rendered_native_tool_call_ids == [COMPLETE_CALL_ID],
            "native replay did not contain the complete call exactly once"
        );
        anyhow::ensure!(
            audit.rendered_native_tool_result_ids == [COMPLETE_CALL_ID],
            "native replay did not contain the complete result exactly once"
        );
    }
    Ok(())
}

fn usage_summary(
    usage: Option<&magician::magician_v2::query_analysis::multi_llm_service::LLMUsage>,
) -> Option<UsageSummary> {
    usage.map(|usage| UsageSummary {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_creation_tokens: usage.cache_creation_tokens,
    })
}

async fn run_provider(selected: &SelectedProfile) -> ProviderReplayRow {
    let provider = selected.profile.provider.to_string();
    let api_mode = selected
        .profile
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("openai_api_mode"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let mut row = ProviderReplayRow {
        family: selected.family.clone(),
        provider,
        api_mode,
        profile: selected.source_name.clone(),
        model: selected.profile.model.clone(),
        credential_env: selected.profile.api_key_env.clone(),
        status: "failed".to_string(),
        latency_ms: None,
        continuation_latency_ms: None,
        audit: None,
        continuation_anchor_reused: None,
        newer_interruption_invalidated_anchor: None,
        usage: None,
        error: None,
    };

    if let Some(env_name) = selected.profile.api_key_env.as_deref() {
        if std::env::var(env_name).map_or(true, |value| value.trim().is_empty()) {
            row.error = Some(format!(
                "configured credential environment variable {env_name} is missing"
            ));
            return row;
        }
    }

    let outcome = async {
        let (router, eval_profile) = eval_router(selected);
        let service = ChatLlmService::new(Arc::new(MultiLLMService::from_router_config(&router)));
        let mut history = interrupted_history(&selected.family);
        let audit = service
            .audit_provider_replay_request(
                &system_prompt(FIRST_SENTINEL),
                &history,
                Some(&eval_profile),
            )
            .await;
        validate_initial_audit(&audit)?;

        let started = Instant::now();
        let first = generate_conformance_response(
            &service,
            &system_prompt(FIRST_SENTINEL),
            &history,
            &eval_profile,
            audit.native_tool_replay,
        )
        .await
        .context("provider rejected repaired interrupted history")?;
        let first_latency_ms = started.elapsed().as_millis();
        let first_text = first.content.as_deref().unwrap_or("").trim();
        anyhow::ensure!(
            first_text.contains(FIRST_SENTINEL),
            "provider response omitted conformance sentinel"
        );
        anyhow::ensure!(
            first.tool_calls.is_empty(),
            "provider requested a tool despite the bounded final-response instruction"
        );

        let mut continuation_latency_ms = None;
        let mut continuation_anchor_reused = None;
        let mut newer_interruption_invalidated_anchor = None;
        if selected.family == "openai_responses" {
            let checkpoint_id = match first.provider_state.clone() {
                Some(AssistantProviderState::OpenaiResponses {
                    response_id,
                    tool_protocol_repair_checkpoint: true,
                }) if !response_id.trim().is_empty() => response_id,
                _ => bail!("OpenAI Responses repair did not produce a durable clean checkpoint"),
            };
            history.push(ChatLlmTranscriptEntry::AssistantTurn {
                text: first.content.clone(),
                tool_calls: Vec::new(),
                provider_state: first.provider_state.clone(),
            });
            history = serde_json::from_value(serde_json::to_value(&history)?)
                .context("round-tripping persisted provider replay transcript")?;
            history.push(ChatLlmTranscriptEntry::UserText {
                text: format!(
                    "Continue from the clean checkpoint. Reply exactly {CONTINUATION_SENTINEL}."
                ),
            });
            let continuation_audit = service
                .audit_provider_replay_request(
                    &system_prompt(CONTINUATION_SENTINEL),
                    &history,
                    Some(&eval_profile),
                )
                .await;
            let anchor_reused = continuation_audit.previous_response_id.as_deref()
                == Some(checkpoint_id.as_str())
                && !continuation_audit.checkpoint_required;
            anyhow::ensure!(
                anchor_reused,
                "persisted clean checkpoint was not selected as the continuation anchor"
            );
            continuation_anchor_reused = Some(true);

            let continuation_started = Instant::now();
            let continued = generate_conformance_response(
                &service,
                &system_prompt(CONTINUATION_SENTINEL),
                &history,
                &eval_profile,
                continuation_audit.native_tool_replay,
            )
            .await
            .context("OpenAI Responses rejected the clean checkpoint continuation")?;
            continuation_latency_ms = Some(continuation_started.elapsed().as_millis());
            anyhow::ensure!(
                continued
                    .content
                    .as_deref()
                    .unwrap_or("")
                    .contains(CONTINUATION_SENTINEL),
                "checkpoint continuation omitted conformance sentinel"
            );

            history.push(ChatLlmTranscriptEntry::AssistantTurn {
                text: continued.content,
                tool_calls: continued
                    .tool_calls
                    .iter()
                    .map(StoredToolCall::from_llm_tool_call)
                    .collect(),
                provider_state: continued.provider_state,
            });
            history.push(ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("A newer synthetic request was interrupted.".to_string()),
                tool_calls: vec![StoredToolCall {
                    id: NEW_ORPHAN_CALL_ID.to_string(),
                    name: "provider_replay_lookup".to_string(),
                    arguments: json!({"record": "new-interruption"}),
                }],
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "resp_newer_poisoned_eval_anchor".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            });
            history.push(ChatLlmTranscriptEntry::UserText {
                text: "Verify that the newer interruption invalidates native continuation."
                    .to_string(),
            });
            let invalidation_audit = service
                .audit_provider_replay_request(
                    &system_prompt(FIRST_SENTINEL),
                    &history,
                    Some(&eval_profile),
                )
                .await;
            let invalidated = invalidation_audit.previous_response_id.is_none()
                && invalidation_audit.checkpoint_required
                && invalidation_audit
                    .removed_tool_calls
                    .saturating_sub(audit.removed_tool_calls)
                    >= 1;
            anyhow::ensure!(
                invalidated,
                "newer interruption did not invalidate provider continuation state"
            );
            newer_interruption_invalidated_anchor = Some(true);
        }

        Ok::<_, anyhow::Error>((
            audit,
            first_latency_ms,
            continuation_latency_ms,
            continuation_anchor_reused,
            newer_interruption_invalidated_anchor,
            usage_summary(first.usage.as_ref()),
        ))
    }
    .await;

    match outcome {
        Ok((audit, latency, continuation_latency, anchor_reused, invalidated, usage)) => {
            row.status = "passed".to_string();
            row.latency_ms = Some(latency);
            row.continuation_latency_ms = continuation_latency;
            row.audit = Some(audit);
            row.continuation_anchor_reused = anchor_reused;
            row.newer_interruption_invalidated_anchor = invalidated;
            row.usage = usage;
        },
        Err(error) => row.error = Some(format!("{error:#}")),
    }
    row
}

fn planned_row(selected: &SelectedProfile) -> ProviderReplayRow {
    ProviderReplayRow {
        family: selected.family.clone(),
        provider: selected.profile.provider.to_string(),
        api_mode: selected
            .profile
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("openai_api_mode"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        profile: selected.source_name.clone(),
        model: selected.profile.model.clone(),
        credential_env: selected.profile.api_key_env.clone(),
        status: "planned".to_string(),
        latency_ms: None,
        continuation_latency_ms: None,
        audit: None,
        continuation_anchor_reused: None,
        newer_interruption_invalidated_anchor: None,
        usage: None,
        error: None,
    }
}

fn configured_families(router: &LLMRouterConfig) -> BTreeSet<String> {
    router
        .profiles
        .values()
        .filter(|profile| is_tool_capable(profile))
        .map(provider_family)
        .collect()
}

fn build_report(
    router: &LLMRouterConfig,
    selected: &[SelectedProfile],
    results: Vec<ProviderReplayRow>,
    dry_run: bool,
) -> EvalReport {
    let configured = configured_families(router);
    let selected_by_family: BTreeMap<&str, &str> = selected
        .iter()
        .map(|profile| (profile.family.as_str(), profile.source_name.as_str()))
        .collect();
    let result_status: BTreeMap<&str, &str> = results
        .iter()
        .map(|result| (result.family.as_str(), result.status.as_str()))
        .collect();
    let all_families: BTreeSet<String> = SUPPORTED_FAMILIES
        .iter()
        .map(|family| (*family).to_string())
        .chain(configured.iter().cloned())
        .collect();
    let family_coverage = all_families
        .into_iter()
        .map(|family| {
            let is_configured = configured.contains(&family);
            FamilyCoverage {
                selected_profile: selected_by_family
                    .get(family.as_str())
                    .map(|value| (*value).to_string()),
                status: if !is_configured {
                    "not_configured".to_string()
                } else {
                    result_status
                        .get(family.as_str())
                        .copied()
                        .unwrap_or("not_selected")
                        .to_string()
                },
                family,
                configured: is_configured,
            }
        })
        .collect::<Vec<_>>();
    let passed = results
        .iter()
        .filter(|result| result.status == "passed")
        .count();
    let failed = results
        .iter()
        .filter(|result| result.status == "failed")
        .count();
    EvalReport {
        schema_version: SCHEMA_VERSION,
        generated_at: Utc::now(),
        mode: if dry_run { "dry_run" } else { "live" }.to_string(),
        status: if dry_run {
            "planned"
        } else if failed == 0 {
            "passed"
        } else {
            "failed"
        }
        .to_string(),
        gate: "Every configured tool-capable provider family must accept production-repaired interrupted history; OpenAI Responses must additionally persist and reuse a clean native continuation checkpoint.".to_string(),
        configured_family_count: configured.len(),
        exercised_family_count: results.len(),
        passed_family_count: passed,
        failed_family_count: failed,
        family_coverage,
        results,
    }
}

fn self_test_report() -> EvalReport {
    EvalReport {
        schema_version: SCHEMA_VERSION,
        generated_at: Utc::now(),
        mode: "self_test".to_string(),
        status: "passed".to_string(),
        gate: "Provider-free report contract smoke test".to_string(),
        configured_family_count: 2,
        exercised_family_count: 2,
        passed_family_count: 2,
        failed_family_count: 0,
        family_coverage: vec![
            FamilyCoverage {
                family: "openai_responses".to_string(),
                configured: true,
                selected_profile: Some("self-test-openai".to_string()),
                status: "passed".to_string(),
            },
            FamilyCoverage {
                family: "anthropic".to_string(),
                configured: true,
                selected_profile: Some("self-test-anthropic".to_string()),
                status: "passed".to_string(),
            },
            FamilyCoverage {
                family: "gemini".to_string(),
                configured: false,
                selected_profile: None,
                status: "not_configured".to_string(),
            },
        ],
        results: vec![
            ProviderReplayRow {
                family: "openai_responses".to_string(),
                provider: "openai".to_string(),
                api_mode: Some("responses".to_string()),
                profile: "self-test-openai".to_string(),
                model: "fixture".to_string(),
                credential_env: Some("OPENAI_API_KEY".to_string()),
                status: "passed".to_string(),
                latency_ms: Some(1),
                continuation_latency_ms: Some(1),
                audit: None,
                continuation_anchor_reused: Some(true),
                newer_interruption_invalidated_anchor: Some(true),
                usage: None,
                error: None,
            },
            ProviderReplayRow {
                family: "anthropic".to_string(),
                provider: "anthropic".to_string(),
                api_mode: None,
                profile: "self-test-anthropic".to_string(),
                model: "fixture".to_string(),
                credential_env: Some("ANTHROPIC_API_KEY".to_string()),
                status: "passed".to_string(),
                latency_ms: Some(1),
                continuation_latency_ms: None,
                audit: None,
                continuation_anchor_reused: None,
                newer_interruption_invalidated_anchor: None,
                usage: None,
                error: None,
            },
        ],
    }
}

fn write_report(output_dir: &Path, report: &EvalReport) -> Result<()> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("creating report directory {}", output_dir.display()))?;
    let json_path = output_dir.join("report.json");
    std::fs::write(&json_path, serde_json::to_vec_pretty(report)?)
        .with_context(|| format!("writing {}", json_path.display()))?;
    let jsonl_path = output_dir.join("results.jsonl");
    let mut jsonl = String::new();
    for result in &report.results {
        jsonl.push_str(&serde_json::to_string(result)?);
        jsonl.push('\n');
    }
    std::fs::write(&jsonl_path, jsonl)
        .with_context(|| format!("writing {}", jsonl_path.display()))?;
    let html_path = output_dir.join("report.html");
    std::fs::write(&html_path, render_html(report))
        .with_context(|| format!("writing {}", html_path.display()))?;
    Ok(())
}

fn render_html(report: &EvalReport) -> String {
    let coverage_rows = report
        .family_coverage
        .iter()
        .map(|coverage| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"{}\">{}</td></tr>",
                html_escape(&coverage.family),
                if coverage.configured { "yes" } else { "no" },
                html_escape(coverage.selected_profile.as_deref().unwrap_or("—")),
                html_escape(&coverage.status),
                html_escape(&coverage.status),
            )
        })
        .collect::<String>();
    let result_rows = report
        .results
        .iter()
        .map(|result| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"{}\">{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&result.family),
                html_escape(&result.provider),
                html_escape(&result.profile),
                html_escape(&result.model),
                html_escape(&result.status),
                html_escape(&result.status),
                result.latency_ms.map_or_else(|| "—".to_string(), |value| value.to_string()),
                html_escape(result.error.as_deref().unwrap_or("—")),
            )
        })
        .collect::<String>();
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Provider replay live eval</title><style>body{{font:15px system-ui;margin:32px;color:#18212f;background:#f6f8fb}}main{{max-width:1180px;margin:auto}}.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:12px}}.card,section{{background:white;border:1px solid #d9e0ea;border-radius:12px;padding:16px;margin:16px 0}}.value{{font-size:28px;font-weight:700}}table{{width:100%;border-collapse:collapse}}th,td{{padding:10px;border-bottom:1px solid #e6eaf0;text-align:left;vertical-align:top}}.passed{{color:#08783e;font-weight:700}}.failed{{color:#b42318;font-weight:700}}.planned,.not_configured{{color:#667085}}code{{word-break:break-word}}</style></head><body><main><h1>Provider replay live eval</h1><p>{}</p><div class=\"cards\"><div class=\"card\"><div>Status</div><div class=\"value {}\">{}</div></div><div class=\"card\"><div>Configured families</div><div class=\"value\">{}</div></div><div class=\"card\"><div>Exercised</div><div class=\"value\">{}</div></div><div class=\"card\"><div>Passed / failed</div><div class=\"value\">{} / {}</div></div></div><section><h2>Provider-family coverage</h2><table><thead><tr><th>Family</th><th>Configured</th><th>Selected profile</th><th>Status</th></tr></thead><tbody>{}</tbody></table></section><section><h2>Live results</h2><table><thead><tr><th>Family</th><th>Provider</th><th>Profile</th><th>Model</th><th>Status</th><th>Latency ms</th><th>Error</th></tr></thead><tbody>{}</tbody></table><p><a href=\"report.json\">Full JSON report</a> · <a href=\"results.jsonl\">JSONL rows</a></p></section></main></body></html>",
        html_escape(&report.gate),
        html_escape(&report.status),
        html_escape(&report.status),
        report.configured_family_count,
        report.exercised_family_count,
        report.passed_family_count,
        report.failed_family_count,
        coverage_rows,
        result_rows,
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn safe_slug(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(provider: LLMProviderKind, model: &str, mode: Option<&str>) -> LLMProfile {
        let mut metadata = HashMap::new();
        metadata.insert("tool_choice".to_string(), Value::String("auto".to_string()));
        if let Some(mode) = mode {
            metadata.insert(
                "openai_api_mode".to_string(),
                Value::String(mode.to_string()),
            );
        }
        LLMProfile {
            provider,
            model: model.to_string(),
            api_key_env: None,
            api_base_url: None,
            temperature: None,
            max_output_tokens: Some(64),
            context_window_tokens: None,
            chunking: None,
            default_modality: None,
            reasoning: None,
            metadata: Some(metadata),
            supports_vision: None,
            supports_reasoning: None,
            supports_tool_calling: Some(true),
            supports_computer_use: None,
            timeout_secs: Some(30),
        }
    }

    #[test]
    fn provider_family_separates_openai_transports_and_other_providers() {
        assert_eq!(
            provider_family(&profile(LLMProviderKind::OpenAI, "gpt", Some("responses"))),
            "openai_responses"
        );
        assert_eq!(
            provider_family(&profile(LLMProviderKind::OpenAI, "gpt", Some("chat"))),
            "openai_chat"
        );
        assert_eq!(
            provider_family(&profile(LLMProviderKind::Anthropic, "claude", None)),
            "anthropic"
        );
        assert_eq!(
            provider_family(&profile(LLMProviderKind::Gemini, "gemini", None)),
            "gemini"
        );
    }

    #[test]
    fn automatic_selection_keeps_one_profile_per_configured_family() {
        let mut router = LLMRouterConfig::default();
        router.profiles.insert(
            "chat-openai".to_string(),
            profile(LLMProviderKind::OpenAI, "gpt-mini", Some("responses")),
        );
        router.profiles.insert(
            "other-openai".to_string(),
            profile(LLMProviderKind::OpenAI, "gpt-large", Some("responses")),
        );
        router.profiles.insert(
            "chat-anthropic".to_string(),
            profile(LLMProviderKind::Anthropic, "haiku", None),
        );
        let selected = select_profiles(&router, &[]).expect("select profiles");
        assert_eq!(selected.len(), 2);
        assert_eq!(
            selected
                .iter()
                .map(|item| item.family.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["anthropic", "openai_responses"])
        );
        assert!(selected
            .iter()
            .any(|item| item.source_name == "chat-openai"));
    }

    #[test]
    fn interrupted_fixture_contains_one_complete_pair_and_one_orphan() {
        let history = interrupted_history("anthropic");
        let calls = history
            .iter()
            .filter_map(|entry| match entry {
                ChatLlmTranscriptEntry::AssistantTurn { tool_calls, .. } => Some(tool_calls.len()),
                _ => None,
            })
            .sum::<usize>();
        let results = history
            .iter()
            .filter(|entry| matches!(entry, ChatLlmTranscriptEntry::ToolResult { .. }))
            .count();
        assert_eq!(calls, 2);
        assert_eq!(results, 1);
    }

    #[test]
    fn html_report_links_machine_readable_outputs_and_escapes_errors() {
        let mut report = self_test_report();
        report.results[0].error = Some("<private & broken>".to_string());
        let html = render_html(&report);
        assert!(html.contains("report.json"));
        assert!(html.contains("results.jsonl"));
        assert!(html.contains("&lt;private &amp; broken&gt;"));
        assert!(!html.contains("<private & broken>"));
    }
}
