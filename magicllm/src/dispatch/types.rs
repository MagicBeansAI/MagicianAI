//! Plain data types used across the dispatch module.

use serde::{Deserialize, Serialize};
use std::fmt;
use ulid::Ulid;

use crate::trace::{LlmScope, LlmTraceContext};

/// ULID-based job identifier. Time-sortable, fits in 26 chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub String);

impl JobId {
    /// Generate a fresh time-sortable ID.
    pub fn new() -> Self {
        Self(Ulid::new().to_string())
    }

    /// Borrow as `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for JobId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for JobId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Dispatch priority lane. Higher priorities are preferred at pickup, with
/// bounded bursts so continuously-ready foreground lanes cannot starve lower
/// lanes indefinitely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// User-facing latency-sensitive — chat turns, voice replies.
    High,
    /// Agent automation / planning — inner-loop decisions, slot-graph.
    Normal,
    /// Best-effort — consolidation, learning, memory eval.
    Background,
}

impl Priority {
    /// String name used in metrics and logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Normal => "normal",
            Self::Background => "background",
        }
    }
}

impl Default for Priority {
    fn default() -> Self {
        Self::Normal
    }
}

/// Identifies a task and (optionally) the agent / chat session that owns the
/// LLM call. Used by cancellation gates and ledger emission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    /// Required: the task this LLM call belongs to.
    pub task_id: String,
    /// Optional: agent that owns the task.
    pub agent_id: Option<String>,
    /// Optional: chat session that initiated the task.
    pub chat_session_id: Option<String>,
    /// Authoritative local tenant/workspace scope when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<LlmScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message_id: Option<String>,
    /// This call PRODUCES the execution's terminal record, so it necessarily
    /// runs after the execution — and often the task — has gone terminal.
    ///
    /// The pre-dispatch gate tombstones any job whose task is cancelled or
    /// terminally failed. That is right for ordinary work and exactly wrong
    /// here: terminal-output synthesis is the job that materialises the
    /// deliverable, so cancelling it on a terminal task is circular — the task
    /// has no output, so it fails; it failed, so its output synthesis is
    /// cancelled. Set on that job class only.
    ///
    /// Explicit cancellation still reaches these jobs: `matches_cancel_id`
    /// matches the task id, execution id and root execution id, and this flag
    /// is read only by the pre-dispatch TASK-STATE gate.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub survives_terminal_task: bool,
}

impl TaskRef {
    /// Construct a minimal `TaskRef` with just a task id.
    pub fn task(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            agent_id: None,
            chat_session_id: None,
            scope: None,
            root_execution_id: None,
            execution_id: None,
            plan_id: None,
            step_id: None,
            chat_turn_id: None,
            iteration_id: None,
            user_message_id: None,
            survives_terminal_task: false,
        }
    }

    /// Mark this job as the one that produces the execution's terminal record.
    /// See [`TaskRef::survives_terminal_task`].
    pub fn surviving_terminal_task(mut self) -> Self {
        self.survives_terminal_task = true;
        self
    }

    /// Builder: attach an agent id.
    pub fn with_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// Builder: attach a chat session id.
    pub fn with_chat_session(mut self, chat_session_id: impl Into<String>) -> Self {
        self.chat_session_id = Some(chat_session_id.into());
        self
    }

    /// A ref whose `task_id` IS its chat session id: a chat turn keyed by its
    /// session for fair-lane scheduling and session cancellation, not a task.
    /// Such a ref is a scheduling identity and must never be written into a
    /// trace as if the turn belonged to a task.
    pub fn is_session_keyed(&self) -> bool {
        self.chat_session_id.as_deref() == Some(self.task_id.as_str())
    }

    pub fn with_scope(
        mut self,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        self.scope = Some(LlmScope::new(principal, workspace));
        self
    }

    pub fn with_execution(
        mut self,
        root_execution_id: impl Into<String>,
        execution_id: impl Into<String>,
    ) -> Self {
        self.root_execution_id = Some(root_execution_id.into());
        self.execution_id = Some(execution_id.into());
        self
    }

    /// Ids `cancel_task` / in-flight cancel tokens must honour.
    ///
    /// Agentic jobs set `task_id` to the persisted artifact task and
    /// `execution_id` to the runtime execution. `cancel_execution` fires the
    /// execution id. Matching only `task_id` leaves those jobs running.
    pub fn matches_cancel_id(&self, id: &str) -> bool {
        !id.is_empty()
            && (self.task_id == id
                || self.execution_id.as_deref() == Some(id)
                || self.root_execution_id.as_deref() == Some(id))
    }

    /// Distinct non-empty cancel keys for this job.
    pub fn cancel_ids(&self) -> impl Iterator<Item = &str> {
        [
            Some(self.task_id.as_str()),
            self.execution_id.as_deref(),
            self.root_execution_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|id| !id.is_empty())
    }

    pub fn with_chat_turn(mut self, chat_turn_id: impl Into<String>) -> Self {
        self.chat_turn_id = Some(chat_turn_id.into());
        self
    }

    pub fn with_iteration(mut self, iteration_id: impl Into<String>) -> Self {
        self.iteration_id = Some(iteration_id.into());
        self
    }

    pub fn with_plan_step(mut self, plan_id: Option<String>, step_id: Option<String>) -> Self {
        self.plan_id = plan_id;
        self.step_id = step_id;
        self
    }

    pub fn with_user_message(mut self, user_message_id: impl Into<String>) -> Self {
        self.user_message_id = Some(user_message_id.into());
        self
    }
}

/// Caller-supplied origin hint, surfaced in the viewer and ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOrigin {
    /// Operation name (e.g. "chat_turn", "task_state_generate").
    pub operation: String,
    /// Caller hint (e.g. file path or component name).
    pub caller: Option<String>,
    /// Live runtime-activity row this job was submitted from — the join key
    /// between the watchable view of the runtime and this job's retrospective
    /// telemetry.
    ///
    /// Opaque by construction: the decimal form of a process-monotonic
    /// counter minted by the submitting process's activity layer. It carries
    /// no credential, prompt or user content, and it is not an identifier
    /// this crate can mint — `magicllm` has no view of the submitter's
    /// tracing context, so the value is always stamped by the caller.
    ///
    /// `None` when the submitting call ran outside any instrumented span.
    /// That is legitimate rather than an error: the job simply has no live
    /// row to point back at, and a fabricated id would join it to one that
    /// was never related.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_id: Option<String>,
}

impl JobOrigin {
    /// Construct an origin with just an operation name.
    pub fn op(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            caller: None,
            activity_id: None,
        }
    }

    /// Builder: attach a caller hint.
    pub fn with_caller(mut self, caller: impl Into<String>) -> Self {
        self.caller = Some(caller.into());
        self
    }

    /// Builder: attach the live activity this job was submitted from.
    ///
    /// Takes the `Option` rather than the id so a submit site can pass a
    /// lookup result straight through. Passing `None` leaves the job
    /// unattributed, which is the correct record for a call made outside any
    /// instrumented span.
    ///
    /// An id that is empty or whitespace-only is normalised to `None` here
    /// rather than carried forward. It joins against nothing either way, and
    /// this is the boundary at which that stays a missing label: downstream it
    /// used to fail `LlmTraceContext::is_valid`, which gates *admission*, so a
    /// caller passing `Some("")` would have had the model call itself refused.
    pub fn with_activity_id(mut self, activity_id: Option<String>) -> Self {
        self.activity_id = crate::trace::normalized_activity_id(activity_id);
        self
    }
}

/// Lifecycle state of a job, as observed by the registry / viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Submitted, awaiting pickup by a worker.
    Pending,
    /// Admitted by the bounded queue and parked in the provider-specific
    /// scheduler until that provider has execution capacity.
    WaitingForProvider,
    /// Admitted and parked on the local-prep coordinator until the serial
    /// Ollama generation slot is free. Does not occupy a dispatch worker.
    WaitingForLocalPrep,
    /// Worker has picked up the job and is running it.
    InFlight,
    /// Completed successfully (response sent to caller).
    Completed,
    /// Terminal failure after retry budget exhausted, or non-retriable error.
    Failed,
    /// Tombstoned by cancellation, deadline, or shutdown.
    Tombstoned,
}

/// Reason a job was tombstoned (never reached completion).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TombstoneReason {
    /// Owning task was already cancelled before dispatch.
    TaskCancelled { reason: Option<String> },
    /// Task was cancelled while LLM call was mid-flight.
    TaskCancelledInFlight { reason: Option<String> },
    /// Owning task no longer exists in the task store.
    TaskMissing,
    /// Chat session that owns the call ended.
    ChatSessionEnded { reason: Option<String> },
    /// External `cancel_job(job_id)` call.
    ExplicitCancel { reason: Option<String> },
    /// Process shutdown drained queued jobs.
    QueueShutdown,
    /// Submission deadline elapsed before worker pickup.
    DeadlineExceeded,
    /// Lane was full at submit time (returned to caller; not stored as
    /// tombstone in the registry — included here for the realtime event
    /// variant only).
    QueueFull,
    /// Synthetic event emitted on boot for ledger entries lacking a
    /// terminal event from the previous process.
    ProcessRestart,
}

impl TombstoneReason {
    /// Short canonical name used in metrics + logs.
    pub fn name(&self) -> &'static str {
        match self {
            Self::TaskCancelled { .. } => "task_cancelled",
            Self::TaskCancelledInFlight { .. } => "task_cancelled_in_flight",
            Self::TaskMissing => "task_missing",
            Self::ChatSessionEnded { .. } => "chat_session_ended",
            Self::ExplicitCancel { .. } => "explicit_cancel",
            Self::QueueShutdown => "queue_shutdown",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::QueueFull => "queue_full",
            Self::ProcessRestart => "process_restart",
        }
    }
}

/// Stats from a local-prep summarisation pass (Ollama Gemma). `None` when
/// local-prep didn't run on this request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPrepStat {
    /// Number of `SummarisableContext` blocks that were summarised.
    pub blocks_processed: u32,
    /// Total input characters across all summarised blocks.
    pub chars_in: u64,
    /// Total output characters from the local model.
    pub chars_out: u64,
    /// Local model identifier.
    pub model: String,
    /// Wall-clock duration of local-prep.
    pub duration_ms: u64,
    /// Exact content-free facts for each nested local provider invocation.
    /// These calls bypass the queue intentionally to avoid recursive deadlock,
    /// but must not disappear from call/attempt observability.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<LocalPrepCallStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPrepCallStat {
    pub trace_context: LlmTraceContext,
    pub provider_attempt_id: String,
    pub provider_attempt_count: u32,
    pub provider: String,
    pub model: String,
    pub purpose: String,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub latency_ms: u64,
    pub success: bool,
    pub error_class: Option<String>,
    pub tokens: Option<TokenSummary>,
}

/// Token usage summary surfaced to the viewer + ledger.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenSummary {
    /// Tokens in the prompt.
    pub prompt_tokens: u32,
    /// Tokens in the completion.
    pub completion_tokens: u32,
    /// Cached tokens (subset of prompt_tokens for providers that report it).
    pub cached_tokens: u32,
    /// Tokens used for reasoning (when reported).
    pub reasoning_tokens: u32,
}
