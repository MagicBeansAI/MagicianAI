//! Phase 3 main loop: execute a `WorkflowGraph` end-to-end via HTTP.

use crate::magician_v2::api_mining::metrics::ReplayMetrics;
use crate::magician_v2::api_mining::types::SessionContext;
use crate::magician_v2::api_mining::workflow::{
    DataFlow, ParamSource, WorkflowGraph, WorkflowMaturity, WorkflowStep,
};
use crate::magician_v2::api_mining::workflow_replay::params::{
    resolve_param_source, DataFlowLookup,
};
use crate::magician_v2::api_mining::workflow_replay::skip::evaluate_skip;
use crate::magician_v2::api_mining::workflow_replay::step_executor::{
    is_capability_not_found, is_http_success, truncate_body_preview, StepExecutionResult,
    StepExecutor,
};
use crate::magician_v2::api_mining::workflow_replay::types::{
    BrowserFallbackRequest, MixedReplayResult, ReplayError, ReplayInputs, ReplayResult,
    StepReplayOutcome,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const REDACTED_SESSION_VALUE: &str = "[SESSION_AUTH]";
const MIN_SENSITIVE_VALUE_LEN: usize = 3;

pub struct WorkflowReplayEngine {
    executor: Arc<StepExecutor>,
    metrics: Option<Arc<ReplayMetrics>>,
}

impl WorkflowReplayEngine {
    pub fn new(executor: Arc<StepExecutor>) -> Self {
        Self {
            executor,
            metrics: None,
        }
    }

    /// Attach the per-scope replay metrics for observability. When `None`,
    /// the engine still runs — useful for tests where metrics don't matter.
    pub fn with_metrics(mut self, metrics: Arc<ReplayMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Walk the workflow step by step, returning a ReplayResult. The
    /// workflow is mutated in place to record replay_stats +
    /// last_replayed_at_ms + possibly promoted confidence.workflow_level.
    pub async fn replay(
        &self,
        workflow: &mut WorkflowGraph,
        inputs: &ReplayInputs,
        session_values: HashMap<String, String>,
        session_ctx: &SessionContext,
    ) -> ReplayResult {
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        if let Some(metrics) = &self.metrics {
            metrics.record_replay_started();
        }

        let original_maturity = workflow.confidence.workflow_level;

        if let Err(err) = check_no_browser_only_steps(workflow) {
            if let Some(metrics) = &self.metrics {
                metrics.record_failed_browser_only();
            }
            return finalize_failure(workflow, vec![], err, started_at_ms);
        }

        let data_flow_lookup = build_data_flow_lookup(workflow);
        let mut prior_responses: HashMap<String, Value> = HashMap::new();
        let mut step_outcomes: Vec<StepReplayOutcome> = Vec::new();

        let replay_started = std::time::Instant::now();

        for step in &workflow.steps {
            let step_started = std::time::Instant::now();

            // Per-workflow timeout: bail before starting a new step if we've
            // exceeded the caller's wall-clock budget. Doesn't interrupt an
            // in-flight step's HTTP call — that's the per-step timeout's
            // responsibility — but bounds total replay duration.
            if let Some(limit_ms) = inputs.timeout_ms {
                let elapsed_ms = replay_started.elapsed().as_millis() as u64;
                if elapsed_ms > limit_ms {
                    if let Some(metrics) = &self.metrics {
                        metrics.record_failed_timeout();
                    }
                    return finalize_failure(
                        workflow,
                        step_outcomes,
                        ReplayError::WorkflowTimeout {
                            elapsed_ms,
                            limit_ms,
                        },
                        started_at_ms,
                    );
                }
            }

            // Skip condition evaluation.
            if let Some(skip_cond) = &step.skip_if {
                match evaluate_skip(skip_cond, &prior_responses) {
                    Some(true) => {
                        step_outcomes.push(StepReplayOutcome {
                            step_id: step.id.clone(),
                            step_index: step.step_index,
                            skipped: true,
                            skip_reason: Some(format!(
                                "{:?} on {} {}",
                                skip_cond.operator, skip_cond.source_step, skip_cond.source_path
                            )),
                            capability_id: step.capability_id.clone(),
                            replay_method: None,
                            replay_url: None,
                            request_params: HashMap::new(),
                            request_body: None,
                            http_status: None,
                            response_body_preview: None,
                            duration_ms: step_started.elapsed().as_millis() as u64,
                        });
                        continue;
                    },
                    Some(false) => {
                        // Skip not triggered — proceed.
                    },
                    None => {
                        tracing::warn!(
                            "workflow_replay: skip_if source step '{}' has no response; \
                             failing open and executing step '{}'",
                            skip_cond.source_step,
                            step.id
                        );
                    },
                }
            }

            // browser_only steps are caught by the pre-flight check; reaching
            // here with capability_id=None means the workflow shape changed
            // after the check — surface a typed error.
            let capability_id = match step.capability_id.as_deref() {
                Some(id) => id,
                None => {
                    if let Some(metrics) = &self.metrics {
                        metrics.record_failed_browser_only();
                    }
                    step_outcomes.push(failing_step_outcome(
                        &step,
                        None,
                        None,
                        step_started.elapsed().as_millis() as u64,
                    ));
                    return finalize_failure(
                        workflow,
                        step_outcomes,
                        ReplayError::BrowserOnlyStep {
                            step_id: step.id.clone(),
                        },
                        started_at_ms,
                    );
                },
            };
            if let Err(err) = self
                .ensure_step_capability_replayable(workflow, &step.id, capability_id)
                .await
            {
                if let Some(metrics) = &self.metrics {
                    metrics.record_failed_http();
                }
                step_outcomes.push(failing_step_outcome(
                    step,
                    None,
                    None,
                    step_started.elapsed().as_millis() as u64,
                ));
                return finalize_failure(workflow, step_outcomes, err, started_at_ms);
            }

            // Resolve parameters.
            let mut resolved_params: HashMap<String, String> = HashMap::new();
            for (param_name, source) in &step.param_sources {
                match resolve_param_source(
                    source,
                    param_name,
                    &step.id,
                    &data_flow_lookup,
                    &prior_responses,
                    &inputs.user_inputs,
                    &session_values,
                ) {
                    Ok(v) => {
                        resolved_params.insert(param_name.clone(), v);
                    },
                    Err(err) => {
                        if let Some(metrics) = &self.metrics {
                            metrics.record_failed_param_resolution();
                        }
                        step_outcomes.push(failing_step_outcome(
                            step,
                            None,
                            None,
                            step_started.elapsed().as_millis() as u64,
                        ));
                        return finalize_failure(workflow, step_outcomes, err, started_at_ms);
                    },
                }
            }

            // Execute HTTP.
            match self
                .execute_step_with_budget(
                    replay_started,
                    inputs.timeout_ms,
                    &step.id,
                    &workflow.origin_key,
                    capability_id,
                    &resolved_params,
                    session_ctx,
                )
                .await
            {
                Ok(exec_result) => {
                    let preview = truncate_body_preview(&exec_result.response_body_text);
                    if !is_http_success(exec_result.http_status) {
                        if let Some(metrics) = &self.metrics {
                            metrics.record_failed_http();
                        }
                        step_outcomes.push(failing_step_outcome(
                            step,
                            Some(exec_result.http_status),
                            Some(preview.clone()),
                            exec_result.duration_ms,
                        ));
                        return finalize_failure(
                            workflow,
                            step_outcomes,
                            ReplayError::HttpFailure {
                                step_id: step.id.clone(),
                                status: exec_result.http_status,
                                body_preview: Some(preview),
                            },
                            started_at_ms,
                        );
                    }
                    // Always insert the step's response into prior_responses
                    // (even when the body isn't valid JSON) so downstream
                    // DataFlow resolution surfaces a precise JsonPathMiss
                    // instead of the misleading "source step has not run"
                    // error. Non-JSON bodies are wrapped as Value::String;
                    // JSONPath lookups on a string value cleanly return
                    // None, which the resolver maps to JsonPathMiss.
                    let body_value = exec_result
                        .response_body_json
                        .clone()
                        .unwrap_or_else(|| Value::String(exec_result.response_body_text.clone()));
                    prior_responses.insert(step.id.clone(), body_value);
                    step_outcomes.push(successful_step_outcome(
                        step,
                        capability_id,
                        &exec_result,
                        session_ctx,
                        preview,
                    ));
                },
                Err(err) => {
                    if let Some(metrics) = &self.metrics {
                        match &err {
                            ReplayError::WorkflowTimeout { .. } => {
                                metrics.record_failed_timeout();
                            },
                            _ => metrics.record_failed_http(),
                        }
                    }
                    step_outcomes.push(failing_step_outcome(
                        &step,
                        None,
                        None,
                        step_started.elapsed().as_millis() as u64,
                    ));
                    return finalize_failure(workflow, step_outcomes, err, started_at_ms);
                },
            }
        }

        // All steps succeeded — promote maturity.
        let finished_at_ms = chrono::Utc::now().timestamp_millis();
        workflow.replay_stats.successful_replays =
            workflow.replay_stats.successful_replays.saturating_add(1);
        workflow.last_replayed_at_ms = Some(finished_at_ms);
        let new_maturity = promote_maturity(
            workflow.confidence.workflow_level,
            workflow.replay_stats.successful_replays,
            workflow.replay_stats.failed_replays,
        );
        workflow.confidence.workflow_level = new_maturity;

        if let Some(metrics) = &self.metrics {
            metrics.record_replay_succeeded();
            if new_maturity != original_maturity {
                match new_maturity {
                    WorkflowMaturity::Candidate => metrics.record_promoted_to_candidate(),
                    WorkflowMaturity::Validated => metrics.record_promoted_to_validated(),
                    WorkflowMaturity::Trusted => metrics.record_promoted_to_trusted(),
                    WorkflowMaturity::Draft => {},
                }
            }
        }

        ReplayResult {
            workflow_id: workflow.id.clone(),
            origin_key: workflow.origin_key.clone(),
            steps: step_outcomes,
            success: true,
            failure: None,
            started_at_ms,
            finished_at_ms,
        }
    }

    /// Execute the API prefix of a workflow and stop at the first step that
    /// needs the browser rail. This does not execute browser actions itself; it
    /// returns the captured browser primitive payload so the normal task
    /// executor can continue with its existing browser session, cancellation,
    /// screenshots, and sequence recording.
    pub async fn replay_until_browser_fallback(
        &self,
        workflow: &mut WorkflowGraph,
        inputs: &ReplayInputs,
        session_values: HashMap<String, String>,
        session_ctx: &SessionContext,
    ) -> MixedReplayResult {
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        if let Some(metrics) = &self.metrics {
            metrics.record_replay_started();
        }

        let original_maturity = workflow.confidence.workflow_level;
        let data_flow_lookup = build_data_flow_lookup(workflow);
        let mut prior_responses: HashMap<String, Value> = HashMap::new();
        let mut step_outcomes: Vec<StepReplayOutcome> = Vec::new();
        let replay_started = std::time::Instant::now();

        for step in workflow.steps.clone() {
            let step_started = std::time::Instant::now();

            if let Some(limit_ms) = inputs.timeout_ms {
                let elapsed_ms = replay_started.elapsed().as_millis() as u64;
                if elapsed_ms > limit_ms {
                    let err = ReplayError::WorkflowTimeout {
                        elapsed_ms,
                        limit_ms,
                    };
                    return mixed_failure_or_fallback(
                        workflow,
                        &step,
                        step_outcomes,
                        err,
                        "workflow timeout before step",
                        started_at_ms,
                    );
                }
            }

            if let Some(skip_cond) = &step.skip_if {
                match evaluate_skip(skip_cond, &prior_responses) {
                    Some(true) => {
                        step_outcomes.push(StepReplayOutcome {
                            step_id: step.id.clone(),
                            step_index: step.step_index,
                            skipped: true,
                            skip_reason: Some(format!(
                                "{:?} on {} {}",
                                skip_cond.operator, skip_cond.source_step, skip_cond.source_path
                            )),
                            capability_id: step.capability_id.clone(),
                            replay_method: None,
                            replay_url: None,
                            request_params: HashMap::new(),
                            request_body: None,
                            http_status: None,
                            response_body_preview: None,
                            duration_ms: step_started.elapsed().as_millis() as u64,
                        });
                        continue;
                    },
                    Some(false) => {},
                    None => {
                        tracing::warn!(
                            "workflow_replay: skip_if source step '{}' has no response; \
                             failing open and executing step '{}'",
                            skip_cond.source_step,
                            step.id
                        );
                    },
                }
            }

            let capability_id = match step.capability_id.as_deref() {
                Some(id) if !step.browser_only => id,
                _ => {
                    let err = ReplayError::BrowserOnlyStep {
                        step_id: step.id.clone(),
                    };
                    step_outcomes.push(failing_step_outcome(
                        &step,
                        None,
                        None,
                        step_started.elapsed().as_millis() as u64,
                    ));
                    return mixed_failure_or_fallback(
                        workflow,
                        &step,
                        step_outcomes,
                        err,
                        "browser-only step",
                        started_at_ms,
                    );
                },
            };

            if let Err(err) = self
                .ensure_step_capability_replayable(workflow, &step.id, capability_id)
                .await
            {
                step_outcomes.push(failing_step_outcome(
                    &step,
                    None,
                    None,
                    step_started.elapsed().as_millis() as u64,
                ));
                return mixed_failure_or_fallback(
                    workflow,
                    &step,
                    step_outcomes,
                    err,
                    "capability is not replayable",
                    started_at_ms,
                );
            }

            let mut resolved_params: HashMap<String, String> = HashMap::new();
            for (param_name, source) in &step.param_sources {
                match resolve_param_source(
                    source,
                    param_name,
                    &step.id,
                    &data_flow_lookup,
                    &prior_responses,
                    &inputs.user_inputs,
                    &session_values,
                ) {
                    Ok(v) => {
                        resolved_params.insert(param_name.clone(), v);
                    },
                    Err(err) => {
                        step_outcomes.push(failing_step_outcome(
                            &step,
                            None,
                            None,
                            step_started.elapsed().as_millis() as u64,
                        ));
                        return mixed_failure_or_fallback(
                            workflow,
                            &step,
                            step_outcomes,
                            err,
                            "parameter resolution failed",
                            started_at_ms,
                        );
                    },
                }
            }

            match self
                .execute_step_with_budget(
                    replay_started,
                    inputs.timeout_ms,
                    &step.id,
                    &workflow.origin_key,
                    capability_id,
                    &resolved_params,
                    session_ctx,
                )
                .await
            {
                Ok(exec_result) => {
                    let preview = truncate_body_preview(&exec_result.response_body_text);
                    if !is_http_success(exec_result.http_status) {
                        let err = ReplayError::HttpFailure {
                            step_id: step.id.clone(),
                            status: exec_result.http_status,
                            body_preview: Some(preview.clone()),
                        };
                        step_outcomes.push(failing_step_outcome(
                            &step,
                            Some(exec_result.http_status),
                            Some(preview),
                            exec_result.duration_ms,
                        ));
                        return mixed_failure_or_fallback(
                            workflow,
                            &step,
                            step_outcomes,
                            err,
                            "API replay returned non-success HTTP status",
                            started_at_ms,
                        );
                    }
                    let body_value = exec_result
                        .response_body_json
                        .clone()
                        .unwrap_or_else(|| Value::String(exec_result.response_body_text.clone()));
                    prior_responses.insert(step.id.clone(), body_value);
                    step_outcomes.push(successful_step_outcome(
                        &step,
                        capability_id,
                        &exec_result,
                        session_ctx,
                        preview,
                    ));
                },
                Err(err) => {
                    if let Some(metrics) = &self.metrics {
                        match &err {
                            ReplayError::WorkflowTimeout { .. } => {
                                metrics.record_failed_timeout();
                            },
                            _ => metrics.record_failed_http(),
                        }
                    }
                    step_outcomes.push(failing_step_outcome(
                        &step,
                        None,
                        None,
                        step_started.elapsed().as_millis() as u64,
                    ));
                    return mixed_failure_or_fallback(
                        workflow,
                        &step,
                        step_outcomes,
                        err,
                        "API replay failed",
                        started_at_ms,
                    );
                },
            }
        }

        let finished_at_ms = chrono::Utc::now().timestamp_millis();
        workflow.replay_stats.successful_replays =
            workflow.replay_stats.successful_replays.saturating_add(1);
        workflow.last_replayed_at_ms = Some(finished_at_ms);
        let new_maturity = promote_maturity(
            workflow.confidence.workflow_level,
            workflow.replay_stats.successful_replays,
            workflow.replay_stats.failed_replays,
        );
        workflow.confidence.workflow_level = new_maturity;
        if let Some(metrics) = &self.metrics {
            metrics.record_replay_succeeded();
            if new_maturity != original_maturity {
                match new_maturity {
                    WorkflowMaturity::Candidate => metrics.record_promoted_to_candidate(),
                    WorkflowMaturity::Validated => metrics.record_promoted_to_validated(),
                    WorkflowMaturity::Trusted => metrics.record_promoted_to_trusted(),
                    WorkflowMaturity::Draft => {},
                }
            }
        }

        MixedReplayResult {
            workflow_id: workflow.id.clone(),
            origin_key: workflow.origin_key.clone(),
            steps: step_outcomes,
            success: true,
            fallback_required: false,
            fallback: None,
            failure: None,
            started_at_ms,
            finished_at_ms,
        }
    }

    async fn ensure_step_capability_replayable(
        &self,
        workflow: &WorkflowGraph,
        step_id: &str,
        capability_id: &str,
    ) -> Result<(), ReplayError> {
        match self
            .executor
            .can_replay(&workflow.origin_key, capability_id)
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(ReplayError::WorkflowStale {
                workflow_id: workflow.id.clone(),
            }),
            Err(err) if is_capability_not_found(&err) => Err(ReplayError::UnknownCapability {
                step_id: step_id.to_string(),
                capability_id: capability_id.to_string(),
            }),
            Err(err) => Err(ReplayError::NetworkError {
                step_id: step_id.to_string(),
                message: err,
            }),
        }
    }

    async fn execute_step_with_budget(
        &self,
        replay_started: Instant,
        timeout_ms: Option<u64>,
        step_id: &str,
        origin_key: &str,
        capability_id: &str,
        resolved_params: &HashMap<String, String>,
        session_ctx: &SessionContext,
    ) -> Result<StepExecutionResult, ReplayError> {
        let execute = self.executor.execute(
            step_id,
            origin_key,
            capability_id,
            resolved_params,
            session_ctx,
        );
        let Some(limit_ms) = timeout_ms else {
            return execute.await;
        };

        let elapsed_ms = replay_started.elapsed().as_millis() as u64;
        if elapsed_ms > limit_ms {
            return Err(ReplayError::WorkflowTimeout {
                elapsed_ms,
                limit_ms,
            });
        }
        let remaining_ms = limit_ms.saturating_sub(elapsed_ms).max(1);
        match tokio::time::timeout(Duration::from_millis(remaining_ms), execute).await {
            Ok(result) => result,
            Err(_) => Err(ReplayError::WorkflowTimeout {
                elapsed_ms: replay_started.elapsed().as_millis() as u64,
                limit_ms,
            }),
        }
    }
}

fn successful_step_outcome(
    step: &WorkflowStep,
    capability_id: &str,
    exec_result: &StepExecutionResult,
    session_ctx: &SessionContext,
    response_body_preview: String,
) -> StepReplayOutcome {
    StepReplayOutcome {
        step_id: step.id.clone(),
        step_index: step.step_index,
        skipped: false,
        skip_reason: None,
        capability_id: Some(capability_id.to_string()),
        replay_method: exec_result
            .replay_request
            .as_ref()
            .map(|request| request.method.clone()),
        replay_url: exec_result.replay_request.as_ref().map(|request| {
            redact_sensitive_replay_text(
                request.url.clone(),
                &step.param_sources,
                &exec_result.request_params,
                session_ctx,
            )
        }),
        request_params: redacted_request_params(&step.param_sources, &exec_result.request_params),
        request_body: exec_result.replay_request.as_ref().and_then(|request| {
            request.body.clone().map(|body| {
                redact_sensitive_replay_text(
                    body,
                    &step.param_sources,
                    &exec_result.request_params,
                    session_ctx,
                )
            })
        }),
        http_status: Some(exec_result.http_status),
        response_body_preview: Some(response_body_preview),
        duration_ms: exec_result.duration_ms,
    }
}

fn redacted_request_params(
    param_sources: &HashMap<String, ParamSource>,
    params: &HashMap<String, String>,
) -> HashMap<String, String> {
    params
        .iter()
        .map(|(key, value)| {
            let safe_value = if matches!(
                param_sources.get(key),
                Some(ParamSource::SessionAuth { .. })
            ) {
                REDACTED_SESSION_VALUE.to_string()
            } else {
                value.clone()
            };
            (key.clone(), safe_value)
        })
        .collect()
}

fn redact_sensitive_replay_text(
    mut text: String,
    param_sources: &HashMap<String, ParamSource>,
    params: &HashMap<String, String>,
    session_ctx: &SessionContext,
) -> String {
    for value in sensitive_values(param_sources, params, session_ctx) {
        text = text.replace(&value, REDACTED_SESSION_VALUE);
    }
    text
}

fn sensitive_values(
    param_sources: &HashMap<String, ParamSource>,
    params: &HashMap<String, String>,
    session_ctx: &SessionContext,
) -> Vec<String> {
    let mut values = Vec::new();

    for (key, source) in param_sources {
        if matches!(source, ParamSource::SessionAuth { .. }) {
            if let Some(value) = params.get(key) {
                push_sensitive_value(&mut values, value);
            }
        }
    }

    for value in session_ctx.auth_headers.values() {
        push_sensitive_value(&mut values, value);
    }
    for value in session_ctx.auth_query_params.values() {
        push_sensitive_value(&mut values, value);
    }
    for value in session_ctx.cookies.values() {
        push_sensitive_value(&mut values, value);
    }
    for cookie in &session_ctx.cookie_header_values {
        push_sensitive_value(&mut values, &cookie.value);
    }
    if let Some(cookie_header) = session_ctx.cookie_header_string() {
        push_sensitive_value(&mut values, &cookie_header);
    }
    for value in session_ctx.local_storage.values() {
        push_sensitive_value(&mut values, value);
    }
    for value in session_ctx.session_storage.values() {
        push_sensitive_value(&mut values, value);
    }

    values.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    values.dedup();
    values
}

fn push_sensitive_value(values: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.len() >= MIN_SENSITIVE_VALUE_LEN {
        values.push(value.to_string());
    }
}

/// Build a `StepReplayOutcome` for a step that failed mid-execution. Used at
/// every failure call-site inside the per-step loop so the returned
/// `ReplayResult.steps` always contains the failing step (with whatever
/// timing / status / body we got before the failure) — not just the
/// successful predecessors.
fn failing_step_outcome(
    step: &crate::magician_v2::api_mining::workflow::WorkflowStep,
    http_status: Option<u16>,
    response_body_preview: Option<String>,
    duration_ms: u64,
) -> StepReplayOutcome {
    StepReplayOutcome {
        step_id: step.id.clone(),
        step_index: step.step_index,
        skipped: false,
        skip_reason: None,
        capability_id: step.capability_id.clone(),
        replay_method: None,
        replay_url: None,
        request_params: HashMap::new(),
        request_body: None,
        http_status,
        response_body_preview,
        duration_ms,
    }
}

pub fn check_no_browser_only_steps(wf: &WorkflowGraph) -> Result<(), ReplayError> {
    if let Some(step) = wf.steps.iter().find(|s| s.browser_only) {
        return Err(ReplayError::BrowserOnlyStep {
            step_id: step.id.clone(),
        });
    }
    Ok(())
}

pub fn build_data_flow_lookup(wf: &WorkflowGraph) -> HashMap<String, DataFlowLookup> {
    wf.data_flows
        .iter()
        .map(|df: &DataFlow| {
            (
                df.id.clone(),
                DataFlowLookup {
                    source_step: df.source_step.clone(),
                    source_path: df.source_path.clone(),
                },
            )
        })
        .collect()
}

/// Promotion thresholds:
/// - Draft → Candidate: 1+ successful replays.
/// - Candidate → Validated: 3+ successful replays.
/// - Validated → Trusted: 10+ total AND failure rate < 10%.
///
/// Failed-only replays do not demote here; demotion is a follow-up
/// (workflow invalidation on capability demotion).
pub fn promote_maturity(
    current: WorkflowMaturity,
    successful: u32,
    failed: u32,
) -> WorkflowMaturity {
    let total = successful.saturating_add(failed);
    if total >= 10 {
        let failure_rate = failed as f32 / total as f32;
        if failure_rate < 0.10 {
            return WorkflowMaturity::Trusted;
        }
    }
    if successful >= 3 {
        return WorkflowMaturity::Validated;
    }
    if successful >= 1 {
        return WorkflowMaturity::Candidate;
    }
    current
}

fn finalize_failure(
    workflow: &mut WorkflowGraph,
    steps: Vec<StepReplayOutcome>,
    err: ReplayError,
    started_at_ms: i64,
) -> ReplayResult {
    workflow.replay_stats.failed_replays = workflow.replay_stats.failed_replays.saturating_add(1);
    let finished_at_ms = chrono::Utc::now().timestamp_millis();
    workflow.last_replayed_at_ms = Some(finished_at_ms);
    ReplayResult {
        workflow_id: workflow.id.clone(),
        origin_key: workflow.origin_key.clone(),
        steps,
        success: false,
        failure: Some(err),
        started_at_ms,
        finished_at_ms,
    }
}

fn mixed_failure_or_fallback(
    workflow: &mut WorkflowGraph,
    step: &crate::magician_v2::api_mining::workflow::WorkflowStep,
    steps: Vec<StepReplayOutcome>,
    err: ReplayError,
    reason: &str,
    started_at_ms: i64,
) -> MixedReplayResult {
    if step.browser_fallback.is_some() {
        return mixed_fallback_result(workflow, step, steps, err, reason, started_at_ms);
    }

    let replay = finalize_failure(workflow, steps, err, started_at_ms);
    MixedReplayResult {
        workflow_id: replay.workflow_id,
        origin_key: replay.origin_key,
        steps: replay.steps,
        success: false,
        fallback_required: false,
        fallback: None,
        failure: replay.failure,
        started_at_ms: replay.started_at_ms,
        finished_at_ms: replay.finished_at_ms,
    }
}

fn mixed_fallback_result(
    workflow: &mut WorkflowGraph,
    step: &crate::magician_v2::api_mining::workflow::WorkflowStep,
    steps: Vec<StepReplayOutcome>,
    err: ReplayError,
    reason: &str,
    started_at_ms: i64,
) -> MixedReplayResult {
    let finished_at_ms = chrono::Utc::now().timestamp_millis();
    workflow.last_replayed_at_ms = Some(finished_at_ms);
    MixedReplayResult {
        workflow_id: workflow.id.clone(),
        origin_key: workflow.origin_key.clone(),
        steps,
        success: false,
        fallback_required: true,
        fallback: Some(BrowserFallbackRequest {
            step_id: step.id.clone(),
            step_index: step.step_index,
            reason: reason.to_string(),
            replay_error: Some(err),
            browser: step.browser_fallback.clone(),
        }),
        failure: None,
        started_at_ms,
        finished_at_ms,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::replay::{ApiRunner, ReplayRequest};
    use crate::magician_v2::api_mining::types::{SessionContext, SessionCookie};
    use crate::magician_v2::api_mining::workflow::{
        AuthRequirements, BrowserFallbackStep, DataFlow, InferenceMethod, ReplayStats,
        WorkflowConfidence, WorkflowGraph, WorkflowMaturity, WorkflowStep,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn minimal_workflow() -> WorkflowGraph {
        WorkflowGraph {
            id: "wf_test".to_string(),
            origin_key: "example.com".to_string(),
            name: "test".to_string(),
            steps: vec![WorkflowStep {
                id: "step_0".to_string(),
                step_index: 0,
                capability_id: Some("cap_login".to_string()),
                param_sources: HashMap::new(),
                skip_if: None,
                browser_only: false,
                browser_fallback: None,
            }],
            data_flows: vec![],
            auth_requirements: AuthRequirements::default(),
            confidence: WorkflowConfidence {
                workflow_level: WorkflowMaturity::Draft,
                step_confidences: HashMap::new(),
            },
            compiled_from_sequence_ids: vec!["seq_1".to_string()],
            last_compiled_at_ms: 0,
            last_replayed_at_ms: None,
            replay_stats: ReplayStats::default(),
        }
    }

    #[test]
    fn engine_rejects_browser_only_steps_before_execution() {
        let mut wf = minimal_workflow();
        wf.steps[0].browser_only = true;
        wf.steps[0].capability_id = None;
        let err = check_no_browser_only_steps(&wf).unwrap_err();
        match err {
            ReplayError::BrowserOnlyStep { step_id } => assert_eq!(step_id, "step_0"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn engine_accepts_workflow_without_browser_only_steps() {
        let wf = minimal_workflow();
        assert!(check_no_browser_only_steps(&wf).is_ok());
    }

    #[test]
    fn build_data_flow_lookup_indexes_by_id() {
        let mut wf = minimal_workflow();
        wf.data_flows.push(DataFlow {
            id: "df_a".to_string(),
            source_step: "step_0".to_string(),
            source_path: "$.token".to_string(),
            target_step: "step_1".to_string(),
            target_param: "auth".to_string(),
            inference_method: InferenceMethod::AutoMatch,
            confidence: 0.9,
        });
        let lookup = build_data_flow_lookup(&wf);
        assert_eq!(lookup.len(), 1);
        assert_eq!(lookup.get("df_a").unwrap().source_step, "step_0");
        assert_eq!(lookup.get("df_a").unwrap().source_path, "$.token");
    }

    #[test]
    fn promote_maturity_draft_to_candidate_on_first_success() {
        let new = promote_maturity(WorkflowMaturity::Draft, 1, 0);
        assert_eq!(new, WorkflowMaturity::Candidate);
    }

    #[test]
    fn promote_maturity_candidate_to_validated_after_three() {
        let new = promote_maturity(WorkflowMaturity::Candidate, 3, 0);
        assert_eq!(new, WorkflowMaturity::Validated);
    }

    #[test]
    fn promote_maturity_validated_to_trusted_with_low_failure_rate() {
        let new = promote_maturity(WorkflowMaturity::Validated, 10, 0);
        assert_eq!(new, WorkflowMaturity::Trusted);
    }

    #[test]
    fn promote_maturity_stays_below_threshold() {
        let new = promote_maturity(WorkflowMaturity::Draft, 0, 0);
        assert_eq!(new, WorkflowMaturity::Draft);
    }

    #[test]
    fn promote_maturity_holds_at_validated_with_high_failure_rate() {
        // 10 attempts but 3 failed = 30% failure rate > 10% threshold.
        let new = promote_maturity(WorkflowMaturity::Validated, 7, 3);
        assert_eq!(new, WorkflowMaturity::Validated);
    }

    #[test]
    fn successful_step_outcome_redacts_session_auth_observability_fields() {
        let mut step = minimal_workflow().steps.remove(0);
        step.param_sources.insert(
            "bearer".to_string(),
            ParamSource::SessionAuth {
                auth_scheme: "bearer_token".to_string(),
            },
        );
        step.param_sources.insert(
            "q".to_string(),
            ParamSource::Literal {
                value: "americano".to_string(),
            },
        );

        let exec_result = StepExecutionResult {
            http_status: 200,
            response_body_text: "{}".to_string(),
            response_body_json: None,
            duration_ms: 12,
            replay_request: Some(ReplayRequest {
                method: "POST".to_string(),
                url: "https://example.com/api/search?token=secret-query-456&bearer=secret-bearer-123&q=americano".to_string(),
                headers: HashMap::new(),
                timeout_ms: 1000,
                body: Some(
                    r#"{"token":"secret-bearer-123","cookie":"secret-cookie-789","q":"americano"}"#
                        .to_string(),
                ),
            }),
            request_params: HashMap::from([
                ("bearer".to_string(), "secret-bearer-123".to_string()),
                ("q".to_string(), "americano".to_string()),
            ]),
        };
        let session_ctx = SessionContext {
            cookie_header_values: vec![SessionCookie {
                name: "SID".to_string(),
                value: "secret-cookie-789".to_string(),
            }],
            auth_headers: HashMap::from([(
                "authorization".to_string(),
                "Bearer secret-bearer-123".to_string(),
            )]),
            auth_query_params: HashMap::from([(
                "token".to_string(),
                "secret-query-456".to_string(),
            )]),
            ..SessionContext::default()
        };

        let outcome =
            successful_step_outcome(&step, "cap_search", &exec_result, &session_ctx, "{}".into());

        assert_eq!(
            outcome.request_params.get("bearer").map(String::as_str),
            Some(REDACTED_SESSION_VALUE)
        );
        assert_eq!(
            outcome.request_params.get("q").map(String::as_str),
            Some("americano")
        );
        let serialized = serde_json::to_string(&outcome).expect("serialize outcome");
        assert!(!serialized.contains("secret-bearer-123"));
        assert!(!serialized.contains("secret-query-456"));
        assert!(!serialized.contains("secret-cookie-789"));
        assert!(serialized.contains(REDACTED_SESSION_VALUE));
    }

    fn test_engine() -> WorkflowReplayEngine {
        let temp = tempfile::tempdir().expect("temp dir");
        let runner = ApiRunner::with_base_path(temp.path()).expect("runner");
        let executor = Arc::new(StepExecutor::new(Arc::new(Mutex::new(runner))));
        WorkflowReplayEngine::new(executor)
    }

    #[tokio::test]
    async fn mixed_replay_stops_at_executable_browser_fallback_without_failure_count() {
        let engine = test_engine();
        let mut wf = minimal_workflow();
        wf.steps[0].capability_id = None;
        wf.steps[0].browser_only = true;
        wf.steps[0].browser_fallback = Some(BrowserFallbackStep {
            action: "click".to_string(),
            arguments: serde_json::json!({"args":["#continue"]}),
            description: Some("browser__click #continue".to_string()),
        });

        let result = engine
            .replay_until_browser_fallback(
                &mut wf,
                &ReplayInputs::default(),
                HashMap::new(),
                &SessionContext::default(),
            )
            .await;

        assert!(!result.success);
        assert!(result.fallback_required);
        assert!(result.failure.is_none());
        assert_eq!(wf.replay_stats.failed_replays, 0);
        let fallback = result.fallback.expect("fallback");
        assert_eq!(fallback.step_id, "step_0");
        assert_eq!(
            fallback.browser.map(|browser| browser.action),
            Some("click".to_string())
        );
    }

    #[tokio::test]
    async fn mixed_replay_browser_only_without_payload_is_unrecoverable_failure() {
        let engine = test_engine();
        let mut wf = minimal_workflow();
        wf.steps[0].capability_id = None;
        wf.steps[0].browser_only = true;

        let result = engine
            .replay_until_browser_fallback(
                &mut wf,
                &ReplayInputs::default(),
                HashMap::new(),
                &SessionContext::default(),
            )
            .await;

        assert!(!result.success);
        assert!(!result.fallback_required);
        assert!(matches!(
            result.failure,
            Some(ReplayError::BrowserOnlyStep { .. })
        ));
        assert_eq!(wf.replay_stats.failed_replays, 1);
    }
}
