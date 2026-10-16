//! Request-scoped attempt receipts, including futures dropped by deadlines.
//! The collector contains metadata only; it never retains state or answers.
use crate::{DecisionError, DecisionResponse, ModelIdentity};
use decision_engine_contract::telemetry::{DecisionCallStatus, DecisionModelCall};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

tokio::task_local! {
    static QUEUE_WAIT: u64;
    static ITEM: (String, Vec<String>);
    static CALLS: Arc<Mutex<Vec<DecisionModelCall>>>;
}

pub async fn capture<T>(
    future: impl std::future::Future<Output = T>,
) -> (T, Vec<DecisionModelCall>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let result = CALLS.scope(calls.clone(), future).await;
    let records = std::mem::take(&mut *calls.lock().unwrap_or_else(|p| p.into_inner()));
    (result, records)
}

/// Keeps item identity through cancellation: Attempt captures it before dispatch.
pub async fn for_items<T>(
    batch: String,
    items: Vec<String>,
    future: impl std::future::Future<Output = T>,
) -> T {
    ITEM.scope((batch, items), future).await
}

pub async fn with_queue_wait<T>(ms: u64, future: impl std::future::Future<Output = T>) -> T {
    QUEUE_WAIT.scope(ms, future).await
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn group_id() -> String {
    ulid::Ulid::new().to_string()
}

pub struct Attempt {
    pub receipt: DecisionModelCall,
    started: Instant,
    collector: Option<Arc<Mutex<Vec<DecisionModelCall>>>>,
}

impl Attempt {
    pub fn new(
        operation: &str,
        identity: ModelIdentity,
        provider: String,
        local: bool,
        group: &str,
        attempt: u32,
    ) -> Self {
        Self {
            receipt: DecisionModelCall {
                batch_id: ITEM.try_with(|item| item.0.clone()).ok(),
                item_ids: ITEM.try_with(|item| item.1.clone()).unwrap_or_default(),
                // Match LLM identities so historical point reads can select
                // the original time partition after the recent window ages out.
                call_id: ulid::Ulid::new().to_string(),
                retry_group_id: group.into(),
                attempt,
                operation: operation.into(),
                adapter: identity.adapter,
                provider,
                requested_model: identity.model.clone(),
                model: identity.model,
                local,
                started_at_ms: now_ms(),
                completed_at_ms: 0,
                latency_ms: 0,
                queue_wait_ms: QUEUE_WAIT.try_with(|ms| *ms).unwrap_or(0),
                status: DecisionCallStatus::Cancelled,
                error_class: Some("cancelled".into()),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            started: Instant::now(),
            collector: CALLS.try_with(Arc::clone).ok(),
        }
    }

    pub fn result(&mut self, result: &Result<DecisionResponse, DecisionError>) {
        match result {
            Ok(response) => {
                self.receipt.model = response.model.model.clone();
                self.receipt.input_tokens = Some(response.usage.input_tokens);
                self.receipt.output_tokens = Some(response.usage.output_tokens);
                self.succeeded();
            },
            Err(error) => self.failed(error),
        }
    }

    pub fn succeeded(&mut self) {
        self.receipt.status = DecisionCallStatus::Succeeded;
        self.receipt.error_class = None;
    }

    pub fn failed(&mut self, error: &DecisionError) {
        self.receipt.status = DecisionCallStatus::Failed;
        self.receipt.error_class = Some(
            error
                .health_reason()
                .unwrap_or(match error {
                    DecisionError::InvalidResponse(_) | DecisionError::UnknownOption { .. } => {
                        "invalid_response"
                    },
                    _ => "model_error",
                })
                .into(),
        );
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        self.receipt.latency_ms = self.started.elapsed().as_millis() as u64;
        self.receipt.completed_at_ms = now_ms().max(self.receipt.started_at_ms);
        if let Some(collector) = &self.collector {
            collector
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(self.receipt.clone());
        }
    }
}
