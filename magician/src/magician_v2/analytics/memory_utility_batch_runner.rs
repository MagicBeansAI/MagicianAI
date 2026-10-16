//! Periodic batch review for memory utility snapshots.
//!
//! Per-run memory utility review remains the fast feedback path. This runner
//! revisits queued review snapshots in grouped batches so repeated workflows
//! can produce a second, workflow-level quality signal without delaying chat or
//! task completion.

use std::time::Duration;

use tokio::{fs, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::magician_v2::{
    agents::{
        memory_temperature_utility_queue_health,
        run_memory_temperature_utility_batch_maintenance_with_telemetry, AgentMemoryResolver,
        MemoryTemperatureUtilityBatchMaintenanceConfig,
    },
    analytics::operation_llm_telemetry::OperationLlmTelemetryContext,
    artifact_v2::workspace::ArtifactV2Workspace,
    query_analysis::operation_llm_router::OperationLlmRouter,
    realtime_events::RuntimeTransportBroadcaster,
};

const DEFAULT_INTERVAL_SECS: u64 = 60;
const DEFAULT_STARTUP_DELAY_SECS: u64 = 180;
const DEFAULT_MIN_BATCH_SIZE: usize = 1;
const DEFAULT_MAX_BATCH_SIZE: usize = 2;

#[derive(Debug)]
pub struct MemoryUtilityBatchRunner {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct MemoryUtilityBatchRunnerConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub startup_delay: Duration,
    pub min_batch_size: usize,
    pub max_batch_size: usize,
}

impl Default for MemoryUtilityBatchRunnerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
            startup_delay: Duration::from_secs(DEFAULT_STARTUP_DELAY_SECS),
            min_batch_size: DEFAULT_MIN_BATCH_SIZE,
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
        }
    }
}

impl MemoryUtilityBatchRunnerConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(raw) = std::env::var("MAGICIAN_MEMORY_UTILITY_BATCH_RUNNER") {
            let raw = raw.trim().to_ascii_lowercase();
            config.enabled = !matches!(raw.as_str(), "0" | "false" | "off" | "disabled");
        }
        if let Some(interval) =
            read_positive_duration_env("MAGICIAN_MEMORY_UTILITY_BATCH_INTERVAL_SECS")
        {
            config.interval = interval;
        }
        if let Some(delay) = read_duration_env("MAGICIAN_MEMORY_UTILITY_BATCH_STARTUP_DELAY_SECS") {
            config.startup_delay = delay;
        }
        if let Some(size) = read_positive_usize_env("MAGICIAN_MEMORY_UTILITY_BATCH_MIN_SIZE") {
            config.min_batch_size = size;
        }
        if let Some(size) = read_positive_usize_env("MAGICIAN_MEMORY_UTILITY_BATCH_MAX_SIZE") {
            config.max_batch_size = size.max(config.min_batch_size);
        }
        config
    }
}

impl MemoryUtilityBatchRunner {
    pub fn spawn(
        workspace_layout: ArtifactV2Workspace,
        memory_resolver: AgentMemoryResolver,
        operation_llm_router: Option<std::sync::Arc<OperationLlmRouter>>,
        event_broadcaster: Option<std::sync::Arc<RuntimeTransportBroadcaster>>,
        config: MemoryUtilityBatchRunnerConfig,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            if !config.enabled {
                info!(
                    target: "analytics::memory_utility_batch_runner",
                    "memory utility batch runner disabled"
                );
                return;
            }
            let Some(router) = operation_llm_router else {
                warn!(
                    target: "analytics::memory_utility_batch_runner",
                    "memory utility batch runner disabled because operation LLM router is unavailable"
                );
                return;
            };
            run_periodic(
                workspace_layout,
                memory_resolver,
                router,
                event_broadcaster,
                config,
                cancel_for_task,
            )
            .await;
        });
        Self { handle, cancel }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

async fn run_periodic(
    workspace_layout: ArtifactV2Workspace,
    memory_resolver: AgentMemoryResolver,
    router: std::sync::Arc<OperationLlmRouter>,
    event_broadcaster: Option<std::sync::Arc<RuntimeTransportBroadcaster>>,
    config: MemoryUtilityBatchRunnerConfig,
    cancel: CancellationToken,
) {
    if !crate::magician_v2::runtime::startup::wait_for_http_or_cancel(&cancel).await {
        return;
    }

    let mut scope_cursor = 0usize;
    if !config.startup_delay.is_zero() {
        tokio::select! {
            _ = tokio::time::sleep(config.startup_delay) => {},
            _ = cancel.cancelled() => return,
        }
    }

    run_once_with_logging(
        &workspace_layout,
        &memory_resolver,
        &router,
        event_broadcaster.as_ref(),
        &config,
        &mut scope_cursor,
    )
    .await;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(config.interval) => {
                run_once_with_logging(
                    &workspace_layout,
                    &memory_resolver,
                    &router,
                    event_broadcaster.as_ref(),
                    &config,
                    &mut scope_cursor,
                ).await;
            },
            _ = cancel.cancelled() => break,
        }
    }
}

async fn run_once_with_logging(
    workspace_layout: &ArtifactV2Workspace,
    memory_resolver: &AgentMemoryResolver,
    router: &std::sync::Arc<OperationLlmRouter>,
    event_broadcaster: Option<&std::sync::Arc<RuntimeTransportBroadcaster>>,
    config: &MemoryUtilityBatchRunnerConfig,
    scope_cursor: &mut usize,
) {
    match run_once(
        workspace_layout,
        memory_resolver,
        router,
        event_broadcaster,
        config,
        scope_cursor,
    )
    .await
    {
        Ok(outcome) if outcome.reviewed_runs > 0 => {
            info!(
                target: "analytics::memory_utility_batch_runner",
                scopes_checked = outcome.scopes_checked,
                reviewed_runs = outcome.reviewed_runs,
                reviewed_memories = outcome.reviewed_memories,
                queued = outcome.queued,
                eligible = outcome.eligible,
                deferred = outcome.deferred,
                failed = outcome.failed,
                retrying = outcome.retrying,
                dead = outcome.dead,
                oldest_pending_age_secs = ?outcome.oldest_pending_age_secs,
                "memory utility batch review completed"
            );
        },
        Ok(outcome) => {
            debug!(
                target: "analytics::memory_utility_batch_runner",
                scopes_checked = outcome.scopes_checked,
                queued = outcome.queued,
                eligible = outcome.eligible,
                deferred = outcome.deferred,
                failed = outcome.failed,
                retrying = outcome.retrying,
                dead = outcome.dead,
                oldest_pending_age_secs = ?outcome.oldest_pending_age_secs,
                "memory utility batch runner found no due batch"
            );
        },
        Err(error) => {
            warn!(
                target: "analytics::memory_utility_batch_runner",
                error = %format_args!("{error:#}"),
                "memory utility batch runner failed"
            );
        },
    }
}

#[derive(Debug, Default)]
struct MemoryUtilityBatchRunOutcome {
    scopes_checked: usize,
    queued: usize,
    eligible: usize,
    reviewed_runs: usize,
    reviewed_memories: usize,
    deferred: usize,
    failed: usize,
    retrying: usize,
    dead: usize,
    oldest_pending_age_secs: Option<u64>,
}

async fn run_once(
    workspace_layout: &ArtifactV2Workspace,
    memory_resolver: &AgentMemoryResolver,
    router: &std::sync::Arc<OperationLlmRouter>,
    event_broadcaster: Option<&std::sync::Arc<RuntimeTransportBroadcaster>>,
    config: &MemoryUtilityBatchRunnerConfig,
    scope_cursor: &mut usize,
) -> anyhow::Result<MemoryUtilityBatchRunOutcome> {
    let mut outcome = MemoryUtilityBatchRunOutcome::default();
    let mut scopes = workspace_layout.list_scope_segments().await?;
    scopes.sort();
    rotate_scopes_for_fair_drain(&mut scopes, scope_cursor);
    let mut remaining_items = config.max_batch_size.max(1);
    for (principal, workspace) in scopes {
        if !scope_has_memory_root(workspace_layout, &principal, &workspace).await {
            continue;
        }
        outcome.scopes_checked += 1;
        let memory_service = match memory_resolver.resolve_for_scope(&principal, &workspace) {
            Ok(service) => service,
            Err(error) => {
                warn!(
                    target: "analytics::memory_utility_batch_runner",
                    principal = %principal,
                    workspace = %workspace,
                    error = %error,
                    "failed to resolve scoped memory service"
                );
                continue;
            },
        };
        let summary = if remaining_items == 0 {
            let health = match memory_temperature_utility_queue_health(&memory_service).await {
                Ok(health) => health,
                Err(error) => {
                    warn!(
                        target: "analytics::memory_utility_batch_runner",
                        principal = %principal,
                        workspace = %workspace,
                        error = %format_args!("{error:#}"),
                        "failed to inspect memory utility durable queue"
                    );
                    continue;
                },
            };
            crate::magician_v2::agents::MemoryTemperatureUtilityBatchMaintenanceSummary {
                queued: health.active,
                eligible: health.eligible,
                reviewed_runs: 0,
                reviewed_memories: 0,
                deferred: health.eligible,
                failed: 0,
                retrying: health.retrying,
                dead: health.dead,
                oldest_pending_age_secs: health.oldest_pending_age_secs,
            }
        } else {
            let telemetry = event_broadcaster.map(|broadcaster| {
                OperationLlmTelemetryContext::new(
                    std::sync::Arc::clone(broadcaster),
                    principal.clone(),
                    workspace.clone(),
                    "memory_temperature",
                )
            });
            let scoped_router = std::sync::Arc::new(router.with_scope_context(Some(
                magicllm::LlmScope::new(principal.clone(), workspace.clone()),
            )));
            match run_memory_temperature_utility_batch_maintenance_with_telemetry(
                memory_service,
                scoped_router,
                MemoryTemperatureUtilityBatchMaintenanceConfig {
                    min_batch_size: config.min_batch_size.min(remaining_items).max(1),
                    max_batch_size: remaining_items,
                },
                telemetry.as_ref(),
            )
            .await
            {
                Ok(summary) => summary,
                Err(error) => {
                    warn!(
                        target: "analytics::memory_utility_batch_runner",
                        principal = %principal,
                        workspace = %workspace,
                        error = %format_args!("{error:#}"),
                        "failed to run memory utility batch maintenance"
                    );
                    // The maintenance call may have reached the provider
                    // before failing to persist its terminal lease state. We
                    // cannot prove that it consumed zero attempts, so fail
                    // closed for admission and spend no more of this global
                    // tick's LLM budget. Remaining scopes are still inspected
                    // below so queue-health totals stay truthful.
                    remaining_items = 0;
                    continue;
                },
            }
        };
        remaining_items = remaining_items.saturating_sub(summary.reviewed_runs + summary.failed);
        outcome.queued += summary.queued;
        outcome.eligible += summary.eligible;
        outcome.reviewed_runs += summary.reviewed_runs;
        outcome.reviewed_memories += summary.reviewed_memories;
        outcome.deferred += summary.deferred;
        outcome.failed += summary.failed;
        outcome.retrying += summary.retrying;
        outcome.dead += summary.dead;
        outcome.oldest_pending_age_secs = match (
            outcome.oldest_pending_age_secs,
            summary.oldest_pending_age_secs,
        ) {
            (Some(current), Some(candidate)) => Some(current.max(candidate)),
            (current @ Some(_), None) => current,
            (None, candidate) => candidate,
        };
    }
    Ok(outcome)
}

fn rotate_scopes_for_fair_drain(scopes: &mut Vec<(String, String)>, cursor: &mut usize) {
    if scopes.is_empty() {
        *cursor = 0;
        return;
    }
    let start = *cursor % scopes.len();
    scopes.rotate_left(start);
    *cursor = (start + 1) % scopes.len();
}

async fn scope_has_memory_root(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> bool {
    fs::try_exists(workspace_layout.memory_root(principal, workspace))
        .await
        .unwrap_or(false)
}

fn read_duration_env(name: &str) -> Option<Duration> {
    let raw = std::env::var(name).ok()?;
    let secs = raw.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(secs))
}

fn read_positive_duration_env(name: &str) -> Option<Duration> {
    read_duration_env(name).filter(|duration| !duration.is_zero())
}

fn read_positive_usize_env(name: &str) -> Option<usize> {
    let raw = std::env::var(name).ok()?;
    raw.trim().parse::<usize>().ok().filter(|value| *value > 0)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn defaults_limit_each_global_maintenance_pass_to_two_items() {
        let config = MemoryUtilityBatchRunnerConfig::default();
        assert_eq!(config.min_batch_size, 1);
        assert_eq!(config.max_batch_size, 2);
        assert_eq!(config.interval, Duration::from_secs(60));
    }

    #[test]
    fn scope_rotation_advances_fairly_between_passes() {
        let original = vec![
            ("p1".to_string(), "w1".to_string()),
            ("p2".to_string(), "w2".to_string()),
            ("p3".to_string(), "w3".to_string()),
        ];
        let mut cursor = 0;

        let mut first = original.clone();
        rotate_scopes_for_fair_drain(&mut first, &mut cursor);
        assert_eq!(first[0], original[0]);
        assert_eq!(cursor, 1);

        let mut second = original.clone();
        rotate_scopes_for_fair_drain(&mut second, &mut cursor);
        assert_eq!(second[0], original[1]);
        assert_eq!(cursor, 2);

        let mut third = original.clone();
        rotate_scopes_for_fair_drain(&mut third, &mut cursor);
        assert_eq!(third[0], original[2]);
        assert_eq!(cursor, 0);
    }
}
