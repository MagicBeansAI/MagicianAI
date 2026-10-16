use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use magician::{
    config::{load_default_magician_config, load_magician_config_from_path},
    magician_v2::{
        agents::{
            configure_memory_prompt_budgets, expand_memory_retrieval_query,
            render_memory_tiers_for_prompt_with_scores_result, score_hybrid_index_for_prompt,
            AgentDefinitionStore, AgentMemoryResolver, MemoryRenderRequest, MemoryTierDefinition,
        },
        artifact_v2::workspace::{default_storage_base_path, ArtifactV2Workspace},
        chat::retrieval_timing::{
            measure_chat_context_retrieval, summarize_latencies, ChatContextRetrievalTiming,
            LatencySummary,
        },
        learning::{
            render_active_procedures_for_prompt_with_hybrid, start_procedure_index_maintainer,
            LearningProcedurePromptRetrievalBackend, LearningProcedureRenderRequest, LearningScope,
            LearningStore,
        },
        workspace_storage_settings::WorkspaceStorageSettingsStore,
    },
};
use magician_vector_index::{
    embedding_admission_stats, memory_index::eval_only_run_memory_index_write_embedding_pipeline,
    query_embedding_coalescer_stats, OllamaEmbedder,
};
use serde::Serialize;
use tokio::sync::oneshot;

#[derive(Debug, Parser)]
#[command(about = "Measure the real chat memory/procedure retrieval critical path")]
struct Args {
    #[arg(long, default_value = "anonymous")]
    principal: String,
    #[arg(long, default_value = "default")]
    workspace: String,
    #[arg(long, default_value = "personal-assistant")]
    agent: String,
    #[arg(
        long,
        default_value = "What should I follow up on next, and what prior procedure should I use?"
    )]
    query: String,
    #[arg(long, default_value_t = 10)]
    runs: usize,
    #[arg(long, default_value_t = 2)]
    warmups: usize,
    #[arg(long, default_value_t = 20)]
    procedure_index_wait_secs: u64,
    /// Fail unless every measured memory/procedure branch uses LanceDB hybrid
    /// retrieval and the procedure index reports ready.
    #[arg(long)]
    require_hybrid: bool,
    /// Fail unless concurrent memory/procedure retrieval shares at least one
    /// query embedding per turn and issues at most one provider call per turn.
    #[arg(long)]
    require_coalescing: bool,
    /// Exercise foreground hybrid retrieval while optional background
    /// embeddings occupy the same physical daemon.
    #[arg(long)]
    require_background_priority: bool,
    #[arg(long, default_value_t = 3)]
    background_contention_runs: usize,
    #[arg(long, default_value_t = 4)]
    background_embedding_inputs: usize,
    #[arg(long, default_value_t = 5_000.0)]
    max_background_contention_p95_ms: f64,
    #[arg(long)]
    max_concurrent_wall_p50_ms: Option<f64>,
    #[arg(long)]
    max_concurrent_wall_p95_ms: Option<f64>,
    /// Override the active runtime workspace root. By default this uses the
    /// same workspace-storage resolution as the Magician service.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Use an explicit Magician config for this eval and its Ollama contract.
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(
        long,
        default_value = "coverage/evals/chat-context-retrieval/latest.json"
    )]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct EvalReport {
    generated_at: DateTime<Utc>,
    runtime_root: String,
    principal: String,
    workspace: String,
    agent: String,
    query: String,
    embedding_endpoint: String,
    embedding_model: String,
    runs: usize,
    warmups: usize,
    procedure_index_ready: bool,
    procedure_index_wait_ms: f64,
    query_embedding_provider_calls: u64,
    query_embedding_coalesced_reuses: u64,
    summaries: BTreeMap<String, LatencySummary>,
    memory_backend_counts: BTreeMap<String, usize>,
    procedure_backend_counts: BTreeMap<String, usize>,
    background_contention: Option<BackgroundContentionReport>,
    samples: Vec<EvalSample>,
}

#[derive(Debug, Serialize)]
struct BackgroundContentionReport {
    optional_background: ContentionLaneReport,
    memory_index_write: ContentionLaneReport,
}

#[derive(Debug, Serialize)]
struct ContentionLaneReport {
    lane: &'static str,
    requested_inputs: usize,
    completed_inputs: usize,
    configured_physical_token_ceiling: usize,
    largest_fixture_bytes: usize,
    background_duration_ms: f64,
    foreground_runs: usize,
    proven_contention_runs: usize,
    foreground_query_embedding_provider_calls: u64,
    foreground_query_embedding_coalesced_reuses: u64,
    foreground_summaries: BTreeMap<String, LatencySummary>,
    foreground_memory_backend_counts: BTreeMap<String, usize>,
    foreground_procedure_backend_counts: BTreeMap<String, usize>,
    background_error: Option<String>,
    foreground_error: Option<String>,
    foreground_samples: Vec<EvalSample>,
}

#[derive(Debug, Serialize)]
struct EvalSample {
    run: usize,
    memory_index_query_ms: f64,
    user_memory_render_ms: f64,
    user_candidate_load_ms: f64,
    user_temperature_overlay_ms: f64,
    user_hot_projection_ms: f64,
    user_rank_select_render_ms: f64,
    agent_memory_render_ms: f64,
    agent_candidate_load_ms: f64,
    agent_temperature_overlay_ms: f64,
    agent_hot_projection_ms: f64,
    agent_rank_select_render_ms: f64,
    memory_total_ms: f64,
    procedures_ms: f64,
    concurrent_wall_ms: f64,
    overlap_saved_ms: f64,
    user_memory_backend: String,
    agent_memory_backend: String,
    procedure_backend: String,
    user_memory_selected: usize,
    agent_memory_selected: usize,
    procedures_selected: usize,
    procedure_candidates: usize,
}

struct MemorySample {
    index_query_ms: f64,
    user_render_ms: f64,
    user_candidate_load_ms: f64,
    user_temperature_overlay_ms: f64,
    user_hot_projection_ms: f64,
    user_rank_select_render_ms: f64,
    agent_render_ms: f64,
    agent_candidate_load_ms: f64,
    agent_temperature_overlay_ms: f64,
    agent_hot_projection_ms: f64,
    agent_rank_select_render_ms: f64,
    user_backend: String,
    agent_backend: String,
    user_selected: usize,
    agent_selected: usize,
}

struct ProcedureSample {
    backend: LearningProcedurePromptRetrievalBackend,
    selected: usize,
    candidates: usize,
}

#[derive(Debug, Clone, Copy)]
enum ContentionLane {
    OptionalBackground,
    MemoryIndexWrite,
}

impl ContentionLane {
    fn label(self) -> &'static str {
        match self {
            Self::OptionalBackground => "optional_background",
            Self::MemoryIndexWrite => "memory_index_write",
        }
    }

    fn fixture_prefix(self) -> &'static str {
        match self {
            Self::OptionalBackground => "optional-resurfacing",
            Self::MemoryIndexWrite => "memory-index-write",
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .compact()
        .init();
    let args = Args::parse();
    anyhow::ensure!(args.runs > 0, "--runs must be greater than zero");
    if args.require_background_priority {
        anyhow::ensure!(
            args.background_contention_runs > 0,
            "--background-contention-runs must be greater than zero"
        );
        anyhow::ensure!(
            args.background_embedding_inputs > 0,
            "--background-embedding-inputs must be greater than zero"
        );
        anyhow::ensure!(
            args.max_background_contention_p95_ms.is_finite()
                && args.max_background_contention_p95_ms > 0.0,
            "--max-background-contention-p95-ms must be positive"
        );
    }
    for (name, value) in [
        (
            "--max-concurrent-wall-p50-ms",
            args.max_concurrent_wall_p50_ms,
        ),
        (
            "--max-concurrent-wall-p95-ms",
            args.max_concurrent_wall_p95_ms,
        ),
    ] {
        if let Some(value) = value {
            anyhow::ensure!(value.is_finite() && value > 0.0, "{name} must be positive");
        }
    }

    let config = match args.config.as_deref() {
        Some(path) => load_magician_config_from_path(path)
            .with_context(|| format!("loading magician config {}", path.display()))?,
        None => load_default_magician_config().context("loading active magician config")?,
    };
    if args.require_background_priority {
        anyhow::ensure!(
            config.runtime.ollama.embedding_num_parallel == 1
                && embedding_admission_stats().installed_capacity == 1,
            "--require-background-priority currently proves single-sequence priority and requires embedding_num_parallel=1"
        );
    }
    configure_memory_prompt_budgets(&config.memory);
    let workspace = resolve_workspace(&args, &config.storage_path)?;
    let scoped_definition_store = AgentDefinitionStore::with_workspace_layout(workspace.clone())
        .for_scope(&args.principal, &args.workspace);
    let definition = scoped_definition_store
        .get_definition(&args.agent)
        .await
        .with_context(|| format!("loading agent definition {}", args.agent))?
        .with_context(|| format!("agent definition {} was not found", args.agent))?
        .definition;
    let memory_service = AgentMemoryResolver::with_workspace_layout(workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .context("resolving scoped memory service")?;
    let learning_store = LearningStore::new(workspace.clone());
    let learning_scope = LearningScope::new(&args.principal, &args.workspace);
    let relevance_query = format!(
        "{}\nagent: {}\nchat_session: retrieval-eval",
        args.query, args.agent
    );
    if args.require_coalescing || args.require_background_priority {
        anyhow::ensure!(
            expand_memory_retrieval_query(&relevance_query) == relevance_query,
            "coalescing gates require a query that is identical for memory and procedure retrieval; this custom query activates deterministic memory-only synonym expansion"
        );
    }

    start_procedure_index_maintainer();
    let (procedure_index_ready, procedure_index_wait_ms) = wait_for_procedure_index(
        &learning_store,
        &learning_scope,
        &args.agent,
        &relevance_query,
        Duration::from_secs(args.procedure_index_wait_secs),
    )
    .await?;

    for _ in 0..args.warmups {
        run_sample(
            &memory_service,
            &scoped_definition_store,
            &definition.memory_tiers,
            &learning_store,
            &learning_scope,
            &args.agent,
            &relevance_query,
            0,
        )
        .await?;
    }

    let coalescer_before = query_embedding_coalescer_stats();
    let mut samples = Vec::with_capacity(args.runs);
    for run in 1..=args.runs {
        samples.push(
            run_sample(
                &memory_service,
                &scoped_definition_store,
                &definition.memory_tiers,
                &learning_store,
                &learning_scope,
                &args.agent,
                &relevance_query,
                run,
            )
            .await?,
        );
    }
    let coalescer_delta = query_embedding_coalescer_stats().saturating_delta(coalescer_before);
    let background_contention = if args.require_background_priority {
        let configured_physical_token_ceiling = config
            .runtime
            .ollama
            .embedding_context_tokens
            .min(config.runtime.ollama.embedding_batch_tokens)
            as usize;
        Some(
            run_background_contention_eval(
                &args,
                &memory_service,
                &scoped_definition_store,
                &definition.memory_tiers,
                &learning_store,
                &learning_scope,
                &relevance_query,
                configured_physical_token_ceiling,
            )
            .await?,
        )
    } else {
        None
    };

    let report = EvalReport {
        generated_at: Utc::now(),
        runtime_root: workspace.base_root().display().to_string(),
        principal: args.principal.clone(),
        workspace: args.workspace.clone(),
        agent: args.agent.clone(),
        query: args.query.clone(),
        embedding_endpoint: config.runtime.ollama.embedding_base_url,
        embedding_model: config.runtime.ollama.embedding_model,
        runs: args.runs,
        warmups: args.warmups,
        procedure_index_ready,
        procedure_index_wait_ms,
        query_embedding_provider_calls: coalescer_delta.provider_calls,
        query_embedding_coalesced_reuses: coalescer_delta.coalesced_reuses,
        summaries: summarize_samples(&samples),
        memory_backend_counts: count_memory_backends(&samples),
        procedure_backend_counts: count_backends(
            samples
                .iter()
                .map(|sample| sample.procedure_backend.as_str()),
        ),
        background_contention,
        samples,
    };
    write_report(&args.output, &report)?;
    print_report(&args.output, &report);
    validate_report(&args, &report)?;
    Ok(())
}

fn validate_report(args: &Args, report: &EvalReport) -> Result<()> {
    let mut failures = Vec::new();
    if args.require_hybrid {
        let expected_memory = report.runs.saturating_mul(2);
        let memory_hybrid = report
            .memory_backend_counts
            .get("lancedb_hybrid")
            .copied()
            .unwrap_or_default();
        let procedure_hybrid = report
            .procedure_backend_counts
            .get("lancedb_hybrid")
            .copied()
            .unwrap_or_default();
        if !report.procedure_index_ready {
            failures.push("procedure index did not become ready".to_string());
        }
        if memory_hybrid != expected_memory || report.memory_backend_counts.len() != 1 {
            failures.push(format!(
                "memory hybrid coverage was {memory_hybrid}/{expected_memory}: {:?}",
                report.memory_backend_counts
            ));
        }
        if procedure_hybrid != report.runs || report.procedure_backend_counts.len() != 1 {
            failures.push(format!(
                "procedure hybrid coverage was {procedure_hybrid}/{}: {:?}",
                report.runs, report.procedure_backend_counts
            ));
        }
    }
    if args.require_coalescing {
        if report.query_embedding_provider_calls > report.runs as u64 {
            failures.push(format!(
                "query embedding provider calls {} exceeded one per measured turn ({})",
                report.query_embedding_provider_calls, report.runs
            ));
        }
        if report.query_embedding_coalesced_reuses < report.runs as u64 {
            failures.push(format!(
                "query embedding coalesced reuses {} were below one per measured turn ({})",
                report.query_embedding_coalesced_reuses, report.runs
            ));
        }
    }
    if let Some(maximum) = args.max_concurrent_wall_p50_ms {
        let actual = report
            .summaries
            .get("concurrent_wall_ms")
            .map(|summary| summary.p50_ms)
            .unwrap_or(f64::INFINITY);
        if actual > maximum {
            failures.push(format!(
                "concurrent wall p50 {actual:.1} ms exceeded {maximum:.1} ms"
            ));
        }
    }
    if let Some(maximum) = args.max_concurrent_wall_p95_ms {
        let actual = report
            .summaries
            .get("concurrent_wall_ms")
            .map(|summary| summary.p95_ms)
            .unwrap_or(f64::INFINITY);
        if actual > maximum {
            failures.push(format!(
                "concurrent wall p95 {actual:.1} ms exceeded {maximum:.1} ms"
            ));
        }
    }
    if args.require_background_priority {
        if let Some(contention) = report.background_contention.as_ref() {
            validate_contention_lane(args, &contention.optional_background, &mut failures);
            validate_contention_lane(args, &contention.memory_index_write, &mut failures);
        } else {
            failures.push("background contention report was missing".to_string());
        }
    }
    anyhow::ensure!(
        failures.is_empty(),
        "chat context retrieval eval failed:\n- {}",
        failures.join("\n- ")
    );
    Ok(())
}

fn validate_contention_lane(args: &Args, lane: &ContentionLaneReport, failures: &mut Vec<String>) {
    if lane.largest_fixture_bytes <= lane.configured_physical_token_ceiling {
        failures.push(format!(
            "{} largest fixture ({} bytes) did not exceed the configured physical token ceiling ({})",
            lane.lane, lane.largest_fixture_bytes, lane.configured_physical_token_ceiling
        ));
    }
    if lane.background_error.is_some() || lane.completed_inputs != lane.requested_inputs {
        failures.push(format!(
            "{} background embedding completed {}/{} inputs: {}",
            lane.lane,
            lane.completed_inputs,
            lane.requested_inputs,
            lane.background_error
                .as_deref()
                .unwrap_or("incomplete without an error")
        ));
    }
    if let Some(error) = lane.foreground_error.as_deref() {
        failures.push(format!(
            "{} foreground retrieval failed: {error}",
            lane.lane
        ));
    }
    if lane.proven_contention_runs != lane.foreground_runs {
        failures.push(format!(
            "{} proved contention for {}/{} foreground runs",
            lane.lane, lane.proven_contention_runs, lane.foreground_runs
        ));
    }
    if lane.foreground_query_embedding_provider_calls != lane.foreground_runs as u64 {
        failures.push(format!(
            "{} foreground provider calls {} did not equal exactly one per turn ({})",
            lane.lane, lane.foreground_query_embedding_provider_calls, lane.foreground_runs
        ));
    }
    if lane.foreground_query_embedding_coalesced_reuses < lane.foreground_runs as u64 {
        failures.push(format!(
            "{} foreground coalesced reuses {} were below one per turn ({})",
            lane.lane, lane.foreground_query_embedding_coalesced_reuses, lane.foreground_runs
        ));
    }
    let expected_memory = lane.foreground_runs.saturating_mul(2);
    let memory_hybrid = lane
        .foreground_memory_backend_counts
        .get("lancedb_hybrid")
        .copied()
        .unwrap_or_default();
    let procedure_hybrid = lane
        .foreground_procedure_backend_counts
        .get("lancedb_hybrid")
        .copied()
        .unwrap_or_default();
    if memory_hybrid != expected_memory || lane.foreground_memory_backend_counts.len() != 1 {
        failures.push(format!(
            "{} memory hybrid coverage was {memory_hybrid}/{expected_memory}: {:?}",
            lane.lane, lane.foreground_memory_backend_counts
        ));
    }
    if procedure_hybrid != lane.foreground_runs
        || lane.foreground_procedure_backend_counts.len() != 1
    {
        failures.push(format!(
            "{} procedure hybrid coverage was {procedure_hybrid}/{}: {:?}",
            lane.lane, lane.foreground_runs, lane.foreground_procedure_backend_counts
        ));
    }
    let p95 = lane
        .foreground_summaries
        .get("concurrent_wall_ms")
        .map(|summary| summary.p95_ms)
        .unwrap_or(f64::INFINITY);
    if p95 > args.max_background_contention_p95_ms {
        failures.push(format!(
            "{} foreground-under-background p95 {p95:.1} ms exceeded {:.1} ms",
            lane.lane, args.max_background_contention_p95_ms
        ));
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_background_contention_eval(
    args: &Args,
    memory_service: &magician::magician_v2::agents::AgentMemoryService,
    definition_store: &AgentDefinitionStore,
    tier_definitions: &[MemoryTierDefinition],
    learning_store: &LearningStore,
    learning_scope: &LearningScope,
    relevance_query: &str,
    configured_physical_token_ceiling: usize,
) -> Result<BackgroundContentionReport> {
    let optional_background = run_contention_lane(
        args,
        memory_service,
        definition_store,
        tier_definitions,
        learning_store,
        learning_scope,
        relevance_query,
        ContentionLane::OptionalBackground,
        configured_physical_token_ceiling,
    )
    .await;

    let memory_index_write = run_contention_lane(
        args,
        memory_service,
        definition_store,
        tier_definitions,
        learning_store,
        learning_scope,
        relevance_query,
        ContentionLane::MemoryIndexWrite,
        configured_physical_token_ceiling,
    )
    .await;

    Ok(BackgroundContentionReport {
        optional_background,
        memory_index_write,
    })
}

fn contention_inputs(lane: &str, count: usize, physical_token_ceiling: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            let mut fixture = format!(
                "{lane} contention fixture {index}: {}",
                "bounded cooperative embedding work ".repeat(8)
            );
            // Exercise real logical fragmentation in every live lane. The
            // production splitter conservatively treats the token ceiling as
            // a byte ceiling (with a special-token reserve), so this fixture
            // deterministically crosses that product boundary.
            if index == 0 {
                let target_bytes = physical_token_ceiling.saturating_add(64);
                while fixture.len() <= target_bytes {
                    fixture.push_str(" complete-tail-evidence");
                }
            }
            fixture
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn run_contention_lane(
    args: &Args,
    memory_service: &magician::magician_v2::agents::AgentMemoryService,
    definition_store: &AgentDefinitionStore,
    tier_definitions: &[MemoryTierDefinition],
    learning_store: &LearningStore,
    learning_scope: &LearningScope,
    relevance_query: &str,
    lane: ContentionLane,
    configured_physical_token_ceiling: usize,
) -> ContentionLaneReport {
    let lane_label = lane.label();
    let background_started = Instant::now();
    let coalescer_before = query_embedding_coalescer_stats();
    let mut foreground_samples = Vec::with_capacity(args.background_contention_runs);
    let mut proven_contention_runs = 0usize;
    let mut completed_inputs = 0usize;
    let mut largest_fixture_bytes = 0usize;
    let mut background_errors = Vec::new();
    let mut foreground_error = None;
    for run in 1..=args.background_contention_runs {
        let inputs = contention_inputs(
            &format!("{}-run-{run}", lane.fixture_prefix()),
            args.background_embedding_inputs,
            configured_physical_token_ceiling,
        );
        largest_fixture_bytes = largest_fixture_bytes.max(
            inputs
                .iter()
                .map(|input| input.len())
                .max()
                .unwrap_or_default(),
        );
        let (acquired_tx, acquired_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let mut background = spawn_contention_background(lane, inputs, acquired_tx, release_rx);
        match tokio::time::timeout(Duration::from_secs(10), acquired_rx).await {
            Ok(Ok(())) => {},
            Ok(Err(_)) => {
                background_errors.push(format!(
                    "run {run}: {lane_label} ended before reporting exact first admission"
                ));
                collect_or_abort_background(lane_label, &mut background, Duration::from_secs(1))
                    .await;
                break;
            },
            Err(_) => {
                background_errors.push(format!(
                    "run {run}: {lane_label} did not acquire exact first admission within 10 seconds"
                ));
                background.abort();
                let _ = background.await;
                break;
            },
        }

        let contention_query = format!(
            "{relevance_query}\ncontention_probe: {lane_label}-foreground-{run}-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let exact_embedder = OllamaEmbedder::from_env();
        let exact_query_text = contention_query.clone();
        let (wait_started_tx, wait_started_rx) = oneshot::channel();
        let (coalesced_reuse_tx, coalesced_reuse_rx) = oneshot::channel();
        let (permit_acquired_tx, mut permit_acquired_rx) = oneshot::channel();
        let mut exact_query = tokio::spawn(async move {
            exact_embedder
                .embed_query_with_contention_probe(
                    &exact_query_text,
                    wait_started_tx,
                    coalesced_reuse_tx,
                    permit_acquired_tx,
                )
                .await
        });
        match tokio::time::timeout(Duration::from_secs(10), wait_started_rx).await {
            Ok(Ok(())) => {},
            Ok(Err(_)) => {
                foreground_error = Some(format!(
                "run {run}: exact foreground query ended before registering its admission waiter"
            ))
            },
            Err(_) => {
                foreground_error = Some(format!(
                "run {run}: exact foreground query did not register admission within 10 seconds"
            ))
            },
        }
        if foreground_error.is_none()
            && !matches!(
                permit_acquired_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            )
        {
            foreground_error = Some(format!(
                "run {run}: exact foreground query acquired while the exact background gate still held capacity"
            ));
        }

        {
            // Keep the foreground future in a strict inner scope. Every error
            // path drops it (and its scheduler waiting/active guard) before we
            // join the background task, so cleanup cannot deadlock behind a
            // stale eval request.
            let foreground = run_sample(
                memory_service,
                definition_store,
                tier_definitions,
                learning_store,
                learning_scope,
                &args.agent,
                &contention_query,
                run,
            );
            tokio::pin!(foreground);
            if foreground_error.is_none() {
                if let Err(error) = wait_for_exact_foreground_coalescer_join(
                    &mut foreground,
                    coalesced_reuse_rx,
                    Duration::from_secs(10),
                )
                .await
                {
                    foreground_error = Some(format!("run {run}: {error:#}"));
                }
            }

            // Always open the exact gate, even after a foreground failure, so
            // the background task can release its provider permit.
            let _ = release_tx.send(());

            if foreground_error.is_none() {
                match tokio::time::timeout(Duration::from_secs(30), &mut permit_acquired_rx).await {
                    Ok(Ok(())) => proven_contention_runs += 1,
                    Ok(Err(_)) => foreground_error = Some(format!(
                        "run {run}: exact foreground query ended without acquiring after background release"
                    )),
                    Err(_) => foreground_error = Some(format!(
                        "run {run}: exact foreground query did not acquire within 30 seconds of background release"
                    )),
                }
            }

            if foreground_error.is_none() {
                match tokio::time::timeout(Duration::from_secs(30), &mut foreground).await {
                    Ok(Ok(sample)) => foreground_samples.push(sample),
                    Ok(Err(error)) => foreground_error = Some(format!("run {run}: {error:#}")),
                    Err(_) => {
                        foreground_error = Some(format!(
                            "run {run}: {lane_label} foreground retrieval did not complete within 30 seconds"
                        ));
                    },
                }
            }
        }

        match join_exact_foreground_query(&mut exact_query).await {
            Ok(vector) if !vector.is_empty() => {},
            Ok(_) => {
                foreground_error = Some(format!(
                    "run {run}: exact foreground query returned an empty vector"
                ))
            },
            Err(error) => {
                foreground_error.get_or_insert_with(|| format!("run {run}: {error:#}"));
            },
        }

        match join_contention_background(lane_label, &mut background).await {
            Ok(completed) => completed_inputs = completed_inputs.saturating_add(completed),
            Err(error) => background_errors.push(format!("run {run}: {error:#}")),
        }
        if foreground_error.is_some() || !background_errors.is_empty() {
            break;
        }
    }
    let coalescer_delta = query_embedding_coalescer_stats().saturating_delta(coalescer_before);
    let background_duration_ms = elapsed_ms(background_started);
    ContentionLaneReport {
        lane: lane_label,
        requested_inputs: args
            .background_embedding_inputs
            .saturating_mul(args.background_contention_runs),
        completed_inputs,
        configured_physical_token_ceiling,
        largest_fixture_bytes,
        background_duration_ms,
        foreground_runs: args.background_contention_runs,
        proven_contention_runs,
        foreground_query_embedding_provider_calls: coalescer_delta.provider_calls,
        foreground_query_embedding_coalesced_reuses: coalescer_delta.coalesced_reuses,
        foreground_summaries: summarize_samples(&foreground_samples),
        foreground_memory_backend_counts: count_memory_backends(&foreground_samples),
        foreground_procedure_backend_counts: count_backends(
            foreground_samples
                .iter()
                .map(|sample| sample.procedure_backend.as_str()),
        ),
        background_error: (!background_errors.is_empty()).then(|| background_errors.join("; ")),
        foreground_error,
        foreground_samples,
    }
}

fn spawn_contention_background(
    lane: ContentionLane,
    inputs: Vec<String>,
    acquired: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<Result<usize>> {
    tokio::spawn(async move {
        match lane {
            ContentionLane::OptionalBackground => OllamaEmbedder::from_env()
                .embed_documents_background_with_admission_gate(&inputs, acquired, release)
                .await
                .map(|vectors| vectors.len()),
            ContentionLane::MemoryIndexWrite => {
                eval_only_run_memory_index_write_embedding_pipeline(&inputs, acquired, release)
                    .await
            },
        }
    })
}

async fn wait_for_exact_foreground_coalescer_join<F>(
    foreground: &mut std::pin::Pin<&mut F>,
    mut exact_reuse_observed: oneshot::Receiver<()>,
    timeout: Duration,
) -> Result<()>
where
    F: std::future::Future<Output = Result<EvalSample>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            anyhow::bail!("foreground retrieval never joined the exact in-flight query embedding");
        }
        tokio::select! {
            result = foreground.as_mut() => {
                match result {
                    Ok(_) => anyhow::bail!("foreground retrieval completed before joining the exact in-flight query embedding"),
                    Err(error) => return Err(error).context("foreground retrieval failed before query coalescing was exercised"),
                }
            },
            observed = &mut exact_reuse_observed => {
                match observed {
                    Ok(()) => return Ok(()),
                    Err(_) => anyhow::bail!("exact query loader ended before foreground retrieval joined its coalescer key"),
                }
            },
            _ = tokio::time::sleep(Duration::from_millis(1).min(deadline.saturating_duration_since(now))) => {},
        }
    }
}

async fn join_exact_foreground_query(
    query: &mut tokio::task::JoinHandle<Result<Vec<f32>>>,
) -> Result<Vec<f32>> {
    match tokio::time::timeout(Duration::from_secs(30), &mut *query).await {
        Ok(Ok(result)) => result.context("exact foreground query embedding failed"),
        Ok(Err(error)) => Err(anyhow::anyhow!(
            "joining exact foreground query task: {error}"
        )),
        Err(_) => {
            query.abort();
            let _ = query.await;
            Err(anyhow::anyhow!(
                "exact foreground query did not complete within 30 seconds"
            ))
        },
    }
}

async fn join_contention_background(
    lane: &str,
    background: &mut tokio::task::JoinHandle<Result<usize>>,
) -> Result<usize> {
    match tokio::time::timeout(Duration::from_secs(120), &mut *background).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(anyhow::anyhow!("joining {lane} task: {error}")),
        Err(_) => {
            background.abort();
            let _ = background.await;
            Err(anyhow::anyhow!(
                "{lane} did not complete within 120 seconds"
            ))
        },
    }
}

async fn collect_or_abort_background(
    lane: &str,
    background: &mut tokio::task::JoinHandle<Result<usize>>,
    timeout: Duration,
) {
    if tokio::time::timeout(timeout, &mut *background)
        .await
        .is_err()
    {
        background.abort();
        let _ = background.await;
        tracing::debug!(
            lane,
            "aborted unfinished contention probe after admission failure"
        );
    }
}

fn resolve_workspace(args: &Args, storage_path: &str) -> Result<ArtifactV2Workspace> {
    if let Some(root) = args.root.as_ref() {
        return Ok(ArtifactV2Workspace::new(root));
    }
    let seed_root = ArtifactV2Workspace::resolve_scoped_root(Path::new(storage_path));
    WorkspaceStorageSettingsStore::new(default_storage_base_path())
        .with_seed_root(seed_root)
        .resolve_workspace_sync()
        .context("resolving active workspace storage")
}

async fn wait_for_procedure_index(
    store: &LearningStore,
    scope: &LearningScope,
    agent: &str,
    query: &str,
    timeout: Duration,
) -> Result<(bool, f64)> {
    let started = Instant::now();
    loop {
        let result = render_procedure(store, scope, agent, query).await?;
        if result.backend == LearningProcedurePromptRetrievalBackend::LancedbHybrid
            || result.candidates == 0
        {
            return Ok((true, elapsed_ms(started)));
        }
        if started.elapsed() >= timeout {
            return Ok((false, elapsed_ms(started)));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_sample(
    memory_service: &magician::magician_v2::agents::AgentMemoryService,
    definition_store: &AgentDefinitionStore,
    tier_definitions: &[MemoryTierDefinition],
    learning_store: &LearningStore,
    learning_scope: &LearningScope,
    agent: &str,
    query: &str,
    run: usize,
) -> Result<EvalSample> {
    let memory = render_memory(
        memory_service,
        definition_store,
        agent,
        tier_definitions,
        query,
    );
    let procedures = render_procedure(learning_store, learning_scope, agent, query);
    let ((memory, procedures), timing) = measure_chat_context_retrieval(memory, procedures).await;
    let memory = memory?;
    let procedures = procedures?;
    Ok(sample_from_parts(run, memory, procedures, timing))
}

async fn render_memory(
    memory_service: &magician::magician_v2::agents::AgentMemoryService,
    definition_store: &AgentDefinitionStore,
    agent: &str,
    tier_definitions: &[MemoryTierDefinition],
    query: &str,
) -> Result<MemorySample> {
    let user_request = MemoryRenderRequest::user(query).with_emit_audit(false);
    let index_query_started = Instant::now();
    let (indexed_scores, retrieval_backend, fallback_error) =
        score_hybrid_index_for_prompt(memory_service, definition_store, agent, &user_request).await;
    let retrieval_backend_label = if fallback_error.is_some() {
        "direct_fallback"
    } else {
        retrieval_backend.as_str()
    };
    let index_query_ms = elapsed_ms(index_query_started);
    let tier_definitions = Arc::new(tier_definitions.to_vec());
    let indexed_scores = indexed_scores.map(Arc::new);
    let mut render_tasks = tokio::task::JoinSet::new();
    {
        let memory_service = memory_service.clone();
        let agent = agent.to_string();
        let query = query.to_string();
        let tier_definitions = Arc::clone(&tier_definitions);
        let indexed_scores = indexed_scores.clone();
        let fallback_error = fallback_error.clone();
        render_tasks.spawn(async move {
            let request = MemoryRenderRequest::user(&query).with_emit_audit(false);
            let started = Instant::now();
            let result = render_memory_tiers_for_prompt_with_scores_result(
                &memory_service,
                &agent,
                &tier_definitions,
                &request,
                indexed_scores.as_deref(),
                retrieval_backend,
                fallback_error.as_deref(),
            )
            .await
            .context("rendering user memory");
            (true, result, elapsed_ms(started))
        });
    }
    {
        let memory_service = memory_service.clone();
        let agent = agent.to_string();
        let query = query.to_string();
        let tier_definitions = Arc::clone(&tier_definitions);
        let indexed_scores = indexed_scores.clone();
        let fallback_error = fallback_error.clone();
        render_tasks.spawn(async move {
            let request = MemoryRenderRequest::agent(&query).with_emit_audit(false);
            let started = Instant::now();
            let result = render_memory_tiers_for_prompt_with_scores_result(
                &memory_service,
                &agent,
                &tier_definitions,
                &request,
                indexed_scores.as_deref(),
                retrieval_backend,
                fallback_error.as_deref(),
            )
            .await
            .context("rendering agent memory");
            (false, result, elapsed_ms(started))
        });
    }

    let mut user_rendered = None;
    let mut agent_rendered = None;
    while let Some(joined) = render_tasks.join_next().await {
        let (is_user, result, render_ms) = joined.context("joining memory render task")?;
        let rendered = (result?, render_ms);
        if is_user {
            user_rendered = Some(rendered);
        } else {
            agent_rendered = Some(rendered);
        }
    }
    let (user, user_render_ms) = user_rendered.context("user memory render task was missing")?;
    let (agent_result, agent_render_ms) =
        agent_rendered.context("agent memory render task was missing")?;

    Ok(MemorySample {
        index_query_ms,
        user_render_ms,
        user_candidate_load_ms: user.timing.candidate_load_ms,
        user_temperature_overlay_ms: user.timing.temperature_overlay_ms,
        user_hot_projection_ms: user.timing.hot_projection_ms,
        user_rank_select_render_ms: user.timing.rank_select_render_ms,
        agent_render_ms,
        agent_candidate_load_ms: agent_result.timing.candidate_load_ms,
        agent_temperature_overlay_ms: agent_result.timing.temperature_overlay_ms,
        agent_hot_projection_ms: agent_result.timing.hot_projection_ms,
        agent_rank_select_render_ms: agent_result.timing.rank_select_render_ms,
        user_backend: retrieval_backend_label.to_string(),
        agent_backend: retrieval_backend_label.to_string(),
        user_selected: user.selected_candidate_keys.len(),
        agent_selected: agent_result.selected_candidate_keys.len(),
    })
}

async fn render_procedure(
    store: &LearningStore,
    scope: &LearningScope,
    agent: &str,
    query: &str,
) -> Result<ProcedureSample> {
    let result = render_active_procedures_for_prompt_with_hybrid(
        store,
        scope,
        &LearningProcedureRenderRequest::for_goal(query)
            .with_agent(Some(agent))
            .with_chat_session(Some("retrieval-eval"))
            .with_emit_audit(false),
    )
    .await
    .context("rendering procedure memory")?;
    Ok(ProcedureSample {
        backend: result.retrieval_backend,
        selected: result.selected.len(),
        candidates: result.candidate_count,
    })
}

fn sample_from_parts(
    run: usize,
    memory: MemorySample,
    procedures: ProcedureSample,
    timing: ChatContextRetrievalTiming,
) -> EvalSample {
    EvalSample {
        run,
        memory_index_query_ms: memory.index_query_ms,
        user_memory_render_ms: memory.user_render_ms,
        user_candidate_load_ms: memory.user_candidate_load_ms,
        user_temperature_overlay_ms: memory.user_temperature_overlay_ms,
        user_hot_projection_ms: memory.user_hot_projection_ms,
        user_rank_select_render_ms: memory.user_rank_select_render_ms,
        agent_memory_render_ms: memory.agent_render_ms,
        agent_candidate_load_ms: memory.agent_candidate_load_ms,
        agent_temperature_overlay_ms: memory.agent_temperature_overlay_ms,
        agent_hot_projection_ms: memory.agent_hot_projection_ms,
        agent_rank_select_render_ms: memory.agent_rank_select_render_ms,
        memory_total_ms: timing.memory_ms,
        procedures_ms: timing.procedures_ms,
        concurrent_wall_ms: timing.concurrent_wall_ms,
        overlap_saved_ms: timing.overlap_saved_ms,
        user_memory_backend: memory.user_backend,
        agent_memory_backend: memory.agent_backend,
        procedure_backend: procedures.backend.as_str().to_string(),
        user_memory_selected: memory.user_selected,
        agent_memory_selected: memory.agent_selected,
        procedures_selected: procedures.selected,
        procedure_candidates: procedures.candidates,
    }
}

fn summarize_samples(samples: &[EvalSample]) -> BTreeMap<String, LatencySummary> {
    let metrics: [(&str, fn(&EvalSample) -> f64); 15] = [
        ("memory_index_query_ms", |sample| {
            sample.memory_index_query_ms
        }),
        ("user_memory_render_ms", |sample| {
            sample.user_memory_render_ms
        }),
        ("user_candidate_load_ms", |sample| {
            sample.user_candidate_load_ms
        }),
        ("user_temperature_overlay_ms", |sample| {
            sample.user_temperature_overlay_ms
        }),
        ("user_hot_projection_ms", |sample| {
            sample.user_hot_projection_ms
        }),
        ("user_rank_select_render_ms", |sample| {
            sample.user_rank_select_render_ms
        }),
        ("agent_memory_render_ms", |sample| {
            sample.agent_memory_render_ms
        }),
        ("agent_candidate_load_ms", |sample| {
            sample.agent_candidate_load_ms
        }),
        ("agent_temperature_overlay_ms", |sample| {
            sample.agent_temperature_overlay_ms
        }),
        ("agent_hot_projection_ms", |sample| {
            sample.agent_hot_projection_ms
        }),
        ("agent_rank_select_render_ms", |sample| {
            sample.agent_rank_select_render_ms
        }),
        ("memory_total_ms", |sample| sample.memory_total_ms),
        ("procedures_ms", |sample| sample.procedures_ms),
        ("concurrent_wall_ms", |sample| sample.concurrent_wall_ms),
        ("overlap_saved_ms", |sample| sample.overlap_saved_ms),
    ];
    metrics
        .into_iter()
        .filter_map(|(name, read)| {
            summarize_latencies(&samples.iter().map(read).collect::<Vec<_>>())
                .map(|summary| (name.to_string(), summary))
        })
        .collect()
}

fn count_memory_backends(samples: &[EvalSample]) -> BTreeMap<String, usize> {
    count_backends(samples.iter().flat_map(|sample| {
        [
            sample.user_memory_backend.as_str(),
            sample.agent_memory_backend.as_str(),
        ]
    }))
}

fn count_backends<'a>(backends: impl Iterator<Item = &'a str>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for backend in backends {
        *counts.entry(backend.to_string()).or_insert(0) += 1;
    }
    counts
}

fn write_report(path: &Path, report: &EvalReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating report directory {}", parent.display()))?;
    }
    let body = serde_json::to_vec_pretty(report).context("serializing eval report")?;
    std::fs::write(path, body).with_context(|| format!("writing report {}", path.display()))
}

fn print_report(path: &Path, report: &EvalReport) {
    println!(
        "Chat context retrieval latency ({} measured runs)",
        report.runs
    );
    println!(
        "scope: {}/{} agent={}",
        report.principal, report.workspace, report.agent
    );
    println!(
        "procedure index: {} (waited {:.1} ms)",
        if report.procedure_index_ready {
            "ready"
        } else {
            "fallback"
        },
        report.procedure_index_wait_ms
    );
    println!(
        "query embeddings: {} provider calls, {} coalesced reuses",
        report.query_embedding_provider_calls, report.query_embedding_coalesced_reuses
    );
    for metric in [
        "memory_index_query_ms",
        "user_memory_render_ms",
        "user_candidate_load_ms",
        "user_temperature_overlay_ms",
        "user_hot_projection_ms",
        "user_rank_select_render_ms",
        "agent_memory_render_ms",
        "agent_candidate_load_ms",
        "agent_temperature_overlay_ms",
        "agent_hot_projection_ms",
        "agent_rank_select_render_ms",
        "memory_total_ms",
        "procedures_ms",
        "concurrent_wall_ms",
        "overlap_saved_ms",
    ] {
        if let Some(summary) = report.summaries.get(metric) {
            println!(
                "{metric:>20}: mean={:8.1} p50={:8.1} p95={:8.1} max={:8.1} ms",
                summary.mean_ms, summary.p50_ms, summary.p95_ms, summary.max_ms
            );
        }
    }
    println!("memory backends: {:?}", report.memory_backend_counts);
    println!("procedure backends: {:?}", report.procedure_backend_counts);
    if let Some(contention) = report.background_contention.as_ref() {
        for lane in [
            &contention.optional_background,
            &contention.memory_index_write,
        ] {
            let p95 = lane
                .foreground_summaries
                .get("concurrent_wall_ms")
                .map(|summary| summary.p95_ms)
                .unwrap_or_default();
            println!(
                "{} priority: foreground p95={p95:.1} ms, background={}/{} inputs in {:.1} ms, proven contention={}/{}, oversized fixture={} bytes > physical ceiling={} tokens",
                lane.lane,
                lane.completed_inputs,
                lane.requested_inputs,
                lane.background_duration_ms,
                lane.proven_contention_runs,
                lane.foreground_runs,
                lane.largest_fixture_bytes,
                lane.configured_physical_token_ceiling,
            );
        }
    }
    println!("report: {}", path.display());
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician_vector_index::{acquire_embedding_permit, EmbeddingPriority};

    #[test]
    fn every_contention_lane_contains_complete_oversized_fixture() {
        let ceiling = 512;
        let inputs = contention_inputs("memory-index-write", 4, ceiling);

        assert_eq!(inputs.len(), 4);
        assert!(inputs[0].len() > ceiling);
        assert!(inputs[0].contains("complete-tail-evidence"));
        assert!(inputs.iter().all(|input| !input.is_empty()));
    }

    #[tokio::test]
    async fn cancelled_eval_foreground_restores_scheduler_waiter_count() {
        let _guard = magician_vector_index::EMBEDDING_ADMISSION_TEST_LOCK
            .lock()
            .await;
        magician_vector_index::install_embedding_admission_capacity(1);
        let background = acquire_embedding_permit(EmbeddingPriority::BackgroundRead).await;
        let baseline = embedding_admission_stats().waiting_foreground_reads;

        {
            let foreground = async {
                let _permit = acquire_embedding_permit(EmbeddingPriority::Read).await;
                std::future::pending::<Result<EvalSample>>().await
            };
            tokio::pin!(foreground);
            let deadline = Instant::now() + Duration::from_secs(1);
            while embedding_admission_stats().waiting_foreground_reads <= baseline {
                assert!(
                    Instant::now() < deadline,
                    "foreground waiter was not registered"
                );
                tokio::select! {
                    _ = &mut foreground => panic!("foreground unexpectedly completed"),
                    _ = tokio::task::yield_now() => {},
                }
            }
            assert_eq!(
                embedding_admission_stats().waiting_foreground_reads,
                baseline + 1
            );
        }

        assert_eq!(
            embedding_admission_stats().waiting_foreground_reads,
            baseline,
            "dropping a failed/timed-out eval future must unregister its waiter"
        );
        drop(background);
    }

    #[tokio::test]
    async fn cancelled_eval_foreground_restores_active_permit_count() {
        let _guard = magician_vector_index::EMBEDDING_ADMISSION_TEST_LOCK
            .lock()
            .await;
        magician_vector_index::install_embedding_admission_capacity(1);
        let baseline = embedding_admission_stats().active_foreground_reads;
        let (acquired_tx, mut acquired_rx) = oneshot::channel();

        {
            let foreground = async move {
                let _permit = acquire_embedding_permit(EmbeddingPriority::Read).await;
                let _ = acquired_tx.send(());
                std::future::pending::<Result<EvalSample>>().await
            };
            tokio::pin!(foreground);
            tokio::select! {
                _ = &mut foreground => panic!("foreground unexpectedly completed"),
                acquired = &mut acquired_rx => acquired.expect("foreground should report its exact active permit"),
            }
            assert_eq!(
                embedding_admission_stats().active_foreground_reads,
                baseline + 1
            );
        }

        assert_eq!(
            embedding_admission_stats().active_foreground_reads,
            baseline,
            "dropping a timed-out eval future must release an active foreground permit"
        );
    }
}
