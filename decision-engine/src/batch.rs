//! Bounded classification execution; mutations and fallback remain with the host.
use crate::EngineState;
use decision_engine_contract::{batch::*, request::DecisionState, *};
use futures::{stream::FuturesUnordered, StreamExt};
use magician_decision::admission::Limiter;
use magician_decision::config::BatchStrategy;
use magician_decision::config::DecisionConfig;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

pub(crate) fn item_slots(config: &DecisionConfig) -> BTreeMap<String, Arc<Limiter>> {
    config
        .operations
        .iter()
        .map(|(name, op)| {
            (
                name.clone(),
                Arc::new(Limiter::new(op.classification.item_concurrency)),
            )
        })
        .collect()
}
fn unfinished(item: &DecisionItem) -> DecisionItemResult {
    DecisionItemResult {
        eligible_answers: Default::default(),
        item_id: item.item_id.clone(),
        status: ItemStatus::NotStarted,
        response: None,
        thresholds: None,
        error: None,
        latency_ms: 0,
    }
}
impl EngineState {
    pub(crate) async fn decide_batch(&self, request: DecideRequest) -> DecideResponse {
        let started = Instant::now();
        let mut reply = DecideResponse {
            batch: BatchResponse {
                request_id: request.batch.request_id.clone(),
                engine_instance: self.instance.clone(),
                policy_revision: self.revision.clone(),
                items: request.batch.items.iter().map(unfinished).collect(),
                model_health: BTreeMap::new(),
            },
            model_calls: Vec::new(),
            contract_version: CONTRACT_VERSION,
            status: DecideStatus::Failed,
            response: None,
            thresholds: None,
            error: None,
            latency_ms: 0,
        };
        let reject = |mut reply: DecideResponse, error: &str| {
            reply.error = Some(error.into());
            reply.latency_ms = started.elapsed().as_millis() as u64;
            reply
        };
        if request.contract_version != CONTRACT_VERSION {
            reply.status = DecideStatus::Unbound;
            return reject(reply, "contract mismatch");
        }
        if let Err(error) = request.batch.validate_identity() {
            return reject(reply, error);
        }
        if request.batch.expected_policy_revision != self.revision {
            return reject(reply, "policy revision changed");
        }
        let Some(operation) = self.config.operations.get(&request.operation) else {
            reply.status = DecideStatus::Unbound;
            return reject(reply, "operation unbound");
        };
        if !self.config.enabled || (!operation.shadow.enabled && !operation.gate.enabled) {
            reply.status = DecideStatus::Unbound;
            return reject(reply, "classification disabled");
        }
        let limits = &operation.classification;
        if let Err(error) = limits.validate() {
            return reject(reply, &error);
        }
        if request.batch.items.len() > limits.max_items {
            return reject(reply, "batch exceeds max_items");
        }
        let bytes = serde_json::to_vec(&request).map_or(usize::MAX, |v| v.len());
        if bytes > limits.max_request_bytes {
            return reject(reply, "batch exceeds max_request_bytes");
        }
        let context_bytes = request
            .batch
            .context
            .as_ref()
            .map_or(0, |c| serde_json::to_vec(c).map_or(usize::MAX, |v| v.len()));
        if context_bytes > limits.max_context_bytes {
            return reject(reply, "context exceeds max_context_bytes");
        }
        let Some(runtime) = self.runtime(request.locality) else {
            reply.status = DecideStatus::Unbound;
            return reject(reply, "no route for locality");
        };
        let cap = match request.batch.mode {
            ClassificationMode::Shadow if operation.shadow.enabled => limits.shadow_budget_ms,
            ClassificationMode::Gate if operation.gate.enabled => {
                limits.decision_budget_ms + limits.queue_budget_ms
            },
            _ => return reject(reply, "requested classification mode disabled"),
        };
        let deadline = tokio::time::Instant::from_std(started)
            + Duration::from_millis(request.batch.execution_budget_ms.min(cap));
        let slots = &self.item_slots[&request.operation];
        let dispatched: Vec<_> = request
            .batch
            .items
            .iter()
            .map(|_| AtomicBool::new(false))
            .collect();
        for (window_index, window) in request.batch.items.chunks(limits.chunk_size).enumerate() {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            for (offset, item) in window.iter().enumerate() {
                if serde_json::to_vec(item).map_or(usize::MAX, |v| v.len()) > limits.max_item_bytes
                {
                    let result = &mut reply.batch.items[window_index * limits.chunk_size + offset];
                    result.status = ItemStatus::Failed;
                    result.error = Some("item exceeds max_item_bytes".into());
                }
            }
            if operation.batch_strategy == BatchStrategy::SharedChunk {
                let mut start = 0;
                while start < window.len() {
                    if reply.batch.items[window_index * limits.chunk_size + start].status
                        == ItemStatus::Failed
                    {
                        start += 1;
                        continue;
                    }
                    let mut end = start + 1;
                    while end < window.len()
                        && reply.batch.items[window_index * limits.chunk_size + end].status
                            != ItemStatus::Failed
                    {
                        end += 1;
                    }
                    let shared = crate::shared_chunk::evaluate(
                        self,
                        runtime,
                        &request.operation,
                        &request.batch,
                        &window[start..end],
                        &dispatched[window_index * limits.chunk_size + start
                            ..window_index * limits.chunk_size + end],
                        deadline,
                        limits,
                    )
                    .await;
                    for (offset, item) in shared.into_iter().enumerate() {
                        if let Some(item) = item {
                            reply.batch.items[window_index * limits.chunk_size + start + offset] =
                                item;
                        }
                    }
                    start = end;
                }
            }
            let mut pending = FuturesUnordered::new();
            for (offset, item) in window.iter().enumerate() {
                let index = window_index * limits.chunk_size + offset;
                if matches!(
                    reply.batch.items[index].status,
                    ItemStatus::Answered | ItemStatus::Failed
                ) {
                    continue;
                }
                let state = match &request.batch.context {
                    Some(context) => DecisionState::from_json(serde_json::json!({
                        "context": context, "item": item.state
                    })),
                    None => item.state.clone(),
                };
                let single = DecideRequest {
                    batch: Default::default(),
                    contract_version: CONTRACT_VERSION,
                    operation: request.operation.clone(),
                    state,
                    choice_candidates: item.choice_candidates.clone(),
                    locality: request.locality,
                };
                let request_id = request.batch.request_id.clone();
                let dispatched = &dispatched[index];
                let shadow = request.batch.mode == ClassificationMode::Shadow;
                let background = !shadow && limits.queue_budget_ms > 0;
                pending.push(async move {
                    let _permit = slots.acquire().await;
                    dispatched.store(true, Ordering::Relaxed);
                    let response = magician_decision::telemetry::for_items(
                        request_id,
                        vec![item.item_id.clone()],
                        magician_decision::admission::with_priority(
                            shadow || background,
                            magician_decision::runtime::with_background_budget(
                                background.then_some(
                                    magician_decision::runtime::BackgroundBudget {
                                        queue: Duration::from_millis(limits.queue_budget_ms),
                                        inference: Duration::from_millis(limits.decision_budget_ms),
                                        deadline,
                                    },
                                ),
                                self.decide_before(single, (!shadow).then_some(deadline)),
                            ),
                        ),
                    )
                    .await;
                    (
                        index,
                        DecisionItemResult {
                            eligible_answers: Default::default(),
                            item_id: item.item_id.clone(),
                            status: match response.status {
                                DecideStatus::Answered => ItemStatus::Answered,
                                DecideStatus::NoFittingModel | DecideStatus::Unbound => {
                                    ItemStatus::NoFittingModel
                                },
                                DecideStatus::Failed => ItemStatus::Failed,
                            },
                            response: response.response,
                            thresholds: response.thresholds,
                            error: response.error,
                            latency_ms: response.latency_ms,
                        },
                    )
                });
            }
            loop {
                match tokio::time::timeout_at(deadline, pending.next()).await {
                    Ok(Some((index, result))) => reply.batch.items[index] = result,
                    Ok(None) | Err(_) => break,
                }
            }
            // Drop active futures before the enclosing receipt collector is drained.
            drop(pending);
        }
        for (index, result) in reply.batch.items.iter_mut().enumerate() {
            if result.status == ItemStatus::NotStarted && dispatched[index].load(Ordering::Relaxed)
            {
                result.status = ItemStatus::Cancelled;
                result.error = Some("execution budget exhausted".into());
            }
        }
        reply.batch.model_health = runtime
            .health_for(&request.operation)
            .into_iter()
            .map(|health| (health.model, health.issue))
            .collect();
        reply.status = if reply
            .batch
            .items
            .iter()
            .any(|i| i.status == ItemStatus::Answered)
        {
            DecideStatus::Answered
        } else {
            DecideStatus::Failed
        };
        reply.latency_ms = started.elapsed().as_millis() as u64;
        reply
    }
}
