use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use magicllm::{
    plan_logical_request, ChunkBudget, ChunkDescriptor, ChunkDomainAdapter, ChunkError,
    ChunkFallbackPolicy, ChunkPlan, ChunkValidationError, DispatchedResponse,
    FinalValidationContract, JobOrigin, LLMError, LLMRequest, LLMResponse, LlmDispatchQueue,
    LlmJob, LogicalItem, LogicalLlmRequest, Priority, ReasoningConfig, ReductionContext,
    ReductionPlan, ReductionStrategy, TaskRef, TokenEstimator, TokenUsage, ValidatedChunkOutput,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use super::ChunkDomainAdapterRegistry;
use crate::magician_v2::analytics::runtime_activity_layer::{current_activity_id, KIND_BACKGROUND};

const MAX_REDUCTION_LEVELS: u32 = 64;
const MAX_TELEMETRY_ERROR_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicalChunkStage {
    Map,
    Repair,
    Fallback,
    Reduce,
}

impl PhysicalChunkStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Map => "map",
            Self::Repair => "repair",
            Self::Fallback => "fallback",
            Self::Reduce => "reduce",
        }
    }
}

/// Complete metadata needed to preserve queue semantics for one child call.
#[derive(Clone)]
pub struct LogicalChunkDispatchRequest {
    pub request: LLMRequest,
    pub origin: JobOrigin,
    pub priority: Priority,
    pub task_ref: Option<TaskRef>,
    pub trace_id: Option<String>,
    pub submission_deadline: Option<Instant>,
    pub cancellation: CancellationToken,
    pub idempotency_key: String,
    pub router_snapshot: Option<Arc<dyn magicllm::dispatch::DispatchRouter>>,
}

#[async_trait]
pub trait LogicalChunkDispatch: Send + Sync {
    async fn dispatch(
        &self,
        request: LogicalChunkDispatchRequest,
    ) -> Result<DispatchedResponse, LLMError>;
}

#[async_trait]
impl LogicalChunkDispatch for LlmDispatchQueue {
    async fn dispatch(
        &self,
        request: LogicalChunkDispatchRequest,
    ) -> Result<DispatchedResponse, LLMError> {
        let (job, response_rx) = LlmJob::new(request.request, request.origin);
        let job_id = job.job_id.clone();
        let mut job = job
            .with_priority(request.priority)
            .with_idempotency_key(request.idempotency_key);
        if let Some(task_ref) = request.task_ref {
            job = job.with_task(task_ref);
        }
        if let Some(trace_id) = request.trace_id {
            job = job.with_trace_id(trace_id);
        }
        if let Some(deadline) = request.submission_deadline {
            job = job.with_submission_deadline(deadline);
        }
        if let Some(router_snapshot) = request.router_snapshot {
            job = job.with_router_snapshot(router_snapshot);
        }
        self.submit(job).await?;

        let receive = async {
            response_rx.await.map_err(|_| LLMError::Cancelled {
                reason: "logical_chunk_receiver_dropped".to_string(),
            })?
        };
        tokio::pin!(receive);

        if let Some(deadline) = request.submission_deadline {
            let deadline_sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
            tokio::pin!(deadline_sleep);
            tokio::select! {
                result = &mut receive => result,
                _ = request.cancellation.cancelled() => {
                    self.cancel_job(&job_id, "logical_chunk_cancelled");
                    Err(LLMError::Cancelled { reason: "logical_chunk_cancelled".to_string() })
                }
                _ = &mut deadline_sleep => {
                    self.cancel_job(&job_id, "logical_chunk_deadline_exceeded");
                    Err(LLMError::DeadlineExceeded)
                }
            }
        } else {
            tokio::select! {
                result = &mut receive => result,
                _ = request.cancellation.cancelled() => {
                    self.cancel_job(&job_id, "logical_chunk_cancelled");
                    Err(LLMError::Cancelled { reason: "logical_chunk_cancelled".to_string() })
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct LogicalChunkExecutionContext {
    pub logical_call_id: String,
    /// Parent logical-call identity for the aggregate operation. Physical
    /// map/repair/fallback/reduce requests are emitted as children of it.
    pub logical_trace_context: Option<magicllm::LlmTraceContext>,
    pub adapter_id: String,
    pub primary_profile: String,
    /// Exact provider lock for the primary profile. Production logical
    /// chunking defaults to Ollama; Phase 6 can explicitly set the current
    /// cloud provider for a non-authoritative comparison run.
    pub primary_provider: String,
    /// Local chunk execution always disables model reasoning. The Phase 6
    /// cloud comparison may retain the baseline profile's reasoning defaults.
    pub disable_reasoning: bool,
    /// Production calls lock the exact configured profile. The live eval's
    /// cloud lane disables only this lock so the selected baseline profile can
    /// exercise its normal same-provider retry profile.
    pub lock_primary_profile: bool,
    pub fallback_policy: ChunkFallbackPolicy,
    pub fallback_profile: Option<String>,
    pub fallback_provider: Option<String>,
    pub priority: Priority,
    pub task_ref: Option<TaskRef>,
    pub trace_id: Option<String>,
    pub deadline: Option<Instant>,
    pub cancellation: CancellationToken,
    pub caller: String,
    /// Exact configured-router generation that supplied the chunk policy and
    /// profile. Queue pickup and every retry must retain this authority.
    pub router_snapshot: Option<Arc<dyn magicllm::dispatch::DispatchRouter>>,
}

impl LogicalChunkExecutionContext {
    pub fn new(adapter_id: impl Into<String>, primary_profile: impl Into<String>) -> Self {
        Self {
            logical_call_id: ulid::Ulid::new().to_string(),
            logical_trace_context: None,
            adapter_id: adapter_id.into(),
            primary_profile: primary_profile.into(),
            primary_provider: "ollama".to_string(),
            disable_reasoning: true,
            lock_primary_profile: true,
            fallback_policy: ChunkFallbackPolicy::SameProviderOnly,
            fallback_profile: None,
            fallback_provider: None,
            priority: Priority::Background,
            task_ref: None,
            trace_id: None,
            deadline: None,
            cancellation: CancellationToken::new(),
            caller: "logical_chunk_runner".to_string(),
            router_snapshot: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AggregateTokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub reasoning_tokens: u64,
    pub cached_tokens: u64,
    pub cache_creation_tokens: u64,
}

impl AggregateTokenUsage {
    fn add_response(&mut self, response: &LLMResponse) {
        let Some(usage) = response.usage.as_ref() else {
            return;
        };
        let prompt = u64::from(usage.prompt_tokens.unwrap_or(0));
        let completion = u64::from(usage.completion_tokens.unwrap_or(0));
        self.prompt_tokens = self.prompt_tokens.saturating_add(prompt);
        self.completion_tokens = self.completion_tokens.saturating_add(completion);
        self.total_tokens =
            self.total_tokens
                .saturating_add(u64::from(usage.total_tokens.unwrap_or_else(|| {
                    usage
                        .prompt_tokens
                        .unwrap_or(0)
                        .saturating_add(usage.completion_tokens.unwrap_or(0))
                })));
        self.reasoning_tokens = self
            .reasoning_tokens
            .saturating_add(u64::from(usage.reasoning_tokens.unwrap_or(0)));
        self.cached_tokens = self
            .cached_tokens
            .saturating_add(u64::from(usage.cached_tokens.unwrap_or(0)));
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(u64::from(usage.cache_creation_tokens.unwrap_or(0)));
    }

    fn as_response_usage(&self) -> TokenUsage {
        TokenUsage {
            prompt_tokens: Some(saturating_u32(self.prompt_tokens)),
            completion_tokens: Some(saturating_u32(self.completion_tokens)),
            total_tokens: Some(saturating_u32(self.total_tokens)),
            reasoning_tokens: Some(saturating_u32(self.reasoning_tokens)),
            cached_tokens: Some(saturating_u32(self.cached_tokens)),
            cache_creation_tokens: Some(saturating_u32(self.cache_creation_tokens)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogicalChunkMetadata {
    pub applied: bool,
    pub logical_call_id: String,
    pub operation: String,
    pub profile: String,
    pub fallback_policy: ChunkFallbackPolicy,
    pub fallback_profile: Option<String>,
    pub adapter: String,
    pub adapter_version: String,
    pub estimated_logical_tokens: u32,
    pub logical_window_tokens: u32,
    pub physical_window_tokens: u32,
    pub target_payload_tokens: u32,
    pub effective_payload_tokens: u32,
    pub chunk_count: u32,
    pub source_item_count: u32,
    pub terminal_item_count: u32,
    pub oversized_split_count: u32,
    pub completed_source_items: u32,
    pub physical_call_count: u32,
    pub reduction_levels: u32,
    pub local_repairs: u32,
    pub fallback_calls: u32,
    pub priority: String,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub chat_session_id: Option<String>,
    pub trace_id: Option<String>,
    pub caller: String,
    pub deadline_present: bool,
    pub planning_ms: u64,
    pub queue_wait_ms: u64,
    pub provider_execution_ms: u64,
    pub map_duration_ms: u64,
    pub repair_duration_ms: u64,
    pub fallback_duration_ms: u64,
    pub reduction_duration_ms: u64,
    pub total_duration_ms: u64,
    pub usage: AggregateTokenUsage,
    /// A logical row summarizes already-accounted physical calls. It must
    /// never add cost when analytics rows are summed.
    pub summary_incremental_cost_usd: f64,
    pub summary_only: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LogicalChunkTelemetryEvent {
    PhysicalCompleted {
        trace_receipt: magicllm::LlmTraceReceipt,
        started_at_ms: i64,
        logical_call_id: String,
        operation: String,
        agent_id: Option<String>,
        profile: String,
        provider: Option<String>,
        adapter: String,
        adapter_version: String,
        stage: PhysicalChunkStage,
        chunk_index: u32,
        reduction_level: u32,
        item_count: u32,
        source_identity_hash: String,
        requested_model: String,
        estimated_payload_tokens: u32,
        usage: AggregateTokenUsage,
        usage_reported: bool,
        validation_ok: bool,
        validation_error: Option<String>,
        queue_wait_ms: u64,
        provider_execution_ms: u64,
        attempts: u32,
        /// Correlation only: authoritative billing remains on the physical
        /// queue/provider row, preventing this event from double-counting it.
        billing_authoritative: bool,
    },
    LogicalCompleted {
        metadata: LogicalChunkMetadata,
        trace_context: Option<magicllm::LlmTraceContext>,
        started_at_ms: i64,
    },
    LogicalFailed {
        trace_context: Option<magicllm::LlmTraceContext>,
        started_at_ms: i64,
        logical_call_id: String,
        operation: String,
        profile: String,
        fallback_policy: ChunkFallbackPolicy,
        fallback_profile: Option<String>,
        adapter: String,
        adapter_version: Option<String>,
        priority: String,
        task_id: Option<String>,
        agent_id: Option<String>,
        chat_session_id: Option<String>,
        trace_id: Option<String>,
        caller: String,
        deadline_present: bool,
        completed_source_items: u32,
        physical_call_count: u32,
        usage: AggregateTokenUsage,
        total_duration_ms: u64,
        error_class: String,
        error: String,
        summary_incremental_cost_usd: f64,
    },
}

pub trait LogicalChunkTelemetrySink: Send + Sync {
    fn emit(&self, event: LogicalChunkTelemetryEvent);
}

#[derive(Debug, Default)]
pub struct TracingLogicalChunkTelemetry;

impl LogicalChunkTelemetrySink for TracingLogicalChunkTelemetry {
    fn emit(&self, event: LogicalChunkTelemetryEvent) {
        match &event {
            LogicalChunkTelemetryEvent::LogicalFailed { error_class, .. } => warn!(
                target: "magician::metrics::logical_chunking",
                error_class,
                event = ?event,
                "logical chunk operation failed"
            ),
            _ => info!(
                target: "magician::metrics::logical_chunking",
                event = ?event,
                "logical chunk telemetry"
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogicalChunkErrorContext {
    pub logical_call_id: String,
    pub operation: String,
    pub profile: String,
    pub adapter: String,
    pub adapter_version: String,
    pub stage: Option<PhysicalChunkStage>,
    pub chunk_index: Option<u32>,
}

impl fmt::Display for LogicalChunkErrorContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "logical_call={} operation={} profile={} adapter={}@{}",
            self.logical_call_id, self.operation, self.profile, self.adapter, self.adapter_version
        )?;
        if let Some(stage) = self.stage {
            write!(formatter, " stage={}", stage.as_str())?;
        }
        if let Some(index) = self.chunk_index {
            write!(formatter, " chunk={index}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum LogicalChunkError {
    #[error("chunk adapter `{adapter}` is not registered for logical call `{logical_call_id}`")]
    AdapterNotFound {
        logical_call_id: String,
        adapter: String,
    },
    #[error("{context}: adapter does not support the operation")]
    AdapterOperationMismatch { context: LogicalChunkErrorContext },
    #[error("{context}: adapter does not provide final validation")]
    FinalValidatorUnavailable { context: LogicalChunkErrorContext },
    #[error("{context}: planning failed: {source}")]
    Planning {
        context: LogicalChunkErrorContext,
        #[source]
        source: ChunkError,
    },
    #[error("{context}: adapter hook failed: {source}")]
    Adapter {
        context: LogicalChunkErrorContext,
        #[source]
        source: ChunkError,
    },
    #[error("{context}: physical transport failed: {source}")]
    Transport {
        context: LogicalChunkErrorContext,
        #[source]
        source: LLMError,
    },
    #[error("{context}: chunk output is invalid: {source}")]
    OutputInvalid {
        context: LogicalChunkErrorContext,
        #[source]
        source: ChunkValidationError,
    },
    #[error("{context}: repair failed: {reason}")]
    RepairFailed {
        context: LogicalChunkErrorContext,
        reason: String,
    },
    #[error("{context}: fallback failed: {reason}")]
    FallbackFailed {
        context: LogicalChunkErrorContext,
        reason: String,
    },
    #[error("{context}: reduction failed: {reason}")]
    ReductionFailed {
        context: LogicalChunkErrorContext,
        reason: String,
    },
    #[error("{context}: final output is invalid: {source}")]
    FinalOutputInvalid {
        context: LogicalChunkErrorContext,
        #[source]
        source: ChunkValidationError,
    },
    #[error("{context}: operation cancelled after completing {completed_source_items} source items: {reason}")]
    Cancelled {
        context: LogicalChunkErrorContext,
        completed_source_items: u32,
        reason: String,
    },
    #[error("{context}: logical deadline exceeded after completing {completed_source_items} source items")]
    DeadlineExceeded {
        context: LogicalChunkErrorContext,
        completed_source_items: u32,
    },
}

impl LogicalChunkError {
    fn class(&self) -> &'static str {
        match self {
            Self::AdapterNotFound { .. } => "chunk_adapter_not_found",
            Self::AdapterOperationMismatch { .. } => "chunk_adapter_operation_mismatch",
            Self::FinalValidatorUnavailable { .. } => "final_validator_unavailable",
            Self::Planning { .. } => "chunk_planning_failed",
            Self::Adapter { .. } => "chunk_adapter_failed",
            Self::Transport { .. } => "chunk_transport_failed",
            Self::OutputInvalid { .. } => "chunk_output_invalid",
            Self::RepairFailed { .. } => "chunk_repair_failed",
            Self::FallbackFailed { .. } => "chunk_fallback_failed",
            Self::ReductionFailed { .. } => "chunk_reduction_failed",
            Self::FinalOutputInvalid { .. } => "final_output_invalid",
            Self::Cancelled { .. } => "chunked_operation_cancelled",
            Self::DeadlineExceeded { .. } => "chunked_operation_deadline_exceeded",
        }
    }
}

pub struct LogicalChunkRunner {
    dispatch: Arc<dyn LogicalChunkDispatch>,
    registry: ChunkDomainAdapterRegistry,
    estimator: Arc<dyn TokenEstimator>,
    telemetry: Arc<dyn LogicalChunkTelemetrySink>,
}

impl LogicalChunkRunner {
    pub fn new(
        dispatch: Arc<dyn LogicalChunkDispatch>,
        registry: ChunkDomainAdapterRegistry,
        estimator: Arc<dyn TokenEstimator>,
    ) -> Self {
        Self {
            dispatch,
            registry,
            estimator,
            telemetry: Arc::new(TracingLogicalChunkTelemetry),
        }
    }

    pub fn with_telemetry(mut self, telemetry: Arc<dyn LogicalChunkTelemetrySink>) -> Self {
        self.telemetry = telemetry;
        self
    }

    /// One span for the whole logical call, not one per physical chunk. The
    /// chunk count is what a reader wants; a span per map/repair/reduce
    /// dispatch would bury the aggregate under its own fan-out.
    ///
    /// `request` carries the prompt being chunked — `skip_all` keeps it off
    /// the wire, and only the adapter, operation and logical call id are named.
    #[instrument(
        name = "llm_chunking_run",
        skip_all,
        fields(
            activity_kind = KIND_BACKGROUND,
            adapter = %execution.adapter_id,
            operation = %request.operation,
            logical_call_id = %execution.logical_call_id,
            principal = execution
                .task_ref
                .as_ref()
                .and_then(|task| task.scope.as_ref())
                .map(|scope| scope.principal.as_str()),
            workspace = execution
                .task_ref
                .as_ref()
                .and_then(|task| task.scope.as_ref())
                .map(|scope| scope.workspace.as_str()),
        )
    )]
    pub async fn execute(
        &self,
        request: LogicalLlmRequest,
        budget: ChunkBudget,
        execution: LogicalChunkExecutionContext,
    ) -> Result<LLMResponse, LogicalChunkError> {
        let started = Instant::now();
        let mut state = ExecutionState {
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            ..ExecutionState::default()
        };
        let Some(adapter) = self.registry.get(&execution.adapter_id) else {
            let error = LogicalChunkError::AdapterNotFound {
                logical_call_id: execution.logical_call_id.clone(),
                adapter: execution.adapter_id.clone(),
            };
            self.emit_failure(&request, &execution, &state, started, &error);
            return Err(error);
        };
        let base_context = error_context(&request, &execution, adapter.as_ref(), None, None);
        if !adapter
            .supported_operations()
            .contains(&request.operation.as_str())
        {
            let error = LogicalChunkError::AdapterOperationMismatch {
                context: base_context,
            };
            self.emit_failure(&request, &execution, &state, started, &error);
            return Err(error);
        }
        if adapter.final_validation_contract() != FinalValidationContract::Available {
            let error = LogicalChunkError::FinalValidatorUnavailable {
                context: base_context,
            };
            self.emit_failure(&request, &execution, &state, started, &error);
            return Err(error);
        }
        if let Err(error) =
            ensure_active(&request, &execution, adapter.as_ref(), &state, None, None)
        {
            self.emit_failure(&request, &execution, &state, started, &error);
            return Err(error);
        }

        let planning_started = Instant::now();
        let plan =
            match plan_logical_request(adapter.as_ref(), &request, budget, self.estimator.as_ref())
            {
                Ok(plan) => plan,
                Err(source) => {
                    let error = LogicalChunkError::Planning {
                        context: error_context(&request, &execution, adapter.as_ref(), None, None),
                        source,
                    };
                    self.emit_failure(&request, &execution, &state, started, &error);
                    return Err(error);
                },
            };
        state.planning_duration = planning_started.elapsed();

        let result = self
            .execute_plan(adapter.as_ref(), &request, &plan, &execution, &mut state)
            .await;
        match result {
            Ok(final_value) => {
                if let Err(source) = adapter.validate_final(&final_value) {
                    let error = LogicalChunkError::FinalOutputInvalid {
                        context: error_context(&request, &execution, adapter.as_ref(), None, None),
                        source,
                    };
                    self.emit_failure(&request, &execution, &state, started, &error);
                    return Err(error);
                }
                let text = match adapter.serialize_final(&final_value) {
                    Ok(text) => text,
                    Err(source) => {
                        let error = LogicalChunkError::Adapter {
                            context: error_context(
                                &request,
                                &execution,
                                adapter.as_ref(),
                                None,
                                None,
                            ),
                            source,
                        };
                        self.emit_failure(&request, &execution, &state, started, &error);
                        return Err(error);
                    },
                };
                let metadata = build_metadata(
                    &request,
                    &execution,
                    adapter.as_ref(),
                    &plan,
                    &state,
                    started.elapsed(),
                );
                self.telemetry
                    .emit(LogicalChunkTelemetryEvent::LogicalCompleted {
                        metadata: metadata.clone(),
                        trace_context: execution.logical_trace_context.clone(),
                        started_at_ms: state.started_at_ms,
                    });
                Ok(LLMResponse {
                    text: Some(Arc::<str>::from(text)),
                    usage: Some(state.usage.as_response_usage()),
                    finish_reason: Some("logical_complete".to_string()),
                    raw_response: Some(Arc::new(json!({ "logical_chunking": metadata }))),
                    provider_latency_ms: Some(duration_ms(state.provider_execution)),
                    ..LLMResponse::default()
                })
            },
            Err(error) => {
                self.emit_failure(&request, &execution, &state, started, &error);
                Err(error)
            },
        }
    }

    async fn execute_plan(
        &self,
        adapter: &dyn ChunkDomainAdapter,
        request: &LogicalLlmRequest,
        plan: &ChunkPlan,
        execution: &LogicalChunkExecutionContext,
        state: &mut ExecutionState,
    ) -> Result<Value, LogicalChunkError> {
        let leaf_items = plan
            .leaf_items
            .iter()
            .map(|item| (item.identity.id.as_str(), item))
            .collect::<HashMap<_, _>>();
        let mut outputs = Vec::with_capacity(plan.chunks.len());

        for chunk in &plan.chunks {
            ensure_active(
                request,
                execution,
                adapter,
                state,
                Some(PhysicalChunkStage::Map),
                Some(chunk.index),
            )?;
            let items = items_for_chunk(chunk, &leaf_items).map_err(|source| {
                LogicalChunkError::Planning {
                    context: error_context(
                        request,
                        execution,
                        adapter,
                        Some(PhysicalChunkStage::Map),
                        Some(chunk.index),
                    ),
                    source,
                }
            })?;
            let map_request =
                adapter
                    .render_map_request(request, &items, chunk)
                    .map_err(|source| LogicalChunkError::Adapter {
                        context: error_context(
                            request,
                            execution,
                            adapter,
                            Some(PhysicalChunkStage::Map),
                            Some(chunk.index),
                        ),
                        source,
                    })?;
            let map_outcome = self
                .dispatch_one(
                    adapter,
                    request,
                    execution,
                    state,
                    map_request,
                    &execution.primary_profile,
                    PhysicalChunkStage::Map,
                    chunk,
                    0,
                )
                .await?;

            match parse_map_response(adapter, &map_outcome.response, chunk) {
                Ok(value) => {
                    self.emit_physical(
                        adapter,
                        request,
                        execution,
                        chunk,
                        &map_outcome,
                        true,
                        None,
                    );
                    mark_completed_sources(state, chunk);
                    outputs.push(ValidatedChunkOutput {
                        chunk: chunk.clone(),
                        value,
                        response: Some(map_outcome.response),
                    });
                },
                Err(validation_error) => {
                    self.emit_physical(
                        adapter,
                        request,
                        execution,
                        chunk,
                        &map_outcome,
                        false,
                        Some(&validation_error),
                    );
                    let value = self
                        .recover_invalid_map(
                            adapter,
                            request,
                            execution,
                            state,
                            chunk,
                            &items,
                            &map_outcome.response,
                            validation_error,
                        )
                        .await?;
                    mark_completed_sources(state, chunk);
                    outputs.push(ValidatedChunkOutput {
                        chunk: chunk.clone(),
                        value,
                        response: None,
                    });
                },
            }
        }

        let reduction_context = ReductionContext {
            operation: request.operation.clone(),
            adapter_id: adapter.id().to_string(),
            adapter_version: adapter.version().to_string(),
            source_identities: plan.source_identities.clone(),
            budget: plan.budget,
            base_request: request.base_request.clone(),
        };
        self.reduce(
            adapter,
            request,
            execution,
            state,
            outputs,
            &reduction_context,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn recover_invalid_map(
        &self,
        adapter: &dyn ChunkDomainAdapter,
        request: &LogicalLlmRequest,
        execution: &LogicalChunkExecutionContext,
        state: &mut ExecutionState,
        chunk: &ChunkDescriptor,
        items: &[LogicalItem],
        invalid_response: &LLMResponse,
        validation_error: ChunkValidationError,
    ) -> Result<Value, LogicalChunkError> {
        if execution.fallback_policy == ChunkFallbackPolicy::Disabled {
            return Err(LogicalChunkError::OutputInvalid {
                context: error_context(
                    request,
                    execution,
                    adapter,
                    Some(PhysicalChunkStage::Map),
                    Some(chunk.index),
                ),
                source: validation_error,
            });
        }

        let mut recovery_error = validation_error.to_string();
        match adapter.render_repair_request(
            request,
            items,
            chunk,
            invalid_response,
            &validation_error,
        ) {
            Ok(Some(repair_request)) => {
                state.local_repairs = state.local_repairs.saturating_add(1);
                match self
                    .dispatch_one(
                        adapter,
                        request,
                        execution,
                        state,
                        repair_request,
                        &execution.primary_profile,
                        PhysicalChunkStage::Repair,
                        chunk,
                        0,
                    )
                    .await
                {
                    Ok(outcome) => match parse_map_response(adapter, &outcome.response, chunk) {
                        Ok(value) => {
                            self.emit_physical(
                                adapter, request, execution, chunk, &outcome, true, None,
                            );
                            return Ok(value);
                        },
                        Err(error) => {
                            self.emit_physical(
                                adapter,
                                request,
                                execution,
                                chunk,
                                &outcome,
                                false,
                                Some(&error),
                            );
                            recovery_error = error.to_string();
                        },
                    },
                    Err(error @ LogicalChunkError::Cancelled { .. })
                    | Err(error @ LogicalChunkError::DeadlineExceeded { .. }) => {
                        return Err(error);
                    },
                    Err(error) => recovery_error = error.to_string(),
                }
            },
            Ok(None) => {},
            Err(error) => recovery_error = error.to_string(),
        }

        match execution.fallback_policy {
            ChunkFallbackPolicy::SameProviderOnly => Err(LogicalChunkError::RepairFailed {
                context: error_context(
                    request,
                    execution,
                    adapter,
                    Some(PhysicalChunkStage::Repair),
                    Some(chunk.index),
                ),
                reason: recovery_error,
            }),
            ChunkFallbackPolicy::MappedProfile => {
                let fallback_profile = execution.fallback_profile.as_deref().ok_or_else(|| {
                    LogicalChunkError::FallbackFailed {
                        context: error_context(
                            request,
                            execution,
                            adapter,
                            Some(PhysicalChunkStage::Fallback),
                            Some(chunk.index),
                        ),
                        reason: "mapped fallback policy has no fallback profile".to_string(),
                    }
                })?;
                let fallback_request =
                    adapter
                        .render_map_request(request, items, chunk)
                        .map_err(|error| LogicalChunkError::FallbackFailed {
                            context: error_context(
                                request,
                                execution,
                                adapter,
                                Some(PhysicalChunkStage::Fallback),
                                Some(chunk.index),
                            ),
                            reason: error.to_string(),
                        })?;
                state.fallback_calls = state.fallback_calls.saturating_add(1);
                let outcome = match self
                    .dispatch_one(
                        adapter,
                        request,
                        execution,
                        state,
                        fallback_request,
                        fallback_profile,
                        PhysicalChunkStage::Fallback,
                        chunk,
                        0,
                    )
                    .await
                {
                    Ok(outcome) => outcome,
                    Err(error @ LogicalChunkError::Cancelled { .. })
                    | Err(error @ LogicalChunkError::DeadlineExceeded { .. }) => {
                        return Err(error);
                    },
                    Err(error) => {
                        return Err(LogicalChunkError::FallbackFailed {
                            context: error_context(
                                request,
                                execution,
                                adapter,
                                Some(PhysicalChunkStage::Fallback),
                                Some(chunk.index),
                            ),
                            reason: error.to_string(),
                        });
                    },
                };
                match parse_map_response(adapter, &outcome.response, chunk) {
                    Ok(value) => {
                        self.emit_physical(
                            adapter, request, execution, chunk, &outcome, true, None,
                        );
                        Ok(value)
                    },
                    Err(error) => {
                        self.emit_physical(
                            adapter,
                            request,
                            execution,
                            chunk,
                            &outcome,
                            false,
                            Some(&error),
                        );
                        Err(LogicalChunkError::FallbackFailed {
                            context: error_context(
                                request,
                                execution,
                                adapter,
                                Some(PhysicalChunkStage::Fallback),
                                Some(chunk.index),
                            ),
                            reason: error.to_string(),
                        })
                    },
                }
            },
            ChunkFallbackPolicy::Deterministic => {
                let value = adapter
                    .deterministic_fallback(items, chunk, &validation_error)
                    .map_err(|error| LogicalChunkError::FallbackFailed {
                        context: error_context(
                            request,
                            execution,
                            adapter,
                            Some(PhysicalChunkStage::Fallback),
                            Some(chunk.index),
                        ),
                        reason: error.to_string(),
                    })?
                    .ok_or_else(|| LogicalChunkError::FallbackFailed {
                        context: error_context(
                            request,
                            execution,
                            adapter,
                            Some(PhysicalChunkStage::Fallback),
                            Some(chunk.index),
                        ),
                        reason: "adapter has no deterministic fallback".to_string(),
                    })?;
                adapter.validate_map_value(&value, chunk).map_err(|error| {
                    LogicalChunkError::FallbackFailed {
                        context: error_context(
                            request,
                            execution,
                            adapter,
                            Some(PhysicalChunkStage::Fallback),
                            Some(chunk.index),
                        ),
                        reason: error.to_string(),
                    }
                })?;
                Ok(value)
            },
            ChunkFallbackPolicy::Disabled => unreachable!("handled before repair"),
        }
    }

    async fn reduce(
        &self,
        adapter: &dyn ChunkDomainAdapter,
        request: &LogicalLlmRequest,
        execution: &LogicalChunkExecutionContext,
        state: &mut ExecutionState,
        mut outputs: Vec<ValidatedChunkOutput>,
        reduction_context: &ReductionContext,
    ) -> Result<Value, LogicalChunkError> {
        loop {
            ensure_active(request, execution, adapter, state, None, None)?;
            let previous_output_count = outputs.len();
            let previous_work_units = outputs
                .iter()
                .map(|output| output.chunk.items.len())
                .sum::<usize>();
            let plan = adapter
                .reduce(outputs, reduction_context)
                .map_err(|error| LogicalChunkError::ReductionFailed {
                    context: error_context(request, execution, adapter, None, None),
                    reason: error.to_string(),
                })?;
            match plan {
                ReductionPlan::Complete(value) => return Ok(value),
                ReductionPlan::PhysicalRequests { strategy, requests } => {
                    if strategy != ReductionStrategy::HierarchicalLlm || requests.is_empty() {
                        return Err(LogicalChunkError::ReductionFailed {
                            context: error_context(request, execution, adapter, None, None),
                            reason: "physical reduction must be a non-empty hierarchical LLM level"
                                .to_string(),
                        });
                    }
                    if state.reduction_levels >= MAX_REDUCTION_LEVELS {
                        return Err(LogicalChunkError::ReductionFailed {
                            context: error_context(request, execution, adapter, None, None),
                            reason: format!(
                                "hierarchical reduction exceeded {MAX_REDUCTION_LEVELS} levels"
                            ),
                        });
                    }
                    state.reduction_levels = state.reduction_levels.saturating_add(1);
                    let level = state.reduction_levels;
                    let mut next_outputs = Vec::with_capacity(requests.len());
                    for reduction_request in requests {
                        ensure_active(
                            request,
                            execution,
                            adapter,
                            state,
                            Some(PhysicalChunkStage::Reduce),
                            Some(reduction_request.chunk.index),
                        )?;
                        let outcome = self
                            .dispatch_one(
                                adapter,
                                request,
                                execution,
                                state,
                                reduction_request.request.clone(),
                                &execution.primary_profile,
                                PhysicalChunkStage::Reduce,
                                &reduction_request.chunk,
                                level,
                            )
                            .await?;
                        match validate_reduction_response(
                            adapter,
                            &outcome.response,
                            &reduction_request,
                            reduction_context,
                        ) {
                            Ok(value) => {
                                self.emit_physical(
                                    adapter,
                                    request,
                                    execution,
                                    &reduction_request.chunk,
                                    &outcome,
                                    true,
                                    None,
                                );
                                next_outputs.push(ValidatedChunkOutput {
                                    chunk: reduction_request.chunk,
                                    value,
                                    response: Some(outcome.response),
                                });
                            },
                            Err(error) => {
                                self.emit_physical(
                                    adapter,
                                    request,
                                    execution,
                                    &reduction_request.chunk,
                                    &outcome,
                                    false,
                                    Some(&error),
                                );
                                return Err(LogicalChunkError::ReductionFailed {
                                    context: error_context(
                                        request,
                                        execution,
                                        adapter,
                                        Some(PhysicalChunkStage::Reduce),
                                        Some(reduction_request.chunk.index),
                                    ),
                                    reason: error.to_string(),
                                });
                            },
                        }
                    }
                    // A first reduction level may expand one packed map response
                    // into fewer requests than the logical items it contains.
                    // Later levels must strictly reduce physical output count to
                    // prevent a reducer from cycling indefinitely.
                    let convergence_baseline = reduction_convergence_baseline(
                        state.reduction_levels,
                        previous_output_count,
                        previous_work_units,
                    );
                    if next_outputs.len() >= convergence_baseline {
                        return Err(LogicalChunkError::ReductionFailed {
                            context: error_context(request, execution, adapter, None, None),
                            reason: format!(
                                "hierarchical reduction did not converge: {convergence_baseline} work units produced {} outputs",
                                next_outputs.len(),
                            ),
                        });
                    }
                    outputs = next_outputs;
                },
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_one(
        &self,
        adapter: &dyn ChunkDomainAdapter,
        logical_request: &LogicalLlmRequest,
        execution: &LogicalChunkExecutionContext,
        state: &mut ExecutionState,
        physical_request: LLMRequest,
        profile: &str,
        stage: PhysicalChunkStage,
        chunk: &ChunkDescriptor,
        reduction_level: u32,
    ) -> Result<PhysicalOutcome, LogicalChunkError> {
        ensure_active(
            logical_request,
            execution,
            adapter,
            state,
            Some(stage),
            Some(chunk.index),
        )?;
        let request = prepare_child_request(
            physical_request,
            logical_request,
            execution,
            profile,
            stage,
            chunk.index,
            reduction_level,
        )
        .map_err(|source| LogicalChunkError::Adapter {
            context: error_context(
                logical_request,
                execution,
                adapter,
                Some(stage),
                Some(chunk.index),
            ),
            source,
        })?;
        let model = request.model.clone();
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let dispatch_request = LogicalChunkDispatchRequest {
            request,
            // The other production submit path. Left unstamped, every chunked
            // call's physical sub-jobs would be the one family of dispatch
            // rows that cannot be joined back to a live row.
            origin: JobOrigin::op(logical_request.operation.clone())
                .with_caller(execution.caller.clone())
                .with_activity_id(current_activity_id().map(|id| id.to_string())),
            priority: execution.priority,
            task_ref: execution.task_ref.clone(),
            trace_id: execution.trace_id.clone(),
            submission_deadline: execution.deadline,
            cancellation: execution.cancellation.clone(),
            idempotency_key: format!(
                "logical:{}:{}:{}:{}:{}",
                execution.logical_call_id,
                stage.as_str(),
                reduction_level,
                chunk.index,
                profile
            ),
            router_snapshot: execution.router_snapshot.clone(),
        };
        state.physical_call_count = state.physical_call_count.saturating_add(1);
        match self.dispatch.dispatch(dispatch_request).await {
            Ok(dispatched) => {
                let wait = dispatched.wait;
                let execution_duration = dispatched.execution;
                state.queue_wait = state.queue_wait.saturating_add(wait);
                state.provider_execution =
                    state.provider_execution.saturating_add(execution_duration);
                let stage_duration = wait.saturating_add(execution_duration);
                match stage {
                    PhysicalChunkStage::Map => {
                        state.map_duration = state.map_duration.saturating_add(stage_duration)
                    },
                    PhysicalChunkStage::Repair => {
                        state.repair_duration = state.repair_duration.saturating_add(stage_duration)
                    },
                    PhysicalChunkStage::Fallback => {
                        state.fallback_duration =
                            state.fallback_duration.saturating_add(stage_duration)
                    },
                    PhysicalChunkStage::Reduce => {
                        state.reduction_duration =
                            state.reduction_duration.saturating_add(stage_duration)
                    },
                }
                state.usage.add_response(&dispatched.response);
                let attempts = dispatched.trace_receipt.provider_attempt_count;
                let response = dispatched.into_response();
                let trace_receipt = response
                    .trace_receipt
                    .clone()
                    .expect("dispatched response carries its trace receipt");
                Ok(PhysicalOutcome {
                    response,
                    trace_receipt,
                    started_at_ms,
                    profile: profile.to_string(),
                    provider: if profile == execution.primary_profile {
                        Some(execution.primary_provider.clone())
                    } else {
                        execution.fallback_provider.clone()
                    },
                    requested_model: model,
                    estimated_payload_tokens: chunk.estimated_payload_tokens,
                    stage,
                    chunk_index: chunk.index,
                    reduction_level,
                    wait,
                    execution: execution_duration,
                    // `DispatchedResponse::attempts` is the dispatch retry
                    // cycle count. Provider fallbacks can make more than one
                    // physical invocation inside a single cycle, so canonical
                    // physical-call telemetry must use the typed receipt.
                    attempts,
                })
            },
            Err(LLMError::Cancelled { reason }) => Err(LogicalChunkError::Cancelled {
                context: error_context(
                    logical_request,
                    execution,
                    adapter,
                    Some(stage),
                    Some(chunk.index),
                ),
                completed_source_items: state.completed_root_ids.len() as u32,
                reason,
            }),
            Err(LLMError::DeadlineExceeded) => Err(LogicalChunkError::DeadlineExceeded {
                context: error_context(
                    logical_request,
                    execution,
                    adapter,
                    Some(stage),
                    Some(chunk.index),
                ),
                completed_source_items: state.completed_root_ids.len() as u32,
            }),
            Err(source) => Err(LogicalChunkError::Transport {
                context: error_context(
                    logical_request,
                    execution,
                    adapter,
                    Some(stage),
                    Some(chunk.index),
                ),
                source,
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_physical(
        &self,
        adapter: &dyn ChunkDomainAdapter,
        request: &LogicalLlmRequest,
        execution: &LogicalChunkExecutionContext,
        chunk: &ChunkDescriptor,
        outcome: &PhysicalOutcome,
        validation_ok: bool,
        validation_error: Option<&ChunkValidationError>,
    ) {
        self.telemetry
            .emit(LogicalChunkTelemetryEvent::PhysicalCompleted {
                trace_receipt: outcome.trace_receipt.clone(),
                started_at_ms: outcome.started_at_ms,
                logical_call_id: execution.logical_call_id.clone(),
                operation: request.operation.clone(),
                agent_id: execution
                    .task_ref
                    .as_ref()
                    .and_then(|task| task.agent_id.clone()),
                profile: outcome.profile.clone(),
                provider: outcome.provider.clone(),
                adapter: adapter.id().to_string(),
                adapter_version: adapter.version().to_string(),
                stage: outcome.stage,
                chunk_index: outcome.chunk_index,
                reduction_level: outcome.reduction_level,
                item_count: chunk.items.len() as u32,
                source_identity_hash: source_identity_hash(chunk),
                requested_model: outcome.requested_model.clone(),
                estimated_payload_tokens: outcome.estimated_payload_tokens,
                usage: response_usage(&outcome.response),
                usage_reported: outcome.response.usage.is_some(),
                validation_ok,
                validation_error: validation_error.map(|error| bounded(error.to_string())),
                queue_wait_ms: duration_ms(outcome.wait),
                provider_execution_ms: duration_ms(outcome.execution),
                attempts: outcome.attempts,
                billing_authoritative: false,
            });
    }

    fn emit_failure(
        &self,
        request: &LogicalLlmRequest,
        execution: &LogicalChunkExecutionContext,
        state: &ExecutionState,
        started: Instant,
        error: &LogicalChunkError,
    ) {
        self.telemetry
            .emit(LogicalChunkTelemetryEvent::LogicalFailed {
                trace_context: execution.logical_trace_context.clone(),
                started_at_ms: state.started_at_ms,
                logical_call_id: execution.logical_call_id.clone(),
                operation: request.operation.clone(),
                profile: execution.primary_profile.clone(),
                fallback_policy: execution.fallback_policy,
                fallback_profile: execution.fallback_profile.clone(),
                adapter: execution.adapter_id.clone(),
                adapter_version: self
                    .registry
                    .get(&execution.adapter_id)
                    .map(|adapter| adapter.version().to_string()),
                priority: execution.priority.as_str().to_string(),
                task_id: execution.task_ref.as_ref().map(|task| task.task_id.clone()),
                agent_id: execution
                    .task_ref
                    .as_ref()
                    .and_then(|task| task.agent_id.clone()),
                chat_session_id: execution
                    .task_ref
                    .as_ref()
                    .and_then(|task| task.chat_session_id.clone()),
                trace_id: execution.trace_id.clone(),
                caller: execution.caller.clone(),
                deadline_present: execution.deadline.is_some(),
                completed_source_items: state.completed_root_ids.len() as u32,
                physical_call_count: state.physical_call_count,
                usage: state.usage.clone(),
                total_duration_ms: duration_ms(started.elapsed()),
                error_class: error.class().to_string(),
                error: bounded(error.to_string()),
                summary_incremental_cost_usd: 0.0,
            });
    }
}

#[derive(Default)]
struct ExecutionState {
    started_at_ms: i64,
    planning_duration: Duration,
    queue_wait: Duration,
    provider_execution: Duration,
    usage: AggregateTokenUsage,
    completed_root_ids: HashSet<String>,
    physical_call_count: u32,
    reduction_levels: u32,
    local_repairs: u32,
    fallback_calls: u32,
    map_duration: Duration,
    repair_duration: Duration,
    fallback_duration: Duration,
    reduction_duration: Duration,
}

struct PhysicalOutcome {
    response: LLMResponse,
    trace_receipt: magicllm::LlmTraceReceipt,
    started_at_ms: i64,
    profile: String,
    provider: Option<String>,
    requested_model: String,
    estimated_payload_tokens: u32,
    stage: PhysicalChunkStage,
    chunk_index: u32,
    reduction_level: u32,
    wait: Duration,
    execution: Duration,
    attempts: u32,
}

fn reduction_convergence_baseline(
    reduction_level: u32,
    previous_output_count: usize,
    previous_work_units: usize,
) -> usize {
    if reduction_level == 1 {
        previous_work_units.max(previous_output_count)
    } else {
        previous_output_count
    }
}

fn parse_map_response(
    adapter: &dyn ChunkDomainAdapter,
    response: &LLMResponse,
    chunk: &ChunkDescriptor,
) -> Result<Value, ChunkValidationError> {
    reject_truncated(response, adapter.id())?;
    let value = adapter.parse_and_validate_map_output(response, chunk)?;
    adapter.validate_map_value(&value, chunk)?;
    Ok(value)
}

fn validate_reduction_response(
    adapter: &dyn ChunkDomainAdapter,
    response: &LLMResponse,
    request: &magicllm::ReductionRequest,
    context: &ReductionContext,
) -> Result<Value, ChunkValidationError> {
    reject_truncated(response, adapter.id())?;
    adapter.parse_and_validate_reduction_output(response, request, context)
}

fn reject_truncated(response: &LLMResponse, adapter_id: &str) -> Result<(), ChunkValidationError> {
    let truncated = response
        .finish_reason
        .as_deref()
        .map(|reason| {
            matches!(
                reason.to_ascii_lowercase().as_str(),
                "length" | "max_tokens" | "max_output_tokens" | "token_limit"
            )
        })
        .unwrap_or(false);
    if truncated {
        return Err(ChunkValidationError::InvalidOutput {
            adapter: adapter_id.to_string(),
            reason: format!(
                "physical response truncated at finish_reason `{}`",
                response.finish_reason.as_deref().unwrap_or_default()
            ),
        });
    }
    Ok(())
}

fn prepare_child_request(
    mut request: LLMRequest,
    logical_request: &LogicalLlmRequest,
    execution: &LogicalChunkExecutionContext,
    profile: &str,
    stage: PhysicalChunkStage,
    chunk_index: u32,
    reduction_level: u32,
) -> Result<LLMRequest, ChunkError> {
    request.metadata.operation = logical_request.operation.clone();
    let relation = match stage {
        PhysicalChunkStage::Map => magicllm::LlmParentRelation::ChunkMap,
        PhysicalChunkStage::Repair => magicllm::LlmParentRelation::ChunkRepair,
        PhysicalChunkStage::Fallback => magicllm::LlmParentRelation::ChunkFallback,
        PhysicalChunkStage::Reduce => magicllm::LlmParentRelation::ChunkReduce,
    };
    let role = match stage {
        PhysicalChunkStage::Map => magicllm::LlmCallRole::Supporting,
        PhysicalChunkStage::Repair | PhysicalChunkStage::Fallback => {
            magicllm::LlmCallRole::Recovery
        },
        PhysicalChunkStage::Reduce => magicllm::LlmCallRole::Summarizer,
    };
    let parent = execution.logical_trace_context.clone().unwrap_or_else(|| {
        let mut context = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::legacy_default(),
            magicllm::LlmWorkloadClass::System,
        )
        .with_scope_resolution(magicllm::LlmScopeResolution::SystemDefault);
        if let Some(trace_id) = execution.trace_id.as_ref() {
            context.trace_id = trace_id.clone();
        }
        context.llm_call_id = execution.logical_call_id.clone();
        context
    });
    let child = parent.child(relation, role);
    request.metadata.trace_id = Some(child.trace_id.clone());
    request.metadata.set_trace_context(child);
    if execution.disable_reasoning {
        request.reasoning = Some(ReasoningConfig {
            effort: Some("off".to_string()),
            ..ReasoningConfig::default()
        });
    }
    let tags = request.metadata.tags.get_or_insert_with(Vec::new);
    for tag in [
        "logical_chunk_child".to_string(),
        format!("logical_call:{}", execution.logical_call_id),
        format!("logical_stage:{}", stage.as_str()),
        format!("logical_chunk:{chunk_index}"),
        format!("logical_reduction_level:{reduction_level}"),
    ] {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }

    let mut extras = match request.take_extra_value() {
        Some(Value::Object(map)) => map,
        Some(_) => {
            return Err(ChunkError::Adapter {
                adapter: execution.adapter_id.clone(),
                reason: "physical request extra must be a JSON object".to_string(),
            })
        },
        None => Map::new(),
    };
    if execution.lock_primary_profile {
        extras.insert(
            "router_profile_override".to_string(),
            Value::String(profile.to_string()),
        );
    }
    if profile == execution.primary_profile {
        extras.insert(
            "router_required_provider_kind".to_string(),
            Value::String(execution.primary_provider.clone()),
        );
    }
    request.set_extra(Value::Object(extras));
    Ok(request)
}

fn items_for_chunk(
    chunk: &ChunkDescriptor,
    leaf_items: &HashMap<&str, &LogicalItem>,
) -> Result<Vec<LogicalItem>, ChunkError> {
    chunk
        .items
        .iter()
        .map(|identity| {
            leaf_items
                .get(identity.id.as_str())
                .map(|item| (*item).clone())
                .ok_or_else(|| ChunkError::MissingPlannedItems {
                    item_ids: vec![identity.id.clone()],
                })
        })
        .collect()
}

fn ensure_active(
    request: &LogicalLlmRequest,
    execution: &LogicalChunkExecutionContext,
    adapter: &dyn ChunkDomainAdapter,
    state: &ExecutionState,
    stage: Option<PhysicalChunkStage>,
    chunk_index: Option<u32>,
) -> Result<(), LogicalChunkError> {
    let context = error_context(request, execution, adapter, stage, chunk_index);
    if execution.cancellation.is_cancelled() {
        return Err(LogicalChunkError::Cancelled {
            context,
            completed_source_items: state.completed_root_ids.len() as u32,
            reason: "logical cancellation token fired".to_string(),
        });
    }
    if execution
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Err(LogicalChunkError::DeadlineExceeded {
            context,
            completed_source_items: state.completed_root_ids.len() as u32,
        });
    }
    Ok(())
}

fn error_context(
    request: &LogicalLlmRequest,
    execution: &LogicalChunkExecutionContext,
    adapter: &dyn ChunkDomainAdapter,
    stage: Option<PhysicalChunkStage>,
    chunk_index: Option<u32>,
) -> LogicalChunkErrorContext {
    let profile = if stage == Some(PhysicalChunkStage::Fallback) {
        execution
            .fallback_profile
            .as_deref()
            .unwrap_or(&execution.primary_profile)
    } else {
        &execution.primary_profile
    };
    LogicalChunkErrorContext {
        logical_call_id: execution.logical_call_id.clone(),
        operation: request.operation.clone(),
        profile: profile.to_string(),
        adapter: adapter.id().to_string(),
        adapter_version: adapter.version().to_string(),
        stage,
        chunk_index,
    }
}

fn mark_completed_sources(state: &mut ExecutionState, chunk: &ChunkDescriptor) {
    state
        .completed_root_ids
        .extend(chunk.items.iter().map(|identity| identity.root_id.clone()));
}

fn build_metadata(
    request: &LogicalLlmRequest,
    execution: &LogicalChunkExecutionContext,
    adapter: &dyn ChunkDomainAdapter,
    plan: &ChunkPlan,
    state: &ExecutionState,
    total_duration: Duration,
) -> LogicalChunkMetadata {
    LogicalChunkMetadata {
        applied: true,
        logical_call_id: execution.logical_call_id.clone(),
        operation: request.operation.clone(),
        profile: execution.primary_profile.clone(),
        fallback_policy: execution.fallback_policy,
        fallback_profile: execution.fallback_profile.clone(),
        adapter: adapter.id().to_string(),
        adapter_version: adapter.version().to_string(),
        estimated_logical_tokens: plan.estimated_logical_tokens,
        logical_window_tokens: plan.budget.logical_window_tokens,
        physical_window_tokens: plan.budget.physical_window_tokens,
        target_payload_tokens: plan.budget.target_payload_tokens,
        effective_payload_tokens: plan.budget.effective_payload_tokens,
        chunk_count: plan.chunks.len() as u32,
        source_item_count: plan.source_identities.len() as u32,
        terminal_item_count: plan.leaf_identities.len() as u32,
        oversized_split_count: plan
            .leaf_identities
            .len()
            .saturating_sub(plan.source_identities.len()) as u32,
        completed_source_items: state.completed_root_ids.len() as u32,
        physical_call_count: state.physical_call_count,
        reduction_levels: state.reduction_levels,
        local_repairs: state.local_repairs,
        fallback_calls: state.fallback_calls,
        priority: execution.priority.as_str().to_string(),
        task_id: execution.task_ref.as_ref().map(|task| task.task_id.clone()),
        agent_id: execution
            .task_ref
            .as_ref()
            .and_then(|task| task.agent_id.clone()),
        chat_session_id: execution
            .task_ref
            .as_ref()
            .and_then(|task| task.chat_session_id.clone()),
        trace_id: execution.trace_id.clone(),
        caller: execution.caller.clone(),
        deadline_present: execution.deadline.is_some(),
        planning_ms: duration_ms(state.planning_duration),
        queue_wait_ms: duration_ms(state.queue_wait),
        provider_execution_ms: duration_ms(state.provider_execution),
        map_duration_ms: duration_ms(state.map_duration),
        repair_duration_ms: duration_ms(state.repair_duration),
        fallback_duration_ms: duration_ms(state.fallback_duration),
        reduction_duration_ms: duration_ms(state.reduction_duration),
        total_duration_ms: duration_ms(total_duration),
        usage: state.usage.clone(),
        summary_incremental_cost_usd: 0.0,
        summary_only: true,
    }
}

fn source_identity_hash(chunk: &ChunkDescriptor) -> String {
    let mut hasher = Sha256::new();
    for identity in &chunk.items {
        hasher.update(identity.id.as_bytes());
        hasher.update([0]);
        hasher.update(identity.root_id.as_bytes());
        hasher.update([0xff]);
    }
    format!("{:x}", hasher.finalize())
}

fn bounded(value: String) -> String {
    value.chars().take(MAX_TELEMETRY_ERROR_CHARS).collect()
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn response_usage(response: &LLMResponse) -> AggregateTokenUsage {
    let mut usage = AggregateTokenUsage::default();
    usage.add_response(response);
    usage
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        collections::{HashSet, VecDeque},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    use async_trait::async_trait;
    use magicllm::{
        ChunkBudget, ChunkDescriptor, ChunkDomainAdapter, ChunkError, ChunkFallbackPolicy,
        ChunkValidationError, DispatchedResponse, FinalValidationContract, LLMError, LLMMessage,
        LLMRequest, LLMResponse, LogicalItem, LogicalLlmRequest, Priority, ReductionContext,
        ReductionPlan, ReductionRequest, ReductionStrategy, TaskRef, TokenEstimate, TokenEstimator,
        TokenUsage, ValidatedChunkOutput,
    };
    use parking_lot::Mutex;
    use serde_json::{json, Value};
    use tokio_util::sync::CancellationToken;

    use super::{
        reduction_convergence_baseline, LogicalChunkDispatch, LogicalChunkDispatchRequest,
        LogicalChunkError, LogicalChunkExecutionContext, LogicalChunkRunner,
        LogicalChunkTelemetryEvent, LogicalChunkTelemetrySink,
    };
    use crate::magician_v2::llm_chunking::ChunkDomainAdapterRegistry;

    #[derive(Clone)]
    struct RecordedDispatch {
        request: LLMRequest,
        priority: Priority,
        task_ref: Option<TaskRef>,
        trace_id: Option<String>,
        has_deadline: bool,
        idempotency_key: String,
    }

    struct ScriptedDispatch {
        outcomes: Mutex<VecDeque<DispatchedResponse>>,
        recorded: Mutex<Vec<RecordedDispatch>>,
        active: AtomicUsize,
        max_active: AtomicUsize,
        call_count: AtomicUsize,
        cancel_after_call: Option<(usize, CancellationToken)>,
    }

    impl ScriptedDispatch {
        fn new(outcomes: Vec<DispatchedResponse>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                recorded: Mutex::new(Vec::new()),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                call_count: AtomicUsize::new(0),
                cancel_after_call: None,
            }
        }

        fn cancelling_after(
            outcomes: Vec<DispatchedResponse>,
            call: usize,
            cancellation: CancellationToken,
        ) -> Self {
            let mut dispatch = Self::new(outcomes);
            dispatch.cancel_after_call = Some((call, cancellation));
            dispatch
        }

        fn calls(&self) -> Vec<RecordedDispatch> {
            self.recorded.lock().clone()
        }
    }

    #[async_trait]
    impl LogicalChunkDispatch for ScriptedDispatch {
        async fn dispatch(
            &self,
            request: LogicalChunkDispatchRequest,
        ) -> Result<DispatchedResponse, LLMError> {
            self.recorded.lock().push(RecordedDispatch {
                request: request.request,
                priority: request.priority,
                task_ref: request.task_ref,
                trace_id: request.trace_id,
                has_deadline: request.submission_deadline.is_some(),
                idempotency_key: request.idempotency_key,
            });
            let call = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::task::yield_now().await;
            self.active.fetch_sub(1, Ordering::SeqCst);

            let outcome = self.outcomes.lock().pop_front().ok_or_else(|| {
                LLMError::Configuration("scripted chunk dispatch exhausted".to_string())
            })?;
            if let Some((cancel_call, cancellation)) = &self.cancel_after_call {
                if call == *cancel_call {
                    cancellation.cancel();
                }
            }
            Ok(outcome)
        }
    }

    #[derive(Default)]
    struct RecordingTelemetry {
        events: Mutex<Vec<LogicalChunkTelemetryEvent>>,
    }

    impl RecordingTelemetry {
        fn events(&self) -> Vec<LogicalChunkTelemetryEvent> {
            self.events.lock().clone()
        }
    }

    impl LogicalChunkTelemetrySink for RecordingTelemetry {
        fn emit(&self, event: LogicalChunkTelemetryEvent) {
            self.events.lock().push(event);
        }
    }

    #[derive(Clone, Copy)]
    struct FakeAdapter {
        repair: bool,
        hierarchical_reduction: bool,
        deterministic_fallback: bool,
    }

    impl Default for FakeAdapter {
        fn default() -> Self {
            Self {
                repair: false,
                hierarchical_reduction: false,
                deterministic_fallback: false,
            }
        }
    }

    impl ChunkDomainAdapter for FakeAdapter {
        fn id(&self) -> &'static str {
            "phase3_fake_v1"
        }

        fn version(&self) -> &'static str {
            "1"
        }

        fn supported_operations(&self) -> &'static [&'static str] {
            &["phase3_test"]
        }

        fn final_validation_contract(&self) -> FinalValidationContract {
            FinalValidationContract::Available
        }

        fn logical_items(&self, input: &Value) -> Result<Vec<LogicalItem>, ChunkError> {
            let items = input
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| ChunkError::Adapter {
                    adapter: self.id().to_string(),
                    reason: "test input requires an items array".to_string(),
                })?;
            items
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let id = value.get("id").and_then(Value::as_str).ok_or_else(|| {
                        ChunkError::Adapter {
                            adapter: self.id().to_string(),
                            reason: "test item requires an id".to_string(),
                        }
                    })?;
                    Ok(LogicalItem::root(id, index as u32, value.clone()))
                })
                .collect()
        }

        fn estimate_item_tokens(
            &self,
            item: &LogicalItem,
            _estimator: &dyn TokenEstimator,
            _model: &str,
        ) -> Result<u32, ChunkError> {
            item.value
                .get("tokens")
                .and_then(Value::as_u64)
                .and_then(|tokens| u32::try_from(tokens).ok())
                .ok_or_else(|| ChunkError::Adapter {
                    adapter: self.id().to_string(),
                    reason: "test item requires u32 tokens".to_string(),
                })
        }

        fn split_oversized_item(
            &self,
            item: &LogicalItem,
            budget: &ChunkBudget,
        ) -> Result<Vec<LogicalItem>, ChunkError> {
            Err(ChunkError::ChunkItemExceedsContextWindow {
                adapter: self.id().to_string(),
                item_id: item.identity.id.clone(),
                estimated_tokens: budget.effective_payload_tokens.saturating_add(1),
                effective_payload_tokens: budget.effective_payload_tokens,
            })
        }

        fn render_map_request(
            &self,
            base: &LogicalLlmRequest,
            items: &[LogicalItem],
            _chunk: &ChunkDescriptor,
        ) -> Result<LLMRequest, ChunkError> {
            Ok(LLMRequest {
                model: base.base_request.model.clone(),
                messages: vec![LLMMessage::user(
                    json!({
                        "items": items.iter().map(|item| item.identity.id.as_str()).collect::<Vec<_>>()
                    })
                    .to_string(),
                )].into(),
                ..LLMRequest::default()
            })
        }

        fn parse_and_validate_map_output(
            &self,
            response: &LLMResponse,
            _chunk: &ChunkDescriptor,
        ) -> Result<Value, ChunkValidationError> {
            serde_json::from_str(response.text.as_deref().unwrap_or_default()).map_err(|error| {
                ChunkValidationError::InvalidOutput {
                    adapter: self.id().to_string(),
                    reason: format!("invalid JSON: {error}"),
                }
            })
        }

        fn validate_map_value(
            &self,
            value: &Value,
            _chunk: &ChunkDescriptor,
        ) -> Result<(), ChunkValidationError> {
            if value.get("id").and_then(Value::as_str).is_none() {
                return Err(ChunkValidationError::InvalidOutput {
                    adapter: self.id().to_string(),
                    reason: "map output requires string id".to_string(),
                });
            }
            Ok(())
        }

        fn render_repair_request(
            &self,
            base: &LogicalLlmRequest,
            items: &[LogicalItem],
            chunk: &ChunkDescriptor,
            _invalid_response: &LLMResponse,
            _validation_error: &ChunkValidationError,
        ) -> Result<Option<LLMRequest>, ChunkError> {
            if self.repair {
                self.render_map_request(base, items, chunk).map(Some)
            } else {
                Ok(None)
            }
        }

        fn deterministic_fallback(
            &self,
            items: &[LogicalItem],
            _chunk: &ChunkDescriptor,
            _validation_error: &ChunkValidationError,
        ) -> Result<Option<Value>, ChunkError> {
            Ok(self.deterministic_fallback.then(|| {
                json!({
                    "id": items.first().map(|item| item.identity.id.as_str()).unwrap_or("empty"),
                    "deterministic_fallback": true
                })
            }))
        }

        fn reduce(
            &self,
            outputs: Vec<ValidatedChunkOutput>,
            _context: &ReductionContext,
        ) -> Result<ReductionPlan, ChunkError> {
            if self.hierarchical_reduction && outputs.len() > 1 {
                let descriptor = ChunkDescriptor {
                    index: 10_000,
                    estimated_payload_tokens: outputs
                        .iter()
                        .map(|output| output.chunk.estimated_payload_tokens)
                        .sum(),
                    items: outputs
                        .iter()
                        .flat_map(|output| output.chunk.items.clone())
                        .collect(),
                };
                return Ok(ReductionPlan::PhysicalRequests {
                    strategy: ReductionStrategy::HierarchicalLlm,
                    requests: vec![ReductionRequest {
                        chunk: descriptor,
                        request: LLMRequest {
                            model: "gemma4:12b".to_string(),
                            messages: vec![LLMMessage::user("reduce")].into(),
                            ..LLMRequest::default()
                        },
                    }],
                });
            }
            if self.hierarchical_reduction {
                return Ok(ReductionPlan::Complete(
                    outputs
                        .into_iter()
                        .next()
                        .map(|output| output.value)
                        .unwrap_or(Value::Null),
                ));
            }
            Ok(ReductionPlan::Complete(json!({
                "results": outputs.into_iter().map(|output| output.value).collect::<Vec<_>>()
            })))
        }

        fn parse_and_validate_reduction_output(
            &self,
            response: &LLMResponse,
            _request: &ReductionRequest,
            _context: &ReductionContext,
        ) -> Result<Value, ChunkValidationError> {
            let value: Value = serde_json::from_str(response.text.as_deref().unwrap_or_default())
                .map_err(|error| ChunkValidationError::InvalidOutput {
                adapter: self.id().to_string(),
                reason: format!("invalid reduction JSON: {error}"),
            })?;
            if value.get("reduced") != Some(&Value::Bool(true)) {
                return Err(ChunkValidationError::InvalidOutput {
                    adapter: self.id().to_string(),
                    reason: "reduction output requires reduced=true".to_string(),
                });
            }
            Ok(value)
        }

        fn validate_final(&self, value: &Value) -> Result<(), ChunkValidationError> {
            if value.is_object() {
                Ok(())
            } else {
                Err(ChunkValidationError::InvalidOutput {
                    adapter: self.id().to_string(),
                    reason: "final value must be an object".to_string(),
                })
            }
        }
    }

    struct FixedEstimator;

    impl TokenEstimator for FixedEstimator {
        fn id(&self) -> &'static str {
            "phase3_test_estimator"
        }

        fn estimate_text(&self, _model: &str, text: &str) -> u32 {
            text.len() as u32
        }

        fn estimate_request(&self, _model: &str, request: &LLMRequest) -> TokenEstimate {
            TokenEstimate {
                estimated_tokens: request.messages.len() as u32,
                source_bytes: request.messages.len() as u64,
                estimator: self.id(),
            }
        }
    }

    fn logical_request() -> LogicalLlmRequest {
        LogicalLlmRequest {
            operation: "phase3_test".to_string(),
            input: json!({
                "items": [
                    {"id": "source-a", "tokens": 600},
                    {"id": "source-b", "tokens": 600}
                ]
            }),
            base_request: LLMRequest {
                model: "gemma4:12b".to_string(),
                ..LLMRequest::default()
            },
        }
    }

    fn budget() -> ChunkBudget {
        ChunkBudget::new(1_000, 5_000, 800, 50, 50, 50).expect("valid test budget")
    }

    fn response(text: &str, prompt_tokens: u32, completion_tokens: u32) -> DispatchedResponse {
        let trace_receipt = magicllm::LlmTraceReceipt::direct(magicllm::LlmTraceContext::legacy(
            None,
            magicllm::LlmWorkloadClass::Evaluation,
        ));
        DispatchedResponse {
            response: Arc::new(LLMResponse {
                text: Some(Arc::<str>::from(text)),
                finish_reason: Some("stop".to_string()),
                usage: Some(TokenUsage {
                    prompt_tokens: Some(prompt_tokens),
                    completion_tokens: Some(completion_tokens),
                    total_tokens: Some(prompt_tokens + completion_tokens),
                    cached_tokens: Some(1),
                    reasoning_tokens: Some(0),
                    cache_creation_tokens: Some(0),
                }),
                trace_receipt: Some(trace_receipt.clone()),
                ..LLMResponse::default()
            }),
            wait: Duration::from_millis(3),
            execution: Duration::from_millis(7),
            local_prep: None,
            attempts: 1,
            trace_receipt,
        }
    }

    fn response_with_physical_attempts(
        text: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        attempts: u32,
    ) -> DispatchedResponse {
        let mut dispatched = response(text, prompt_tokens, completion_tokens);
        let receipt = magicllm::LlmTraceReceipt::direct_with_attempt_count(
            dispatched.trace_receipt.context.clone(),
            attempts,
        );
        Arc::make_mut(&mut dispatched.response).trace_receipt = Some(receipt.clone());
        dispatched.trace_receipt = receipt;
        dispatched
    }

    fn runner(
        adapter: FakeAdapter,
        dispatch: Arc<ScriptedDispatch>,
        telemetry: Arc<RecordingTelemetry>,
    ) -> LogicalChunkRunner {
        let mut registry = ChunkDomainAdapterRegistry::new();
        registry
            .register(Arc::new(adapter))
            .expect("register fake adapter");
        LogicalChunkRunner::new(dispatch, registry, Arc::new(FixedEstimator))
            .with_telemetry(telemetry)
    }

    fn execution_context() -> LogicalChunkExecutionContext {
        let mut execution = LogicalChunkExecutionContext::new("phase3_fake_v1", "local-chunk");
        execution.priority = Priority::High;
        execution.task_ref = Some(TaskRef::task("task-1").with_agent("agent-1"));
        execution.trace_id = Some("trace-1".to_string());
        let mut logical_trace = magicllm::LlmTraceContext::legacy(
            execution.trace_id.as_deref(),
            magicllm::LlmWorkloadClass::Evaluation,
        );
        logical_trace.llm_call_id = execution.logical_call_id.clone();
        logical_trace.task_id = Some("task-1".to_string());
        execution.logical_trace_context = Some(logical_trace);
        execution.deadline = Some(Instant::now() + Duration::from_secs(30));
        execution.caller = "phase3_test".to_string();
        execution
    }

    fn profile_override(request: &LLMRequest) -> Option<&str> {
        request
            .extra
            .as_ref()
            .and_then(|extra| extra.get("router_profile_override"))
            .and_then(Value::as_str)
    }

    fn required_provider(request: &LLMRequest) -> Option<&str> {
        request
            .extra
            .as_ref()
            .and_then(|extra| extra.get("router_required_provider_kind"))
            .and_then(Value::as_str)
    }

    #[tokio::test]
    async fn phase3_runner_dispatches_chunks_sequentially_and_returns_one_aggregate_response() {
        let dispatch = Arc::new(ScriptedDispatch::new(vec![
            response(r#"{"id":"source-a"}"#, 10, 2),
            response(r#"{"id":"source-b"}"#, 20, 3),
        ]));
        let telemetry = Arc::new(RecordingTelemetry::default());
        let runner = runner(FakeAdapter::default(), dispatch.clone(), telemetry.clone());
        let execution = execution_context();
        let logical_call_id = execution.logical_call_id.clone();

        let result = runner
            .execute(logical_request(), budget(), execution)
            .await
            .expect("logical execution succeeds");

        assert_eq!(dispatch.max_active.load(Ordering::SeqCst), 1);
        let calls = dispatch.calls();
        assert_eq!(calls.len(), 2);
        let mut child_call_ids = HashSet::new();
        for call in &calls {
            assert_eq!(profile_override(&call.request), Some("local-chunk"));
            assert_eq!(required_provider(&call.request), Some("ollama"));
            assert!(call
                .request
                .reasoning
                .as_ref()
                .is_some_and(|r| r.is_disabled()));
            assert!(call
                .request
                .metadata
                .tags
                .as_ref()
                .is_some_and(|tags| { tags.iter().any(|tag| tag == "logical_chunk_child") }));
            assert_eq!(call.priority, Priority::High);
            assert_eq!(
                call.task_ref.as_ref().map(|task| task.task_id.as_str()),
                Some("task-1")
            );
            assert_eq!(call.trace_id.as_deref(), Some("trace-1"));
            let trace = call
                .request
                .metadata
                .trace_context
                .as_ref()
                .expect("physical child trace");
            assert_eq!(trace.trace_id, "trace-1");
            assert_eq!(
                trace.parent_call_id.as_deref(),
                Some(logical_call_id.as_str())
            );
            assert_eq!(
                trace.parent_relation,
                Some(magicllm::LlmParentRelation::ChunkMap)
            );
            assert_eq!(trace.call_role, magicllm::LlmCallRole::Supporting);
            assert!(child_call_ids.insert(trace.llm_call_id.clone()));
            assert!(call.has_deadline);
            assert!(call.idempotency_key.starts_with("logical:"));
        }
        let usage = result.usage.expect("aggregate usage");
        assert_eq!(usage.prompt_tokens, Some(30));
        assert_eq!(usage.completion_tokens, Some(5));
        assert_eq!(usage.total_tokens, Some(35));
        assert_eq!(usage.cached_tokens, Some(2));
        assert_eq!(result.finish_reason.as_deref(), Some("logical_complete"));

        let events = telemetry.events();
        assert_eq!(events.len(), 3);
        let metadata = events
            .into_iter()
            .find_map(|event| match event {
                LogicalChunkTelemetryEvent::LogicalCompleted { metadata, .. } => Some(metadata),
                _ => None,
            })
            .expect("logical completion event");
        assert_eq!(metadata.chunk_count, 2);
        assert_eq!(metadata.completed_source_items, 2);
        assert_eq!(metadata.physical_call_count, 2);
        assert_eq!(metadata.summary_incremental_cost_usd, 0.0);
        assert!(metadata.summary_only);
    }

    #[tokio::test]
    async fn phase3_runner_allows_exactly_one_local_repair() {
        let dispatch = Arc::new(ScriptedDispatch::new(vec![
            response("not-json", 5, 1),
            response("still-not-json", 5, 1),
        ]));
        let telemetry = Arc::new(RecordingTelemetry::default());
        let runner = runner(
            FakeAdapter {
                repair: true,
                ..FakeAdapter::default()
            },
            dispatch.clone(),
            telemetry,
        );

        let error = runner
            .execute(logical_request(), budget(), execution_context())
            .await
            .expect_err("second invalid output must fail without another repair");

        assert!(matches!(error, LogicalChunkError::RepairFailed { .. }));
        let calls = dispatch.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].idempotency_key.contains(":map:"));
        assert!(calls[1].idempotency_key.contains(":repair:"));
    }

    #[tokio::test]
    async fn phase3_runner_stops_between_chunks_when_cancelled() {
        let execution = execution_context();
        let cancellation = execution.cancellation.clone();
        let dispatch = Arc::new(ScriptedDispatch::cancelling_after(
            vec![response(r#"{"id":"source-a"}"#, 10, 2)],
            1,
            cancellation,
        ));
        let telemetry = Arc::new(RecordingTelemetry::default());
        let runner = runner(FakeAdapter::default(), dispatch.clone(), telemetry.clone());

        let error = runner
            .execute(logical_request(), budget(), execution)
            .await
            .expect_err("logical execution is cancelled before the second chunk");

        assert!(matches!(
            error,
            LogicalChunkError::Cancelled {
                completed_source_items: 1,
                ..
            }
        ));
        assert_eq!(dispatch.calls().len(), 1);
        assert!(telemetry.events().iter().any(|event| matches!(
            event,
            LogicalChunkTelemetryEvent::LogicalFailed {
                completed_source_items: 1,
                physical_call_count: 1,
                summary_incremental_cost_usd: 0.0,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn phase3_runner_rejects_expired_deadline_without_dispatch() {
        let dispatch = Arc::new(ScriptedDispatch::new(Vec::new()));
        let telemetry = Arc::new(RecordingTelemetry::default());
        let runner = runner(FakeAdapter::default(), dispatch.clone(), telemetry);
        let mut execution = execution_context();
        execution.deadline = Some(Instant::now() - Duration::from_millis(1));

        let error = runner
            .execute(logical_request(), budget(), execution)
            .await
            .expect_err("expired logical deadline fails before planning/dispatch");

        assert!(matches!(error, LogicalChunkError::DeadlineExceeded { .. }));
        assert!(dispatch.calls().is_empty());
    }

    #[tokio::test]
    async fn phase3_runner_uses_exact_mapped_fallback_without_recursive_ollama_lock() {
        let dispatch = Arc::new(ScriptedDispatch::new(vec![
            response("not-json", 10, 1),
            response(r#"{"id":"source-a","fallback":true}"#, 12, 2),
            response(r#"{"id":"source-b"}"#, 10, 2),
        ]));
        let telemetry = Arc::new(RecordingTelemetry::default());
        let runner = runner(FakeAdapter::default(), dispatch.clone(), telemetry.clone());
        let mut execution = execution_context();
        execution.fallback_policy = ChunkFallbackPolicy::MappedProfile;
        execution.fallback_profile = Some("remote-fallback".to_string());
        execution.fallback_provider = Some("openai".to_string());

        runner
            .execute(logical_request(), budget(), execution)
            .await
            .expect("mapped fallback recovers one invalid map");

        let calls = dispatch.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(profile_override(&calls[0].request), Some("local-chunk"));
        assert_eq!(profile_override(&calls[1].request), Some("remote-fallback"));
        assert_eq!(required_provider(&calls[1].request), None);
        assert_eq!(profile_override(&calls[2].request), Some("local-chunk"));
        let events = telemetry.events();
        assert!(events.iter().any(|event| matches!(
            event,
            LogicalChunkTelemetryEvent::PhysicalCompleted {
                profile,
                provider: Some(provider),
                ..
            } if profile == "remote-fallback" && provider == "openai"
        )));
        let metadata = events
            .into_iter()
            .find_map(|event| match event {
                LogicalChunkTelemetryEvent::LogicalCompleted { metadata, .. } => Some(metadata),
                _ => None,
            })
            .expect("logical completion event");
        assert_eq!(metadata.fallback_calls, 1);
        assert_eq!(metadata.local_repairs, 0);
    }

    #[tokio::test]
    async fn physical_telemetry_counts_provider_fallback_hops_not_dispatch_cycles() {
        let dispatch = Arc::new(ScriptedDispatch::new(vec![
            response_with_physical_attempts(r#"{"id":"source-a"}"#, 10, 2, 2),
            response_with_physical_attempts(r#"{"id":"source-b"}"#, 10, 2, 2),
        ]));
        let telemetry = Arc::new(RecordingTelemetry::default());
        runner(FakeAdapter::default(), dispatch, telemetry.clone())
            .execute(logical_request(), budget(), execution_context())
            .await
            .expect("logical execution");

        let attempts = telemetry
            .events()
            .into_iter()
            .filter_map(|event| match event {
                LogicalChunkTelemetryEvent::PhysicalCompleted { attempts, .. } => Some(attempts),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!attempts.is_empty());
        assert!(attempts.into_iter().all(|attempts| attempts == 2));
    }

    #[tokio::test]
    async fn phase3_runner_executes_hierarchical_reduction_as_sequential_child_work() {
        let dispatch = Arc::new(ScriptedDispatch::new(vec![
            response(r#"{"id":"source-a"}"#, 10, 2),
            response(r#"{"id":"source-b"}"#, 10, 2),
            response(r#"{"reduced":true,"ids":["source-a","source-b"]}"#, 15, 3),
        ]));
        let telemetry = Arc::new(RecordingTelemetry::default());
        let runner = runner(
            FakeAdapter {
                hierarchical_reduction: true,
                ..FakeAdapter::default()
            },
            dispatch.clone(),
            telemetry.clone(),
        );

        let result = runner
            .execute(logical_request(), budget(), execution_context())
            .await
            .expect("hierarchical reduction succeeds");

        assert!(result
            .text
            .as_deref()
            .is_some_and(|text| text.contains("reduced")));
        let calls = dispatch.calls();
        assert_eq!(calls.len(), 3);
        assert!(calls[2].idempotency_key.contains(":reduce:1:"));
        assert_eq!(dispatch.max_active.load(Ordering::SeqCst), 1);
        let metadata = telemetry
            .events()
            .into_iter()
            .find_map(|event| match event {
                LogicalChunkTelemetryEvent::LogicalCompleted { metadata, .. } => Some(metadata),
                _ => None,
            })
            .expect("logical completion event");
        assert_eq!(metadata.reduction_levels, 1);
        assert_eq!(metadata.physical_call_count, 3);
    }

    #[tokio::test]
    async fn phase3_runner_supports_non_llm_deterministic_fallback_without_extra_billing_row() {
        let dispatch = Arc::new(ScriptedDispatch::new(vec![
            response("not-json", 10, 1),
            response(r#"{"id":"source-b"}"#, 10, 2),
        ]));
        let telemetry = Arc::new(RecordingTelemetry::default());
        let runner = runner(
            FakeAdapter {
                deterministic_fallback: true,
                ..FakeAdapter::default()
            },
            dispatch.clone(),
            telemetry.clone(),
        );
        let mut execution = execution_context();
        execution.fallback_policy = ChunkFallbackPolicy::Deterministic;

        runner
            .execute(logical_request(), budget(), execution)
            .await
            .expect("deterministic fallback succeeds");

        assert_eq!(dispatch.calls().len(), 2);
        let events = telemetry.events();
        let physical_events = events
            .iter()
            .filter(|event| matches!(event, LogicalChunkTelemetryEvent::PhysicalCompleted { .. }))
            .count();
        assert_eq!(physical_events, 2);
        assert!(events.iter().all(|event| match event {
            LogicalChunkTelemetryEvent::PhysicalCompleted {
                billing_authoritative,
                ..
            } => !billing_authoritative,
            LogicalChunkTelemetryEvent::LogicalCompleted { metadata, .. } => {
                metadata.summary_incremental_cost_usd == 0.0
            },
            LogicalChunkTelemetryEvent::LogicalFailed { .. } => false,
        }));
    }

    #[test]
    fn first_hierarchy_level_measures_packed_logical_work_then_uses_physical_cardinality() {
        assert_eq!(reduction_convergence_baseline(1, 1, 8), 8);
        assert_eq!(reduction_convergence_baseline(1, 4, 8), 8);
        assert_eq!(reduction_convergence_baseline(2, 4, 8), 4);
        assert_eq!(reduction_convergence_baseline(3, 2, 8), 2);
    }
}
