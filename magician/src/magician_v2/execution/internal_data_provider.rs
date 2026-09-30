//! InternalDataProvider — read-only access to Magician operational data.
//!
//! This provider is intentionally scoped to `ArtifactV2Workspace`. It gives
//! internal agents a stable way to inspect analytics views, LLM-call Parquet
//! partitions, task state, execution event JSONL, and prompt/runtime files
//! without teaching every agent the on-disk layout.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use duckdb::types::Value as DuckValue;
use duckdb::{params, Connection};
use magicllm::LlmScope;
use serde_json::{json, Map, Value};
use tokio::time::timeout;

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::analytics::duckdb_safety::{
    configure_analytics_connection_checked, duckdb_value_ref_output_bytes,
    json_string_encoded_bytes, run_analytics_query_with_interrupt_timeout,
    try_analytics_duckdb_guard_for, AnalyticsDuckDbQueryError, ANALYTICS_DUCKDB_MAX_RESULT_BYTES,
    ANALYTICS_DUCKDB_MAX_RESULT_ROWS,
};
use crate::magician_v2::analytics::{
    legacy_llm_compat::{install_legacy_llm_views, validate_legacy_llm_query},
    llm_analytics_read_service::{LlmAnalyticsReadService, LlmFactFilter, LlmFactQuery},
    llm_fact_registry::LlmFactRelation,
    llm_scoped_path::ensure_real_scoped_directory_chain,
    llm_sql_guard::is_one_read_only_select_statement,
};
use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use crate::magician_v2::artifact_v2::ArtifactV2Error;
use crate::magician_v2::learning::{
    build_learning_gap_audit, LearningCandidate, LearningCandidateFilters,
    LearningCapabilityEvolutionApplicationFilters, LearningCapabilityEvolutionBacklogFilters,
    LearningCapabilityEvolutionImplementationFilters,
    LearningCapabilityEvolutionPostPromotionMonitorFilters,
    LearningCapabilityEvolutionPromotionFilters, LearningCapabilityEvolutionProposalFilters,
    LearningCapabilityEvolutionRollbackRecommendationFilters,
    LearningCapabilityEvolutionValidationFilters, LearningEvaluationBacklogFilters,
    LearningEvaluationBacklogItem, LearningEvaluationRunFilters, LearningEvaluationRunReport,
    LearningEvent, LearningGrowthEvaluationRunFilters, LearningGrowthEvaluationRunReport,
    LearningProcedureFilters, LearningScope, LearningStore,
};
use crate::magician_v2::notes::AudioNoteIndexEntry;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::strategy::plan::PlanStep;

pub const INTERNAL_DATA_TOOL_NAME: &str = "internal_data";

const DEFAULT_ROW_LIMIT: usize = 200;
const MAX_ROW_LIMIT: usize = 10_000;
const DEFAULT_LINE_LIMIT: usize = 200;
const MAX_LINE_LIMIT: usize = 5_000;
const DEFAULT_FILE_MAX_CHARS: usize = 120_000;
const MAX_FILE_MAX_CHARS: usize = 500_000;
const DEFAULT_LIST_LIMIT: usize = 50;
const MAX_LIST_LIMIT: usize = 1_000;
const MAX_AUDIO_NOTE_LIST_TRANSCRIPT_CHARS: usize = 1_000;

#[derive(Clone)]
pub struct InternalDataProvider {
    workspace_layout: ArtifactV2Workspace,
    llm_analytics: Arc<LlmAnalyticsReadService>,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for InternalDataProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalDataProvider")
            .field("workspace_layout", &self.workspace_layout)
            .field("llm_analytics", &"shared")
            .field("pack_def", &self.pack_def.as_ref().map(|pack| &pack.name))
            .finish()
    }
}

impl InternalDataProvider {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            llm_analytics: Arc::new(LlmAnalyticsReadService::new(workspace_layout.clone())),
            workspace_layout,
            pack_def: None,
        }
    }

    pub fn with_llm_analytics_read_service(
        mut self,
        llm_analytics: Arc<LlmAnalyticsReadService>,
    ) -> Self {
        self.llm_analytics = llm_analytics;
        self
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for InternalDataProvider {
    fn tool_name(&self) -> &str {
        INTERNAL_DATA_TOOL_NAME
    }

    /// App-path argument proof (plan 2.5). The app bind kernel admits exactly
    /// the two learning review reads; this closes their parameter surface:
    /// one action selector, an optional state filter from the closed candidate
    /// state set, an optional bounded page limit, or one exact candidate id.
    /// Anything else — other actions, unexpected keys, wrong types, oversized
    /// values — fails closed. The runtime scope never passes through here:
    /// `__principal`/`__workspace` are executor-owned and re-verified by
    /// `authorize_runtime_scope` before any store read.
    fn prove_app_tool_args(&self, parameters: &HashMap<String, Value>) -> bool {
        prove_app_learning_read_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: INTERNAL_DATA_TOOL_NAME.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: INTERNAL_DATA_TOOL_NAME.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let params = match action {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params.clone(),
            _ => {
                return Err(ExecutionError::Step(
                    "internal_data: unexpected action type".to_string(),
                ))
            },
        };

        let action_name = string_param(&params, "__action_name")
            .or_else(|| string_param(&params, "action"))
            .unwrap_or_else(|| "catalog".to_string());
        let params = authorize_runtime_scope(params)?;
        let workspace_layout = self.workspace_layout.clone();
        let llm_analytics = Arc::clone(&self.llm_analytics);
        let effective_timeout = timeout_secs.max(1);
        let action_name_for_task = action_name.clone();

        if internal_data_action_uses_duckdb(&action_name) {
            let value = tokio::task::spawn_blocking(move || {
                let budget = InternalQueryBudget::new(Duration::from_secs(effective_timeout));
                let remaining = budget.remaining(&action_name_for_task)?;
                let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(remaining) else {
                    return Err(budget.timeout_error(
                        &action_name_for_task,
                        Some("waiting for analytics capacity".to_string()),
                    ));
                };
                execute_internal_data_action_with_budget(
                    &workspace_layout,
                    &llm_analytics,
                    &action_name_for_task,
                    &params,
                    Some(&budget),
                )
            })
            .await
            .map_err(|join_err| {
                ExecutionError::Step(format!("internal_data task panicked: {join_err}"))
            })??;

            return Ok(ActionResult::text(bounded_duckdb_result_json(
                &value,
                &action_name,
            )?));
        }

        let result = timeout(Duration::from_secs(effective_timeout), async move {
            tokio::task::spawn_blocking(move || {
                execute_internal_data_action_with_service(
                    &workspace_layout,
                    &llm_analytics,
                    &action_name_for_task,
                    &params,
                )
            })
            .await
        })
        .await;

        let value = match result {
            Ok(Ok(inner)) => inner?,
            Ok(Err(join_err)) => {
                return Err(ExecutionError::Step(format!(
                    "internal_data task panicked: {join_err}"
                )))
            },
            Err(_) => {
                return Err(ExecutionError::Step(format!(
                    "internal_data action `{action_name}` timed out after {effective_timeout}s"
                )))
            },
        };

        Ok(ActionResult::text(pretty_json(&value)))
    }

    fn default_timeout_secs(&self) -> u64 {
        self.pack_def
            .as_ref()
            .and_then(|pack| pack.execution.as_ref())
            .and_then(|execution| execution.default_timeout_secs)
            .unwrap_or(30)
    }
}

/// The closed candidate-state vocabulary the app-bound list filter accepts.
/// It mirrors `LearningCandidateState::as_str`; an unknown state fails the
/// proof rather than being passed to the store as a filter that silently
/// matches nothing.
const APP_LEARNING_READ_STATES: &[&str] = &[
    "observed",
    "proposed",
    "triaged",
    "approved",
    "implemented",
    "evaluated",
    "promoted",
    "rejected",
    "superseded",
    "archived",
];

const APP_LEARNING_READ_MAX_LIMIT: u64 = 25;
const APP_LEARNING_READ_MAX_CANDIDATE_ID_BYTES: usize = 128;

/// Argument proof for the app-bound learning review reads (plan 2.5). Exactly
/// one action, a closed parameter surface, bounded values; every other key,
/// type or action is refused. Runtime-owned `__*` correlation fields pass
/// through uninterpreted — model-origin hidden keys were already stripped
/// before this proof runs, and `authorize_runtime_scope` re-derives the store
/// scope from the executor-owned values alone.
fn prove_app_learning_read_args(parameters: &HashMap<String, Value>) -> bool {
    let Some(operation) = parameters.get("__action_name").and_then(Value::as_str) else {
        return false;
    };
    let listed = operation == "list_learning_candidates";
    if !listed && operation != "read_learning_candidate" {
        return false;
    }
    for (key, value) in parameters {
        match key.as_str() {
            "__action_name" => {},
            "operation" | "action" | "method" => {
                // An alias must agree with the routing key, tool-qualified
                // spellings included, or the call names two operations.
                let agrees = value.as_str().is_some_and(|alias| {
                    crate::magician_v2::apps::app_tool_bind::normalize_app_action_selector(
                        "internal_data",
                        alias,
                    )
                    .as_deref()
                        == Some(operation)
                });
                if !agrees {
                    return false;
                }
            },
            "state" if listed => {
                if value
                    .as_str()
                    .is_none_or(|state| !APP_LEARNING_READ_STATES.contains(&state))
                {
                    return false;
                }
            },
            "limit" if listed => {
                if value
                    .as_u64()
                    .is_none_or(|limit| !(1..=APP_LEARNING_READ_MAX_LIMIT).contains(&limit))
                {
                    return false;
                }
            },
            "candidate_id" if !listed => {
                if value.as_str().is_none_or(|id| {
                    id.is_empty() || id.len() > APP_LEARNING_READ_MAX_CANDIDATE_ID_BYTES
                }) {
                    return false;
                }
            },
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }
    if !listed && parameters.get("candidate_id").is_none() {
        return false;
    }
    true
}

#[cfg(any(test, feature = "test-fixtures"))]
fn execute_internal_data_action(
    workspace_layout: &ArtifactV2Workspace,
    action_name: &str,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let llm_analytics = LlmAnalyticsReadService::new(workspace_layout.clone());
    execute_internal_data_action_with_budget(
        workspace_layout,
        &llm_analytics,
        action_name,
        params,
        None,
    )
}

fn execute_internal_data_action_with_service(
    workspace_layout: &ArtifactV2Workspace,
    llm_analytics: &LlmAnalyticsReadService,
    action_name: &str,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    execute_internal_data_action_with_budget(
        workspace_layout,
        llm_analytics,
        action_name,
        params,
        None,
    )
}

fn execute_internal_data_action_with_budget(
    workspace_layout: &ArtifactV2Workspace,
    llm_analytics: &LlmAnalyticsReadService,
    action_name: &str,
    params: &HashMap<String, Value>,
    budget: Option<&InternalQueryBudget>,
) -> Result<Value, ExecutionError> {
    match action_name {
        "catalog" => catalog(workspace_layout, llm_analytics, params),
        "schema" => Ok(schema(workspace_layout, llm_analytics, params)),
        "llm_observability_overview" => llm_observability_overview(llm_analytics, params),
        "list_llm_traces" => list_llm_traces(llm_analytics, params),
        "read_llm_trace" => read_llm_trace(llm_analytics, params),
        "list_llm_calls" => list_llm_facts(llm_analytics, params, LlmFactRelation::Calls),
        "read_llm_call" => read_llm_call(llm_analytics, params),
        "list_llm_provider_attempts" => {
            list_llm_facts(llm_analytics, params, LlmFactRelation::ProviderAttempts)
        },
        "read_llm_provider_attempt" => read_llm_provider_attempt(llm_analytics, params),
        "query_llm_facts" => query_llm_facts(llm_analytics, params),
        "query_events" => query_events(workspace_layout, params, budget),
        "query_llm_calls" => query_llm_calls(workspace_layout, params, budget),
        "query_memory_events" => query_memory_events(workspace_layout, params, budget),
        "memory_observability_summary" => {
            memory_observability_summary(workspace_layout, params, budget)
        },
        "memory_index_snapshot" => Ok(memory_index_snapshot(workspace_layout, params)),
        "tail_logs" => tail_logs(workspace_layout, params, budget),
        "find_errors" => find_errors(workspace_layout, params, budget),
        "list_tasks" => list_tasks(workspace_layout, params),
        "latest_task" => latest_task(workspace_layout, params),
        "read_task_state" => read_task_state(workspace_layout, params),
        "list_task_outputs" => list_task_outputs(workspace_layout, params),
        "read_task_output" => read_task_output(workspace_layout, params),
        "list_audio_notes" => list_audio_notes(workspace_layout, params),
        "read_audio_note" => read_audio_note(workspace_layout, params),
        "list_executions" => list_executions(workspace_layout, params),
        "read_execution_events" => read_execution_events(workspace_layout, params),
        "list_workspace_events" => list_workspace_events(workspace_layout, params),
        "read_workspace_events" => read_workspace_events(workspace_layout, params),
        "timeline" => timeline(workspace_layout, params, budget),
        "list_execution_files" => list_execution_files(workspace_layout, params),
        "read_execution_file" => read_execution_file(workspace_layout, params),
        "memory_regression_status" => memory_regression_status(workspace_layout, params),
        "review_memory_effects" => review_memory_effects(workspace_layout, params),
        "learning_audit" => learning_audit(workspace_layout, params),
        "list_learning_candidates" => list_learning_candidates(workspace_layout, params),
        "read_learning_candidate" => read_learning_candidate(workspace_layout, params),
        "list_learning_evaluations" => list_learning_evaluations(workspace_layout, params),
        "read_learning_evaluation" => read_learning_evaluation(workspace_layout, params),
        "list_learning_evaluation_runs" => list_learning_evaluation_runs(workspace_layout, params),
        "read_learning_evaluation_run" => read_learning_evaluation_run(workspace_layout, params),
        "list_learning_growth_evaluations" => {
            list_learning_growth_evaluations(workspace_layout, params)
        },
        "read_learning_growth_evaluation" => {
            read_learning_growth_evaluation(workspace_layout, params)
        },
        "list_learning_procedures" => list_learning_procedures(workspace_layout, params),
        "read_learning_procedure" => read_learning_procedure(workspace_layout, params),
        "list_learning_capability_evolution" => {
            list_learning_capability_evolution(workspace_layout, params)
        },
        "read_learning_capability_evolution" => {
            read_learning_capability_evolution(workspace_layout, params)
        },
        "list_learning_capability_proposals" => {
            list_learning_capability_proposals(workspace_layout, params)
        },
        "read_learning_capability_proposal" => {
            read_learning_capability_proposal(workspace_layout, params)
        },
        "list_learning_capability_validations" => {
            list_learning_capability_validations(workspace_layout, params)
        },
        "read_learning_capability_validation" => {
            read_learning_capability_validation(workspace_layout, params)
        },
        "list_learning_capability_implementations" => {
            list_learning_capability_implementations(workspace_layout, params)
        },
        "read_learning_capability_implementation" => {
            read_learning_capability_implementation(workspace_layout, params)
        },
        "list_learning_capability_applications" => {
            list_learning_capability_applications(workspace_layout, params)
        },
        "read_learning_capability_application" => {
            read_learning_capability_application(workspace_layout, params)
        },
        "list_learning_capability_rollback_recommendations" => {
            list_learning_capability_rollback_recommendations(workspace_layout, params)
        },
        "read_learning_capability_rollback_recommendation" => {
            read_learning_capability_rollback_recommendation(workspace_layout, params)
        },
        "list_learning_capability_post_promotion_monitors" => {
            list_learning_capability_post_promotion_monitors(workspace_layout, params)
        },
        "read_learning_capability_post_promotion_monitor" => {
            read_learning_capability_post_promotion_monitor(workspace_layout, params)
        },
        "list_learning_capability_promotions" => {
            list_learning_capability_promotions(workspace_layout, params)
        },
        "read_learning_capability_promotion" => {
            read_learning_capability_promotion(workspace_layout, params)
        },
        "list_learning_events" => list_learning_events(workspace_layout, params),
        "list_learning_feed_insights" => {
            list_learning_feed_insights(workspace_layout, params, budget)
        },
        other => Err(ExecutionError::Step(format!(
            "internal_data: unknown action `{other}`"
        ))),
    }
}

fn internal_data_action_uses_duckdb(action_name: &str) -> bool {
    matches!(
        action_name,
        "query_events"
            | "query_llm_calls"
            | "query_memory_events"
            | "memory_observability_summary"
            | "tail_logs"
            | "find_errors"
            | "timeline"
            | "list_learning_feed_insights"
    )
}

#[derive(Clone, Copy)]
struct InternalQueryBudget {
    timeout: Duration,
    started_at: Instant,
}

impl InternalQueryBudget {
    fn new(timeout: Duration) -> Self {
        let started_at = Instant::now();
        Self {
            timeout,
            started_at,
        }
    }

    fn remaining(&self, action: &str) -> Result<Duration, ExecutionError> {
        self.timeout
            .checked_sub(self.started_at.elapsed())
            .ok_or_else(|| self.timeout_error(action, None))
    }

    fn timeout_error(&self, action: &str, source: Option<String>) -> ExecutionError {
        let detail = source
            .map(|source| format!(": {source}"))
            .unwrap_or_default();
        ExecutionError::Step(format!(
            "internal_data action `{action}` timed out after {}s{detail}",
            self.timeout.as_secs()
        ))
    }
}

fn configure_internal_analytics_connection(
    conn: &Connection,
    action: &str,
) -> Result<(), ExecutionError> {
    configure_analytics_connection_checked(conn, action).map_err(|error| {
        ExecutionError::Step(format!(
            "internal_data {action}: failed to apply DuckDB safety settings: {error}"
        ))
    })
}

fn disable_internal_external_access(conn: &Connection, action: &str) -> Result<(), ExecutionError> {
    conn.execute_batch(
        "SET enable_external_access = false; \
         SET autoinstall_known_extensions = false; \
         SET autoload_known_extensions = false;",
    )
    .map_err(|error| {
        ExecutionError::Step(format!(
            "internal_data {action}: failed to disable DuckDB external access: {error}"
        ))
    })
}

fn catalog(
    workspace_layout: &ArtifactV2Workspace,
    llm_analytics: &LlmAnalyticsReadService,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let scope_root = workspace_layout.scope_root(&scope.principal, &scope.workspace);
    let analytics_db = workspace_layout.analytics_db_path(&scope.principal, &scope.workspace);
    let llm_calls_root =
        workspace_layout.analytics_llm_calls_root(&scope.principal, &scope.workspace);
    let memory_events_root =
        workspace_layout.analytics_memory_events_root(&scope.principal, &scope.workspace);
    let tasks_root = workspace_layout.tasks_root(&scope.principal, &scope.workspace);
    let scoped_executions_root =
        workspace_layout.scoped_executions_root(&scope.principal, &scope.workspace);
    let workspace_events = scope_root.join("events.jsonl");
    let learning_root = workspace_layout.learning_root(&scope.principal, &scope.workspace);
    let learning_store = LearningStore::new(workspace_layout.clone());
    let capability_evolution_rollback_recommendation_count = learning_store
        .list_capability_evolution_rollback_recommendation_records(
            &learning_scope,
            LearningCapabilityEvolutionRollbackRecommendationFilters::default(),
        )
        .map(|records| records.len())
        .unwrap_or(0);
    let capability_evolution_post_promotion_monitor_count = learning_store
        .list_capability_evolution_post_promotion_monitor_records(
            &learning_scope,
            LearningCapabilityEvolutionPostPromotionMonitorFilters::default(),
        )
        .map(|records| records.len())
        .unwrap_or(0);
    let memory_regression_status =
        crate::magician_v2::analytics::memory_eval_runner::memory_regression_status_path(
            &workspace_layout.memory_root(&scope.principal, &scope.workspace),
        );

    let mut response = json!({
        "scope": scope.as_json(),
        "sources": {
            "scope_root": path_status(scope_root),
            "analytics_db": path_status(analytics_db),
            "llm_calls_root": {
                "path": llm_calls_root.display().to_string(),
                "exists": llm_calls_root.exists(),
                "parquet_files": count_files_with_extension(&llm_calls_root, "parquet")
            },
            "memory_events_root": {
                "path": memory_events_root.display().to_string(),
                "exists": memory_events_root.exists(),
                "parquet_files": count_files_with_extension(&memory_events_root, "parquet")
            },
            "tasks_root": {
                "path": tasks_root.display().to_string(),
                "exists": tasks_root.exists(),
                "task_count": count_child_dirs(&tasks_root)
            },
            "scoped_executions_root": {
                "path": scoped_executions_root.display().to_string(),
                "exists": scoped_executions_root.exists(),
                "execution_count": count_child_dirs(&scoped_executions_root)
            },
            "workspace_events": path_status(workspace_events),
            "memory_regression_status": path_status(memory_regression_status),
            "learning_root": {
                "path": learning_root.display().to_string(),
                "exists": learning_root.exists(),
                "candidate_count": count_files_with_extension(
                    &workspace_layout.learning_candidates_dir(&scope.principal, &scope.workspace),
                    "json"
                ),
                "evaluation_backlog_count": count_files_with_extension(
                    &workspace_layout.learning_evaluation_backlog_dir(&scope.principal, &scope.workspace),
                    "json"
                ),
                "evaluation_run_count": count_files_recursive(
                    &workspace_layout.learning_evaluation_runs_dir(&scope.principal, &scope.workspace),
                    2
                ),
                "growth_evaluation_run_count": count_files_with_extension(
                    &workspace_layout.learning_growth_evaluation_runs_dir(&scope.principal, &scope.workspace),
                    "json"
                ),
                "procedure_count": count_learning_procedure_records(workspace_layout, &scope.principal, &scope.workspace),
                "capability_evolution_backlog_count": learning_store.count_capability_evolution_backlog_items(&learning_scope),
                "capability_evolution_proposal_count": learning_store.count_capability_evolution_proposals(&learning_scope),
                "capability_evolution_validation_count": learning_store.count_capability_evolution_validation_reports(&learning_scope),
                "capability_evolution_implementation_count": learning_store.count_capability_evolution_implementation_records(&learning_scope),
                "capability_evolution_application_count": learning_store.count_capability_evolution_application_records(&learning_scope),
                "capability_evolution_rollback_recommendation_count": capability_evolution_rollback_recommendation_count,
                "capability_evolution_post_promotion_monitor_count": capability_evolution_post_promotion_monitor_count,
                "capability_evolution_promotion_count": learning_store.count_capability_evolution_promotion_records(&learning_scope),
                "event_file_count": count_files_with_extension(
                    &workspace_layout.learning_events_dir(&scope.principal, &scope.workspace),
                    "jsonl"
                )
            }
        },
        "recommended_start": [
            "Use schema to inspect available analytics/LLM-call columns.",
            "Use tail_logs or find_errors for recent runtime issues.",
            "Use list_tasks/latest_task, then read_execution_events/read_execution_file for task-specific debugging.",
            "Use memory_regression_status to inspect the latest internal memory-evaluation health gate.",
            "Use review_memory_effects to see whether shadow memory-effect records justify Canary or Enforced — Accept is HITL or POST /memory/effect-review.",
            "Use memory_index_snapshot to inspect the current derived memory index manifest and local footprint.",
            "Use memory_observability_summary for recent memory retrieval/index health, then query_memory_events for deeper slices.",
            "Use learning_audit/list_learning_candidates for learning and agent-growth debugging.",
            "Use list_learning_evaluations/read_learning_evaluation for meta-harness eval backlog review.",
            "Use list_learning_evaluation_runs/read_learning_evaluation_run for durable eval-worker reports.",
            "Use list_learning_growth_evaluations/read_learning_growth_evaluation for Phase 9 growth rollups across memory, skill, capability, program, teaching, and guardrail evidence.",
            "Use list_learning_procedures/read_learning_procedure for reusable ways of working stored separately from semantic memory and executable skills.",
            "Use list_learning_capability_evolution/read_learning_capability_evolution for skill/tool-pack evolution backlog review, including skill_update and workflow_template candidates.",
            "Use list_learning_capability_proposals/read_learning_capability_proposal for skill/tool-pack fix/eval plans and their review decision status.",
            "Use list_learning_capability_validations/read_learning_capability_validation for validation evidence attached to approved skill/tool-pack proposals.",
            "Use list_learning_capability_implementations/read_learning_capability_implementation for reviewed implementation bundles before promotion.",
            "Use list_learning_capability_applications/read_learning_capability_application for dry-run or applied scoped file changes from implementation bundles.",
            "Use list_learning_capability_rollback_recommendations/read_learning_capability_rollback_recommendation for rollback work produced by failed apply, validation, or post-promotion monitoring.",
            "Use list_learning_capability_post_promotion_monitors/read_learning_capability_post_promotion_monitor for Phase 7 regression monitors and their remediation links.",
            "Use list_learning_capability_promotions/read_learning_capability_promotion for manual implementation/promotion audit records.",
            "Use list_learning_feed_insights for the user-facing learning insight and memory-review cards projected into /feed and Today Activity, including projection summary counts.",
            "Use list_audio_notes for scoped Audio Notes metadata and bounded transcript previews, then read_audio_note for one full transcript. Recording bytes are intentionally not exposed through this read-only model tool.",
            "Use query_events/query_llm_calls for aggregate analysis."
        ],
        "actions": [
            "catalog",
            "schema",
            "llm_observability_overview",
            "list_llm_traces",
            "read_llm_trace",
            "list_llm_calls",
            "read_llm_call",
            "list_llm_provider_attempts",
            "read_llm_provider_attempt",
            "query_llm_facts",
            "query_events",
            "query_llm_calls",
            "query_memory_events",
            "memory_observability_summary",
            "memory_index_snapshot",
            "tail_logs",
            "find_errors",
            "list_tasks",
            "latest_task",
            "read_task_state",
            "list_task_outputs",
            "read_task_output",
            "list_audio_notes",
            "read_audio_note",
            "list_executions",
            "read_execution_events",
            "list_workspace_events",
            "read_workspace_events",
            "timeline",
            "list_execution_files",
            "read_execution_file",
            "memory_regression_status",
            "review_memory_effects",
            "learning_audit",
            "list_learning_candidates",
            "read_learning_candidate",
            "list_learning_evaluations",
            "read_learning_evaluation",
            "list_learning_evaluation_runs",
            "read_learning_evaluation_run",
            "list_learning_growth_evaluations",
            "read_learning_growth_evaluation",
            "list_learning_procedures",
            "read_learning_procedure",
            "list_learning_capability_evolution",
            "read_learning_capability_evolution",
            "list_learning_capability_proposals",
            "read_learning_capability_proposal",
            "list_learning_capability_validations",
            "read_learning_capability_validation",
            "list_learning_capability_implementations",
            "read_learning_capability_implementation",
            "list_learning_capability_applications",
            "read_learning_capability_application",
            "list_learning_capability_rollback_recommendations",
            "read_learning_capability_rollback_recommendation",
            "list_learning_capability_post_promotion_monitors",
            "read_learning_capability_post_promotion_monitor",
            "list_learning_capability_promotions",
            "read_learning_capability_promotion",
            "list_learning_events",
            "list_learning_feed_insights"
        ]
    });
    let llm_scope = LlmScope::new(scope.principal, scope.workspace);
    let fact_catalog = llm_analytics
        .refresh_catalog(&llm_scope)
        .map_err(|error| ExecutionError::Step(format!("internal_data catalog: {error}")))?;
    response["llm_fact_catalog"] = serde_json::to_value(fact_catalog).map_err(|error| {
        ExecutionError::Step(format!("internal_data catalog serialization: {error}"))
    })?;
    Ok(response)
}

fn schema(
    _workspace_layout: &ArtifactV2Workspace,
    llm_analytics: &LlmAnalyticsReadService,
    params: &HashMap<String, Value>,
) -> Value {
    let scope = scope_from_params(params);
    let mut response = json!({
        "scope": scope.as_json(),
        "analytics": {
            "table": {
                "events": [
                    "id BIGINT",
                    "timestamp TIMESTAMPTZ",
                    "event_type VARCHAR",
                    "source VARCHAR",
                    "payload JSON"
                ]
            },
            "views": {
                "logs": ["timestamp", "level", "message", "target", "source"],
                "bot_logs": ["timestamp", "bot_name", "stream", "line", "source"],
                "chat_messages": ["timestamp", "session_id", "direction", "content_text", "source"],
                "chat_sessions": ["timestamp", "session_id", "principal", "agent_id", "status", "source"],
                "task_executions": ["timestamp", "task_id", "execution_id", "status", "duration_ms", "source"],
                "task_steps": ["timestamp", "execution_id", "step_number", "step_name", "status", "source"],
                "artifacts": ["timestamp", "artifact_uid", "namespace", "name", "event_type", "source"],
                "episodes": ["memory-agent episode JSON columns; inspect with query_events"],
                "memory_tiers": ["memory tier JSON columns; inspect with query_events"]
            }
        },
        "llm_calls": [
            "timestamp_ms BIGINT",
            "started_at_ms BIGINT",
            "latency_ms BIGINT",
            "principal VARCHAR",
            "workspace VARCHAR",
            "execution_id VARCHAR",
            "task_id VARCHAR",
            "plan_id VARCHAR",
            "step_id VARCHAR",
            "step_index BIGINT",
            "agent_id VARCHAR",
            "delegated_agent_id VARCHAR",
            "chat_session_id VARCHAR",
            "operation VARCHAR",
            "profile VARCHAR",
            "provider VARCHAR",
            "model VARCHAR",
            "capability VARCHAR",
            "response_kind VARCHAR",
            "attempt INTEGER",
            "success BOOLEAN",
            "error VARCHAR",
            "input_tokens INTEGER",
            "output_tokens INTEGER",
            "reasoning_tokens INTEGER",
            "cache_read_tokens INTEGER",
            "cache_creation_tokens INTEGER",
            "ttft_ms BIGINT",
            "cost_usd DOUBLE"
        ],
        "chat_session_cache_summary": [
            "chat_session_id VARCHAR",
            "turns BIGINT",
            "total_input BIGINT",
            "total_cached BIGINT",
            "total_cache_writes BIGINT",
            "cache_hit_pct DOUBLE",
            "total_output BIGINT",
            "total_reasoning BIGINT",
            "avg_ttft_ms DOUBLE",
            "avg_latency_ms DOUBLE",
            "total_cost_usd DOUBLE",
            "first_call_ms BIGINT",
            "last_call_ms BIGINT"
        ],
        "raw_files": {
            "task_execution_events": "tasks/<task_id>/executions/<execution_id>/events.jsonl",
            "task_execution_index": "tasks/<task_id>/indexes/executions.jsonl",
            "workspace_events": "events.jsonl",
            "task_state": "tasks/<task_id>/state/task_state.json",
            "task_outputs": "tasks/<task_id>/outputs/",
            "audio_notes": "notes/audio-index/<note_id>.json (metadata/transcript index; provider-owned audio and Markdown stay outside runtime storage)",
            "execution_files": "tasks/<task_id>/executions/<execution_id>/** or executions/<execution_id>/**",
            "memory_index": "memory/index/manifest.json, memory/index/documents.jsonl, memory/index/lancedb/",
            "learning": "learning/events/YYYY-MM-DD.jsonl, learning/candidates/<candidate_id>.json, learning/decisions/<candidate_id>.jsonl, learning/procedures/{draft,active,deprecated,archived}/<procedure_id>.yaml, learning/evaluations/backlog/<candidate_id>.json, learning/evaluations/runs/<candidate_id>/<run_id>.json",
            "skill_evolution_backlog": "skill_evolution/backlog/<candidate_id>.json",
            "skill_evolution_proposals": "skill_evolution/proposals/<candidate_id>.json",
            "skill_evolution_validations": "skill_evolution/validations/<candidate_id>/<validation_id>.json",
            "skill_evolution_implementations": "skill_evolution/implementations/<candidate_id>/<implementation_id>.json",
            "skill_evolution_applications": "skill_evolution/applications/<candidate_id>/<application_id>.json",
            "skill_evolution_rollback_recommendations": "skill_evolution/rollback_recommendations/<candidate_id>/<recommendation_id>.json",
            "skill_evolution_post_promotion_monitors": "skill_evolution/post_promotion_monitors/<promotion_id>.json",
            "skill_evolution_promotion_audit": "skill_evolution/promotion_audit.jsonl",
            "legacy_capability_evolution": "capability_evolution/* is read-only compatibility input for older Skill Evolution records"
        },
        "memory_events": [
            "timestamp_ms BIGINT",
            "principal VARCHAR",
            "workspace VARCHAR",
            "event_kind VARCHAR",
            "source VARCHAR",
            "agent_id VARCHAR",
            "goal_id VARCHAR",
            "scope VARCHAR",
            "tier_name VARCHAR",
            "item_key VARCHAR",
            "selected BOOLEAN",
            "score INTEGER",
            "confidence DOUBLE",
            "query_excerpt VARCHAR",
            "candidate_count INTEGER",
            "selected_count INTEGER",
            "dropped_count INTEGER",
            "retrieval_backend VARCHAR",
            "status VARCHAR",
            "payload_json VARCHAR",
            "dt VARCHAR"
        ]
    });
    response["llm_fact_registry"] = serde_json::to_value(llm_analytics.registry())
        .unwrap_or_else(|_| json!({"schema_version": 1, "facts": {}}));
    response
}

fn llm_observability_overview(
    llm_analytics: &LlmAnalyticsReadService,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = llm_scope_from_params(params);
    let from_ms = optional_i64_param(params, "from_ms")?;
    let to_ms = optional_i64_param(params, "to_ms")?;
    let envelope = llm_analytics
        .overview_envelope(&scope, from_ms, to_ms)
        .map_err(|error| {
            ExecutionError::Step(format!("internal_data llm_observability_overview: {error}"))
        })?;
    serialize_llm_read("llm_observability_overview", envelope)
}

fn list_llm_traces(
    llm_analytics: &LlmAnalyticsReadService,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = llm_scope_from_params(params);
    let from_ms = optional_i64_param(params, "from_ms")?;
    let to_ms = optional_i64_param(params, "to_ms")?;
    let limit = params.get("limit").map(|_| {
        bounded_usize(
            params,
            "limit",
            DEFAULT_ROW_LIMIT,
            ANALYTICS_DUCKDB_MAX_RESULT_ROWS,
        )
    });
    let envelope = llm_analytics
        .list_traces_envelope(&scope, from_ms, to_ms, limit)
        .map_err(|error| ExecutionError::Step(format!("internal_data list_llm_traces: {error}")))?;
    serialize_llm_read("list_llm_traces", envelope)
}

fn read_llm_trace(
    llm_analytics: &LlmAnalyticsReadService,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = llm_scope_from_params(params);
    let trace_id = required_string(params, "trace_id")?;
    let from_ms = optional_i64_param(params, "from_ms")?;
    let to_ms = optional_i64_param(params, "to_ms")?;
    let envelope = llm_analytics
        .read_trace_envelope(&scope, &trace_id, from_ms, to_ms)
        .map_err(|error| ExecutionError::Step(format!("internal_data read_llm_trace: {error}")))?;
    serialize_llm_read("read_llm_trace", envelope)
}

fn list_llm_facts(
    llm_analytics: &LlmAnalyticsReadService,
    params: &HashMap<String, Value>,
    relation: LlmFactRelation,
) -> Result<Value, ExecutionError> {
    let scope = llm_scope_from_params(params);
    let mut query = LlmFactQuery::for_relation(relation);
    query.from_ms = optional_i64_param(params, "from_ms")?;
    query.to_ms = optional_i64_param(params, "to_ms")?;
    query.limit = params.get("limit").map(|_| {
        bounded_usize(
            params,
            "limit",
            DEFAULT_ROW_LIMIT,
            ANALYTICS_DUCKDB_MAX_RESULT_ROWS,
        )
    });
    query.offset = optional_usize_param(params, "offset")?.unwrap_or_default();
    if let Some(order_by) = string_param(params, "order_by") {
        query.order_by = Some(order_by);
    }
    if let Some(descending) = optional_bool_param(params, "descending")? {
        query.descending = descending;
    }
    for (column, parameter) in [
        ("operation", "operation"),
        ("provider", "provider"),
        ("model", "model"),
        ("effective_profile", "profile"),
    ] {
        if let Some(value) = string_param(params, parameter) {
            query.filters.push(LlmFactFilter::TextEquals {
                column: column.to_string(),
                value,
            });
        }
    }
    if let Some(value) = optional_bool_param(params, "success")? {
        query.filters.push(LlmFactFilter::BooleanEquals {
            column: "transport_success".to_string(),
            value,
        });
    }
    let envelope = llm_analytics
        .query_facts_envelope(&scope, query)
        .map_err(|error| {
            ExecutionError::Step(format!(
                "internal_data {}: {error}",
                match relation {
                    LlmFactRelation::Calls => "list_llm_calls",
                    LlmFactRelation::ProviderAttempts => "list_llm_provider_attempts",
                    _ => "list_llm_facts",
                }
            ))
        })?;
    serialize_llm_read("list_llm_facts", envelope)
}

fn read_llm_call(
    llm_analytics: &LlmAnalyticsReadService,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = llm_scope_from_params(params);
    let llm_call_id = required_string(params, "llm_call_id")?;
    let envelope = llm_analytics
        .read_call_detail_envelope(&scope, &llm_call_id)
        .map_err(|error| ExecutionError::Step(format!("internal_data read_llm_call: {error}")))?;
    serialize_llm_read("read_llm_call", envelope)
}

fn read_llm_provider_attempt(
    llm_analytics: &LlmAnalyticsReadService,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = llm_scope_from_params(params);
    let provider_attempt_id = required_string(params, "provider_attempt_id")?;
    let envelope = llm_analytics
        .read_provider_attempt_envelope(&scope, &provider_attempt_id)
        .map_err(|error| {
            ExecutionError::Step(format!("internal_data read_llm_provider_attempt: {error}"))
        })?;
    serialize_llm_read("read_llm_provider_attempt", envelope)
}

fn query_llm_facts(
    llm_analytics: &LlmAnalyticsReadService,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = llm_scope_from_params(params);
    let sql = required_string(params, "sql")?;
    let from_ms = optional_i64_param(params, "from_ms")?;
    let to_ms = optional_i64_param(params, "to_ms")?;
    let limit = params.get("limit").map(|_| {
        bounded_usize(
            params,
            "limit",
            DEFAULT_ROW_LIMIT,
            ANALYTICS_DUCKDB_MAX_RESULT_ROWS,
        )
    });
    let envelope = llm_analytics
        .query_fact_sql(&scope, &sql, from_ms, to_ms, limit)
        .map_err(|error| ExecutionError::Step(format!("internal_data query_llm_facts: {error}")))?;
    serialize_llm_read("query_llm_facts", envelope)
}

fn serialize_llm_read<T: serde::Serialize>(
    action: &str,
    value: T,
) -> Result<Value, ExecutionError> {
    serde_json::to_value(value).map_err(|error| {
        ExecutionError::Step(format!("internal_data {action} serialization: {error}"))
    })
}

fn llm_scope_from_params(params: &HashMap<String, Value>) -> LlmScope {
    let scope = scope_from_params(params);
    LlmScope::new(scope.principal, scope.workspace)
}

fn query_events(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    budget: Option<&InternalQueryBudget>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let sql = required_string(params, "sql")?;
    ensure_safe_select(&sql, "query_events")?;
    let limit = bounded_usize(params, "limit", DEFAULT_ROW_LIMIT, MAX_ROW_LIMIT);
    let pool = crate::magician_v2::analytics::scoped_pool(
        workspace_layout,
        &scope.principal,
        &scope.workspace,
    )
    .map_err(|e| ExecutionError::Step(format!("internal_data query_events: {e}")))?;
    let conn = pool
        .read_connection()
        .map_err(|e| ExecutionError::Step(format!("internal_data query_events: {e}")))?;
    configure_internal_analytics_connection(&conn, "query_events")?;
    disable_internal_external_access(&conn, "query_events")?;
    let rows = run_duckdb_query(&conn, &sql, limit, budget, "query_events")?;
    Ok(json!({
        "scope": scope.as_json(),
        "source": "analytics.duckdb",
        "sql": sql,
        "limit": limit,
        "columns": rows.columns,
        "rows": rows.rows,
        "row_count": rows.row_count,
        "truncated": rows.truncated
    }))
}

fn query_llm_calls(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    budget: Option<&InternalQueryBudget>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let sql = required_string(params, "sql")?;
    validate_legacy_llm_query(&sql).map_err(|error| {
        ExecutionError::Step(format!(
            "internal_data query_llm_calls rejected query: {error}"
        ))
    })?;
    let limit = bounded_usize(params, "limit", DEFAULT_ROW_LIMIT, MAX_ROW_LIMIT);
    let llm_calls_root =
        workspace_layout.analytics_llm_calls_root(&scope.principal, &scope.workspace);
    ensure_real_scoped_directory_chain(workspace_layout.base_root(), &llm_calls_root).map_err(
        |error| {
            ExecutionError::Step(format!(
                "internal_data query_llm_calls rejected unsafe scoped storage: {error}"
            ))
        },
    )?;
    let llm_scope = LlmScope::new(scope.principal.clone(), scope.workspace.clone());
    let parquet_files =
        regular_llm_call_partition_files(workspace_layout, &llm_scope, &llm_calls_root)?;
    if parquet_files.is_empty() {
        return Ok(json!({
            "scope": scope.as_json(),
            "source": "llm_calls",
            "sql": sql,
            "limit": limit,
            "columns": [],
            "rows": [],
            "row_count": 0,
            "truncated": false,
            "note": "No llm_calls Parquet partitions exist for this scope yet."
        }));
    }

    let conn = Connection::open_in_memory()
        .map_err(|e| ExecutionError::Step(format!("internal_data query_llm_calls: {e}")))?;
    configure_internal_analytics_connection(&conn, "query_llm_calls")?;
    let parquet_source = parquet_files
        .iter()
        .map(|path| format!("'{}'", escape_sql_string(&path.display().to_string())))
        .collect::<Vec<_>>();
    let parquet_source = match parquet_source.as_slice() {
        [one] => one.clone(),
        _ => format!("[{}]", parquet_source.join(", ")),
    };
    run_internal_duckdb_operation(&conn, budget, "query_llm_calls", || {
        install_legacy_llm_views(
            &conn,
            &parquet_source,
            None,
            &scope.principal,
            &scope.workspace,
            None,
        )
        .map_err(|error| {
            ExecutionError::Step(format!(
                "internal_data query_llm_calls could not install compatibility views: {error}"
            ))
        })
    })?;
    disable_internal_external_access(&conn, "query_llm_calls")?;
    let rows = run_duckdb_query(&conn, &sql, limit, budget, "query_llm_calls")?;
    Ok(json!({
        "scope": scope.as_json(),
        "source": "llm_calls",
        "path": llm_calls_root.display().to_string(),
        "parquet_files": parquet_files.len(),
        "sql": sql,
        "limit": limit,
        "columns": rows.columns,
        "rows": rows.rows,
        "row_count": rows.row_count,
        "truncated": rows.truncated
    }))
}

fn query_memory_events(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    budget: Option<&InternalQueryBudget>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let sql = required_string(params, "sql")?;
    ensure_safe_select(&sql, "query_memory_events")?;
    let limit = bounded_usize(params, "limit", DEFAULT_ROW_LIMIT, MAX_ROW_LIMIT);
    let root = workspace_layout.analytics_memory_events_root(&scope.principal, &scope.workspace);
    let parquet_count = count_files_with_extension(&root, "parquet");
    if parquet_count == 0 {
        return Ok(json!({
            "scope": scope.as_json(),
            "source": "memory_events",
            "path": root.display().to_string(),
            "sql": sql,
            "limit": limit,
            "columns": [],
            "rows": [],
            "row_count": 0,
            "truncated": false,
            "note": "No memory_events Parquet partitions exist for this scope yet."
        }));
    }

    let conn = memory_events_connection(&root, "query_memory_events", budget)?;
    let rows = run_duckdb_query(&conn, &sql, limit, budget, "query_memory_events")?;
    Ok(json!({
        "scope": scope.as_json(),
        "source": "memory_events",
        "path": root.display().to_string(),
        "parquet_files": parquet_count,
        "sql": sql,
        "limit": limit,
        "columns": rows.columns,
        "rows": rows.rows,
        "row_count": rows.row_count,
        "truncated": rows.truncated
    }))
}

fn memory_observability_summary(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    budget: Option<&InternalQueryBudget>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let hours = bounded_usize(params, "hours", 24, 24 * 30);
    let limit = bounded_usize(params, "limit", 50, MAX_LIST_LIMIT);
    let root = workspace_layout.analytics_memory_events_root(&scope.principal, &scope.workspace);
    let parquet_count = count_files_with_extension(&root, "parquet");
    if parquet_count == 0 {
        return Ok(json!({
            "scope": scope.as_json(),
            "source": "memory_events",
            "path": root.display().to_string(),
            "parquet_files": 0,
            "hours": hours,
            "event_counts": [],
            "backend_counts": [],
            "relevance_summary": [],
            "relevance_by_backend": [],
            "index_write_summary": [],
            "low_score_selected": [],
            "recent_problem_events": [],
            "note": "No memory_events Parquet partitions exist for this scope yet."
        }));
    }

    let conn = memory_events_connection(&root, "memory_observability_summary", budget)?;
    let cutoff_ms = Utc::now()
        .timestamp_millis()
        .saturating_sub((hours as i64).saturating_mul(60 * 60 * 1000));
    let event_counts_sql = format!(
        "SELECT event_kind, status, COUNT(*) AS count
         FROM memory_events
         WHERE timestamp_ms >= {cutoff_ms}
         GROUP BY event_kind, status
         ORDER BY count DESC, event_kind ASC"
    );
    let backend_counts_sql = format!(
        "SELECT retrieval_backend, status, COUNT(*) AS count
         FROM memory_events
         WHERE timestamp_ms >= {cutoff_ms}
           AND retrieval_backend IS NOT NULL
         GROUP BY retrieval_backend, status
         ORDER BY count DESC, retrieval_backend ASC"
    );
    let problem_sql = format!(
        "SELECT timestamp_ms, event_kind, source, agent_id, scope, retrieval_backend, status,
                query_excerpt, payload_json
         FROM memory_events
         WHERE timestamp_ms >= {cutoff_ms}
           AND (
                event_kind IN (
                    'memory_index_rebuild_failed',
                    'memory_index_reconcile_failed',
                    'memory_retrieval_fallback'
                )
                OR status IN ('failed', 'fallback', 'direct_fallback')
           )
         ORDER BY timestamp_ms DESC
         LIMIT {limit}"
    );
    let relevance_summary_sql = format!(
        "SELECT COUNT(*) AS candidates,
                SUM(CASE WHEN selected THEN 1 ELSE 0 END) AS selected_candidates,
                SUM(CASE WHEN selected THEN 0 ELSE 1 END) AS dropped_candidates,
                AVG(score) AS avg_score,
                AVG(CASE WHEN selected THEN score ELSE NULL END) AS avg_selected_score,
                AVG(CASE WHEN selected = false THEN score ELSE NULL END) AS avg_dropped_score,
                AVG(CASE WHEN selected THEN score ELSE NULL END)
                    - AVG(CASE WHEN selected = false THEN score ELSE NULL END) AS selection_lift
         FROM memory_events
         WHERE timestamp_ms >= {cutoff_ms}
           AND event_kind = 'retrieval'
           AND score IS NOT NULL"
    );
    let relevance_by_backend_sql = format!(
        "SELECT COALESCE(retrieval_backend, 'unknown') AS retrieval_backend,
                CASE WHEN selected THEN 'selected' ELSE 'dropped' END AS selection,
                COUNT(*) AS candidates,
                AVG(score) AS avg_score,
                MIN(score) AS min_score,
                MAX(score) AS max_score
         FROM memory_events
         WHERE timestamp_ms >= {cutoff_ms}
           AND event_kind = 'retrieval'
           AND score IS NOT NULL
         GROUP BY retrieval_backend, selection
         ORDER BY retrieval_backend, selection DESC"
    );
    let low_score_selected_sql = format!(
        "SELECT timestamp_ms, agent_id, scope, tier_name, item_key, score,
                confidence, retrieval_backend, query_excerpt
         FROM memory_events
         WHERE timestamp_ms >= {cutoff_ms}
           AND event_kind = 'retrieval'
           AND selected = true
           AND score IS NOT NULL
         ORDER BY score ASC, timestamp_ms DESC
         LIMIT {limit}"
    );
    let index_write_summary_sql = format!(
        "SELECT event_kind, status, COALESCE(source_kind, 'unknown') AS write_mode,
                COUNT(*) AS events,
                SUM(input_count) AS input_rows,
                SUM(output_count) AS output_rows
         FROM memory_events
         WHERE timestamp_ms >= {cutoff_ms}
           AND event_kind LIKE 'memory_index_lancedb_%'
         GROUP BY event_kind, status, write_mode
         ORDER BY events DESC, event_kind ASC
         LIMIT {limit}"
    );
    let event_counts = run_duckdb_query(
        &conn,
        &event_counts_sql,
        limit,
        budget,
        "memory_observability_summary",
    )?;
    let backend_counts = run_duckdb_query(
        &conn,
        &backend_counts_sql,
        limit,
        budget,
        "memory_observability_summary",
    )?;
    let relevance_summary = run_duckdb_query(
        &conn,
        &relevance_summary_sql,
        1,
        budget,
        "memory_observability_summary",
    )?;
    let relevance_by_backend = run_duckdb_query(
        &conn,
        &relevance_by_backend_sql,
        limit,
        budget,
        "memory_observability_summary",
    )?;
    let low_score_selected = run_duckdb_query(
        &conn,
        &low_score_selected_sql,
        limit,
        budget,
        "memory_observability_summary",
    )?;
    let index_write_summary = run_duckdb_query(
        &conn,
        &index_write_summary_sql,
        limit,
        budget,
        "memory_observability_summary",
    )?;
    let recent_problem_events = run_duckdb_query(
        &conn,
        &problem_sql,
        limit,
        budget,
        "memory_observability_summary",
    )?;
    Ok(json!({
        "scope": scope.as_json(),
        "source": "memory_events",
        "path": root.display().to_string(),
        "parquet_files": parquet_count,
        "hours": hours,
        "cutoff_ms": cutoff_ms,
        "event_counts": event_counts.rows,
        "backend_counts": backend_counts.rows,
        "relevance_summary": relevance_summary.rows,
        "relevance_by_backend": relevance_by_backend.rows,
        "index_write_summary": index_write_summary.rows,
        "low_score_selected": low_score_selected.rows,
        "recent_problem_events": recent_problem_events.rows,
    }))
}

fn memory_events_connection(
    root: &Path,
    action: &str,
    budget: Option<&InternalQueryBudget>,
) -> Result<Connection, ExecutionError> {
    let conn = Connection::open_in_memory()
        .map_err(|e| ExecutionError::Step(format!("internal_data {action}: {e}")))?;
    configure_internal_analytics_connection(&conn, action)?;
    let glob_path = crate::magician_v2::dataset_owners::family_parquet_glob(
        root,
        crate::magician_v2::dataset_owners::DatasetFamily::MemoryEvents,
    );
    let glob_sql = escape_sql_string(&glob_path.display().to_string());
    let view_sql = format!(
        "CREATE OR REPLACE TEMP TABLE memory_events AS \
         SELECT * FROM read_parquet('{glob_sql}', hive_partitioning = true, union_by_name = true);"
    );
    run_internal_duckdb_operation(&conn, budget, action, || {
        conn.execute_batch(&view_sql)
            .map_err(|e| ExecutionError::Step(format!("internal_data {action}: {e}")))
    })?;
    disable_internal_external_access(&conn, action)?;
    Ok(conn)
}

fn memory_index_snapshot(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Value {
    let scope = scope_from_params(params);
    let memory_root = workspace_layout.memory_root(&scope.principal, &scope.workspace);
    let index_root = memory_root.join("index");
    let manifest_path = index_root.join("manifest.json");
    let documents_path = index_root.join("documents.jsonl");
    let lancedb_root = index_root.join("lancedb");
    let manifest = read_json_file_optional(&manifest_path).unwrap_or_else(|error| {
        json!({
            "read_error": error.to_string()
        })
    });
    json!({
        "scope": scope.as_json(),
        "memory_root": path_status(memory_root),
        "index_root": path_status(index_root),
        "manifest_path": path_status(manifest_path),
        "documents_path": path_status(documents_path.clone()),
        "lancedb_root": path_status(lancedb_root.clone()),
        "documents_line_count": count_jsonl_records(&documents_path),
        "lancedb_file_count": count_files_recursive(&lancedb_root, 4),
        "manifest": manifest
    })
}

fn tail_logs(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    budget: Option<&InternalQueryBudget>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let limit = bounded_usize(params, "limit", DEFAULT_LINE_LIMIT, MAX_LINE_LIMIT);
    let mut filters = Vec::new();
    if let Some(level) = string_param(params, "level") {
        filters.push(format!(
            "lower(level) = lower('{}')",
            escape_sql_string(&level)
        ));
    }
    if let Some(target_contains) = string_param(params, "target_contains") {
        filters.push(format!(
            "target ILIKE '%{}%' ESCAPE '\\'",
            escape_like_string(&target_contains)
        ));
    }
    if let Some(contains) = string_param(params, "contains") {
        filters.push(format!(
            "message ILIKE '%{}%' ESCAPE '\\'",
            escape_like_string(&contains)
        ));
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", filters.join(" AND "))
    };
    let sql = format!(
        "SELECT timestamp, level, target, message, source FROM logs {where_clause} ORDER BY timestamp DESC LIMIT {limit}"
    );
    query_generated_events_sql(workspace_layout, &scope, &sql, limit, "tail_logs", budget)
}

fn find_errors(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    budget: Option<&InternalQueryBudget>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let limit = bounded_usize(params, "limit", DEFAULT_LINE_LIMIT, MAX_LINE_LIMIT);

    if string_param(params, "execution_id").is_some() {
        let mut filtered_params = params.clone();
        filtered_params
            .entry("max_lines".to_string())
            .or_insert_with(|| json!(limit));
        filtered_params
            .entry("contains".to_string())
            .or_insert_with(|| Value::String("error".to_string()));
        let events = read_execution_events(workspace_layout, &filtered_params)?;
        return Ok(json!({
            "scope": scope.as_json(),
            "source": "execution_events",
            "result": events
        }));
    }

    let sql = format!(
        "SELECT timestamp, level, target, message, source FROM logs \
         WHERE lower(level) IN ('warn', 'error') \
            OR message ILIKE '%error%' \
            OR message ILIKE '%failed%' \
            OR message ILIKE '%panic%' \
         ORDER BY timestamp DESC LIMIT {limit}"
    );
    query_generated_events_sql(workspace_layout, &scope, &sql, limit, "find_errors", budget)
}

fn query_generated_events_sql(
    workspace_layout: &ArtifactV2Workspace,
    scope: &Scope,
    sql: &str,
    limit: usize,
    source: &str,
    budget: Option<&InternalQueryBudget>,
) -> Result<Value, ExecutionError> {
    let pool = crate::magician_v2::analytics::scoped_pool(
        workspace_layout,
        &scope.principal,
        &scope.workspace,
    )
    .map_err(|e| ExecutionError::Step(format!("internal_data {source}: {e}")))?;
    let conn = pool
        .read_connection()
        .map_err(|e| ExecutionError::Step(format!("internal_data {source}: {e}")))?;
    configure_internal_analytics_connection(&conn, source)?;
    let rows = run_duckdb_query(&conn, sql, limit, budget, source)?;
    Ok(json!({
        "scope": scope.as_json(),
        "source": source,
        "sql": sql,
        "limit": limit,
        "columns": rows.columns,
        "rows": rows.rows,
        "row_count": rows.row_count,
        "truncated": rows.truncated
    }))
}

fn list_tasks(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let tasks_root = workspace_layout.tasks_root(&scope.principal, &scope.workspace);
    let mut entries = list_child_dirs(&tasks_root)?;
    entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    let total = entries.len();
    let tasks: Vec<Value> = entries
        .into_iter()
        .take(limit)
        .map(|entry| {
            let task_id = entry.name;
            let manifest_path =
                workspace_layout.task_manifest_path(&scope.principal, &scope.workspace, &task_id);
            let state_path =
                workspace_layout.task_state_path(&scope.principal, &scope.workspace, &task_id);
            let executions_root =
                workspace_layout.executions_root(&scope.principal, &scope.workspace, &task_id);
            json!({
                "task_id": task_id,
                "path": entry.path.display().to_string(),
                "modified": system_time_json(entry.modified),
                "manifest_exists": manifest_path.exists(),
                "state_exists": state_path.exists(),
                "execution_count": count_child_dirs(&executions_root),
                "manifest": read_json_file_optional(&manifest_path).unwrap_or(Value::Null)
            })
        })
        .collect();

    Ok(json!({
        "scope": scope.as_json(),
        "path": tasks_root.display().to_string(),
        "total": total,
        "limit": limit,
        "tasks": tasks
    }))
}

fn latest_task(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let mut params = params.clone();
    params.insert("limit".to_string(), json!(1));
    let listed = list_tasks(workspace_layout, &params)?;
    let latest = listed
        .get("tasks")
        .and_then(Value::as_array)
        .and_then(|tasks| tasks.first())
        .cloned()
        .unwrap_or(Value::Null);
    Ok(json!({
        "scope": listed.get("scope").cloned().unwrap_or(Value::Null),
        "latest_task": latest
    }))
}

fn read_task_state(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let task_id = required_string(params, "task_id")?;
    let task_dir = workspace_layout.task_dir(&scope.principal, &scope.workspace, &task_id);
    let manifest_path =
        workspace_layout.task_manifest_path(&scope.principal, &scope.workspace, &task_id);
    let state_path = workspace_layout.task_state_path(&scope.principal, &scope.workspace, &task_id);
    let index_path =
        workspace_layout.task_executions_index_path(&scope.principal, &scope.workspace, &task_id);
    let max_lines = bounded_usize(params, "max_lines", DEFAULT_LINE_LIMIT, MAX_LINE_LIMIT);

    Ok(json!({
        "scope": scope.as_json(),
        "task_id": task_id,
        "task_dir": path_status(task_dir),
        "manifest": read_json_file_optional(&manifest_path).unwrap_or(Value::Null),
        "task_state": read_json_file_optional(&state_path).unwrap_or(Value::Null),
        "execution_index": tail_jsonl_file(&index_path, None, max_lines, None, None, false)?
    }))
}

fn list_task_outputs(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let task_id = required_string(params, "task_id")?;
    let max_depth = bounded_usize(params, "max_depth", 4, 8);
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let root = workspace_layout.task_outputs_dir(&scope.principal, &scope.workspace, &task_id);
    let mut files = Vec::new();
    collect_files(&root, &root, 0, max_depth, limit, &mut files)?;
    Ok(json!({
        "scope": scope.as_json(),
        "task_id": task_id,
        "root": root.display().to_string(),
        "limit": limit,
        "files": files
    }))
}

fn read_task_output(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let task_id = required_string(params, "task_id")?;
    let relative_path = required_string(params, "path")?;
    let max_chars = bounded_usize(
        params,
        "max_chars",
        DEFAULT_FILE_MAX_CHARS,
        MAX_FILE_MAX_CHARS,
    );
    let root = workspace_layout.task_outputs_dir(&scope.principal, &scope.workspace, &task_id);
    let file_path = resolve_existing_child_file(&root, &relative_path)?;
    let content = read_text_file_limited(&file_path, max_chars)?;
    Ok(json!({
        "scope": scope.as_json(),
        "task_id": task_id,
        "path": file_path.display().to_string(),
        "relative_path": relative_path,
        "max_chars": max_chars,
        "truncated": content.truncated,
        "content": content.content
    }))
}

fn list_audio_notes(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, 100);
    let offset = optional_usize_param(params, "offset")?.unwrap_or_default();
    let query = string_param(params, "contains").map(|value| value.to_lowercase());
    let root = workspace_layout
        .scope_root(&scope.principal, &scope.workspace)
        .join("notes")
        .join("audio-index");
    let mut notes = Vec::new();
    for entry in workspace_layout
        .read_dir_path_sync(&root)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.is_file && entry.file_name.ends_with(".json"))
    {
        let path = root.join(&entry.file_name);
        let Ok(bytes) = workspace_layout.read_path_sync(&path) else {
            continue;
        };
        let Ok(note) = serde_json::from_slice::<AudioNoteIndexEntry>(&bytes) else {
            continue;
        };
        if let Some(query) = query.as_deref() {
            let haystack = format!(
                "{} {} {} {}",
                note.captured_at,
                note.source_surface,
                note.provider,
                note.transcript.as_deref().unwrap_or_default()
            )
            .to_lowercase();
            if !haystack.contains(query) {
                continue;
            }
        }
        notes.push(note);
    }
    notes.sort_by(|left, right| {
        match (
            DateTime::parse_from_rfc3339(&left.captured_at),
            DateTime::parse_from_rfc3339(&right.captured_at),
        ) {
            (Ok(left_at), Ok(right_at)) => right_at
                .timestamp_micros()
                .cmp(&left_at.timestamp_micros())
                .then_with(|| right.note_id.cmp(&left.note_id)),
            _ => right
                .captured_at
                .cmp(&left.captured_at)
                .then_with(|| right.note_id.cmp(&left.note_id)),
        }
    });
    let total = notes.len();
    let items = notes
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(audio_note_list_projection)
        .collect::<Vec<_>>();
    Ok(json!({
        "scope": scope.as_json(),
        "items": items,
        "offset": offset,
        "limit": limit,
        "total": total,
        "has_more": offset.saturating_add(items.len()) < total,
    }))
}

fn audio_note_list_projection(note: AudioNoteIndexEntry) -> Value {
    let mut value = serde_json::to_value(note).unwrap_or(Value::Null);
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    let transcript = object
        .get("transcript")
        .and_then(Value::as_str)
        .map(str::to_string);
    let (preview, truncated, transcript_chars) = match transcript {
        Some(transcript) => {
            let mut chars = transcript.chars();
            let preview = chars
                .by_ref()
                .take(MAX_AUDIO_NOTE_LIST_TRANSCRIPT_CHARS)
                .collect::<String>();
            let truncated = chars.next().is_some();
            let count = if truncated {
                transcript.chars().count()
            } else {
                preview.chars().count()
            };
            (Some(preview), truncated, count)
        },
        None => (None, false, 0),
    };
    if let Some(preview) = preview {
        object.insert("transcript".to_string(), Value::String(preview));
    }
    object.insert("transcript_truncated".to_string(), Value::Bool(truncated));
    object.insert(
        "transcript_chars".to_string(),
        Value::from(transcript_chars as u64),
    );
    value
}

fn read_audio_note(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let requested = required_string(params, "note_id")?;
    let note_id = uuid::Uuid::parse_str(requested.trim()).map_err(|_| {
        ExecutionError::Step("internal_data read_audio_note: note_id must be a UUID".to_string())
    })?;
    let path = workspace_layout
        .scope_root(&scope.principal, &scope.workspace)
        .join("notes")
        .join("audio-index")
        .join(format!("{}.json", note_id.hyphenated()));
    let note = match workspace_layout.read_path_sync(&path) {
        Ok(bytes) => serde_json::from_slice::<AudioNoteIndexEntry>(&bytes).map_err(|error| {
            ExecutionError::Step(format!(
                "internal_data read_audio_note: invalid index record: {error}"
            ))
        })?,
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"scope": scope.as_json(), "note": null}));
        },
        Err(error) => {
            return Err(ExecutionError::Step(format!(
                "internal_data read_audio_note: {error}"
            )))
        },
    };
    Ok(json!({"scope": scope.as_json(), "note": note}))
}

fn list_executions(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let task_id = string_param(params, "task_id");
    let root = match task_id.as_deref() {
        Some(task_id) => {
            workspace_layout.executions_root(&scope.principal, &scope.workspace, task_id)
        },
        None => workspace_layout.scoped_executions_root(&scope.principal, &scope.workspace),
    };
    let mut entries = list_child_dirs(&root)?;
    entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    let total = entries.len();
    let executions: Vec<Value> = entries
        .into_iter()
        .take(limit)
        .map(|entry| {
            let execution_id = entry.name;
            let events_path = entry.path.join("events.jsonl");
            json!({
                "execution_id": execution_id,
                "path": entry.path.display().to_string(),
                "modified": system_time_json(entry.modified),
                "events_exists": events_path.exists(),
                "events_path": events_path.display().to_string(),
                "file_count": count_files_recursive(&entry.path, 3)
            })
        })
        .collect();
    Ok(json!({
        "scope": scope.as_json(),
        "task_id": task_id,
        "path": root.display().to_string(),
        "total": total,
        "limit": limit,
        "executions": executions
    }))
}

fn read_execution_events(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let execution_id = required_string(params, "execution_id")?;
    let task_id = string_param(params, "task_id");
    let max_lines = bounded_usize(params, "max_lines", DEFAULT_LINE_LIMIT, MAX_LINE_LIMIT);
    let event_type = string_param(params, "event_type");
    let contains = string_param(params, "contains");
    let events_path =
        execution_events_path(workspace_layout, &scope, task_id.as_deref(), &execution_id);
    let commit_path = events_path.with_file_name("events.jsonl.commit");
    let committed_len = committed_jsonl_len_sync(&events_path, &commit_path)?;
    Ok(json!({
        "scope": scope.as_json(),
        "task_id": task_id,
        "execution_id": execution_id,
        "path": events_path.display().to_string(),
        "events": tail_jsonl_file(
            &events_path,
            Some(committed_len),
            max_lines,
            event_type.as_deref(),
            contains.as_deref(),
            true,
        )?
    }))
}

fn list_workspace_events(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let events_path = workspace_layout
        .scope_root(&scope.principal, &scope.workspace)
        .join("events.jsonl");
    let max_lines = bounded_usize(params, "max_lines", DEFAULT_LINE_LIMIT, MAX_LINE_LIMIT);
    Ok(json!({
        "scope": scope.as_json(),
        "path": events_path.display().to_string(),
        "events": tail_jsonl_file(&events_path, None, max_lines, None, None, true)?
    }))
}

fn read_workspace_events(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let events_path = workspace_layout
        .scope_root(&scope.principal, &scope.workspace)
        .join("events.jsonl");
    let max_lines = bounded_usize(params, "max_lines", DEFAULT_LINE_LIMIT, MAX_LINE_LIMIT);
    let event_type = string_param(params, "event_type");
    let contains = string_param(params, "contains");
    Ok(json!({
        "scope": scope.as_json(),
        "path": events_path.display().to_string(),
        "events": tail_jsonl_file(
            &events_path,
            None,
            max_lines,
            event_type.as_deref(),
            contains.as_deref(),
            true,
        )?
    }))
}

/// Cross-source merged timeline for one execution. Composes
/// `read_execution_events`, `read_workspace_events` filtered by
/// execution_id, `query_llm_calls` for that execution, and `query_events`
/// (logs view) bounded to the resulting time window. Returns a single
/// ascending-time array of `{ ts_ms, source, entry }` rows so the agent
/// doesn't burn context assembling cross-source views by hand.
///
/// Soft-fails per source: if one of the four sources is unavailable,
/// the error is recorded under `errors.<source>` and the other sources
/// still contribute. `source_counts` documents what made it in.
fn timeline(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    budget: Option<&InternalQueryBudget>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let execution_id = required_string(params, "execution_id")?;
    let task_id = string_param(params, "task_id");
    let max_per_source = bounded_usize(params, "max_lines_per_source", 100, 500);

    let mut merged: Vec<Value> = Vec::new();
    let mut source_counts: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut errors: serde_json::Map<String, Value> = serde_json::Map::new();

    // Extract a millisecond-epoch timestamp from any of the common
    // shapes we see across sources.
    fn extract_ts_ms(v: &Value) -> Option<i64> {
        if let Some(ms) = v.get("ts").and_then(Value::as_i64) {
            return Some(ms);
        }
        if let Some(ms) = v.get("ts_ms").and_then(Value::as_i64) {
            return Some(ms);
        }
        if let Some(ms) = v.get("timestamp_ms").and_then(Value::as_i64) {
            return Some(ms);
        }
        if let Some(s) = v.get("timestamp").and_then(Value::as_str) {
            return chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.timestamp_millis());
        }
        if let Some(ms) = v.get("timestamp").and_then(Value::as_i64) {
            return Some(ms);
        }
        None
    }

    // 1. Execution events (tasks/<task_id>/executions/<exec_id>/events.jsonl or scoped chat exec)
    // Build a clean param set so unrelated timeline filters (contains,
    // event_type, status) don't leak into one source and silently narrow it
    // while the other sources see the unfiltered shape.
    let mut exec_params: HashMap<String, Value> = HashMap::new();
    exec_params.insert("principal".into(), json!(scope.principal.clone()));
    exec_params.insert("workspace".into(), json!(scope.workspace.clone()));
    exec_params.insert("execution_id".into(), json!(execution_id.clone()));
    if let Some(task) = task_id.as_ref() {
        exec_params.insert("task_id".into(), json!(task.clone()));
    }
    exec_params.insert("max_lines".into(), json!(max_per_source));
    match read_execution_events(workspace_layout, &exec_params) {
        Ok(v) => {
            if let Some(events) = v
                .get("events")
                .and_then(|e| e.get("lines"))
                .and_then(Value::as_array)
            {
                source_counts.insert("exec_events".into(), json!(events.len()));
                for e in events {
                    if let Some(ts_ms) = extract_ts_ms(e) {
                        merged.push(json!({
                            "ts_ms": ts_ms,
                            "source": "exec_events",
                            "entry": e
                        }));
                    }
                }
            }
        },
        Err(e) => {
            errors.insert("exec_events".into(), json!(e.to_string()));
        },
    }

    // 2. Workspace events filtered to lines mentioning this execution_id
    let mut ws_params: HashMap<String, Value> = HashMap::new();
    ws_params.insert("principal".into(), json!(scope.principal.clone()));
    ws_params.insert("workspace".into(), json!(scope.workspace.clone()));
    ws_params.insert("contains".into(), json!(execution_id.clone()));
    ws_params.insert("max_lines".into(), json!(max_per_source));
    match read_workspace_events(workspace_layout, &ws_params) {
        Ok(v) => {
            if let Some(events) = v
                .get("events")
                .and_then(|e| e.get("lines"))
                .and_then(Value::as_array)
            {
                source_counts.insert("workspace_events".into(), json!(events.len()));
                for e in events {
                    if let Some(ts_ms) = extract_ts_ms(e) {
                        merged.push(json!({
                            "ts_ms": ts_ms,
                            "source": "workspace_events",
                            "entry": e
                        }));
                    }
                }
            }
        },
        Err(e) => {
            errors.insert("workspace_events".into(), json!(e.to_string()));
        },
    }

    // 3. LLM calls for this execution from Parquet
    let mut llm_params: HashMap<String, Value> = HashMap::new();
    llm_params.insert("principal".into(), json!(scope.principal.clone()));
    llm_params.insert("workspace".into(), json!(scope.workspace.clone()));
    llm_params.insert("limit".into(), json!(max_per_source));
    llm_params.insert(
        "sql".into(),
        json!(format!(
            "SELECT timestamp_ms, operation, model, agent_id, latency_ms, success, error \
             FROM llm_calls WHERE execution_id = '{}' \
             ORDER BY timestamp_ms ASC LIMIT {}",
            escape_sql_string(&execution_id),
            max_per_source
        )),
    );
    match query_llm_calls(workspace_layout, &llm_params, budget) {
        Ok(v) => {
            if let Some(rows) = v.get("rows").and_then(Value::as_array) {
                source_counts.insert("llm_calls".into(), json!(rows.len()));
                for r in rows {
                    if let Some(ts_ms) = extract_ts_ms(r) {
                        merged.push(json!({
                            "ts_ms": ts_ms,
                            "source": "llm_calls",
                            "entry": r
                        }));
                    }
                }
            }
        },
        Err(e) => {
            errors.insert("llm_calls".into(), json!(e.to_string()));
        },
    }

    // 4. Analytics service logs bounded to the time window established
    // above. We only pull logs if we have at least one anchor from the
    // earlier sources — otherwise we'd have nothing to bound by.
    let ts_min = merged
        .iter()
        .filter_map(|m| m.get("ts_ms").and_then(Value::as_i64))
        .min();
    let ts_max = merged
        .iter()
        .filter_map(|m| m.get("ts_ms").and_then(Value::as_i64))
        .max();
    if let (Some(lo), Some(hi)) = (ts_min, ts_max) {
        let mut log_params: HashMap<String, Value> = HashMap::new();
        log_params.insert("principal".into(), json!(scope.principal.clone()));
        log_params.insert("workspace".into(), json!(scope.workspace.clone()));
        log_params.insert("limit".into(), json!(max_per_source));
        log_params.insert(
            "sql".into(),
            json!(format!(
                "SELECT epoch_ms(timestamp) AS ts_ms, level, target, message \
                 FROM logs WHERE epoch_ms(timestamp) BETWEEN {} AND {} \
                 ORDER BY ts_ms ASC LIMIT {}",
                lo, hi, max_per_source
            )),
        );
        match query_events(workspace_layout, &log_params, budget) {
            Ok(v) => {
                if let Some(rows) = v.get("rows").and_then(Value::as_array) {
                    source_counts.insert("logs".into(), json!(rows.len()));
                    for r in rows {
                        if let Some(ts_ms) = extract_ts_ms(r) {
                            merged.push(json!({
                                "ts_ms": ts_ms,
                                "source": "logs",
                                "entry": r
                            }));
                        }
                    }
                }
            },
            Err(e) => {
                errors.insert("logs".into(), json!(e.to_string()));
            },
        }
    }

    // Sort the merged set chronologically. Stable enough — entries with
    // identical ts_ms keep their source-order insertion.
    merged.sort_by_key(|e| e.get("ts_ms").and_then(Value::as_i64).unwrap_or(0));

    Ok(json!({
        "scope": scope.as_json(),
        "task_id": task_id,
        "execution_id": execution_id,
        "source_counts": source_counts,
        "errors": errors,
        "entry_count": merged.len(),
        "entries": merged
    }))
}

fn list_execution_files(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let execution_id = required_string(params, "execution_id")?;
    let task_id = string_param(params, "task_id");
    let max_depth = bounded_usize(params, "max_depth", 4, 8);
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let root = execution_dir(workspace_layout, &scope, task_id.as_deref(), &execution_id);
    let mut files = Vec::new();
    collect_files(&root, &root, 0, max_depth, limit, &mut files)?;
    Ok(json!({
        "scope": scope.as_json(),
        "task_id": task_id,
        "execution_id": execution_id,
        "root": root.display().to_string(),
        "limit": limit,
        "files": files
    }))
}

fn read_execution_file(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let execution_id = required_string(params, "execution_id")?;
    let task_id = string_param(params, "task_id");
    let relative_path = required_string(params, "path")?;
    let max_chars = bounded_usize(
        params,
        "max_chars",
        DEFAULT_FILE_MAX_CHARS,
        MAX_FILE_MAX_CHARS,
    );
    let root = execution_dir(workspace_layout, &scope, task_id.as_deref(), &execution_id);
    let file_path = resolve_existing_child_file(&root, &relative_path)?;
    let content = if Path::new(&relative_path) == Path::new("events.jsonl") {
        let commit_path = file_path.with_file_name("events.jsonl.commit");
        let committed_len = committed_jsonl_len_sync(&file_path, &commit_path)?;
        filter_transport_event_text(read_text_file_limited_through(
            &file_path,
            max_chars,
            committed_len,
        )?)
    } else {
        read_text_file_limited(&file_path, max_chars)?
    };
    Ok(json!({
        "scope": scope.as_json(),
        "task_id": task_id,
        "execution_id": execution_id,
        "path": file_path.display().to_string(),
        "relative_path": relative_path,
        "max_chars": max_chars,
        "truncated": content.truncated,
        "content": content.content
    }))
}

fn review_memory_effects(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    // The resurfacing engine lives in magician-comms; the lib cannot import
    // that crate. Read the same sqlite/json files the engine persists.
    let scope = scope_from_params(params);
    let limit = bounded_usize(params, "limit", 200, 500);
    let mode = persisted_memory_effect_mode(workspace_layout.base_root());
    let db_path = workspace_layout.base_root().join("resurfacing.db");
    let judgements = if db_path.is_file() {
        read_memory_judgement_jsons(&db_path, &scope.principal, &scope.workspace, limit)?
    } else {
        Vec::new()
    };
    Ok(recommend_memory_effect_review(mode, &judgements))
}

fn persisted_memory_effect_mode(base_root: &Path) -> &'static str {
    let path = base_root.join("memory_effect_mode.json");
    let Ok(bytes) = fs::read(path) else {
        return "shadow";
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return "shadow";
    };
    match value.get("mode").and_then(Value::as_str) {
        Some("canary") => "canary",
        Some("enforced") => "enforced",
        _ => "shadow",
    }
}

fn read_memory_judgement_jsons(
    db_path: &Path,
    principal: &str,
    workspace: &str,
    limit: usize,
) -> Result<Vec<Value>, ExecutionError> {
    let conn = rusqlite::Connection::open(db_path).map_err(|error| {
        ExecutionError::Step(format!("internal_data review_memory_effects open: {error}"))
    })?;
    let mut stmt = conn
        .prepare(
            "SELECT judgement_json FROM resurfacing_memory_applications
             WHERE principal = ? AND workspace = ?
             ORDER BY recorded_at DESC
             LIMIT ?",
        )
        .map_err(|error| {
            ExecutionError::Step(format!("internal_data review_memory_effects: {error}"))
        })?;
    let rows = stmt
        .query_map(
            rusqlite::params![principal, workspace, limit as i64],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| {
            ExecutionError::Step(format!("internal_data review_memory_effects: {error}"))
        })?;
    let mut out = Vec::new();
    for row in rows {
        let json = row.map_err(|error| {
            ExecutionError::Step(format!("internal_data review_memory_effects: {error}"))
        })?;
        match serde_json::from_str::<Value>(&json) {
            Ok(value) => out.push(value),
            Err(error) => {
                return Err(ExecutionError::Step(format!(
                    "internal_data review_memory_effects: {error}"
                )))
            },
        }
    }
    Ok(out)
}

fn recommend_memory_effect_review(mode: &str, judgements: &[Value]) -> Value {
    let mut unique = BTreeSet::<String>::new();
    let mut would_suppress_count = 0u32;
    let mut explain_count = 0u32;
    let mut conflict_count = 0u32;
    for judgement in judgements {
        if judgement
            .get("would_suppress")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            would_suppress_count += 1;
        }
        if judgement
            .get("conflicts")
            .and_then(Value::as_array)
            .is_some_and(|conflicts| !conflicts.is_empty())
        {
            conflict_count += 1;
        }
        if let Some(applications) = judgement
            .get("applications")
            .and_then(|value| value.get("would_apply"))
            .and_then(Value::as_array)
        {
            for application in applications {
                if let Some(key) = application.get("memory_key").and_then(Value::as_str) {
                    unique.insert(key.to_owned());
                }
                if application.get("direction").and_then(Value::as_str) == Some("explain") {
                    explain_count += 1;
                }
            }
        }
    }
    let judgement_count = judgements.len() as u32;
    let (advice, reason, next_step) = if judgement_count == 0 {
        (
            "collect_shadow_evidence",
            "no shadow judgements recorded".to_owned(),
            "confirm a scoped preference, let Today/Worth score, then re-run review_memory_effects"
                .to_owned(),
        )
    } else if explain_count == 0 && would_suppress_count == 0 {
        (
            "investigate_attachment",
            format!("{judgement_count} judgements recorded but none would explain or suppress"),
            "check scope on /memory — empty topics/entities never attach".to_owned(),
        )
    } else {
        match mode {
            "canary" => {
                if judgement_count >= 50
                    && would_suppress_count >= 1
                    && conflict_count * 2 <= would_suppress_count
                {
                    (
                        "advance_to_enforced",
                        format!(
                            "canary has {judgement_count} judgements, {would_suppress_count} would-hide, {conflict_count} conflicts"
                        ),
                        "inspect would-hide cards, then accept the AttentionBar prompt or POST /memory/effect-review to switch to Enforced"
                            .to_owned(),
                    )
                } else {
                    (
                        "stay_in_canary_collect_labels",
                        format!(
                            "canary not ready for hide (judgements {judgement_count}, would-hide {would_suppress_count}, conflicts {conflict_count})"
                        ),
                        "stay in Canary; collect opens/dismisses on down-ranked cards".to_owned(),
                    )
                }
            }
            "enforced" => (
                "stay_enforced",
                "enforced hide is already live".to_owned(),
                "watch conflict ratio; POST stay-equivalent is a no-op, or set mode back to Canary if hides look wrong"
                    .to_owned(),
            ),
            _ => {
                if judgement_count >= 20 && explain_count >= 3 && would_suppress_count >= 1 {
                    (
                        "advance_to_canary",
                        format!(
                            "shadow has {judgement_count} judgements, {explain_count} explains, {would_suppress_count} would-hide"
                        ),
                        "accept the AttentionBar prompt or POST /memory/effect-review to switch to Canary (salience only, still no hide)"
                            .to_owned(),
                    )
                } else {
                    (
                        "collect_shadow_evidence",
                        format!(
                            "need ≥20 judgements, ≥3 explains, ≥1 would-hide; have {judgement_count}/{explain_count}/{would_suppress_count}"
                        ),
                        "keep Shadow; confirm more scoped preferences and wait for scorer passes"
                            .to_owned(),
                    )
                }
            }
        }
    };
    json!({
        "observation": {
            "mode": mode,
            "judgement_count": judgement_count,
            "would_suppress_count": would_suppress_count,
            "explain_count": explain_count,
            "conflict_count": conflict_count,
            "unique_memory_keys": unique.len() as u32,
        },
        "advice": advice,
        "reason": reason,
        "next_step": next_step,
    })
}

fn memory_regression_status(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let memory_root = workspace_layout.memory_root(&scope.principal, &scope.workspace);
    let path = crate::magician_v2::analytics::memory_eval_runner::memory_regression_status_path(
        &memory_root,
    );
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let status: Value = serde_json::from_str(&raw).map_err(|e| {
                ExecutionError::Step(format!(
                    "internal_data memory_regression_status parse {}: {e}",
                    path.display()
                ))
            })?;
            Ok(json!({
                "scope": scope.as_json(),
                "path": path.display().to_string(),
                "exists": true,
                "status": status
            }))
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({
            "scope": scope.as_json(),
            "path": path.display().to_string(),
            "exists": false,
            "status": {
                "schema_version": 1,
                "principal": scope.principal,
                "workspace": scope.workspace,
                "status": "unknown",
                "status_reason": "No memory regression status snapshot has been written yet.",
                "suite_count": 0,
                "case_count": 0,
                "passed_count": 0,
                "failed_count": 0,
                "direct_fallback_count": 0,
                "failing_cases": []
            }
        })),
        Err(error) => Err(ExecutionError::Step(format!(
            "internal_data memory_regression_status read {}: {error}",
            path.display()
        ))),
    }
}

fn learning_audit(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let report = build_learning_gap_audit(workspace_layout, &scope.principal, &scope.workspace)
        .map_err(|e| ExecutionError::Step(format!("internal_data learning_audit: {e}")))?;
    serde_json::to_value(report)
        .map_err(|e| ExecutionError::Step(format!("internal_data learning_audit: {e}")))
}

fn list_learning_candidates(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let store = LearningStore::new(workspace_layout.clone());
    let candidates = store
        .list_candidates(
            &learning_scope,
            LearningCandidateFilters {
                state: string_param(params, "state"),
                candidate_type: string_param(params, "candidate_type"),
                source_agent_id: string_param(params, "source_agent_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!("internal_data list_learning_candidates: {e}"))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": candidates.len(),
        "candidates": candidates
    }))
}

fn read_learning_candidate(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let candidate_id = required_string(params, "candidate_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    LearningStore::new(workspace_layout.clone())
        .read_candidate_with_decisions(&learning_scope, &candidate_id)
        .map_err(|e| ExecutionError::Step(format!("internal_data read_learning_candidate: {e}")))
}

fn list_learning_evaluations(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let store = LearningStore::new(workspace_layout.clone());
    let items = store
        .list_evaluation_backlog_items(
            &learning_scope,
            LearningEvaluationBacklogFilters {
                status: string_param(params, "status"),
                target_agent_id: string_param(params, "target_agent_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!("internal_data list_learning_evaluations: {e}"))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": items.len(),
        "evaluations": items
    }))
}

fn read_learning_evaluation(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let candidate_id = required_string(params, "candidate_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let item = LearningStore::new(workspace_layout.clone())
        .read_evaluation_backlog_item(&learning_scope, &candidate_id)
        .map_err(|e| {
            ExecutionError::Step(format!("internal_data read_learning_evaluation: {e}"))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "evaluation": item
    }))
}

fn list_learning_evaluation_runs(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let store = LearningStore::new(workspace_layout.clone());
    let runs = store
        .list_evaluation_run_reports(
            &learning_scope,
            LearningEvaluationRunFilters {
                status: string_param(params, "status"),
                candidate_id: string_param(params, "candidate_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!("internal_data list_learning_evaluation_runs: {e}"))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": runs.len(),
        "runs": runs
    }))
}

fn read_learning_evaluation_run(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let candidate_id = required_string(params, "candidate_id")?;
    let run_id = required_string(params, "run_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let run = LearningStore::new(workspace_layout.clone())
        .read_evaluation_run_report(&learning_scope, &candidate_id, &run_id)
        .map_err(|e| {
            ExecutionError::Step(format!("internal_data read_learning_evaluation_run: {e}"))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "run": run
    }))
}

fn list_learning_growth_evaluations(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let store = LearningStore::new(workspace_layout.clone());
    let runs = store
        .list_growth_evaluation_run_reports(
            &learning_scope,
            LearningGrowthEvaluationRunFilters {
                status: string_param(params, "status"),
                suite_id: string_param(params, "suite_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_growth_evaluations: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": runs.len(),
        "runs": runs
    }))
}

fn read_learning_growth_evaluation(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let run_id = required_string(params, "run_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let run = LearningStore::new(workspace_layout.clone())
        .read_growth_evaluation_run_report(&learning_scope, &run_id)
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data read_learning_growth_evaluation: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "run": run
    }))
}

fn list_learning_procedures(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let procedures = LearningStore::new(workspace_layout.clone())
        .list_procedures(
            &learning_scope,
            LearningProcedureFilters {
                status: string_param(params, "status"),
                owner_agent: string_param(params, "owner_agent"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!("internal_data list_learning_procedures: {e}"))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": procedures.len(),
        "procedures": procedures
    }))
}

fn read_learning_procedure(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let procedure_id = required_string(params, "procedure_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    LearningStore::new(workspace_layout.clone())
        .read_procedure_with_decisions(&learning_scope, &procedure_id)
        .map_err(|e| ExecutionError::Step(format!("internal_data read_learning_procedure: {e}")))
}

fn list_learning_capability_evolution(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let store = LearningStore::new(workspace_layout.clone());
    let items = store
        .list_capability_evolution_backlog_items(
            &learning_scope,
            LearningCapabilityEvolutionBacklogFilters {
                status: string_param(params, "status"),
                candidate_type: string_param(params, "candidate_type"),
                capability_id: string_param(params, "capability_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_capability_evolution: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": items.len(),
        "capability_evolution": items
    }))
}

fn read_learning_capability_evolution(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let candidate_id = required_string(params, "candidate_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let item = LearningStore::new(workspace_layout.clone())
        .read_capability_evolution_backlog_item(&learning_scope, &candidate_id)
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data read_learning_capability_evolution: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "capability_evolution": item
    }))
}

fn list_learning_capability_proposals(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let store = LearningStore::new(workspace_layout.clone());
    let proposals = store
        .list_capability_evolution_proposals(
            &learning_scope,
            LearningCapabilityEvolutionProposalFilters {
                status: string_param(params, "status"),
                capability_id: string_param(params, "capability_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_capability_proposals: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": proposals.len(),
        "proposals": proposals
    }))
}

fn read_learning_capability_proposal(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let candidate_id = required_string(params, "candidate_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let proposal = LearningStore::new(workspace_layout.clone())
        .read_capability_evolution_proposal(&learning_scope, &candidate_id)
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data read_learning_capability_proposal: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "proposal": proposal
    }))
}

fn list_learning_capability_validations(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let store = LearningStore::new(workspace_layout.clone());
    let validations = store
        .list_capability_evolution_validation_reports(
            &learning_scope,
            LearningCapabilityEvolutionValidationFilters {
                status: string_param(params, "status"),
                candidate_id: string_param(params, "candidate_id"),
                capability_id: string_param(params, "capability_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_capability_validations: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": validations.len(),
        "validations": validations
    }))
}

fn read_learning_capability_validation(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let candidate_id = required_string(params, "candidate_id")?;
    let validation_id = required_string(params, "validation_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let validation = LearningStore::new(workspace_layout.clone())
        .read_capability_evolution_validation_report(&learning_scope, &candidate_id, &validation_id)
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data read_learning_capability_validation: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "validation": validation
    }))
}

fn list_learning_capability_implementations(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let implementations = LearningStore::new(workspace_layout.clone())
        .list_capability_evolution_implementation_records(
            &learning_scope,
            LearningCapabilityEvolutionImplementationFilters {
                candidate_id: string_param(params, "candidate_id"),
                capability_id: string_param(params, "capability_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_capability_implementations: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": implementations.len(),
        "implementations": implementations
    }))
}

fn read_learning_capability_implementation(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let candidate_id = required_string(params, "candidate_id")?;
    let implementation_id = required_string(params, "implementation_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let implementation = LearningStore::new(workspace_layout.clone())
        .read_capability_evolution_implementation_record(
            &learning_scope,
            &candidate_id,
            &implementation_id,
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data read_learning_capability_implementation: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "implementation": implementation
    }))
}

fn list_learning_capability_applications(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let applications = LearningStore::new(workspace_layout.clone())
        .list_capability_evolution_application_records(
            &learning_scope,
            LearningCapabilityEvolutionApplicationFilters {
                candidate_id: string_param(params, "candidate_id"),
                capability_id: string_param(params, "capability_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_capability_applications: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": applications.len(),
        "applications": applications
    }))
}

fn read_learning_capability_application(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let candidate_id = required_string(params, "candidate_id")?;
    let application_id = required_string(params, "application_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let application = LearningStore::new(workspace_layout.clone())
        .read_capability_evolution_application_record(
            &learning_scope,
            &candidate_id,
            &application_id,
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data read_learning_capability_application: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "application": application
    }))
}

fn list_learning_capability_rollback_recommendations(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let recommendations = LearningStore::new(workspace_layout.clone())
        .list_capability_evolution_rollback_recommendation_records(
            &learning_scope,
            LearningCapabilityEvolutionRollbackRecommendationFilters {
                status: string_param(params, "status"),
                candidate_id: string_param(params, "candidate_id"),
                capability_id: string_param(params, "capability_id"),
                application_id: string_param(params, "application_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_capability_rollback_recommendations: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": recommendations.len(),
        "rollback_recommendations": recommendations
    }))
}

fn read_learning_capability_rollback_recommendation(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let candidate_id = required_string(params, "candidate_id")?;
    let recommendation_id = required_string(params, "recommendation_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let recommendation = LearningStore::new(workspace_layout.clone())
        .read_capability_evolution_rollback_recommendation_record(
            &learning_scope,
            &candidate_id,
            &recommendation_id,
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data read_learning_capability_rollback_recommendation: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "rollback_recommendation": recommendation
    }))
}

fn list_learning_capability_post_promotion_monitors(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let monitors = LearningStore::new(workspace_layout.clone())
        .list_capability_evolution_post_promotion_monitor_records(
            &learning_scope,
            LearningCapabilityEvolutionPostPromotionMonitorFilters {
                status: string_param(params, "status"),
                candidate_id: string_param(params, "candidate_id"),
                capability_id: string_param(params, "capability_id"),
                promotion_id: string_param(params, "promotion_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_capability_post_promotion_monitors: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": monitors.len(),
        "post_promotion_monitors": monitors
    }))
}

fn read_learning_capability_post_promotion_monitor(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let promotion_id = required_string(params, "promotion_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let monitor = LearningStore::new(workspace_layout.clone())
        .read_capability_evolution_post_promotion_monitor_record(&learning_scope, &promotion_id)
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data read_learning_capability_post_promotion_monitor: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "post_promotion_monitor": monitor
    }))
}

fn list_learning_capability_promotions(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let promotions = LearningStore::new(workspace_layout.clone())
        .list_capability_evolution_promotion_records(
            &learning_scope,
            LearningCapabilityEvolutionPromotionFilters {
                candidate_id: string_param(params, "candidate_id"),
                capability_id: string_param(params, "capability_id"),
                limit: Some(limit),
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_capability_promotions: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": promotions.len(),
        "promotions": promotions
    }))
}

fn read_learning_capability_promotion(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let promotion_id = required_string(params, "promotion_id")?;
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let promotion = LearningStore::new(workspace_layout.clone())
        .read_capability_evolution_promotion_record(&learning_scope, &promotion_id)
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data read_learning_capability_promotion: {e}"
            ))
        })?;
    Ok(json!({
        "scope": scope.as_json(),
        "promotion": promotion
    }))
}

fn list_learning_events(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let max_lines = bounded_usize(params, "max_lines", DEFAULT_LINE_LIMIT, MAX_LINE_LIMIT);
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let events = LearningStore::new(workspace_layout.clone())
        .list_events(&learning_scope, max_lines)
        .map_err(|e| ExecutionError::Step(format!("internal_data list_learning_events: {e}")))?;
    Ok(json!({
        "scope": scope.as_json(),
        "count": events.len(),
        "events": events
    }))
}

fn list_learning_feed_insights(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    budget: Option<&InternalQueryBudget>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params);
    let limit = bounded_usize(params, "limit", DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
    let item_type = string_param(params, "item_type");
    let status = string_param(params, "status");
    let db_path = workspace_layout.ui_feed_db_path(&scope.principal, &scope.workspace);
    let source_projection = learning_feed_source_projection(
        workspace_layout,
        &scope,
        item_type.as_deref(),
        status.as_deref(),
        limit,
    )?;
    let source_projection_count = source_projection["count"].as_u64().unwrap_or(0);
    if !db_path.exists() {
        let source_projection_ids = source_projection_item_ids(&source_projection);
        let missing_source_ids = source_projection_ids.iter().cloned().collect::<Vec<_>>();
        return Ok(json!({
            "scope": scope.as_json(),
            "db_path": db_path.display().to_string(),
            "count": 0,
            "summary": learning_feed_projection_summary(&[]),
            "projection_source": "source_backed_no_feed_db",
            "projection_stale": !missing_source_ids.is_empty(),
            "projection_diagnostics": {
                "missing_source_ids": missing_source_ids,
                "extra_projection_ids": []
            },
            "source_projection": source_projection,
            "items": []
        }));
    }
    let conn = Connection::open(&db_path).map_err(|e| {
        ExecutionError::Step(format!(
            "internal_data list_learning_feed_insights open {}: {e}",
            db_path.display()
        ))
    })?;
    configure_internal_analytics_connection(&conn, "list_learning_feed_insights")?;
    let (items, items_truncated) = run_internal_duckdb_operation(
        &conn,
        budget,
        "list_learning_feed_insights",
        || {
            let mut stmt = conn
                .prepare(
                    "SELECT id, item_type, title, summary, status, task_id, ui_thread_id, agent_id, \
                        created_at, updated_at, metadata_json \
                 FROM feed_items \
                 WHERE principal = ? \
                   AND workspace = ? \
                   AND item_type IN ('learning_insight', 'learning_candidate') \
                   AND (? IS NULL OR item_type = ?) \
                   AND (? IS NULL OR status = ?) \
                 ORDER BY updated_at DESC, created_at DESC \
                 LIMIT ?",
                )
                .map_err(|e| {
                    ExecutionError::Step(format!(
                        "internal_data list_learning_feed_insights prepare: {e}"
                    ))
                })?;
            let query_limit = limit.saturating_add(1);
            let limit_i64 = i64::try_from(query_limit).unwrap_or(i64::MAX);
            let mut rows = stmt
                .query(params![
                    scope.principal.clone(),
                    scope.workspace.clone(),
                    item_type.clone(),
                    item_type.clone(),
                    status.clone(),
                    status.clone(),
                    limit_i64
                ])
                .map_err(|e| {
                    ExecutionError::Step(format!(
                        "internal_data list_learning_feed_insights query: {e}"
                    ))
                })?;
            let mut items = Vec::new();
            let mut materialized_bytes = 0usize;
            let mut truncated = false;
            'rows: while let Some(row) = rows.next().map_err(|e| {
                ExecutionError::Step(format!(
                    "internal_data list_learning_feed_insights row: {e}"
                ))
            })? {
                if items.len() >= limit {
                    truncated = true;
                    break;
                }
                let mut row_bytes = 64usize;
                for column_index in 0..=10 {
                    let value_ref = row.get_ref(column_index).map_err(|error| {
                        ExecutionError::Step(format!(
                            "internal_data list_learning_feed_insights column {column_index} decode error: {error}"
                        ))
                    })?;
                    if let Some(value_bytes) = duckdb_value_ref_output_bytes(value_ref) {
                        if materialized_bytes
                            .saturating_add(row_bytes)
                            .saturating_add(value_bytes)
                            > ANALYTICS_DUCKDB_MAX_RESULT_BYTES
                        {
                            truncated = true;
                            break 'rows;
                        }
                        row_bytes = row_bytes.saturating_add(value_bytes);
                    }
                }
                let metadata_json: String = row.get(10).map_err(|e| {
                    ExecutionError::Step(format!(
                        "internal_data list_learning_feed_insights metadata: {e}"
                    ))
                })?;
                let metadata =
                    serde_json::from_str::<Value>(&metadata_json).unwrap_or_else(|error| {
                        json!({
                            "parse_error": error.to_string(),
                            "raw": metadata_json
                        })
                    });
                let item = json!({
                    "id": row.get::<_, String>(0).unwrap_or_default(),
                    "item_type": row.get::<_, String>(1).unwrap_or_default(),
                    "title": row.get::<_, String>(2).unwrap_or_default(),
                    "summary": row.get::<_, Option<String>>(3).unwrap_or(None),
                    "status": row.get::<_, String>(4).unwrap_or_default(),
                    "task_id": row.get::<_, Option<String>>(5).unwrap_or(None),
                    "ui_thread_id": row.get::<_, Option<String>>(6).unwrap_or(None),
                    "agent_id": row.get::<_, Option<String>>(7).unwrap_or(None),
                    "created_at": row.get::<_, i64>(8).unwrap_or_default(),
                    "updated_at": row.get::<_, i64>(9).unwrap_or_default(),
                    "metadata": metadata
                });
                let serialized_bytes = serde_json::to_vec(&item)
                    .map(|serialized| serialized.len())
                    .unwrap_or(usize::MAX);
                if materialized_bytes.saturating_add(serialized_bytes)
                    > ANALYTICS_DUCKDB_MAX_RESULT_BYTES
                {
                    truncated = true;
                    break;
                }
                materialized_bytes = materialized_bytes.saturating_add(serialized_bytes);
                items.push(item);
            }
            Ok((items, truncated))
        },
    )?;
    let summary = learning_feed_projection_summary(&items);
    let source_projection_ids = source_projection_item_ids(&source_projection);
    let stored_projection_ids = feed_item_ids(&items);
    let missing_source_ids = source_projection_ids
        .difference(&stored_projection_ids)
        .cloned()
        .collect::<Vec<_>>();
    let extra_projection_ids = stored_projection_ids
        .difference(&source_projection_ids)
        .cloned()
        .collect::<Vec<_>>();
    Ok(json!({
        "scope": scope.as_json(),
        "db_path": db_path.display().to_string(),
        "projection_source": "stored_feed_projection_with_source_diagnostics",
        "projection_stale": !missing_source_ids.is_empty() || !extra_projection_ids.is_empty(),
        "projection_diagnostics": {
            "source_count": source_projection_count,
            "stored_count": items.len(),
            "missing_source_ids": missing_source_ids,
            "extra_projection_ids": extra_projection_ids
        },
        "source_projection": source_projection,
        "filters": {
            "item_type": item_type,
            "status": status,
            "limit": limit
        },
        "count": items.len(),
        "truncated": items_truncated,
        "summary": summary,
        "items": items
    }))
}

fn learning_feed_projection_summary(items: &[Value]) -> Value {
    let mut by_item_type = BTreeMap::new();
    let mut by_status = BTreeMap::new();
    let mut by_source_type = BTreeMap::new();
    let mut by_insight_kind = BTreeMap::new();
    let mut with_evidence = 0usize;
    let mut actionable = 0usize;

    for item in items {
        let Some(record) = item.as_object() else {
            continue;
        };
        bump_summary_count(
            &mut by_item_type,
            record.get("item_type").and_then(Value::as_str),
        );
        let status = record.get("status").and_then(Value::as_str);
        bump_summary_count(&mut by_status, status);
        if status == Some("needs_action") {
            actionable += 1;
        }
        let metadata = record.get("metadata").and_then(Value::as_object);
        bump_summary_count(
            &mut by_source_type,
            metadata
                .and_then(|metadata| metadata.get("source_type"))
                .and_then(Value::as_str),
        );
        bump_summary_count(
            &mut by_insight_kind,
            metadata
                .and_then(|metadata| metadata.get("insight_kind"))
                .and_then(Value::as_str),
        );
        if metadata
            .and_then(|metadata| metadata.get("evidence_refs"))
            .and_then(Value::as_array)
            .map(|refs| !refs.is_empty())
            .unwrap_or(false)
        {
            with_evidence += 1;
        }
    }

    json!({
        "total": items.len(),
        "actionable": actionable,
        "with_evidence": with_evidence,
        "by_item_type": by_item_type,
        "by_status": by_status,
        "by_source_type": by_source_type,
        "by_insight_kind": by_insight_kind
    })
}

fn feed_item_ids(items: &[Value]) -> BTreeSet<String> {
    items
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

fn source_projection_item_ids(projection: &Value) -> BTreeSet<String> {
    projection
        .get("items")
        .and_then(Value::as_array)
        .map(|items| feed_item_ids(items))
        .unwrap_or_default()
}

fn learning_feed_source_projection(
    workspace_layout: &ArtifactV2Workspace,
    scope: &Scope,
    item_type_filter: Option<&str>,
    status_filter: Option<&str>,
    limit: usize,
) -> Result<Value, ExecutionError> {
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let store = LearningStore::new(workspace_layout.clone());
    let mut items = Vec::new();

    let candidates = store
        .list_candidates(
            &learning_scope,
            LearningCandidateFilters {
                limit: None,
                ..Default::default()
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_feed_insights source candidates: {e}"
            ))
        })?;
    for candidate in candidates {
        if let Some(item) = source_candidate_learning_feed_item(&candidate) {
            push_filtered_source_item(
                &mut items,
                item,
                item_type_filter,
                status_filter,
                workspace_layout,
                scope,
            );
        }
    }

    let events = store.list_events(&learning_scope, 500).map_err(|e| {
        ExecutionError::Step(format!(
            "internal_data list_learning_feed_insights source events: {e}"
        ))
    })?;
    for event in events {
        if let Some(item) = source_event_learning_feed_item(&event) {
            push_filtered_source_item(
                &mut items,
                item,
                item_type_filter,
                status_filter,
                workspace_layout,
                scope,
            );
        }
    }

    let eval_reports = store
        .list_evaluation_run_reports(
            &learning_scope,
            LearningEvaluationRunFilters {
                limit: Some(100),
                ..Default::default()
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_feed_insights source eval reports: {e}"
            ))
        })?;
    for report in eval_reports {
        push_filtered_source_item(
            &mut items,
            source_eval_report_learning_feed_item(&report),
            item_type_filter,
            status_filter,
            workspace_layout,
            scope,
        );
    }

    let growth_reports = store
        .list_growth_evaluation_run_reports(
            &learning_scope,
            LearningGrowthEvaluationRunFilters {
                limit: Some(50),
                ..Default::default()
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_feed_insights source growth eval reports: {e}"
            ))
        })?;
    for report in growth_reports {
        push_filtered_source_item(
            &mut items,
            source_growth_report_learning_feed_item(&report),
            item_type_filter,
            status_filter,
            workspace_layout,
            scope,
        );
    }

    let backlog_items = store
        .list_evaluation_backlog_items(
            &learning_scope,
            LearningEvaluationBacklogFilters {
                limit: Some(100),
                ..Default::default()
            },
        )
        .map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data list_learning_feed_insights source eval backlog: {e}"
            ))
        })?;
    for backlog_item in backlog_items {
        push_filtered_source_item(
            &mut items,
            source_backlog_learning_feed_item(&backlog_item),
            item_type_filter,
            status_filter,
            workspace_layout,
            scope,
        );
    }

    items.sort_by(|left, right| {
        right
            .get("updated_at")
            .and_then(Value::as_i64)
            .cmp(&left.get("updated_at").and_then(Value::as_i64))
    });
    items.truncate(limit);
    let summary = learning_feed_projection_summary(&items);
    Ok(json!({
        "count": items.len(),
        "summary": summary,
        "items": items
    }))
}

fn push_filtered_source_item(
    items: &mut Vec<Value>,
    item: Value,
    item_type_filter: Option<&str>,
    status_filter: Option<&str>,
    workspace_layout: &ArtifactV2Workspace,
    scope: &Scope,
) {
    if source_learning_item_is_archived(workspace_layout, scope, &item) {
        return;
    }
    if item_type_filter
        .is_some_and(|expected| item.get("item_type").and_then(Value::as_str) != Some(expected))
    {
        return;
    }
    if status_filter
        .is_some_and(|expected| item.get("status").and_then(Value::as_str) != Some(expected))
    {
        return;
    }
    items.push(item);
}

fn source_candidate_learning_feed_item(candidate: &LearningCandidate) -> Option<Value> {
    if candidate.state.is_terminal() {
        return None;
    }
    let review_card = source_candidate_is_user_memory_review(candidate);
    let item_type = if review_card {
        "learning_candidate"
    } else {
        "learning_insight"
    };
    let status = if review_card
        || candidate.review_required
        || matches!(candidate.risk_level.as_str(), "high" | "critical")
        || matches!(
            candidate.candidate_type.as_str(),
            "bug_report" | "tool_wrapper_fix" | "evaluation_case"
        ) {
        "needs_action"
    } else {
        "info"
    };
    let source_type = if review_card {
        "learning_candidate"
    } else {
        "learning_candidate"
    };
    let id = if review_card {
        format!("learning_candidate:{}", candidate.id)
    } else {
        source_learning_insight_source_feed_id("candidate", &candidate.id)
    };
    Some(source_learning_item(
        id,
        item_type,
        if candidate.title.trim().is_empty() {
            humanize_internal_token(candidate.candidate_type.as_str())
        } else {
            candidate.title.clone()
        },
        Some(candidate.summary.clone()),
        status,
        candidate.created_at.timestamp_millis(),
        candidate.updated_at.timestamp_millis(),
        json!({
            "source_type": source_type,
            "source_id": candidate.id,
            "candidate_id": candidate.id,
            "candidate_type": candidate.candidate_type.as_str(),
            "candidate_state": candidate.state.as_str(),
            "insight_kind": source_candidate_insight_kind(candidate.candidate_type.as_str()),
            "target_scope": source_candidate_memory_scope(candidate),
            "target_tier": source_candidate_memory_tier(candidate),
            "risk_level": candidate.risk_level.as_str(),
            "review_required": candidate.review_required,
            "evidence_refs": candidate.evidence_refs,
        }),
    ))
}

fn source_event_learning_feed_item(event: &LearningEvent) -> Option<Value> {
    if !source_event_is_high_signal(event) {
        return None;
    }
    let status = if event.event_type.contains("failed") || event.event_type.contains("error") {
        "failed"
    } else {
        "info"
    };
    Some(source_learning_item(
        source_learning_insight_source_feed_id("event", &event.id),
        "learning_insight",
        format!("Learning {}", humanize_internal_token(&event.event_type)),
        Some(event.summary.clone()),
        status,
        event.created_at.timestamp_millis(),
        event.created_at.timestamp_millis(),
        json!({
            "source_type": "learning_event",
            "source_id": event.id,
            "event_id": event.id,
            "event_type": event.event_type,
            "insight_kind": source_event_kind(&event.event_type),
            "source_agent_id": event.agent_id,
            "source_task_id": event.task_id,
            "source_execution_id": event.execution_id,
            "source_chat_session_id": event.chat_session_id,
            "evidence_refs": event.evidence_refs,
        }),
    ))
}

fn source_eval_report_learning_feed_item(report: &LearningEvaluationRunReport) -> Value {
    source_learning_item(
        source_learning_insight_source_feed_id("eval_run", &report.id),
        "learning_insight",
        format!(
            "Learning evaluation {}",
            humanize_internal_token(report.status.as_str())
        ),
        Some(report.summary.clone()),
        source_eval_status(report.status.as_str()),
        report.created_at.timestamp_millis(),
        report.created_at.timestamp_millis(),
        json!({
            "source_type": "learning_evaluation_run",
            "source_id": report.id,
            "run_id": report.id,
            "candidate_id": report.candidate_id,
            "backlog_id": report.backlog_id,
            "insight_kind": "evaluation_result",
            "evidence_refs": report.evidence_refs,
        }),
    )
}

fn source_growth_report_learning_feed_item(report: &LearningGrowthEvaluationRunReport) -> Value {
    source_learning_item(
        source_learning_insight_source_feed_id("growth_eval", &report.id),
        "learning_insight",
        format!(
            "Growth evaluation {}",
            humanize_internal_token(report.status.as_str())
        ),
        Some(report.summary.clone()),
        source_eval_status(report.status.as_str()),
        report.created_at.timestamp_millis(),
        report.created_at.timestamp_millis(),
        json!({
            "source_type": "learning_growth_evaluation_run",
            "source_id": report.id,
            "run_id": report.id,
            "suite_id": report.suite_id,
            "insight_kind": "growth_evaluation",
            "evidence_refs": report.evidence_refs,
        }),
    )
}

fn source_backlog_learning_feed_item(item: &LearningEvaluationBacklogItem) -> Value {
    let status = match item.status.as_str() {
        "queued" | "in_review" => "needs_action",
        "archived" | "rejected" => "done",
        _ => "info",
    };
    source_learning_item(
        source_learning_insight_source_feed_id("eval_backlog", &item.id),
        "learning_insight",
        if item.title.trim().is_empty() {
            "Learning evaluation candidate".to_string()
        } else {
            item.title.clone()
        },
        Some(item.summary.clone()),
        status,
        item.created_at.timestamp_millis(),
        item.updated_at.timestamp_millis(),
        json!({
            "source_type": "learning_evaluation_backlog",
            "source_id": item.id,
            "backlog_id": item.id,
            "candidate_id": item.candidate_id,
            "insight_kind": "evaluation_candidate",
            "priority": item.priority,
            "evidence_refs": item.evidence_refs,
        }),
    )
}

fn source_learning_item(
    id: String,
    item_type: &str,
    title: String,
    summary: Option<String>,
    status: &str,
    created_at: i64,
    updated_at: i64,
    metadata: Value,
) -> Value {
    json!({
        "id": id,
        "item_type": item_type,
        "title": title,
        "summary": summary,
        "status": status,
        "task_id": metadata.get("source_task_id").cloned().unwrap_or(Value::Null),
        "ui_thread_id": metadata.get("source_chat_session_id").cloned().unwrap_or(Value::Null),
        "agent_id": metadata.get("source_agent_id").cloned().unwrap_or(Value::Null),
        "created_at": created_at,
        "updated_at": updated_at,
        "metadata": metadata
    })
}

fn source_learning_item_is_archived(
    workspace_layout: &ArtifactV2Workspace,
    scope: &Scope,
    item: &Value,
) -> bool {
    if item.get("item_type").and_then(Value::as_str) != Some("learning_insight") {
        return false;
    }
    let Some(id) = item.get("id").and_then(Value::as_str) else {
        return false;
    };
    workspace_layout
        .learning_root(&scope.principal, &scope.workspace)
        .join("feed_insights")
        .join("archived")
        .join(format!("{}.json", source_safe_feed_id_segment(id)))
        .exists()
}

fn source_learning_insight_source_feed_id(source_type: &str, source_id: &str) -> String {
    format!(
        "learning_insight:{}:{}",
        source_safe_feed_id_segment(source_type),
        source_safe_feed_id_segment(source_id)
    )
}

fn source_safe_feed_id_segment(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            output.push(ch);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "unknown".to_string()
    } else {
        output
    }
}

fn source_candidate_is_user_memory_review(candidate: &LearningCandidate) -> bool {
    matches!(
        candidate.candidate_type.as_str(),
        "memory_fact" | "memory_preference"
    ) && matches!(
        candidate.state.as_str(),
        "observed" | "proposed" | "triaged" | "approved" | "evaluated"
    ) && source_candidate_memory_scope(candidate) == "user"
        && crate::magician_v2::chat::service::is_curated_user_memory_tier(
            &source_candidate_memory_tier(candidate),
        )
}

fn source_candidate_memory_scope(candidate: &LearningCandidate) -> String {
    let payload = source_memory_payload(candidate);
    let explicit_user_request = source_read_bool_any(
        payload,
        &[
            "explicit_user_request",
            "explicit_request",
            "user_requested",
        ],
    );
    let explicit_user_correction = source_read_bool_any(
        payload,
        &[
            "explicit_user_correction",
            "explicit_correction",
            "user_correction",
        ],
    );
    source_read_string_any(payload, &["scope", "memory_scope", "target_scope"])
        .or_else(|| source_target_part(candidate.promotion_target.as_deref(), 0))
        .or_else(|| source_target_part(candidate.proposed_target.as_deref(), 0))
        .unwrap_or_else(|| {
            if explicit_user_request
                || explicit_user_correction
                || candidate.candidate_type.as_str() == "memory_preference"
            {
                "user".to_string()
            } else {
                "agent".to_string()
            }
        })
        .trim()
        .to_ascii_lowercase()
}

fn source_candidate_memory_tier(candidate: &LearningCandidate) -> String {
    let payload = source_memory_payload(candidate);
    source_read_string_any(payload, &["target_tier", "tier_name", "tier"])
        .or_else(|| source_target_part(candidate.promotion_target.as_deref(), 1))
        .or_else(|| source_target_part(candidate.proposed_target.as_deref(), 1))
        .unwrap_or_else(|| {
            if candidate.candidate_type.as_str() == "memory_preference" {
                "preferences".to_string()
            } else {
                "knowledge".to_string()
            }
        })
        .trim()
        .to_ascii_lowercase()
}

fn source_memory_payload(candidate: &LearningCandidate) -> &Value {
    candidate
        .proposed_change
        .get("memory")
        .unwrap_or(&candidate.proposed_change)
}

fn source_read_string_any(payload: &Value, keys: &[&str]) -> Option<String> {
    let object = payload.as_object()?;
    keys.iter().find_map(|key| {
        object.get(*key).and_then(|value| match value {
            Value::String(text) => {
                let text = text.trim();
                if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                }
            },
            Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
            _ => None,
        })
    })
}

fn source_read_bool_any(payload: &Value, keys: &[&str]) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    for key in keys {
        match object.get(*key) {
            Some(Value::Bool(value)) => return *value,
            Some(Value::String(value)) => {
                let value = value.trim().to_ascii_lowercase();
                if matches!(value.as_str(), "true" | "yes" | "1") {
                    return true;
                }
                if matches!(value.as_str(), "false" | "no" | "0") {
                    return false;
                }
            },
            _ => {},
        }
    }
    false
}

fn source_target_part(target: Option<&str>, index: usize) -> Option<String> {
    target
        .and_then(|target| target.split('.').nth(index))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn source_candidate_insight_kind(candidate_type: &str) -> &'static str {
    match candidate_type {
        "skill_update" | "workflow_template" | "memory_procedure" => "procedure_or_skill",
        "capability_update" | "tool_schema_update" | "tool_wrapper_fix" => "capability_evolution",
        "agent_persona_update" => "agent_behavior",
        "evaluation_case" => "evaluation_candidate",
        "program_state_update" => "program_state",
        "bug_report" => "bug_pattern",
        "docs_update" => "documentation",
        "memory_fact" | "memory_preference" => "memory",
        _ => "learning",
    }
}

fn source_event_is_high_signal(event: &LearningEvent) -> bool {
    let event_type = event.event_type.as_str();
    if matches!(
        event_type,
        "learning_candidate_created"
            | "learning_memory_candidate_promoted"
            | "memory_index_rebuild_completed"
            | "memory_index_reconcile_completed"
            | "memory_prompt_block_rendered"
    ) {
        return false;
    }
    let summary = event.summary.trim();
    let failure = event_type.contains("failed") || event_type.contains("error");
    !summary.is_empty()
        && (failure
            || event_type.contains("route")
            || event_type.contains("eval")
            || event_type.contains("reflection")
            || (event_type.contains("memory") && event_type.contains("completed"))
            || event_type.contains("teaching")
            || (event_type.contains("retrieval") && failure)
            || (event_type.contains("index") && failure))
}

fn source_event_kind(event_type: &str) -> &'static str {
    if event_type.contains("memory") {
        "memory"
    } else if event_type.contains("eval") {
        "evaluation"
    } else if event_type.contains("reflection") {
        "reflection"
    } else if event_type.contains("route") || event_type.contains("teaching") {
        "agent_behavior"
    } else if event_type.contains("index") || event_type.contains("retrieval") {
        "retrieval"
    } else {
        "learning_event"
    }
}

fn source_eval_status(status: &str) -> &'static str {
    match status {
        "failed" => "failed",
        "blocked" => "needs_action",
        _ => "info",
    }
}

fn humanize_internal_token(value: &str) -> String {
    let words = value
        .split(['_', '-', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>();
    if words.is_empty() {
        "Learning".to_string()
    } else {
        words.join(" ")
    }
}

fn bump_summary_count(map: &mut BTreeMap<String, usize>, key: Option<&str>) {
    let normalized = key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_string();
    *map.entry(normalized).or_insert(0) += 1;
}

#[derive(Debug, Clone)]
struct Scope {
    principal: String,
    workspace: String,
}

impl Scope {
    fn as_json(&self) -> Value {
        json!({
            "principal": self.principal,
            "workspace": self.workspace
        })
    }
}

fn scope_from_params(params: &HashMap<String, Value>) -> Scope {
    let principal = string_param(params, "__principal")
        .or_else(|| string_param(params, "principal"))
        .unwrap_or_else(|| DEFAULT_SCOPE_PRINCIPAL.to_string());
    let workspace = string_param(params, "__workspace")
        .or_else(|| string_param(params, "workspace"))
        .unwrap_or_else(|| DEFAULT_SCOPE_WORKSPACE.to_string());
    Scope {
        principal,
        workspace,
    }
}

fn authorize_runtime_scope(
    mut params: HashMap<String, Value>,
) -> Result<HashMap<String, Value>, ExecutionError> {
    let principal = required_runtime_scope_value(&params, "__principal")?;
    let workspace = required_runtime_scope_value(&params, "__workspace")?;
    if !LlmScope::new(&principal, &workspace).is_valid() {
        return Err(ExecutionError::Step(
            "internal_data runtime scope contains an unsafe principal or workspace component"
                .to_string(),
        ));
    }
    for (public_key, trusted_value) in [
        ("principal", principal.as_str()),
        ("workspace", workspace.as_str()),
    ] {
        let Some(value) = params.get(public_key) else {
            continue;
        };
        let Value::String(value) = value else {
            return Err(ExecutionError::Step(format!(
                "internal_data: `{public_key}` is an optional scope assertion and must be a string"
            )));
        };
        if !value.is_empty() && value != trusted_value {
            return Err(ExecutionError::Step(format!(
                "internal_data: model-supplied `{public_key}` does not match the runtime-authorized scope"
            )));
        }
    }
    // Preserve the public compatibility fields, but force them to the trusted
    // values so every legacy helper and every new action sees one authority.
    params.insert("principal".to_string(), Value::String(principal));
    params.insert("workspace".to_string(), Value::String(workspace));
    Ok(params)
}

fn required_runtime_scope_value(
    params: &HashMap<String, Value>,
    key: &str,
) -> Result<String, ExecutionError> {
    let value = params.get(key).and_then(Value::as_str).ok_or_else(|| {
        ExecutionError::Step(format!(
            "internal_data requires runtime-owned scope `{key}`; unscoped execution is denied"
        ))
    })?;
    if value.is_empty() || value.trim() != value {
        return Err(ExecutionError::Step(format!(
            "internal_data runtime-owned scope `{key}` must be a nonblank canonical component"
        )));
    }
    Ok(value.to_string())
}

fn execution_dir(
    workspace_layout: &ArtifactV2Workspace,
    scope: &Scope,
    task_id: Option<&str>,
    execution_id: &str,
) -> PathBuf {
    match task_id {
        Some(task_id) => workspace_layout.execution_dir(
            &scope.principal,
            &scope.workspace,
            task_id,
            execution_id,
        ),
        None => {
            workspace_layout.scoped_execution_dir(&scope.principal, &scope.workspace, execution_id)
        },
    }
}

fn execution_events_path(
    workspace_layout: &ArtifactV2Workspace,
    scope: &Scope,
    task_id: Option<&str>,
    execution_id: &str,
) -> PathBuf {
    match task_id {
        Some(task_id) => workspace_layout.execution_events_path(
            &scope.principal,
            &scope.workspace,
            task_id,
            execution_id,
        ),
        None => execution_dir(workspace_layout, scope, None, execution_id).join("events.jsonl"),
    }
}

struct QueryRows {
    columns: Vec<String>,
    rows: Vec<Value>,
    row_count: usize,
    truncated: bool,
}

fn run_duckdb_query(
    conn: &Connection,
    sql: &str,
    limit: usize,
    budget: Option<&InternalQueryBudget>,
    action: &str,
) -> Result<QueryRows, ExecutionError> {
    run_internal_duckdb_operation(conn, budget, action, || {
        run_duckdb_query_inner(conn, sql, limit)
    })
}

fn run_internal_duckdb_operation<T, F>(
    conn: &Connection,
    budget: Option<&InternalQueryBudget>,
    action: &str,
    operation: F,
) -> Result<T, ExecutionError>
where
    F: FnOnce() -> Result<T, ExecutionError>,
{
    let Some(budget) = budget else {
        return operation();
    };
    let remaining = budget.remaining(action)?;
    match run_analytics_query_with_interrupt_timeout(conn, remaining, operation) {
        Ok(result) => Ok(result),
        Err(AnalyticsDuckDbQueryError::Query(error)) => Err(error),
        Err(AnalyticsDuckDbQueryError::TimedOut(source)) => {
            Err(budget.timeout_error(action, source.map(|error| error.to_string())))
        },
    }
}

fn run_duckdb_query_inner(
    conn: &Connection,
    sql: &str,
    limit: usize,
) -> Result<QueryRows, ExecutionError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| ExecutionError::Step(format!("internal_data DuckDB prepare error: {e}")))?;
    let mut rows_iter = stmt
        .query([])
        .map_err(|e| ExecutionError::Step(format!("internal_data DuckDB query error: {e}")))?;
    let stmt_ref = rows_iter.as_ref().ok_or_else(|| {
        ExecutionError::Step("internal_data DuckDB query did not expose metadata".to_string())
    })?;
    let column_count = stmt_ref.column_count();
    let columns: Vec<String> = (0..column_count)
        .map(|i| {
            stmt_ref
                .column_name(i)
                .map_or_else(|_| "?".to_string(), |s| s.to_string())
        })
        .collect();

    let effective_limit = limit.min(ANALYTICS_DUCKDB_MAX_RESULT_ROWS);
    let repeated_column_bytes = columns.iter().fold(0usize, |size, column| {
        size.saturating_add(json_string_encoded_bytes(column.as_bytes()))
            .saturating_add(4)
    });
    if repeated_column_bytes > ANALYTICS_DUCKDB_MAX_RESULT_BYTES {
        return Err(ExecutionError::Step(format!(
            "internal_data DuckDB result metadata exceeds the {} byte output limit",
            ANALYTICS_DUCKDB_MAX_RESULT_BYTES
        )));
    }
    let mut materialized_bytes = repeated_column_bytes;
    let mut rows = Vec::new();
    let mut truncated = false;
    'rows: while let Some(row) = rows_iter
        .next()
        .map_err(|e| ExecutionError::Step(format!("internal_data DuckDB row error: {e}")))?
    {
        if rows.len() >= effective_limit {
            truncated = true;
            break;
        }
        let mut row_bytes = repeated_column_bytes.saturating_add(8);
        let mut obj = Map::new();
        for (idx, column) in columns.iter().enumerate() {
            // A decode failure is storage/schema corruption, not an unknown
            // telemetry value. Preserve the compatibility endpoint's existing
            // shape while failing the read closed instead of inventing null.
            let value_ref = row.get_ref(idx).map_err(|error| {
                ExecutionError::Step(format!(
                    "internal_data DuckDB column {idx} decode error: {error}"
                ))
            })?;
            if let Some(value_bytes) = duckdb_value_ref_output_bytes(value_ref) {
                if materialized_bytes
                    .saturating_add(row_bytes)
                    .saturating_add(value_bytes)
                    > ANALYTICS_DUCKDB_MAX_RESULT_BYTES
                {
                    truncated = true;
                    break 'rows;
                }
            }

            let value = duckvalue_to_json(&value_ref.to_owned());
            let value_bytes = serde_json::to_string(&value)
                .map(|serialized| serialized.len())
                .unwrap_or(4);
            if materialized_bytes
                .saturating_add(row_bytes)
                .saturating_add(value_bytes)
                > ANALYTICS_DUCKDB_MAX_RESULT_BYTES
            {
                truncated = true;
                break 'rows;
            }
            row_bytes = row_bytes.saturating_add(value_bytes);
            obj.insert(column.clone(), value);
        }
        materialized_bytes = materialized_bytes.saturating_add(row_bytes);
        rows.push(Value::Object(obj));
    }
    let row_count = rows.len();
    Ok(QueryRows {
        columns,
        rows,
        row_count,
        truncated,
    })
}

fn ensure_safe_select(sql: &str, action: &str) -> Result<(), ExecutionError> {
    if !is_one_read_only_select_statement(sql) {
        return Err(ExecutionError::Step(format!(
            "internal_data {action}: exactly one side-effect-free SELECT/WITH query is required"
        )));
    }
    let lowered = strip_leading_sql_comments(sql).trim_start().to_lowercase();
    const BLOCKED_TOKENS: &[&str] = &[
        " attach ",
        " copy ",
        " create ",
        " delete ",
        " drop ",
        " insert ",
        " install ",
        " load ",
        " pragma ",
        " read_blob",
        " read_csv",
        " read_json",
        " read_parquet",
        " read_text",
        " sqlite_scan",
        " update ",
    ];
    let padded = format!(" {lowered} ");
    if let Some(blocked) = BLOCKED_TOKENS.iter().find(|token| padded.contains(**token)) {
        return Err(ExecutionError::Step(format!(
            "internal_data {action}: blocked SQL token `{}` keeps this tool scoped to internal views",
            blocked.trim()
        )));
    }
    Ok(())
}

fn strip_leading_sql_comments(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = match rest.find('\n') {
                Some(idx) => &rest[idx + 1..],
                None => "",
            }
            .trim_start();
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = match rest.find("*/") {
                Some(idx) => &rest[idx + 2..],
                None => "",
            }
            .trim_start();
        } else {
            return s;
        }
    }
}

fn tail_jsonl_file(
    path: &Path,
    committed_len: Option<u64>,
    max_lines: usize,
    event_type: Option<&str>,
    contains: Option<&str>,
    enforce_transport_privacy: bool,
) -> Result<Value, ExecutionError> {
    if !path.exists() {
        return Ok(json!({
            "path": path.display().to_string(),
            "exists": false,
            "line_count_returned": 0,
            "lines": []
        }));
    }
    let file = fs::File::open(path)
        .map_err(|e| ExecutionError::Step(format!("internal_data read {}: {e}", path.display())))?;
    let reader = BufReader::new(file.take(committed_len.unwrap_or(u64::MAX)));
    let contains_lc = contains.map(|s| s.to_lowercase());
    let event_type_lc = event_type.map(|s| s.to_lowercase());
    let mut lines = VecDeque::with_capacity(max_lines.min(MAX_LINE_LIMIT));
    let mut total_matching = 0usize;
    for line_result in reader.lines() {
        let line = line_result.map_err(|e| {
            ExecutionError::Step(format!("internal_data read {}: {e}", path.display()))
        })?;
        // The physical legacy-log scrub is intentionally asynchronous and can
        // take an unbounded amount of wall time. Generic operational readers
        // must therefore enforce the same fail-closed boundary as `/events`:
        // neither a marked owner notification nor an unclassifiable damaged
        // transport row may bypass its absolute TTL while startup scrubbing is
        // still in progress. Execution indexes are not transport envelopes and
        // opt out at their only call site above.
        if enforce_transport_privacy
            && !crate::magician_v2::transport_log::serialized_event_is_safe_for_generic_backfill(
                &line,
            )
        {
            continue;
        }
        if let Some(needle) = contains_lc.as_deref() {
            if !line.to_lowercase().contains(needle) {
                continue;
            }
        }
        let parsed =
            serde_json::from_str::<Value>(&line).unwrap_or_else(|_| json!({ "raw": line }));
        if let Some(expected) = event_type_lc.as_deref() {
            let actual = parsed
                .get("event_type")
                .or_else(|| parsed.get("type"))
                .or_else(|| parsed.get("event"))
                .and_then(Value::as_str)
                .map(str::to_lowercase);
            if actual.as_deref() != Some(expected) {
                continue;
            }
        }
        total_matching += 1;
        lines.push_back(parsed);
        if lines.len() > max_lines {
            lines.pop_front();
        }
    }
    Ok(json!({
        "path": path.display().to_string(),
        "exists": true,
        "matched_line_count": total_matching,
        "line_count_returned": lines.len(),
        "tail": true,
        "lines": lines.into_iter().collect::<Vec<_>>()
    }))
}

fn committed_jsonl_len_sync(path: &Path, commit_path: &Path) -> Result<u64, ExecutionError> {
    let physical_len = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(ExecutionError::Step(format!(
                "internal_data read metadata {}: {error}",
                path.display()
            )))
        },
    };
    let committed_len = match fs::read_to_string(commit_path) {
        Ok(value) => value.trim().parse::<u64>().map_err(|error| {
            ExecutionError::Step(format!(
                "internal_data invalid commit authority {}: {error}",
                commit_path.display()
            ))
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => physical_len,
        Err(error) => {
            return Err(ExecutionError::Step(format!(
                "internal_data read commit authority {}: {error}",
                commit_path.display()
            )))
        },
    };
    if physical_len < committed_len {
        return Err(ExecutionError::Step(format!(
            "internal_data commit authority {} exceeds physical events length: {committed_len} > {physical_len}",
            commit_path.display()
        )));
    }
    Ok(committed_len)
}

fn read_json_file_optional(path: &Path) -> Result<Value, ExecutionError> {
    if !path.exists() {
        return Ok(Value::Null);
    }
    let content = fs::read_to_string(path)
        .map_err(|e| ExecutionError::Step(format!("internal_data read {}: {e}", path.display())))?;
    serde_json::from_str(&content)
        .map_err(|e| ExecutionError::Step(format!("internal_data parse {}: {e}", path.display())))
}

struct LimitedText {
    content: String,
    truncated: bool,
}

fn read_text_file_limited(path: &Path, max_chars: usize) -> Result<LimitedText, ExecutionError> {
    let content = fs::read_to_string(path)
        .map_err(|e| ExecutionError::Step(format!("internal_data read {}: {e}", path.display())))?;
    let mut chars = content.chars();
    let limited: String = chars.by_ref().take(max_chars).collect();
    let truncated = chars.next().is_some();
    Ok(LimitedText {
        content: limited,
        truncated,
    })
}

fn read_text_file_limited_through(
    path: &Path,
    max_chars: usize,
    committed_len: u64,
) -> Result<LimitedText, ExecutionError> {
    let file = fs::File::open(path)
        .map_err(|e| ExecutionError::Step(format!("internal_data read {}: {e}", path.display())))?;
    let character_probe_bytes = max_chars.saturating_add(1).saturating_mul(4) as u64;
    let read_limit = committed_len.min(character_probe_bytes);
    let mut bytes = Vec::with_capacity(read_limit.min(usize::MAX as u64) as usize);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|e| ExecutionError::Step(format!("internal_data read {}: {e}", path.display())))?;
    let authoritative_bytes_remain = committed_len > bytes.len() as u64;
    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(error) if error.utf8_error().error_len().is_none() => {
            let valid_up_to = error.utf8_error().valid_up_to();
            String::from_utf8(error.into_bytes()[..valid_up_to].to_vec()).map_err(|error| {
                ExecutionError::Step(format!(
                    "internal_data read {} as UTF-8: {error}",
                    path.display()
                ))
            })?
        },
        Err(error) => {
            return Err(ExecutionError::Step(format!(
                "internal_data read {} as UTF-8: {error}",
                path.display()
            )))
        },
    };
    let mut chars = content.chars();
    let limited: String = chars.by_ref().take(max_chars).collect();
    let truncated = authoritative_bytes_remain || chars.next().is_some();
    Ok(LimitedText {
        content: limited,
        truncated,
    })
}

fn filter_transport_event_text(input: LimitedText) -> LimitedText {
    let mut content = String::with_capacity(input.content.len());
    for line in input.content.split_inclusive('\n') {
        let serialized = line.strip_suffix('\n').unwrap_or(line);
        if crate::magician_v2::transport_log::serialized_event_is_safe_for_generic_backfill(
            serialized,
        ) {
            content.push_str(line);
        }
    }
    LimitedText {
        content,
        truncated: input.truncated,
    }
}

#[derive(Debug)]
struct DirEntryInfo {
    name: String,
    path: PathBuf,
    modified: SystemTime,
}

fn list_child_dirs(root: &Path) -> Result<Vec<DirEntryInfo>, ExecutionError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(root)
        .map_err(|e| ExecutionError::Step(format!("internal_data list {}: {e}", root.display())))?
    {
        let entry = entry.map_err(|e| {
            ExecutionError::Step(format!("internal_data list {}: {e}", root.display()))
        })?;
        let file_type = entry.file_type().map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data stat {}: {e}",
                entry.path().display()
            ))
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let metadata = entry.metadata().map_err(|e| {
            ExecutionError::Step(format!(
                "internal_data stat {}: {e}",
                entry.path().display()
            ))
        })?;
        let name = entry.file_name().to_string_lossy().to_string();
        entries.push(DirEntryInfo {
            name,
            path: entry.path(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }
    Ok(entries)
}

fn collect_files(
    root: &Path,
    current: &Path,
    depth: usize,
    max_depth: usize,
    limit: usize,
    out: &mut Vec<Value>,
) -> Result<(), ExecutionError> {
    if out.len() >= limit || !current.exists() || depth > max_depth {
        return Ok(());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(current).map_err(|e| {
        ExecutionError::Step(format!("internal_data list {}: {e}", current.display()))
    })? {
        entries.push(entry.map_err(|e| {
            ExecutionError::Step(format!("internal_data list {}: {e}", current.display()))
        })?);
    }
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        if out.len() >= limit {
            break;
        }
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.is_dir() {
            collect_files(root, &path, depth + 1, max_depth, limit, out)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            out.push(json!({
                "path": relative,
                "size_bytes": metadata.len(),
                "modified": metadata.modified().ok().map(system_time_json).unwrap_or(Value::Null)
            }));
        }
    }
    Ok(())
}

fn resolve_existing_child_file(
    root: &Path,
    relative_path: &str,
) -> Result<PathBuf, ExecutionError> {
    let root_canonical = root.canonicalize().map_err(|e| {
        ExecutionError::Step(format!(
            "internal_data: execution root {} is not readable: {e}",
            root.display()
        ))
    })?;
    let target = root.join(relative_path);
    let target_canonical = target.canonicalize().map_err(|e| {
        ExecutionError::Step(format!(
            "internal_data: file {} is not readable: {e}",
            target.display()
        ))
    })?;
    if !target_canonical.starts_with(&root_canonical) {
        return Err(ExecutionError::Step(format!(
            "internal_data: path `{relative_path}` escapes execution root"
        )));
    }
    if !target_canonical.is_file() {
        return Err(ExecutionError::Step(format!(
            "internal_data: path `{relative_path}` is not a file"
        )));
    }
    Ok(target_canonical)
}

fn count_child_dirs(root: &Path) -> usize {
    fs::read_dir(root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false))
        .count()
}

fn count_files_with_extension(root: &Path, extension: &str) -> usize {
    if !root.exists() {
        return 0;
    }
    let mut count = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case(extension))
                .unwrap_or(false)
            {
                count += 1;
            }
        }
    }
    count
}

/// Sources DuckDB should scan for `internal_data query_llm_calls`.
///
/// MUST consume compaction output, for the same reason as
/// `governed_llm_call_partition_files` in `analytics_api.rs`: canonical fact
/// revisions (`part-call_fact-<hash>-rN.parquet`) are retained deliberately as
/// a corruption fallback and accumulate per call, so simply listing a partition
/// directory hands DuckDB every revision ever written alongside the one
/// compacted object meant to replace them. One observed scope listed 10,716
/// files where the governed selection is 97.
///
/// Both halves are compaction-aware:
/// - canonical, via `governed_dataset_sources_in_date_range`, which returns
///   `[compacted] + uncompacted_tail` only after validating the manifest and
///   the compacted file's checksum and row count, and otherwise falls back to
///   the full raw set;
/// - legacy batch, via `parquet_maintenance::partition_sources`.
///
/// Both helpers reject non-regular files, so the per-file guard the manual walk
/// used to apply is preserved. The partition-directory guard is kept here.
fn regular_llm_call_partition_files(
    workspace_layout: &ArtifactV2Workspace,
    scope: &LlmScope,
    root: &Path,
) -> Result<Vec<PathBuf>, ExecutionError> {
    let mut files: Vec<PathBuf> =
        crate::magician_v2::analytics::llm_fact_compactor::governed_dataset_sources_in_date_range(
            workspace_layout,
            scope,
            crate::magician_v2::analytics::llm_fact_registry::LlmCanonicalDataset::Calls,
            None,
            None,
        )
        .map_err(|error| {
            ExecutionError::Step(format!(
                "internal_data query_llm_calls cannot select governed canonical sources: {error}"
            ))
        })?
        .into_iter()
        .flat_map(|source| source.files)
        .collect();

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(ExecutionError::Step(format!(
                "internal_data query_llm_calls cannot read scoped dataset: {error}"
            )))
        },
    };
    for partition in entries {
        let partition = partition.map_err(|error| {
            ExecutionError::Step(format!(
                "internal_data query_llm_calls cannot inspect a partition: {error}"
            ))
        })?;
        let name = partition.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("dt=") {
            continue;
        }
        let file_type = partition.file_type().map_err(|error| {
            ExecutionError::Step(format!(
                "internal_data query_llm_calls cannot inspect partition type: {error}"
            ))
        })?;
        if !file_type.is_dir() {
            return Err(ExecutionError::Step(format!(
                "internal_data query_llm_calls partition must be a real directory: {}",
                partition.path().display()
            )));
        }
        // Validation only — this walk no longer builds the read set, but it
        // still has to run. The governed selectors below reject non-regular
        // files only among names they recognise, so dropping this pass would
        // silently ignore a symlinked `*.parquet` planted in a partition
        // instead of failing closed on it.
        let partition_entries = fs::read_dir(partition.path()).map_err(|error| {
            ExecutionError::Step(format!(
                "internal_data query_llm_calls cannot read partition: {error}"
            ))
        })?;
        for file in partition_entries {
            let file = file.map_err(|error| {
                ExecutionError::Step(format!(
                    "internal_data query_llm_calls cannot inspect a source: {error}"
                ))
            })?;
            if file
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("parquet")
            {
                continue;
            }
            if !file
                .file_type()
                .map_err(|error| {
                    ExecutionError::Step(format!(
                        "internal_data query_llm_calls cannot inspect source type: {error}"
                    ))
                })?
                .is_file()
            {
                return Err(ExecutionError::Step(format!(
                    "internal_data query_llm_calls source must be a regular file: {}",
                    file.path().display()
                )));
            }
        }
        files.extend(
            crate::magician_v2::analytics::parquet_maintenance::partition_sources(
                &partition.path(),
                crate::magician_v2::analytics::parquet_maintenance::PartitionedDataset::LegacyLlmCalls,
            )
            .map_err(|error| {
                ExecutionError::Step(format!(
                    "internal_data query_llm_calls cannot select legacy sources: {error}"
                ))
            })?,
        );
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn count_jsonl_records(path: &Path) -> usize {
    fs::read_to_string(path)
        .map(|content| {
            content
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
        })
        .unwrap_or(0)
}

fn count_files_recursive(root: &Path, max_depth: usize) -> usize {
    fn walk(path: &Path, depth: usize, max_depth: usize, count: &mut usize) {
        if depth > max_depth {
            return;
        }
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                walk(&entry.path(), depth + 1, max_depth, count);
            } else if file_type.is_file() {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(root, 0, max_depth, &mut count);
    count
}

fn count_learning_procedure_records(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> usize {
    ["draft", "active", "deprecated", "archived"]
        .iter()
        .map(|status| {
            count_files_with_extension(
                &workspace_layout.learning_procedure_status_dir(principal, workspace, status),
                "yaml",
            )
        })
        .sum()
}

fn path_status(path: PathBuf) -> Value {
    let metadata = fs::metadata(&path).ok();
    json!({
        "path": path.display().to_string(),
        "exists": metadata.is_some(),
        "is_dir": metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false),
        "is_file": metadata.as_ref().map(|m| m.is_file()).unwrap_or(false),
        "size_bytes": metadata.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
        "modified": metadata.and_then(|m| m.modified().ok()).map(system_time_json)
    })
}

fn system_time_json(time: SystemTime) -> Value {
    let dt: DateTime<Utc> = time.into();
    Value::String(dt.to_rfc3339())
}

fn string_param(params: &HashMap<String, Value>, key: &str) -> Option<String> {
    params.get(key).and_then(|value| match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

fn required_string(params: &HashMap<String, Value>, key: &str) -> Result<String, ExecutionError> {
    string_param(params, key).ok_or_else(|| {
        ExecutionError::Step(format!("internal_data: missing required parameter `{key}`"))
    })
}

fn bounded_usize(params: &HashMap<String, Value>, key: &str, default: usize, max: usize) -> usize {
    params
        .get(key)
        .and_then(|value| match value {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.trim().parse::<u64>().ok(),
            _ => None,
        })
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
        .min(max)
}

fn optional_i64_param(
    params: &HashMap<String, Value>,
    key: &str,
) -> Result<Option<i64>, ExecutionError> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    let parsed = match value {
        Value::Number(number) => number.as_i64(),
        Value::String(value) => value.trim().parse::<i64>().ok(),
        _ => None,
    };
    parsed.map(Some).ok_or_else(|| {
        ExecutionError::Step(format!(
            "internal_data: `{key}` must be an integer when supplied"
        ))
    })
}

fn optional_usize_param(
    params: &HashMap<String, Value>,
    key: &str,
) -> Result<Option<usize>, ExecutionError> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    let parsed = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(value) => value.trim().parse::<u64>().ok(),
        _ => None,
    }
    .and_then(|value| usize::try_from(value).ok());
    parsed.map(Some).ok_or_else(|| {
        ExecutionError::Step(format!(
            "internal_data: `{key}` must be a non-negative integer when supplied"
        ))
    })
}

fn optional_bool_param(
    params: &HashMap<String, Value>,
    key: &str,
) -> Result<Option<bool>, ExecutionError> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    let parsed = match value {
        Value::Bool(value) => Some(*value),
        Value::String(value) if value.eq_ignore_ascii_case("true") => Some(true),
        Value::String(value) if value.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    };
    parsed.map(Some).ok_or_else(|| {
        ExecutionError::Step(format!(
            "internal_data: `{key}` must be a boolean when supplied"
        ))
    })
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn bounded_duckdb_result_json(value: &Value, action: &str) -> Result<String, ExecutionError> {
    bounded_duckdb_result_json_with_limit(value, action, ANALYTICS_DUCKDB_MAX_RESULT_BYTES)
}

fn bounded_duckdb_result_json_with_limit(
    value: &Value,
    action: &str,
    max_bytes: usize,
) -> Result<String, ExecutionError> {
    let serialized = serde_json::to_string_pretty(value).map_err(|error| {
        ExecutionError::Step(format!(
            "internal_data action `{action}` could not serialize its DuckDB result: {error}"
        ))
    })?;
    if serialized.len() > max_bytes {
        return Err(ExecutionError::Step(format!(
            "internal_data action `{action}` result exceeds the {max_bytes} byte output limit; narrow the request"
        )));
    }
    Ok(serialized)
}

fn escape_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}

fn escape_like_string(s: &str) -> String {
    escape_sql_string(s)
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn duckvalue_to_json(val: &DuckValue) -> Value {
    match val {
        DuckValue::Null => Value::Null,
        DuckValue::Boolean(b) => json!(b),
        DuckValue::TinyInt(n) => json!(n),
        DuckValue::SmallInt(n) => json!(n),
        DuckValue::Int(n) => json!(n),
        DuckValue::BigInt(n) => json!(n),
        DuckValue::HugeInt(n) => json!(n.to_string()),
        DuckValue::UTinyInt(n) => json!(n),
        DuckValue::USmallInt(n) => json!(n),
        DuckValue::UInt(n) => json!(n),
        DuckValue::UBigInt(n) => json!(n),
        DuckValue::Float(f) => json!(f),
        DuckValue::Double(f) => json!(f),
        DuckValue::Text(s) => json!(s),
        DuckValue::Blob(bytes) => json!(format!("<blob {} bytes>", bytes.len())),
        _ => json!(format!("{:?}", val)),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn generic_internal_event_text_withholds_owner_notifications_fail_closed() {
        let input = LimitedText {
            content: concat!(
                "{\"event_type\":\"ExecutionStarted\",\"data\":{}}\n",
                "{\"event_type\":\"HitlRequested\",\"data\":{\"input_schema\":{\"context\":{\"app_owner_notification\":true}},\"prompt\":\"private\"}}\n",
                "{\"event_type\":\"HitlRequested\",\"data\":\"damaged private prompt\"}\n",
                "{\"event_type\":\"HitlResolved\",\"data\":{\"source\":\"app_owner_notification\"}}\n",
            )
            .to_owned(),
            truncated: true,
        };

        let filtered = filter_transport_event_text(input);
        assert!(filtered.content.contains("ExecutionStarted"));
        assert!(!filtered.content.contains("private"));
        assert!(!filtered.content.contains("app_owner_notification"));
        assert!(filtered.truncated);
    }

    #[test]
    fn app_learning_read_proof_admits_only_the_closed_review_reads() {
        let list = HashMap::from([
            (
                "__action_name".to_string(),
                json!("list_learning_candidates"),
            ),
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
            ("state".to_string(), json!("proposed")),
            ("limit".to_string(), json!(25)),
        ]);
        assert!(prove_app_learning_read_args(&list));
        // Tool-qualified alias spelling agrees with the routing key.
        let mut qualified = list.clone();
        qualified.insert(
            "operation".to_string(),
            json!("internal_data__list_learning_candidates"),
        );
        assert!(prove_app_learning_read_args(&qualified));
        let read = HashMap::from([
            (
                "__action_name".to_string(),
                json!("read_learning_candidate"),
            ),
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
            ("candidate_id".to_string(), json!("lc_example")),
        ]);
        assert!(prove_app_learning_read_args(&read));

        // Any other action stays refused even with a perfect scope envelope.
        for action in ["catalog", "query_events", "list_learning_procedures"] {
            let mut other = list.clone();
            other.insert("__action_name".to_string(), json!(action));
            assert!(!prove_app_learning_read_args(&other), "{action}");
        }
        // Unknown state, oversized limit, wrong types, unexpected keys, an
        // alias naming a second operation, and a missing candidate id all
        // fail closed.
        let mut bad_state = list.clone();
        bad_state.insert("state".to_string(), json!("guessed"));
        assert!(!prove_app_learning_read_args(&bad_state));
        let mut bad_limit = list.clone();
        bad_limit.insert("limit".to_string(), json!(26));
        assert!(!prove_app_learning_read_args(&bad_limit));
        let mut string_limit = list.clone();
        string_limit.insert("limit".to_string(), json!("10"));
        assert!(!prove_app_learning_read_args(&string_limit));
        let mut extra_key = list.clone();
        extra_key.insert("candidate_type".to_string(), json!("memory_fact"));
        assert!(!prove_app_learning_read_args(&extra_key));
        let mut divergent = list.clone();
        divergent.insert("operation".to_string(), json!("catalog"));
        assert!(!prove_app_learning_read_args(&divergent));
        let mut no_id = read.clone();
        no_id.remove("candidate_id");
        assert!(!prove_app_learning_read_args(&no_id));
        // A read cannot carry the list filter, and a list cannot carry the
        // read selector.
        let mut mixed = read.clone();
        mixed.insert("state".to_string(), json!("proposed"));
        assert!(!prove_app_learning_read_args(&mixed));
    }

    #[tokio::test]
    async fn app_bound_learning_reads_return_real_store_candidates_in_scope() {
        // The population-path core leg (plan 2.5): the two admitted actions
        // read the real LearningStore through the same provider execution the
        // app attested dispatch settles into, with the executor-owned scope
        // envelope and nothing else.
        use crate::magician_v2::learning::{
            CreateLearningCandidateRequest, LearningCandidateState, LearningCandidateType,
            LearningRiskLevel,
        };
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let scope = LearningScope::new("owner", "default");
        LearningStore::new(workspace.clone())
            .create_candidate(
                scope.clone(),
                CreateLearningCandidateRequest {
                    principal: None,
                    workspace: None,
                    candidate_type: LearningCandidateType::MemoryFact,
                    state: LearningCandidateState::Proposed,
                    title: "Prefers concise replies".to_string(),
                    summary: "The owner asked for concise replies in code reviews.".to_string(),
                    rationale: "Explicit user teaching.".to_string(),
                    proposed_change: json!({"value": "concise replies"}),
                    proposed_target: None,
                    confidence: Some(0.9),
                    source_agent_id: Some("personal-assistant".to_string()),
                    source_task_id: None,
                    source_execution_id: None,
                    source_chat_session_id: None,
                    event_refs: Vec::new(),
                    evidence_refs: Vec::new(),
                    risk_level: LearningRiskLevel::Low,
                    review_required: true,
                    review_reason: None,
                    review_policy: json!({}),
                    promotion_target: None,
                    promotion_policy: json!({}),
                },
            )
            .expect("seed candidate");

        let provider = InternalDataProvider::new(workspace.clone());
        let list = ExecutableAction::Pack {
            capability_name: INTERNAL_DATA_TOOL_NAME.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: INTERNAL_DATA_TOOL_NAME.to_string(),
            },
            resolved_params: HashMap::from([
                (
                    "__action_name".to_string(),
                    json!("list_learning_candidates"),
                ),
                ("__principal".to_string(), json!("owner")),
                ("__workspace".to_string(), json!("default")),
                ("state".to_string(), json!("proposed")),
                ("limit".to_string(), json!(10)),
            ]),
        };
        assert!(prove_app_learning_read_args(&match &list {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params.clone(),
            _ => unreachable!("fixture is a pack action"),
        }));
        let ActionResult::Text { content } =
            provider.execute(&list, None, 5).await.expect("list action")
        else {
            panic!("list action returns text");
        };
        let payload: Value = serde_json::from_str(&content).expect("list result json");
        assert_eq!(payload["count"], json!(1));
        assert_eq!(
            payload["candidates"][0]["candidate_type"],
            json!("memory_fact")
        );
        assert_eq!(payload["candidates"][0]["state"], json!("proposed"));
        let candidate_id = payload["candidates"][0]["id"].as_str().unwrap().to_owned();

        let read = ExecutableAction::Pack {
            capability_name: INTERNAL_DATA_TOOL_NAME.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: INTERNAL_DATA_TOOL_NAME.to_string(),
            },
            resolved_params: HashMap::from([
                (
                    "__action_name".to_string(),
                    json!("read_learning_candidate"),
                ),
                ("__principal".to_string(), json!("owner")),
                ("__workspace".to_string(), json!("default")),
                ("candidate_id".to_string(), json!(candidate_id)),
            ]),
        };
        let ActionResult::Text { content } =
            provider.execute(&read, None, 5).await.expect("read action")
        else {
            panic!("read action returns text");
        };
        let payload: Value = serde_json::from_str(&content).expect("read result json");
        assert_eq!(payload["candidate"]["id"], json!(candidate_id));
        // The decision log travels with the detail read, which is what the
        // console's decision ledger mirrors.
        assert!(payload["decisions"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty()));
    }

    #[test]
    fn rejects_non_select_queries() {
        assert!(ensure_safe_select("SELECT * FROM logs", "test").is_ok());
        assert!(
            ensure_safe_select("-- comment\nWITH x AS (SELECT 1) SELECT * FROM x", "test").is_ok()
        );
        assert!(ensure_safe_select("DELETE FROM logs", "test").is_err());
        assert!(ensure_safe_select("/* x */ DROP TABLE events", "test").is_err());
        assert!(ensure_safe_select("SELECT 1; SELECT 2", "test").is_err());
        assert!(ensure_safe_select("SELECT * INTO copied FROM events", "test").is_err());
        assert!(ensure_safe_select("VALUES (1)", "test").is_err());
        assert!(ensure_safe_select("TABLE events", "test").is_err());
        assert!(
            ensure_safe_select("SELECT * FROM read_parquet('/tmp/x.parquet')", "test").is_err()
        );
    }

    #[test]
    fn legacy_llm_query_connection_disables_external_table_functions_after_view_install() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parquet = temp.path().join("calls.parquet");
        let conn = Connection::open_in_memory().expect("open DuckDB");
        configure_internal_analytics_connection(&conn, "legacy_llm_external_access")
            .expect("configure DuckDB");
        conn.execute_batch(&format!(
            "COPY (SELECT 1700000000000::BIGINT AS timestamp_ms,
                          'chat'::VARCHAR AS operation,
                          'private reasoning'::VARCHAR AS reasoning_summary,
                          'private error'::VARCHAR AS error)
             TO '{}' (FORMAT PARQUET);",
            escape_sql_string(&parquet.display().to_string()),
        ))
        .expect("server-owned Parquet fixture");
        install_legacy_llm_views(
            &conn,
            &format!("'{}'", escape_sql_string(&parquet.display().to_string())),
            None,
            "owner",
            "default",
            None,
        )
        .expect("server-owned compatibility views");
        disable_internal_external_access(&conn, "legacy_llm_external_access")
            .expect("disable external access");

        let (operation, error): (String, Option<String>) = conn
            .query_row("SELECT operation, error FROM llm_calls", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("installed internal view remains readable");
        assert_eq!(operation, "chat");
        assert_eq!(error.as_deref(), Some("legacy_error_redacted"));
        assert!(conn
            .prepare("SELECT reasoning_summary FROM llm_calls")
            .is_err());
        assert!(conn.prepare("SELECT * FROM llm_calls_raw").is_err());
        assert!(
            conn.prepare("SELECT * FROM glob('/tmp/*')").is_err(),
            "external table functions must fail even when the token scanner does not name them"
        );
    }

    #[test]
    fn clamps_limits() {
        let mut params = HashMap::new();
        params.insert("limit".to_string(), json!(999_999));
        assert_eq!(bounded_usize(&params, "limit", 10, 100), 100);
        params.insert("limit".to_string(), json!(0));
        assert_eq!(bounded_usize(&params, "limit", 10, 100), 10);
    }

    #[test]
    fn duckdb_query_rows_enforce_requested_row_limit() {
        let conn = Connection::open_in_memory().expect("open DuckDB");
        configure_internal_analytics_connection(&conn, "row_limit_test").expect("configure DuckDB");

        let result =
            run_duckdb_query_inner(&conn, "SELECT * FROM range(5)", 2).expect("bounded query");

        assert_eq!(result.row_count, 2);
        assert!(result.truncated);
    }

    #[test]
    fn serialized_duckdb_result_limit_fails_closed() {
        let value = json!({"rows": ["x".repeat(128)]});

        let error = bounded_duckdb_result_json_with_limit(&value, "test", 64)
            .expect_err("oversized result");

        assert!(error.to_string().contains("64 byte output limit"));
    }

    #[test]
    fn query_budget_handles_unrepresentable_instant_deadlines() {
        let budget = InternalQueryBudget::new(Duration::MAX);
        assert!(budget.remaining("test").is_ok());
    }

    #[test]
    fn runtime_scope_is_required_authoritative_and_cross_scope_overrides_fail() {
        assert!(authorize_runtime_scope(HashMap::new()).is_err());

        for unsafe_component in ["../owner", "owner/team", " owner", "owner\\team"] {
            let unsafe_scope = HashMap::from([
                ("__principal".to_string(), json!(unsafe_component)),
                ("__workspace".to_string(), json!("default")),
            ]);
            assert!(
                authorize_runtime_scope(unsafe_scope).is_err(),
                "unsafe component {unsafe_component:?} must fail closed"
            );
        }

        let mut forged = HashMap::from([
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("allowed")),
            ("principal".to_string(), json!("other")),
        ]);
        assert!(authorize_runtime_scope(forged.clone()).is_err());
        forged.insert("principal".to_string(), json!("owner"));
        forged.insert("workspace".to_string(), json!("denied"));
        assert!(authorize_runtime_scope(forged.clone()).is_err());
        forged.insert("workspace".to_string(), json!("allowed"));

        let authorized = authorize_runtime_scope(forged).expect("matching assertion");
        let scope = scope_from_params(&authorized);
        assert_eq!(scope.principal, "owner");
        assert_eq!(scope.workspace, "allowed");
        assert_eq!(authorized["principal"], json!("owner"));
        assert_eq!(authorized["workspace"], json!("allowed"));
    }

    #[cfg(unix)]
    #[test]
    fn legacy_llm_call_partition_discovery_rejects_symlinked_inputs() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::fs::write(outside.path().join("outside.parquet"), b"fixture")
            .expect("outside fixture");
        symlink(outside.path(), temp.path().join("dt=2026-07-01")).expect("symlink partition");

        // Scope points at an empty governed root, so the canonical selector
        // contributes nothing and the assertions below isolate the symlink
        // guards on the partition walk.
        let layout = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("anonymous".to_string(), "default".to_string());

        let error = regular_llm_call_partition_files(&layout, &scope, temp.path())
            .expect_err("symlinked partition must fail closed");
        assert!(error.to_string().contains("real directory"));

        std::fs::remove_file(temp.path().join("dt=2026-07-01")).expect("remove partition symlink");
        let partition = temp.path().join("dt=2026-07-01");
        std::fs::create_dir_all(&partition).expect("partition");
        symlink(
            outside.path().join("outside.parquet"),
            partition.join("linked.parquet"),
        )
        .expect("symlink parquet");

        let error = regular_llm_call_partition_files(&layout, &scope, temp.path())
            .expect_err("symlinked parquet must fail closed");
        assert!(error.to_string().contains("regular file"));
    }

    #[tokio::test]
    async fn runtime_provider_rejects_unscoped_actions_before_dispatch() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let provider = InternalDataProvider::new(ArtifactV2Workspace::new(temp_dir.path()));
        let action = ExecutableAction::Pack {
            capability_name: INTERNAL_DATA_TOOL_NAME.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: INTERNAL_DATA_TOOL_NAME.to_string(),
            },
            resolved_params: HashMap::from([("action".to_string(), json!("catalog"))]),
        };

        let error = provider
            .execute(&action, None, 1)
            .await
            .expect_err("unscoped runtime call must fail closed");

        assert!(error
            .to_string()
            .contains("requires runtime-owned scope `__principal`"));
    }

    #[test]
    fn internal_llm_overview_uses_the_shared_reader_envelope_without_formula_drift() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let service = LlmAnalyticsReadService::new(workspace.clone());
        let params = HashMap::from([
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
            ("from_ms".to_string(), json!(1_700_000_000_000_i64)),
            ("to_ms".to_string(), json!(1_700_003_600_000_i64)),
        ]);

        let provider = execute_internal_data_action_with_service(
            &workspace,
            &service,
            "llm_observability_overview",
            &params,
        )
        .expect("provider overview");
        let direct = serde_json::to_value(
            service
                .overview_envelope(
                    &LlmScope::new("owner", "default"),
                    Some(1_700_000_000_000),
                    Some(1_700_003_600_000),
                )
                .expect("direct overview"),
        )
        .expect("serialize direct overview");

        assert_eq!(provider["scope"], direct["scope"]);
        assert_eq!(provider["effective_range"], direct["effective_range"]);
        assert_eq!(provider["coverage"], direct["coverage"]);
        let mut provider_data = provider["data"].clone();
        let mut direct_data = direct["data"].clone();
        provider_data
            .as_object_mut()
            .expect("provider overview object")
            .remove("generated_at_ms");
        direct_data
            .as_object_mut()
            .expect("direct overview object")
            .remove("generated_at_ms");
        assert_eq!(provider_data, direct_data);
        assert_eq!(provider["pagination"], Value::Null);
    }

    #[test]
    fn internal_llm_fact_actions_are_typed_bounded_and_content_free() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let service = LlmAnalyticsReadService::new(workspace.clone());
        let base = HashMap::from([
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
        ]);

        let calls = execute_internal_data_action_with_service(
            &workspace,
            &service,
            "list_llm_calls",
            &base,
        )
        .expect("list calls");
        assert_eq!(calls["data"]["relation"], json!("llm_calls"));
        assert_eq!(calls["pagination"]["page_size"], json!(100));

        let mut sql = base.clone();
        sql.insert(
            "sql".to_string(),
            json!("SELECT count(*) AS calls FROM llm_calls"),
        );
        assert!(execute_internal_data_action_with_service(
            &workspace,
            &service,
            "query_llm_facts",
            &sql,
        )
        .is_ok());
        sql.insert(
            "sql".to_string(),
            json!("SELECT read_text('/tmp/private') FROM llm_calls"),
        );
        assert!(execute_internal_data_action_with_service(
            &workspace,
            &service,
            "query_llm_facts",
            &sql,
        )
        .is_err());

        let mut trace_params = base.clone();
        trace_params.insert("from_ms".to_string(), json!(1_700_000_000_000_i64));
        trace_params.insert("to_ms".to_string(), json!(1_700_003_600_000_i64));
        let traces = execute_internal_data_action_with_service(
            &workspace,
            &service,
            "list_llm_traces",
            &trace_params,
        )
        .expect("list traces");
        assert_eq!(traces["data"]["row_count"], json!(0));
        assert_eq!(traces["pagination"], Value::Null);

        trace_params.insert("trace_id".to_string(), json!("missing-trace"));
        let trace = execute_internal_data_action_with_service(
            &workspace,
            &service,
            "read_llm_trace",
            &trace_params,
        )
        .expect("read missing trace");
        assert_eq!(trace["data"], Value::Null);

        let catalog =
            execute_internal_data_action_with_service(&workspace, &service, "catalog", &base)
                .expect("catalog");
        let actions = catalog["actions"].as_array().expect("actions");
        assert!(actions.contains(&json!("list_llm_traces")));
        assert!(actions.contains(&json!("read_llm_trace")));
    }

    #[test]
    fn exposes_capability_rollback_recommendations_and_post_promotion_monitors() {
        use crate::magician_v2::learning::{
            LearningCapabilityEvolutionPostPromotionMonitorRecord,
            LearningCapabilityEvolutionPostPromotionMonitorStatus,
            LearningCapabilityEvolutionRollbackRecommendationRecord,
            LearningCapabilityEvolutionRollbackRecommendationStatus,
        };

        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let scope = LearningScope::new("anonymous".to_string(), "default".to_string());
        let store = LearningStore::new(workspace.clone());
        let now = Utc::now();

        store
            .write_capability_evolution_rollback_recommendation_record(
                &LearningCapabilityEvolutionRollbackRecommendationRecord {
                    id: "rollback_1".to_string(),
                    scope: scope.clone(),
                    candidate_id: "candidate_1".to_string(),
                    proposal_id: "proposal_1".to_string(),
                    validation_id: Some("validation_1".to_string()),
                    implementation_id: Some("implementation_1".to_string()),
                    application_id: "application_1".to_string(),
                    promotion_id: Some("promotion_1".to_string()),
                    capability_id: Some("skill:foo".to_string()),
                    status: LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended,
                    trigger_kind: "post_promotion_regression".to_string(),
                    severity: "high".to_string(),
                    actor: "tester".to_string(),
                    summary: "Rollback foo".to_string(),
                    rollback_files: vec!["skills/foo/SKILL.md".to_string()],
                    evidence_refs: Vec::new(),
                    payload: json!({}),
                    created_at: now,
                },
            )
            .expect("write rollback recommendation");
        store
            .write_capability_evolution_post_promotion_monitor_record(
                &LearningCapabilityEvolutionPostPromotionMonitorRecord {
                    id: "monitor_1".to_string(),
                    scope: scope.clone(),
                    promotion_id: "promotion_1".to_string(),
                    candidate_id: "candidate_1".to_string(),
                    proposal_id: "proposal_1".to_string(),
                    validation_id: "validation_1".to_string(),
                    implementation_id: Some("implementation_1".to_string()),
                    application_id: Some("application_1".to_string()),
                    capability_id: Some("skill:foo".to_string()),
                    status:
                        LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected,
                    summary: "Regression detected".to_string(),
                    skill_names: vec!["foo".to_string()],
                    before_invocation_count: 1,
                    after_invocation_count: 3,
                    before_success_count: 1,
                    after_success_count: 0,
                    before_failure_count: 0,
                    after_failure_count: 3,
                    before_success_rate: Some(1.0),
                    after_success_rate: Some(0.0),
                    same_failure_recurrence_count: 3,
                    new_failure_classes: vec!["tool_misuse".to_string()],
                    user_negative_feedback_count: 0,
                    rollback_recommendation_id: Some("rollback_1".to_string()),
                    follow_up_candidate_id: None,
                    evidence_refs: Vec::new(),
                    payload: json!({}),
                    created_at: now,
                    updated_at: now,
                },
            )
            .expect("write post-promotion monitor");

        let params = HashMap::new();
        let recommendations = execute_internal_data_action(
            &workspace,
            "list_learning_capability_rollback_recommendations",
            &params,
        )
        .expect("list rollback recommendations");
        assert_eq!(recommendations["count"], json!(1));
        assert_eq!(
            recommendations["rollback_recommendations"][0]["id"],
            json!("rollback_1")
        );

        let mut read_recommendation_params = HashMap::new();
        read_recommendation_params.insert("candidate_id".to_string(), json!("candidate_1"));
        read_recommendation_params.insert("recommendation_id".to_string(), json!("rollback_1"));
        let recommendation = execute_internal_data_action(
            &workspace,
            "read_learning_capability_rollback_recommendation",
            &read_recommendation_params,
        )
        .expect("read rollback recommendation");
        assert_eq!(
            recommendation["rollback_recommendation"]["application_id"],
            json!("application_1")
        );

        let monitors = execute_internal_data_action(
            &workspace,
            "list_learning_capability_post_promotion_monitors",
            &params,
        )
        .expect("list post-promotion monitors");
        assert_eq!(monitors["count"], json!(1));
        assert_eq!(
            monitors["post_promotion_monitors"][0]["promotion_id"],
            json!("promotion_1")
        );

        let mut read_monitor_params = HashMap::new();
        read_monitor_params.insert("promotion_id".to_string(), json!("promotion_1"));
        let monitor = execute_internal_data_action(
            &workspace,
            "read_learning_capability_post_promotion_monitor",
            &read_monitor_params,
        )
        .expect("read post-promotion monitor");
        assert_eq!(
            monitor["post_promotion_monitor"]["rollback_recommendation_id"],
            json!("rollback_1")
        );

        let catalog =
            execute_internal_data_action(&workspace, "catalog", &params).expect("catalog");
        assert_eq!(
            catalog["sources"]["learning_root"]
                ["capability_evolution_rollback_recommendation_count"],
            json!(1)
        );
        assert_eq!(
            catalog["sources"]["learning_root"]
                ["capability_evolution_post_promotion_monitor_count"],
            json!(1)
        );
        let actions = catalog["actions"].as_array().expect("actions");
        assert!(actions
            .iter()
            .any(|action| action.as_str()
                == Some("list_learning_capability_rollback_recommendations")));
        assert!(actions
            .iter()
            .any(|action| action.as_str()
                == Some("list_learning_capability_post_promotion_monitors")));
    }

    #[test]
    fn review_memory_effects_on_an_empty_store_recommends_collecting() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let review = execute_internal_data_action(
            &workspace,
            "review_memory_effects",
            &HashMap::from([
                ("principal".to_string(), json!("anonymous")),
                ("workspace".to_string(), json!("default")),
            ]),
        )
        .expect("review");
        assert_eq!(review["advice"], json!("collect_shadow_evidence"));
        assert_eq!(review["observation"]["mode"], json!("shadow"));
        assert_eq!(review["observation"]["judgement_count"], json!(0));
        assert!(review["next_step"].as_str().unwrap().contains("confirm"));
    }

    #[test]
    fn audio_note_actions_are_scoped_paginated_and_content_bounded() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let index = workspace
            .scope_root("anonymous", "default")
            .join("notes/audio-index");
        for (id, captured_at, transcript) in [
            (
                "11111111-1111-4111-8111-111111111111",
                "2026-08-01T10:00:00+05:30",
                "launch thought",
            ),
            (
                "22222222-2222-4222-8222-222222222222",
                "2026-08-02T10:00:00+05:30",
                "private design thought",
            ),
        ] {
            workspace
                .write_json_atomic_path_sync(
                    index.join(format!("{id}.json")),
                    &AudioNoteIndexEntry {
                        note_id: id.to_string(),
                        requested_provider: "local_markdown".to_string(),
                        provider: "local_markdown".to_string(),
                        used_fallback: false,
                        fallback_reason: None,
                        captured_at: captured_at.to_string(),
                        source_surface: "ios_voice_note".to_string(),
                        transcript: Some(transcript.to_string()),
                        duration_ms: Some(1_000),
                        mime_type: "audio/m4a".to_string(),
                        note_path: format!("Audio Notes/{id}.md"),
                        audio_path: format!("Audio Notes/{id}.m4a"),
                        bytes: 42,
                        content_hash: "blake3:test".to_string(),
                    },
                )
                .expect("write audio-note index");
        }

        let params = HashMap::from([
            ("limit".to_string(), json!(1)),
            ("offset".to_string(), json!(0)),
            ("contains".to_string(), json!("DESIGN")),
        ]);
        let listed = execute_internal_data_action(&workspace, "list_audio_notes", &params)
            .expect("list audio notes");
        assert_eq!(listed["total"], json!(1));
        assert_eq!(
            listed["items"][0]["note_id"],
            json!("22222222-2222-4222-8222-222222222222")
        );
        assert!(listed["items"][0].get("audio_absolute_path").is_none());

        let read = execute_internal_data_action(
            &workspace,
            "read_audio_note",
            &HashMap::from([(
                "note_id".to_string(),
                json!("11111111-1111-4111-8111-111111111111"),
            )]),
        )
        .expect("read audio note");
        assert_eq!(read["note"]["transcript"], json!("launch thought"));
        assert!(read["note"].get("audio_absolute_path").is_none());
    }

    #[test]
    fn audio_note_list_projection_bounds_transcript_but_single_read_remains_canonical() {
        let note = AudioNoteIndexEntry {
            note_id: "11111111-1111-4111-8111-111111111111".to_string(),
            requested_provider: "local_markdown".to_string(),
            provider: "local_markdown".to_string(),
            used_fallback: false,
            fallback_reason: None,
            captured_at: "2026-08-01T10:00:00+05:30".to_string(),
            source_surface: "ios_voice_note".to_string(),
            transcript: Some("a".repeat(MAX_AUDIO_NOTE_LIST_TRANSCRIPT_CHARS + 25)),
            duration_ms: None,
            mime_type: "audio/m4a".to_string(),
            note_path: "Audio Notes/note.md".to_string(),
            audio_path: "Audio Notes/note.m4a".to_string(),
            bytes: 42,
            content_hash: "blake3:test".to_string(),
        };

        let projected = audio_note_list_projection(note.clone());
        assert_eq!(
            projected["transcript"].as_str().unwrap().chars().count(),
            MAX_AUDIO_NOTE_LIST_TRANSCRIPT_CHARS
        );
        assert_eq!(projected["transcript_truncated"], json!(true));
        assert_eq!(
            projected["transcript_chars"],
            json!(MAX_AUDIO_NOTE_LIST_TRANSCRIPT_CHARS + 25)
        );
        assert_eq!(
            note.transcript.unwrap().chars().count(),
            MAX_AUDIO_NOTE_LIST_TRANSCRIPT_CHARS + 25
        );
    }
}
