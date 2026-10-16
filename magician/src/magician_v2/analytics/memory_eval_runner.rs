//! Periodic memory retrieval eval runner.
//!
//! Eval cases live on disk and exercise the same memory prompt renderer used by
//! agent loops. Results are emitted as `eval_case` rows in memory_events
//! Parquet so the `/memory` dashboard can show pass/fail trends beside normal
//! retrieval and consolidation telemetry.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{fs, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::magician_v2::{
    agents::{
        render_memory_tiers_for_prompt, render_memory_tiers_for_prompt_with_index_result,
        AgentDefinitionStore, AgentMemoryResolver, MemoryPromptRetrievalBackend,
        MemoryRenderRequest,
    },
    analytics::memory_parquet::{emit_rows_for_storage, json_payload, MemoryAnalyticsRow},
    artifact_v2::{
        io::write_bytes_durably,
        workspace::{ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE},
    },
};

const DEFAULT_INTERVAL_SECS: u64 = 6 * 60 * 60;
const DEFAULT_STARTUP_DELAY_SECS: u64 = 60;
const DEFAULT_MAX_ENTRIES: usize = 8;
const DEFAULT_MAX_CHARS: usize = 4_000;
const MAX_RENDERED_EXCERPT_CHARS: usize = 4_000;
const MAX_REGRESSION_FAILURES: usize = 50;
const MEMORY_EVAL_ENABLED_ENV: &str = "MAGICIAN_MEMORY_EVAL_ENABLED";
const MEMORY_REGRESSION_STATUS_SCHEMA_VERSION: u32 = 1;
pub const MEMORY_REGRESSION_STATUS_EVENT_KIND: &str = "memory_regression_status";
const BUILTIN_CORE_MEMORY_SMOKE: &str =
    include_str!("../../../../data/magician_v2/memory_evals/core-memory-smoke.json");
const BUILTIN_PERSONAL_ASSISTANT: &str =
    include_str!("../../../../data/magician_v2/memory_evals/personal-assistant-regression.json");
const BUILTIN_INTERNAL_SYSTEM_ANALYST: &str = include_str!(
    "../../../../data/magician_v2/memory_evals/internal-system-analyst-regression.json"
);
const BUILTIN_SIMPLE_DATA_ANALYST: &str =
    include_str!("../../../../data/magician_v2/memory_evals/simple-data-analyst-regression.json");
const BUILTIN_WEB_RESEARCHER: &str =
    include_str!("../../../../data/magician_v2/memory_evals/web-researcher-regression.json");
const BUILTIN_MEMORY_LIFECYCLE: &str =
    include_str!("../../../../data/magician_v2/memory_evals/memory-lifecycle-smoke.json");
/// Ships `enabled: false`: the per-meeting `meeting:<thread-id>` tier entries
/// it asserts only exist after the first live meeting capture on v0.6.749+.
/// Flip the suite's `enabled` (or override it with a scoped suite of the same
/// id under `<scope>/memory/evals/`) once a meeting has been captured.
const BUILTIN_MEETINGS_MEMORY: &str =
    include_str!("../../../../data/magician_v2/memory_evals/meetings-memory-regression.json");
/// Ships `enabled: true` with `skip_if_absent` on every case. The ambient-derived
/// `user.knowledge` / `user.work_evidence` entries it asserts (provenance
/// `evd:amb:*`, rationale "ambient browsing evidence") only exist after the user
/// enables ambient capture, a cluster is distilled, and the review-gated memory
/// candidate is approved (WEG Phase C). `skip_if_absent` scope-gates the guard: a
/// scope holding none of that data reports the case `skipped` (not `failed`), so
/// the suite is safe to run globally and only *asserts* — that an approved ambient
/// fact is actually retrieved + selected for an on-topic work query — against
/// scopes that actually accrued ambient data (a scope that HAS the data but fails
/// to retrieve it still fails, as a genuine regression).
const BUILTIN_WEG_AMBIENT_MEMORY: &str =
    include_str!("../../../../data/magician_v2/memory_evals/weg-ambient-memory-regression.json");

#[derive(Debug)]
pub struct MemoryEvalRunner {
    handle: Option<JoinHandle<()>>,
    cancel: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct MemoryEvalRunnerConfig {
    pub interval: Duration,
    pub startup_delay: Duration,
}

impl Default for MemoryEvalRunnerConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
            startup_delay: Duration::from_secs(DEFAULT_STARTUP_DELAY_SECS),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRegressionStatusSnapshot {
    pub schema_version: u32,
    pub principal: String,
    pub workspace: String,
    pub generated_at_ms: i64,
    pub status: String,
    pub status_reason: String,
    pub suite_count: usize,
    pub case_count: usize,
    pub passed_count: usize,
    pub failed_count: usize,
    pub direct_fallback_count: usize,
    pub failing_cases: Vec<MemoryRegressionCaseFailure>,
}

impl MemoryRegressionStatusSnapshot {
    pub fn unknown(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: MEMORY_REGRESSION_STATUS_SCHEMA_VERSION,
            principal: principal.into(),
            workspace: workspace.into(),
            generated_at_ms: chrono::Utc::now().timestamp_millis(),
            status: "unknown".to_string(),
            status_reason: reason.into(),
            suite_count: 0,
            case_count: 0,
            passed_count: 0,
            failed_count: 0,
            direct_fallback_count: 0,
            failing_cases: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRegressionCaseFailure {
    pub suite_id: String,
    pub case_id: String,
    pub agent_id: String,
    pub goal_id: Option<String>,
    pub retrieval_backend: String,
    pub status: String,
    pub expected_count: usize,
    pub matched_count: usize,
    pub selected_count: usize,
    pub best_rank: Option<usize>,
    pub payload_excerpt: Option<String>,
}

pub fn memory_regression_status_path(memory_root: &Path) -> PathBuf {
    memory_root
        .join("eval_status")
        .join("regression_status.json")
}

pub async fn read_scope_regression_status(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<Option<MemoryRegressionStatusSnapshot>> {
    let path = memory_regression_status_path(&workspace_layout.memory_root(principal, workspace));
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", path.display()))
        .map(Some)
}

impl MemoryEvalRunnerConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Some(interval) = read_duration_env("MAGICIAN_MEMORY_EVAL_INTERVAL_SECS") {
            config.interval = interval;
        }
        if let Some(delay) = read_duration_env("MAGICIAN_MEMORY_EVAL_STARTUP_DELAY_SECS") {
            config.startup_delay = delay;
        }
        config
    }
}

impl MemoryEvalRunner {
    pub fn spawn(
        workspace_layout: ArtifactV2Workspace,
        definition_store: AgentDefinitionStore,
        memory_resolver: AgentMemoryResolver,
        config: MemoryEvalRunnerConfig,
    ) -> Self {
        let cancel = CancellationToken::new();
        if !memory_eval_runner_enabled() {
            info!(
                target: "analytics::memory_eval_runner",
                env = MEMORY_EVAL_ENABLED_ENV,
                "memory eval runner disabled; set MAGICIAN_MEMORY_EVAL_ENABLED=true to enable periodic evaluation"
            );
            return Self {
                handle: None,
                cancel,
            };
        }

        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            run_periodic(
                workspace_layout,
                definition_store,
                memory_resolver,
                config,
                cancel_for_task,
            )
            .await;
        });
        Self {
            handle: Some(handle),
            cancel,
        }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle {
            let _ = handle.await;
        }
    }
}

async fn run_periodic(
    workspace_layout: ArtifactV2Workspace,
    definition_store: AgentDefinitionStore,
    memory_resolver: AgentMemoryResolver,
    config: MemoryEvalRunnerConfig,
    cancel: CancellationToken,
) {
    if !crate::magician_v2::runtime::startup::wait_for_http_or_cancel(&cancel).await {
        return;
    }

    if !config.startup_delay.is_zero() {
        tokio::select! {
            _ = tokio::time::sleep(config.startup_delay) => {},
            _ = cancel.cancelled() => return,
        }
    }

    run_once_with_logging(&workspace_layout, &definition_store, &memory_resolver).await;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(config.interval) => {
                run_once_with_logging(&workspace_layout, &definition_store, &memory_resolver).await;
            }
            _ = cancel.cancelled() => break,
        }
    }
}

async fn run_once_with_logging(
    workspace_layout: &ArtifactV2Workspace,
    definition_store: &AgentDefinitionStore,
    memory_resolver: &AgentMemoryResolver,
) {
    match run_once(workspace_layout, definition_store, memory_resolver).await {
        Ok(outcome) if outcome.case_count > 0 => {
            info!(
                target: "analytics::memory_eval_runner",
                suites = outcome.suite_count,
                cases = outcome.case_count,
                passed = outcome.passed_count,
                failed = outcome.failed_count,
                "memory eval runner completed"
            );
        },
        Ok(_) => {
            debug!(
                target: "analytics::memory_eval_runner",
                "memory eval runner found no eval cases"
            );
        },
        Err(error) => {
            warn!(
                target: "analytics::memory_eval_runner",
                error = %error,
                "memory eval runner failed"
            );
        },
    }
}

pub async fn run_once(
    workspace_layout: &ArtifactV2Workspace,
    definition_store: &AgentDefinitionStore,
    memory_resolver: &AgentMemoryResolver,
) -> Result<MemoryEvalRunOutcome> {
    let system_suites = load_suites_from_dir(&workspace_layout.system_root().join("memory_evals"))
        .await
        .context("loading system memory eval suites")?;
    let mut scopes = workspace_layout
        .list_scope_segments()
        .await
        .context("listing scoped workspaces for memory evals")?;
    if scopes.is_empty() {
        scopes.push((
            DEFAULT_SCOPE_PRINCIPAL.to_string(),
            DEFAULT_SCOPE_WORKSPACE.to_string(),
        ));
    }

    let mut outcome = MemoryEvalRunOutcome::default();
    for (principal, workspace) in scopes {
        let scoped_suites = load_suites_for_scope(workspace_layout, &principal, &workspace).await?;
        let suites = system_suites
            .iter()
            .chain(scoped_suites.iter())
            .collect::<Vec<_>>();
        outcome.add(
            run_loaded_suites_for_scope(
                definition_store,
                memory_resolver,
                &principal,
                &workspace,
                &suites,
            )
            .await?,
        );
    }

    Ok(outcome)
}

pub async fn run_scope_once(
    workspace_layout: &ArtifactV2Workspace,
    definition_store: &AgentDefinitionStore,
    memory_resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
) -> Result<MemoryEvalRunOutcome> {
    let system_suites = load_suites_from_dir(&workspace_layout.system_root().join("memory_evals"))
        .await
        .context("loading system memory eval suites")?;
    let scoped_suites = load_suites_for_scope(workspace_layout, principal, workspace).await?;
    let suites = system_suites
        .iter()
        .chain(scoped_suites.iter())
        .collect::<Vec<_>>();
    run_loaded_suites_for_scope(
        definition_store,
        memory_resolver,
        principal,
        workspace,
        &suites,
    )
    .await
}

async fn load_suites_for_scope(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<Vec<MemoryEvalSuite>> {
    let mut scoped_suites = load_suites_from_dir(
        &workspace_layout
            .memory_root(principal, workspace)
            .join("evals"),
    )
    .await
    .with_context(|| format!("loading scoped memory eval suites for {principal}/{workspace}"))?;
    for builtin in builtin_suites_for_scope(principal, workspace)? {
        if !scoped_suites
            .iter()
            .any(|suite| suite.suite_id == builtin.suite_id)
        {
            scoped_suites.push(builtin);
        }
    }
    Ok(scoped_suites)
}

async fn run_loaded_suites_for_scope(
    definition_store: &AgentDefinitionStore,
    memory_resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    suites: &[&MemoryEvalSuite],
) -> Result<MemoryEvalRunOutcome> {
    if suites.is_empty() {
        return Ok(MemoryEvalRunOutcome::default());
    }

    let scoped_store = definition_store.for_scope(principal, workspace);
    let memory_service = memory_resolver
        .resolve_for_scope(principal, workspace)
        .with_context(|| format!("resolving memory service for {principal}/{workspace}"))?;
    let mut outcome = MemoryEvalRunOutcome::default();
    for &suite in suites {
        if !suite.enabled {
            continue;
        }
        outcome.suite_count += 1;
        for case in &suite.cases {
            if matches!(case.kind, MemoryEvalKind::Lifecycle) {
                outcome.case_count += 1;
                let result = run_lifecycle_case(&scoped_store, &memory_service, suite, case).await;
                match result {
                    Ok(eval) => {
                        outcome.record_result(&eval);
                        emit_rows_for_storage(memory_service.storage(), vec![eval.into_row()]);
                    },
                    Err(error) => {
                        // `format!("{error:#}")` (anyhow's alternate
                        // Display) walks the source chain so the
                        // persisted excerpt contains the root cause —
                        // not just the outermost with_context wrapper.
                        // Plain `error.to_string()` drops everything
                        // below the top frame, which is exactly what
                        // hid the persona-cap validation error behind
                        // the generic "loading agent definition X"
                        // message in earlier runs.
                        let error_chain = format!("{error:#}");
                        outcome.record_error(
                            suite,
                            case,
                            "lifecycle",
                            "lifecycle_error",
                            case.lifecycle_expected_count() as usize,
                            error_chain.clone(),
                        );
                        let mut row = MemoryAnalyticsRow::now("eval_case", "memory_eval_runner");
                        row.agent_id = Some(case.agent_id.clone());
                        row.goal_id = case.goal_id.clone();
                        row.scope = Some(scope_labels(&case.effective_scopes()).join(","));
                        row.eval_suite = Some(suite.suite_id.clone());
                        row.eval_case_id = Some(case.case_id.clone());
                        row.eval_query = Some(case.query.clone());
                        row.eval_pass = Some(false);
                        row.expected_count = Some(case.lifecycle_expected_count());
                        row.matched_count = Some(0);
                        row.retrieval_backend = Some("lifecycle".to_string());
                        row.status = "lifecycle_error".to_string();
                        row.payload_json = json_payload(&json!({
                            "retrieval_backend": "lifecycle",
                            "error": error_chain,
                        }));
                        emit_rows_for_storage(memory_service.storage(), vec![row]);
                    },
                }
                continue;
            }
            for backend in [
                EvalRetrievalBackend::Direct,
                EvalRetrievalBackend::LancedbHybrid,
            ] {
                outcome.case_count += 1;
                let result = run_case(&scoped_store, &memory_service, suite, case, backend).await;
                match result {
                    Ok(eval) => {
                        outcome.record_result(&eval);
                        emit_rows_for_storage(memory_service.storage(), vec![eval.into_row()]);
                    },
                    Err(error) => {
                        // `format!("{error:#}")` — see lifecycle arm
                        // above for why; same trap, same fix.
                        let error_chain = format!("{error:#}");
                        outcome.record_error(
                            suite,
                            case,
                            backend.as_str(),
                            "render_error",
                            case.expected_substrings.len(),
                            error_chain.clone(),
                        );
                        let mut row = MemoryAnalyticsRow::now("eval_case", "memory_eval_runner");
                        row.agent_id = Some(case.agent_id.clone());
                        row.goal_id = case.goal_id.clone();
                        row.scope = Some(scope_labels(&case.effective_scopes()).join(","));
                        row.eval_suite = Some(suite.suite_id.clone());
                        row.eval_case_id = Some(case.case_id.clone());
                        row.eval_query = Some(case.query.clone());
                        row.eval_pass = Some(false);
                        row.expected_count =
                            Some(case.expected_substrings.len().min(u32::MAX as usize) as u32);
                        row.matched_count = Some(0);
                        row.retrieval_backend = Some(backend.as_str().to_string());
                        row.status = "render_error".to_string();
                        row.payload_json = json_payload(&json!({
                            "retrieval_backend": backend.as_str(),
                            "error": error_chain,
                        }));
                        emit_rows_for_storage(memory_service.storage(), vec![row]);
                    },
                }
            }
        }
    }
    let snapshot = outcome.regression_snapshot(principal, workspace);
    if let Err(error) = write_regression_status(memory_service.storage().root(), &snapshot).await {
        warn!(
            target: "analytics::memory_eval_runner",
            principal,
            workspace,
            error = %error,
            "failed to persist memory regression status"
        );
    }
    emit_regression_status_row(memory_service.storage(), &snapshot);
    Ok(outcome)
}

/// Caps for the `skip_if_absent` presence probe — large enough to include an
/// entire normal-sized tier so "absent in this render" means "absent from the
/// scope", not merely "ranked below the assertion's cap".
const SKIP_PROBE_MAX_ENTRIES: usize = 1_000;
const SKIP_PROBE_MAX_CHARS: usize = 1_000_000;

async fn run_case(
    definition_store: &AgentDefinitionStore,
    memory_service: &crate::magician_v2::agents::AgentMemoryService,
    suite: &MemoryEvalSuite,
    case: &MemoryEvalCase,
    backend: EvalRetrievalBackend,
) -> Result<MemoryEvalCaseResult> {
    let Some(record) = definition_store
        .get_definition(&case.agent_id)
        .await
        .with_context(|| format!("loading agent definition {}", case.agent_id))?
    else {
        return Ok(MemoryEvalCaseResult::missing_agent(suite, case, backend));
    };

    let mut rendered_sections = Vec::new();
    let mut actual_backend = backend;
    for scope in case.effective_scopes() {
        let mut request = match scope {
            MemoryEvalScope::User => MemoryRenderRequest::user(&case.query),
            MemoryEvalScope::Agent => MemoryRenderRequest::agent(&case.query),
            MemoryEvalScope::AgentGoal => {
                MemoryRenderRequest::agent_goal(&case.query, case.goal_id.as_deref())
            },
        };
        request.max_entries = case.max_entries;
        request.max_chars = case.max_chars;
        request.include_provenance = true;
        request.emit_audit = false;

        let rendered = match backend {
            EvalRetrievalBackend::Direct | EvalRetrievalBackend::DirectFallback => {
                render_memory_tiers_for_prompt(
                    memory_service,
                    &case.agent_id,
                    &record.definition.memory_tiers,
                    &request,
                )
                .await
            },
            EvalRetrievalBackend::LancedbHybrid => {
                match render_memory_tiers_for_prompt_with_index_result(
                    memory_service,
                    definition_store,
                    &case.agent_id,
                    &record.definition.memory_tiers,
                    &request,
                )
                .await
                {
                    Ok(result) => {
                        actual_backend = match result.retrieval_backend {
                            MemoryPromptRetrievalBackend::LancedbHybrid => {
                                EvalRetrievalBackend::LancedbHybrid
                            },
                            MemoryPromptRetrievalBackend::Direct => {
                                EvalRetrievalBackend::DirectFallback
                            },
                        };
                        Ok(result.section)
                    },
                    Err(error) => Err(error),
                }
            },
        }
        .with_context(|| {
            format!(
                "rendering memory scope {} for eval case {} with {}",
                scope.as_str(),
                case.case_id,
                backend.as_str()
            )
        })?;
        if let Some(rendered) = rendered {
            rendered_sections.push(rendered);
        }
    }

    let result = evaluate_rendered_sections(suite, case, actual_backend, rendered_sections);

    // skip_if_absent scope-gate: if the capped retrieval surfaced NONE of the
    // expected substrings, probe the tiers broadly. If the data isn't there at
    // all, the case is not applicable to this scope → skipped. If it IS there,
    // retrieval genuinely missed it within the cap → keep the failure (a real
    // regression). Only probes on a zero-match miss, so passing cases pay nothing.
    // A forbidden-substring violation is never "not applicable to this scope" —
    // the thing that must not appear did appear. And a case with no expected
    // substrings has nothing for the probe to look for, so it would always come
    // back absent and skip. Both must stay failures.
    let forbidden_violation = result
        .payload
        .get("forbidden_present")
        .and_then(|value| value.as_array())
        .is_some_and(|values| !values.is_empty());
    if case.skip_if_absent
        && !result.pass
        && result.matched_count == 0
        && !case.expected_substrings.is_empty()
        && !forbidden_violation
    {
        let mut present = false;
        for scope in case.effective_scopes() {
            let mut request = match scope {
                MemoryEvalScope::User => MemoryRenderRequest::user(&case.query),
                MemoryEvalScope::Agent => MemoryRenderRequest::agent(&case.query),
                MemoryEvalScope::AgentGoal => {
                    MemoryRenderRequest::agent_goal(&case.query, case.goal_id.as_deref())
                },
            };
            request.max_entries = SKIP_PROBE_MAX_ENTRIES;
            request.max_chars = SKIP_PROBE_MAX_CHARS;
            request.include_provenance = true;
            request.emit_audit = false;
            if let Some(rendered) = render_memory_tiers_for_prompt(
                memory_service,
                &case.agent_id,
                &record.definition.memory_tiers,
                &request,
            )
            .await
            .with_context(|| format!("skip-probe render for eval case {}", case.case_id))?
            {
                if case
                    .expected_substrings
                    .iter()
                    .any(|expected| contains_case_insensitive(&rendered, expected))
                {
                    present = true;
                    break;
                }
            }
        }
        if !present {
            return Ok(MemoryEvalCaseResult::skipped(
                suite,
                case,
                actual_backend,
                result.selected_count,
            ));
        }
    }

    Ok(result)
}

async fn run_lifecycle_case(
    definition_store: &AgentDefinitionStore,
    memory_service: &crate::magician_v2::agents::AgentMemoryService,
    suite: &MemoryEvalSuite,
    case: &MemoryEvalCase,
) -> Result<MemoryEvalCaseResult> {
    let Some(record) = definition_store
        .get_definition(&case.agent_id)
        .await
        .with_context(|| format!("loading agent definition {}", case.agent_id))?
    else {
        return Ok(MemoryEvalCaseResult {
            suite_id: suite.suite_id.clone(),
            case_id: case.case_id.clone(),
            agent_id: case.agent_id.clone(),
            goal_id: case.goal_id.clone(),
            query: case.query.clone(),
            scopes: scope_labels(&case.effective_scopes()).join(","),
            retrieval_backend: "lifecycle".to_string(),
            pass: false,
            status: "missing_agent".to_string(),
            expected_count: case.lifecycle_expected_count() as usize,
            matched_count: 0,
            selected_count: 0,
            selected_item_keys: Vec::new(),
            best_rank: None,
            payload: json!({
                "retrieval_backend": "lifecycle",
                "error": "agent definition not found"
            }),
        });
    };

    let episodes = memory_service
        .load_native_episodes(&case.agent_id)
        .await
        .with_context(|| format!("loading episodes for {}", case.agent_id))?;
    let mut candidate_type_counts = std::collections::BTreeMap::<String, usize>::new();
    for episode in &episodes {
        for candidate in &episode.memory_candidates {
            *candidate_type_counts
                .entry(candidate.candidate_type.clone())
                .or_default() += 1;
        }
    }
    let found_candidate_count = candidate_type_counts.values().copied().sum::<usize>();
    let missing_candidate_types = case
        .expected_episode_candidate_types
        .iter()
        .filter(|expected| !candidate_type_counts.contains_key(expected.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let min_episode_candidates_ok = case
        .min_episode_candidates
        .map(|minimum| found_candidate_count >= minimum)
        .unwrap_or(true);

    let consolidation_records =
        load_consolidation_audit_records(memory_service, &case.agent_id).await?;
    let consolidation_targets = consolidation_records
        .iter()
        .filter_map(|record| record.get("target").and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let missing_consolidation_targets = case
        .expected_consolidation_targets
        .iter()
        .filter(|expected| {
            !consolidation_targets
                .iter()
                .any(|target| target == *expected)
        })
        .cloned()
        .collect::<Vec<_>>();
    let min_consolidation_records_ok = case
        .min_consolidation_records
        .map(|minimum| consolidation_records.len() >= minimum)
        .unwrap_or(true);

    let mut rendered_sections = Vec::new();
    for scope in case.effective_scopes() {
        let mut request = match scope {
            MemoryEvalScope::User => MemoryRenderRequest::user(&case.query),
            MemoryEvalScope::Agent => MemoryRenderRequest::agent(&case.query),
            MemoryEvalScope::AgentGoal => {
                MemoryRenderRequest::agent_goal(&case.query, case.goal_id.as_deref())
            },
        };
        request.max_entries = case.max_entries;
        request.max_chars = case.max_chars;
        request.include_provenance = true;
        request.emit_audit = false;
        if let Some(rendered) = render_memory_tiers_for_prompt(
            memory_service,
            &case.agent_id,
            &record.definition.memory_tiers,
            &request,
        )
        .await
        .with_context(|| format!("rendering lifecycle memory case {}", case.case_id))?
        {
            rendered_sections.push(rendered);
        }
    }
    let rendered_text = rendered_sections.join("\n\n");
    let selected_count = count_rendered_entries(&rendered_text);
    let selected_item_keys = extract_selected_item_keys(&rendered_text);
    let mut matched_substrings = Vec::new();
    let mut missing_substrings = Vec::new();
    for expected in &case.expected_substrings {
        if contains_case_insensitive(&rendered_text, expected) {
            matched_substrings.push(expected.clone());
        } else {
            missing_substrings.push(expected.clone());
        }
    }
    let min_selected_ok = case
        .min_selected_count
        .map(|minimum| selected_count >= minimum)
        .unwrap_or(true);

    let expected_count = case.lifecycle_expected_count() as usize;
    let matched_count = case
        .expected_substrings
        .len()
        .saturating_sub(missing_substrings.len())
        + case
            .expected_episode_candidate_types
            .len()
            .saturating_sub(missing_candidate_types.len())
        + case
            .expected_consolidation_targets
            .len()
            .saturating_sub(missing_consolidation_targets.len())
        + usize::from(min_episode_candidates_ok && case.min_episode_candidates.is_some())
        + usize::from(min_consolidation_records_ok && case.min_consolidation_records.is_some())
        + usize::from(min_selected_ok && case.min_selected_count.is_some());
    let pass = missing_substrings.is_empty()
        && missing_candidate_types.is_empty()
        && missing_consolidation_targets.is_empty()
        && min_episode_candidates_ok
        && min_consolidation_records_ok
        && min_selected_ok;

    Ok(MemoryEvalCaseResult {
        suite_id: suite.suite_id.clone(),
        case_id: case.case_id.clone(),
        agent_id: case.agent_id.clone(),
        goal_id: case.goal_id.clone(),
        query: case.query.clone(),
        scopes: scope_labels(&case.effective_scopes()).join(","),
        retrieval_backend: "lifecycle".to_string(),
        pass,
        status: if pass { "passed" } else { "failed" }.to_string(),
        expected_count,
        matched_count,
        selected_count,
        selected_item_keys: selected_item_keys.clone(),
        best_rank: best_rank_for_expected(&rendered_text, &case.expected_substrings)
            .or_else(|| (selected_count > 0).then_some(1)),
        payload: json!({
            "retrieval_backend": "lifecycle",
            "episode_count": episodes.len(),
            "memory_candidate_count": found_candidate_count,
            "candidate_type_counts": candidate_type_counts,
            "expected_episode_candidate_types": case.expected_episode_candidate_types,
            "missing_episode_candidate_types": missing_candidate_types,
            "min_episode_candidates": case.min_episode_candidates,
            "consolidation_record_count": consolidation_records.len(),
            "consolidation_targets": consolidation_targets,
            "expected_consolidation_targets": case.expected_consolidation_targets,
            "missing_consolidation_targets": missing_consolidation_targets,
            "min_consolidation_records": case.min_consolidation_records,
            "expected_substrings": case.expected_substrings,
            "matched_substrings": matched_substrings,
            "missing_substrings": missing_substrings,
            "min_selected_count": case.min_selected_count,
            "selected_count": selected_count,
            "selected_item_keys": selected_item_keys,
            "rendered_excerpt": bounded_chars(&rendered_text, MAX_RENDERED_EXCERPT_CHARS),
        }),
    })
}

async fn load_consolidation_audit_records(
    memory_service: &crate::magician_v2::agents::AgentMemoryService,
    agent_id: &str,
) -> Result<Vec<serde_json::Value>> {
    let path = memory_service
        .storage()
        .agent_consolidations_dir(agent_id)?
        .join("memory_consolidation_audit.jsonl");
    let Ok(raw) = fs::read_to_string(&path).await else {
        return Ok(Vec::new());
    };
    Ok(raw
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                None
            } else {
                serde_json::from_str::<serde_json::Value>(trimmed).ok()
            }
        })
        .collect())
}

fn evaluate_rendered_sections(
    suite: &MemoryEvalSuite,
    case: &MemoryEvalCase,
    backend: EvalRetrievalBackend,
    rendered_sections: Vec<String>,
) -> MemoryEvalCaseResult {
    let rendered_text = rendered_sections.join("\n\n");
    let selected_count = count_rendered_entries(&rendered_text);
    let selected_item_keys = extract_selected_item_keys(&rendered_text);
    if case.expected_substrings.is_empty()
        && case.min_selected_count.is_none()
        && case
            .forbidden_substrings
            .iter()
            .all(|forbidden| forbidden.trim().is_empty())
    {
        return MemoryEvalCaseResult::invalid_case(
            suite,
            case,
            backend,
            selected_count,
            selected_item_keys,
            rendered_text,
        );
    }

    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for expected in &case.expected_substrings {
        if contains_case_insensitive(&rendered_text, expected) {
            matched.push(expected.clone());
        } else {
            missing.push(expected.clone());
        }
    }
    // An empty needle matches every haystack, so an accidental `""` in a suite
    // would fail every case it appears in with a reason that reads like a real
    // exclusion violation. Ignore blanks rather than assert on them.
    let forbidden_present = case
        .forbidden_substrings
        .iter()
        .filter(|forbidden| !forbidden.trim().is_empty())
        .filter(|forbidden| contains_case_insensitive(&rendered_text, forbidden))
        .cloned()
        .collect::<Vec<_>>();
    let expected_count = case.expected_substrings.len();
    let matched_count = matched.len();
    let min_selected_ok = case
        .min_selected_count
        .map(|minimum| selected_count >= minimum)
        .unwrap_or(true);
    let pass = matched_count == expected_count && min_selected_ok && forbidden_present.is_empty();
    let best_rank = best_rank_for_expected(&rendered_text, &case.expected_substrings)
        .or_else(|| (selected_count > 0).then_some(1));

    MemoryEvalCaseResult {
        suite_id: suite.suite_id.clone(),
        case_id: case.case_id.clone(),
        agent_id: case.agent_id.clone(),
        goal_id: case.goal_id.clone(),
        query: case.query.clone(),
        scopes: scope_labels(&case.effective_scopes()).join(","),
        retrieval_backend: backend.as_str().to_string(),
        pass,
        status: if pass { "passed" } else { "failed" }.to_string(),
        expected_count,
        matched_count,
        selected_count,
        selected_item_keys: selected_item_keys.clone(),
        best_rank,
        payload: json!({
            "retrieval_backend": backend.as_str(),
            "expected_substrings": case.expected_substrings,
            "matched_substrings": matched,
            "missing_substrings": missing,
            "forbidden_substrings": case.forbidden_substrings,
            "forbidden_present": forbidden_present,
            "min_selected_count": case.min_selected_count,
            "selected_count": selected_count,
            "selected_item_keys": selected_item_keys,
            "rendered_excerpt": bounded_chars(&rendered_text, MAX_RENDERED_EXCERPT_CHARS),
        }),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct MemoryEvalSuite {
    #[serde(default)]
    suite_id: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    cases: Vec<MemoryEvalCase>,
}

#[derive(Debug, Clone, Deserialize)]
struct MemoryEvalCase {
    pub case_id: String,
    #[serde(default)]
    pub kind: MemoryEvalKind,
    pub agent_id: String,
    #[serde(default)]
    pub goal_id: Option<String>,
    pub query: String,
    #[serde(default, alias = "expected")]
    pub expected_substrings: Vec<String>,
    /// Snippets that must NOT appear in the rendered memory.
    ///
    /// Every other assertion here is recall-shaped — "did the anchor surface?"
    /// — so a case that injects eight entries to reach its anchor scores the
    /// same as one that injects exactly the right entry. These express the
    /// other half: superseded facts that must stay excluded, a stale value a
    /// newer memory replaced, or a known distractor that must not out-rank the
    /// answer.
    #[serde(default, alias = "forbidden")]
    pub forbidden_substrings: Vec<String>,
    #[serde(default)]
    pub min_selected_count: Option<usize>,
    #[serde(default)]
    pub scopes: Vec<MemoryEvalScope>,
    #[serde(default = "default_max_entries")]
    pub max_entries: usize,
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    #[serde(default)]
    pub expected_episode_candidate_types: Vec<String>,
    #[serde(default)]
    pub min_episode_candidates: Option<usize>,
    #[serde(default)]
    pub expected_consolidation_targets: Vec<String>,
    #[serde(default)]
    pub min_consolidation_records: Option<usize>,
    /// Scope-gate for a regression guard: when true, a retrieval case whose
    /// expected substrings are ABSENT from the scope's tiers entirely (verified
    /// via a broad presence probe, independent of the capped retrieval the
    /// assertion uses) is reported `skipped` instead of `failed`. This lets a
    /// suite ship `enabled` globally and only assert against scopes that
    /// actually hold the data — a scope that HAS the data but fails to retrieve
    /// it still fails (a real regression).
    #[serde(default)]
    pub skip_if_absent: bool,
}

impl MemoryEvalCase {
    fn effective_scopes(&self) -> Vec<MemoryEvalScope> {
        if self.scopes.is_empty() {
            return vec![
                MemoryEvalScope::User,
                MemoryEvalScope::Agent,
                MemoryEvalScope::AgentGoal,
            ];
        }
        self.scopes.clone()
    }

    fn lifecycle_expected_count(&self) -> u32 {
        let expected = self.expected_substrings.len()
            + self.expected_episode_candidate_types.len()
            + self.expected_consolidation_targets.len()
            + usize::from(self.min_episode_candidates.is_some())
            + usize::from(self.min_consolidation_records.is_some())
            + usize::from(self.min_selected_count.is_some());
        expected.min(u32::MAX as usize) as u32
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MemoryEvalKind {
    #[default]
    Retrieval,
    Lifecycle,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemoryEvalScope {
    User,
    Agent,
    AgentGoal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvalRetrievalBackend {
    Direct,
    DirectFallback,
    LancedbHybrid,
}

impl EvalRetrievalBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::DirectFallback => "direct_fallback",
            Self::LancedbHybrid => "lancedb_hybrid",
        }
    }
}

impl MemoryEvalScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::AgentGoal => "agent_goal",
        }
    }
}

#[derive(Debug, Default)]
pub struct MemoryEvalRunOutcome {
    pub suite_count: usize,
    pub case_count: usize,
    pub passed_count: usize,
    pub failed_count: usize,
    /// Cases skipped because `skip_if_absent` matched (no applicable data in the
    /// scope). Not counted as passed or failed, so a globally-enabled regression
    /// guard stays green on scopes that never accrued the data it asserts.
    pub skipped_count: usize,
    pub direct_fallback_count: usize,
    pub failing_cases: Vec<MemoryRegressionCaseFailure>,
}

impl MemoryEvalRunOutcome {
    fn add(&mut self, other: Self) {
        self.suite_count += other.suite_count;
        self.case_count += other.case_count;
        self.passed_count += other.passed_count;
        self.failed_count += other.failed_count;
        self.skipped_count += other.skipped_count;
        self.direct_fallback_count += other.direct_fallback_count;
        self.failing_cases.extend(other.failing_cases);
        self.truncate_failures();
    }

    fn record_result(&mut self, result: &MemoryEvalCaseResult) {
        if result.status == "skipped" {
            // skip_if_absent matched: the scope holds none of this case's
            // required data, so the case is not applicable here. Count it as
            // skipped — never failed — which is exactly what lets a regression
            // suite ship `enabled` globally without false-failing fresh scopes.
            self.skipped_count += 1;
            return;
        }
        if result.retrieval_backend == EvalRetrievalBackend::DirectFallback.as_str() {
            self.direct_fallback_count += 1;
        }
        if result.pass {
            self.passed_count += 1;
        } else {
            self.failed_count += 1;
            self.failing_cases.push(result.to_failure());
            self.truncate_failures();
        }
    }

    fn record_error(
        &mut self,
        suite: &MemoryEvalSuite,
        case: &MemoryEvalCase,
        retrieval_backend: &str,
        status: &str,
        expected_count: usize,
        error: String,
    ) {
        self.failed_count += 1;
        self.failing_cases.push(MemoryRegressionCaseFailure {
            suite_id: suite.suite_id.clone(),
            case_id: case.case_id.clone(),
            agent_id: case.agent_id.clone(),
            goal_id: case.goal_id.clone(),
            retrieval_backend: retrieval_backend.to_string(),
            status: status.to_string(),
            expected_count,
            matched_count: 0,
            selected_count: 0,
            best_rank: None,
            payload_excerpt: Some(bounded_chars(&error, 1_200)),
        });
        self.truncate_failures();
    }

    pub fn regression_snapshot(
        &self,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> MemoryRegressionStatusSnapshot {
        let (status, status_reason) = if self.case_count == 0 {
            (
                "unknown".to_string(),
                "No enabled memory eval cases were found for this scope.".to_string(),
            )
        } else if self.failed_count > 0 {
            (
                "failing".to_string(),
                format!(
                    "{} of {} memory eval cases failed in the latest run.",
                    self.failed_count, self.case_count
                ),
            )
        } else if self.direct_fallback_count > 0 {
            (
                "degraded".to_string(),
                format!(
                    "{} memory eval cases passed only after falling back from the indexed backend.",
                    self.direct_fallback_count
                ),
            )
        } else {
            (
                "healthy".to_string(),
                if self.skipped_count > 0 {
                    format!(
                        "All {} applicable memory eval cases passed ({} skipped — no applicable data in scope).",
                        self.passed_count, self.skipped_count
                    )
                } else {
                    format!("All {} memory eval cases passed.", self.case_count)
                },
            )
        };
        MemoryRegressionStatusSnapshot {
            schema_version: MEMORY_REGRESSION_STATUS_SCHEMA_VERSION,
            principal: principal.into(),
            workspace: workspace.into(),
            generated_at_ms: chrono::Utc::now().timestamp_millis(),
            status,
            status_reason,
            suite_count: self.suite_count,
            case_count: self.case_count,
            passed_count: self.passed_count,
            failed_count: self.failed_count,
            direct_fallback_count: self.direct_fallback_count,
            failing_cases: self.failing_cases.clone(),
        }
    }

    fn truncate_failures(&mut self) {
        if self.failing_cases.len() > MAX_REGRESSION_FAILURES {
            self.failing_cases.truncate(MAX_REGRESSION_FAILURES);
        }
    }
}

#[derive(Debug)]
struct MemoryEvalCaseResult {
    suite_id: String,
    case_id: String,
    agent_id: String,
    goal_id: Option<String>,
    query: String,
    scopes: String,
    retrieval_backend: String,
    pass: bool,
    status: String,
    expected_count: usize,
    matched_count: usize,
    selected_count: usize,
    selected_item_keys: Vec<String>,
    best_rank: Option<usize>,
    payload: serde_json::Value,
}

impl MemoryEvalCaseResult {
    fn missing_agent(
        suite: &MemoryEvalSuite,
        case: &MemoryEvalCase,
        backend: EvalRetrievalBackend,
    ) -> Self {
        Self {
            suite_id: suite.suite_id.clone(),
            case_id: case.case_id.clone(),
            agent_id: case.agent_id.clone(),
            goal_id: case.goal_id.clone(),
            query: case.query.clone(),
            scopes: scope_labels(&case.effective_scopes()).join(","),
            retrieval_backend: backend.as_str().to_string(),
            pass: false,
            status: "missing_agent".to_string(),
            expected_count: case.expected_substrings.len(),
            matched_count: 0,
            selected_count: 0,
            selected_item_keys: Vec::new(),
            best_rank: None,
            payload: json!({
                "retrieval_backend": backend.as_str(),
                "error": "agent definition not found"
            }),
        }
    }

    fn invalid_case(
        suite: &MemoryEvalSuite,
        case: &MemoryEvalCase,
        backend: EvalRetrievalBackend,
        selected_count: usize,
        selected_item_keys: Vec<String>,
        rendered_text: String,
    ) -> Self {
        Self {
            suite_id: suite.suite_id.clone(),
            case_id: case.case_id.clone(),
            agent_id: case.agent_id.clone(),
            goal_id: case.goal_id.clone(),
            query: case.query.clone(),
            scopes: scope_labels(&case.effective_scopes()).join(","),
            retrieval_backend: backend.as_str().to_string(),
            pass: false,
            status: "invalid_case".to_string(),
            expected_count: 0,
            matched_count: 0,
            selected_count,
            selected_item_keys: selected_item_keys.clone(),
            best_rank: None,
            payload: json!({
                "retrieval_backend": backend.as_str(),
                "error": "eval case must define expected_substrings or min_selected_count",
                "selected_item_keys": selected_item_keys,
                "rendered_excerpt": bounded_chars(&rendered_text, MAX_RENDERED_EXCERPT_CHARS),
            }),
        }
    }

    /// A case reported neither passed nor failed: `skip_if_absent` matched and
    /// the scope holds none of the expected substrings, so the assertion does
    /// not apply here. `record_result` keys off `status == "skipped"`.
    fn skipped(
        suite: &MemoryEvalSuite,
        case: &MemoryEvalCase,
        backend: EvalRetrievalBackend,
        selected_count: usize,
    ) -> Self {
        Self {
            suite_id: suite.suite_id.clone(),
            case_id: case.case_id.clone(),
            agent_id: case.agent_id.clone(),
            goal_id: case.goal_id.clone(),
            query: case.query.clone(),
            scopes: scope_labels(&case.effective_scopes()).join(","),
            retrieval_backend: backend.as_str().to_string(),
            pass: false,
            status: "skipped".to_string(),
            expected_count: case.expected_substrings.len(),
            matched_count: 0,
            selected_count,
            selected_item_keys: Vec::new(),
            best_rank: None,
            payload: json!({
                "retrieval_backend": backend.as_str(),
                "skipped_reason": "skip_if_absent: none of the expected substrings exist in this scope's memory tiers",
                "expected_substrings": case.expected_substrings,
            }),
        }
    }

    fn into_row(self) -> MemoryAnalyticsRow {
        let mut row = MemoryAnalyticsRow::now("eval_case", "memory_eval_runner");
        row.agent_id = Some(self.agent_id);
        row.goal_id = self.goal_id;
        row.scope = Some(self.scopes);
        row.eval_suite = Some(self.suite_id);
        row.eval_case_id = Some(self.case_id);
        row.eval_query = Some(self.query);
        row.eval_pass = Some(self.pass);
        row.retrieval_backend = Some(self.retrieval_backend.clone());
        row.expected_count = Some(self.expected_count.min(u32::MAX as usize) as u32);
        row.matched_count = Some(self.matched_count.min(u32::MAX as usize) as u32);
        row.selected_count = Some(self.selected_count.min(u32::MAX as usize) as u32);
        row.selected_item_keys = json_payload(&json!(self.selected_item_keys));
        row.best_rank = self
            .best_rank
            .map(|rank| rank.min(u32::MAX as usize) as u32);
        row.status = self.status;
        row.payload_json = json_payload(&self.payload);
        row
    }

    fn to_failure(&self) -> MemoryRegressionCaseFailure {
        MemoryRegressionCaseFailure {
            suite_id: self.suite_id.clone(),
            case_id: self.case_id.clone(),
            agent_id: self.agent_id.clone(),
            goal_id: self.goal_id.clone(),
            retrieval_backend: self.retrieval_backend.clone(),
            status: self.status.clone(),
            expected_count: self.expected_count,
            matched_count: self.matched_count,
            selected_count: self.selected_count,
            best_rank: self.best_rank,
            payload_excerpt: serde_json::to_string(&self.payload)
                .ok()
                .map(|payload| bounded_chars(&payload, 1_200)),
        }
    }
}

async fn write_regression_status(
    memory_root: &Path,
    snapshot: &MemoryRegressionStatusSnapshot,
) -> Result<()> {
    let path = memory_regression_status_path(memory_root);
    let Some(parent) = path.parent() else {
        return Err(anyhow::anyhow!(
            "memory regression status path has no parent: {}",
            path.display()
        ));
    };
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating {}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(snapshot).context("serializing regression status")?;
    // Staged through the shared durable writer rather than a fixed
    // `<file>.json.tmp` sibling: that name is shared by every concurrent
    // writer of this status file, so two overlapping eval runs could rename a
    // half-written snapshot over it. The writer also `sync_all`s the staging
    // file and the parent directory, neither of which happened here.
    write_bytes_durably(&path, &bytes)
        .await
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn emit_regression_status_row(
    storage: &crate::magician_v2::agents::storage::AgentStorage,
    snapshot: &MemoryRegressionStatusSnapshot,
) {
    let mut row =
        MemoryAnalyticsRow::now(MEMORY_REGRESSION_STATUS_EVENT_KIND, "memory_eval_runner");
    row.candidate_count = Some(snapshot.case_count.min(u32::MAX as usize) as u32);
    row.selected_count = Some(snapshot.passed_count.min(u32::MAX as usize) as u32);
    row.dropped_count = Some(snapshot.failed_count.min(u32::MAX as usize) as u32);
    row.status = snapshot.status.clone();
    row.payload_json = json_payload(&json!(snapshot));
    emit_rows_for_storage(storage, vec![row]);
}

async fn load_suites_from_dir(dir: &Path) -> Result<Vec<MemoryEvalSuite>> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };

    let mut suites = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)
            .await
            .with_context(|| format!("reading memory eval suite {}", path.display()))?;
        let mut suite: MemoryEvalSuite = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing memory eval suite {}", path.display()))?;
        if suite.suite_id.trim().is_empty() {
            suite.suite_id = path_stem(&path);
        }
        suite.cases.retain(|case| !case.case_id.trim().is_empty());
        suites.push(suite);
    }
    suites.sort_by(|a, b| a.suite_id.cmp(&b.suite_id));
    Ok(suites)
}

fn builtin_suites_for_scope(principal: &str, workspace: &str) -> Result<Vec<MemoryEvalSuite>> {
    if principal != DEFAULT_SCOPE_PRINCIPAL || workspace != DEFAULT_SCOPE_WORKSPACE {
        return Ok(Vec::new());
    }
    [
        ("core-memory-smoke", BUILTIN_CORE_MEMORY_SMOKE),
        ("personal-assistant-regression", BUILTIN_PERSONAL_ASSISTANT),
        (
            "internal-system-analyst-regression",
            BUILTIN_INTERNAL_SYSTEM_ANALYST,
        ),
        (
            "simple-data-analyst-regression",
            BUILTIN_SIMPLE_DATA_ANALYST,
        ),
        ("web-researcher-regression", BUILTIN_WEB_RESEARCHER),
        ("memory-lifecycle-smoke", BUILTIN_MEMORY_LIFECYCLE),
        ("meetings-memory-regression", BUILTIN_MEETINGS_MEMORY),
        ("weg-ambient-memory-regression", BUILTIN_WEG_AMBIENT_MEMORY),
    ]
    .into_iter()
    .map(|(fallback_id, contents)| parse_builtin_suite(fallback_id, contents))
    .collect()
}

fn parse_builtin_suite(fallback_id: &str, contents: &str) -> Result<MemoryEvalSuite> {
    let mut suite: MemoryEvalSuite = serde_json::from_str(contents)
        .with_context(|| format!("parsing built-in memory eval suite {fallback_id}"))?;
    if suite.suite_id.trim().is_empty() {
        suite.suite_id = fallback_id.to_string();
    }
    Ok(suite)
}

fn scope_labels(scopes: &[MemoryEvalScope]) -> Vec<&'static str> {
    scopes.iter().map(|scope| scope.as_str()).collect()
}

fn count_rendered_entries(rendered_text: &str) -> usize {
    rendered_text
        .lines()
        .filter(|line| line.starts_with('[') && line.contains(" key="))
        .count()
}

fn extract_selected_item_keys(rendered_text: &str) -> Vec<String> {
    rendered_text
        .lines()
        .filter(|line| line.starts_with('[') && line.contains(" key="))
        .filter_map(|line| {
            let after_key = line.split_once(" key=")?.1;
            let key = after_key
                .split(" confidence=")
                .next()
                .unwrap_or(after_key)
                .split(" @ ")
                .next()
                .unwrap_or(after_key)
                .trim();
            (!key.is_empty()).then(|| key.to_string())
        })
        .collect()
}

fn best_rank_for_expected(rendered_text: &str, expected_substrings: &[String]) -> Option<usize> {
    if expected_substrings.is_empty() {
        return None;
    }
    let lowered_expected = expected_substrings
        .iter()
        .map(|value| value.to_lowercase())
        .collect::<Vec<_>>();
    let mut rank = 0usize;
    let mut current = String::new();
    for line in rendered_text.lines() {
        if line.starts_with('[') && line.contains(" key=") {
            if rank > 0 && any_expected_match(&current, &lowered_expected) {
                return Some(rank);
            }
            rank += 1;
            current.clear();
        }
        current.push_str(line);
        current.push('\n');
    }
    if rank > 0 && any_expected_match(&current, &lowered_expected) {
        return Some(rank);
    }
    None
}

fn any_expected_match(rendered_entry: &str, lowered_expected: &[String]) -> bool {
    let haystack = rendered_entry.to_lowercase();
    lowered_expected
        .iter()
        .any(|expected| haystack.contains(expected))
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn bounded_chars(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in value.chars().take(max_chars) {
        out.push(ch);
    }
    if value.chars().count() > max_chars {
        out.push_str("\n...");
    }
    out
}

fn path_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("memory_eval")
        .to_string()
}

fn read_duration_env(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0 || name == "MAGICIAN_MEMORY_EVAL_STARTUP_DELAY_SECS")
        .map(Duration::from_secs)
}

fn memory_eval_runner_enabled() -> bool {
    std::env::var(MEMORY_EVAL_ENABLED_ENV)
        .ok()
        .is_some_and(|value| parse_truthy_env_value(&value))
}

fn parse_truthy_env_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn default_true() -> bool {
    true
}

fn default_max_entries() -> usize {
    DEFAULT_MAX_ENTRIES
}

fn default_max_chars() -> usize {
    DEFAULT_MAX_CHARS
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn suite_for_tests() -> MemoryEvalSuite {
        serde_json::from_value(json!({ "suite_id": "exclusion", "cases": [] }))
            .expect("suite fixture")
    }

    fn case_from(value: serde_json::Value) -> MemoryEvalCase {
        serde_json::from_value(value).expect("case fixture")
    }

    const RENDERED_WITH_STALE_FACT: &str = r#"## USER MEMORY
<user_memory>
[user/preferences key=coffee_current @ 2026-08-21]
likes coke zero
[user/preferences key=coffee_old @ 2026-05-13]
likes iced americano
</user_memory>"#;

    /// Every other assertion in a suite is recall-shaped — "did the anchor
    /// surface?" — so a case that drags in a superseded fact alongside the right
    /// one scores identically to a case that surfaced only the right one. This
    /// is the other half.
    #[test]
    fn a_forbidden_substring_that_appears_fails_the_case() {
        let case = case_from(json!({
            "case_id": "stale-coffee-must-not-surface",
            "agent_id": "personal-assistant",
            "query": "what does the user drink",
            "expected_substrings": ["coke zero"],
            "forbidden_substrings": ["iced americano"],
        }));

        let result = evaluate_rendered_sections(
            &suite_for_tests(),
            &case,
            EvalRetrievalBackend::Direct,
            vec![RENDERED_WITH_STALE_FACT.to_string()],
        );

        assert!(!result.pass, "the excluded fact was rendered");
        assert_eq!(result.status, "failed");
        assert_eq!(
            result.payload["forbidden_present"],
            json!(["iced americano"])
        );
    }

    #[test]
    fn a_case_passes_when_the_forbidden_substring_is_absent() {
        let case = case_from(json!({
            "case_id": "stale-coffee-must-not-surface",
            "agent_id": "personal-assistant",
            "query": "what does the user drink",
            "forbidden_substrings": ["iced americano"],
        }));

        let result = evaluate_rendered_sections(
            &suite_for_tests(),
            &case,
            EvalRetrievalBackend::Direct,
            vec!["<user_memory>\n[user/preferences key=coffee_current @ 2026-08-21]\nlikes coke zero\n</user_memory>".to_string()],
        );

        assert!(result.pass);
        assert_eq!(result.payload["forbidden_present"], json!([]));
    }

    /// A case may assert exclusion and nothing else.
    #[test]
    fn forbidden_substrings_alone_is_a_valid_case() {
        let case = case_from(json!({
            "case_id": "exclusion-only",
            "agent_id": "personal-assistant",
            "query": "q",
            "forbidden": ["iced americano"],
        }));
        assert_eq!(
            case.forbidden_substrings,
            vec!["iced americano".to_string()],
            "the `forbidden` alias must deserialize"
        );

        let result = evaluate_rendered_sections(
            &suite_for_tests(),
            &case,
            EvalRetrievalBackend::Direct,
            vec![RENDERED_WITH_STALE_FACT.to_string()],
        );
        assert_eq!(result.status, "failed", "not invalid_case");
    }

    /// An empty needle matches every haystack. A stray `""` in a suite would
    /// otherwise fail every case it appears in, with a reason that reads like a
    /// real exclusion violation.
    #[test]
    fn a_blank_forbidden_substring_is_ignored_not_asserted_on() {
        let case = case_from(json!({
            "case_id": "blank-needle",
            "agent_id": "personal-assistant",
            "query": "q",
            "expected_substrings": ["coke zero"],
            "forbidden_substrings": ["   ", ""],
        }));

        let result = evaluate_rendered_sections(
            &suite_for_tests(),
            &case,
            EvalRetrievalBackend::Direct,
            vec![RENDERED_WITH_STALE_FACT.to_string()],
        );
        assert!(result.pass, "blanks must not fail the case");
    }

    /// ...but a case made only of blanks asserts nothing at all, which is an
    /// authoring mistake rather than a pass.
    #[test]
    fn a_case_of_only_blank_forbidden_substrings_is_invalid() {
        let case = case_from(json!({
            "case_id": "blank-only",
            "agent_id": "personal-assistant",
            "query": "q",
            "forbidden_substrings": ["   "],
        }));

        let result = evaluate_rendered_sections(
            &suite_for_tests(),
            &case,
            EvalRetrievalBackend::Direct,
            vec![RENDERED_WITH_STALE_FACT.to_string()],
        );
        assert_eq!(result.status, "invalid_case");
    }

    #[test]
    fn best_rank_tracks_first_matching_rendered_item() {
        let rendered = r#"## USER MEMORY
<user_memory>
[user/preferences key=first @ 2026-05-13]
likes terse updates

[user/preferences key=second @ 2026-05-13]
prefers browser headed mode

</user_memory>"#;

        let rank = best_rank_for_expected(rendered, &["browser headed".to_string()]);

        assert_eq!(rank, Some(2));
    }

    #[test]
    fn bounded_chars_truncates_on_char_boundary() {
        assert_eq!(bounded_chars("a👍b", 2), "a👍\n...");
    }

    #[test]
    fn truthy_env_values_enable_memory_evals() {
        for value in ["1", "true", "TRUE", " yes ", "on"] {
            assert!(parse_truthy_env_value(value), "{value} should enable evals");
        }
    }

    #[test]
    fn missing_or_non_truthy_env_values_do_not_enable_memory_evals() {
        for value in ["", "0", "false", "no", "off", "enabled"] {
            assert!(
                !parse_truthy_env_value(value),
                "{value} should leave evals disabled"
            );
        }
    }

    #[test]
    fn builtin_default_scope_suite_parses() {
        let suites = builtin_suites_for_scope(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
            .expect("built-in memory eval suites should parse");
        let suite_ids = suites
            .iter()
            .map(|suite| suite.suite_id.as_str())
            .collect::<Vec<_>>();

        assert!(suite_ids.contains(&"core-memory-smoke"));
        assert!(suite_ids.contains(&"personal-assistant-regression"));
        assert!(suite_ids.contains(&"internal-system-analyst-regression"));
        assert!(suite_ids.contains(&"simple-data-analyst-regression"));
        assert!(suite_ids.contains(&"web-researcher-regression"));
        assert!(suite_ids.contains(&"memory-lifecycle-smoke"));
        assert!(suites.iter().all(|suite| !suite.cases.is_empty()));
    }

    #[test]
    fn weg_ambient_suite_ships_enabled_and_scope_gated() {
        let suite: MemoryEvalSuite = serde_json::from_str(BUILTIN_WEG_AMBIENT_MEMORY)
            .expect("weg-ambient-memory-regression suite parses");
        assert_eq!(suite.suite_id, "weg-ambient-memory-regression");
        assert!(
            suite.enabled,
            "suite ships enabled — it is scope-gated by skip_if_absent, not by being disabled"
        );
        assert!(!suite.cases.is_empty());
        assert!(
            suite.cases.iter().all(|case| case.skip_if_absent),
            "every case must be skip_if_absent so the global flip is safe on scopes without ambient data"
        );
    }

    #[test]
    fn skipped_case_counts_as_skip_never_fail() {
        let suite: MemoryEvalSuite = serde_json::from_str(BUILTIN_WEG_AMBIENT_MEMORY).unwrap();
        let case = suite.cases.first().expect("at least one case");
        let result = MemoryEvalCaseResult::skipped(&suite, case, EvalRetrievalBackend::Direct, 0);
        assert_eq!(result.status, "skipped");
        assert!(!result.pass);

        let mut outcome = MemoryEvalRunOutcome::default();
        outcome.record_result(&result);
        assert_eq!(outcome.skipped_count, 1);
        assert_eq!(outcome.failed_count, 0);
        assert_eq!(outcome.passed_count, 0);
        assert!(
            outcome.failing_cases.is_empty(),
            "a skipped case must never be reported as a failure"
        );
    }

    #[test]
    fn run_stays_healthy_when_all_applicable_cases_skip() {
        // The safety property behind the global flip: a scope that never accrued
        // the asserted data must not turn the regression suite red. case_count is
        // non-zero so the snapshot does not short-circuit to "unknown".
        let outcome = MemoryEvalRunOutcome {
            suite_count: 1,
            case_count: 2,
            skipped_count: 2,
            ..Default::default()
        };
        let snapshot = outcome.regression_snapshot("anonymous", "default");
        assert_eq!(snapshot.status, "healthy");
        assert_eq!(snapshot.failed_count, 0);
        assert!(
            snapshot.status_reason.contains("skipped"),
            "healthy-with-skips reason should disclose the skip count, got: {}",
            snapshot.status_reason
        );
    }

    /// The status file is read by the health surface while eval runs write it.
    /// A reader must see a complete snapshot or the previous one, never a
    /// staging sibling left in `eval_status/`.
    #[tokio::test]
    async fn regression_status_publishes_without_leaving_a_staging_sibling() {
        let temp = tempfile::tempdir().expect("tempdir");
        let memory_root = temp.path().join("memory");
        let path = memory_regression_status_path(&memory_root);

        write_regression_status(
            &memory_root,
            &MemoryRegressionStatusSnapshot::unknown("anonymous", "default", "first"),
        )
        .await
        .expect("first status write");
        write_regression_status(
            &memory_root,
            &MemoryRegressionStatusSnapshot::unknown("anonymous", "default", "second"),
        )
        .await
        .expect("status rewrite");

        let published: MemoryRegressionStatusSnapshot =
            serde_json::from_slice(&std::fs::read(&path).expect("status readable"))
                .expect("published status parses");
        assert_eq!(published.status_reason, "second");

        let staging_left = std::fs::read_dir(path.parent().expect("eval_status dir"))
            .expect("eval_status listing")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(
            !staging_left,
            "status publish must leave no staging sibling in eval_status/"
        );
    }
}
