//! Mixed live evaluation for memory retrieval, temperature behavior, and the
//! configured memory-utility reviewer.
//!
//! The real-memory lane is strictly read-only: it suppresses audit/usage writes
//! and background temperature-overlay repair. Synthetic and reviewer-backed
//! lanes use an isolated temporary workspace.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use magician::{
    config::{load_default_magician_config, load_magician_config_from_path, MagicianConfig},
    magician_v2::{
        agents::{
            compute_memory_tier_health, configure_memory_prompt_budgets,
            enqueue_memory_temperature_utility_review, evaluate_memory_tier_health,
            load_fresh_index_documents, load_memory_candidate_documents,
            load_memory_hot_projection_index, load_memory_temperature_overlay,
            memory_candidate_has_superseded_lifecycle, memory_candidate_index_score_key,
            memory_temperature_candidate_key, memory_temperature_entry_is_superseded,
            memory_temperature_utility_queue_health, memory_temperature_utility_review_was_applied,
            rebuild_scope_memory_index, render_memory_tiers_for_prompt_with_index_result,
            render_memory_tiers_for_prompt_with_scores_result,
            run_memory_temperature_utility_batch_maintenance, save_memory_temperature_overlay,
            score_hybrid_index_for_prompt, source_text_hash, sync_memory_temperature_overlay,
            AgentDefinitionStore, AgentMemoryResolver, AgentMemoryService, MemoryCandidateDocument,
            MemoryCandidateRequest, MemoryPromptRetrievalBackend, MemoryPromptSelectedCandidate,
            MemoryRenderRequest, MemoryTemperatureTier,
            MemoryTemperatureUtilityBatchMaintenanceConfig, MemoryTemperatureUtilityLabel,
            MemoryTemperatureUtilityReviewInput, MemoryTierHealthGates, MemoryTierHealthMetrics,
            SemanticMemoryType, TierScope,
        },
        artifact_v2::{
            memory::V3MemoryTierRecord,
            workspace::{default_storage_base_path, ArtifactV2Workspace},
        },
        query_analysis::operation_llm_router::OperationLlmRouter,
        workspace_storage_settings::WorkspaceStorageSettingsStore,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const BUILTIN_SUITES: &[(&str, &str)] = &[
    (
        "core-memory-smoke",
        include_str!("../../data/magician_v2/memory_evals/core-memory-smoke.json"),
    ),
    (
        "personal-assistant-regression",
        include_str!("../../data/magician_v2/memory_evals/personal-assistant-regression.json"),
    ),
    (
        "internal-system-analyst-regression",
        include_str!("../../data/magician_v2/memory_evals/internal-system-analyst-regression.json"),
    ),
    (
        "simple-data-analyst-regression",
        include_str!("../../data/magician_v2/memory_evals/simple-data-analyst-regression.json"),
    ),
    (
        "web-researcher-regression",
        include_str!("../../data/magician_v2/memory_evals/web-researcher-regression.json"),
    ),
    (
        "weg-ambient-memory-regression",
        include_str!("../../data/magician_v2/memory_evals/weg-ambient-memory-regression.json"),
    ),
];

#[derive(Debug, Parser)]
#[command(
    about = "Evaluate real and synthetic memory-temperature behavior with the configured local reviewer"
)]
struct Args {
    #[arg(long, default_value = "anonymous")]
    principal: String,
    #[arg(long, default_value = "default")]
    workspace: String,
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, default_value = "coverage/evals/memory-temperature/latest")]
    output_dir: PathBuf,
    #[arg(long, default_value_t = 0.80)]
    min_real_recall: f64,
    #[arg(long, default_value_t = 0.80)]
    min_hybrid_coverage: f64,
    #[arg(long, default_value_t = 0.75)]
    min_utility_accuracy: f64,
    #[arg(long, default_value_t = 1_000.0)]
    max_retrieval_p95_ms: f64,
    #[arg(long, default_value_t = 120_000.0)]
    max_utility_p95_ms: f64,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct EvalSuite {
    #[serde(default)]
    suite_id: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    cases: Vec<EvalCase>,
}

#[derive(Debug, Clone, Deserialize)]
struct EvalCase {
    case_id: String,
    agent_id: String,
    query: String,
    #[serde(default, alias = "expected")]
    expected_substrings: Vec<String>,
    #[serde(default)]
    min_selected_count: Option<usize>,
    #[serde(default)]
    scopes: Vec<EvalScope>,
    #[serde(default = "default_max_entries")]
    max_entries: usize,
    #[serde(default = "default_max_chars")]
    max_chars: usize,
    #[serde(default)]
    kind: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EvalScope {
    User,
    Agent,
    AgentGoal,
}

#[derive(Debug, Clone, Serialize)]
struct RetrievalCaseReport {
    lane: String,
    suite_id: String,
    case_id: String,
    state: String,
    matched_anchors: usize,
    expected_anchors: usize,
    existing_anchors: usize,
    configured_anchors: usize,
    selected_count: usize,
    selected_key_hashes: Vec<String>,
    best_rank: Option<usize>,
    reciprocal_rank: f64,
    backends: Vec<String>,
    latency_ms: f64,
    latency_sample: String,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct UtilityCaseReport {
    run_id: String,
    label_accuracy: f64,
    passed: bool,
    labels: BTreeMap<String, String>,
    reasons: BTreeMap<String, String>,
    projections_expected: usize,
    projections_created: usize,
    projection_markers_preserved: usize,
    latency_ms: f64,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct MetricSummary {
    total: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
    recall: f64,
    mrr: f64,
    hybrid_coverage: f64,
    latency_p50_ms: f64,
    latency_p95_ms: f64,
    cold_latency_p95_ms: f64,
}

#[derive(Debug, Serialize)]
struct GateResult {
    name: String,
    passed: bool,
    actual: String,
    required: String,
}

#[derive(Debug, Serialize)]
struct EvalReport {
    schema_version: u32,
    generated_at: DateTime<Utc>,
    runtime_root: String,
    principal: String,
    workspace: String,
    embedding_endpoint: String,
    embedding_model: String,
    utility_profile_operation: String,
    real_fixture_coverage: f64,
    /// Tier-assignment effectiveness for the real scope. Distinct from every
    /// other metric here, which measures retrieval: this asks whether the
    /// temperature tier a memory landed in was earned and predicts use.
    tier_health: Option<MemoryTierHealthMetrics>,
    real: MetricSummary,
    synthetic: MetricSummary,
    utility_accuracy: f64,
    utility_latency_p95_ms: f64,
    projection_coverage: f64,
    projection_factuality: f64,
    real_cases: Vec<RetrievalCaseReport>,
    synthetic_cases: Vec<RetrievalCaseReport>,
    utility_cases: Vec<UtilityCaseReport>,
    stage_errors: Vec<String>,
    gates: Vec<GateResult>,
    passed: bool,
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
    validate_args(&args)?;
    load_runtime_env_files();
    let config = load_config(&args)?;
    configure_memory_prompt_budgets(&config.memory);
    let workspace = resolve_workspace(&args, &config.storage_path)?;
    let suites = load_builtin_suites()?;

    if args.dry_run {
        println!(
            "memory-temperature live eval plan: {} enabled retrieval cases; real root={}; synthetic workspace=isolated; utility operation=memory_temperature_utility_review",
            suites.iter().filter(|suite| suite.enabled).map(|suite| suite.cases.iter().filter(|case| case.kind.is_empty() || case.kind == "retrieval").count()).sum::<usize>(),
            workspace.base_root().display(),
        );
        return Ok(());
    }

    let definition_store = AgentDefinitionStore::with_workspace_layout(workspace.clone())
        .for_scope(&args.principal, &args.workspace);
    let memory_service = AgentMemoryResolver::with_workspace_layout(workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .context("resolving real scoped memory")?;

    let real_cases = run_real_retrieval(&definition_store, &memory_service, &suites).await?;
    let temp = tempfile::tempdir().context("creating isolated eval workspace")?;
    let synthetic_workspace = ArtifactV2Workspace::new(temp.path());
    let synthetic_memory = AgentMemoryResolver::with_workspace_layout(synthetic_workspace)
        .resolve_for_scope("memory-eval", "isolated")
        .context("resolving isolated synthetic memory")?;
    let synthetic_cases =
        run_synthetic_retrieval(&definition_store, &synthetic_memory, "personal-assistant").await?;
    let router_config = config
        .router_config()
        .cloned()
        .context("configured LLM router is required for utility-review live eval")?;
    let mut stage_errors = Vec::new();
    let utility_cases = match run_utility_review(
        synthetic_memory.clone(),
        Arc::new(OperationLlmRouter::new(Some(router_config))),
    )
    .await
    {
        Ok(cases) => cases,
        Err(error) => {
            let error = format!("{error:#}");
            stage_errors.push(format!("utility_review: {error}"));
            vec![UtilityCaseReport {
                run_id: "utility-review-stage".to_string(),
                label_accuracy: 0.0,
                passed: false,
                labels: BTreeMap::new(),
                reasons: BTreeMap::new(),
                projections_expected: 3,
                projections_created: 0,
                projection_markers_preserved: 0,
                latency_ms: 0.0,
                error: Some(error),
            }]
        },
    };

    let real = summarize_retrieval(&real_cases);
    let real_fixture_coverage = fixture_coverage(&real_cases);
    // Read-only: the tier metrics are computed from the overlay this run
    // already produced, and never write back.
    let tier_health = match load_memory_temperature_overlay(memory_service.storage()).await {
        Ok(overlay) => Some(compute_memory_tier_health(&overlay)),
        Err(error) => {
            stage_errors.push(format!("tier health: {error}"));
            None
        },
    };
    let synthetic = summarize_retrieval(&synthetic_cases);
    let utility_accuracy = mean(utility_cases.iter().map(|case| case.label_accuracy));
    let utility_latency_p95_ms = percentile(
        utility_cases.iter().map(|case| case.latency_ms).collect(),
        0.95,
    );
    let projection_expected = utility_cases
        .iter()
        .map(|case| case.projections_expected)
        .sum::<usize>();
    let projection_created = utility_cases
        .iter()
        .map(|case| case.projections_created)
        .sum::<usize>();
    let projection_markers = utility_cases
        .iter()
        .map(|case| case.projection_markers_preserved)
        .sum::<usize>();
    let projection_coverage = ratio(projection_created, projection_expected);
    let projection_factuality = if projection_expected > 0 && projection_created == 0 {
        0.0
    } else {
        ratio(projection_markers, projection_created)
    };
    let mut gates = vec![
        gate("real recall", real.recall, args.min_real_recall),
        gate(
            "real hybrid coverage",
            real.hybrid_coverage,
            args.min_hybrid_coverage,
        ),
        gate_at_most(
            "real warm retrieval p95",
            real.latency_p95_ms,
            args.max_retrieval_p95_ms,
            "ms",
        ),
        gate("synthetic retrieval", synthetic.recall, 1.0),
        gate(
            "utility label accuracy",
            utility_accuracy,
            args.min_utility_accuracy,
        ),
        gate_at_most(
            "utility review p95",
            utility_latency_p95_ms,
            args.max_utility_p95_ms,
            "ms",
        ),
        gate("hot projection coverage", projection_coverage, 0.66),
        gate(
            "hot projection marker factuality",
            projection_factuality,
            1.0,
        ),
    ];
    if let Some(tier_health) = tier_health.as_ref() {
        for tier_gate in evaluate_memory_tier_health(tier_health, MemoryTierHealthGates::default())
        {
            if tier_gate.skipped {
                continue;
            }
            gates.push(GateResult {
                name: format!("tier: {}", tier_gate.name),
                passed: tier_gate.passed,
                actual: format!("{:.4}", tier_gate.actual),
                required: format!(
                    "{} {:.4}",
                    if tier_gate.upper_bound { "<=" } else { ">=" },
                    tier_gate.threshold
                ),
            });
        }
    }
    let passed = gates.iter().all(|gate| gate.passed);
    let report = EvalReport {
        schema_version: 1,
        generated_at: Utc::now(),
        runtime_root: workspace.base_root().display().to_string(),
        principal: args.principal,
        workspace: args.workspace,
        embedding_endpoint: config.runtime.ollama.embedding_base_url,
        embedding_model: config.runtime.ollama.embedding_model,
        utility_profile_operation: "memory_temperature_utility_review".to_string(),
        real_fixture_coverage,
        tier_health,
        real,
        synthetic,
        utility_accuracy,
        utility_latency_p95_ms,
        projection_coverage,
        projection_factuality,
        real_cases,
        synthetic_cases,
        utility_cases,
        stage_errors,
        gates,
        passed,
    };
    write_report(&args.output_dir, &report)?;
    print_summary(&args.output_dir, &report);
    anyhow::ensure!(
        report.passed,
        "one or more memory-temperature live-eval gates failed"
    );
    Ok(())
}

fn validate_args(args: &Args) -> Result<()> {
    for (name, value) in [
        ("--min-real-recall", args.min_real_recall),
        ("--min-hybrid-coverage", args.min_hybrid_coverage),
        ("--min-utility-accuracy", args.min_utility_accuracy),
    ] {
        anyhow::ensure!(
            (0.0..=1.0).contains(&value),
            "{name} must be between 0 and 1"
        );
    }
    anyhow::ensure!(
        args.max_retrieval_p95_ms > 0.0,
        "retrieval latency gate must be positive"
    );
    anyhow::ensure!(
        args.max_utility_p95_ms > 0.0,
        "utility latency gate must be positive"
    );
    Ok(())
}

fn load_config(args: &Args) -> Result<MagicianConfig> {
    match args.config.as_deref() {
        Some(path) => load_magician_config_from_path(path)
            .with_context(|| format!("loading config {}", path.display())),
        None => load_default_magician_config().context("loading active magician config"),
    }
}

fn load_runtime_env_files() {
    for file_name in [".env.development", ".env"] {
        let path = magician::magician_v2::artifact_v2::workspace::runtime_config_path(
            file_name, file_name,
        );
        let _ = dotenvy::from_path(path);
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

fn load_builtin_suites() -> Result<Vec<EvalSuite>> {
    BUILTIN_SUITES
        .iter()
        .map(|(fallback, raw)| {
            let mut suite: EvalSuite = serde_json::from_str(raw)
                .with_context(|| format!("parsing built-in memory eval {fallback}"))?;
            if suite.suite_id.trim().is_empty() {
                suite.suite_id = (*fallback).to_string();
            }
            Ok(suite)
        })
        .collect()
}

async fn run_real_retrieval(
    definition_store: &AgentDefinitionStore,
    memory_service: &AgentMemoryService,
    suites: &[EvalSuite],
) -> Result<Vec<RetrievalCaseReport>> {
    let mut reports = Vec::new();
    let fresh_documents = load_fresh_index_documents(memory_service.storage(), definition_store)
        .await
        .context("loading fresh real-memory documents for fixture applicability")?;
    let temperature_overlay = load_memory_temperature_overlay(memory_service.storage()).await?;
    for suite in suites.iter().filter(|suite| suite.enabled) {
        for case in suite
            .cases
            .iter()
            .filter(|case| case.kind.is_empty() || case.kind == "retrieval")
        {
            let Some(record) = definition_store
                .get_definition(&case.agent_id)
                .await
                .with_context(|| format!("loading definition {}", case.agent_id))?
            else {
                reports.push(failed_retrieval_case(
                    "real",
                    suite,
                    case,
                    "agent definition is missing",
                ));
                continue;
            };
            let scopes = effective_scopes(case);
            let existing_expected = fresh_documents.as_ref().map(|documents| {
                case.expected_substrings
                    .iter()
                    .filter(|expected| {
                        documents.iter().any(|document| {
                            document_matches_case_scope(document, &case.agent_id, &scopes)
                                && document_is_current(document, &temperature_overlay)
                                && document_contains(document, expected)
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let applicable_expected = fresh_documents.as_ref().map(|documents| {
                case.expected_substrings
                    .iter()
                    .filter(|expected| {
                        documents.iter().any(|document| {
                            document_is_prompt_eligible_for_case(
                                document,
                                &case.agent_id,
                                case,
                                &scopes,
                            ) && document_is_current(document, &temperature_overlay)
                                && document_contains(document, expected)
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            });
            if matches!(applicable_expected.as_ref(), Some(expected) if expected.is_empty()) {
                let existing_count = existing_expected.as_ref().map_or(0, Vec::len);
                reports.push(RetrievalCaseReport {
                    lane: "real".to_string(),
                    suite_id: suite.suite_id.clone(),
                    case_id: case.case_id.clone(),
                    state: "skipped".to_string(),
                    matched_anchors: 0,
                    expected_anchors: 0,
                    existing_anchors: existing_count,
                    configured_anchors: case.expected_substrings.len(),
                    selected_count: 0,
                    selected_key_hashes: Vec::new(),
                    best_rank: None,
                    reciprocal_rank: 0.0,
                    backends: Vec::new(),
                    latency_ms: 0.0,
                    latency_sample: "not_measured".to_string(),
                    notes: vec![if existing_count == 0 {
                        format!(
                            "none of {} configured anchors exists in the current real scope",
                            case.expected_substrings.len()
                        )
                    } else {
                        format!(
                            "{existing_count} existing anchor(s) are in search-only or disabled automatic prompt lanes"
                        )
                    }],
                });
                continue;
            }
            let expected = applicable_expected
                .as_deref()
                .unwrap_or(&case.expected_substrings);
            let started = Instant::now();
            let mut selected = Vec::new();
            let mut backends = Vec::new();
            let mut candidate_load_ms = 0.0_f64;
            let score_request = request_for_case(case, scopes[0], false);
            let (indexed_scores, retrieval_backend, fallback_error) =
                score_hybrid_index_for_prompt(
                    memory_service,
                    definition_store,
                    &case.agent_id,
                    &score_request,
                )
                .await;
            for scope in &scopes {
                let request = request_for_case(case, *scope, false);
                let rendered = render_memory_tiers_for_prompt_with_scores_result(
                    memory_service,
                    &case.agent_id,
                    &record.definition.memory_tiers,
                    &request,
                    indexed_scores.as_ref(),
                    retrieval_backend,
                    fallback_error.as_deref(),
                )
                .await
                .with_context(|| {
                    format!(
                        "rendering real memory case {}/{}",
                        suite.suite_id, case.case_id
                    )
                })?;
                candidate_load_ms = candidate_load_ms.max(rendered.timing.candidate_load_ms);
                backends.push(rendered.retrieval_backend.as_str().to_string());
                selected.extend(rendered.selected_candidates);
            }
            let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
            let (matched, best_rank) = match_selected(&selected, expected);
            let selected_ok = case
                .min_selected_count
                .map(|n| selected.len() >= n)
                .unwrap_or(true);
            let state = if matched == expected.len() && selected_ok {
                "passed"
            } else {
                "failed"
            };
            let mut notes = Vec::new();
            if expected.len() < case.expected_substrings.len() {
                notes.push(format!(
                    "{} of {} configured anchors exist in the current real scope",
                    expected.len(),
                    case.expected_substrings.len()
                ));
            }
            notes.push(format!(
                "maximum per-scope candidate load: {candidate_load_ms:.1}ms"
            ));
            if let (Some(documents), Some(scores)) =
                (fresh_documents.as_ref(), indexed_scores.as_ref())
            {
                let mut ranked_scores = scores.iter().collect::<Vec<_>>();
                ranked_scores.sort_by(|(left_key, left_score), (right_key, right_score)| {
                    right_score
                        .total_cmp(left_score)
                        .then_with(|| left_key.cmp(right_key))
                });
                let score_ranks = ranked_scores
                    .into_iter()
                    .enumerate()
                    .map(|(index, (key, _))| (key.as_str(), index + 1))
                    .collect::<BTreeMap<_, _>>();
                let best_anchor_details = expected
                    .iter()
                    .filter_map(|expected| {
                        documents
                            .iter()
                            .filter(|document| {
                                document_is_prompt_eligible_for_case(
                                    document,
                                    &case.agent_id,
                                    case,
                                    &scopes,
                                ) && document_is_current(document, &temperature_overlay)
                                    && document_contains(document, expected)
                            })
                            .filter_map(|document| {
                                let key = memory_candidate_index_score_key(document);
                                let rank = score_ranks.get(key.as_str()).copied()?;
                                let lower = document.text.to_lowercase();
                                let needle = expected.to_lowercase();
                                let offset = lower
                                    .find(&needle)
                                    .map(|byte_index| lower[..byte_index].chars().count())
                                    .unwrap_or(0);
                                let candidate_key = memory_temperature_candidate_key(document);
                                let selected_candidate = selected.iter().find(|candidate| {
                                    candidate.memory_candidate_key == candidate_key
                                });
                                let selection = selected_candidate.map_or_else(
                                    || "selected=0".to_string(),
                                    |candidate| {
                                        format!(
                                            "selected=1/projection={}/rendered_chars={}",
                                            usize::from(candidate.projection_used),
                                            candidate.text.chars().count()
                                        )
                                    },
                                );
                                Some((
                                    rank,
                                    document.text.chars().count(),
                                    offset,
                                    document.semantic_memory_type.as_str(),
                                    selection,
                                ))
                            })
                            .min_by_key(|(rank, _, _, _, _)| *rank)
                    })
                    .map(|(rank, chars, offset, lane, selection)| {
                        format!("r{rank}/c{chars}/o{offset}/lane={lane}/{selection}")
                    })
                    .collect::<Vec<_>>();
                let indexed_anchors = best_anchor_details.len();
                notes.push(format!(
                    "hybrid search scored {indexed_anchors}/{} applicable anchors; best rank/characters/anchor-offset: {}",
                    expected.len(),
                    best_anchor_details.join(", ")
                ));
            }
            reports.push(RetrievalCaseReport {
                lane: "real".to_string(),
                suite_id: suite.suite_id.clone(),
                case_id: case.case_id.clone(),
                state: state.to_string(),
                matched_anchors: matched,
                expected_anchors: expected.len(),
                existing_anchors: existing_expected.as_ref().map_or(expected.len(), Vec::len),
                configured_anchors: case.expected_substrings.len(),
                selected_count: selected.len(),
                selected_key_hashes: selected
                    .iter()
                    .map(|candidate| short_hash(&candidate.memory_candidate_key))
                    .collect(),
                best_rank,
                reciprocal_rank: best_rank.map(|rank| 1.0 / rank as f64).unwrap_or(0.0),
                backends,
                latency_ms: elapsed,
                latency_sample: if candidate_load_ms <= 50.0 {
                    "warm"
                } else {
                    "cold"
                }
                .to_string(),
                notes,
            });
        }
    }
    Ok(reports)
}

fn document_matches_case_scope(
    document: &MemoryCandidateDocument,
    agent_id: &str,
    scopes: &[EvalScope],
) -> bool {
    scopes.iter().any(|scope| match (scope, &document.scope) {
        (EvalScope::User, TierScope::User) => true,
        (EvalScope::Agent, TierScope::Agent) | (EvalScope::AgentGoal, TierScope::AgentGoal) => {
            document.agent_id.as_deref() == Some(agent_id)
        },
        _ => false,
    })
}

fn document_is_prompt_eligible_for_case(
    document: &MemoryCandidateDocument,
    agent_id: &str,
    case: &EvalCase,
    scopes: &[EvalScope],
) -> bool {
    if !document_matches_case_scope(document, agent_id, scopes) {
        return false;
    }
    if matches!(document.scope, TierScope::Agent)
        && document
            .tier_name
            .split('.')
            .next()
            .is_some_and(|tier| tier == "environment_knowledge")
    {
        return false;
    }
    scopes.iter().any(|scope| {
        let same_scope = matches!(
            (scope, &document.scope),
            (EvalScope::User, TierScope::User)
                | (EvalScope::Agent, TierScope::Agent)
                | (EvalScope::AgentGoal, TierScope::AgentGoal)
        );
        same_scope
            && request_for_case(case, *scope, false)
                .lane_budgets
                .allows(document.semantic_memory_type)
    })
}

fn document_contains(document: &MemoryCandidateDocument, needle: &str) -> bool {
    document
        .text
        .to_lowercase()
        .contains(&needle.to_lowercase())
        || document
            .item_key
            .to_lowercase()
            .contains(&needle.to_lowercase())
}

fn document_is_current(
    document: &MemoryCandidateDocument,
    overlay: &magician::magician_v2::agents::MemoryTemperatureOverlay,
) -> bool {
    !memory_candidate_has_superseded_lifecycle(document)
        && !overlay
            .entries
            .get(&memory_temperature_candidate_key(document))
            .is_some_and(memory_temperature_entry_is_superseded)
}

fn request_for_case<'a>(
    case: &'a EvalCase,
    scope: EvalScope,
    repair_overlay: bool,
) -> MemoryRenderRequest<'a> {
    let mut request = match scope {
        EvalScope::User => MemoryRenderRequest::user(&case.query),
        EvalScope::Agent => MemoryRenderRequest::agent(&case.query),
        EvalScope::AgentGoal => MemoryRenderRequest::agent_goal(&case.query, None),
    };
    request.max_entries = case.max_entries;
    request.max_chars = case.max_chars;
    request.emit_audit = false;
    request.repair_temperature_overlay = repair_overlay;
    request
}

fn effective_scopes(case: &EvalCase) -> Vec<EvalScope> {
    if case.scopes.is_empty() {
        vec![EvalScope::User, EvalScope::Agent, EvalScope::AgentGoal]
    } else {
        case.scopes.clone()
    }
}

async fn run_synthetic_retrieval(
    definition_store: &AgentDefinitionStore,
    memory_service: &AgentMemoryService,
    agent_id: &str,
) -> Result<Vec<RetrievalCaseReport>> {
    let definition = definition_store
        .get_definition(agent_id)
        .await?
        .context("personal-assistant definition is required for synthetic eval")?
        .definition;
    seed_synthetic_memory(memory_service, agent_id, &definition.memory_tiers).await?;
    rebuild_scope_memory_index(memory_service.storage(), definition_store)
        .await
        .context("building isolated synthetic memory index")?;
    let documents = load_memory_candidate_documents(
        memory_service.storage(),
        agent_id,
        &definition.memory_tiers,
        &MemoryCandidateRequest {
            scope: TierScope::Agent,
            goal_id: None,
            recency_cutoff: None,
            include_environment_knowledge: true,
            // Unbound (§5A.2): an offline eval over a synthetic corpus. No
            // engagement exists to contain to, and containing to one would
            // silently shrink the fixture the eval measures.
            retrieval_scope: magician::magician_v2::agents::RetrievalScope::Unbound,
        },
    )
    .await?;
    let mut overlay = sync_memory_temperature_overlay(memory_service.storage(), &documents).await?;
    for document in &documents {
        let key = memory_temperature_candidate_key(document);
        if let Some(entry) = overlay.entries.get_mut(&key) {
            if document.text.contains("HOT-DECOY-000") {
                entry.selected_count = 50;
                entry.injected_count = 50;
                entry.reviewed_useful_count = 20;
                entry.last_utility_review_label = Some(MemoryTemperatureUtilityLabel::Useful);
            } else if document.text.contains("COLD-TRUTH-731") {
                entry.failed_use_count = 50;
                entry.reviewed_stale_count = 20;
                entry.last_utility_review_label = Some(MemoryTemperatureUtilityLabel::Stale);
            }
        }
    }
    save_memory_temperature_overlay(memory_service.storage(), &overlay).await?;
    let counters_before = overlay_counters(&overlay);

    let cases = vec![
        (
            "cold_beats_hot",
            "Recall COLD-TRUTH-731, amber 731, and the north deploy queue",
            vec!["COLD-TRUTH-731"],
            1usize,
            vec!["HOT-DECOY-000"],
        ),
        (
            "superseded_excluded",
            "What is the current launch protocol?",
            vec!["CURRENT-LAUNCH-924"],
            4usize,
            vec!["STALE-LAUNCH-111"],
        ),
        (
            "typed_lane_diversity",
            "Recall Nia, the recent rollout episode, and the current launch protocol",
            vec!["ENTITY-NIA-516", "EPISODE-ORCHID-287", "CURRENT-LAUNCH-924"],
            4usize,
            vec![],
        ),
    ];
    let mut reports = Vec::new();
    for (case_id, query, expected, max_entries, forbidden) in cases {
        let started = Instant::now();
        let mut request = MemoryRenderRequest::agent(query)
            .with_emit_audit(false)
            .with_temperature_overlay_repair(false);
        request.max_entries = max_entries;
        request.max_chars = 8_000;
        let rendered = render_memory_tiers_for_prompt_with_index_result(
            memory_service,
            definition_store,
            agent_id,
            &definition.memory_tiers,
            &request,
        )
        .await?;
        let expected = expected
            .into_iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let (matched, best_rank) = match_selected(&rendered.selected_candidates, &expected);
        let forbidden_seen = forbidden.iter().any(|needle| {
            rendered
                .selected_candidates
                .iter()
                .any(|candidate| candidate_contains(candidate, needle))
        });
        let selected_markers = [
            "COLD-TRUTH-731",
            "HOT-DECOY-000",
            "CURRENT-LAUNCH-924",
            "STALE-LAUNCH-111",
            "ENTITY-NIA-516",
            "EPISODE-ORCHID-287",
        ]
        .into_iter()
        .filter(|marker| {
            rendered
                .selected_candidates
                .iter()
                .any(|candidate| candidate_contains(candidate, marker))
        })
        .collect::<Vec<_>>();
        let passed = matched == expected.len()
            && !forbidden_seen
            && rendered.retrieval_backend == MemoryPromptRetrievalBackend::LancedbHybrid;
        reports.push(RetrievalCaseReport {
            lane: "synthetic".to_string(),
            suite_id: "memory-temperature-adversarial".to_string(),
            case_id: case_id.to_string(),
            state: if passed { "passed" } else { "failed" }.to_string(),
            matched_anchors: matched,
            expected_anchors: expected.len(),
            existing_anchors: expected.len(),
            configured_anchors: expected.len(),
            selected_count: rendered.selected_candidates.len(),
            selected_key_hashes: rendered
                .selected_candidates
                .iter()
                .map(|candidate| short_hash(&candidate.memory_candidate_key))
                .collect(),
            best_rank,
            reciprocal_rank: best_rank.map(|rank| 1.0 / rank as f64).unwrap_or(0.0),
            backends: vec![rendered.retrieval_backend.as_str().to_string()],
            latency_ms: started.elapsed().as_secs_f64() * 1_000.0,
            latency_sample: "measured".to_string(),
            notes: [
                forbidden_seen.then(|| "forbidden stale/hot distractor was selected".to_string()),
                Some(format!(
                    "selected synthetic markers: {}",
                    selected_markers.join(", ")
                )),
            ]
            .into_iter()
            .flatten()
            .collect(),
        });
    }
    let counters_after =
        overlay_counters(&load_memory_temperature_overlay(memory_service.storage()).await?);
    let unchanged = counters_before == counters_after;
    reports.push(RetrievalCaseReport {
        lane: "synthetic".to_string(),
        suite_id: "memory-temperature-adversarial".to_string(),
        case_id: "read_only_retrieval_does_not_heat_memory".to_string(),
        state: if unchanged { "passed" } else { "failed" }.to_string(),
        matched_anchors: usize::from(unchanged),
        expected_anchors: 1,
        existing_anchors: 1,
        configured_anchors: 1,
        selected_count: 0,
        selected_key_hashes: Vec::new(),
        best_rank: unchanged.then_some(1),
        reciprocal_rank: if unchanged { 1.0 } else { 0.0 },
        backends: Vec::new(),
        latency_ms: 0.0,
        latency_sample: "measured".to_string(),
        notes: (!unchanged)
            .then(|| "retrieval-only render changed temperature usage counters".to_string())
            .into_iter()
            .collect(),
    });
    Ok(reports)
}

async fn seed_synthetic_memory(
    memory_service: &AgentMemoryService,
    agent_id: &str,
    tiers: &[magician::magician_v2::agents::MemoryTierDefinition],
) -> Result<()> {
    let mut values = BTreeMap::<&str, (&str, Value)>::new();
    values.insert("insights", ("insights", Value::Array(vec![
        json!({"pattern":"release route COLD-TRUTH-731", "detail":"The correct release route is amber 731 through the north deploy queue.", "confidence":0.99}),
        json!({"pattern":"release route HOT-DECOY-000", "detail":"The unrelated cafeteria menu uses blue labels.", "confidence":0.99}),
        json!({"pattern":"current launch protocol", "detail":"CURRENT-LAUNCH-924 requires a canary before full rollout.", "confidence":0.98}),
        json!({"pattern":"old launch protocol", "detail":"STALE-LAUNCH-111 says deploy directly without canary.", "confidence":0.99, "memory_lifecycle":"superseded", "superseded_by":"CURRENT-LAUNCH-924"}),
    ])));
    values.insert("entities", ("entities", Value::Array(vec![
        json!({"name":"ENTITY-NIA-516", "type":"person", "attributes":{"role":"Atlas API owner"}, "confidence":0.97}),
    ])));
    values.insert(
        "recent_activity",
        (
            "summary",
            Value::String(
                "EPISODE-ORCHID-287 records the recent signed-canary rollout.".to_string(),
            ),
        ),
    );
    for (tier_name, (field, value)) in values {
        let tier = tiers
            .iter()
            .find(|tier| tier.name == tier_name)
            .with_context(|| format!("synthetic tier {tier_name} is missing from {agent_id}"))?;
        let mut record = V3MemoryTierRecord::new(
            tier_name,
            tier.scope.clone(),
            None,
            Some("memory-eval"),
            Some("isolated"),
            Some(agent_id),
        );
        record.fields.insert(field.to_string(), value);
        memory_service
            .save_native_tier(agent_id, tier, None, &record)
            .await?;
    }
    Ok(())
}

async fn run_utility_review(
    memory_service: AgentMemoryService,
    router: Arc<OperationLlmRouter>,
) -> Result<Vec<UtilityCaseReport>> {
    let mut documents = Vec::new();
    let mut runs = Vec::new();
    let mut gold = BTreeMap::<String, BTreeMap<String, Vec<MemoryTemperatureUtilityLabel>>>::new();
    let mut marker_by_key = BTreeMap::new();
    for index in 1..=1 {
        let run_id = format!("memory-temperature-live-{index}");
        let marker = format!("UTILITY-FACT-{index}91");
        let useful = utility_document(
            &format!("useful-{index}"),
            SemanticMemoryType::ProjectContext,
            &format!("{marker}: the production release must use signed canary artifacts before promotion; this rule is authoritative and required for the answer."),
        );
        let irrelevant = utility_document(
            &format!("irrelevant-{index}"),
            SemanticMemoryType::Entity,
            "The office ficus is watered on Friday; this fact has no relationship to deployment.",
        );
        let harmful = utility_document(
            &format!("harmful-{index}"),
            SemanticMemoryType::ProjectContext,
            "Obsolete instruction: bypass the signed canary and promote an unsigned artifact directly.",
        );
        let selected = [&useful, &irrelevant, &harmful]
            .into_iter()
            .map(selected_from_document)
            .collect::<Vec<_>>();
        let useful_key = selected[0].memory_candidate_key.clone();
        let irrelevant_key = selected[1].memory_candidate_key.clone();
        let harmful_key = selected[2].memory_candidate_key.clone();
        marker_by_key.insert(useful_key.clone(), marker.clone());
        gold.insert(
            run_id.clone(),
            BTreeMap::from([
                (
                    useful_key,
                    vec![
                        MemoryTemperatureUtilityLabel::LoadBearing,
                        MemoryTemperatureUtilityLabel::Useful,
                    ],
                ),
                (
                    irrelevant_key,
                    vec![MemoryTemperatureUtilityLabel::Irrelevant],
                ),
                (
                    harmful_key,
                    vec![
                        MemoryTemperatureUtilityLabel::Harmful,
                        MemoryTemperatureUtilityLabel::Stale,
                    ],
                ),
            ]),
        );
        runs.push(MemoryTemperatureUtilityReviewInput {
            run_id: run_id.clone(),
            agent_id: "memory-temperature-live-eval".to_string(),
            task_id: None,
            execution_id: None,
            chat_session_id: Some(run_id),
            goal: "State the safe production release procedure using only relevant current memory.".to_string(),
            outcome: format!("Succeeded: followed {marker}; rejected the obsolete unsigned-artifact instruction."),
            final_answer: format!("Use {marker}: signed canary artifacts are required before promotion. The direct unsigned promotion instruction is wrong and was not used."),
            action_trace: Vec::new(),
            selected_candidates: selected,
        });
        documents.extend([useful, irrelevant, harmful]);
    }
    sync_memory_temperature_overlay(memory_service.storage(), &documents).await?;
    let started = Instant::now();
    for run in &runs {
        enqueue_memory_temperature_utility_review(&memory_service, run)
            .await
            .with_context(|| format!("enqueueing durable utility review {}", run.run_id))?;
    }
    let maintenance = run_memory_temperature_utility_batch_maintenance(
        memory_service.clone(),
        router,
        MemoryTemperatureUtilityBatchMaintenanceConfig {
            min_batch_size: 1,
            max_batch_size: 1,
        },
    )
    .await
    .context("running durable configured memory utility reviewer")?;
    anyhow::ensure!(
        maintenance.reviewed_runs == 1 && maintenance.failed == 0,
        "durable utility maintenance did not commit exactly one isolated run: {maintenance:?}"
    );
    let queue_health = memory_temperature_utility_queue_health(&memory_service).await?;
    anyhow::ensure!(
        queue_health.active == 0 && queue_health.dead == 0,
        "durable utility maintenance left unexpected work behind: {queue_health:?}"
    );
    anyhow::ensure!(
        memory_temperature_utility_review_was_applied(memory_service.storage(), &runs[0].run_id,)
            .await?,
        "durable utility review completed without its exactly-once overlay marker"
    );
    let batch_latency_ms = started.elapsed().as_secs_f64() * 1_000.0;
    // Analytics rows are flushed asynchronously; keep the isolated workspace
    // alive long enough for those writes to finish before TempDir cleanup.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let overlay = load_memory_temperature_overlay(memory_service.storage()).await?;
    let projections = load_memory_hot_projection_index(memory_service.storage()).await?;
    let mut reports = Vec::new();
    for run in runs {
        let expected = gold.get(&run.run_id).expect("gold run");
        let mut labels = BTreeMap::new();
        let mut reasons = BTreeMap::new();
        let mut correct = 0usize;
        let mut projection_expected = 0usize;
        let mut projection_created = 0usize;
        let mut markers_preserved = 0usize;
        for candidate in &run.selected_candidates {
            let label = overlay
                .entries
                .get(&candidate.memory_candidate_key)
                .and_then(|entry| entry.last_utility_review_label);
            labels.insert(
                short_hash(&candidate.memory_candidate_key),
                label
                    .map(|label| label.as_str())
                    .unwrap_or("missing")
                    .to_string(),
            );
            reasons.insert(
                short_hash(&candidate.memory_candidate_key),
                overlay
                    .entries
                    .get(&candidate.memory_candidate_key)
                    .and_then(|entry| entry.last_utility_review_reason.clone())
                    .unwrap_or_else(|| "missing review reason".to_string()),
            );
            if label.is_some_and(|label| {
                expected
                    .get(&candidate.memory_candidate_key)
                    .is_some_and(|allowed| allowed.contains(&label))
            }) {
                correct += 1;
            }
            if marker_by_key.contains_key(&candidate.memory_candidate_key) {
                projection_expected += 1;
                if let Some(projection) =
                    projections.projections.get(&candidate.memory_candidate_key)
                {
                    projection_created += 1;
                    if marker_by_key
                        .get(&candidate.memory_candidate_key)
                        .is_some_and(|marker| projection.compact_text.contains(marker))
                    {
                        markers_preserved += 1;
                    }
                }
            }
        }
        let accuracy = ratio(correct, expected.len());
        reports.push(UtilityCaseReport {
            run_id: run.run_id,
            label_accuracy: accuracy,
            passed: accuracy >= 2.0 / 3.0,
            labels,
            reasons,
            projections_expected: projection_expected,
            projections_created: projection_created,
            projection_markers_preserved: markers_preserved,
            latency_ms: batch_latency_ms,
            error: None,
        });
    }
    Ok(reports)
}

fn utility_document(
    item_key: &str,
    lane: SemanticMemoryType,
    text: &str,
) -> MemoryCandidateDocument {
    MemoryCandidateDocument {
        principal: Some("memory-eval".to_string()),
        workspace: Some("isolated".to_string()),
        agent_id: Some("memory-temperature-live-eval".to_string()),
        scope: TierScope::Agent,
        tier_name: "utility_eval".to_string(),
        semantic_memory_type: lane,
        goal_id: None,
        item_key: item_key.to_string(),
        source_path: None,
        json_pointer: format!("/utility/{item_key}"),
        content_hash: source_text_hash(text),
        last_updated: Utc::now(),
        confidence: Some(0.99),
        text: text.to_string(),
        metadata_json: json!({"candidate_kind":"live_eval"}),
    }
}

fn selected_from_document(document: &MemoryCandidateDocument) -> MemoryPromptSelectedCandidate {
    MemoryPromptSelectedCandidate {
        memory_candidate_key: memory_temperature_candidate_key(document),
        semantic_memory_type: document.semantic_memory_type,
        temperature_tier: MemoryTemperatureTier::T2,
        tier_name: document.tier_name.clone(),
        source_key: document.item_key.clone(),
        source_ids: vec![format!("eval:{}", document.item_key)],
        source_text_hash: document.content_hash.clone(),
        source_text: document.text.clone(),
        text: document.text.clone(),
        projection_used: false,
        app_model_processing: None,
    }
}

fn overlay_counters(
    overlay: &magician::magician_v2::agents::MemoryTemperatureOverlay,
) -> BTreeMap<String, (u32, u32, u32)> {
    overlay
        .entries
        .iter()
        .map(|(key, entry)| {
            (
                key.clone(),
                (
                    entry.retrieved_count,
                    entry.selected_count,
                    entry.injected_count,
                ),
            )
        })
        .collect()
}

fn match_selected(
    selected: &[MemoryPromptSelectedCandidate],
    expected: &[String],
) -> (usize, Option<usize>) {
    let matched = expected
        .iter()
        .filter(|needle| {
            selected
                .iter()
                .any(|candidate| candidate_contains(candidate, needle))
        })
        .count();
    let best_rank = selected.iter().enumerate().find_map(|(index, candidate)| {
        expected
            .iter()
            .any(|needle| candidate_contains(candidate, needle))
            .then_some(index + 1)
    });
    (matched, best_rank)
}

fn candidate_contains(candidate: &MemoryPromptSelectedCandidate, needle: &str) -> bool {
    candidate
        .text
        .to_lowercase()
        .contains(&needle.to_lowercase())
        || candidate
            .source_key
            .to_lowercase()
            .contains(&needle.to_lowercase())
}

fn failed_retrieval_case(
    lane: &str,
    suite: &EvalSuite,
    case: &EvalCase,
    note: &str,
) -> RetrievalCaseReport {
    RetrievalCaseReport {
        lane: lane.to_string(),
        suite_id: suite.suite_id.clone(),
        case_id: case.case_id.clone(),
        state: "failed".to_string(),
        matched_anchors: 0,
        expected_anchors: case.expected_substrings.len(),
        existing_anchors: case.expected_substrings.len(),
        configured_anchors: case.expected_substrings.len(),
        selected_count: 0,
        selected_key_hashes: Vec::new(),
        best_rank: None,
        reciprocal_rank: 0.0,
        backends: Vec::new(),
        latency_ms: 0.0,
        latency_sample: "not_measured".to_string(),
        notes: vec![note.to_string()],
    }
}

fn summarize_retrieval(cases: &[RetrievalCaseReport]) -> MetricSummary {
    let applicable = cases
        .iter()
        .filter(|case| case.state != "skipped")
        .collect::<Vec<_>>();
    let expected = applicable
        .iter()
        .map(|case| case.expected_anchors)
        .sum::<usize>();
    let matched = applicable
        .iter()
        .map(|case| case.matched_anchors)
        .sum::<usize>();
    let backends = applicable
        .iter()
        .flat_map(|case| case.backends.iter())
        .collect::<Vec<_>>();
    let warm_latencies = applicable
        .iter()
        .filter(|case| case.latency_sample != "cold")
        .map(|case| case.latency_ms)
        .collect::<Vec<_>>();
    let cold_latencies = applicable
        .iter()
        .filter(|case| case.latency_sample == "cold")
        .map(|case| case.latency_ms)
        .collect::<Vec<_>>();
    MetricSummary {
        total: cases.len(),
        passed: cases.iter().filter(|case| case.state == "passed").count(),
        failed: cases.iter().filter(|case| case.state == "failed").count(),
        skipped: cases.iter().filter(|case| case.state == "skipped").count(),
        recall: ratio(matched, expected),
        mrr: mean(applicable.iter().map(|case| case.reciprocal_rank)),
        hybrid_coverage: if backends.is_empty() {
            1.0
        } else {
            ratio(
                backends
                    .iter()
                    .filter(|backend| backend.as_str() == "lancedb_hybrid")
                    .count(),
                backends.len(),
            )
        },
        latency_p50_ms: percentile(warm_latencies.clone(), 0.50),
        latency_p95_ms: percentile(warm_latencies, 0.95),
        cold_latency_p95_ms: percentile(cold_latencies, 0.95),
    }
}

fn fixture_coverage(cases: &[RetrievalCaseReport]) -> f64 {
    ratio(
        cases
            .iter()
            .map(|case| case.existing_anchors)
            .sum::<usize>(),
        cases
            .iter()
            .map(|case| case.configured_anchors)
            .sum::<usize>(),
    )
}

fn gate(name: &str, actual: f64, required: f64) -> GateResult {
    GateResult {
        name: name.to_string(),
        passed: actual + f64::EPSILON >= required,
        actual: format!("{:.1}%", actual * 100.0),
        required: format!(">= {:.1}%", required * 100.0),
    }
}

fn gate_at_most(name: &str, actual: f64, required: f64, unit: &str) -> GateResult {
    GateResult {
        name: name.to_string(),
        passed: actual <= required,
        actual: format!("{actual:.1}{unit}"),
        required: format!("<= {required:.1}{unit}"),
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn mean(values: impl IntoIterator<Item = f64>) -> f64 {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn percentile(mut values: Vec<f64>, quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let index = ((values.len() - 1) as f64 * quantile).ceil() as usize;
    values[index.min(values.len() - 1)]
}

fn short_hash(value: &str) -> String {
    source_text_hash(value).chars().take(12).collect()
}

fn default_true() -> bool {
    true
}
fn default_max_entries() -> usize {
    8
}
fn default_max_chars() -> usize {
    4_000
}

fn write_report(output_dir: &Path, report: &EvalReport) -> Result<()> {
    fs::create_dir_all(output_dir).with_context(|| format!("creating {}", output_dir.display()))?;
    let json_path = output_dir.join("report.json");
    fs::write(&json_path, serde_json::to_vec_pretty(report)?)
        .with_context(|| format!("writing {}", json_path.display()))?;
    let html_path = output_dir.join("report.html");
    fs::write(&html_path, render_html(report))
        .with_context(|| format!("writing {}", html_path.display()))?;
    Ok(())
}

fn render_html(report: &EvalReport) -> String {
    let status = if report.passed { "Passed" } else { "Failed" };
    let gate_rows = report
        .gates
        .iter()
        .map(|gate| {
            format!(
                "<tr><td>{}</td><td class=\"{}\">{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&gate.name),
                if gate.passed { "pass" } else { "fail" },
                if gate.passed { "PASS" } else { "FAIL" },
                escape_html(&gate.actual),
                escape_html(&gate.required)
            )
        })
        .collect::<String>();
    let case_rows = report.real_cases.iter().chain(report.synthetic_cases.iter()).map(|case| format!(
        "<tr><td>{}</td><td>{}/{}</td><td class=\"{}\">{}</td><td>{}/{}</td><td>{:.3}</td><td>{:.1} ms ({})</td><td>{}</td><td>{}</td></tr>",
        escape_html(&case.lane), escape_html(&case.suite_id), escape_html(&case.case_id), escape_html(&case.state), escape_html(&case.state.to_uppercase()), case.matched_anchors, case.expected_anchors, case.reciprocal_rank, case.latency_ms, escape_html(&case.latency_sample), escape_html(&case.backends.join(", ")), escape_html(&case.notes.join("; "))
    )).collect::<String>();
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Memory temperature live eval · {status}</title><style>
body{{font:14px/1.5 system-ui;margin:0;background:#09111f;color:#eef4ff}}main{{max-width:1180px;margin:auto;padding:40px 22px}}h1{{font-size:38px;margin:.2rem 0}}h2{{margin-top:34px}}.sub{{color:#9fb0cc}}.cards{{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;margin:24px 0}}.card{{background:#121e32;border:1px solid #293d5d;border-radius:14px;padding:16px}}.card b{{font-size:25px;display:block}}table{{width:100%;border-collapse:collapse;background:#101b2d;border-radius:12px;overflow:hidden}}th,td{{padding:10px;border-bottom:1px solid #263852;text-align:left}}th{{color:#a9bad4}}.pass,.passed{{color:#55d6a7}}.fail,.failed{{color:#ff7f91}}.skipped{{color:#efca66}}code{{color:#b9cbef}}@media(max-width:800px){{.cards{{grid-template-columns:repeat(2,1fr)}}table{{font-size:12px}}}}
</style></head><body><main><p class="sub">Magician · mixed real + synthetic + Ollama-backed evaluation</p><h1>Memory temperature live eval: <span class="{}">{status}</span></h1><p class="sub">Generated {}. Real-memory evidence is reported only as bounded counts and hashes; the production scope is not mutated.</p>
<div class="cards"><div class="card"><b>{:.1}%</b>Real anchor recall</div><div class="card"><b>{:.1}%</b>Fixture coverage</div><div class="card"><b>{:.3}</b>Real MRR</div><div class="card"><b>{:.1} ms</b>Warm p95</div><div class="card"><b>{:.1} ms</b>Cold p95</div><div class="card"><b>{:.1}%</b>Utility accuracy</div><div class="card"><b>{:.1} ms</b>Utility review p95<br><span class="sub">one-batch background sample, not response TTFT</span></div></div>
<h2>Quality gates</h2><table><thead><tr><th>Gate</th><th>Status</th><th>Actual</th><th>Required</th></tr></thead><tbody>{gate_rows}</tbody></table>
<h2>Retrieval cases</h2><table><thead><tr><th>Lane</th><th>Case</th><th>Status</th><th>Recall</th><th>RR</th><th>Latency</th><th>Backend</th><th>Notes</th></tr></thead><tbody>{case_rows}</tbody></table>
<h2>Utility reviewer</h2><pre>{}</pre></main></body></html>"#,
        if report.passed { "pass" } else { "fail" },
        report.generated_at,
        report.real.recall * 100.0,
        report.real_fixture_coverage * 100.0,
        report.real.mrr,
        report.real.latency_p95_ms,
        report.real.cold_latency_p95_ms,
        report.utility_accuracy * 100.0,
        report.utility_latency_p95_ms,
        escape_html(&serde_json::to_string_pretty(&report.utility_cases).unwrap_or_default())
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn print_summary(output_dir: &Path, report: &EvalReport) {
    println!(
        "memory-temperature live eval: {}",
        if report.passed { "PASS" } else { "FAIL" }
    );
    println!(
        "  real recall={:.1}% fixture coverage={:.1}% mrr={:.3} hybrid={:.1}% warm p95={:.1}ms cold p95={:.1}ms",
        report.real.recall * 100.0,
        report.real_fixture_coverage * 100.0,
        report.real.mrr,
        report.real.hybrid_coverage * 100.0,
        report.real.latency_p95_ms,
        report.real.cold_latency_p95_ms
    );
    println!(
        "  synthetic recall={:.1}% utility accuracy={:.1}% utility p95={:.1}ms",
        report.synthetic.recall * 100.0,
        report.utility_accuracy * 100.0,
        report.utility_latency_p95_ms
    );
    println!(
        "  report: {}",
        output_dir
            .join("report.html")
            .canonicalize()
            .unwrap_or_else(|_| output_dir.join("report.html"))
            .display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_observed_upper_rank() {
        assert_eq!(percentile(vec![10.0, 30.0, 20.0, 40.0], 0.50), 30.0);
        assert_eq!(percentile(vec![10.0, 30.0, 20.0, 40.0], 0.95), 40.0);
    }

    #[test]
    fn empty_denominator_is_vacuously_complete() {
        assert_eq!(ratio(0, 0), 1.0);
    }

    #[test]
    fn html_escapes_report_control_text() {
        assert_eq!(escape_html("<script>&\""), "&lt;script&gt;&amp;&quot;");
    }

    #[test]
    fn builtin_retrieval_suites_are_well_formed() {
        let suites = load_builtin_suites().unwrap();
        assert!(suites
            .iter()
            .any(|suite| suite.suite_id == "core-memory-smoke"));
        assert!(suites
            .iter()
            .flat_map(|suite| &suite.cases)
            .all(|case| !case.case_id.trim().is_empty()));
    }
}
