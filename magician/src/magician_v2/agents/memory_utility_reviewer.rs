//! Post-run utility review for temperature-ranked memory.
//!
//! Deterministic feedback marks injected memories as successful/failed at the
//! run level. This reviewer adds bounded, model-judged utility labels so the
//! temperature overlay can distinguish load-bearing memory from merely present
//! context.

mod decision;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use magician_vector_index::storage_trait::{MemoryStorage, MemoryStorageError};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::magician_v2::analytics::memory_parquet::{
    emit_rows_for_storage, json_payload, MemoryAnalyticsRow,
};
use crate::magician_v2::analytics::operation_llm_telemetry::{
    OperationLlmCallAttribution, OperationLlmTelemetryContext,
};
use crate::magician_v2::llm_chunking::LogicalChunkError;
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};

use super::{
    memory_hot_projection_lane_allows_projection, memory_hot_projection_lane_allows_t0,
    memory_temperature_utility_review_was_applied, record_memory_temperature_utility_review,
    source_text_hash, upsert_memory_hot_projections, AgentMemoryService, MemoryHotProjectionUpsert,
    MemoryPromptSelectedCandidate, MemoryTemperatureUtilityLabel,
    MemoryTemperatureUtilityReviewJudgement, MemoryTemperatureUtilityReviewSummary,
    SemanticMemoryType, MEMORY_HOT_PROJECTION_POLICY_VERSION,
};

const MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION: &str = "memory_temperature_utility_review";
const MEMORY_TEMPERATURE_UTILITY_REVIEW_QUEUE_SCHEMA_VERSION: u32 = 3;
const MEMORY_TEMPERATURE_UTILITY_REVIEW_QUEUE_FILE: &str = "utility_review_queue.json";
const MEMORY_TEMPERATURE_UTILITY_REVIEW_CONTRACT: &str =
    "memory_temperature_utility_review_v1@1.0.0";
const MAX_REVIEW_CANDIDATES: usize = 12;
const MAX_GOAL_CHARS: usize = 1_200;
const MAX_OUTCOME_CHARS: usize = 320;
const MAX_FINAL_ANSWER_CHARS: usize = 3_000;
const MAX_MEMORY_TEXT_CHARS: usize = 900;
const MAX_TRACE_ITEMS: usize = 18;
const MAX_TRACE_SUMMARY_CHARS: usize = 700;
const MAX_TRACE_PREVIEW_CHARS: usize = 1_000;
const MAX_TRACE_ERROR_CHARS: usize = 700;
const MAX_REASON_CHARS: usize = 600;
const MAX_PROJECTION_TEXT_CHARS: usize = 900;
const MIN_TEXT_CHARS_FOR_USEFUL_PROJECTION: usize = 600;
const MIN_TEXT_CHARS_FOR_LOAD_BEARING_PROJECTION: usize = 220;
const MIN_CONFIDENCE_FOR_LOAD_BEARING_PROJECTION: f64 = 0.55;
const MIN_CONFIDENCE_FOR_USEFUL_PROJECTION: f64 = 0.65;
const DEFAULT_BATCH_MAINTENANCE_MIN_BATCH_SIZE: usize = 1;
const DEFAULT_BATCH_MAINTENANCE_MAX_BATCH_SIZE: usize = 2;
const MAX_REVIEW_QUEUE_ENTRIES: usize = 500;
const UTILITY_REVIEW_LEASE_SECS: i64 = 15 * 60;
const UTILITY_REVIEW_RETRY_BASE_SECS: i64 = 60;
const UTILITY_REVIEW_RETRY_MAX_SECS: i64 = 60 * 60;
const UTILITY_REVIEW_MAX_POISON_ATTEMPTS: u32 = 8;

// Each storage root is one memory scope. The outer mutex only protects lookup;
// callers hold the returned mutex for a queue read-modify-write window.
static UTILITY_REVIEW_QUEUE_LOCKS: Lazy<Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTemperatureUtilityReviewTraceItem {
    pub source: String,
    pub action_type: Option<String>,
    pub tool_name: Option<String>,
    pub succeeded: Option<bool>,
    pub duration_ms: Option<u64>,
    pub summary: String,
    pub output_preview: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTemperatureUtilityReviewInput {
    pub run_id: String,
    pub agent_id: String,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub chat_session_id: Option<String>,
    pub goal: String,
    pub outcome: String,
    pub final_answer: String,
    pub action_trace: Vec<MemoryTemperatureUtilityReviewTraceItem>,
    pub selected_candidates: Vec<MemoryPromptSelectedCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTemperatureUtilityReviewBatchInput {
    pub batch_id: String,
    pub runs: Vec<MemoryTemperatureUtilityReviewInput>,
}

#[derive(Debug, Clone)]
pub struct MemoryTemperatureUtilityBatchMaintenanceConfig {
    pub min_batch_size: usize,
    pub max_batch_size: usize,
}

impl Default for MemoryTemperatureUtilityBatchMaintenanceConfig {
    fn default() -> Self {
        Self {
            min_batch_size: DEFAULT_BATCH_MAINTENANCE_MIN_BATCH_SIZE,
            max_batch_size: DEFAULT_BATCH_MAINTENANCE_MAX_BATCH_SIZE,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct MemoryTemperatureUtilityBatchMaintenanceSummary {
    pub queued: usize,
    pub eligible: usize,
    pub reviewed_runs: usize,
    pub reviewed_memories: usize,
    pub deferred: usize,
    pub failed: usize,
    pub retrying: usize,
    pub dead: usize,
    pub oldest_pending_age_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemoryTemperatureUtilityReviewWorkState {
    #[default]
    Pending,
    InFlight,
    Retry,
    Completed,
    Dead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryTemperatureUtilityReviewQueue {
    schema_version: u32,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    entries: BTreeMap<String, MemoryTemperatureUtilityReviewQueueEntry>,
}

impl Default for MemoryTemperatureUtilityReviewQueue {
    fn default() -> Self {
        Self {
            schema_version: MEMORY_TEMPERATURE_UTILITY_REVIEW_QUEUE_SCHEMA_VERSION,
            updated_at: Utc::now(),
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryTemperatureUtilityReviewQueueEntry {
    input: MemoryTemperatureUtilityReviewInput,
    enqueued_at: DateTime<Utc>,
    #[serde(default)]
    contract: String,
    #[serde(default)]
    state: MemoryTemperatureUtilityReviewWorkState,
    #[serde(default)]
    attempts: u32,
    #[serde(default)]
    next_retry_at: Option<DateTime<Utc>>,
    #[serde(default)]
    lease_owner: Option<String>,
    #[serde(default)]
    lease_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    batch_reviewed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    batch_review_count: u32,
    #[serde(default)]
    last_batch_error: Option<String>,
}

pub fn spawn_memory_temperature_utility_review(
    memory_service: AgentMemoryService,
    _operation_llm_router: Option<Arc<OperationLlmRouter>>,
    mut input: MemoryTemperatureUtilityReviewInput,
) {
    input
        .selected_candidates
        .retain(|candidate| !candidate.requires_provider_bound_local_processing());
    if input.selected_candidates.is_empty() {
        return;
    }

    tokio::spawn(async move {
        match enqueue_memory_temperature_utility_review(&memory_service, &input).await {
            Ok(()) => {
                debug!(
                    run_id = %input.run_id,
                    selected = input.selected_candidates.len(),
                    "[MEMORY-TEMPERATURE] utility review queued for batch maintenance"
                );
            },
            Err(error) => {
                warn!(
                    run_id = %input.run_id,
                    error = %error,
                    "[MEMORY-TEMPERATURE] failed to enqueue utility review for batch maintenance"
                );
            },
        }
    });
}

pub fn spawn_memory_temperature_utility_review_batch(
    memory_service: AgentMemoryService,
    _operation_llm_router: Option<Arc<OperationLlmRouter>>,
    mut input: MemoryTemperatureUtilityReviewBatchInput,
) {
    for run in &mut input.runs {
        run.selected_candidates
            .retain(|candidate| !candidate.requires_provider_bound_local_processing());
    }
    if input
        .runs
        .iter()
        .all(|run| run.selected_candidates.is_empty())
    {
        return;
    }
    tokio::spawn(async move {
        let mut queued = 0usize;
        for run in input.runs {
            if run.selected_candidates.is_empty() {
                continue;
            }
            match enqueue_memory_temperature_utility_review(&memory_service, &run).await {
                Ok(()) => queued = queued.saturating_add(1),
                Err(error) => {
                    warn!(
                        run_id = %run.run_id,
                        error = %format_args!("{error:#}"),
                        "[MEMORY-TEMPERATURE] failed to durably enqueue batched utility review"
                    );
                },
            }
        }
        debug!(
            batch_id = %input.batch_id,
            queued,
            "[MEMORY-TEMPERATURE] utility review batch queued for incremental maintenance"
        );
    });
}

fn scoped_memory_utility_router(
    memory_service: &AgentMemoryService,
    router: Arc<OperationLlmRouter>,
) -> Arc<OperationLlmRouter> {
    memory_service
        .scoped_memory_scope()
        .map_or(router.clone(), |(principal, workspace)| {
            Arc::new(router.with_scope_context(Some(magicllm::LlmScope::new(principal, workspace))))
        })
}

pub async fn review_memory_temperature_utility(
    memory_service: AgentMemoryService,
    router: Arc<OperationLlmRouter>,
    mut input: MemoryTemperatureUtilityReviewInput,
) -> Result<MemoryTemperatureUtilityReviewSummary> {
    input
        .selected_candidates
        .retain(|candidate| !candidate.requires_provider_bound_local_processing());
    review_memory_temperature_utility_with_telemetry(memory_service, router, input, None).await
}

async fn review_memory_temperature_utility_with_telemetry(
    memory_service: AgentMemoryService,
    router: Arc<OperationLlmRouter>,
    mut input: MemoryTemperatureUtilityReviewInput,
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> Result<MemoryTemperatureUtilityReviewSummary> {
    input
        .selected_candidates
        .retain(|candidate| !candidate.requires_provider_bound_local_processing());
    let router = scoped_memory_utility_router(&memory_service, router);
    let allowed_keys = input
        .selected_candidates
        .iter()
        .map(|candidate| candidate.memory_candidate_key.clone())
        .collect::<BTreeSet<_>>();
    if allowed_keys.is_empty() {
        return Ok(MemoryTemperatureUtilityReviewSummary::default());
    }

    let system_prompt = "You are a memory utility reviewer. Judge only whether each \
injected memory helped the completed run. Do not invent new memory. Return strict JSON only.";
    let payload = review_prompt_payload(&input);
    let prompt = format!(
        "Review the injected memories against the run goal, outcome, and final answer.\n\n\
Labels:\n\
- load_bearing: the final answer or successful action depended on this memory.\n\
- useful: the memory materially helped but was not essential.\n\
- referenced: the memory was merely mentioned or loosely related.\n\
- irrelevant: the memory did not help this run.\n\
- stale: the memory is outdated for this run.\n\
- harmful: the memory was wrong, misleading, or contradicted the result.\n\
- unknown: insufficient evidence.\n\n\
Return JSON only in this shape:\n\
{{\"memories\":[{{\"memory_candidate_key\":\"...\",\"label\":\"load_bearing|useful|referenced|irrelevant|stale|harmful|unknown\",\"confidence\":0.0,\"reason\":\"short reason\",\"compact_text\":\"optional prompt-ready memory summary for useful/load_bearing items\"}}]}}\n\n\
Input:\n{}",
        serde_json::to_string_pretty(&payload)?
    );

    let mut reviewed = decision::review(
        &memory_service,
        &router,
        std::slice::from_ref(&input),
        false,
        system_prompt,
        &prompt,
        telemetry,
    )
    .await?;
    let judgements = reviewed.remove(&input.run_id).unwrap_or_default();
    if judgements.is_empty() {
        return Ok(MemoryTemperatureUtilityReviewSummary::default());
    }

    let summary = record_memory_temperature_utility_review(
        memory_service.storage(),
        &input.run_id,
        &judgements,
    )
    .await
    .context("failed to record memory utility review")?;
    let projection_upserts = build_hot_projection_upserts(&input, &judgements);
    let mut projection_upsert_count = 0usize;
    if !projection_upserts.is_empty() {
        if let Err(error) =
            upsert_memory_hot_projections(memory_service.storage(), &projection_upserts).await
        {
            warn!(
                run_id = %input.run_id,
                error = %error,
                "[MEMORY-TEMPERATURE] failed to upsert memory hot projections"
            );
        } else {
            projection_upsert_count = projection_upserts.len();
            emit_hot_projection_upsert_rows(&memory_service, &input, &projection_upserts);
        }
    }
    emit_memory_utility_review_rows(
        &memory_service,
        &input,
        &judgements,
        &summary,
        projection_upsert_count,
    );
    Ok(summary)
}

pub async fn review_memory_temperature_utility_batch(
    memory_service: AgentMemoryService,
    router: Arc<OperationLlmRouter>,
    input: MemoryTemperatureUtilityReviewBatchInput,
) -> Result<Vec<MemoryTemperatureUtilityReviewSummary>> {
    review_memory_temperature_utility_batch_with_telemetry(memory_service, router, input, None)
        .await
}

async fn review_memory_temperature_utility_batch_with_telemetry(
    memory_service: AgentMemoryService,
    router: Arc<OperationLlmRouter>,
    input: MemoryTemperatureUtilityReviewBatchInput,
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> Result<Vec<MemoryTemperatureUtilityReviewSummary>> {
    let router = scoped_memory_utility_router(&memory_service, router);
    let batch_id = input.batch_id.clone();
    let runs = input
        .runs
        .into_iter()
        .filter_map(|mut run| {
            run.selected_candidates
                .retain(|candidate| !candidate.requires_provider_bound_local_processing());
            (!run.selected_candidates.is_empty()).then_some(run)
        })
        .collect::<Vec<_>>();
    if runs.is_empty() {
        return Ok(Vec::new());
    }

    let system_prompt = "You are a memory utility reviewer. Judge only whether each \
injected memory helped each completed run. Do not invent new memory. Return strict JSON only.";
    let payload = json!({
        "batch_id": batch_id.clone(),
        "runs": runs.iter().map(review_prompt_payload).collect::<Vec<_>>(),
    });
    let prompt = format!(
        "Review the injected memories for every run. Return JSON only in this shape:\n\
{{\"runs\":[{{\"run_id\":\"...\",\"memories\":[{{\"memory_candidate_key\":\"...\",\"label\":\"load_bearing|useful|referenced|irrelevant|stale|harmful|unknown\",\"confidence\":0.0,\"reason\":\"short reason\",\"compact_text\":\"optional prompt-ready memory summary for useful/load_bearing items\"}}]}}]}}\n\n\
Input:\n{}",
        serde_json::to_string_pretty(&payload)?
    );

    let operation = LLMOperation::Other(MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION.to_string());
    let judgements_by_run = if router.logical_chunking_enabled(&operation) {
        // The adapter's identity and completeness contract is per run. Keep
        // the maintenance batch as a scheduling/storage unit while each
        // logical request independently chunks its selected candidates.
        let mut reviewed = BTreeMap::new();
        for run in &runs {
            let run_payload = review_prompt_payload(run);
            let run_prompt = format!(
                "Review the injected memories against this completed run. Return JSON only as \
{{\"memories\":[{{\"memory_candidate_key\":\"...\",\"label\":\"load_bearing|useful|referenced|irrelevant|stale|harmful|unknown\",\"confidence\":0.0,\"reason\":\"short reason\",\"compact_text\":\"optional summary\"}}]}}.\n\nInput:\n{}",
                serde_json::to_string_pretty(&run_payload)?
            );
            let mut classified = decision::review(
                &memory_service,
                &router,
                std::slice::from_ref(run),
                false,
                system_prompt,
                &run_prompt,
                telemetry,
            )
            .await?;
            let judgements = classified.remove(&run.run_id).unwrap_or_default();
            reviewed.insert(run.run_id.clone(), judgements);
        }
        reviewed
    } else {
        decision::review(
            &memory_service,
            &router,
            &runs,
            true,
            system_prompt,
            &prompt,
            telemetry,
        )
        .await?
    };

    let mut summaries = Vec::new();
    for run in runs {
        let Some(judgements) = judgements_by_run.get(&run.run_id) else {
            continue;
        };
        if judgements.is_empty() {
            continue;
        }
        let summary = record_memory_temperature_utility_review(
            memory_service.storage(),
            &run.run_id,
            judgements,
        )
        .await
        .with_context(|| format!("failed to record memory utility review for {}", run.run_id))?;
        let projection_upserts = build_hot_projection_upserts(&run, judgements);
        let mut projection_upsert_count = 0usize;
        if !projection_upserts.is_empty() {
            if let Err(error) =
                upsert_memory_hot_projections(memory_service.storage(), &projection_upserts).await
            {
                warn!(
                    run_id = %run.run_id,
                    error = %error,
                    "[MEMORY-TEMPERATURE] failed to upsert memory hot projections from batch review"
                );
            } else {
                projection_upsert_count = projection_upserts.len();
                emit_hot_projection_upsert_rows(&memory_service, &run, &projection_upserts);
            }
        }
        emit_memory_utility_review_rows(
            &memory_service,
            &run,
            judgements,
            &summary,
            projection_upsert_count,
        );
        summaries.push(summary);
    }
    Ok(summaries)
}

pub async fn run_memory_temperature_utility_batch_maintenance(
    memory_service: AgentMemoryService,
    router: Arc<OperationLlmRouter>,
    config: MemoryTemperatureUtilityBatchMaintenanceConfig,
) -> Result<MemoryTemperatureUtilityBatchMaintenanceSummary> {
    run_memory_temperature_utility_batch_maintenance_with_telemetry(
        memory_service,
        router,
        config,
        None,
    )
    .await
}

pub async fn run_memory_temperature_utility_batch_maintenance_with_telemetry(
    memory_service: AgentMemoryService,
    router: Arc<OperationLlmRouter>,
    config: MemoryTemperatureUtilityBatchMaintenanceConfig,
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> Result<MemoryTemperatureUtilityBatchMaintenanceSummary> {
    let operation = LLMOperation::Other(MEMORY_TEMPERATURE_UTILITY_REVIEW_OPERATION.to_string());
    let initial = utility_review_queue_snapshot(&memory_service).await?;
    let eligible = initial.eligible;
    let min_batch_size = config.min_batch_size.max(1);
    let max_batch_size = config.max_batch_size.max(min_batch_size);
    if eligible < min_batch_size {
        return Ok(MemoryTemperatureUtilityBatchMaintenanceSummary {
            queued: initial.active,
            eligible,
            retrying: initial.retrying,
            dead: initial.dead,
            oldest_pending_age_secs: initial.oldest_pending_age_secs,
            reviewed_runs: 0,
            reviewed_memories: 0,
            deferred: 0,
            failed: 0,
        });
    }

    let lease_owner = format!(
        "memory-utility:{}:{}",
        std::process::id(),
        Utc::now().timestamp_millis()
    );
    let mut reviewed_runs = 0usize;
    let mut reviewed_memories = 0usize;
    let mut deferred = 0usize;
    let mut failed = 0usize;

    for _ in 0..max_batch_size {
        // Durable work remains pending while foreground work or an existing
        // call for the same provider owns dispatch capacity. This prevents a
        // periodic producer from spending its logical deadline in the
        // in-memory queue and keeps foreground latency authoritative.
        if router.should_defer_background_operation(&operation) {
            deferred = utility_review_queue_snapshot(&memory_service)
                .await?
                .eligible;
            break;
        }

        let Some(claimed) = claim_next_utility_review(&memory_service, &lease_owner).await? else {
            break;
        };
        let run_id = claimed.run_id.clone();
        // The overlay write and queue lease completion are separate atomic
        // files. If the process died between them, settle the recovered lease
        // without paying for or applying the same review again.
        if memory_temperature_utility_review_was_applied(memory_service.storage(), &run_id)
            .await
            .context("failed to inspect utility-review idempotency ledger")?
        {
            anyhow::ensure!(
                complete_claimed_utility_review(&memory_service, &run_id, &lease_owner).await?,
                "replayed memory utility review completed after its durable lease was lost: {run_id}"
            );
            reviewed_runs = reviewed_runs.saturating_add(1);
            continue;
        }
        match review_memory_temperature_utility_with_telemetry(
            memory_service.clone(),
            router.clone(),
            claimed,
            telemetry,
        )
        .await
        {
            Ok(summary) => {
                anyhow::ensure!(
                    complete_claimed_utility_review(&memory_service, &run_id, &lease_owner).await?,
                    "memory utility review completed after its durable lease was lost: {run_id}"
                );
                reviewed_runs = reviewed_runs.saturating_add(1);
                reviewed_memories = reviewed_memories.saturating_add(summary.reviewed);
            },
            Err(error) => {
                let failure_class = utility_review_failure_class(&error);
                anyhow::ensure!(
                    fail_claimed_utility_review(
                        &memory_service,
                        &run_id,
                        &lease_owner,
                        failure_class,
                        &format!("{error:#}"),
                    )
                    .await?,
                    "memory utility review failed after its durable lease was lost: {run_id}"
                );
                failed = failed.saturating_add(1);
                warn!(
                    run_id = %run_id,
                    error_code = failure_class.code,
                    error = %error,
                    "memory utility maintenance item failed; durable retry scheduled"
                );
                // A provider/deadline failure is a pressure signal. Leave the
                // remaining durable items unclaimed for a later pass instead
                // of immediately adding more work behind the same bottleneck.
                break;
            },
        }
    }

    let final_snapshot = utility_review_queue_snapshot(&memory_service).await?;

    Ok(MemoryTemperatureUtilityBatchMaintenanceSummary {
        queued: final_snapshot.active,
        eligible: final_snapshot.eligible,
        reviewed_runs,
        reviewed_memories,
        deferred,
        failed,
        retrying: final_snapshot.retrying,
        dead: final_snapshot.dead,
        oldest_pending_age_secs: final_snapshot.oldest_pending_age_secs,
    })
}

fn utility_review_attribution(
    input: &MemoryTemperatureUtilityReviewInput,
) -> OperationLlmCallAttribution {
    OperationLlmCallAttribution {
        execution_id: input
            .execution_id
            .clone()
            .or_else(|| Some(input.run_id.clone())),
        task_id: input.task_id.clone(),
        agent_id: Some(input.agent_id.clone()),
        chat_session_id: input.chat_session_id.clone(),
        ..OperationLlmCallAttribution::default()
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct MemoryTemperatureUtilityQueueHealth {
    pub active: usize,
    pub eligible: usize,
    pub retrying: usize,
    pub dead: usize,
    pub oldest_pending_age_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct UtilityReviewFailureClass {
    code: &'static str,
    retryable_indefinitely: bool,
}

fn utility_review_failure_class(error: &anyhow::Error) -> UtilityReviewFailureClass {
    if let Some(error) = error
        .chain()
        .find_map(|source| source.downcast_ref::<LogicalChunkError>())
    {
        return match error {
            LogicalChunkError::DeadlineExceeded { .. } => UtilityReviewFailureClass {
                code: "logical_deadline_exceeded",
                retryable_indefinitely: true,
            },
            LogicalChunkError::Cancelled { .. } => UtilityReviewFailureClass {
                code: "cancelled",
                retryable_indefinitely: true,
            },
            LogicalChunkError::Transport { .. } => UtilityReviewFailureClass {
                code: "transport_failure",
                retryable_indefinitely: true,
            },
            LogicalChunkError::OutputInvalid { .. }
            | LogicalChunkError::RepairFailed { .. }
            | LogicalChunkError::FallbackFailed { .. }
            | LogicalChunkError::ReductionFailed { .. }
            | LogicalChunkError::FinalOutputInvalid { .. } => UtilityReviewFailureClass {
                code: "invalid_model_output",
                retryable_indefinitely: false,
            },
            LogicalChunkError::AdapterNotFound { .. }
            | LogicalChunkError::AdapterOperationMismatch { .. }
            | LogicalChunkError::FinalValidatorUnavailable { .. }
            | LogicalChunkError::Planning { .. }
            | LogicalChunkError::Adapter { .. } => UtilityReviewFailureClass {
                code: "logical_chunk_configuration",
                retryable_indefinitely: false,
            },
        };
    }

    if let Some(error) = error
        .chain()
        .find_map(|source| source.downcast_ref::<magicllm::LLMError>())
    {
        return utility_review_llm_failure_class(error);
    }

    if let Some(error) = error
        .chain()
        .find_map(|source| source.downcast_ref::<MemoryStorageError>())
    {
        return match error {
            MemoryStorageError::Io(_) | MemoryStorageError::FileLockTimeout { .. } => {
                UtilityReviewFailureClass {
                    code: "memory_storage_busy",
                    retryable_indefinitely: true,
                }
            },
            MemoryStorageError::InvalidIdentifier(_)
            | MemoryStorageError::PathOutsideRoot { .. }
            | MemoryStorageError::MissingGoalId { .. }
            | MemoryStorageError::Json(_)
            | MemoryStorageError::Other(_) => UtilityReviewFailureClass {
                code: "memory_storage_invalid",
                retryable_indefinitely: false,
            },
        };
    }

    UtilityReviewFailureClass {
        code: "unknown_failure",
        retryable_indefinitely: false,
    }
}

fn utility_review_llm_failure_class(error: &magicllm::LLMError) -> UtilityReviewFailureClass {
    match error {
        magicllm::LLMError::Routed { source, .. } => {
            utility_review_llm_failure_class(source.as_ref())
        },
        magicllm::LLMError::Timeout
        | magicllm::LLMError::DeadlineExceeded
        | magicllm::LLMError::WorkerWatchdog { .. } => UtilityReviewFailureClass {
            code: "provider_deadline_exceeded",
            retryable_indefinitely: true,
        },
        magicllm::LLMError::ProviderStatus { status, .. } => UtilityReviewFailureClass {
            code: if *status == 429 {
                "provider_rate_limited"
            } else if (500..=599).contains(status) {
                "provider_server_error"
            } else {
                "provider_client_error"
            },
            retryable_indefinitely: *status == 429 || (500..=599).contains(status),
        },
        magicllm::LLMError::QueueFull { .. } | magicllm::LLMError::QueueBytesFull { .. } => {
            UtilityReviewFailureClass {
                code: "dispatch_queue_full",
                retryable_indefinitely: true,
            }
        },
        magicllm::LLMError::Provider { .. }
        | magicllm::LLMError::Transport(_)
        | magicllm::LLMError::RateLimited { .. }
        | magicllm::LLMError::AllRetriesExhausted { .. }
        | magicllm::LLMError::ProviderUnavailable => UtilityReviewFailureClass {
            code: "provider_unavailable",
            retryable_indefinitely: true,
        },
        magicllm::LLMError::Cancelled { .. } => UtilityReviewFailureClass {
            code: "cancelled",
            retryable_indefinitely: true,
        },
        magicllm::LLMError::Configuration(_)
        | magicllm::LLMError::UnsupportedCapability(_)
        | magicllm::LLMError::Validation(_)
        | magicllm::LLMError::Serialization(_)
        | magicllm::LLMError::Context(_)
        | magicllm::LLMError::RequestTooLarge { .. }
        | magicllm::LLMError::Other(_) => UtilityReviewFailureClass {
            code: "non_retryable_request",
            retryable_indefinitely: false,
        },
    }
}

fn utility_review_entry_is_due(
    entry: &MemoryTemperatureUtilityReviewQueueEntry,
    now: DateTime<Utc>,
) -> bool {
    match entry.state {
        MemoryTemperatureUtilityReviewWorkState::Pending => true,
        MemoryTemperatureUtilityReviewWorkState::Retry => {
            entry.next_retry_at.is_none_or(|retry_at| retry_at <= now)
        },
        MemoryTemperatureUtilityReviewWorkState::InFlight
        | MemoryTemperatureUtilityReviewWorkState::Completed
        | MemoryTemperatureUtilityReviewWorkState::Dead => false,
    }
}

fn normalize_utility_review_queue(
    queue: &mut MemoryTemperatureUtilityReviewQueue,
    now: DateTime<Utc>,
) {
    let legacy = queue.schema_version < MEMORY_TEMPERATURE_UTILITY_REVIEW_QUEUE_SCHEMA_VERSION;
    for entry in queue.entries.values_mut() {
        if legacy {
            entry.state = if entry.batch_review_count > 0 {
                MemoryTemperatureUtilityReviewWorkState::Completed
            } else {
                MemoryTemperatureUtilityReviewWorkState::Pending
            };
        }
        if entry.state == MemoryTemperatureUtilityReviewWorkState::InFlight
            && entry
                .lease_expires_at
                .is_none_or(|expires_at| expires_at <= now)
        {
            entry.state = MemoryTemperatureUtilityReviewWorkState::Retry;
            entry.next_retry_at = Some(now);
            entry.lease_owner = None;
            entry.lease_expires_at = None;
        }
        if entry.state == MemoryTemperatureUtilityReviewWorkState::Completed
            && entry.batch_review_count == 0
        {
            entry.batch_review_count = 1;
        }
        if entry.contract != MEMORY_TEMPERATURE_UTILITY_REVIEW_CONTRACT {
            // A completed primary write stays complete. Every other state is
            // safe to retry under the new reviewer contract, including poison
            // output that was dead-lettered before its schema/prompt repair.
            if entry.state != MemoryTemperatureUtilityReviewWorkState::Completed {
                entry.state = MemoryTemperatureUtilityReviewWorkState::Pending;
                entry.attempts = 0;
                entry.next_retry_at = None;
                entry.lease_owner = None;
                entry.lease_expires_at = None;
                entry.last_batch_error = None;
            }
            entry.contract = MEMORY_TEMPERATURE_UTILITY_REVIEW_CONTRACT.to_string();
        }
    }
    queue.schema_version = MEMORY_TEMPERATURE_UTILITY_REVIEW_QUEUE_SCHEMA_VERSION;
}

async fn utility_review_queue_snapshot(
    memory_service: &AgentMemoryService,
) -> Result<MemoryTemperatureUtilityQueueHealth> {
    let queue_lock = utility_review_queue_lock(memory_service).await;
    let _queue_guard = queue_lock.lock().await;
    let mut queue = load_utility_review_queue(memory_service).await?;
    // The loader performs schema/lease normalization with its own current
    // timestamp. Capture the transaction time afterwards so a lease reclaimed
    // there cannot receive a `next_retry_at` a few microseconds in our future.
    let now = Utc::now();
    normalize_utility_review_queue(&mut queue, now);
    let mut snapshot = MemoryTemperatureUtilityQueueHealth::default();
    let mut oldest_enqueued_at = None;
    for entry in queue.entries.values() {
        match entry.state {
            MemoryTemperatureUtilityReviewWorkState::Pending
            | MemoryTemperatureUtilityReviewWorkState::InFlight
            | MemoryTemperatureUtilityReviewWorkState::Retry => {
                snapshot.active = snapshot.active.saturating_add(1);
                oldest_enqueued_at = Some(
                    oldest_enqueued_at.map_or(entry.enqueued_at, |oldest: DateTime<Utc>| {
                        oldest.min(entry.enqueued_at)
                    }),
                );
            },
            MemoryTemperatureUtilityReviewWorkState::Completed => {},
            MemoryTemperatureUtilityReviewWorkState::Dead => {
                snapshot.dead = snapshot.dead.saturating_add(1);
            },
        }
        if entry.state == MemoryTemperatureUtilityReviewWorkState::Retry {
            snapshot.retrying = snapshot.retrying.saturating_add(1);
        }
        if utility_review_entry_is_due(entry, now) {
            snapshot.eligible = snapshot.eligible.saturating_add(1);
        }
    }
    snapshot.oldest_pending_age_secs = oldest_enqueued_at.and_then(|enqueued_at| {
        u64::try_from(now.signed_duration_since(enqueued_at).num_seconds().max(0)).ok()
    });
    Ok(snapshot)
}

pub async fn memory_temperature_utility_queue_health(
    memory_service: &AgentMemoryService,
) -> Result<MemoryTemperatureUtilityQueueHealth> {
    utility_review_queue_snapshot(memory_service).await
}

async fn claim_next_utility_review(
    memory_service: &AgentMemoryService,
    lease_owner: &str,
) -> Result<Option<MemoryTemperatureUtilityReviewInput>> {
    let queue_lock = utility_review_queue_lock(memory_service).await;
    let _queue_guard = queue_lock.lock().await;
    let mut queue = load_utility_review_queue(memory_service).await?;
    // See `utility_review_queue_snapshot`: the transaction clock must not
    // precede normalization performed while loading the durable queue.
    let now = Utc::now();
    normalize_utility_review_queue(&mut queue, now);
    let next_run_id = queue
        .entries
        .iter()
        .filter(|(_, entry)| utility_review_entry_is_due(entry, now))
        .min_by(|(left_id, left), (right_id, right)| {
            left.enqueued_at
                .cmp(&right.enqueued_at)
                .then_with(|| left_id.cmp(right_id))
        })
        .map(|(run_id, _)| run_id.clone());
    let Some(run_id) = next_run_id else {
        return Ok(None);
    };
    let entry = queue
        .entries
        .get_mut(&run_id)
        .expect("claimed utility review came from the same queue snapshot");
    entry.state = MemoryTemperatureUtilityReviewWorkState::InFlight;
    entry.lease_owner = Some(lease_owner.to_string());
    entry.lease_expires_at = Some(now + chrono::Duration::seconds(UTILITY_REVIEW_LEASE_SECS));
    entry.last_attempt_at = Some(now);
    entry.next_retry_at = None;
    let input = entry.input.clone();
    queue.updated_at = now;
    save_utility_review_queue(memory_service, &queue).await?;
    Ok(Some(input))
}

async fn complete_claimed_utility_review(
    memory_service: &AgentMemoryService,
    run_id: &str,
    lease_owner: &str,
) -> Result<bool> {
    let queue_lock = utility_review_queue_lock(memory_service).await;
    let _queue_guard = queue_lock.lock().await;
    let mut queue = load_utility_review_queue(memory_service).await?;
    let now = Utc::now();
    normalize_utility_review_queue(&mut queue, now);
    let Some(entry) = queue.entries.get_mut(run_id) else {
        return Ok(false);
    };
    if entry.state != MemoryTemperatureUtilityReviewWorkState::InFlight
        || entry.lease_owner.as_deref() != Some(lease_owner)
    {
        return Ok(false);
    }
    entry.state = MemoryTemperatureUtilityReviewWorkState::Completed;
    entry.batch_review_count = entry.batch_review_count.saturating_add(1);
    entry.batch_reviewed_at = Some(now);
    entry.last_batch_error = None;
    entry.next_retry_at = None;
    entry.lease_owner = None;
    entry.lease_expires_at = None;
    prune_utility_review_queue(&mut queue, now);
    queue.updated_at = now;
    save_utility_review_queue(memory_service, &queue).await?;
    Ok(true)
}

async fn fail_claimed_utility_review(
    memory_service: &AgentMemoryService,
    run_id: &str,
    lease_owner: &str,
    failure_class: UtilityReviewFailureClass,
    error_message: &str,
) -> Result<bool> {
    let queue_lock = utility_review_queue_lock(memory_service).await;
    let _queue_guard = queue_lock.lock().await;
    let mut queue = load_utility_review_queue(memory_service).await?;
    let now = Utc::now();
    normalize_utility_review_queue(&mut queue, now);
    let Some(entry) = queue.entries.get_mut(run_id) else {
        return Ok(false);
    };
    if entry.state != MemoryTemperatureUtilityReviewWorkState::InFlight
        || entry.lease_owner.as_deref() != Some(lease_owner)
    {
        return Ok(false);
    }
    entry.attempts = entry.attempts.saturating_add(1);
    let dead = !failure_class.retryable_indefinitely
        && entry.attempts >= UTILITY_REVIEW_MAX_POISON_ATTEMPTS;
    entry.state = if dead {
        MemoryTemperatureUtilityReviewWorkState::Dead
    } else {
        MemoryTemperatureUtilityReviewWorkState::Retry
    };
    entry.next_retry_at = if dead {
        None
    } else {
        let exponent = entry.attempts.saturating_sub(1).min(6);
        let delay_secs = UTILITY_REVIEW_RETRY_BASE_SECS
            .saturating_mul(1_i64 << exponent)
            .min(UTILITY_REVIEW_RETRY_MAX_SECS);
        Some(now + chrono::Duration::seconds(delay_secs))
    };
    entry.last_batch_error = Some(truncate_chars(
        &format!("{}: {error_message}", failure_class.code),
        MAX_REASON_CHARS,
    ));
    entry.lease_owner = None;
    entry.lease_expires_at = None;
    queue.updated_at = now;
    save_utility_review_queue(memory_service, &queue).await?;
    Ok(true)
}

pub async fn enqueue_memory_temperature_utility_review(
    memory_service: &AgentMemoryService,
    input: &MemoryTemperatureUtilityReviewInput,
) -> Result<()> {
    let bounded_input = bounded_review_input_for_queue(input);
    if bounded_input.selected_candidates.is_empty() {
        return Ok(());
    }
    let queue_lock = utility_review_queue_lock(memory_service).await;
    let _queue_guard = queue_lock.lock().await;
    let mut queue = load_utility_review_queue(memory_service).await?;
    let now = Utc::now();
    queue
        .entries
        .entry(input.run_id.clone())
        .and_modify(|entry| {
            // A duplicate completion event may refresh evidence while the
            // item is waiting, but it must not steal an active lease or
            // resurrect an already committed review. In particular, an
            // identical duplicate must not clear a retry delay and create a
            // tight provider-failure loop.
            let evidence_changed = entry.input != bounded_input;
            if !matches!(
                entry.state,
                MemoryTemperatureUtilityReviewWorkState::Completed
                    | MemoryTemperatureUtilityReviewWorkState::InFlight
            ) && evidence_changed
            {
                entry.input = bounded_input.clone();
                entry.state = MemoryTemperatureUtilityReviewWorkState::Pending;
                entry.attempts = 0;
                entry.next_retry_at = None;
                entry.last_batch_error = None;
            }
        })
        .or_insert_with(|| MemoryTemperatureUtilityReviewQueueEntry {
            input: bounded_input.clone(),
            enqueued_at: now,
            contract: MEMORY_TEMPERATURE_UTILITY_REVIEW_CONTRACT.to_string(),
            state: MemoryTemperatureUtilityReviewWorkState::Pending,
            attempts: 0,
            next_retry_at: None,
            lease_owner: None,
            lease_expires_at: None,
            last_attempt_at: None,
            batch_reviewed_at: None,
            batch_review_count: 0,
            last_batch_error: None,
        });
    prune_utility_review_queue(&mut queue, now);
    queue.updated_at = now;
    save_utility_review_queue(memory_service, &queue).await
}

async fn utility_review_queue_lock(memory_service: &AgentMemoryService) -> Arc<Mutex<()>> {
    let scope_root = memory_service.storage().root().to_path_buf();
    let mut locks = UTILITY_REVIEW_QUEUE_LOCKS.lock().await;
    locks
        .entry(scope_root)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

async fn load_utility_review_queue(
    memory_service: &AgentMemoryService,
) -> Result<MemoryTemperatureUtilityReviewQueue> {
    let path = memory_service
        .storage()
        .root()
        .join("index")
        .join(MEMORY_TEMPERATURE_UTILITY_REVIEW_QUEUE_FILE);
    let value = match memory_service.storage().read_json_value(&path).await {
        Ok(value) => value,
        Err(MemoryStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MemoryTemperatureUtilityReviewQueue::default());
        },
        Err(error) => return Err(error).context("failed to read memory utility review queue"),
    };
    let mut queue = serde_json::from_value::<MemoryTemperatureUtilityReviewQueue>(value)
        .context("failed to parse memory utility review queue")?;
    normalize_utility_review_queue(&mut queue, Utc::now());
    Ok(queue)
}

async fn save_utility_review_queue(
    memory_service: &AgentMemoryService,
    queue: &MemoryTemperatureUtilityReviewQueue,
) -> Result<()> {
    let path = memory_service
        .storage()
        .root()
        .join("index")
        .join(MEMORY_TEMPERATURE_UTILITY_REVIEW_QUEUE_FILE);
    let value = serde_json::to_value(queue).context("failed to serialize utility review queue")?;
    memory_service
        .storage()
        .write_json_value_atomic(&path, &value)
        .await
        .context("failed to save utility review queue")
}

fn prune_utility_review_queue(queue: &mut MemoryTemperatureUtilityReviewQueue, now: DateTime<Utc>) {
    if queue.entries.len() <= MAX_REVIEW_QUEUE_ENTRIES {
        return;
    }
    let mut terminal = queue
        .entries
        .iter()
        .filter(|(_, entry)| {
            matches!(
                entry.state,
                MemoryTemperatureUtilityReviewWorkState::Completed
                    | MemoryTemperatureUtilityReviewWorkState::Dead
            )
        })
        .map(|(run_id, entry)| {
            (
                run_id.clone(),
                entry.batch_reviewed_at.unwrap_or(entry.enqueued_at),
            )
        })
        .collect::<Vec<_>>();
    terminal.sort_by(|left, right| left.1.cmp(&right.1));
    for (run_id, _) in terminal {
        if queue.entries.len() <= MAX_REVIEW_QUEUE_ENTRIES {
            break;
        }
        queue.entries.remove(&run_id);
    }
    // The cap is deliberately soft for live work. Dropping pending or leased
    // entries to enforce a storage limit would silently lose memory feedback;
    // terminal rows are the only safe pruning candidates.
    if queue.entries.len() > MAX_REVIEW_QUEUE_ENTRIES {
        warn!(
            entries = queue.entries.len(),
            soft_cap = MAX_REVIEW_QUEUE_ENTRIES,
            "memory utility durable queue exceeds soft cap; preserving live work"
        );
    }
    queue.updated_at = now;
}

fn bounded_review_input_for_queue(
    input: &MemoryTemperatureUtilityReviewInput,
) -> MemoryTemperatureUtilityReviewInput {
    MemoryTemperatureUtilityReviewInput {
        run_id: input.run_id.clone(),
        agent_id: input.agent_id.clone(),
        task_id: input.task_id.clone(),
        execution_id: input.execution_id.clone(),
        chat_session_id: input.chat_session_id.clone(),
        goal: truncate_chars(&input.goal, MAX_GOAL_CHARS),
        outcome: truncate_chars(&input.outcome, MAX_OUTCOME_CHARS),
        final_answer: truncate_chars(&input.final_answer, MAX_FINAL_ANSWER_CHARS),
        action_trace: input
            .action_trace
            .iter()
            .take(MAX_TRACE_ITEMS)
            .map(|trace| MemoryTemperatureUtilityReviewTraceItem {
                source: truncate_chars(&trace.source, MAX_TRACE_SUMMARY_CHARS),
                action_type: trace.action_type.clone(),
                tool_name: trace.tool_name.clone(),
                succeeded: trace.succeeded,
                duration_ms: trace.duration_ms,
                summary: truncate_chars(&trace.summary, MAX_TRACE_SUMMARY_CHARS),
                output_preview: trace
                    .output_preview
                    .as_deref()
                    .map(|value| truncate_chars(value, MAX_TRACE_PREVIEW_CHARS)),
                error: trace
                    .error
                    .as_deref()
                    .map(|value| truncate_chars(value, MAX_TRACE_ERROR_CHARS)),
            })
            .collect(),
        selected_candidates: input
            .selected_candidates
            .iter()
            .filter(|candidate| !candidate.requires_provider_bound_local_processing())
            .take(MAX_REVIEW_CANDIDATES)
            .map(|candidate| MemoryPromptSelectedCandidate {
                memory_candidate_key: candidate.memory_candidate_key.clone(),
                semantic_memory_type: candidate.semantic_memory_type,
                temperature_tier: candidate.temperature_tier,
                tier_name: candidate.tier_name.clone(),
                source_key: candidate.source_key.clone(),
                source_ids: candidate.source_ids.clone(),
                source_text_hash: candidate.source_text_hash.clone(),
                source_text: truncate_chars(&candidate.source_text, MAX_MEMORY_TEXT_CHARS),
                text: truncate_chars(&candidate.text, MAX_MEMORY_TEXT_CHARS),
                projection_used: candidate.projection_used,
                app_model_processing: candidate.app_model_processing,
            })
            .collect(),
    }
}

pub fn review_prompt_payload(input: &MemoryTemperatureUtilityReviewInput) -> Value {
    review_prompt_payload_for_candidates(input, &input.selected_candidates)
}

pub fn review_prompt_payload_for_candidates(
    input: &MemoryTemperatureUtilityReviewInput,
    selected_candidates: &[MemoryPromptSelectedCandidate],
) -> Value {
    json!({
        "run": {
            "run_id": input.run_id.clone(),
            "agent_id": input.agent_id.clone(),
            "task_id": input.task_id.clone(),
            "execution_id": input.execution_id.clone(),
            "chat_session_id": input.chat_session_id.clone(),
            "goal": truncate_chars(&input.goal, MAX_GOAL_CHARS),
            "outcome": truncate_chars(&input.outcome, MAX_OUTCOME_CHARS),
            "final_answer": truncate_chars(&input.final_answer, MAX_FINAL_ANSWER_CHARS),
        },
        "action_trace": input
            .action_trace
            .iter()
            .take(MAX_TRACE_ITEMS)
            .map(|trace| {
                json!({
                    "source": truncate_chars(&trace.source, MAX_TRACE_SUMMARY_CHARS),
                    "action_type": trace.action_type.clone(),
                    "tool_name": trace.tool_name.clone(),
                    "succeeded": trace.succeeded,
                    "duration_ms": trace.duration_ms,
                    "summary": truncate_chars(&trace.summary, MAX_TRACE_SUMMARY_CHARS),
                    "output_preview": trace.output_preview.as_deref().map(|value| truncate_chars(value, MAX_TRACE_PREVIEW_CHARS)),
                    "error": trace.error.as_deref().map(|value| truncate_chars(value, MAX_TRACE_ERROR_CHARS)),
                })
            })
            .collect::<Vec<_>>(),
        "injected_memories": selected_candidates
            .iter()
            .filter(|candidate| !candidate.requires_provider_bound_local_processing())
            .take(MAX_REVIEW_CANDIDATES)
            .map(|candidate| {
                json!({
                    "memory_candidate_key": candidate.memory_candidate_key.clone(),
                    "semantic_memory_type": candidate.semantic_memory_type.as_str(),
                    "temperature_tier": candidate.temperature_tier.as_str(),
                    "tier_name": candidate.tier_name.clone(),
                    "source_key": candidate.source_key.clone(),
                    "text": truncate_chars(&candidate.text, MAX_MEMORY_TEXT_CHARS),
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn emit_memory_utility_review_rows(
    memory_service: &AgentMemoryService,
    input: &MemoryTemperatureUtilityReviewInput,
    judgements: &[MemoryTemperatureUtilityReviewJudgement],
    summary: &MemoryTemperatureUtilityReviewSummary,
    projection_upsert_count: usize,
) {
    let candidates_by_key = input
        .selected_candidates
        .iter()
        .map(|candidate| (candidate.memory_candidate_key.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::with_capacity(judgements.len() + 1);
    let mut aggregate = MemoryAnalyticsRow::now(
        "memory_temperature_utility_review",
        "memory_utility_reviewer",
    );
    aggregate.agent_id = Some(input.agent_id.clone());
    aggregate.goal_id = input.task_id.clone();
    aggregate.scope = input
        .chat_session_id
        .as_ref()
        .map(|_| "chat".to_string())
        .or_else(|| input.execution_id.as_ref().map(|_| "agentic".to_string()));
    aggregate.candidate_count = Some(input.selected_candidates.len() as u32);
    aggregate.selected_count = Some(summary.reviewed as u32);
    aggregate.output_count = Some(projection_upsert_count as u32);
    aggregate.input_count = Some(summary.maintenance.promoted as u32);
    aggregate.skipped_count = Some(summary.maintenance.demoted as u32);
    aggregate.retrieval_backend = Some("memory_temperature_utility_review".to_string());
    aggregate.selected_item_keys = Some(
        judgements
            .iter()
            .map(|judgement| judgement.memory_candidate_key.as_str())
            .collect::<Vec<_>>()
            .join(","),
    );
    aggregate.status = "ok".to_string();
    aggregate.payload_json = json_payload(&json!({
        "run_id": input.run_id.clone(),
        "task_id": input.task_id.clone(),
        "execution_id": input.execution_id.clone(),
        "chat_session_id": input.chat_session_id.clone(),
        "outcome": input.outcome.clone(),
        "trace_count": input.action_trace.len(),
        "reviewed": summary.reviewed,
        "load_bearing": summary.load_bearing,
        "useful": summary.useful,
        "irrelevant": summary.irrelevant,
        "stale": summary.stale,
        "harmful": summary.harmful,
        "promoted": summary.maintenance.promoted,
        "demoted": summary.maintenance.demoted,
        "temperature_changed": summary.maintenance.changed,
        "projection_upsert_count": projection_upsert_count,
    }));
    rows.push(aggregate);

    for judgement in judgements {
        let candidate = candidates_by_key
            .get(judgement.memory_candidate_key.as_str())
            .copied();
        let mut row = MemoryAnalyticsRow::now(
            "memory_temperature_utility_judgement",
            "memory_utility_reviewer",
        );
        row.agent_id = Some(input.agent_id.clone());
        row.goal_id = input.task_id.clone();
        row.scope = candidate.map(|candidate| candidate.semantic_memory_type.as_str().to_string());
        row.tier_name = candidate.map(|candidate| candidate.tier_name.clone());
        row.item_key = Some(judgement.memory_candidate_key.clone());
        row.selected = Some(matches!(
            judgement.label,
            MemoryTemperatureUtilityLabel::Referenced
                | MemoryTemperatureUtilityLabel::Useful
                | MemoryTemperatureUtilityLabel::LoadBearing
        ));
        row.confidence = judgement.confidence;
        row.target = Some(judgement.label.as_str().to_string());
        row.status = judgement.label.as_str().to_string();
        row.payload_json = json_payload(&json!({
            "run_id": input.run_id.clone(),
            "task_id": input.task_id.clone(),
            "execution_id": input.execution_id.clone(),
            "chat_session_id": input.chat_session_id.clone(),
            "label": judgement.label.as_str(),
            "reason": judgement.reason.clone(),
            "compact_text_present": judgement.compact_text.as_ref().is_some_and(|text| !text.trim().is_empty()),
            "projection_used_in_prompt": candidate.map(|candidate| candidate.projection_used).unwrap_or(false),
            "source_key": candidate.map(|candidate| candidate.source_key.clone()),
        }));
        rows.push(row);
    }

    emit_rows_for_storage(memory_service.storage(), rows);
}

fn emit_hot_projection_upsert_rows(
    memory_service: &AgentMemoryService,
    input: &MemoryTemperatureUtilityReviewInput,
    upserts: &[MemoryHotProjectionUpsert],
) {
    let rows = upserts
        .iter()
        .map(|upsert| {
            let mut row = MemoryAnalyticsRow::now(
                "memory_hot_projection_upserted",
                "memory_utility_reviewer",
            );
            row.agent_id = Some(input.agent_id.clone());
            row.goal_id = input.task_id.clone();
            row.scope = Some(upsert.semantic_memory_type.as_str().to_string());
            row.tier_name = Some(upsert.review_label.as_str().to_string());
            row.item_key = Some(upsert.source_memory_candidate_key.clone());
            row.confidence = upsert.reviewer_confidence;
            row.output_chars = Some(upsert.compact_text.chars().count() as u32);
            row.target = Some(upsert.review_label.as_str().to_string());
            row.status = "ok".to_string();
            row.payload_json = json_payload(&json!({
                "run_id": input.run_id.clone(),
                "task_id": input.task_id.clone(),
                "execution_id": input.execution_id.clone(),
                "chat_session_id": input.chat_session_id.clone(),
                "source_memory_candidate_key": upsert.source_memory_candidate_key.clone(),
                "semantic_memory_type": upsert.semantic_memory_type.as_str(),
                "source_text_hash": upsert.source_text_hash.clone(),
                "source_ids": upsert.source_ids.clone(),
                "source_tier_name": upsert.source_tier_name.clone(),
                "source_item_key": upsert.source_item_key.clone(),
                "projection_policy_version": MEMORY_HOT_PROJECTION_POLICY_VERSION,
                "review_label": upsert.review_label.as_str(),
                "promotion_reason": upsert.promotion_reason.clone(),
            }));
            row
        })
        .collect::<Vec<_>>();
    emit_rows_for_storage(memory_service.storage(), rows);
}

fn build_hot_projection_upserts(
    input: &MemoryTemperatureUtilityReviewInput,
    judgements: &[MemoryTemperatureUtilityReviewJudgement],
) -> Vec<MemoryHotProjectionUpsert> {
    let candidates_by_key = input
        .selected_candidates
        .iter()
        .map(|candidate| (candidate.memory_candidate_key.as_str(), candidate))
        .collect::<std::collections::BTreeMap<_, _>>();
    judgements
        .iter()
        .filter_map(|judgement| {
            let candidate = candidates_by_key.get(judgement.memory_candidate_key.as_str())?;
            if !should_create_hot_projection(candidate, judgement) {
                return None;
            }
            let compact_text = judgement
                .compact_text
                .as_deref()
                .map(|text| truncate_chars(text, MAX_PROJECTION_TEXT_CHARS))
                .filter(|text| projection_text_is_useful(text))
                .unwrap_or_else(|| deterministic_projection_text(candidate));
            if !projection_text_is_useful(&compact_text) {
                return None;
            }
            Some(MemoryHotProjectionUpsert {
                source_memory_candidate_key: candidate.memory_candidate_key.clone(),
                semantic_memory_type: candidate.semantic_memory_type,
                source_text_hash: if candidate.source_text_hash.trim().is_empty() {
                    source_text_hash(&candidate.source_text)
                } else {
                    candidate.source_text_hash.clone()
                },
                compact_text,
                source_ids: projection_source_ids(candidate),
                source_tier_name: Some(candidate.tier_name.clone()),
                source_item_key: Some(candidate.source_key.clone()),
                review_run_id: Some(input.run_id.clone()),
                review_label: judgement.label,
                reviewer_confidence: judgement.confidence,
                promotion_reason: judgement.reason.clone(),
            })
        })
        .collect()
}

fn should_create_hot_projection(
    candidate: &MemoryPromptSelectedCandidate,
    judgement: &MemoryTemperatureUtilityReviewJudgement,
) -> bool {
    if !memory_hot_projection_lane_allows_projection(candidate.semantic_memory_type) {
        return false;
    }
    let source_chars = candidate.source_text.chars().count();
    let confidence = judgement.confidence.unwrap_or(0.75);
    match judgement.label {
        MemoryTemperatureUtilityLabel::LoadBearing => {
            confidence >= MIN_CONFIDENCE_FOR_LOAD_BEARING_PROJECTION
                && (memory_hot_projection_lane_allows_t0(candidate.semantic_memory_type)
                    || source_chars >= MIN_TEXT_CHARS_FOR_LOAD_BEARING_PROJECTION
                    || projection_lane_prefers_compaction(candidate.semantic_memory_type))
        },
        MemoryTemperatureUtilityLabel::Useful => {
            confidence >= MIN_CONFIDENCE_FOR_USEFUL_PROJECTION
                && (source_chars >= MIN_TEXT_CHARS_FOR_USEFUL_PROJECTION
                    || projection_lane_prefers_compaction(candidate.semantic_memory_type))
        },
        _ => false,
    }
}

fn projection_lane_prefers_compaction(lane: SemanticMemoryType) -> bool {
    matches!(
        lane,
        SemanticMemoryType::Episode
            | SemanticMemoryType::ProjectContext
            | SemanticMemoryType::Procedure
            | SemanticMemoryType::Entity
    )
}

fn projection_source_ids(candidate: &MemoryPromptSelectedCandidate) -> Vec<String> {
    let mut source_ids = candidate
        .source_ids
        .iter()
        .map(|source_id| source_id.trim())
        .filter(|source_id| !source_id.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    source_ids.push(candidate.memory_candidate_key.clone());
    source_ids.sort();
    source_ids.dedup();
    source_ids
}

fn deterministic_projection_text(candidate: &MemoryPromptSelectedCandidate) -> String {
    let source = candidate.source_text.trim();
    if source.chars().count() <= MAX_PROJECTION_TEXT_CHARS {
        return source.to_string();
    }
    truncate_chars(source, MAX_PROJECTION_TEXT_CHARS)
}

fn projection_text_is_useful(value: &str) -> bool {
    value.split_whitespace().count() >= 3
}

fn parse_memory_utility_review_output(
    raw: &str,
    allowed_keys: &BTreeSet<String>,
) -> Result<Vec<MemoryTemperatureUtilityReviewJudgement>, String> {
    let value = parse_json_value(raw)?;
    parse_memory_utility_review_value(&value, allowed_keys)
}

fn parse_memory_utility_batch_review_output(
    raw: &str,
    allowed_by_run: &BTreeMap<String, BTreeSet<String>>,
) -> Result<BTreeMap<String, Vec<MemoryTemperatureUtilityReviewJudgement>>, String> {
    let value = parse_json_value(raw)?;
    if allowed_by_run.len() == 1
        && value.get("runs").is_none()
        && value.get("run_reviews").is_none()
    {
        let (run_id, allowed_keys) = allowed_by_run
            .iter()
            .next()
            .ok_or_else(|| "missing allowed run keys".to_string())?;
        return Ok(BTreeMap::from([(
            run_id.clone(),
            parse_memory_utility_review_value(&value, allowed_keys)?,
        )]));
    }

    let runs = if let Some(items) = value.get("runs").and_then(Value::as_array) {
        items
    } else if let Some(items) = value.get("run_reviews").and_then(Value::as_array) {
        items
    } else if let Some(items) = value.as_array() {
        items
    } else {
        return Err("missing runs array".to_string());
    };

    let mut parsed = BTreeMap::new();
    for run in runs {
        let Some(run_id) = run
            .get("run_id")
            .or_else(|| run.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|run_id| !run_id.is_empty())
        else {
            continue;
        };
        let Some(allowed_keys) = allowed_by_run.get(run_id) else {
            continue;
        };
        let judgements = parse_memory_utility_review_value(run, allowed_keys)?;
        if !judgements.is_empty() {
            parsed.insert(run_id.to_string(), judgements);
        }
    }

    Ok(parsed)
}

pub fn parse_memory_utility_review_value(
    value: &Value,
    allowed_keys: &BTreeSet<String>,
) -> Result<Vec<MemoryTemperatureUtilityReviewJudgement>, String> {
    let memories = if let Some(items) = value.get("memories").and_then(Value::as_array) {
        items
    } else if let Some(items) = value.get("judgements").and_then(Value::as_array) {
        items
    } else if let Some(items) = value.get("reviews").and_then(Value::as_array) {
        items
    } else if let Some(items) = value.get("memory_usage").and_then(Value::as_array) {
        items
    } else if let Some(items) = value.as_array() {
        items
    } else {
        return Err("missing memories array".to_string());
    };

    let mut seen = BTreeSet::new();
    let mut judgements = Vec::new();
    for item in memories {
        let Some(key) = item
            .get("memory_candidate_key")
            .or_else(|| item.get("key"))
            .or_else(|| item.get("memory_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
        else {
            continue;
        };
        if !allowed_keys.contains(key) || !seen.insert(key.to_string()) {
            continue;
        }
        let label = item
            .get("label")
            .or_else(|| item.get("utility"))
            .or_else(|| item.get("usage"))
            .and_then(Value::as_str)
            .map(MemoryTemperatureUtilityLabel::from_review_label)
            .unwrap_or(MemoryTemperatureUtilityLabel::Unknown);
        let confidence = item
            .get("confidence")
            .and_then(Value::as_f64)
            .map(|value| value.clamp(0.0, 1.0));
        let reason = item
            .get("reason")
            .or_else(|| item.get("rationale"))
            .and_then(Value::as_str)
            .map(|reason| truncate_chars(reason, MAX_REASON_CHARS));
        let compact_text = item
            .get("compact_text")
            .or_else(|| item.get("projection_text"))
            .or_else(|| item.get("summary"))
            .and_then(Value::as_str)
            .map(|text| truncate_chars(text, MAX_PROJECTION_TEXT_CHARS));
        judgements.push(MemoryTemperatureUtilityReviewJudgement {
            memory_candidate_key: key.to_string(),
            label,
            confidence,
            reason,
            compact_text,
        });
    }

    Ok(judgements)
}

fn parse_json_value(raw: &str) -> Result<Value, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty response".to_string());
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Ok(value);
    }

    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim);
    if let Some(unfenced) = unfenced {
        if let Ok(value) = serde_json::from_str::<Value>(unfenced) {
            return Ok(value);
        }
    }

    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start < end {
            if let Ok(value) = serde_json::from_str::<Value>(&trimmed[start..=end]) {
                return Ok(value);
            }
        }
    }
    if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
        if start < end {
            if let Ok(value) = serde_json::from_str::<Value>(&trimmed[start..=end]) {
                return Ok(value);
            }
        }
    }

    Err("could not parse JSON response".to_string())
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::MemoryTemperatureTier;
    use super::*;

    pub(super) fn queue_review_input(
        run_id: &str,
        outcome: &str,
    ) -> MemoryTemperatureUtilityReviewInput {
        let source_text = "durable utility-review queue test source";
        MemoryTemperatureUtilityReviewInput {
            run_id: run_id.to_string(),
            agent_id: "agent-1".to_string(),
            task_id: Some("task-1".to_string()),
            execution_id: Some("execution-1".to_string()),
            chat_session_id: None,
            goal: "preserve every queued review".to_string(),
            outcome: outcome.to_string(),
            final_answer: "completed".to_string(),
            action_trace: Vec::new(),
            selected_candidates: vec![MemoryPromptSelectedCandidate {
                memory_candidate_key: format!("candidate-{run_id}"),
                semantic_memory_type: SemanticMemoryType::Episode,
                temperature_tier: MemoryTemperatureTier::T2,
                tier_name: "episodes.items".to_string(),
                source_key: format!("episode-{run_id}"),
                source_ids: vec![format!("episodes.items#episode-{run_id}")],
                source_text_hash: source_text_hash(source_text),
                source_text: source_text.to_string(),
                text: source_text.to_string(),
                projection_used: false,
                app_model_processing: None,
            }],
        }
    }

    #[test]
    fn provider_bound_local_only_memory_never_enters_durable_or_model_review_payloads() {
        let mut input = queue_review_input("run-local-only", "completed");
        input.selected_candidates[0].app_model_processing =
            Some(crate::magician_v2::apps::models::AppModelProcessing::LocalOnly);

        let bounded = bounded_review_input_for_queue(&input);
        assert!(bounded.selected_candidates.is_empty());
        assert_eq!(
            review_prompt_payload(&input)["injected_memories"],
            serde_json::json!([])
        );
    }

    #[tokio::test]
    async fn concurrent_queue_enqueues_preserve_each_run() {
        let temp_dir = tempfile::tempdir().expect("temporary memory root");
        let memory_service = AgentMemoryService::with_base_path(temp_dir.path());
        let first = queue_review_input("run-1", "completed-first");
        let second = queue_review_input("run-2", "completed-second");

        let (first_result, second_result) = tokio::join!(
            enqueue_memory_temperature_utility_review(&memory_service, &first),
            enqueue_memory_temperature_utility_review(&memory_service, &second),
        );
        first_result.expect("first queue entry");
        second_result.expect("second queue entry");

        let queue = load_utility_review_queue(&memory_service)
            .await
            .expect("load utility review queue");
        assert_eq!(
            queue.entries.keys().cloned().collect::<Vec<_>>(),
            vec!["run-1".to_string(), "run-2".to_string()]
        );
    }

    #[tokio::test]
    async fn queue_enqueue_is_idempotent_by_run_id() {
        let temp_dir = tempfile::tempdir().expect("temporary memory root");
        let memory_service = AgentMemoryService::with_base_path(temp_dir.path());
        let initial = queue_review_input("run-1", "first outcome");
        let updated = queue_review_input("run-1", "updated outcome");

        enqueue_memory_temperature_utility_review(&memory_service, &initial)
            .await
            .expect("initial queue entry");
        enqueue_memory_temperature_utility_review(&memory_service, &updated)
            .await
            .expect("updated queue entry");

        let queue = load_utility_review_queue(&memory_service)
            .await
            .expect("load utility review queue");
        assert_eq!(queue.entries.len(), 1);
        assert_eq!(queue.entries["run-1"].input.outcome, "updated outcome");
        assert_eq!(queue.entries["run-1"].batch_review_count, 0);
    }

    #[tokio::test]
    async fn durable_queue_claims_incrementally_and_isolates_item_failure() {
        let temp_dir = tempfile::tempdir().expect("temporary memory root");
        let memory_service = AgentMemoryService::with_base_path(temp_dir.path());
        enqueue_memory_temperature_utility_review(
            &memory_service,
            &queue_review_input("run-1", "first"),
        )
        .await
        .expect("first queue entry");
        enqueue_memory_temperature_utility_review(
            &memory_service,
            &queue_review_input("run-2", "second"),
        )
        .await
        .expect("second queue entry");

        let first = claim_next_utility_review(&memory_service, "worker-a")
            .await
            .expect("claim first")
            .expect("first item");
        assert_eq!(first.run_id, "run-1");
        fail_claimed_utility_review(
            &memory_service,
            "run-1",
            "worker-a",
            UtilityReviewFailureClass {
                code: "provider_deadline_exceeded",
                retryable_indefinitely: true,
            },
            "provider was saturated",
        )
        .await
        .expect("schedule first retry");

        let second = claim_next_utility_review(&memory_service, "worker-b")
            .await
            .expect("claim second")
            .expect("second item");
        assert_eq!(second.run_id, "run-2");
        assert!(
            complete_claimed_utility_review(&memory_service, "run-2", "worker-b")
                .await
                .expect("complete second")
        );

        let queue = load_utility_review_queue(&memory_service)
            .await
            .expect("load utility queue");
        assert_eq!(
            queue.entries["run-1"].state,
            MemoryTemperatureUtilityReviewWorkState::Retry
        );
        assert_eq!(queue.entries["run-1"].attempts, 1);
        assert!(queue.entries["run-1"].last_batch_error.is_some());
        assert_eq!(
            queue.entries["run-2"].state,
            MemoryTemperatureUtilityReviewWorkState::Completed
        );
        assert_eq!(queue.entries["run-2"].batch_review_count, 1);
        assert!(queue.entries["run-2"].last_batch_error.is_none());

        let health = memory_temperature_utility_queue_health(&memory_service)
            .await
            .expect("queue health");
        assert_eq!(health.active, 1);
        assert_eq!(health.retrying, 1);
        assert_eq!(health.eligible, 0);
        assert_eq!(health.dead, 0);
    }

    #[tokio::test]
    async fn identical_duplicate_does_not_bypass_retry_backoff() {
        let temp_dir = tempfile::tempdir().expect("temporary memory root");
        let memory_service = AgentMemoryService::with_base_path(temp_dir.path());
        let input = queue_review_input("run-1", "completed");
        enqueue_memory_temperature_utility_review(&memory_service, &input)
            .await
            .expect("queue entry");
        claim_next_utility_review(&memory_service, "worker-a")
            .await
            .expect("claim")
            .expect("claimed item");
        fail_claimed_utility_review(
            &memory_service,
            "run-1",
            "worker-a",
            UtilityReviewFailureClass {
                code: "provider_deadline_exceeded",
                retryable_indefinitely: true,
            },
            "provider was saturated",
        )
        .await
        .expect("schedule retry");
        let before = load_utility_review_queue(&memory_service)
            .await
            .expect("load retry queue")
            .entries
            .remove("run-1")
            .expect("retry entry");

        enqueue_memory_temperature_utility_review(&memory_service, &input)
            .await
            .expect("duplicate completion");
        let after = load_utility_review_queue(&memory_service)
            .await
            .expect("load duplicate queue")
            .entries
            .remove("run-1")
            .expect("retry entry");
        assert_eq!(after.state, MemoryTemperatureUtilityReviewWorkState::Retry);
        assert_eq!(after.attempts, before.attempts);
        assert_eq!(after.next_retry_at, before.next_retry_at);
        assert_eq!(after.last_batch_error, before.last_batch_error);
    }

    #[tokio::test]
    async fn expired_utility_review_lease_is_reclaimed_after_restart_boundary() {
        let temp_dir = tempfile::tempdir().expect("temporary memory root");
        let memory_service = AgentMemoryService::with_base_path(temp_dir.path());
        enqueue_memory_temperature_utility_review(
            &memory_service,
            &queue_review_input("run-1", "completed"),
        )
        .await
        .expect("queue entry");
        claim_next_utility_review(&memory_service, "stale-worker")
            .await
            .expect("claim")
            .expect("claimed item");

        let queue_lock = utility_review_queue_lock(&memory_service).await;
        let _guard = queue_lock.lock().await;
        let mut queue = load_utility_review_queue(&memory_service)
            .await
            .expect("load claimed queue");
        queue.entries.get_mut("run-1").unwrap().lease_expires_at =
            Some(Utc::now() - chrono::Duration::seconds(1));
        save_utility_review_queue(&memory_service, &queue)
            .await
            .expect("persist expired lease");
        drop(_guard);

        let reclaimed = claim_next_utility_review(&memory_service, "replacement-worker")
            .await
            .expect("reclaim")
            .expect("reclaimed item");
        assert_eq!(reclaimed.run_id, "run-1");
        let queue = load_utility_review_queue(&memory_service)
            .await
            .expect("load reclaimed queue");
        assert_eq!(
            queue.entries["run-1"].lease_owner.as_deref(),
            Some("replacement-worker")
        );
    }

    #[tokio::test]
    async fn poison_utility_review_dead_letters_only_after_bounded_attempts() {
        let temp_dir = tempfile::tempdir().expect("temporary memory root");
        let memory_service = AgentMemoryService::with_base_path(temp_dir.path());
        enqueue_memory_temperature_utility_review(
            &memory_service,
            &queue_review_input("run-1", "completed"),
        )
        .await
        .expect("queue entry");
        {
            let queue_lock = utility_review_queue_lock(&memory_service).await;
            let _guard = queue_lock.lock().await;
            let mut queue = load_utility_review_queue(&memory_service)
                .await
                .expect("load queue");
            queue.entries.get_mut("run-1").unwrap().attempts =
                UTILITY_REVIEW_MAX_POISON_ATTEMPTS - 1;
            save_utility_review_queue(&memory_service, &queue)
                .await
                .expect("persist attempts");
        }
        claim_next_utility_review(&memory_service, "worker-a")
            .await
            .expect("claim")
            .expect("claimed item");
        fail_claimed_utility_review(
            &memory_service,
            "run-1",
            "worker-a",
            UtilityReviewFailureClass {
                code: "invalid_model_output",
                retryable_indefinitely: false,
            },
            "schema remained invalid after repair",
        )
        .await
        .expect("dead letter");

        let queue = load_utility_review_queue(&memory_service)
            .await
            .expect("load queue");
        assert_eq!(
            queue.entries["run-1"].state,
            MemoryTemperatureUtilityReviewWorkState::Dead
        );
        assert!(queue.entries["run-1"].next_retry_at.is_none());
    }

    #[tokio::test]
    async fn duplicate_completion_does_not_resurrect_unchanged_dead_letter() {
        let temp_dir = tempfile::tempdir().expect("temporary memory root");
        let memory_service = AgentMemoryService::with_base_path(temp_dir.path());
        let input = queue_review_input("run-1", "completed");
        enqueue_memory_temperature_utility_review(&memory_service, &input)
            .await
            .expect("queue entry");
        {
            let queue_lock = utility_review_queue_lock(&memory_service).await;
            let _guard = queue_lock.lock().await;
            let mut queue = load_utility_review_queue(&memory_service)
                .await
                .expect("load queue");
            let entry = queue.entries.get_mut("run-1").unwrap();
            entry.state = MemoryTemperatureUtilityReviewWorkState::Dead;
            entry.attempts = UTILITY_REVIEW_MAX_POISON_ATTEMPTS;
            entry.last_batch_error = Some("invalid_model_output".to_string());
            save_utility_review_queue(&memory_service, &queue)
                .await
                .expect("persist dead letter");
        }

        enqueue_memory_temperature_utility_review(&memory_service, &input)
            .await
            .expect("duplicate completion");
        let queue = load_utility_review_queue(&memory_service)
            .await
            .expect("load queue");
        assert_eq!(
            queue.entries["run-1"].state,
            MemoryTemperatureUtilityReviewWorkState::Dead
        );
        assert_eq!(
            queue.entries["run-1"].attempts,
            UTILITY_REVIEW_MAX_POISON_ATTEMPTS
        );
    }

    #[tokio::test]
    async fn changed_evidence_releases_a_dead_utility_review() {
        let temp_dir = tempfile::tempdir().expect("temporary memory root");
        let memory_service = AgentMemoryService::with_base_path(temp_dir.path());
        let input = queue_review_input("run-1", "old outcome");
        enqueue_memory_temperature_utility_review(&memory_service, &input)
            .await
            .expect("queue entry");
        {
            let queue_lock = utility_review_queue_lock(&memory_service).await;
            let _guard = queue_lock.lock().await;
            let mut queue = load_utility_review_queue(&memory_service)
                .await
                .expect("load queue");
            let entry = queue.entries.get_mut("run-1").unwrap();
            entry.state = MemoryTemperatureUtilityReviewWorkState::Dead;
            entry.attempts = UTILITY_REVIEW_MAX_POISON_ATTEMPTS;
            save_utility_review_queue(&memory_service, &queue)
                .await
                .expect("persist dead letter");
        }

        let changed = queue_review_input("run-1", "new outcome with corrected evidence");
        enqueue_memory_temperature_utility_review(&memory_service, &changed)
            .await
            .expect("changed completion");
        let queue = load_utility_review_queue(&memory_service)
            .await
            .expect("load queue");
        assert_eq!(
            queue.entries["run-1"].state,
            MemoryTemperatureUtilityReviewWorkState::Pending
        );
        assert_eq!(queue.entries["run-1"].attempts, 0);
        assert_eq!(queue.entries["run-1"].input.outcome, changed.outcome);
    }

    #[tokio::test]
    async fn reviewer_contract_upgrade_releases_unchanged_dead_letter() {
        let temp_dir = tempfile::tempdir().expect("temporary memory root");
        let memory_service = AgentMemoryService::with_base_path(temp_dir.path());
        let input = queue_review_input("run-1", "unchanged evidence");
        enqueue_memory_temperature_utility_review(&memory_service, &input)
            .await
            .expect("queue entry");
        {
            let queue_lock = utility_review_queue_lock(&memory_service).await;
            let _guard = queue_lock.lock().await;
            let mut queue = load_utility_review_queue(&memory_service)
                .await
                .expect("load queue");
            let entry = queue.entries.get_mut("run-1").unwrap();
            entry.state = MemoryTemperatureUtilityReviewWorkState::Dead;
            entry.attempts = UTILITY_REVIEW_MAX_POISON_ATTEMPTS;
            entry.contract = "memory_temperature_utility_review_v0@0.9.0".to_string();
            save_utility_review_queue(&memory_service, &queue)
                .await
                .expect("persist old-contract dead letter");
        }

        let queue = load_utility_review_queue(&memory_service)
            .await
            .expect("normalize upgraded contract");
        let entry = &queue.entries["run-1"];
        assert_eq!(
            entry.state,
            MemoryTemperatureUtilityReviewWorkState::Pending
        );
        assert_eq!(entry.attempts, 0);
        assert_eq!(entry.contract, MEMORY_TEMPERATURE_UTILITY_REVIEW_CONTRACT);
    }

    #[tokio::test]
    async fn legacy_queue_rows_migrate_without_replaying_completed_work() {
        let temp_dir = tempfile::tempdir().expect("temporary memory root");
        let memory_service = AgentMemoryService::with_base_path(temp_dir.path());
        let now = Utc::now();
        let legacy = serde_json::json!({
            "schema_version": 1,
            "updated_at": now,
            "entries": {
                "pending": {
                    "input": queue_review_input("pending", "pending"),
                    "enqueued_at": now,
                    "batch_reviewed_at": null,
                    "batch_review_count": 0,
                    "last_batch_error": null
                },
                "completed": {
                    "input": queue_review_input("completed", "completed"),
                    "enqueued_at": now,
                    "batch_reviewed_at": now,
                    "batch_review_count": 1,
                    "last_batch_error": null
                }
            }
        });
        let path = memory_service
            .storage()
            .root()
            .join("index")
            .join(MEMORY_TEMPERATURE_UTILITY_REVIEW_QUEUE_FILE);
        memory_service
            .storage()
            .write_json_value_atomic(&path, &legacy)
            .await
            .expect("write legacy queue");

        let queue = load_utility_review_queue(&memory_service)
            .await
            .expect("migrate queue");
        assert_eq!(
            queue.entries["pending"].state,
            MemoryTemperatureUtilityReviewWorkState::Pending
        );
        assert_eq!(
            queue.entries["completed"].state,
            MemoryTemperatureUtilityReviewWorkState::Completed
        );
        let health = memory_temperature_utility_queue_health(&memory_service)
            .await
            .expect("queue health");
        assert_eq!(health.active, 1);
        assert_eq!(health.eligible, 1);
    }

    #[test]
    fn utility_review_failure_classification_uses_typed_errors() {
        let deadline = anyhow!(magicllm::LLMError::DeadlineExceeded);
        assert_eq!(
            utility_review_failure_class(&deadline).code,
            "provider_deadline_exceeded"
        );
        assert!(utility_review_failure_class(&deadline).retryable_indefinitely);

        let invalid = anyhow!(magicllm::LLMError::Validation(
            "invalid request contract".to_string()
        ));
        assert_eq!(
            utility_review_failure_class(&invalid).code,
            "non_retryable_request"
        );
        assert!(!utility_review_failure_class(&invalid).retryable_indefinitely);

        let byte_pressure = anyhow!(magicllm::LLMError::QueueBytesFull {
            priority: "background",
            queued_bytes: 128,
            capacity_bytes: 128,
        });
        assert_eq!(
            utility_review_failure_class(&byte_pressure).code,
            "dispatch_queue_full"
        );
        assert!(utility_review_failure_class(&byte_pressure).retryable_indefinitely);

        let oversized = anyhow!(magicllm::LLMError::RequestTooLarge {
            bytes: 129,
            capacity_bytes: 128,
        });
        assert_eq!(
            utility_review_failure_class(&oversized).code,
            "non_retryable_request"
        );
        assert!(!utility_review_failure_class(&oversized).retryable_indefinitely);

        let storage_busy = anyhow!(MemoryStorageError::FileLockTimeout {
            lock_path: "/tmp/memory.lock".to_string(),
            wait_ms: 5_000,
        });
        assert_eq!(
            utility_review_failure_class(&storage_busy).code,
            "memory_storage_busy"
        );
        assert!(utility_review_failure_class(&storage_busy).retryable_indefinitely);
    }

    #[test]
    fn utility_maintenance_defaults_are_small_and_incremental() {
        let config = MemoryTemperatureUtilityBatchMaintenanceConfig::default();
        assert_eq!(config.min_batch_size, 1);
        assert_eq!(config.max_batch_size, 2);
    }

    #[test]
    fn queue_soft_cap_never_discards_live_work() {
        let now = Utc::now();
        let mut queue = MemoryTemperatureUtilityReviewQueue::default();
        for index in 0..=MAX_REVIEW_QUEUE_ENTRIES {
            let run_id = format!("live-{index:04}");
            queue.entries.insert(
                run_id.clone(),
                MemoryTemperatureUtilityReviewQueueEntry {
                    input: queue_review_input(&run_id, "pending"),
                    enqueued_at: now,
                    contract: MEMORY_TEMPERATURE_UTILITY_REVIEW_CONTRACT.to_string(),
                    state: MemoryTemperatureUtilityReviewWorkState::Pending,
                    attempts: 0,
                    next_retry_at: None,
                    lease_owner: None,
                    lease_expires_at: None,
                    last_attempt_at: None,
                    batch_reviewed_at: None,
                    batch_review_count: 0,
                    last_batch_error: None,
                },
            );
        }

        prune_utility_review_queue(&mut queue, now);

        assert_eq!(queue.entries.len(), MAX_REVIEW_QUEUE_ENTRIES + 1);
        assert!(queue
            .entries
            .values()
            .all(|entry| { entry.state == MemoryTemperatureUtilityReviewWorkState::Pending }));
    }

    #[test]
    fn parse_review_output_accepts_fenced_json_and_filters_unknown_keys() {
        let allowed = BTreeSet::from(["candidate-1".to_string()]);
        let raw = r#"```json
{"memories":[
  {"memory_candidate_key":"candidate-1","label":"load-bearing","confidence":0.9,"reason":"used directly"},
  {"memory_candidate_key":"candidate-2","label":"harmful","confidence":0.7,"reason":"not allowed"}
]}
```"#;

        let parsed = parse_memory_utility_review_output(raw, &allowed).expect("parsed");

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].memory_candidate_key, "candidate-1");
        assert_eq!(parsed[0].label, MemoryTemperatureUtilityLabel::LoadBearing);
        assert_eq!(parsed[0].confidence, Some(0.9));
    }

    #[test]
    fn parse_review_output_maps_unknown_labels() {
        let allowed = BTreeSet::from(["candidate-1".to_string()]);
        let raw = r#"{"reviews":[{"key":"candidate-1","label":"maybe","reason":"unclear"}]}"#;

        let parsed = parse_memory_utility_review_output(raw, &allowed).expect("parsed");

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].label, MemoryTemperatureUtilityLabel::Unknown);
    }

    #[test]
    fn parse_batch_review_output_groups_by_run_and_supports_plan_names() {
        let allowed_by_run = BTreeMap::from([
            (
                "run-1".to_string(),
                BTreeSet::from(["candidate-1".to_string()]),
            ),
            (
                "run-2".to_string(),
                BTreeSet::from(["candidate-2".to_string()]),
            ),
        ]);
        let raw = r#"{"runs":[
  {"run_id":"run-1","memory_usage":[{"memory_id":"candidate-1","usage":"useful","confidence":0.8}]},
  {"run_id":"run-2","memories":[{"memory_candidate_key":"candidate-2","label":"irrelevant"}]},
  {"run_id":"run-3","memories":[{"memory_candidate_key":"candidate-3","label":"harmful"}]}
]}"#;

        let parsed =
            parse_memory_utility_batch_review_output(raw, &allowed_by_run).expect("parsed");

        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed["run-1"][0].label,
            MemoryTemperatureUtilityLabel::Useful
        );
        assert_eq!(
            parsed["run-2"][0].label,
            MemoryTemperatureUtilityLabel::Irrelevant
        );
    }

    #[test]
    fn review_prompt_payload_includes_bounded_action_trace() {
        let input = MemoryTemperatureUtilityReviewInput {
            run_id: "run-1".to_string(),
            agent_id: "agent-1".to_string(),
            task_id: None,
            execution_id: Some("exec-1".to_string()),
            chat_session_id: None,
            goal: "finish task".to_string(),
            outcome: "goal_achieved".to_string(),
            final_answer: "Done".to_string(),
            action_trace: vec![MemoryTemperatureUtilityReviewTraceItem {
                source: "agentic".to_string(),
                action_type: Some("bash".to_string()),
                tool_name: Some("cargo test".to_string()),
                succeeded: Some(true),
                duration_ms: Some(42),
                summary: "Ran tests after applying the remembered setup command.".to_string(),
                output_preview: Some("all tests passed".to_string()),
                error: None,
            }],
            selected_candidates: Vec::new(),
        };

        let payload = review_prompt_payload(&input);

        assert_eq!(payload["action_trace"][0]["source"], "agentic");
        assert_eq!(payload["action_trace"][0]["action_type"], "bash");
        assert_eq!(payload["action_trace"][0]["succeeded"], true);
    }

    #[test]
    fn projection_upserts_use_model_compact_text_for_load_bearing_memory() {
        let source =
            "episode: the agent tried three approaches, found that the second workflow was \
stable, and used the cached project command to finish the task. This is intentionally long enough \
to justify a projection instead of reinjecting the raw episode every time.";
        let candidate = MemoryPromptSelectedCandidate {
            memory_candidate_key: "agent:agent-1::episodes.items:ep-1".to_string(),
            semantic_memory_type: SemanticMemoryType::Episode,
            temperature_tier: MemoryTemperatureTier::T2,
            tier_name: "episodes.items".to_string(),
            source_key: "ep-1".to_string(),
            source_ids: vec![
                "episodes.items#ep-1".to_string(),
                "agent:agent-1::episodes.items:ep-1".to_string(),
            ],
            source_text_hash: source_text_hash(source),
            source_text: source.to_string(),
            text: source.to_string(),
            projection_used: false,
            app_model_processing: None,
        };
        let input = MemoryTemperatureUtilityReviewInput {
            run_id: "run-1".to_string(),
            agent_id: "agent-1".to_string(),
            task_id: None,
            execution_id: Some("exec-1".to_string()),
            chat_session_id: None,
            goal: "finish task".to_string(),
            outcome: "goal_achieved".to_string(),
            final_answer: "Done".to_string(),
            action_trace: Vec::new(),
            selected_candidates: vec![candidate],
        };
        let upserts = build_hot_projection_upserts(
            &input,
            &[MemoryTemperatureUtilityReviewJudgement {
                memory_candidate_key: "agent:agent-1::episodes.items:ep-1".to_string(),
                label: MemoryTemperatureUtilityLabel::LoadBearing,
                confidence: Some(0.91),
                reason: Some("The workflow directly shaped the final answer.".to_string()),
                compact_text: Some(
                    "Use the second workflow and cached project command.".to_string(),
                ),
            }],
        );

        assert_eq!(upserts.len(), 1);
        assert_eq!(
            upserts[0].compact_text,
            "Use the second workflow and cached project command."
        );
        assert_eq!(
            upserts[0].review_label,
            MemoryTemperatureUtilityLabel::LoadBearing
        );
        assert_eq!(upserts[0].source_text_hash, source_text_hash(source));
        assert_eq!(
            upserts[0].source_ids,
            vec![
                "agent:agent-1::episodes.items:ep-1".to_string(),
                "episodes.items#ep-1".to_string()
            ]
        );
        assert_eq!(
            upserts[0].source_tier_name.as_deref(),
            Some("episodes.items")
        );
        assert_eq!(upserts[0].source_item_key.as_deref(), Some("ep-1"));
    }

    #[test]
    fn projection_upserts_skip_source_evidence_lane() {
        let source = "source evidence should stay in canonical memory and hydrate on demand only";
        let candidate = MemoryPromptSelectedCandidate {
            memory_candidate_key: "agent:agent-1::source_evidence.items:src-1".to_string(),
            semantic_memory_type: SemanticMemoryType::SourceEvidence,
            temperature_tier: MemoryTemperatureTier::T2,
            tier_name: "source_evidence.items".to_string(),
            source_key: "src-1".to_string(),
            source_ids: vec!["source_evidence.items#src-1".to_string()],
            source_text_hash: source_text_hash(source),
            source_text: source.repeat(12),
            text: source.to_string(),
            projection_used: false,
            app_model_processing: None,
        };
        let input = MemoryTemperatureUtilityReviewInput {
            run_id: "run-1".to_string(),
            agent_id: "agent-1".to_string(),
            task_id: None,
            execution_id: Some("exec-1".to_string()),
            chat_session_id: None,
            goal: "finish task".to_string(),
            outcome: "goal_achieved".to_string(),
            final_answer: "Done".to_string(),
            action_trace: Vec::new(),
            selected_candidates: vec![candidate],
        };

        let upserts = build_hot_projection_upserts(
            &input,
            &[MemoryTemperatureUtilityReviewJudgement {
                memory_candidate_key: "agent:agent-1::source_evidence.items:src-1".to_string(),
                label: MemoryTemperatureUtilityLabel::LoadBearing,
                confidence: Some(0.99),
                reason: Some("Useful raw evidence.".to_string()),
                compact_text: Some("Raw evidence summary should not be projected.".to_string()),
            }],
        );

        assert!(upserts.is_empty());
    }
}
