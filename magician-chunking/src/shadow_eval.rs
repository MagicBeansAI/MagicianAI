use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering, Arc, Mutex},
    time::Instant,
};

use async_trait::async_trait;
use magicllm::{
    config::OperationProfileSelector, ChunkBudget, ChunkFallbackPolicy, ConfiguredRouter,
    ConservativeOllamaEstimator, DispatchedResponse, LLMError, LLMProfile, LLMRequest,
    LLMResponseFormat, LLMRouterConfig, LogicalLlmRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::readiness::{
    build_plan_only_diagnostics, build_release_readiness, ChunkReleaseReadiness,
    PlanOnlyDiagnostics,
};
use magician::magician_v2::llm_chunking::{
    ChunkDomainAdapterRegistry, LogicalChunkDispatch, LogicalChunkDispatchRequest,
    LogicalChunkExecutionContext, LogicalChunkRunner, LogicalChunkTelemetryEvent,
    LogicalChunkTelemetrySink, CHUNK_RELEASE_CANDIDATES,
};

#[derive(Debug, Clone, Deserialize)]
pub struct ChunkEvalFixtureSuite {
    pub baseline_reference: String,
    pub cases: Vec<ChunkEvalFixtureCase>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChunkEvalFixtureCase {
    pub name: String,
    pub operation: String,
    pub adapter_id: String,
    pub input: Value,
    #[serde(default)]
    pub required_top_level_keys: Vec<String>,
    #[serde(default)]
    pub required_output_substrings: Vec<String>,
    #[serde(default)]
    pub forbidden_output_substrings: Vec<String>,
    /// Runtime-owned archive membership that must be covered exactly once.
    #[serde(default)]
    pub required_source_episode_ids: Vec<String>,
    /// Maximum durable archive-root size accepted by the checkpoint contract.
    #[serde(default)]
    pub maximum_archive_group_episodes: Option<usize>,
    #[serde(default = "default_minimum_chunk_count")]
    pub minimum_chunk_count: usize,
}

const fn default_minimum_chunk_count() -> usize {
    1
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkShadowRunResult {
    pub repeat: u32,
    pub success: bool,
    pub schema_valid: bool,
    pub golden_valid: bool,
    pub duration_ms: u64,
    pub output_sha256: Option<String>,
    pub top_level_keys: Vec<String>,
    pub logical_chunking: Option<Value>,
    pub validation_errors: Vec<String>,
    pub error: Option<String>,
    pub durable_writes: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkShadowCaseReport {
    pub name: String,
    pub operation: String,
    pub adapter_id: String,
    pub candidate_profile: String,
    pub baseline_profile: String,
    pub plan: Option<PlanOnlyDiagnostics>,
    pub plan_error: Option<String>,
    pub runs: Vec<ChunkShadowRunResult>,
    pub cloud_runs: Vec<ChunkShadowRunResult>,
    pub success_rate: Option<f64>,
    pub schema_validity_rate: Option<f64>,
    pub golden_validity_rate: Option<f64>,
    pub latency_p50_ms: Option<u64>,
    pub latency_p90_ms: Option<u64>,
    pub cloud_success_rate: Option<f64>,
    pub cloud_schema_validity_rate: Option<f64>,
    pub cloud_golden_validity_rate: Option<f64>,
    pub cloud_latency_p50_ms: Option<u64>,
    pub cloud_latency_p90_ms: Option<u64>,
    pub local_cloud_top_level_agreement_rate: Option<f64>,
    pub local_repair_rate: Option<f64>,
    pub local_fallback_call_rate: Option<f64>,
    pub logical_latency_limit_ms: Option<u64>,
    pub slo_pass: Option<bool>,
    pub slo_failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkShadowEvalReport {
    pub schema_version: &'static str,
    pub mode: &'static str,
    pub repeats: u32,
    pub baseline_reference: String,
    pub cloud_comparison_executed: bool,
    pub queue_impact_measured: bool,
    pub persistence_attached: bool,
    pub durable_writes: u32,
    pub readiness: ChunkReleaseReadiness,
    pub cases: Vec<ChunkShadowCaseReport>,
    pub all_plans_valid: bool,
    pub all_live_runs_valid: Option<bool>,
}

struct DirectRouterDispatch {
    router: Arc<ConfiguredRouter>,
}

#[derive(Default)]
struct EvalLogicalChunkTelemetry {
    validation_errors: Mutex<BTreeMap<String, Vec<String>>>,
}

impl EvalLogicalChunkTelemetry {
    fn take_validation_errors(&self, logical_call_id: &str) -> Vec<String> {
        self.validation_errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(logical_call_id)
            .unwrap_or_default()
    }
}

impl LogicalChunkTelemetrySink for EvalLogicalChunkTelemetry {
    fn emit(&self, event: LogicalChunkTelemetryEvent) {
        let LogicalChunkTelemetryEvent::PhysicalCompleted {
            logical_call_id,
            validation_error: Some(validation_error),
            ..
        } = event
        else {
            return;
        };
        let mut errors = self
            .validation_errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entries = errors.entry(logical_call_id).or_default();
        if entries.len() < 16 && !entries.contains(&validation_error) {
            entries.push(validation_error);
        }
    }
}

#[async_trait]
impl LogicalChunkDispatch for DirectRouterDispatch {
    async fn dispatch(
        &self,
        mut request: LogicalChunkDispatchRequest,
    ) -> Result<DispatchedResponse, LLMError> {
        if request.cancellation.is_cancelled() {
            return Err(LLMError::Cancelled {
                reason: "logical_chunk_shadow_cancelled".to_string(),
            });
        }
        let trace_context = request
            .request
            .metadata
            .ensure_trace_context(None, magicllm::LlmWorkloadClass::Evaluation);
        let provider_attempt_counter = request.request.metadata.ensure_provider_attempt_counter();
        let started = Instant::now();
        let mut response = if let Some(deadline) = request.submission_deadline {
            tokio::select! {
                result = self.router.route(request.request) => result?,
                _ = request.cancellation.cancelled() => {
                    return Err(LLMError::Cancelled {
                        reason: "logical_chunk_shadow_cancelled".to_string(),
                    });
                }
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    return Err(LLMError::DeadlineExceeded);
                }
            }
        } else {
            tokio::select! {
                result = self.router.route(request.request) => result?,
                _ = request.cancellation.cancelled() => {
                    return Err(LLMError::Cancelled {
                        reason: "logical_chunk_shadow_cancelled".to_string(),
                    });
                }
            }
        };
        let provider_attempt_count = provider_attempt_counter.load(Ordering::Relaxed);
        if provider_attempt_count == 0 {
            return Err(LLMError::Other(
                "router returned success without a physical provider attempt".to_string(),
            ));
        }
        let trace_receipt = response.trace_receipt.clone().unwrap_or_else(|| {
            magicllm::LlmTraceReceipt::direct_with_attempt_count(
                trace_context,
                provider_attempt_count,
            )
        });
        if trace_receipt.provider_attempt_count != provider_attempt_count {
            return Err(LLMError::Other(format!(
                "router receipt attempt count {} disagrees with observed physical count {provider_attempt_count}",
                trace_receipt.provider_attempt_count
            )));
        }
        response.trace_receipt = Some(trace_receipt.clone());
        Ok(DispatchedResponse {
            response: Arc::new(response),
            wait: std::time::Duration::ZERO,
            execution: started.elapsed(),
            local_prep: None,
            attempts: 1,
            trace_receipt,
        })
    }
}

/// Run the provider-free planner or the explicit local shadow lane. Neither
/// mode receives a memory store, repository, or persistence callback.
pub async fn run_chunk_shadow_eval(
    config: &LLMRouterConfig,
    registry: ChunkDomainAdapterRegistry,
    fixtures: ChunkEvalFixtureSuite,
    execute: bool,
    compare_cloud: bool,
    repeats: u32,
) -> Result<ChunkShadowEvalReport, String> {
    if fixtures.cases.is_empty() {
        return Err("fixture suite must contain at least one case".to_string());
    }
    let repeats = repeats.max(1);
    if compare_cloud && !execute {
        return Err("cloud comparison requires live execution".to_string());
    }
    let readiness = build_release_readiness(config, &registry);
    if execute && !readiness.shadow_ready {
        return Err(format!(
            "release-candidate config is not evaluation-ready: {}",
            readiness.issues.join("; ")
        ));
    }
    let mut eval_config = config.clone();
    if compare_cloud {
        // The adapters compare providers under one structured JSON contract
        // and intentionally expose no native tools. Some historical producer
        // profiles require a tool in their normal call site; neutralize only
        // that transport hint in this in-memory eval clone.
        for spec in CHUNK_RELEASE_CANDIDATES {
            eval_config.operation_mapping.insert(
                spec.operation.to_string(),
                OperationProfileSelector::Simple(spec.baseline_profile.to_string()),
            );
            if let Some(profile) = eval_config.profiles.get_mut(spec.baseline_profile) {
                profile
                    .metadata
                    .get_or_insert_with(Default::default)
                    .insert(
                        "tool_choice".to_string(),
                        serde_json::json!({"type":"none"}),
                    );
            }
        }
    }
    let router = if execute {
        Some(Arc::new(
            ConfiguredRouter::from_router_config(eval_config)
                .map_err(|error| format!("building shadow router: {error}"))?,
        ))
    } else {
        None
    };
    let telemetry = Arc::new(EvalLogicalChunkTelemetry::default());
    let runner = router.map(|router| {
        LogicalChunkRunner::new(
            Arc::new(DirectRouterDispatch { router }),
            registry.clone(),
            Arc::new(ConservativeOllamaEstimator),
        )
        .with_telemetry(telemetry.clone())
    });

    let mut case_reports = Vec::with_capacity(fixtures.cases.len());
    for case in fixtures.cases {
        let spec = CHUNK_RELEASE_CANDIDATES
            .iter()
            .find(|spec| spec.operation == case.operation && spec.adapter_id == case.adapter_id)
            .ok_or_else(|| {
                format!(
                    "fixture `{}` does not match a release-candidate operation/adapter pair",
                    case.name
                )
            })?;
        let profile = config
            .profiles
            .get(spec.candidate_profile)
            .ok_or_else(|| format!("candidate profile `{}` is missing", spec.candidate_profile))?;
        let baseline_profile = config
            .profiles
            .get(spec.baseline_profile)
            .ok_or_else(|| format!("baseline profile `{}` is missing", spec.baseline_profile))?;
        let policy = profile.chunking.as_ref().ok_or_else(|| {
            format!(
                "candidate profile `{}` has no chunk policy",
                spec.candidate_profile
            )
        })?;
        let budget = ChunkBudget::new(
            profile.context_window_tokens.unwrap_or(32_768),
            policy.logical_window_tokens.unwrap_or(262_144),
            policy.target_payload_tokens.unwrap_or(24_576),
            2_048,
            profile.max_output_tokens.unwrap_or(4_096),
            policy.safety_margin_tokens,
        )
        .map_err(|error| {
            format!(
                "candidate profile `{}` budget: {error}",
                spec.candidate_profile
            )
        })?;
        let plan = build_plan_only_diagnostics(
            &registry,
            &case.adapter_id,
            &case.operation,
            case.input.clone(),
            &profile.model,
            budget,
        );
        let (plan, plan_error) = match plan {
            Ok(plan) if plan.chunk_count >= case.minimum_chunk_count => (Some(plan), None),
            Ok(plan) => (
                None,
                Some(format!(
                    "fixture planned {} chunk(s); requires at least {}",
                    plan.chunk_count, case.minimum_chunk_count
                )),
            ),
            Err(error) => (None, Some(error)),
        };
        let mut runs = Vec::new();
        let mut cloud_runs = Vec::new();
        if let Some(runner) = runner.as_ref().filter(|_| plan.is_some()) {
            for repeat in 1..=repeats {
                eprintln!(
                    "[ollama-chunking-eval] operation={} repeat={repeat}/{repeats} lane=local status=starting",
                    case.operation
                );
                let local_run = execute_fixture_run(
                    runner,
                    &case,
                    profile,
                    spec.candidate_profile,
                    "ollama",
                    true,
                    policy.fallback_policy,
                    budget,
                    repeat,
                    "local",
                    &telemetry,
                )
                .await;
                eprintln!(
                    "[ollama-chunking-eval] operation={} repeat={repeat}/{repeats} lane=local status={} duration_ms={}",
                    case.operation,
                    if local_run.success && local_run.schema_valid && local_run.golden_valid { "passed" } else { "failed" },
                    local_run.duration_ms
                );
                runs.push(local_run);
                if compare_cloud {
                    eprintln!(
                        "[ollama-chunking-eval] operation={} repeat={repeat}/{repeats} lane=cloud status=starting",
                        case.operation
                    );
                    let cloud_run = execute_fixture_run(
                        runner,
                        &case,
                        baseline_profile,
                        spec.baseline_profile,
                        baseline_profile.provider.as_str(),
                        false,
                        ChunkFallbackPolicy::SameProviderOnly,
                        budget,
                        repeat,
                        "cloud",
                        &telemetry,
                    )
                    .await;
                    eprintln!(
                        "[ollama-chunking-eval] operation={} repeat={repeat}/{repeats} lane=cloud status={} duration_ms={}",
                        case.operation,
                        if cloud_run.success && cloud_run.schema_valid && cloud_run.golden_valid { "passed" } else { "failed" },
                        cloud_run.duration_ms
                    );
                    cloud_runs.push(cloud_run);
                }
            }
        }
        let successes = runs.iter().filter(|run| run.success).count();
        let schema_valid = runs.iter().filter(|run| run.schema_valid).count();
        let golden_valid = runs.iter().filter(|run| run.golden_valid).count();
        let mut latencies = runs.iter().map(|run| run.duration_ms).collect::<Vec<_>>();
        latencies.sort_unstable();
        let cloud_successes = cloud_runs.iter().filter(|run| run.success).count();
        let cloud_schema_valid = cloud_runs.iter().filter(|run| run.schema_valid).count();
        let cloud_golden_valid = cloud_runs.iter().filter(|run| run.golden_valid).count();
        let mut cloud_latencies = cloud_runs
            .iter()
            .map(|run| run.duration_ms)
            .collect::<Vec<_>>();
        cloud_latencies.sort_unstable();
        let agreements = runs
            .iter()
            .zip(&cloud_runs)
            .filter(|(local, cloud)| local.top_level_keys == cloud.top_level_keys)
            .count();
        let local_physical_calls = sum_logical_metric(&runs, "physical_call_count");
        let local_repairs = sum_logical_metric(&runs, "local_repairs");
        let local_fallbacks = sum_logical_metric(&runs, "fallback_calls");
        let denominator = runs.len();
        let cloud_denominator = cloud_runs.len();
        let latency_limit_ms = if compare_cloud {
            percentile(&cloud_latencies, 0.90).map(|p90| p90.saturating_mul(3).max(180_000))
        } else if execute {
            Some(180_000)
        } else {
            None
        };
        let repair_rate = rate_u64(local_repairs, local_physical_calls);
        let fallback_rate = rate_u64(local_fallbacks, local_physical_calls);
        let mut slo_failures = Vec::new();
        if execute && rate(schema_valid, denominator) != Some(1.0) {
            slo_failures.push("final schema validity is below 100%".to_string());
        }
        if execute && rate(golden_valid, denominator) != Some(1.0) {
            slo_failures.push("golden validity is below 100%".to_string());
        }
        if execute && repair_rate.is_some_and(|value| value > 0.02) {
            slo_failures.push("local repair rate exceeds 2%".to_string());
        }
        if execute && local_fallbacks != 0 {
            slo_failures.push("local fallback call rate exceeds 0%".to_string());
        }
        if execute
            && percentile(&latencies, 0.90)
                .zip(latency_limit_ms)
                .is_some_and(|(observed, limit)| observed > limit)
        {
            slo_failures
                .push("logical p90 latency exceeds the configured release limit".to_string());
        }
        if compare_cloud && rate(cloud_golden_valid, cloud_denominator) != Some(1.0) {
            slo_failures.push("cloud comparison golden validity is below 100%".to_string());
        }
        case_reports.push(ChunkShadowCaseReport {
            name: case.name,
            operation: case.operation,
            adapter_id: case.adapter_id,
            candidate_profile: spec.candidate_profile.to_string(),
            baseline_profile: spec.baseline_profile.to_string(),
            plan,
            plan_error,
            runs,
            cloud_runs,
            success_rate: rate(successes, denominator),
            schema_validity_rate: rate(schema_valid, denominator),
            golden_validity_rate: rate(golden_valid, denominator),
            latency_p50_ms: percentile(&latencies, 0.50),
            latency_p90_ms: percentile(&latencies, 0.90),
            cloud_success_rate: rate(cloud_successes, cloud_denominator),
            cloud_schema_validity_rate: rate(cloud_schema_valid, cloud_denominator),
            cloud_golden_validity_rate: rate(cloud_golden_valid, cloud_denominator),
            cloud_latency_p50_ms: percentile(&cloud_latencies, 0.50),
            cloud_latency_p90_ms: percentile(&cloud_latencies, 0.90),
            local_cloud_top_level_agreement_rate: rate(agreements, cloud_denominator),
            local_repair_rate: repair_rate,
            local_fallback_call_rate: fallback_rate,
            logical_latency_limit_ms: latency_limit_ms,
            slo_pass: execute.then(|| slo_failures.is_empty()),
            slo_failures,
        });
    }

    let all_plans_valid = case_reports.iter().all(|case| case.plan.is_some());
    let all_live_runs_valid = execute.then(|| {
        case_reports.iter().all(|case| {
            case.runs.len() == repeats as usize
                && case
                    .runs
                    .iter()
                    .all(|run| run.success && run.schema_valid && run.golden_valid)
                && (!compare_cloud
                    || (case.cloud_runs.len() == repeats as usize
                        && case
                            .cloud_runs
                            .iter()
                            .all(|run| run.success && run.schema_valid && run.golden_valid)))
                && case.slo_pass == Some(true)
        })
    });
    Ok(ChunkShadowEvalReport {
        schema_version: "ollama_logical_chunk_shadow_eval.v1",
        mode: if compare_cloud {
            "local_shadow_with_cloud_comparison"
        } else if execute {
            "local_shadow"
        } else {
            "plan_only"
        },
        repeats: if execute { repeats } else { 0 },
        baseline_reference: fixtures.baseline_reference,
        cloud_comparison_executed: compare_cloud,
        // The focused CLI routes directly so it cannot claim service-queue
        // contention evidence. Phase 7 canary monitoring owns that SLO.
        queue_impact_measured: false,
        persistence_attached: false,
        durable_writes: 0,
        readiness,
        cases: case_reports,
        all_plans_valid,
        all_live_runs_valid,
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute_fixture_run(
    runner: &LogicalChunkRunner,
    case: &ChunkEvalFixtureCase,
    profile: &LLMProfile,
    profile_name: &str,
    primary_provider: &str,
    disable_reasoning: bool,
    fallback_policy: ChunkFallbackPolicy,
    budget: ChunkBudget,
    repeat: u32,
    lane: &str,
    telemetry: &EvalLogicalChunkTelemetry,
) -> ChunkShadowRunResult {
    let mut base_request = LLMRequest {
        model: profile.model.clone(),
        temperature: profile.temperature,
        // Hold output capacity constant so the provider comparison measures
        // model quality/latency rather than a different generation ceiling.
        max_output_tokens: Some(budget.reserved_output_tokens),
        response_format: Some(LLMResponseFormat::JsonObject.into()),
        ..LLMRequest::default()
    };
    base_request.metadata.operation = case.operation.clone();
    base_request.metadata.tags = Some(vec![
        "phase7_activation_verification".to_string(),
        format!("lane:{lane}"),
        format!("fixture:{}", case.name),
        format!("repeat:{repeat}"),
    ]);
    let request = LogicalLlmRequest {
        operation: case.operation.clone(),
        input: case.input.clone(),
        base_request,
    };
    let mut execution = LogicalChunkExecutionContext::new(case.adapter_id.clone(), profile_name);
    execution.caller = format!("phase7_{lane}_eval_no_persistence");
    execution.primary_provider = primary_provider.to_string();
    execution.disable_reasoning = disable_reasoning;
    execution.lock_primary_profile = lane != "cloud";
    execution.fallback_policy = fallback_policy;
    let logical_call_id = execution.logical_call_id.clone();
    let started = Instant::now();
    let result = runner.execute(request, budget, execution).await;
    let mut validation_errors = telemetry.take_validation_errors(&logical_call_id);
    match result {
        Ok(response) => {
            let text = response.text.as_deref().unwrap_or_default();
            let parsed = serde_json::from_str::<Value>(text).ok();
            let mut top_level_keys = parsed
                .as_ref()
                .and_then(Value::as_object)
                .map(|object| object.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            top_level_keys.sort();
            let archive_membership_valid = parsed
                .as_ref()
                .map(|value| validate_archive_membership_contract(case, value))
                .unwrap_or_else(|| Ok(()));
            if let Err(reason) = &archive_membership_valid {
                validation_errors.push(reason.clone());
            }
            let golden_valid = parsed.is_some()
                && case
                    .required_top_level_keys
                    .iter()
                    .all(|key| top_level_keys.contains(key))
                && case
                    .required_output_substrings
                    .iter()
                    .all(|needle| text.contains(needle))
                && case
                    .forbidden_output_substrings
                    .iter()
                    .all(|needle| !text.contains(needle))
                && archive_membership_valid.is_ok();
            let logical_chunking = response
                .raw_response
                .as_ref()
                .and_then(|raw| raw.get("logical_chunking"))
                .cloned();
            ChunkShadowRunResult {
                repeat,
                success: true,
                schema_valid: parsed.is_some(),
                golden_valid,
                duration_ms: duration_ms(started.elapsed()),
                output_sha256: Some(sha256_hex(text)),
                top_level_keys,
                logical_chunking,
                validation_errors,
                error: None,
                durable_writes: 0,
            }
        },
        Err(error) => ChunkShadowRunResult {
            repeat,
            success: false,
            schema_valid: false,
            golden_valid: false,
            duration_ms: duration_ms(started.elapsed()),
            output_sha256: None,
            top_level_keys: Vec::new(),
            logical_chunking: None,
            validation_errors,
            error: Some(error.to_string()),
            durable_writes: 0,
        },
    }
}

fn validate_archive_membership_contract(
    case: &ChunkEvalFixtureCase,
    value: &Value,
) -> Result<(), String> {
    if case.required_source_episode_ids.is_empty() && case.maximum_archive_group_episodes.is_none()
    {
        return Ok(());
    }
    let entries = value
        .get("archive_entries")
        .and_then(Value::as_array)
        .ok_or_else(|| "archive checkpoint eval is missing archive_entries".to_string())?;
    let mut observed = Vec::new();
    for entry in entries {
        let episode_ids = entry
            .get("episode_ids")
            .and_then(Value::as_array)
            .ok_or_else(|| "archive checkpoint entry is missing episode_ids".to_string())?;
        if case
            .maximum_archive_group_episodes
            .is_some_and(|maximum| episode_ids.len() > maximum)
        {
            return Err(format!(
                "archive checkpoint contains {} episodes; maximum is {}",
                episode_ids.len(),
                case.maximum_archive_group_episodes.unwrap_or_default()
            ));
        }
        observed.extend(
            episode_ids
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned),
        );
    }
    let observed_count = observed.len();
    observed.sort();
    let mut unique = observed.clone();
    unique.dedup();
    if unique.len() != observed_count {
        return Err("archive checkpoint membership contains duplicate episode IDs".to_string());
    }
    let mut required = case.required_source_episode_ids.clone();
    required.sort();
    required.dedup();
    if unique != required {
        return Err(format!(
            "archive checkpoint membership mismatch: expected {} unique episodes, observed {}",
            required.len(),
            unique.len()
        ));
    }
    Ok(())
}

fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn duration_ms(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn rate(count: usize, total: usize) -> Option<f64> {
    (total > 0).then_some(count as f64 / total as f64)
}

fn rate_u64(count: u64, total: u64) -> Option<f64> {
    (total > 0).then_some(count as f64 / total as f64)
}

fn sum_logical_metric(runs: &[ChunkShadowRunResult], field: &str) -> u64 {
    runs.iter()
        .filter_map(|run| run.logical_chunking.as_ref())
        .filter_map(|metadata| metadata.get(field))
        .filter_map(Value::as_u64)
        .sum()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    fn archive_case() -> ChunkEvalFixtureCase {
        ChunkEvalFixtureCase {
            name: "archive-checkpoint".to_string(),
            operation: "memory_archive_summary".to_string(),
            adapter_id: "memory_archive_v1".to_string(),
            input: Value::Null,
            required_top_level_keys: Vec::new(),
            required_output_substrings: Vec::new(),
            forbidden_output_substrings: Vec::new(),
            required_source_episode_ids: vec![
                "ep-1".to_string(),
                "ep-2".to_string(),
                "ep-3".to_string(),
            ],
            maximum_archive_group_episodes: Some(2),
            minimum_chunk_count: 1,
        }
    }

    #[test]
    fn live_archive_golden_contract_requires_exact_bounded_membership() {
        let case = archive_case();
        let valid = json!({
            "archive_entries": [
                {"episode_ids": ["ep-1", "ep-2"]},
                {"episode_ids": ["ep-3"]}
            ]
        });
        assert!(validate_archive_membership_contract(&case, &valid).is_ok());

        let duplicate = json!({
            "archive_entries": [
                {"episode_ids": ["ep-1", "ep-2"]},
                {"episode_ids": ["ep-2", "ep-3"]}
            ]
        });
        assert!(validate_archive_membership_contract(&case, &duplicate)
            .unwrap_err()
            .contains("duplicate"));

        let oversized = json!({
            "archive_entries": [{"episode_ids": ["ep-1", "ep-2", "ep-3"]}]
        });
        assert!(validate_archive_membership_contract(&case, &oversized)
            .unwrap_err()
            .contains("maximum"));
    }
}

fn percentile(sorted: &[u64], percentile: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted.get(index).copied()
}
