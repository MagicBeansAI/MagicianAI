//! Test fixtures for downstream callers + the dispatch module's own tests.
//!
//! The real `LlmDispatchQueue` requires a `MultiLLMRouter` with registered
//! providers; for unit tests we want something simpler. `ImmediateLlmDispatchQueue`
//! runs jobs inline using a scripted response queue, and `MockLlmDispatchQueue`
//! offers a programmable mock that records calls.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::error::{LLMError, LLMResult};
use crate::trace::{LlmTraceContext, LlmTraceReceipt, LlmWorkloadClass};
use crate::types::{LLMRequest, LLMResponse};

use super::cancellation::{CancellationToken as CancelToken, TaskSnapshot, TaskStateView};
use super::job::DispatchedResponse;
use super::ledger::{LlmCallLedgerEvent, TaskLedgerSink};
use super::types::TaskRef;

/// Mock task-state view backed by a hand-set status table.
pub struct MockTaskStateView {
    snapshots: Mutex<HashMap<String, TaskSnapshot>>,
    tokens: Mutex<HashMap<String, CancellationToken>>,
}

impl Default for MockTaskStateView {
    fn default() -> Self {
        Self {
            snapshots: Mutex::new(HashMap::new()),
            tokens: Mutex::new(HashMap::new()),
        }
    }
}

impl MockTaskStateView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make `task_id` appear cancelled. Worker's pre-dispatch gate will tombstone.
    pub fn set_cancelled(&self, task_id: &str, reason: impl Into<String>) {
        self.snapshots.lock().insert(
            task_id.to_string(),
            TaskSnapshot {
                is_cancelled_or_terminal_failed: true,
                cancel_reason: Some(reason.into()),
            },
        );
        if let Some(tok) = self.tokens.lock().get(task_id) {
            tok.cancel();
        }
    }

    /// Make `task_id` appear active.
    pub fn set_active(&self, task_id: &str) {
        self.snapshots.lock().insert(
            task_id.to_string(),
            TaskSnapshot {
                is_cancelled_or_terminal_failed: false,
                cancel_reason: None,
            },
        );
    }

    /// Make `task_id` appear missing (returns `Ok(None)`).
    pub fn set_missing(&self, task_id: &str) {
        self.snapshots.lock().remove(task_id);
    }

    /// Force-fire the in-flight cancel token for `task_id` without changing
    /// snapshot — useful for race tests.
    pub fn fire_cancel(&self, task_id: &str) {
        let token = self
            .tokens
            .lock()
            .entry(task_id.to_string())
            .or_insert_with(CancellationToken::new)
            .clone();
        token.cancel();
    }
}

#[async_trait]
impl TaskStateView for MockTaskStateView {
    async fn snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, String> {
        Ok(self.snapshots.lock().get(task_id).cloned())
    }

    fn subscribe_cancel(&self, task_ref: Option<&TaskRef>) -> CancelToken {
        let Some(task_ref) = task_ref else {
            return CancelToken::new();
        };
        let ids: Vec<String> = task_ref
            .cancel_ids()
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut tokens = self.tokens.lock();
        let token = ids
            .iter()
            .find_map(|id| tokens.get(id).cloned())
            .unwrap_or_else(CancelToken::new);
        for id in ids {
            tokens.entry(id).or_insert_with(|| token.clone());
        }
        token
    }
}

/// Recording ledger sink — collects every event for assertions.
pub struct MockTaskLedgerSink {
    events: AsyncMutex<Vec<(TaskRef, LlmCallLedgerEvent)>>,
}

impl Default for MockTaskLedgerSink {
    fn default() -> Self {
        Self {
            events: AsyncMutex::new(Vec::new()),
        }
    }
}

impl MockTaskLedgerSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn events(&self) -> Vec<(TaskRef, LlmCallLedgerEvent)> {
        self.events.lock().await.clone()
    }
}

#[async_trait]
impl TaskLedgerSink for MockTaskLedgerSink {
    async fn append(&self, task_ref: &TaskRef, event: LlmCallLedgerEvent) {
        self.events.lock().await.push((task_ref.clone(), event));
    }
}

/// In-memory deterministic queue: runs jobs immediately, no workers.
///
/// Scripted via `enqueue_response()` — each call to `submit_and_wait()`
/// pops one outcome off the script.
pub struct ImmediateLlmDispatchQueue {
    script: Mutex<std::collections::VecDeque<LLMResult<LLMResponse>>>,
}

impl Default for ImmediateLlmDispatchQueue {
    fn default() -> Self {
        Self {
            script: Mutex::new(std::collections::VecDeque::new()),
        }
    }
}

impl ImmediateLlmDispatchQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one scripted response (or error) for the next caller.
    pub fn enqueue_response(&self, outcome: LLMResult<LLMResponse>) {
        self.script.lock().push_back(outcome);
    }

    /// Pop + return the next scripted outcome, or `Err(Other("script empty"))`.
    pub fn next_response(&self) -> LLMResult<LLMResponse> {
        self.script
            .lock()
            .pop_front()
            .unwrap_or_else(|| Err(LLMError::Other("script empty".to_string())))
    }

    /// Run one request synchronously against the script.
    pub async fn submit_and_wait(&self, req: LLMRequest) -> LLMResult<DispatchedResponse> {
        let mut response = self.next_response()?;
        let trace_receipt = response.trace_receipt.clone().unwrap_or_else(|| {
            LlmTraceReceipt::direct(req.metadata.trace_context.clone().unwrap_or_else(|| {
                LlmTraceContext::legacy(req.metadata.trace_id.as_deref(), LlmWorkloadClass::System)
            }))
        });
        response.trace_receipt = Some(trace_receipt.clone());
        Ok(DispatchedResponse {
            response: Arc::new(response),
            wait: Duration::ZERO,
            execution: Duration::ZERO,
            local_prep: None,
            attempts: 1,
            trace_receipt,
        })
    }
}

/// Recorded call to the mock queue.
#[derive(Debug, Clone)]
pub struct RecordedCall {
    pub operation: String,
    pub model: String,
    pub submitted_at: Instant,
}

/// Programmable mock queue: keyed responses + call recording.
pub struct MockLlmDispatchQueue {
    script: Mutex<HashMap<String, std::collections::VecDeque<LLMResult<LLMResponse>>>>,
    fallback: Mutex<std::collections::VecDeque<LLMResult<LLMResponse>>>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl Default for MockLlmDispatchQueue {
    fn default() -> Self {
        Self {
            script: Mutex::new(HashMap::new()),
            fallback: Mutex::new(std::collections::VecDeque::new()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl MockLlmDispatchQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Script a response keyed by operation name.
    pub fn script_for_operation(&self, op: impl Into<String>, outcome: LLMResult<LLMResponse>) {
        self.script
            .lock()
            .entry(op.into())
            .or_insert_with(std::collections::VecDeque::new)
            .push_back(outcome);
    }

    /// Script a fallback response used when the operation doesn't match.
    pub fn script_fallback(&self, outcome: LLMResult<LLMResponse>) {
        self.fallback.lock().push_back(outcome);
    }

    /// Recorded calls in order.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().clone()
    }

    /// Submit a request and resolve via the script.
    pub async fn submit_and_wait(&self, req: LLMRequest) -> LLMResult<DispatchedResponse> {
        let op = if req.metadata.operation.is_empty() {
            "default".to_string()
        } else {
            req.metadata.operation.clone()
        };
        self.calls.lock().push(RecordedCall {
            operation: op.clone(),
            model: req.model.clone(),
            submitted_at: Instant::now(),
        });
        let outcome = self
            .script
            .lock()
            .get_mut(&op)
            .and_then(|q| q.pop_front())
            .or_else(|| self.fallback.lock().pop_front())
            .unwrap_or_else(|| {
                Err(LLMError::Other(format!(
                    "MockLlmDispatchQueue: no scripted response for operation `{}`",
                    op
                )))
            });
        outcome.map(|mut response| {
            let trace_receipt = response.trace_receipt.clone().unwrap_or_else(|| {
                LlmTraceReceipt::direct(req.metadata.trace_context.clone().unwrap_or_else(|| {
                    LlmTraceContext::legacy(
                        req.metadata.trace_id.as_deref(),
                        LlmWorkloadClass::System,
                    )
                }))
            });
            response.trace_receipt = Some(trace_receipt.clone());
            DispatchedResponse {
                response: Arc::new(response),
                wait: Duration::ZERO,
                execution: Duration::ZERO,
                local_prep: None,
                attempts: 1,
                trace_receipt,
            }
        })
    }
}
