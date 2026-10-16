//! Job envelopes, attempt history, and dispatched-response shapes.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Notify};

fn now_unix_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

use crate::capability::LLMProviderKind;
use crate::error::LLMResult;
use crate::trace::{LlmScopeResolution, LlmTraceContext, LlmTraceReceipt, LlmWorkloadClass};
use crate::types::{LLMRequest, LLMResponse, StreamDelta};

use super::router_handle::DispatchRouter;
use super::types::{
    JobId, JobOrigin, JobState, LocalPrepStat, Priority, TaskRef, TokenSummary, TombstoneReason,
};

#[derive(Default)]
pub(crate) struct QueueByteCounters {
    high: AtomicU64,
    normal: AtomicU64,
    background: AtomicU64,
    global: AtomicU64,
    released: Notify,
}

impl QueueByteCounters {
    fn lane(&self, priority: Priority) -> &AtomicU64 {
        match priority {
            Priority::High => &self.high,
            Priority::Normal => &self.normal,
            Priority::Background => &self.background,
        }
    }

    pub(crate) fn try_reserve(
        self: &Arc<Self>,
        priority: Priority,
        bytes: u64,
        lane_capacity: u64,
        global_capacity: u64,
    ) -> Result<QueuedBytePermit, (u64, u64)> {
        fn reserve(counter: &AtomicU64, bytes: u64, cap: u64) -> Result<(), u64> {
            counter
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    current.checked_add(bytes).filter(|next| *next <= cap)
                })
                .map(|_| ())
                .map_err(|current| current)
        }

        reserve(&self.global, bytes, global_capacity)
            .map_err(|current| (current, global_capacity))?;
        if let Err(current) = reserve(self.lane(priority), bytes, lane_capacity) {
            self.global.fetch_sub(bytes, Ordering::AcqRel);
            return Err((current, lane_capacity));
        }
        Ok(QueuedBytePermit {
            counters: Arc::clone(self),
            priority,
            bytes,
        })
    }

    pub(crate) fn lane_bytes(&self, priority: Priority) -> u64 {
        self.lane(priority).load(Ordering::Acquire)
    }

    pub(crate) fn global_bytes(&self) -> u64 {
        self.global.load(Ordering::Acquire)
    }

    pub(crate) async fn released(&self) {
        self.released.notified().await;
    }
}

pub(crate) struct QueuedBytePermit {
    counters: Arc<QueueByteCounters>,
    priority: Priority,
    bytes: u64,
}

impl Drop for QueuedBytePermit {
    fn drop(&mut self) {
        self.counters
            .lane(self.priority)
            .fetch_sub(self.bytes, Ordering::AcqRel);
        self.counters.global.fetch_sub(self.bytes, Ordering::AcqRel);
        // One coordinator owns all byte waiters, so a stored single permit is
        // sufficient and avoids a thundering herd on every lane pickup.
        self.counters.released.notify_one();
    }
}

/// Sync job envelope. The worker resolves `response_tx` exactly once with the
/// terminal outcome (success or unrecoverable error).
pub struct LlmJob {
    pub job_id: JobId,
    pub request: LLMRequest,
    pub response_tx: oneshot::Sender<LLMResult<DispatchedResponse>>,
    pub submitted_at: Instant,
    pub priority: Priority,
    pub trace_id: Option<String>,
    pub trace_context: LlmTraceContext,
    pub origin: JobOrigin,
    pub task_ref: Option<TaskRef>,
    pub attempts: AttemptHistory,
    pub idempotency_key: Option<String>,
    pub submission_deadline: Option<Instant>,
    /// Optional immutable routing authority captured by the producer. This is
    /// used by snapshot-sensitive calls whose profile/config decision must not
    /// be reinterpreted by a queue router from another hot-reload generation.
    /// Retries retain the same `Arc` with the job.
    pub(crate) router_snapshot: Option<Arc<dyn DispatchRouter>>,
    pub(crate) queued_byte_permit: Option<QueuedBytePermit>,
    /// Internal continuation marker for an in-place retry that temporarily
    /// yielded its worker while waiting for retained-byte capacity/backoff.
    /// It skips gates/local prep that the original in-worker loop did not
    /// repeat, and is cleared only immediately before the next provider call.
    pub(crate) resume_in_place_retry: bool,
    /// Set after the local-prep coordinator finishes (or when prep was
    /// skipped). Prevents a second park when the job re-enters a worker.
    pub(crate) local_prep_done: bool,
    /// Stats from the coordinator (or a prior in-worker cheap path resume).
    /// Taken once when the job re-enters a worker with `local_prep_done`.
    pub(crate) local_prep_stat: Option<LocalPrepStat>,
}

impl LlmJob {
    /// Builder helper for callers that just want to submit with sane defaults.
    pub fn new(
        mut request: LLMRequest,
        origin: JobOrigin,
    ) -> (Self, oneshot::Receiver<LLMResult<DispatchedResponse>>) {
        let (tx, rx) = oneshot::channel();
        let mut trace_context = request
            .metadata
            .ensure_trace_context(None, LlmWorkloadClass::System);
        // Copy the activity from the origin the caller already stamped, rather
        // than re-deriving it. This is the one place both are in hand, and
        // deriving it twice is precisely how the two would come to disagree —
        // the dispatch sink reads the origin while the lifecycle recorder sees
        // only the context, so a second derivation would make cost and
        // dispatch rows attribute the same call to different spans.
        //
        // Only fills a hole; never overwrites. A context that already names an
        // activity was given one deliberately by a caller closer to the work.
        //
        // Normalised on the way across rather than copied verbatim: `JobOrigin`
        // exposes the field, so `with_activity_id` is not the only way a value
        // gets in, and a whitespace-only id must become `None` before anything
        // downstream treats it as present.
        if trace_context.activity_id.is_none() && origin.activity_id.is_some() {
            trace_context.set_activity_id(origin.activity_id.as_deref());
            // `ensure_trace_context` hands back a CLONE and keeps its own copy
            // on the request metadata, so the assignment above changed only
            // the job-side view. Two readers see the request-side copy and
            // would otherwise read `None` for a call that has an activity:
            // local prep derives its summariser child context from
            // `request.metadata.trace_context`, and the router's
            // content-capture path clones the same field. Push the filled
            // value back so the two copies of one call's identity cannot
            // disagree about which span the work belongs to.
            //
            // Safe to do before `ensure_provider_attempt_counter`:
            // `set_trace_context` only clears the counter when the
            // `llm_call_id` changes, and this is the same context.
            request.metadata.set_trace_context(trace_context.clone());
        }
        request.metadata.ensure_provider_attempt_counter();
        let job = Self {
            job_id: JobId::new(),
            request,
            response_tx: tx,
            submitted_at: Instant::now(),
            priority: Priority::Normal,
            trace_id: Some(trace_context.trace_id.clone()),
            trace_context,
            origin,
            task_ref: None,
            attempts: AttemptHistory::default(),
            idempotency_key: None,
            submission_deadline: None,
            router_snapshot: None,
            queued_byte_permit: None,
            resume_in_place_retry: false,
            local_prep_done: false,
            local_prep_stat: None,
        };
        (job, rx)
    }

    /// Builder: attach a priority.
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Builder: attach a task reference (enables cancellation gates).
    pub fn with_task(mut self, task_ref: TaskRef) -> Self {
        merge_task_ref_into_trace(&mut self.trace_context, &task_ref);
        self.request
            .metadata
            .set_trace_context(self.trace_context.clone());
        self.task_ref = Some(task_ref);
        self
    }

    pub(crate) fn validate_trace_lineage(&self) -> Result<(), String> {
        validate_task_ref_against_trace(&self.trace_context, self.task_ref.as_ref())
    }

    /// Builder: attach a trace id.
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        let trace_id = trace_id.into();
        self.trace_id = Some(trace_id.clone());
        self.trace_context.trace_id = trace_id;
        self.request
            .metadata
            .set_trace_context(self.trace_context.clone());
        self
    }

    pub fn with_trace_context(mut self, trace_context: LlmTraceContext) -> Self {
        self.trace_id = Some(trace_context.trace_id.clone());
        self.request
            .metadata
            .set_trace_context(trace_context.clone());
        self.trace_context = trace_context;
        self
    }

    /// Builder: attach an idempotency key.
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Builder: attach a submission deadline.
    pub fn with_submission_deadline(mut self, deadline: Instant) -> Self {
        self.submission_deadline = Some(deadline);
        self
    }

    /// Bind this job and all of its in-place/requeued retries to one immutable
    /// configured-router generation.
    pub fn with_router_snapshot(mut self, router: Arc<dyn DispatchRouter>) -> Self {
        self.router_snapshot = Some(router);
        self
    }
}

/// Streaming variant. Worker pushes `StreamDelta` items into `delta_tx`.
pub struct LlmStreamJob {
    pub job_id: JobId,
    pub request: LLMRequest,
    pub delta_tx: mpsc::Sender<StreamDelta>,
    pub submitted_at: Instant,
    pub priority: Priority,
    pub trace_id: Option<String>,
    pub trace_context: LlmTraceContext,
    pub origin: JobOrigin,
    pub task_ref: Option<TaskRef>,
    pub attempts: AttemptHistory,
    pub submission_deadline: Option<Instant>,
    /// Optional immutable routing authority captured by the producer. Chat
    /// streams bind MultiLLMService's live configured router so a hot reload
    /// cannot reinterpret the request on the queue's boot-time router.
    pub(crate) router_snapshot: Option<Arc<dyn DispatchRouter>>,
    pub(crate) queued_byte_permit: Option<QueuedBytePermit>,
}

impl LlmStreamJob {
    /// Builder helper.
    pub fn new(
        mut request: LLMRequest,
        origin: JobOrigin,
        buffer: usize,
    ) -> (Self, mpsc::Receiver<StreamDelta>) {
        let (tx, rx) = mpsc::channel(buffer.max(1));
        let trace_context = request
            .metadata
            .ensure_trace_context(None, LlmWorkloadClass::ForegroundChat);
        request.metadata.ensure_provider_attempt_counter();
        let job = Self {
            job_id: JobId::new(),
            request,
            delta_tx: tx,
            submitted_at: Instant::now(),
            priority: Priority::Normal,
            trace_id: Some(trace_context.trace_id.clone()),
            trace_context,
            origin,
            task_ref: None,
            attempts: AttemptHistory::default(),
            submission_deadline: None,
            router_snapshot: None,
            queued_byte_permit: None,
        };
        (job, rx)
    }

    /// Builder: attach a priority.
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Builder: attach a task reference.
    pub fn with_task(mut self, task_ref: TaskRef) -> Self {
        merge_task_ref_into_trace(&mut self.trace_context, &task_ref);
        self.request
            .metadata
            .set_trace_context(self.trace_context.clone());
        self.task_ref = Some(task_ref);
        self
    }

    pub(crate) fn validate_trace_lineage(&self) -> Result<(), String> {
        validate_task_ref_against_trace(&self.trace_context, self.task_ref.as_ref())
    }

    pub fn with_trace_context(mut self, trace_context: LlmTraceContext) -> Self {
        self.trace_id = Some(trace_context.trace_id.clone());
        self.request
            .metadata
            .set_trace_context(trace_context.clone());
        self.trace_context = trace_context;
        self
    }

    pub fn with_submission_deadline(mut self, deadline: Instant) -> Self {
        self.submission_deadline = Some(deadline);
        self
    }

    /// Bind this stream to one immutable configured-router generation.
    pub fn with_router_snapshot(mut self, router: Arc<dyn DispatchRouter>) -> Self {
        self.router_snapshot = Some(router);
        self
    }
}

fn merge_task_ref_into_trace(context: &mut LlmTraceContext, task_ref: &TaskRef) {
    // **A scheduling key is not a trace fact.** A chat turn's ref carries its
    // session id as `task_id` so the fair lane and `cancel_chat_session` have
    // something to key on. Writing that into the trace made every chat call
    // row claim a task the turn never had, while the turn's own context — the
    // one its tool-lineage rows carry — said none. The governed analytics
    // read then refused the whole day partition for ownership drift. Only a
    // real task id fills the trace.
    if !task_ref.is_session_keyed() {
        fill_optional(&mut context.task_id, &Some(task_ref.task_id.clone()));
    }
    fill_optional(&mut context.root_execution_id, &task_ref.root_execution_id);
    fill_optional(&mut context.execution_id, &task_ref.execution_id);
    fill_optional(&mut context.plan_id, &task_ref.plan_id);
    fill_optional(&mut context.step_id, &task_ref.step_id);
    fill_optional(&mut context.chat_session_id, &task_ref.chat_session_id);
    fill_optional(&mut context.chat_turn_id, &task_ref.chat_turn_id);
    fill_optional(&mut context.iteration_id, &task_ref.iteration_id);
    fill_optional(&mut context.user_message_id, &task_ref.user_message_id);
    if let Some(scope) = task_ref.scope.as_ref() {
        // `Inherited` means the scope CAME from the ref. A context that
        // already names the same scope explicitly keeps saying so; downgrading
        // it made the call row disagree with every other row of its turn.
        if matches!(
            context.scope_resolution,
            LlmScopeResolution::LegacyDefault | LlmScopeResolution::SystemDefault
        ) {
            context.scope = scope.clone();
            context.scope_resolution = LlmScopeResolution::Inherited;
        }
    }
}

fn fill_optional(target: &mut Option<String>, incoming: &Option<String>) {
    if target.is_none() {
        let Some(value) = incoming.as_ref() else {
            return;
        };
        *target = Some(value.clone());
    }
}

fn validate_task_ref_against_trace(
    context: &LlmTraceContext,
    task_ref: Option<&TaskRef>,
) -> Result<(), String> {
    let Some(task_ref) = task_ref else {
        return Ok(());
    };
    if task_ref.task_id.trim().is_empty() {
        return Err("task reference task_id must not be blank".to_string());
    }
    if task_ref.is_session_keyed() {
        // The trace carries no task for a session-keyed turn; the session is
        // what must agree.
        if context.chat_session_id.as_deref() != Some(task_ref.task_id.as_str()) {
            return Err(
                "session-keyed task reference conflicts with the trace chat session".to_string(),
            );
        }
    } else if context.task_id.as_deref() != Some(task_ref.task_id.as_str()) {
        return Err("task reference task_id conflicts with the trace context".to_string());
    }
    if let Some(scope) = task_ref.scope.as_ref() {
        if !scope.is_valid() {
            return Err("task reference scope is invalid".to_string());
        }
        if &context.scope != scope {
            return Err("task reference scope conflicts with the trace context".to_string());
        }
    }
    for (label, trace_value, task_value) in [
        (
            "root_execution_id",
            context.root_execution_id.as_deref(),
            task_ref.root_execution_id.as_deref(),
        ),
        (
            "execution_id",
            context.execution_id.as_deref(),
            task_ref.execution_id.as_deref(),
        ),
        (
            "plan_id",
            context.plan_id.as_deref(),
            task_ref.plan_id.as_deref(),
        ),
        (
            "step_id",
            context.step_id.as_deref(),
            task_ref.step_id.as_deref(),
        ),
        (
            "chat_session_id",
            context.chat_session_id.as_deref(),
            task_ref.chat_session_id.as_deref(),
        ),
        (
            "chat_turn_id",
            context.chat_turn_id.as_deref(),
            task_ref.chat_turn_id.as_deref(),
        ),
        (
            "iteration_id",
            context.iteration_id.as_deref(),
            task_ref.iteration_id.as_deref(),
        ),
        (
            "user_message_id",
            context.user_message_id.as_deref(),
            task_ref.user_message_id.as_deref(),
        ),
    ] {
        if task_value.is_some_and(|task_value| trace_value != Some(task_value)) {
            return Err(format!(
                "task reference {label} conflicts with the trace context"
            ));
        }
    }
    Ok(())
}

/// History of attempts for a single job. Preserved across re-queue cycles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AttemptHistory {
    /// Total attempts made so far across all cycles.
    pub total: u32,
    /// Actual provider invocations, including uncharged rate-limit attempts.
    #[serde(default)]
    pub provider_total: u32,
    /// Attempts in the current dispatch cycle (0..max_attempts_per_dispatch).
    pub dispatch: u32,
    /// Number of times re-queued (0..max_dispatch_cycles).
    pub cycle: u32,
    /// Diagnostic record of recent failures (bounded by `max_recorded_errors`).
    pub errors: Vec<AttemptError>,
}

impl AttemptHistory {
    /// Append an error, capped at `max` entries.
    pub fn record(&mut self, err: AttemptError, max: usize) {
        if self.errors.len() >= max {
            self.errors.remove(0);
        }
        self.errors.push(err);
    }
}

/// One failed attempt's metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptError {
    pub attempt: u32,
    pub at: SystemTime,
    pub error: String,
    pub class: ErrorClass,
    pub retriable: bool,
}

/// Classification of an underlying provider/transport error. Drives the
/// retry decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Connection reset, DNS, TLS — retriable.
    Network,
    /// Provider timed out — retriable.
    Timeout,
    /// 500/502/503/504 — retriable.
    Server5xx,
    /// 429 — retriable, honour Retry-After when present.
    RateLimit,
    /// 400/401/403/422 — not retriable.
    Provider4xx,
    /// Safety / refusal — not retriable.
    ContentPolicy,
    /// Malformed provider response — not retriable.
    ParseError,
    /// Cancellation; never reaches the retry layer.
    Cancelled,
    /// Conservative bucket — not retriable.
    Unknown,
}

impl ErrorClass {
    /// Whether errors in this class should consume a retry slot.
    pub fn is_retriable(self) -> bool {
        matches!(
            self,
            Self::Network | Self::Timeout | Self::Server5xx | Self::RateLimit
        )
    }

    /// Short canonical name for metrics + ledger.
    pub fn name(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::Server5xx => "server_5xx",
            Self::RateLimit => "rate_limit",
            Self::Provider4xx => "provider_4xx",
            Self::ContentPolicy => "content_policy",
            Self::ParseError => "parse_error",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

/// Successful dispatch outcome returned to the caller alongside timing.
#[derive(Debug, Clone)]
pub struct DispatchedResponse {
    /// Immutable provider payload. Idempotent subscribers and the recent
    /// result cache share this allocation; consumer-specific reuse metadata
    /// remains in `trace_receipt` and is never written back into shared bytes.
    pub response: Arc<LLMResponse>,
    /// Time from submission to worker pickup (queue dwell).
    pub wait: Duration,
    /// Time from pickup to response (provider execution).
    pub execution: Duration,
    /// Local-prep stats if any.
    pub local_prep: Option<LocalPrepStat>,
    /// Dispatch retry cycles that produced this success. Physical provider
    /// invocations (including profile fallback hops inside one cycle) are
    /// authoritative in `trace_receipt.provider_attempt_count`.
    pub attempts: u32,
    /// Stable logical-call/attempt/dispatch correlation. The original owner
    /// also sees it in `response.trace_receipt`; cached consumers use this
    /// envelope as authority, or `into_response()` to stamp an owned response.
    pub trace_receipt: LlmTraceReceipt,
}

impl DispatchedResponse {
    /// Consume the dispatch envelope and return the traditional owned provider
    /// response, stamping the consumer-specific receipt at the ownership
    /// boundary. The common single-owner path unwraps the Arc. Shared
    /// idempotent consumers clone only the small response envelope: all large
    /// provider lanes inside [`LLMResponse`] are Arc-backed and remain shared.
    pub fn into_response(self) -> LLMResponse {
        let mut response = match Arc::try_unwrap(self.response) {
            Ok(response) => response,
            Err(response) => (*response).clone(),
        };
        response.trace_receipt = Some(self.trace_receipt);
        response
    }

    /// Approximate heap bytes retained by one cached result. This deliberately
    /// excludes Arc-shared allocation overhead and includes the payload once;
    /// the idempotency index owns one strong reference per cached key.
    pub(crate) fn estimated_retained_bytes(&self) -> usize {
        self.response
            .estimated_retained_bytes()
            .saturating_add(estimated_trace_receipt_bytes(&self.trace_receipt))
            .saturating_add(std::mem::size_of::<Self>())
    }
}

fn estimated_trace_receipt_bytes(receipt: &LlmTraceReceipt) -> usize {
    let context = &receipt.context;
    let optional_len = |value: &Option<String>| value.as_ref().map_or(0, String::len);
    std::mem::size_of::<LlmTraceReceipt>()
        .saturating_add(context.trace_id.len())
        .saturating_add(context.llm_call_id.len())
        .saturating_add(optional_len(&context.parent_call_id))
        .saturating_add(optional_len(&context.retry_group_id))
        .saturating_add(optional_len(&context.route_decision_id))
        .saturating_add(context.scope.principal.len())
        .saturating_add(context.scope.workspace.len())
        .saturating_add(optional_len(&context.task_id))
        .saturating_add(optional_len(&context.root_execution_id))
        .saturating_add(optional_len(&context.execution_id))
        .saturating_add(optional_len(&context.plan_id))
        .saturating_add(optional_len(&context.step_id))
        .saturating_add(optional_len(&context.iteration_id))
        .saturating_add(optional_len(&context.chat_session_id))
        .saturating_add(optional_len(&context.chat_turn_id))
        .saturating_add(optional_len(&context.user_message_id))
        .saturating_add(optional_len(&receipt.dispatch_job_id))
        .saturating_add(optional_len(&receipt.provider_attempt_id))
}

/// Serialisable, externally-observable view of a job for the viewer / ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobMeta {
    pub job_id: JobId,
    pub trace_context: LlmTraceContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_attempt_id: Option<String>,
    #[serde(default)]
    pub provider_attempt_count: u32,
    #[serde(default)]
    pub response_reused: bool,
    pub priority: Priority,
    pub task_ref: Option<TaskRef>,
    pub origin: JobOrigin,
    /// Effective profile selected for the terminal provider attempt. Pending
    /// and pre-provider terminal jobs leave this unset rather than claiming a
    /// requested profile was actually invoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub provider: Option<LLMProviderKind>,
    pub model: Option<String>,
    pub state: JobState,
    /// Unix-millis timestamps. Serde's default SystemTime serialization is
    /// `{secs_since_epoch, nanos_since_epoch}` — opaque to JS/TS consumers
    /// and over-precise for a viewer. Plain i64 millis serialise as numbers
    /// the panel can pass into `new Date(...)`.
    pub submitted_at_ms: i64,
    pub dispatched_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
    pub wait_ms: Option<u64>,
    /// Time spent after worker pickup waiting for the configured provider's
    /// concurrency permit. Kept separate from lane wait and provider execution
    /// so saturation is observable instead of being attributed to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_wait_ms: Option<u64>,
    pub execution_ms: Option<u64>,
    pub tokens: Option<TokenSummary>,
    pub tombstone: Option<TombstoneReason>,
    pub error: Option<String>,
    pub error_class: Option<ErrorClass>,
    pub attempts: u32,
    pub local_prep: Option<LocalPrepStat>,
    pub idempotency_key: Option<String>,
}

impl JobMeta {
    /// Populate terminal wall-clock timings consistently for completion,
    /// failure, cancellation and shutdown paths. Pre-dispatch tombstones have
    /// queue dwell but zero execution; dispatched jobs split at pickup.
    pub(crate) fn set_terminal_timing(&mut self, completed_at_ms: i64) {
        self.completed_at_ms = Some(completed_at_ms);
        if let Some(dispatched_at_ms) = self.dispatched_at_ms {
            if self.wait_ms.is_none() {
                self.wait_ms =
                    u64::try_from(dispatched_at_ms.saturating_sub(self.submitted_at_ms)).ok();
            }
            // The worker records exact provider-attempt durations as they
            // finish. Preserve that sum so local preparation, semaphore wait,
            // retry backoff and requeue dwell are not mislabeled as provider
            // execution. A partially-cancelled call has no completed-attempt
            // sample, so fall back to elapsed provider-phase wall time.
            if self.execution_ms.is_none() {
                self.execution_ms =
                    u64::try_from(completed_at_ms.saturating_sub(dispatched_at_ms)).ok();
            }
        } else {
            if self.wait_ms.is_none() {
                self.wait_ms =
                    u64::try_from(completed_at_ms.saturating_sub(self.submitted_at_ms)).ok();
            }
            self.execution_ms = Some(0);
        }
    }

    /// Accumulate one completed provider-routing attempt without including
    /// queue, local-prep or retry-delay time in provider execution.
    pub(crate) fn add_provider_execution(&mut self, duration: Duration) {
        let duration_ms = duration.as_millis().min(u128::from(u64::MAX)) as u64;
        self.execution_ms = Some(
            self.execution_ms
                .unwrap_or_default()
                .saturating_add(duration_ms),
        );
    }

    /// Build the initial metadata when a job is first submitted.
    pub fn pending_from(job: &LlmJob) -> Self {
        Self {
            job_id: job.job_id.clone(),
            trace_context: job.trace_context.clone(),
            provider_attempt_id: None,
            provider_attempt_count: job.attempts.provider_total,
            response_reused: false,
            priority: job.priority,
            task_ref: job.task_ref.clone(),
            origin: job.origin.clone(),
            profile: None,
            provider: None,
            model: Some(job.request.model.clone()).filter(|m| !m.is_empty()),
            state: JobState::Pending,
            submitted_at_ms: now_unix_ms(),
            dispatched_at_ms: None,
            completed_at_ms: None,
            wait_ms: None,
            provider_wait_ms: None,
            execution_ms: None,
            tokens: None,
            tombstone: None,
            error: None,
            error_class: None,
            attempts: job.attempts.total,
            local_prep: None,
            idempotency_key: job.idempotency_key.clone(),
        }
    }

    /// Build initial metadata for a streaming job.
    pub fn pending_from_stream(job: &LlmStreamJob) -> Self {
        Self {
            job_id: job.job_id.clone(),
            trace_context: job.trace_context.clone(),
            provider_attempt_id: None,
            provider_attempt_count: job.attempts.provider_total,
            response_reused: false,
            priority: job.priority,
            task_ref: job.task_ref.clone(),
            origin: job.origin.clone(),
            profile: None,
            provider: None,
            model: Some(job.request.model.clone()).filter(|m| !m.is_empty()),
            state: JobState::Pending,
            submitted_at_ms: now_unix_ms(),
            dispatched_at_ms: None,
            completed_at_ms: None,
            wait_ms: None,
            provider_wait_ms: None,
            execution_ms: None,
            tokens: None,
            tombstone: None,
            error: None,
            error_class: None,
            attempts: job.attempts.total,
            local_prep: None,
            idempotency_key: None,
        }
    }
}
