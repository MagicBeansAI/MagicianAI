//! Durable, grant-narrowed scheduling for app-owned background behaviors.
//!
//! The scheduler deliberately keeps time computation separate from workflow
//! execution authority. It first records one deterministic pending fire in the
//! scoped app registry, then hands a server-owned invocation to the existing
//! scheduled-action ingress. A crash between launch and settlement reuses the
//! same idempotency key. Missed intervals collapse to one fire and successful
//! settlement advances from the acceptance boundary, matching the established
//! task-scheduler semantics without pretending its process-memory registry is
//! durable.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use futures_util::{stream, StreamExt};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::magician_v2::artifact_v2::service::V3ReadApi;
use crate::magician_v2::artifact_v2::{ArtifactV2Error, ArtifactV2Service, ScopeRef};

use super::authority::AuthenticatedAppScope;
use super::entity_store::{
    build_behavior_input_envelope_in_snapshot, AppEntityStoreError, AppEntityStoreService,
};
use super::lifecycle::AppInstallationStatus;
use super::manifest::{AppManifestBehavior, AppManifestBehaviorInputSelector, AppManifestTrigger};
use super::models::{
    AppActionInvocation, AppContractError, AppContractLimits, AppDataEnvelope, AppDataSource,
    AppDigest, AppFieldPath, AppInstallationId, AppName, AppProtocolVersion, AppReference,
    AppRevision, AppSourceRefKind,
};
use super::package_staging::{AppPackageStager, AppPackageStagingError};
use super::records::{
    validate_policy, AppBackgroundExecution, AppBehaviorGrant, AppDataHandlingPolicy,
    AppInstallation,
};
use super::registry::{AppRegistryError, AppRegistryService};
mod recurring;
pub use recurring::{AppBehaviorExecutionObservation, AppBehaviorRecurringState};

use super::workflows::{
    app_workflow_task_id_for_scope, recurring_behavior_task_id, AppBackgroundLaunchAuthority,
    AppWorkflowError,
};

pub const APP_BEHAVIOR_MAX_INSTALLATIONS_PER_SCOPE: usize = 256;
pub const APP_BEHAVIOR_MAX_CLAIMS_PER_SCOPE_TICK: usize = 8;
pub const APP_BEHAVIOR_MAX_HEALTH_ITEMS: usize = 256;

const APP_BEHAVIOR_HEALTH_SCHEMA: &str = "magician.app-behavior-health.v1";
const APP_BEHAVIOR_RECONCILE_SECONDS: i64 = 5 * 60;
const APP_BEHAVIOR_RECONCILE_WORK_SECONDS: i64 = 10;
const APP_BEHAVIOR_RECONCILE_INSTALLATIONS_PER_TICK: usize = 16;
const APP_BEHAVIOR_INSTALLATION_WORK_SECONDS: i64 = 3;
/// The least budget worth starting an installation's reconciliation with.
///
/// Below this, the work reliably does not finish: it opens a scoped connection,
/// claims heads, and writes state, all behind the registry's blocking
/// admission. Starting anyway spends the remainder of the scope window, times
/// out, and isolates the installation — strictly worse than deferring it one
/// tick, which costs `tick_interval_seconds` and nothing else.
const APP_BEHAVIOR_INSTALLATION_MIN_WORK_MILLIS: i64 = 1_000;
const APP_BEHAVIOR_DISABLED_PENDING_INSTALLATIONS_PER_TICK: usize = 16;
const APP_BEHAVIOR_ACCEPTANCE_PROBE_CONCURRENCY: usize = 32;
const APP_BEHAVIOR_INSTALLATION_SCAN_SENTINEL: &str = "installation_scan";
const APP_BEHAVIOR_DUE_INSTALLATIONS_SQL: &str = r#"
WITH due_installations(installation_id) AS (
    SELECT installation_id
      FROM app_behavior_heads
     WHERE state = 'idle' AND next_due_at <= ?1
       AND available_at <= ?1
    UNION
    SELECT installation_id
      FROM app_behavior_heads
     WHERE state = 'pending' AND available_at <= ?1
       AND (lease_expires_at IS NULL OR lease_expires_at <= ?1)
    UNION
    SELECT e.installation_id FROM app_behavior_execution_state e
      JOIN app_behavior_heads h USING (installation_id, behavior_id)
     WHERE e.needs_observation = 1
)
SELECT due.installation_id
  FROM due_installations due
  JOIN app_installations installation USING(installation_id)
 WHERE installation.lifecycle_status = 'enabled'
 ORDER BY CASE
     WHEN ?2 IS NULL OR due.installation_id > ?2 THEN 0
     ELSE 1
 END,
 due.installation_id
 LIMIT ?3
"#;
const APP_BEHAVIOR_EVENT_RETENTION: i64 = 512;
const APP_BEHAVIOR_RETIRE_BATCH: i64 = 256;
// One more than the closed 256-installation x 32-behavior inventory. Health
// scans the bounded physical page so corrupt rows cannot consume lookahead and
// make a later valid head unreachable.
const APP_BEHAVIOR_HEALTH_SCAN_LIMIT: i64 = 8_193;
const APP_BEHAVIOR_EXECUTION_BINDING_MISSING: &str = "execution_binding_missing";

#[derive(Debug, Clone, Copy)]
pub struct AppBehaviorRuntimeLimits {
    pub max_installations_per_scope: usize,
    pub max_claims_per_scope_tick: usize,
    pub lease_seconds: u64,
    pub retry_seconds: u64,
}

impl AppBehaviorRuntimeLimits {
    pub fn validate(self) -> Result<Self, AppBehaviorSchedulerError> {
        if self.max_installations_per_scope == 0
            || self.max_installations_per_scope > APP_BEHAVIOR_MAX_INSTALLATIONS_PER_SCOPE
            || self.max_claims_per_scope_tick == 0
            || self.max_claims_per_scope_tick > APP_BEHAVIOR_MAX_CLAIMS_PER_SCOPE_TICK
            || !(30..=600).contains(&self.lease_seconds)
            || !(5..=3_600).contains(&self.retry_seconds)
        {
            return Err(AppBehaviorSchedulerError::InvalidRuntimePolicy);
        }
        Ok(self)
    }
}

impl Default for AppBehaviorRuntimeLimits {
    fn default() -> Self {
        Self {
            max_installations_per_scope: APP_BEHAVIOR_MAX_INSTALLATIONS_PER_SCOPE,
            max_claims_per_scope_tick: APP_BEHAVIOR_MAX_CLAIMS_PER_SCOPE_TICK,
            lease_seconds: 120,
            retry_seconds: 30,
        }
    }
}

#[derive(Debug, Error)]
pub enum AppBehaviorSchedulerError {
    #[error("app behavior runtime policy is outside the supported bounds")]
    InvalidRuntimePolicy,
    #[error("the app behavior inventory exceeds the per-scope ceiling")]
    InventoryCapacityExceeded,
    #[error("the app behavior binding is stale or substituted")]
    StaleBehaviorBinding,
    #[error("the scheduled behavior input selector or source binding is unsupported")]
    UnsupportedBehaviorInput,
    #[error("the durable behavior lease was lost")]
    LeaseLost,
    #[error("the durable behavior state is corrupt")]
    CorruptState,
    #[error("the durable workflow task-state acceptance proof is unavailable")]
    TaskStateProbeUnavailable,
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    EntityStore(#[from] AppEntityStoreError),
    #[error(transparent)]
    Package(#[from] AppPackageStagingError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Workflow(#[from] AppWorkflowError),
}

#[derive(Debug, Clone)]
pub struct AppBehaviorDispatch {
    installation_id: AppInstallationId,
    behavior_id: AppName,
    invocation: AppActionInvocation<Value>,
    source_policy: AppDataHandlingPolicy,
    selector: AppManifestBehaviorInputSelector,
    launch_ref: AppReference,
    grant: AppBehaviorGrant,
    lease: AppBehaviorLease,
}

impl AppBehaviorDispatch {
    pub fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }

    pub fn behavior_id(&self) -> &AppName {
        &self.behavior_id
    }

    pub fn invocation(&self) -> &AppActionInvocation<Value> {
        &self.invocation
    }

    pub fn launch_ref(&self) -> &AppReference {
        &self.launch_ref
    }

    pub fn lease_expires_at(&self) -> DateTime<Utc> {
        self.lease.expires_at
    }
}

#[derive(Debug, Clone)]
struct AppBehaviorLease {
    installation_generation: u64,
    fence: u64,
    token: String,
    fire_ref: AppReference,
    effective_interval_seconds: u64,
    expires_at: DateTime<Utc>,
    consecutive_failures: u32,
}

struct AppBehaviorClaim {
    lease: AppBehaviorLease,
    invocation: AppActionInvocation<Value>,
    source_policy: AppDataHandlingPolicy,
}

enum AppBehaviorClaimOutcome {
    NoDispatch,
    Dispatch(AppBehaviorClaim),
    FencedTransition {
        candidate: AppBehaviorPendingAcceptanceCandidate,
        next_due_at: DateTime<Utc>,
        reason: &'static str,
    },
}

/// `PartialEq` is load-bearing rather than incidental: a pending head's stored
/// evidence is compared against freshly rematerialized evidence, and a
/// difference means the source binding moved underneath the fire. Both field
/// types derive it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedBehaviorInvocation {
    /// Payload-free invocation evidence. `input.value` is always JSON null;
    /// the content digest, exact source revision/fields and policy/provenance
    /// digests bind bytes that must be rematerialized from the live entity
    /// store before every retry. This prevents a pending head from becoming
    /// an unbounded private copy after the source is deleted or reclassified.
    invocation: AppActionInvocation<Value>,
    source_policy: AppDataHandlingPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppBehaviorSettlement {
    Accepted,
    Retry,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBehaviorScopePolicy {
    pub revision: u64,
    pub paused: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBehaviorHealthItem {
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub behavior_id: AppName,
    pub package_revision_ref: AppReference,
    pub state: String,
    pub revision: u64,
    pub fence: u64,
    pub effective_interval_seconds: u64,
    pub next_due_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub pending_fire_ref: Option<AppReference>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub accepted_count: u64,
    pub attempt_count: u32,
    pub consecutive_failures: u32,
    pub period_started_at: DateTime<Utc>,
    pub period_seconds: u64,
    pub period_starts: u32,
    pub max_starts_per_period: u32,
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring: Option<AppBehaviorRecurringState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBehaviorHealthSnapshot {
    pub schema: String,
    pub scope_policy: AppBehaviorScopePolicy,
    /// Process-local liveness only, read from the scheduler that produced this
    /// snapshot. Durable scheduler state and the pause control remain available
    /// when this is false so operators can fail closed while an already
    /// accepted workflow converges.
    pub worker_running: bool,
    pub recovery_posture: String,
    pub incomplete: bool,
    /// Rows retained in the registry but omitted because their typed health
    /// projection could not be decoded. A single damaged row must not blind
    /// operators to every healthy head in the scope.
    pub corrupt_item_count: u32,
    /// Retained terminal events omitted for the same reason.
    pub corrupt_event_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<AppBehaviorHealthCursor>,
    pub items: Vec<AppBehaviorHealthItem>,
    pub events: Vec<AppBehaviorHealthEvent>,
    pub observed_at: DateTime<Utc>,
}

/// One health scan's raw result, before it is assembled into a snapshot.
///
/// Named because the closure that produces it must declare its return type --
/// `execute_scoped_typed_read` is generic over its error and cannot infer one
/// -- and a seven-tuple written inline at the closure head is unreadable.
type HealthScan = (
    AppBehaviorScopePolicy,
    Vec<(AppBehaviorHealthCursor, AppBehaviorHealthItem)>,
    u32,
    bool,
    Option<AppBehaviorHealthCursor>,
    Vec<AppBehaviorHealthEvent>,
    u32,
);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBehaviorHealthCursor {
    /// Opaque physical ordering keys. These intentionally remain strings so
    /// an otherwise corrupt row can still advance the live health scan.
    pub updated_at: String,
    pub installation_id: String,
    pub behavior_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBehaviorHealthEvent {
    pub event_id: u64,
    pub installation_id: AppInstallationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<AppName>,
    pub kind: String,
    pub reason: String,
    pub observed_at: DateTime<Utc>,
}

/// Cross-store acceptance fence. The opaque Artifact start-admission guard is
/// retained until the scheduler has either settled the accepted root or made
/// the corresponding registry transition. A negative observation therefore
/// cannot race a late root publication.
pub struct AppBehaviorTaskAcceptanceFence {
    accepted_root: bool,
    _start_admission: super::super::artifact_v2::service::AppWorkflowStartAdmission,
}

impl AppBehaviorTaskAcceptanceFence {
    fn has_accepted_root(&self) -> bool {
        self.accepted_root
    }
}

/// Bridge to Artifact TaskState and its canonical per-task start-admission
/// lock. A task binding is sealed idempotency evidence only; acceptance still
/// requires Artifact to have durably published a root execution id.
#[async_trait]
pub trait AppBehaviorTaskAcceptanceProbe: Send + Sync {
    async fn acquire_acceptance_fence(
        &self,
        task_id: &str,
    ) -> Result<AppBehaviorTaskAcceptanceFence, AppBehaviorSchedulerError>;

    async fn recurring_execution(
        &self,
        task_id: &str,
    ) -> Result<Option<AppBehaviorExecutionObservation>, AppBehaviorSchedulerError>;
}

pub struct AppArtifactTaskAcceptanceProbe {
    artifact_service: Option<Arc<ArtifactV2Service>>,
    scope: ScopeRef,
}

impl AppArtifactTaskAcceptanceProbe {
    pub fn new(artifact_service: Option<Arc<ArtifactV2Service>>, scope: ScopeRef) -> Self {
        Self {
            artifact_service,
            scope,
        }
    }
}

#[async_trait]
impl AppBehaviorTaskAcceptanceProbe for AppArtifactTaskAcceptanceProbe {
    async fn acquire_acceptance_fence(
        &self,
        task_id: &str,
    ) -> Result<AppBehaviorTaskAcceptanceFence, AppBehaviorSchedulerError> {
        let service = self
            .artifact_service
            .as_ref()
            .ok_or(AppBehaviorSchedulerError::TaskStateProbeUnavailable)?;
        let occurrence = service
            .app_workflow_service()
            .recurring_occurrence_locator(&self.scope, task_id)
            .await?;
        let actual_task_id = occurrence
            .as_ref()
            .map_or(task_id, |(task, _)| task.as_str());
        let start_admission = service
            .acquire_app_workflow_start_admission(&self.scope, actual_task_id)
            .await
            .map_err(|error| {
                tracing::warn!(
                    task_id,
                    error = %error,
                    "app background-behavior Artifact acceptance fence failed"
                );
                AppBehaviorSchedulerError::TaskStateProbeUnavailable
            })?;
        let accepted_root = match service.get_task(&self.scope, actual_task_id).await {
            Ok(task) => {
                if let Some((_, execution_id)) = occurrence.as_ref() {
                    match service
                        .get_execution(&self.scope, actual_task_id, execution_id)
                        .await
                    {
                        Ok(execution) => {
                            execution.state.parent_execution_id.is_none()
                                && execution.state.root_execution_id.as_deref()
                                    == Some(execution_id)
                        },
                        Err(ArtifactV2Error::ExecutionNotFound(_)) => false,
                        Err(_) => return Err(AppBehaviorSchedulerError::TaskStateProbeUnavailable),
                    }
                } else {
                    task.state.latest_root_execution_id.is_some()
                }
            },
            Err(ArtifactV2Error::TaskNotFound(_)) => false,
            Err(error) => {
                tracing::warn!(
                    task_id,
                    error = %error,
                    "app background-behavior Artifact root probe failed"
                );
                return Err(AppBehaviorSchedulerError::TaskStateProbeUnavailable);
            },
        };
        Ok(AppBehaviorTaskAcceptanceFence {
            accepted_root,
            _start_admission: start_admission,
        })
    }

    async fn recurring_execution(
        &self,
        task_id: &str,
    ) -> Result<Option<AppBehaviorExecutionObservation>, AppBehaviorSchedulerError> {
        self.read_recurring_execution(task_id).await
    }
}

#[derive(Clone)]
pub struct AppBehaviorScheduler {
    registry: AppRegistryService,
    stager: AppPackageStager,
    entity_store: AppEntityStoreService,
    limits: AppBehaviorRuntimeLimits,
    /// Whether a worker attempt is currently driving this scheduler.
    ///
    /// Shared across clones, so every handle to one scheduler reports the same
    /// bit. It lived on the API type instead, where `health()` hardcoded
    /// `false` and only the HTTP health handler overwrote it afterwards — so
    /// every other caller of `health()` read a lie. `/social/policy` was one:
    /// it derives `enabled = configured && worker_running`, which made the Town
    /// Square banner say "Social activity is disabled" no matter what the
    /// worker was doing. Owning the bit here means the snapshot is true
    /// wherever it is read, and a new reader cannot forget to patch it.
    worker_running: Arc<AtomicBool>,
}

/// Holds `worker_running` true for the life of one worker attempt.
///
/// Clearing on `Drop` rather than at the end of the attempt covers the unwind
/// path: a panicking worker must report as stopped, not as permanently running.
pub struct AppBehaviorWorkerRunningGuard(Arc<AtomicBool>);

impl Drop for AppBehaviorWorkerRunningGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl AppBehaviorScheduler {
    pub fn new(
        registry: AppRegistryService,
        stager: AppPackageStager,
        limits: AppBehaviorRuntimeLimits,
    ) -> Result<Self, AppBehaviorSchedulerError> {
        let limits = limits.validate()?;
        Ok(Self {
            entity_store: AppEntityStoreService::new(registry.clone()),
            registry,
            stager,
            limits,
            worker_running: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Report a worker attempt as live until the returned guard is dropped.
    ///
    /// The supervisor calls this once per attempt. A replacement scheduler
    /// starts with a fresh `false`, so a supervisor that was aborted mid-attempt
    /// cannot leave the next one reporting a worker that is no longer there.
    pub fn mark_worker_running(&self) -> AppBehaviorWorkerRunningGuard {
        self.worker_running.store(true, Ordering::Release);
        AppBehaviorWorkerRunningGuard(Arc::clone(&self.worker_running))
    }

    /// Reconcile current manifest/grant truth and claim a bounded due batch.
    /// Initial discovery waits one complete effective interval; enabling the
    /// feature therefore cannot create a startup thundering herd.
    pub async fn claim_due(
        &self,
        authenticated: &AuthenticatedAppScope,
        worker_ref: &AppReference,
        task_acceptance: &dyn AppBehaviorTaskAcceptanceProbe,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppBehaviorDispatch>, AppBehaviorSchedulerError> {
        if self.scope_policy(authenticated, now).await?.paused {
            return Ok(Vec::new());
        }
        let mut reconciliation = self.begin_reconciliation_window(authenticated, now).await?;
        let reconciliation_deadline = now
            .checked_add_signed(Duration::seconds(APP_BEHAVIOR_RECONCILE_WORK_SECONDS))
            .ok_or(AppBehaviorSchedulerError::CorruptState)?;
        let retirement_cursor = if reconciliation.is_some() {
            self.scan_cursor(authenticated, now).await?
        } else {
            None
        };
        let mut retirement_complete = reconciliation.is_some();
        if reconciliation.is_some() {
            let disabled_pending = self
                .disabled_pending_installations(authenticated, retirement_cursor.as_ref(), now)
                .await?;
            if disabled_pending.len() == APP_BEHAVIOR_DISABLED_PENDING_INSTALLATIONS_PER_TICK {
                retirement_complete = false;
            }
            for installation_id in disabled_pending {
                let recovery_installation_id = installation_id.clone();
                if let Err(error) = self
                    .advance_scan_cursor_past_installation(
                        authenticated,
                        installation_id.clone(),
                        Utc::now(),
                    )
                    .await
                {
                    retirement_complete = false;
                    tracing::warn!(
                        installation_id = %recovery_installation_id,
                        error = %error,
                        "disabled app background-behavior recovery cursor did not advance"
                    );
                    continue;
                }
                let Some(installation_budget) = installation_work_budget(
                    reconciliation_deadline,
                    Utc::now(),
                    APP_BEHAVIOR_INSTALLATION_WORK_SECONDS,
                ) else {
                    retirement_complete = false;
                    break;
                };
                match tokio::time::timeout(installation_budget, async {
                    self.recover_accepted_pending_heads(
                        authenticated,
                        &installation_id,
                        retirement_cursor.as_ref(),
                        task_acceptance,
                        AppBehaviorPendingNegativeDisposition::RetireDisabled,
                        Utc::now(),
                    )
                    .await
                })
                .await
                {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => {
                        retirement_complete = false;
                        tracing::warn!(
                            installation_id = %recovery_installation_id,
                            error = %error,
                            "disabled app background-behavior acceptance recovery was isolated"
                        );
                    },
                    Err(_) => {
                        retirement_complete = false;
                        tracing::warn!(
                            installation_id = %recovery_installation_id,
                            timeout_ms = installation_budget.as_millis(),
                            "disabled app background-behavior acceptance recovery timed out"
                        );
                    },
                }
            }
            for _ in 0..32 {
                if Utc::now() >= reconciliation_deadline {
                    retirement_complete = false;
                    break;
                }
                let Some(retirement_budget) = installation_work_budget(
                    reconciliation_deadline,
                    Utc::now(),
                    APP_BEHAVIOR_INSTALLATION_WORK_SECONDS,
                ) else {
                    retirement_complete = false;
                    break;
                };
                match tokio::time::timeout(
                    retirement_budget,
                    self.retire_disabled_installation_heads(authenticated, now),
                )
                .await
                {
                    Ok(Ok(false)) => {
                        // Preserve any incompleteness found while recovering
                        // disabled pending roots above.
                        break;
                    },
                    Ok(Ok(true)) => retirement_complete = false,
                    Ok(Err(error)) => {
                        retirement_complete = false;
                        tracing::warn!(
                            error = %error,
                            "disabled app background-behavior retirement was isolated"
                        );
                        break;
                    },
                    Err(_) => {
                        retirement_complete = false;
                        tracing::warn!(
                            timeout_ms = retirement_budget.as_millis(),
                            "disabled app background-behavior retirement timed out"
                        );
                        break;
                    },
                }
            }
        }
        let scan_cursor = self.scan_cursor(authenticated, now).await?;
        if let Some(window) = reconciliation.as_mut() {
            // Retry only debt inherited from an earlier tick. A timed-out
            // blocking store operation may still be unwinding after its
            // async waiter is cancelled, so newly deferred work must not be
            // re-entered in this same scheduler invocation.
            let deferred_at_tick_start = window.deferred_installation_ids.clone();
            let installations = self
                .registry
                .enabled_installations_bounded(
                    authenticated,
                    self.limits.max_installations_per_scope,
                    now,
                )
                .await?;
            if installations.len() > self.limits.max_installations_per_scope {
                return Err(AppBehaviorSchedulerError::InventoryCapacityExceeded);
            }
            let installation_ids = installations
                .iter()
                .map(|installation| installation.installation_id.as_str())
                .collect::<Vec<_>>();
            let (start, end, mut inventory_exhausted) = reconciliation_batch_bounds(
                &installation_ids,
                window
                    .after_installation_id
                    .as_ref()
                    .map(AppInstallationId::as_str),
                APP_BEHAVIOR_RECONCILE_INSTALLATIONS_PER_TICK,
            );
            let mut reconciliation_claims = Vec::new();
            let mut visited_installation_ids = Vec::new();
            for installation in installations[start..end].iter().cloned() {
                if Utc::now() >= reconciliation_deadline {
                    inventory_exhausted = false;
                    break;
                }
                let scan_now = Utc::now();
                let installation_id = installation.installation_id.clone();
                visited_installation_ids.push(installation_id.clone());
                // Persist forward progress and provisional timeout debt before
                // entering work that may outlive cancellation in spawn_blocking.
                // A crash or timeout can therefore neither rewind the prefix
                // nor make this installation look fully reconciled.
                self.advance_reconciliation_cursor(
                    authenticated,
                    window,
                    installation_id.clone(),
                    Utc::now(),
                )
                .await?;
                let Some(installation_budget) = installation_work_budget(
                    reconciliation_deadline,
                    Utc::now(),
                    APP_BEHAVIOR_INSTALLATION_WORK_SECONDS,
                ) else {
                    break;
                };
                let scan_result = tokio::time::timeout(
                    installation_budget,
                    self.claim_installation_behaviors(
                        authenticated,
                        worker_ref,
                        installation,
                        scan_cursor.as_ref(),
                        task_acceptance,
                        true,
                        false,
                        reconciliation_deadline,
                        scan_now,
                        &mut reconciliation_claims,
                    ),
                )
                .await;
                match scan_result {
                    Ok(Ok(())) => {
                        self.clear_reconciliation_deferred(
                            authenticated,
                            window,
                            installation_id.clone(),
                            Utc::now(),
                        )
                        .await?;
                    },
                    Ok(Err(error)) => {
                        // The visit returned, but failures before head
                        // reconciliation are not completion. Keep the
                        // provisional debt until a later full visit succeeds.
                        tracing::warn!(
                            installation_id = %installation_id,
                            error = %error,
                            "app background-behavior installation reconciliation was isolated"
                        );
                        if let Err(record_error) = self
                            .record_installation_error(
                                authenticated,
                                installation_id,
                                error.to_string(),
                                scan_now,
                            )
                            .await
                        {
                            tracing::warn!(
                                error = %record_error,
                                "app background-behavior installation health could not be recorded"
                            );
                        }
                    },
                    Err(_) => {
                        tracing::warn!(
                            installation_id = %installation_id,
                            timeout_ms = installation_budget.as_millis(),
                            "app background-behavior installation reconciliation timed out and was isolated"
                        );
                        if let Err(record_error) = self
                            .record_installation_scan_timeout(
                                authenticated,
                                installation_id,
                                Utc::now(),
                            )
                            .await
                        {
                            tracing::warn!(
                                error = %record_error,
                                "app background-behavior reconciliation timeout event could not be recorded"
                            );
                        }
                    },
                }
            }
            if inventory_exhausted {
                let deferred_retry_ids = deferred_at_tick_start
                    .into_iter()
                    .filter(|installation_id| {
                        !visited_installation_ids
                            .iter()
                            .any(|visited| visited == installation_id)
                    })
                    .take(APP_BEHAVIOR_RECONCILE_INSTALLATIONS_PER_TICK)
                    .collect::<Vec<_>>();
                for installation_id in deferred_retry_ids {
                    if Utc::now() >= reconciliation_deadline {
                        break;
                    }
                    let Some(installation) = installations
                        .iter()
                        .find(|installation| installation.installation_id == installation_id)
                        .cloned()
                    else {
                        // Disabled/uninstalled debt is terminal for this pass;
                        // the retirement sweep owns any remaining heads.
                        self.clear_reconciliation_deferred(
                            authenticated,
                            window,
                            installation_id,
                            Utc::now(),
                        )
                        .await?;
                        continue;
                    };
                    let scan_now = Utc::now();
                    let Some(installation_budget) = installation_work_budget(
                        reconciliation_deadline,
                        scan_now,
                        APP_BEHAVIOR_INSTALLATION_WORK_SECONDS,
                    ) else {
                        break;
                    };
                    let mut deferred_claims = Vec::new();
                    let scan_result = tokio::time::timeout(
                        installation_budget,
                        self.claim_installation_behaviors(
                            authenticated,
                            worker_ref,
                            installation,
                            scan_cursor.as_ref(),
                            task_acceptance,
                            true,
                            false,
                            reconciliation_deadline,
                            scan_now,
                            &mut deferred_claims,
                        ),
                    )
                    .await;
                    match scan_result {
                        Ok(Ok(())) => {
                            self.clear_reconciliation_deferred(
                                authenticated,
                                window,
                                installation_id.clone(),
                                Utc::now(),
                            )
                            .await?;
                        },
                        Ok(Err(error)) => {
                            tracing::warn!(
                                installation_id = %installation_id,
                                error = %error,
                                "deferred app background-behavior reconciliation was isolated"
                            );
                            if let Err(record_error) = self
                                .record_installation_error(
                                    authenticated,
                                    installation_id,
                                    error.to_string(),
                                    scan_now,
                                )
                                .await
                            {
                                tracing::warn!(
                                    error = %record_error,
                                    "deferred app background-behavior installation health could not be recorded"
                                );
                            }
                        },
                        Err(_) => {
                            tracing::warn!(
                                installation_id = %installation_id,
                                timeout_ms = installation_budget.as_millis(),
                                "deferred app background-behavior reconciliation timed out and remains capacity debt"
                            );
                            if let Err(record_error) = self
                                .record_installation_scan_timeout(
                                    authenticated,
                                    installation_id,
                                    Utc::now(),
                                )
                                .await
                            {
                                tracing::warn!(
                                    error = %record_error,
                                    "deferred app background-behavior timeout event could not be recorded"
                                );
                            }
                        },
                    }
                }
            }
            if reconciliation_pass_complete(
                retirement_complete,
                inventory_exhausted,
                window.deferred_installation_ids.is_empty(),
            ) {
                if let Err(error) = self
                    .complete_reconciliation_window(authenticated, window, Utc::now())
                    .await
                {
                    tracing::warn!(
                        error = %error,
                        "app background-behavior reconciliation completion will retry"
                    );
                }
            } else if inventory_exhausted && !window.deferred_installation_ids.is_empty() {
                // A pass with unresolved timeout debt remains incomplete, but
                // it must not freeze discovery at the old end cursor forever.
                // Begin another bounded forward sweep while preserving debt;
                // this admits installations inserted before the old cursor.
                self.rewind_reconciliation_cursor(authenticated, window, Utc::now())
                    .await?;
            }
        }

        // Due work owns an independent indexed pass even while reconciliation
        // is partial. A large inventory cannot starve already known due heads.
        let due_now = Utc::now();
        let due_installations = self
            .due_installations(
                authenticated,
                scan_cursor
                    .as_ref()
                    .map(|cursor| cursor.installation_id.clone()),
                due_now,
            )
            .await?;
        let mut claimed = Vec::new();
        let dispatch_deadline = due_now
            .checked_add_signed(Duration::seconds(
                i64::try_from((self.limits.lease_seconds / 2).max(1))
                    .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
            ))
            .ok_or(AppBehaviorSchedulerError::CorruptState)?;
        for installation_id in due_installations {
            if claimed.len() >= self.limits.max_claims_per_scope_tick
                || Utc::now() >= dispatch_deadline
            {
                break;
            }
            let claim_now = Utc::now();
            if let Err(cursor_error) = self
                .advance_scan_cursor_past_installation(
                    authenticated,
                    installation_id.clone(),
                    Utc::now(),
                )
                .await
            {
                tracing::warn!(
                    installation_id = %installation_id,
                    error = %cursor_error,
                    "app background-behavior due fairness cursor did not advance"
                );
                // No behavior lease exists yet. Skip cancellable installation
                // work unless its forward progress is durable; a later id may
                // still advance and keep the bounded page live.
                continue;
            }
            let Some(installation_budget) = installation_work_budget(
                dispatch_deadline,
                Utc::now(),
                APP_BEHAVIOR_INSTALLATION_WORK_SECONDS,
            ) else {
                break;
            };
            let outcome = tokio::time::timeout(installation_budget, async {
                let installation = self
                    .registry
                    .installation(authenticated, &installation_id, claim_now)
                    .await?
                    .ok_or(AppBehaviorSchedulerError::StaleBehaviorBinding)?;
                self.claim_installation_behaviors(
                    authenticated,
                    worker_ref,
                    installation,
                    scan_cursor.as_ref(),
                    task_acceptance,
                    false,
                    true,
                    dispatch_deadline,
                    claim_now,
                    &mut claimed,
                )
                .await
            })
            .await;
            match outcome {
                Ok(Ok(())) => {},
                Ok(Err(error)) => {
                    tracing::warn!(
                        installation_id = %installation_id,
                        error = %error,
                        "app background-behavior due scan was isolated"
                    );
                    if let Err(record_error) = self
                        .record_installation_error(
                            authenticated,
                            installation_id,
                            error.to_string(),
                            claim_now,
                        )
                        .await
                    {
                        tracing::warn!(
                            error = %record_error,
                            "app background-behavior installation health could not be recorded"
                        );
                    }
                },
                Err(_) => {
                    tracing::warn!(
                        installation_id = %installation_id,
                        timeout_ms = installation_budget.as_millis(),
                        "app background-behavior due installation timed out and was rotated"
                    );
                    if let Err(record_error) = self
                        .record_installation_scan_timeout(
                            authenticated,
                            installation_id,
                            Utc::now(),
                        )
                        .await
                    {
                        tracing::warn!(
                            error = %record_error,
                            "app background-behavior due timeout event could not be recorded"
                        );
                    }
                },
            }
        }
        Ok(claimed)
    }

    async fn begin_reconciliation_window(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<Option<AppBehaviorReconciliationWindow>, AppBehaviorSchedulerError> {
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                begin_reconciliation_window_blocking(connection, now)
            })
            .await
    }

    async fn advance_reconciliation_cursor(
        &self,
        authenticated: &AuthenticatedAppScope,
        window: &mut AppBehaviorReconciliationWindow,
        installation_id: AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        let pass_started_at = timestamp(window.pass_started_at);
        let prior = window
            .after_installation_id
            .as_ref()
            .map(|value| value.as_str().to_owned());
        let next = installation_id.as_str().to_owned();
        let expected_deferred =
            encode_reconciliation_deferred_installations(&window.deferred_installation_ids)?;
        let next_deferred = reconciliation_deferred_after_visit(
            &window.deferred_installation_ids,
            &installation_id,
            true,
        );
        let next_deferred_json = encode_reconciliation_deferred_installations(&next_deferred)?;
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let changed = transaction.execute(
                    "UPDATE app_behavior_reconcile_state
                        SET cursor_installation_id = ?1,
                            deferred_installation_ids_json = ?2
                      WHERE singleton = 1 AND pass_started_at = ?3
                        AND ((cursor_installation_id IS NULL AND ?4 IS NULL)
                          OR cursor_installation_id = ?4)
                        AND deferred_installation_ids_json = ?5",
                    params![
                        next,
                        next_deferred_json,
                        pass_started_at,
                        prior,
                        expected_deferred,
                    ],
                )?;
                if changed != 1 {
                    return Err(AppBehaviorSchedulerError::LeaseLost);
                }
                transaction.commit()?;
                Ok(())
            })
            .await?;
        window.after_installation_id = Some(installation_id);
        window.deferred_installation_ids = next_deferred;
        Ok(())
    }

    async fn clear_reconciliation_deferred(
        &self,
        authenticated: &AuthenticatedAppScope,
        window: &mut AppBehaviorReconciliationWindow,
        installation_id: AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        let pass_started_at = timestamp(window.pass_started_at);
        let expected_cursor = window
            .after_installation_id
            .as_ref()
            .map(|value| value.as_str().to_owned());
        let expected_deferred =
            encode_reconciliation_deferred_installations(&window.deferred_installation_ids)?;
        let next_deferred = reconciliation_deferred_after_visit(
            &window.deferred_installation_ids,
            &installation_id,
            false,
        );
        let next_deferred_json = encode_reconciliation_deferred_installations(&next_deferred)?;
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let changed = transaction.execute(
                    "UPDATE app_behavior_reconcile_state
                        SET deferred_installation_ids_json = ?1
                      WHERE singleton = 1 AND pass_started_at = ?2
                        AND ((cursor_installation_id IS NULL AND ?3 IS NULL)
                          OR cursor_installation_id = ?3)
                        AND deferred_installation_ids_json = ?4",
                    params![
                        next_deferred_json,
                        pass_started_at,
                        expected_cursor,
                        expected_deferred,
                    ],
                )?;
                if changed != 1 {
                    return Err(AppBehaviorSchedulerError::LeaseLost);
                }
                transaction.commit()?;
                Ok(())
            })
            .await?;
        window.deferred_installation_ids = next_deferred;
        Ok(())
    }

    async fn rewind_reconciliation_cursor(
        &self,
        authenticated: &AuthenticatedAppScope,
        window: &mut AppBehaviorReconciliationWindow,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        let pass_started_at = timestamp(window.pass_started_at);
        let expected_cursor = window
            .after_installation_id
            .as_ref()
            .map(|value| value.as_str().to_owned());
        let expected_deferred =
            encode_reconciliation_deferred_installations(&window.deferred_installation_ids)?;
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let changed = connection.execute(
                    "UPDATE app_behavior_reconcile_state
                        SET cursor_installation_id = NULL
                      WHERE singleton = 1 AND pass_started_at = ?1
                        AND ((cursor_installation_id IS NULL AND ?2 IS NULL)
                          OR cursor_installation_id = ?2)
                        AND deferred_installation_ids_json = ?3",
                    params![pass_started_at, expected_cursor, expected_deferred],
                )?;
                if changed != 1 {
                    return Err(AppBehaviorSchedulerError::LeaseLost);
                }
                Ok(())
            })
            .await?;
        window.after_installation_id = None;
        Ok(())
    }

    async fn complete_reconciliation_window(
        &self,
        authenticated: &AuthenticatedAppScope,
        window: &AppBehaviorReconciliationWindow,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        let pass_started_at = timestamp(window.pass_started_at);
        let expected_cursor = window
            .after_installation_id
            .as_ref()
            .map(|value| value.as_str().to_owned());
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                // A pass may span ticks. Ack its start watermark, so changes
                // behind its cursor during that pass trigger the next sweep.
                complete_reconciliation_window_blocking(
                    connection,
                    &pass_started_at,
                    expected_cursor.as_deref(),
                )
            })
            .await
    }

    async fn advance_scan_cursor_past_installation(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        self.advance_scan_cursor_position(
            authenticated,
            installation_id,
            APP_BEHAVIOR_INSTALLATION_SCAN_SENTINEL.to_owned(),
            now,
        )
        .await
    }

    async fn advance_scan_cursor_position(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        behavior_id: String,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                transaction.execute(
                    "INSERT INTO app_behavior_scan_cursor(
                         singleton, installation_id, behavior_id, updated_at
                     ) VALUES (1, ?1, ?2, ?3)
                     ON CONFLICT(singleton) DO UPDATE SET
                         installation_id = excluded.installation_id,
                         behavior_id = excluded.behavior_id,
                         updated_at = excluded.updated_at",
                    params![installation_id.as_str(), behavior_id, timestamp(now),],
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
    }

    async fn due_installations(
        &self,
        authenticated: &AuthenticatedAppScope,
        after_installation_id: Option<AppInstallationId>,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppInstallationId>, AppBehaviorSchedulerError> {
        let limit = i64::try_from(self.limits.max_installations_per_scope)
            .map_err(|_| AppBehaviorSchedulerError::InvalidRuntimePolicy)?;
        let after_installation_id = after_installation_id
            .as_ref()
            .map(|installation_id| installation_id.as_str().to_owned());
        Ok(self
            .registry
            .execute_scoped_typed_read(authenticated, &now, move |connection, _| {
                let mut statement = connection.prepare(APP_BEHAVIOR_DUE_INSTALLATIONS_SQL)?;
                let rows = statement
                    .query_map(
                        params![timestamp(now), after_installation_id, limit],
                        |row| row.get::<_, String>(0),
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                rows.into_iter()
                    .map(AppInstallationId::parse)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(AppBehaviorSchedulerError::from)
            })
            .await?
            .unwrap_or_default())
    }

    async fn due_behaviors(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppName>, AppBehaviorSchedulerError> {
        Ok(self
            .registry
            .execute_scoped_typed_read(authenticated, &now, move |connection, _| {
                let mut statement = connection.prepare(
                    "SELECT behavior_id FROM (
                         SELECT behavior_id
                           FROM app_behavior_heads
                          WHERE installation_id = ?1 AND state = 'idle'
                            AND next_due_at <= ?2 AND available_at <= ?2
                         UNION
                         SELECT behavior_id
                           FROM app_behavior_heads
                          WHERE installation_id = ?1 AND state = 'pending'
                            AND available_at <= ?2
                            AND (lease_expires_at IS NULL OR lease_expires_at <= ?2)
                         UNION
                         SELECT e.behavior_id FROM app_behavior_execution_state e
                           JOIN app_behavior_heads h USING (installation_id, behavior_id)
                          WHERE e.installation_id = ?1 AND e.needs_observation = 1
                     )
                      ORDER BY behavior_id",
                )?;
                let rows = statement
                    .query_map(params![installation_id.as_str(), timestamp(now)], |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows.into_iter()
                    .map(AppName::parse)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(AppBehaviorSchedulerError::from)
            })
            .await?
            .unwrap_or_default())
    }

    async fn record_installation_scan_timeout(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                append_behavior_event(
                    &transaction,
                    installation_id.as_str(),
                    None,
                    "scan_fault",
                    "installation_scan_failed",
                    now,
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
    }

    async fn record_installation_error(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        _error: String,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        let retry_seconds = self.limits.retry_seconds;
        let error_code = "installation_scan_failed";
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let due_heads = {
                    let mut statement = transaction.prepare(
                        "SELECT behavior_id, revision, consecutive_failures
                           FROM app_behavior_heads
                          WHERE installation_id = ?1 AND available_at <= ?2
                            AND ((state = 'idle' AND next_due_at <= ?2)
                              OR (state = 'pending' AND
                                  (lease_expires_at IS NULL OR lease_expires_at <= ?2)))",
                    )?;
                    let rows = statement.query_map(
                        params![installation_id.as_str(), timestamp(now)],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, i64>(2)?,
                            ))
                        },
                    )?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };
                for (behavior_id, revision, consecutive_failures) in due_heads {
                    let next_failure = u32::try_from(consecutive_failures)
                        .map_err(|_| AppBehaviorSchedulerError::CorruptState)?
                        .saturating_add(1);
                    let retry_at = now
                        .checked_add_signed(Duration::seconds(retry_delay_seconds(
                            retry_seconds,
                            next_failure,
                        )?))
                        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
                    transaction.execute(
                        "UPDATE app_behavior_heads
                            SET revision = revision + 1,
                                fence = fence + CASE WHEN state = 'pending' THEN 1 ELSE 0 END,
                                available_at = ?1, lease_owner = NULL,
                                lease_token = NULL, lease_expires_at = NULL,
                                attempt_count = MIN(attempt_count + 1, 4294967295),
                                consecutive_failures = MIN(consecutive_failures + 1, 64),
                                last_error = ?2, updated_at = ?3
                          WHERE installation_id = ?4 AND behavior_id = ?5 AND revision = ?6
                            AND available_at <= ?3
                            AND ((state = 'idle' AND next_due_at <= ?3)
                              OR (state = 'pending' AND
                                  (lease_expires_at IS NULL OR lease_expires_at <= ?3)))",
                        params![
                            timestamp(retry_at),
                            error_code,
                            timestamp(now),
                            installation_id.as_str(),
                            behavior_id,
                            revision,
                        ],
                    )?;
                }
                append_behavior_event(
                    &transaction,
                    installation_id.as_str(),
                    None,
                    "scan_fault",
                    error_code,
                    now,
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
    }

    async fn record_behavior_error(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        behavior_id: AppName,
        _error: String,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        let retry_seconds = self.limits.retry_seconds;
        let error_code = "behavior_scan_failed";
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let consecutive_failures = transaction
                    .query_row(
                        "SELECT consecutive_failures FROM app_behavior_heads
                          WHERE installation_id = ?1 AND behavior_id = ?2",
                        params![installation_id.as_str(), behavior_id.as_str()],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .unwrap_or_default();
                let next_failure = u32::try_from(consecutive_failures)
                    .map_err(|_| AppBehaviorSchedulerError::CorruptState)?
                    .saturating_add(1);
                let retry_at = now
                    .checked_add_signed(Duration::seconds(retry_delay_seconds(
                        retry_seconds,
                        next_failure,
                    )?))
                    .ok_or(AppBehaviorSchedulerError::CorruptState)?;
                transaction.execute(
                    "UPDATE app_behavior_heads
                        SET revision = revision + 1,
                            fence = fence + CASE WHEN state = 'pending' THEN 1 ELSE 0 END,
                            available_at = ?1, lease_owner = NULL,
                            lease_token = NULL, lease_expires_at = NULL,
                            attempt_count = MIN(attempt_count + 1, 4294967295),
                            consecutive_failures = MIN(consecutive_failures + 1, 64),
                            last_error = ?2, updated_at = ?3
                      WHERE installation_id = ?4 AND behavior_id = ?5
                        AND available_at <= ?3
                        AND ((state = 'idle' AND next_due_at <= ?3)
                          OR (state = 'pending' AND
                              (lease_expires_at IS NULL OR lease_expires_at <= ?3)))",
                    params![
                        timestamp(retry_at),
                        error_code,
                        timestamp(now),
                        installation_id.as_str(),
                        behavior_id.as_str(),
                    ],
                )?;
                append_behavior_event(
                    &transaction,
                    installation_id.as_str(),
                    Some(behavior_id.as_str()),
                    "scan_fault",
                    error_code,
                    now,
                )?;
                // Advance fairness even when the malformed item has never
                // produced a head. Otherwise it can remain the first failing
                // item in this installation forever.
                transaction.execute(
                    "INSERT INTO app_behavior_scan_cursor(
                         singleton, installation_id, behavior_id, updated_at
                     ) VALUES (1, ?1, ?2, ?3)
                     ON CONFLICT(singleton) DO UPDATE SET
                         installation_id = excluded.installation_id,
                         behavior_id = excluded.behavior_id,
                         updated_at = excluded.updated_at",
                    params![
                        installation_id.as_str(),
                        behavior_id.as_str(),
                        timestamp(now),
                    ],
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
    }

    async fn retire_disabled_installation_heads(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<bool, AppBehaviorSchedulerError> {
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let retired = {
                    let mut statement = transaction.prepare(
                        "SELECT h.installation_id, h.behavior_id
                           FROM app_behavior_heads h
                           JOIN app_installations i USING(installation_id)
                          WHERE i.lifecycle_status <> 'enabled'
                            AND h.state <> 'pending'
                          ORDER BY h.installation_id, h.behavior_id
                          LIMIT ?1",
                    )?;
                    let rows = statement.query_map(params![APP_BEHAVIOR_RETIRE_BATCH], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };
                let has_more = retired.len() == APP_BEHAVIOR_RETIRE_BATCH as usize;
                for (installation_id, behavior_id) in retired {
                    append_behavior_event(
                        &transaction,
                        &installation_id,
                        Some(&behavior_id),
                        "retired",
                        "installation_disabled",
                        now,
                    )?;
                    transaction.execute(
                        "DELETE FROM app_behavior_heads
                          WHERE installation_id = ?1 AND behavior_id = ?2",
                        params![installation_id, behavior_id],
                    )?;
                }
                transaction.commit()?;
                Ok(has_more)
            })
            .await
    }

    async fn disabled_pending_installations(
        &self,
        authenticated: &AuthenticatedAppScope,
        cursor: Option<&AppBehaviorScanCursor>,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppInstallationId>, AppBehaviorSchedulerError> {
        let after_installation_id = cursor.map(|cursor| cursor.installation_id.as_str().to_owned());
        self.registry
            .execute_scoped_typed_read(authenticated, &now, move |connection, _| {
                let mut statement = connection.prepare(
                    "SELECT installation_id FROM (
                         SELECT DISTINCT h.installation_id
                           FROM app_behavior_heads h
                           JOIN app_installations i USING(installation_id)
                          WHERE i.lifecycle_status <> 'enabled'
                            AND h.state = 'pending' AND h.available_at <= ?1
                            AND (h.lease_expires_at IS NULL OR h.lease_expires_at <= ?1)
                     )
                     ORDER BY CASE
                         WHEN ?2 IS NULL OR installation_id > ?2 THEN 0
                         ELSE 1
                     END,
                     installation_id
                     LIMIT ?3",
                )?;
                let rows = statement.query_map(
                    params![
                        timestamp(now),
                        after_installation_id,
                        i64::try_from(APP_BEHAVIOR_DISABLED_PENDING_INSTALLATIONS_PER_TICK,)
                            .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                    ],
                    |row| row.get::<_, String>(0),
                )?;
                rows.map(|row| Ok(AppInstallationId::parse(row?)?))
                    .collect::<Result<Vec<_>, AppBehaviorSchedulerError>>()
            })
            .await
            .map(|installations| installations.unwrap_or_default())
    }

    async fn reconcile_installation_heads(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        retained_behavior_ids: Vec<AppName>,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let mut statement = transaction.prepare(
                    "SELECT behavior_id, state
                       FROM app_behavior_heads WHERE installation_id = ?1",
                )?;
                let existing = statement
                    .query_map(params![installation_id.as_str()], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                drop(statement);
                for (behavior_id, state) in existing {
                    if !retained_behavior_ids
                        .iter()
                        .any(|retained| retained.as_str() == behavior_id)
                    {
                        // Pending retirement owns an Artifact start-admission
                        // fence in `recover_accepted_pending_heads`. Never let
                        // this ordinary blocking cleanup bypass that seam.
                        if state == "pending" {
                            continue;
                        }
                        append_behavior_event(
                            &transaction,
                            installation_id.as_str(),
                            Some(&behavior_id),
                            "retired",
                            "behavior_binding_removed",
                            now,
                        )?;
                        transaction.execute(
                            "DELETE FROM app_behavior_heads
                              WHERE installation_id = ?1 AND behavior_id = ?2",
                            params![installation_id.as_str(), behavior_id],
                        )?;
                    }
                }
                transaction.commit()?;
                Ok(())
            })
            .await
    }

    async fn claim_installation_behaviors(
        &self,
        authenticated: &AuthenticatedAppScope,
        worker_ref: &AppReference,
        installation: AppInstallation,
        cursor: Option<&AppBehaviorScanCursor>,
        task_acceptance: &dyn AppBehaviorTaskAcceptanceProbe,
        reconcile_inventory: bool,
        claim_dispatches: bool,
        dispatch_deadline: DateTime<Utc>,
        now: DateTime<Utc>,
        claimed: &mut Vec<AppBehaviorDispatch>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        if installation.lifecycle.status != AppInstallationStatus::Enabled {
            return Ok(());
        }
        // Recover exact Artifact-accepted historical roots before reopening any
        // current schema, grant, package, or recipe authority. Reconciliation
        // may otherwise retire/reset the old binding before its committed start
        // is reflected in scheduler counters.
        self.recover_accepted_pending_heads(
            authenticated,
            &installation.installation_id,
            cursor,
            task_acceptance,
            AppBehaviorPendingNegativeDisposition::Preserve,
            now,
        )
        .await?;
        let active = self
            .entity_store
            .active_schema(authenticated, &installation.installation_id, now)
            .await?
            .ok_or(AppBehaviorSchedulerError::StaleBehaviorBinding)?;
        let (background_minimum, _max_concurrent_runs) =
            match active.grant().granted_background_execution {
                AppBackgroundExecution::Denied => {
                    self.recover_accepted_pending_heads(
                        authenticated,
                        &installation.installation_id,
                        cursor,
                        task_acceptance,
                        AppBehaviorPendingNegativeDisposition::RetireUnlessCurrent(Vec::new()),
                        now,
                    )
                    .await?;
                    self.reconcile_installation_heads(
                        authenticated,
                        installation.installation_id.clone(),
                        Vec::new(),
                        now,
                    )
                    .await?;
                    return Ok(());
                },
                AppBackgroundExecution::Granted {
                    min_interval_seconds,
                    max_concurrent_runs,
                } => (min_interval_seconds, max_concurrent_runs),
            };
        let package = self
            .registry
            .package_revision(authenticated, &installation.package_revision_ref, now)
            .await?
            .ok_or(AppBehaviorSchedulerError::StaleBehaviorBinding)?;
        let staged = self
            .stager
            .load_staged_package(authenticated, package.content_digest.clone(), now)
            .await?;
        if staged.storage_digest() != &package.content_digest
            || staged.candidate().bundle_digest() != &package.content_digest
        {
            return Err(AppBehaviorSchedulerError::StaleBehaviorBinding);
        }
        let manifest = staged.candidate().manifest().manifest();
        // Why a declared behavior did not become a binding.
        //
        // These two filters used to be `?` operators, so a behavior the package
        // declares but the installation never granted vanished with no log, no
        // error and no health record — indistinguishable from a package that
        // declares nothing. An installation admitted while
        // `app_platform.background_behaviors.enabled` was false carries no
        // behavior grants, and arming the switch afterwards does not
        // retroactively grant anything, so this is the expected steady state for
        // every app installed before the lane was armed. It cost hours to find
        // once; it should cost one log line thereafter.
        let mut ungranted: Vec<String> = Vec::new();
        let mut invalid: Vec<(String, String)> = Vec::new();
        let current_behavior_bindings = manifest
            .app
            .behaviors
            .iter()
            .filter_map(|behavior| {
                let Some(grant) = active
                    .grant()
                    .granted_behavior_grants
                    .iter()
                    .find(|grant| grant.behavior_id == behavior.id)
                else {
                    ungranted.push(behavior.id.to_string());
                    return None;
                };
                if let Err(error) = validate_live_behavior_binding(behavior, grant) {
                    invalid.push((behavior.id.to_string(), error.to_string()));
                    return None;
                }
                Some(AppBehaviorBinding {
                    installation_id: installation.installation_id.clone(),
                    installation_generation: installation.lifecycle.generation,
                    package_revision_ref: installation.package_revision_ref.clone(),
                    schema_revision: active.schema_revision(),
                    grant_revision: active.grant_revision(),
                    behavior_id: behavior.id.clone(),
                    behavior_digest: grant.reviewed_request_digest.clone(),
                    effective_interval_seconds: behavior
                        .cadence
                        .min_interval_seconds()
                        .max(grant.min_interval_seconds)
                        .max(background_minimum),
                    max_starts_per_period: grant.resources.max_starts_per_period,
                    period_seconds: grant.resources.period_seconds,
                })
            })
            .collect::<Vec<_>>();
        // Emitted on the inventory pass rather than every tick: the condition is
        // durable — it does not clear until the installation is reviewed again —
        // so repeating it every `tick_interval_seconds` would be noise, while
        // saying it once per inventory sweep keeps it findable.
        if reconcile_inventory && !ungranted.is_empty() {
            tracing::warn!(
                installation_id = %installation.installation_id,
                behaviors = %ungranted.join(", "),
                "app behaviors are declared by the package but not granted by this \
                 installation, so they will never run; review the installation again \
                 to grant them (an installation admitted while \
                 app_platform.background_behaviors.enabled was false has no behavior \
                 grants, and arming it later does not add them)"
            );
        }
        if reconcile_inventory && !invalid.is_empty() {
            for (behavior_id, error) in &invalid {
                tracing::warn!(
                    installation_id = %installation.installation_id,
                    behavior_id = %behavior_id,
                    error = %error,
                    "app behavior is granted but its live binding failed validation, so it \
                     will not run"
                );
            }
        }
        let retained_behavior_ids = current_behavior_bindings
            .iter()
            .map(|binding| binding.behavior_id.clone())
            .collect::<Vec<_>>();
        if reconcile_inventory {
            self.recover_accepted_pending_heads(
                authenticated,
                &installation.installation_id,
                cursor,
                task_acceptance,
                AppBehaviorPendingNegativeDisposition::RetireUnlessCurrent(
                    current_behavior_bindings,
                ),
                now,
            )
            .await?;
            self.reconcile_installation_heads(
                authenticated,
                installation.installation_id.clone(),
                retained_behavior_ids,
                now,
            )
            .await?;
        }

        let mut behaviors = manifest.app.behaviors.iter().collect::<Vec<_>>();
        let due_behaviors = if reconcile_inventory {
            None
        } else {
            Some(
                self.due_behaviors(authenticated, installation.installation_id.clone(), now)
                    .await?,
            )
        };
        if let Some(due_behaviors) = due_behaviors.as_ref() {
            behaviors.retain(|behavior| due_behaviors.iter().any(|due| due == &behavior.id));
        }
        if let Some(cursor) =
            cursor.filter(|cursor| cursor.installation_id == installation.installation_id)
        {
            if let Some(position) = behaviors
                .iter()
                .position(|behavior| behavior.id.as_str() == cursor.behavior_id.as_str())
            {
                let start = (position + 1) % behaviors.len().max(1);
                behaviors.rotate_left(start);
            }
        }
        for behavior in behaviors {
            if claim_dispatches && Utc::now() >= dispatch_deadline {
                break;
            }
            if claimed.len() >= self.limits.max_claims_per_scope_tick && !reconcile_inventory {
                break;
            }
            let Some(grant) = active
                .grant()
                .granted_behavior_grants
                .iter()
                .find(|grant| grant.behavior_id == behavior.id)
            else {
                continue;
            };
            let result = async {
                validate_live_behavior_binding(behavior, grant)?;
                let action = manifest
                    .app
                    .actions
                    .get(&behavior.action)
                    .ok_or(AppBehaviorSchedulerError::StaleBehaviorBinding)?;
                let workflow = manifest
                    .app
                    .workflows
                    .get(&action.workflow)
                    .ok_or(AppBehaviorSchedulerError::StaleBehaviorBinding)?;
                if workflow.trigger != AppManifestTrigger::Schedule {
                    return Err(AppBehaviorSchedulerError::UnsupportedBehaviorInput);
                }
                let effective_interval_seconds = behavior
                    .cadence
                    .min_interval_seconds()
                    .max(grant.min_interval_seconds)
                    .max(background_minimum);
                let binding = AppBehaviorBinding {
                    installation_id: installation.installation_id.clone(),
                    installation_generation: installation.lifecycle.generation,
                    package_revision_ref: installation.package_revision_ref.clone(),
                    schema_revision: active.schema_revision(),
                    grant_revision: active.grant_revision(),
                    behavior_id: behavior.id.clone(),
                    behavior_digest: grant.reviewed_request_digest.clone(),
                    effective_interval_seconds,
                    max_starts_per_period: grant.resources.max_starts_per_period,
                    period_seconds: grant.resources.period_seconds,
                };
                let invocation_contract = AppBehaviorInvocationContract {
                    selector: behavior.input.clone(),
                    action_id: behavior.action.clone(),
                    input_schema_ref: action.input_from.clone(),
                    result_schema_ref: action.result_from.clone(),
                    execution_ready: super::behavior_recipe::behavior_execution_ready(
                        workflow.runner,
                        &behavior.operations,
                        &behavior.steps,
                    ),
                };
                let Some(claim) = self
                    .claim_binding(
                        authenticated,
                        worker_ref,
                        binding,
                        invocation_contract,
                        task_acceptance,
                        claim_dispatches && claimed.len() < self.limits.max_claims_per_scope_tick,
                        now,
                    )
                    .await?
                else {
                    return Ok(None);
                };
                Ok(Some(AppBehaviorDispatch {
                    installation_id: installation.installation_id.clone(),
                    behavior_id: behavior.id.clone(),
                    launch_ref: AppReference::parse(format!(
                        "behavior-launch:{}",
                        claim.lease.fire_ref.as_str()
                    ))?,
                    invocation: claim.invocation,
                    source_policy: claim.source_policy,
                    selector: behavior.input.clone(),
                    grant: grant.clone(),
                    lease: claim.lease,
                }))
            }
            .await;
            match result {
                Ok(Some(dispatch)) => claimed.push(dispatch),
                Ok(None) => {},
                Err(error) => {
                    tracing::warn!(
                        installation_id = %installation.installation_id,
                        behavior_id = %behavior.id,
                        error = %error,
                        "app background-behavior item scan was isolated"
                    );
                    if let Err(record_error) = self
                        .record_behavior_error(
                            authenticated,
                            installation.installation_id.clone(),
                            behavior.id.clone(),
                            error.to_string(),
                            now,
                        )
                        .await
                    {
                        tracing::warn!(
                            error = %record_error,
                            "app background-behavior item health could not be recorded"
                        );
                    }
                },
            }
        }
        Ok(())
    }

    async fn recover_accepted_pending_heads(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        cursor: Option<&AppBehaviorScanCursor>,
        task_acceptance: &dyn AppBehaviorTaskAcceptanceProbe,
        negative_disposition: AppBehaviorPendingNegativeDisposition,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        let installation_id = installation_id.clone();
        let after_behavior_id = cursor
            .filter(|cursor| cursor.installation_id == installation_id)
            .map(|cursor| cursor.behavior_id.clone());
        let scope_binding_ref = authenticated.scope_binding_ref().clone();
        let queried_installation_id = installation_id.clone();
        let candidate_rows = self
            .registry
            .execute_scoped_typed_read(
                authenticated,
                &now,
                move |connection,
                      _|
                      -> Result<Vec<(String, BehaviorHeadRow)>, AppBehaviorSchedulerError> {
                let mut statement = connection.prepare(
                        "SELECT behavior_id, installation_generation,
                                package_revision_ref, schema_revision,
                                grant_revision, behavior_digest, state, revision, fence,
                                effective_interval_seconds, next_due_at, available_at,
                                pending_scheduled_at, pending_fire_ref, lease_expires_at,
                                period_started_at, period_seconds, period_starts,
                                max_starts_per_period, invocation_json, invocation_digest,
                                consecutive_failures, last_error
                           FROM app_behavior_heads
                          WHERE installation_id = ?1
                            AND state = 'pending' AND available_at <= ?2
                            AND (lease_expires_at IS NULL OR lease_expires_at <= ?2)
                          ORDER BY CASE
                              WHEN ?3 IS NULL OR behavior_id > ?3 THEN 0
                              ELSE 1
                          END,
                          behavior_id",
                    )?;
                let rows = statement
                    .query_map(
                        params![
                            queried_installation_id.as_str(),
                            timestamp(now),
                            after_behavior_id,
                        ],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                BehaviorHeadRow {
                                    installation_generation: row.get(1)?,
                                    package_revision_ref: row.get(2)?,
                                    schema_revision: row.get(3)?,
                                    grant_revision: row.get(4)?,
                                    behavior_digest: row.get(5)?,
                                    state: row.get(6)?,
                                    revision: row.get(7)?,
                                    fence: row.get(8)?,
                                    effective_interval_seconds: row.get(9)?,
                                    next_due_at: row.get(10)?,
                                    available_at: row.get(11)?,
                                    pending_scheduled_at: row.get(12)?,
                                    pending_fire_ref: row.get(13)?,
                                    lease_expires_at: row.get(14)?,
                                    period_started_at: row.get(15)?,
                                    period_seconds: row.get(16)?,
                                    period_starts: row.get(17)?,
                                    max_starts_per_period: row.get(18)?,
                                    invocation_json: row.get(19)?,
                                    invocation_digest: row.get(20)?,
                                    consecutive_failures: row.get(21)?,
                                    last_error: row.get(22)?,
                                },
                            ))
                        },
                    )?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            },
            )
            .await?
            .unwrap_or_default();
        let mut candidates = Vec::with_capacity(candidate_rows.len());
        let mut first_error = None;
        for (behavior_id_raw, row) in candidate_rows {
            // Durable rotation precedes parsing and the possibly contended
            // Artifact lock. Neither malformed history nor one slow task may
            // pin later recovery candidates.
            self.advance_scan_cursor_position(
                authenticated,
                installation_id.clone(),
                behavior_id_raw.clone(),
                now,
            )
            .await?;
            let candidate = (|| {
                let behavior_id = AppName::parse(behavior_id_raw.clone())?;
                let binding = historical_binding_from_head(&installation_id, &behavior_id, &row)?;
                let task_id = pending_task_id_from_head(
                    authenticated.scope(),
                    &binding,
                    &scope_binding_ref,
                    &row,
                )?
                .ok_or(AppBehaviorSchedulerError::CorruptState)?;
                Ok::<_, AppBehaviorSchedulerError>(AppBehaviorPendingAcceptanceCandidate {
                    binding,
                    row,
                    task_id,
                })
            })();
            match candidate {
                Ok(candidate) => candidates.push(candidate),
                Err(error) => {
                    first_error.get_or_insert(error);
                },
            }
        }
        let mut probes = stream::iter(candidates.into_iter().map(|candidate| {
            let disposition = negative_disposition.clone();
            async move {
                let acceptance_fence = task_acceptance
                    .acquire_acceptance_fence(&candidate.task_id)
                    .await?;
                self.settle_or_retire_pending_candidate(
                    authenticated,
                    candidate,
                    acceptance_fence,
                    disposition,
                    now,
                )
                .await
            }
        }))
        .buffer_unordered(APP_BEHAVIOR_ACCEPTANCE_PROBE_CONCURRENCY);
        while let Some(result) = probes.next().await {
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    async fn settle_or_retire_pending_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        candidate: AppBehaviorPendingAcceptanceCandidate,
        acceptance_fence: AppBehaviorTaskAcceptanceFence,
        negative_disposition: AppBehaviorPendingNegativeDisposition,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        let accepted = acceptance_fence.has_accepted_root();
        let transition_required = match &negative_disposition {
            AppBehaviorPendingNegativeDisposition::Preserve => false,
            AppBehaviorPendingNegativeDisposition::RetireDisabled
            | AppBehaviorPendingNegativeDisposition::Reset { .. } => true,
            AppBehaviorPendingNegativeDisposition::RetireUnlessCurrent(current)
                if !current.iter().any(|binding| binding == &candidate.binding) =>
            {
                true
            },
            AppBehaviorPendingNegativeDisposition::RetireUnlessCurrent(_) => false,
        };
        if !accepted && !transition_required {
            return Ok(());
        }
        let accepted_task_id = candidate.task_id.clone();
        let scope_binding_ref = authenticated.scope_binding_ref().clone();
        self.registry
            .execute_scoped_typed_background_write(
                authenticated,
                &now,
                move |connection, scope| -> Result<(), AppBehaviorSchedulerError> {
                    // Ownership moves into the non-cancellable blocking job.
                    // Dropping an async timeout waiter cannot expose the root
                    // race while this transaction is still committing.
                    let _acceptance_fence = acceptance_fence;
                    let transaction =
                        connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    if accepted {
                        let settled = settle_accepted_pending_launch_from_head(
                            &transaction,
                            scope,
                            &candidate.binding,
                            &scope_binding_ref,
                            &candidate.row,
                            Some(&accepted_task_id),
                            now,
                        )?;
                        if !settled {
                            return Err(AppBehaviorSchedulerError::CorruptState);
                        }
                    } else {
                        match negative_disposition {
                            AppBehaviorPendingNegativeDisposition::Reset {
                                next_due_at,
                                reason,
                            } => {
                                let pending_fire_ref = candidate
                                    .row
                                    .pending_fire_ref
                                    .as_deref()
                                    .ok_or(AppBehaviorSchedulerError::CorruptState)?;
                                let invocation_digest = candidate
                                    .row
                                    .invocation_digest
                                    .as_deref()
                                    .ok_or(AppBehaviorSchedulerError::CorruptState)?;
                                abandon_pending_source(
                                    &transaction,
                                    &candidate.binding,
                                    candidate.row.revision,
                                    candidate.row.fence,
                                    pending_fire_ref,
                                    invocation_digest,
                                    next_due_at,
                                    now,
                                    reason,
                                )?;
                            },
                            AppBehaviorPendingNegativeDisposition::RetireDisabled => {
                                retire_pending_candidate(
                                    &transaction,
                                    &candidate,
                                    "installation_disabled",
                                    true,
                                    now,
                                )?;
                            },
                            AppBehaviorPendingNegativeDisposition::RetireUnlessCurrent(_) => {
                                retire_pending_candidate(
                                    &transaction,
                                    &candidate,
                                    "behavior_binding_removed",
                                    false,
                                    now,
                                )?;
                            },
                            AppBehaviorPendingNegativeDisposition::Preserve => {},
                        }
                    }
                    transaction.commit()?;
                    Ok(())
                },
            )
            .await
    }

    async fn claim_binding(
        &self,
        authenticated: &AuthenticatedAppScope,
        worker_ref: &AppReference,
        binding: AppBehaviorBinding,
        invocation_contract: AppBehaviorInvocationContract,
        task_acceptance: &dyn AppBehaviorTaskAcceptanceProbe,
        allow_dispatch: bool,
        now: DateTime<Utc>,
    ) -> Result<Option<AppBehaviorClaim>, AppBehaviorSchedulerError> {
        if !self
            .observe_recurring_before_claim(authenticated, &binding, task_acceptance, now)
            .await?
        {
            return Ok(None);
        }
        let lease_seconds = self.limits.lease_seconds;
        let retry_seconds = self.limits.retry_seconds;
        let worker = worker_ref.to_string();
        let scope_binding_ref = authenticated.scope_binding_ref().clone();
        let outcome = self
            .registry
            .execute_scoped_typed_background_write(
                authenticated,
                &now,
                move |connection,
                      scope|
                      -> Result<AppBehaviorClaimOutcome, AppBehaviorSchedulerError> {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if load_scope_policy(&transaction, now)?.paused {
                    transaction.commit()?;
                    return Ok(AppBehaviorClaimOutcome::NoDispatch);
                }
                let result = claim_behavior_head(
                    &transaction,
                    scope,
                    &scope_binding_ref,
                    &binding,
                    &invocation_contract,
                    &worker,
                    lease_seconds,
                    retry_seconds,
                    allow_dispatch,
                    now,
                )?;
                if matches!(&result, AppBehaviorClaimOutcome::Dispatch(_)) {
                    transaction.execute(
                        "INSERT INTO app_behavior_scan_cursor(
                             singleton, installation_id, behavior_id, updated_at
                         ) VALUES (1, ?1, ?2, ?3)
                         ON CONFLICT(singleton) DO UPDATE SET
                             installation_id = excluded.installation_id,
                             behavior_id = excluded.behavior_id,
                             updated_at = excluded.updated_at",
                        params![
                            binding.installation_id.as_str(),
                            binding.behavior_id.as_str(),
                            timestamp(now),
                        ],
                    )?;
                }
                transaction.commit()?;
                Ok(result)
            },
            )
            .await?;
        match outcome {
            AppBehaviorClaimOutcome::NoDispatch => Ok(None),
            AppBehaviorClaimOutcome::Dispatch(claim) => Ok(Some(claim)),
            AppBehaviorClaimOutcome::FencedTransition {
                candidate,
                next_due_at,
                reason,
            } => {
                let acceptance_fence = task_acceptance
                    .acquire_acceptance_fence(&candidate.task_id)
                    .await?;
                self.settle_or_retire_pending_candidate(
                    authenticated,
                    candidate,
                    acceptance_fence,
                    AppBehaviorPendingNegativeDisposition::Reset {
                        next_due_at,
                        reason,
                    },
                    now,
                )
                .await?;
                Ok(None)
            },
        }
    }

    async fn scan_cursor(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<Option<AppBehaviorScanCursor>, AppBehaviorSchedulerError> {
        Ok(self
            .registry
            .execute_scoped_typed_read(
                authenticated,
                &now,
                move |connection,
                      _|
                      -> Result<Option<AppBehaviorScanCursor>, AppBehaviorSchedulerError> {
                    connection
                        .query_row(
                            "SELECT installation_id, behavior_id
                               FROM app_behavior_scan_cursor WHERE singleton = 1",
                            [],
                            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                        )
                        .optional()?
                        .map(|(installation_id, behavior_id)| {
                            Ok(AppBehaviorScanCursor {
                                installation_id: AppInstallationId::parse(installation_id)?,
                                behavior_id,
                            })
                        })
                        .transpose()
                },
            )
            .await?
            .flatten())
    }

    pub async fn settle(
        &self,
        authenticated: &AuthenticatedAppScope,
        dispatch: &AppBehaviorDispatch,
        settlement: AppBehaviorSettlement,
        _error: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        let installation_id = dispatch.installation_id.clone();
        let behavior_id = dispatch.behavior_id.clone();
        let lease = dispatch.lease.clone();
        let retry_seconds = self.limits.retry_seconds;
        let error_code = match settlement {
            AppBehaviorSettlement::Accepted => None,
            AppBehaviorSettlement::Retry => Some("workflow_launch_retryable"),
            AppBehaviorSettlement::Blocked => Some("workflow_launch_blocked"),
        };
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if load_scope_policy(&transaction, now)?.paused {
                    return Err(AppBehaviorSchedulerError::LeaseLost);
                }
                let changed = match settlement {
                    AppBehaviorSettlement::Accepted => transaction.execute(
                        "UPDATE app_behavior_heads
                            SET state = 'idle', revision = revision + 1,
                                next_due_at = ?1, pending_scheduled_at = NULL,
                                pending_fire_ref = NULL, lease_owner = NULL,
                                lease_token = NULL, lease_expires_at = NULL,
                                invocation_json = NULL, invocation_digest = NULL,
                                accepted_count = accepted_count + 1,
                                consecutive_failures = 0,
                                period_starts = period_starts + 1,
                                last_launch_ref = ?2, last_error = NULL,
                                updated_at = ?3
                          WHERE installation_id = ?4 AND behavior_id = ?5
                            AND installation_generation = ?6 AND fence = ?7
                            AND lease_token = ?8 AND pending_fire_ref = ?9
                            AND lease_expires_at = ?10 AND lease_expires_at > ?3",
                        params![
                            timestamp(
                                now.checked_add_signed(Duration::seconds(
                                    i64::try_from(lease.effective_interval_seconds)
                                        .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                                ))
                                .ok_or(AppBehaviorSchedulerError::CorruptState)?
                            ),
                            format!("behavior-launch:{}", lease.fire_ref.as_str()),
                            timestamp(now),
                            installation_id.as_str(),
                            behavior_id.as_str(),
                            i64::try_from(lease.installation_generation)
                                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                            i64::try_from(lease.fence)
                                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                            lease.token,
                            lease.fire_ref.as_str(),
                            timestamp(lease.expires_at),
                        ],
                    )?,
                    AppBehaviorSettlement::Retry => transaction.execute(
                        "UPDATE app_behavior_heads
                            SET state = 'pending', revision = revision + 1,
                                available_at = ?1, lease_owner = NULL,
                                lease_token = NULL, lease_expires_at = NULL,
                                consecutive_failures = MIN(consecutive_failures + 1, 64),
                                last_error = ?2, updated_at = ?3
                          WHERE installation_id = ?4 AND behavior_id = ?5
                            AND installation_generation = ?6 AND fence = ?7
                            AND lease_token = ?8 AND pending_fire_ref = ?9
                            AND lease_expires_at = ?10 AND lease_expires_at > ?3",
                        params![
                            timestamp(
                                now.checked_add_signed(Duration::seconds(retry_delay_seconds(
                                    retry_seconds,
                                    lease.consecutive_failures.saturating_add(1),
                                )?))
                                .ok_or(AppBehaviorSchedulerError::CorruptState)?
                            ),
                            error_code,
                            timestamp(now),
                            installation_id.as_str(),
                            behavior_id.as_str(),
                            i64::try_from(lease.installation_generation)
                                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                            i64::try_from(lease.fence)
                                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                            lease.token,
                            lease.fire_ref.as_str(),
                            timestamp(lease.expires_at),
                        ],
                    )?,
                    AppBehaviorSettlement::Blocked => transaction.execute(
                        "UPDATE app_behavior_heads
                            SET state = 'blocked', revision = revision + 1,
                                pending_scheduled_at = NULL, pending_fire_ref = NULL,
                                lease_owner = NULL, lease_token = NULL,
                                lease_expires_at = NULL, invocation_json = NULL,
                                invocation_digest = NULL,
                                consecutive_failures = MIN(consecutive_failures + 1, 64),
                                last_error = ?1, updated_at = ?2
                          WHERE installation_id = ?3 AND behavior_id = ?4
                            AND installation_generation = ?5 AND fence = ?6
                            AND lease_token = ?7 AND pending_fire_ref = ?8
                            AND lease_expires_at = ?9 AND lease_expires_at > ?2",
                        params![
                            error_code,
                            timestamp(now),
                            installation_id.as_str(),
                            behavior_id.as_str(),
                            i64::try_from(lease.installation_generation)
                                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                            i64::try_from(lease.fence)
                                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                            lease.token,
                            lease.fire_ref.as_str(),
                            timestamp(lease.expires_at),
                        ],
                    )?,
                };
                if changed != 1 {
                    return Err(AppBehaviorSchedulerError::LeaseLost);
                }
                transaction.commit()?;
                Ok(())
            })
            .await
    }

    /// Reopen the exact durable lease immediately before workflow admission.
    /// The returned move-only workflow authority expires with this lease; a
    /// paused scope, reclaimed fence or substituted invocation cannot launch.
    pub async fn authorize_dispatch(
        &self,
        authenticated: &AuthenticatedAppScope,
        dispatch: &AppBehaviorDispatch,
        now: DateTime<Utc>,
    ) -> Result<AppBackgroundLaunchAuthority, AppBehaviorSchedulerError> {
        let installation_id = dispatch.installation_id.clone();
        let behavior_id = dispatch.behavior_id.clone();
        let lease = dispatch.lease.clone();
        let selector = dispatch.selector.clone();
        let expected_input = dispatch.invocation.input.clone();
        let source_policy = dispatch.source_policy.clone();
        let scope_binding_ref = authenticated.scope_binding_ref().clone();
        let persisted =
            persisted_invocation_evidence(&dispatch.invocation, dispatch.source_policy.clone());
        let expected_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(&persisted)?)?;
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, scope| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if load_scope_policy(&transaction, now)?.paused {
                    return Err(AppBehaviorSchedulerError::LeaseLost);
                }
                let row = transaction
                    .query_row(
                        "SELECT state, fence, lease_token, lease_expires_at,
                                pending_fire_ref, invocation_digest
                           FROM app_behavior_heads
                          WHERE installation_id = ?1 AND behavior_id = ?2",
                        params![installation_id.as_str(), behavior_id.as_str()],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, Option<String>>(3)?,
                                row.get::<_, Option<String>>(4)?,
                                row.get::<_, Option<String>>(5)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((state, fence, token, expires_at, fire_ref, invocation_digest)) = row
                else {
                    return Err(AppBehaviorSchedulerError::LeaseLost);
                };
                if state != "pending"
                    || u64::try_from(fence).ok() != Some(lease.fence)
                    || token.as_deref() != Some(lease.token.as_str())
                    || fire_ref.as_deref() != Some(lease.fire_ref.as_str())
                    || invocation_digest.as_deref() != Some(expected_digest.as_str())
                    || expires_at
                        .as_deref()
                        .map(parse_timestamp)
                        .transpose()?
                        .is_none_or(|expires_at| {
                            expires_at <= now || expires_at != lease.expires_at
                        })
                {
                    return Err(AppBehaviorSchedulerError::LeaseLost);
                }
                let material = build_behavior_input_envelope_in_snapshot(
                    &transaction,
                    scope,
                    &scope_binding_ref,
                    &expected_input.installation_id,
                    lease.installation_generation,
                    &expected_input.package_revision_ref,
                    expected_input.schema_revision,
                    expected_input.grant_revision,
                    &selector,
                    expected_input.value_schema_ref.clone(),
                    now,
                )?;
                let mut current_input = material.envelope;
                current_input.produced_at = expected_input.produced_at.clone();
                current_input.expires_at = expected_input.expires_at.clone();
                if current_input != expected_input || material.handling_policy != source_policy {
                    return Err(AppBehaviorSchedulerError::StaleBehaviorBinding);
                }
                transaction.commit()?;
                Ok(())
            })
            .await?;
        Ok(AppBackgroundLaunchAuthority::from_server_behavior_schedule(
            authenticated,
            dispatch.installation_id.clone(),
            dispatch.launch_ref.clone(),
            &dispatch.invocation.input,
            dispatch.grant.clone(),
            dispatch.source_policy.clone(),
            lease.expires_at,
            now,
        )?)
    }

    pub async fn scope_policy(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<AppBehaviorScopePolicy, AppBehaviorSchedulerError> {
        let loaded = self
            .registry
            .execute_scoped_typed_read(
                authenticated,
                &now,
                // `move` because the bound is `F: ... + 'static`; `now` is
                // `Copy`, so taking ownership costs nothing.
                move |connection, _| -> Result<AppBehaviorScopePolicy, AppBehaviorSchedulerError> {
                    load_scope_policy(connection, now)
                },
            )
            .await?;
        Ok(loaded.unwrap_or_else(|| default_scope_policy(now)))
    }

    pub async fn set_scope_paused(
        &self,
        authenticated: &AuthenticatedAppScope,
        expected_revision: u64,
        paused: bool,
        now: DateTime<Utc>,
    ) -> Result<AppBehaviorScopePolicy, AppBehaviorSchedulerError> {
        self.registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let current = load_scope_policy(&transaction, now)?;
                if current.revision != expected_revision {
                    return Err(AppBehaviorSchedulerError::LeaseLost);
                }
                let revision = current
                    .revision
                    .checked_add(1)
                    .ok_or(AppBehaviorSchedulerError::CorruptState)?;
                transaction.execute(
                    "INSERT INTO app_behavior_scope_policy(singleton, revision, paused, updated_at)
                     VALUES (1, ?1, ?2, ?3)
                     ON CONFLICT(singleton) DO UPDATE SET
                         revision = excluded.revision,
                         paused = excluded.paused,
                         updated_at = excluded.updated_at",
                    params![
                        i64::try_from(revision)
                            .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                        if paused { 1_i64 } else { 0_i64 },
                        timestamp(now),
                    ],
                )?;
                transaction.commit()?;
                Ok(AppBehaviorScopePolicy {
                    revision,
                    paused,
                    updated_at: now,
                })
            })
            .await
    }

    /// Owner-requested retry after repairing a workflow launch or settled execution failure. This
    /// only makes the existing scheduled fire claimable: ordinary reconciliation,
    /// source reads, current grants, budgets and launch admission still apply.
    /// Preserve next_due_at so the fire/task idempotency key does not change.
    pub async fn retry_blocked_launch(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        behavior_id: AppName,
        expected_generation: u64,
        expected_revision: u64,
        now: DateTime<Utc>,
    ) -> Result<u64, AppBehaviorSchedulerError> {
        self.registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, _| {
                retry_blocked_launch_blocking(
                    connection,
                    &installation_id,
                    &behavior_id,
                    expected_generation,
                    expected_revision,
                    now,
                )
            })
            .await
    }

    pub async fn health(
        &self,
        authenticated: &AuthenticatedAppScope,
        limit: usize,
        cursor: Option<AppBehaviorHealthCursor>,
        now: DateTime<Utc>,
    ) -> Result<AppBehaviorHealthSnapshot, AppBehaviorSchedulerError> {
        if limit == 0 || limit > APP_BEHAVIOR_MAX_HEALTH_ITEMS {
            return Err(AppBehaviorSchedulerError::InvalidRuntimePolicy);
        }
        let loaded = self
            .registry
            .execute_scoped_typed_read(
                authenticated,
                &now,
                move |connection, _| -> Result<HealthScan, AppBehaviorSchedulerError> {
                    let policy = load_scope_policy(connection, now)?;
                    let base = "SELECT installation_id, installation_generation, behavior_id,
                            package_revision_ref, state, revision, fence,
                            effective_interval_seconds, next_due_at, pending_fire_ref,
                            accepted_count, attempt_count, last_error, updated_at,
                            available_at, lease_expires_at, consecutive_failures,
                            period_started_at, period_seconds, period_starts,
                            max_starts_per_period,
                            CAST(updated_at AS TEXT),
                            CAST(installation_id AS TEXT),
                            CAST(behavior_id AS TEXT)
                       FROM app_behavior_heads";
                    let mut rows = Vec::new();
                    let mut corrupt_item_count = 0_u32;
                    let mut last_physical_cursor = None;
                    let scan_saturated = if let Some(cursor) = cursor {
                        let mut statement = connection.prepare(&format!(
                            "{base}
                         WHERE updated_at < ?1
                            OR (updated_at = ?1 AND installation_id > ?2)
                            OR (updated_at = ?1 AND installation_id = ?2 AND behavior_id > ?3)
                         ORDER BY updated_at DESC, installation_id, behavior_id
                         LIMIT ?4"
                        ))?;
                        let mapped = statement.query_map(
                            params![
                                cursor.updated_at,
                                cursor.installation_id,
                                cursor.behavior_id,
                                APP_BEHAVIOR_HEALTH_SCAN_LIMIT,
                            ],
                            |row| {
                                let cursor = AppBehaviorHealthCursor {
                                    updated_at: row.get(21)?,
                                    installation_id: row.get(22)?,
                                    behavior_id: row.get(23)?,
                                };
                                Ok((cursor, decode_health_row(row)))
                            },
                        )?;
                        let mut physical_count = 0_i64;
                        for row in mapped {
                            physical_count = physical_count.saturating_add(1);
                            match row {
                                Ok((physical_cursor, Ok(row))) => {
                                    last_physical_cursor = Some(physical_cursor.clone());
                                    rows.push((physical_cursor, row));
                                    if rows.len() > limit {
                                        break;
                                    }
                                },
                                Ok((physical_cursor, Err(_))) => {
                                    last_physical_cursor = Some(physical_cursor);
                                    corrupt_item_count = corrupt_item_count.saturating_add(1);
                                },
                                Err(_) => {
                                    corrupt_item_count = corrupt_item_count.saturating_add(1);
                                },
                            }
                        }
                        physical_count == APP_BEHAVIOR_HEALTH_SCAN_LIMIT
                    } else {
                        let mut statement = connection.prepare(&format!(
                            "{base}
                         ORDER BY updated_at DESC, installation_id, behavior_id
                         LIMIT ?1"
                        ))?;
                        let mapped = statement.query_map(
                            params![APP_BEHAVIOR_HEALTH_SCAN_LIMIT],
                            |row| {
                                let cursor = AppBehaviorHealthCursor {
                                    updated_at: row.get(21)?,
                                    installation_id: row.get(22)?,
                                    behavior_id: row.get(23)?,
                                };
                                Ok((cursor, decode_health_row(row)))
                            },
                        )?;
                        let mut physical_count = 0_i64;
                        for row in mapped {
                            physical_count = physical_count.saturating_add(1);
                            match row {
                                Ok((physical_cursor, Ok(row))) => {
                                    last_physical_cursor = Some(physical_cursor.clone());
                                    rows.push((physical_cursor, row));
                                    if rows.len() > limit {
                                        break;
                                    }
                                },
                                Ok((physical_cursor, Err(_))) => {
                                    last_physical_cursor = Some(physical_cursor);
                                    corrupt_item_count = corrupt_item_count.saturating_add(1);
                                },
                                Err(_) => {
                                    corrupt_item_count = corrupt_item_count.saturating_add(1);
                                },
                            }
                        }
                        physical_count == APP_BEHAVIOR_HEALTH_SCAN_LIMIT
                    };
                    let mut event_statement = connection.prepare(
                        "SELECT event_id, installation_id, behavior_id, kind, reason, observed_at
                       FROM app_behavior_events
                      ORDER BY event_id DESC
                      LIMIT ?1",
                    )?;
                    let mut events = Vec::new();
                    let mut corrupt_event_count = 0_u32;
                    let mapped_events = event_statement.query_map(
                        params![APP_BEHAVIOR_EVENT_RETENTION],
                        decode_health_event_row,
                    )?;
                    for event in mapped_events {
                        match event {
                            Ok(event) => events.push(event),
                            Err(_) => {
                                corrupt_event_count = corrupt_event_count.saturating_add(1);
                            },
                        }
                    }
                    Ok((
                        policy,
                        rows,
                        corrupt_item_count,
                        scan_saturated,
                        last_physical_cursor,
                        events,
                        corrupt_event_count,
                    ))
                },
            )
            .await?;
        let (
            policy,
            mut items,
            corrupt_item_count,
            scan_saturated,
            last_physical_cursor,
            events,
            corrupt_event_count,
        ) = loaded.unwrap_or_else(|| {
            (
                default_scope_policy(now),
                Vec::new(),
                0,
                false,
                None,
                Vec::new(),
                0,
            )
        });
        let incomplete = items.len() > limit || scan_saturated;
        let next_cursor = if items.len() > limit {
            items
                .get(limit.saturating_sub(1))
                .map(|(cursor, _)| cursor.clone())
        } else if scan_saturated {
            last_physical_cursor
        } else {
            None
        };
        items.truncate(limit);
        let mut items: Vec<_> = items.into_iter().map(|(_, item)| item).collect();
        self.populate_recurring_health(authenticated, &mut items, now)
            .await?;
        Ok(AppBehaviorHealthSnapshot {
            schema: APP_BEHAVIOR_HEALTH_SCHEMA.to_owned(),
            scope_policy: policy,
            worker_running: self.worker_running.load(Ordering::Acquire),
            recovery_posture:
                "recurring tasks; completion-aware scheduling; execution recovery before next occurrence".to_owned(),
            incomplete,
            corrupt_item_count,
            corrupt_event_count,
            next_cursor,
            items,
            events,
            observed_at: now,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppBehaviorBinding {
    installation_id: AppInstallationId,
    installation_generation: u64,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    grant_revision: AppRevision,
    behavior_id: AppName,
    behavior_digest: AppDigest,
    effective_interval_seconds: u64,
    max_starts_per_period: u32,
    period_seconds: u64,
}

#[derive(Debug, Clone)]
struct AppBehaviorScanCursor {
    installation_id: AppInstallationId,
    // Opaque physical key. Keeping this raw lets recovery rotate past a
    // malformed historical behavior id instead of corrupting the cursor read
    // and pinning every later candidate.
    behavior_id: String,
}

#[derive(Debug, Clone)]
struct AppBehaviorReconciliationWindow {
    pass_started_at: DateTime<Utc>,
    after_installation_id: Option<AppInstallationId>,
    deferred_installation_ids: Vec<AppInstallationId>,
}

#[derive(Debug, Clone)]
struct AppBehaviorInvocationContract {
    selector: AppManifestBehaviorInputSelector,
    action_id: AppName,
    input_schema_ref: AppReference,
    result_schema_ref: AppReference,
    execution_ready: bool,
}

fn reconciliation_batch_bounds(
    sorted_installation_ids: &[&str],
    after_installation_id: Option<&str>,
    limit: usize,
) -> (usize, usize, bool) {
    let start = after_installation_id.map_or(0, |after| {
        sorted_installation_ids.partition_point(|installation_id| *installation_id <= after)
    });
    let end = start
        .saturating_add(limit)
        .min(sorted_installation_ids.len());
    (start, end, end == sorted_installation_ids.len())
}

fn reconciliation_pass_complete(
    retirement_complete: bool,
    inventory_exhausted: bool,
    deferred_installations_empty: bool,
) -> bool {
    retirement_complete && inventory_exhausted && deferred_installations_empty
}

fn reconciliation_deferred_after_visit(
    current: &[AppInstallationId],
    installation_id: &AppInstallationId,
    retain_installation: bool,
) -> Vec<AppInstallationId> {
    let mut next = current.to_vec();
    if retain_installation {
        if !next.iter().any(|deferred| deferred == installation_id) {
            next.push(installation_id.clone());
        }
    } else {
        next.retain(|deferred| deferred != installation_id);
    }
    next
}

fn encode_reconciliation_deferred_installations(
    installation_ids: &[AppInstallationId],
) -> Result<Vec<u8>, AppBehaviorSchedulerError> {
    serde_json::to_vec(
        &installation_ids
            .iter()
            .map(AppInstallationId::as_str)
            .collect::<Vec<_>>(),
    )
    .map_err(AppBehaviorSchedulerError::from)
}

fn decode_reconciliation_deferred_installations(
    bytes: &[u8],
) -> Result<Vec<AppInstallationId>, AppBehaviorSchedulerError> {
    // 256 maximally sized installation ids plus JSON framing exceed 32 KiB.
    // Keep this aligned with the V25 column constraint so the closed inventory
    // can always represent every timed-out installation without write failure.
    if bytes.len() > 65_536 {
        return Err(AppBehaviorSchedulerError::CorruptState);
    }
    let raw: Vec<String> = serde_json::from_slice(bytes)?;
    if raw.len() > APP_BEHAVIOR_MAX_INSTALLATIONS_PER_SCOPE {
        return Err(AppBehaviorSchedulerError::CorruptState);
    }
    let mut parsed = Vec::with_capacity(raw.len());
    for value in raw {
        let installation_id = AppInstallationId::parse(value)?;
        if parsed.iter().any(|existing| existing == &installation_id) {
            return Err(AppBehaviorSchedulerError::CorruptState);
        }
        parsed.push(installation_id);
    }
    Ok(parsed)
}

/// The budget for reconciling one installation, or `None` to stop this tick.
///
/// `None` means "defer", not "fail". The caller breaks out of the loop and the
/// scan cursor resumes here on the next tick, so an installation that did not
/// fit is retried rather than punished.
///
/// The floor is why this matters. The scope window is shared across every
/// installation in it, so with several apps per scope the tail of the list used
/// to receive whatever milliseconds were left — one was observed getting 492ms
/// — spend them, time out, and be **isolated**, recording a timeout against an
/// installation whose only fault was being last. An installation that cannot be
/// given a workable budget is deferred to the next tick instead.
fn installation_work_budget(
    deadline: DateTime<Utc>,
    now: DateTime<Utc>,
    maximum_seconds: i64,
) -> Option<StdDuration> {
    let maximum_milliseconds = maximum_seconds.checked_mul(1_000)?;
    let remaining_milliseconds = deadline
        .signed_duration_since(now)
        .num_milliseconds()
        .min(maximum_milliseconds);
    (remaining_milliseconds >= APP_BEHAVIOR_INSTALLATION_MIN_WORK_MILLIS)
        .then(|| StdDuration::from_millis(remaining_milliseconds as u64))
}

fn validate_live_behavior_binding(
    behavior: &AppManifestBehavior,
    grant: &AppBehaviorGrant,
) -> Result<(), AppBehaviorSchedulerError> {
    let output_schema_digest = behavior
        .output_schema
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?
        .as_ref()
        .map(AppDigest::blake3_canonical_json)
        .transpose()?;
    let input_selector_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&behavior.input)?)?;
    // The recipe is part of the binding. Without this a package update could
    // reorder steps or drop a guard while every other reviewed field stayed
    // identical, and the scheduler would keep dispatching the old grant.
    let steps_digest =
        crate::magician_v2::apps::manifest::app_behavior_steps_digest(&behavior.steps)
            .map_err(|_| AppBehaviorSchedulerError::StaleBehaviorBinding)?;
    if grant.purpose != behavior.purpose
        || grant.action != behavior.action
        || grant.operations != behavior.operations
        || grant.input_selector_digest != input_selector_digest
        || grant.steps_digest != steps_digest
        || grant.output_schema_digest != output_schema_digest
        || grant.min_interval_seconds < behavior.cadence.min_interval_seconds()
    {
        return Err(AppBehaviorSchedulerError::StaleBehaviorBinding);
    }
    Ok(())
}

fn scheduled_invocation(
    binding: &AppBehaviorBinding,
    contract: &AppBehaviorInvocationContract,
    lease: &AppBehaviorLease,
    input: AppDataEnvelope<Value>,
) -> Result<AppActionInvocation<Value>, AppBehaviorSchedulerError> {
    Ok(AppActionInvocation {
        protocol_version: AppProtocolVersion::V1,
        idempotency_key: AppReference::parse(format!("behavior-fire:{}", lease.fire_ref.as_str()))?,
        action_id: contract.action_id.clone(),
        action_revision: AppRevision::new(binding.installation_generation)?,
        input,
        requested_result_schema_ref: contract.result_schema_ref.clone(),
        caller_surface_or_execution_ref: AppReference::parse(format!(
            "behavior:{}",
            binding.behavior_id.as_str()
        ))?,
    })
}

fn pending_fenced_transition(
    scope: &super::records::AppScope,
    scope_binding_ref: &super::models::AppScopeBindingRef,
    binding: &AppBehaviorBinding,
    row: BehaviorHeadRow,
    next_due_at: DateTime<Utc>,
    reason: &'static str,
) -> Result<AppBehaviorClaimOutcome, AppBehaviorSchedulerError> {
    let task_id = pending_task_id_from_head(scope, binding, scope_binding_ref, &row)?
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    Ok(AppBehaviorClaimOutcome::FencedTransition {
        candidate: AppBehaviorPendingAcceptanceCandidate {
            binding: binding.clone(),
            row,
            task_id,
        },
        next_due_at,
        reason,
    })
}

fn claim_behavior_head(
    transaction: &rusqlite::Transaction<'_>,
    scope: &super::records::AppScope,
    scope_binding_ref: &super::models::AppScopeBindingRef,
    binding: &AppBehaviorBinding,
    contract: &AppBehaviorInvocationContract,
    worker: &str,
    lease_seconds: u64,
    retry_seconds: u64,
    allow_dispatch: bool,
    now: DateTime<Utc>,
) -> Result<AppBehaviorClaimOutcome, AppBehaviorSchedulerError> {
    let generation = i64::try_from(binding.installation_generation)
        .map_err(|_| AppBehaviorSchedulerError::CorruptState)?;
    let interval = i64::try_from(binding.effective_interval_seconds)
        .map_err(|_| AppBehaviorSchedulerError::CorruptState)?;
    let period_seconds = i64::try_from(binding.period_seconds)
        .map_err(|_| AppBehaviorSchedulerError::CorruptState)?;
    let now_text = timestamp(now);
    let initial_due = now
        .checked_add_signed(Duration::seconds(interval))
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    transaction.execute(
        "INSERT INTO app_behavior_heads(
             installation_id, installation_generation, package_revision_ref,
             schema_revision, grant_revision, behavior_id, behavior_digest,
             state, revision, fence, effective_interval_seconds, next_due_at,
             available_at, accepted_count, attempt_count, consecutive_failures, period_started_at,
             period_seconds, period_starts, max_starts_per_period, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'idle', 1, 0, ?8, ?9,
                   ?10, 0, 0, 0, ?10, ?11, 0, ?12, ?10)
         ON CONFLICT(installation_id, behavior_id) DO NOTHING",
        params![
            binding.installation_id.as_str(),
            generation,
            binding.package_revision_ref.as_str(),
            i64::try_from(binding.schema_revision.get())
                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
            i64::try_from(binding.grant_revision.get())
                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
            binding.behavior_id.as_str(),
            binding.behavior_digest.as_str(),
            interval,
            timestamp(initial_due),
            &now_text,
            period_seconds,
            i64::from(binding.max_starts_per_period),
        ],
    )?;

    let row = transaction.query_row(
        "SELECT installation_generation, package_revision_ref, schema_revision,
                grant_revision, behavior_digest, state, revision, fence,
                effective_interval_seconds, next_due_at, available_at, pending_scheduled_at,
                pending_fire_ref, lease_expires_at, period_started_at,
                period_seconds, period_starts, max_starts_per_period,
                invocation_json, invocation_digest, consecutive_failures, last_error
           FROM app_behavior_heads
          WHERE installation_id = ?1 AND behavior_id = ?2",
        params![
            binding.installation_id.as_str(),
            binding.behavior_id.as_str()
        ],
        |row| {
            Ok(BehaviorHeadRow {
                installation_generation: row.get(0)?,
                package_revision_ref: row.get(1)?,
                schema_revision: row.get(2)?,
                grant_revision: row.get(3)?,
                behavior_digest: row.get(4)?,
                state: row.get(5)?,
                revision: row.get(6)?,
                fence: row.get(7)?,
                effective_interval_seconds: row.get(8)?,
                next_due_at: row.get(9)?,
                available_at: row.get(10)?,
                pending_scheduled_at: row.get(11)?,
                pending_fire_ref: row.get(12)?,
                lease_expires_at: row.get(13)?,
                period_started_at: row.get(14)?,
                period_seconds: row.get(15)?,
                period_starts: row.get(16)?,
                max_starts_per_period: row.get(17)?,
                invocation_json: row.get(18)?,
                invocation_digest: row.get(19)?,
                consecutive_failures: row.get(20)?,
                last_error: row.get(21)?,
            })
        },
    )?;
    let binding_changed = row.installation_generation != generation
        || row.package_revision_ref != binding.package_revision_ref.as_str()
        || row.schema_revision
            != i64::try_from(binding.schema_revision.get())
                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?
        || row.grant_revision
            != i64::try_from(binding.grant_revision.get())
                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?
        || row.behavior_digest != binding.behavior_digest.as_str();
    // A package/grant update cannot revoke a lease already handed to a live
    // worker. That worker may be crossing workflow final authority now; wait
    // for its settlement or expiry before replacing the binding and making a
    // new-generation fire eligible.
    if !binding_reset_allowed_after_lease(row.lease_expires_at.as_deref(), now)? {
        return Ok(AppBehaviorClaimOutcome::NoDispatch);
    }
    if binding_changed {
        // An expired pending fire may still be crossing Artifact's root
        // boundary. Binding-changing retirement is performed only by the
        // acceptance-fenced recovery pass; this claim transaction may reset
        // idle state but never erase an unfenced pending identity.
        if row.state == "pending" {
            return Ok(AppBehaviorClaimOutcome::NoDispatch);
        }
        // A package generation or grant changed while the head was idle.
        // Begin a full fresh interval; stale authority is never replayed.
        // An unexpired fixed-period start count survives the authority change,
        // however, so an update cannot reset a behavior's frequency ceiling.
        let old_period_started = parse_timestamp(&row.period_started_at)?;
        let old_period_ends = old_period_started
            .checked_add_signed(Duration::seconds(row.period_seconds))
            .ok_or(AppBehaviorSchedulerError::CorruptState)?;
        let (
            migrated_period_started,
            migrated_period_seconds,
            migrated_period_starts,
            migrated_max_starts,
        ) = if old_period_ends > now {
            (
                old_period_started,
                row.period_seconds,
                row.period_starts,
                row.period_starts.max(
                    row.max_starts_per_period
                        .min(i64::from(binding.max_starts_per_period)),
                ),
            )
        } else {
            (
                now,
                period_seconds,
                0,
                i64::from(binding.max_starts_per_period),
            )
        };
        transaction.execute(
            "UPDATE app_behavior_heads
                SET installation_generation = ?1, package_revision_ref = ?2,
                    schema_revision = ?3, grant_revision = ?4,
                    behavior_digest = ?5, state = 'idle', revision = revision + 1,
                    fence = fence + 1, effective_interval_seconds = ?6,
                    next_due_at = ?7, available_at = ?8,
                    pending_scheduled_at = NULL, pending_fire_ref = NULL,
                    lease_owner = NULL, lease_token = NULL,
                    lease_expires_at = NULL, accepted_count = 0,
                    invocation_json = NULL, invocation_digest = NULL,
                    attempt_count = 0, consecutive_failures = 0, period_started_at = ?9,
                    period_seconds = ?10, period_starts = ?11,
                    max_starts_per_period = ?12, last_launch_ref = NULL,
                    last_error = NULL, updated_at = ?8
              WHERE installation_id = ?13 AND behavior_id = ?14",
            params![
                generation,
                binding.package_revision_ref.as_str(),
                i64::try_from(binding.schema_revision.get())
                    .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                i64::try_from(binding.grant_revision.get())
                    .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
                binding.behavior_digest.as_str(),
                interval,
                timestamp(initial_due),
                &now_text,
                timestamp(migrated_period_started),
                migrated_period_seconds,
                migrated_period_starts,
                migrated_max_starts,
                binding.installation_id.as_str(),
                binding.behavior_id.as_str(),
            ],
        )?;
        return Ok(AppBehaviorClaimOutcome::NoDispatch);
    }

    let next_due = parse_timestamp(&row.next_due_at)?;
    let available = parse_timestamp(&row.available_at)?;
    if available > now {
        return Ok(AppBehaviorClaimOutcome::NoDispatch);
    }
    if row.state == "idle" && next_due > now {
        return Ok(AppBehaviorClaimOutcome::NoDispatch);
    }
    if row.state == "blocked"
        && contract.execution_ready
        && row.last_error.as_deref() == Some(APP_BEHAVIOR_EXECUTION_BINDING_MISSING)
    {
        transaction.execute(
            "UPDATE app_behavior_heads
                SET state = 'idle', revision = revision + 1,
                    next_due_at = ?1, available_at = ?2,
                    consecutive_failures = 0, last_error = NULL, updated_at = ?2
              WHERE installation_id = ?3 AND behavior_id = ?4 AND revision = ?5",
            params![
                timestamp(initial_due),
                &now_text,
                binding.installation_id.as_str(),
                binding.behavior_id.as_str(),
                row.revision,
            ],
        )?;
        return Ok(AppBehaviorClaimOutcome::NoDispatch);
    }
    if row.state == "blocked" {
        return Ok(AppBehaviorClaimOutcome::NoDispatch);
    }
    if !contract.execution_ready {
        let reason = APP_BEHAVIOR_EXECUTION_BINDING_MISSING;
        if row.state == "pending" {
            return pending_fenced_transition(
                scope,
                scope_binding_ref,
                binding,
                row,
                initial_due,
                reason,
            );
        } else if row.state == "idle" {
            let changed = transaction.execute(
                "UPDATE app_behavior_heads
                    SET state = 'blocked', revision = revision + 1, next_due_at = ?1,
                        available_at = ?1,
                        attempt_count = MIN(attempt_count + 1, 4294967295),
                        consecutive_failures = MIN(consecutive_failures + 1, 64),
                        last_error = ?2, updated_at = ?3
                  WHERE installation_id = ?4 AND behavior_id = ?5
                    AND revision = ?6 AND state = 'idle'",
                params![
                    timestamp(initial_due),
                    reason,
                    &now_text,
                    binding.installation_id.as_str(),
                    binding.behavior_id.as_str(),
                    row.revision,
                ],
            )?;
            if changed != 1 {
                return Err(AppBehaviorSchedulerError::LeaseLost);
            }
        } else {
            return Err(AppBehaviorSchedulerError::CorruptState);
        }
        return Ok(AppBehaviorClaimOutcome::NoDispatch);
    }
    // A full inventory reconciliation must initialize, retire, and recover
    // every head even after this tick's launch quota is full. It must not mint
    // another lease once that bounded dispatch quota has been exhausted.
    if !allow_dispatch {
        return Ok(AppBehaviorClaimOutcome::NoDispatch);
    }
    let period_started = parse_timestamp(&row.period_started_at)?;
    let period_ends = period_started
        .checked_add_signed(Duration::seconds(row.period_seconds))
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    let (period_started, period_seconds, period_starts, max_starts_per_period) =
        if period_ends <= now {
            (
                now,
                period_seconds,
                0_i64,
                i64::from(binding.max_starts_per_period),
            )
        } else {
            (
                period_started,
                row.period_seconds,
                row.period_starts,
                row.max_starts_per_period,
            )
        };
    if period_starts >= max_starts_per_period {
        // A period reset cannot shorten the reviewed cadence. In particular,
        // a late settlement may put `next_due_at` after the period boundary.
        // Advancing availability as well makes capped heads dormant until the
        // later boundary instead of revision-bumping them on every poll.
        let capped_until = next_due.max(period_ends);
        transaction.execute(
            "UPDATE app_behavior_heads
                SET revision = revision + 1, next_due_at = ?1, available_at = ?1,
                    period_started_at = ?2, period_seconds = ?3,
                    period_starts = ?4, max_starts_per_period = ?5, updated_at = ?6
              WHERE installation_id = ?7 AND behavior_id = ?8",
            params![
                timestamp(capped_until),
                timestamp(period_started),
                period_seconds,
                period_starts,
                max_starts_per_period,
                &now_text,
                binding.installation_id.as_str(),
                binding.behavior_id.as_str(),
            ],
        )?;
        return Ok(AppBehaviorClaimOutcome::NoDispatch);
    }

    let scheduled_at = if row.state == "pending" {
        row.pending_scheduled_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?
            .ok_or(AppBehaviorSchedulerError::CorruptState)?
    } else {
        if row.state != "idle" {
            return Ok(AppBehaviorClaimOutcome::NoDispatch);
        }
        next_due
    };
    let fire_ref = if row.state == "pending" {
        AppReference::parse(
            row.pending_fire_ref
                .as_deref()
                .ok_or(AppBehaviorSchedulerError::CorruptState)?,
        )?
    } else {
        behavior_fire_ref(binding, scheduled_at)?
    };
    let fence = u64::try_from(row.fence)
        .ok()
        .and_then(|fence| fence.checked_add(1))
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    let token = AppDigest::blake3_canonical_json(&json!({
        "fire_ref": &fire_ref,
        "worker": worker,
        "fence": fence,
        "nonce": uuid::Uuid::new_v4(),
    }))?
    .to_string();
    let lease_expires_at = now
        .checked_add_signed(Duration::seconds(
            i64::try_from(lease_seconds).map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
        ))
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    let lease_expires_at = parse_timestamp(&timestamp(lease_expires_at))?;
    let lease = AppBehaviorLease {
        installation_generation: binding.installation_generation,
        fence,
        token: token.clone(),
        fire_ref: fire_ref.clone(),
        effective_interval_seconds: binding.effective_interval_seconds,
        expires_at: lease_expires_at,
        consecutive_failures: u32::try_from(row.consecutive_failures)
            .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
    };
    let existing_evidence = if row.state == "pending" {
        let bytes = row
            .invocation_json
            .as_deref()
            .ok_or(AppBehaviorSchedulerError::CorruptState)?;
        let expected_digest = row
            .invocation_digest
            .as_deref()
            .ok_or(AppBehaviorSchedulerError::CorruptState)?;
        let persisted: PersistedBehaviorInvocation = serde_json::from_slice(bytes)?;
        validate_policy(&persisted.source_policy, &AppContractLimits::default())?;
        if AppDigest::blake3_canonical_json(&serde_json::to_value(&persisted)?)?.as_str()
            != expected_digest
            || !persisted_invocation_matches(
                &persisted,
                binding,
                contract,
                scope_binding_ref,
                &fire_ref,
            )?
        {
            return Err(AppBehaviorSchedulerError::CorruptState);
        }
        Some(persisted)
    } else {
        None
    };
    let material = match build_behavior_input_envelope_in_snapshot(
        transaction,
        scope,
        scope_binding_ref,
        &binding.installation_id,
        binding.installation_generation,
        &binding.package_revision_ref,
        binding.schema_revision,
        binding.grant_revision,
        &contract.selector,
        contract.input_schema_ref.clone(),
        now,
    ) {
        Ok(material) => material,
        Err(AppEntityStoreError::MissingBehaviorSourceRecord) => {
            if row.state == "pending" {
                return pending_fenced_transition(
                    scope,
                    scope_binding_ref,
                    binding,
                    row,
                    initial_due,
                    "source_record_removed",
                );
            } else {
                let next_failure = u32::try_from(row.consecutive_failures)
                    .map_err(|_| AppBehaviorSchedulerError::CorruptState)?
                    .saturating_add(1);
                let available_at = now
                    .checked_add_signed(Duration::seconds(retry_delay_seconds(
                        retry_seconds,
                        next_failure,
                    )?))
                    .ok_or(AppBehaviorSchedulerError::CorruptState)?;
                let changed = transaction.execute(
                    "UPDATE app_behavior_heads
                        SET revision = revision + 1, available_at = ?1,
                            attempt_count = MIN(attempt_count + 1, 4294967295),
                            consecutive_failures = MIN(consecutive_failures + 1, 64),
                            last_error = ?2, updated_at = ?3
                      WHERE installation_id = ?4 AND behavior_id = ?5
                        AND revision = ?6 AND state = 'idle'",
                    params![
                        timestamp(available_at),
                        "source_record_missing",
                        &now_text,
                        binding.installation_id.as_str(),
                        binding.behavior_id.as_str(),
                        row.revision,
                    ],
                )?;
                if changed != 1 {
                    return Err(AppBehaviorSchedulerError::LeaseLost);
                }
            }
            return Ok(AppBehaviorClaimOutcome::NoDispatch);
        },
        Err(error) => return Err(error.into()),
    };
    let mut input = material.envelope;
    if let Some(persisted) = existing_evidence.as_ref() {
        // These timestamps are part of idempotent invocation identity but not
        // the source bytes. Preserve the first claim's values after proving
        // the current source revision, content and policy still match.
        input.produced_at = persisted.invocation.input.produced_at;
        input.expires_at = persisted.invocation.input.expires_at;
    }
    let invocation = scheduled_invocation(binding, contract, &lease, input)?;
    let persisted = persisted_invocation_evidence(&invocation, material.handling_policy.clone());
    if existing_evidence
        .as_ref()
        .is_some_and(|existing| existing != &persisted)
    {
        return pending_fenced_transition(
            scope,
            scope_binding_ref,
            binding,
            row,
            initial_due,
            "source_binding_changed",
        );
    }
    let invocation_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(&persisted)?)?;
    let invocation_json = serde_json::to_vec(&persisted)?;
    let changed = transaction.execute(
        "UPDATE app_behavior_heads
            SET state = 'pending', revision = revision + 1, fence = ?1,
                pending_scheduled_at = ?2, pending_fire_ref = ?3,
                lease_owner = ?4, lease_token = ?5, lease_expires_at = ?6,
                available_at = ?7,
                attempt_count = MIN(attempt_count + 1, 4294967295),
                period_started_at = ?8, period_seconds = ?9, period_starts = ?10,
                max_starts_per_period = ?11,
                invocation_json = ?12, invocation_digest = ?13, updated_at = ?7
          WHERE installation_id = ?14 AND behavior_id = ?15 AND revision = ?16",
        params![
            i64::try_from(fence).map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
            timestamp(scheduled_at),
            fire_ref.as_str(),
            worker,
            &token,
            timestamp(lease_expires_at),
            &now_text,
            timestamp(period_started),
            period_seconds,
            period_starts,
            max_starts_per_period,
            invocation_json,
            invocation_digest.as_str(),
            binding.installation_id.as_str(),
            binding.behavior_id.as_str(),
            row.revision,
        ],
    )?;
    if changed != 1 {
        return Err(AppBehaviorSchedulerError::LeaseLost);
    }
    Ok(AppBehaviorClaimOutcome::Dispatch(AppBehaviorClaim {
        lease,
        invocation,
        source_policy: material.handling_policy,
    }))
}

#[derive(Debug)]
struct BehaviorHeadRow {
    installation_generation: i64,
    package_revision_ref: String,
    schema_revision: i64,
    grant_revision: i64,
    behavior_digest: String,
    state: String,
    revision: i64,
    fence: i64,
    effective_interval_seconds: i64,
    next_due_at: String,
    available_at: String,
    pending_scheduled_at: Option<String>,
    pending_fire_ref: Option<String>,
    lease_expires_at: Option<String>,
    period_started_at: String,
    period_seconds: i64,
    period_starts: i64,
    max_starts_per_period: i64,
    invocation_json: Option<Vec<u8>>,
    invocation_digest: Option<String>,
    consecutive_failures: i64,
    last_error: Option<String>,
}

#[derive(Debug)]
struct AppBehaviorPendingAcceptanceCandidate {
    binding: AppBehaviorBinding,
    row: BehaviorHeadRow,
    task_id: String,
}

#[derive(Clone)]
enum AppBehaviorPendingNegativeDisposition {
    Preserve,
    RetireDisabled,
    RetireUnlessCurrent(Vec<AppBehaviorBinding>),
    Reset {
        next_due_at: DateTime<Utc>,
        reason: &'static str,
    },
}

fn settle_accepted_pending_launch_from_head(
    transaction: &rusqlite::Transaction<'_>,
    scope: &super::records::AppScope,
    current_binding: &AppBehaviorBinding,
    scope_binding_ref: &super::models::AppScopeBindingRef,
    row: &BehaviorHeadRow,
    accepted_task_id: Option<&str>,
    now: DateTime<Utc>,
) -> Result<bool, AppBehaviorSchedulerError> {
    let Some(task_id) = pending_task_id_from_head(scope, current_binding, scope_binding_ref, row)?
    else {
        return Ok(false);
    };
    let fire_ref = AppReference::parse(
        row.pending_fire_ref
            .as_deref()
            .ok_or(AppBehaviorSchedulerError::CorruptState)?,
    )?;
    let launch_ref = AppReference::parse(format!("behavior-launch:{}", fire_ref.as_str()))?;
    let invocation_digest = row
        .invocation_digest
        .as_deref()
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    // Artifact root publication is the acceptance boundary. The registry row
    // remains a required immutable correlation guard, never sufficient proof.
    let binding_active = transaction
        .query_row(
            "SELECT lifecycle_state
               FROM app_workflow_control_heads
              WHERE task_id = ?1 AND execution_id = ?1
                AND control_kind = 'task_binding'",
            params![&task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some_and(|lifecycle| lifecycle == "active")
        || transaction.query_row("SELECT EXISTS(SELECT 1 FROM app_recurring_occurrence_locators WHERE occurrence_id = ?1)",
            [&task_id], |row| row.get::<_, bool>(0))?;
    let accepted = exact_root_acceptance_proof(accepted_task_id, &task_id, binding_active);
    if !accepted {
        return Ok(false);
    }
    let next_due_at = now
        .checked_add_signed(Duration::seconds(row.effective_interval_seconds))
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    let changed = transaction.execute(
        "UPDATE app_behavior_heads
            SET state = 'idle', revision = revision + 1, fence = fence + 1,
                next_due_at = ?1, available_at = ?1,
                pending_scheduled_at = NULL, pending_fire_ref = NULL,
                lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL,
                invocation_json = NULL, invocation_digest = NULL,
                accepted_count = accepted_count + 1,
                consecutive_failures = 0, period_starts = period_starts + 1,
                last_launch_ref = ?2, last_error = NULL, updated_at = ?3
          WHERE installation_id = ?4 AND behavior_id = ?5
            AND revision = ?6 AND fence = ?7 AND state = 'pending'
            AND pending_fire_ref = ?8 AND invocation_digest = ?9",
        params![
            timestamp(next_due_at),
            launch_ref.as_str(),
            timestamp(now),
            current_binding.installation_id.as_str(),
            current_binding.behavior_id.as_str(),
            row.revision,
            row.fence,
            fire_ref.as_str(),
            invocation_digest,
        ],
    )?;
    if changed != 1 {
        return Err(AppBehaviorSchedulerError::LeaseLost);
    }
    Ok(true)
}

fn exact_root_acceptance_proof(
    artifact_accepted_task_id: Option<&str>,
    reconstructed_task_id: &str,
    task_binding_active: bool,
) -> bool {
    task_binding_active && artifact_accepted_task_id == Some(reconstructed_task_id)
}

fn binding_reset_allowed_after_lease(
    lease_expires_at: Option<&str>,
    now: DateTime<Utc>,
) -> Result<bool, AppBehaviorSchedulerError> {
    Ok(!lease_expires_at
        .map(parse_timestamp)
        .transpose()?
        .is_some_and(|expires| expires > now))
}

fn historical_binding_from_head(
    installation_id: &AppInstallationId,
    behavior_id: &AppName,
    row: &BehaviorHeadRow,
) -> Result<AppBehaviorBinding, AppBehaviorSchedulerError> {
    Ok(AppBehaviorBinding {
        installation_id: installation_id.clone(),
        installation_generation: u64::try_from(row.installation_generation)
            .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
        package_revision_ref: AppReference::parse(row.package_revision_ref.clone())?,
        schema_revision: AppRevision::new(
            u64::try_from(row.schema_revision)
                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
        )?,
        grant_revision: AppRevision::new(
            u64::try_from(row.grant_revision)
                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
        )?,
        behavior_id: behavior_id.clone(),
        behavior_digest: AppDigest::parse(row.behavior_digest.clone())?,
        effective_interval_seconds: u64::try_from(row.effective_interval_seconds)
            .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
        max_starts_per_period: u32::try_from(row.max_starts_per_period)
            .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
        period_seconds: u64::try_from(row.period_seconds)
            .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
    })
}

/// Reconstruct the exact task identity from the durable historical head. This
/// deliberately does not use the current package/grant binding: a root may
/// have been accepted immediately before that authority changed.
fn pending_task_id_from_head(
    scope: &super::records::AppScope,
    current_binding: &AppBehaviorBinding,
    scope_binding_ref: &super::models::AppScopeBindingRef,
    row: &BehaviorHeadRow,
) -> Result<Option<String>, AppBehaviorSchedulerError> {
    if row.state != "pending" {
        return Ok(None);
    }
    let scheduled_at = row
        .pending_scheduled_at
        .as_deref()
        .map(parse_timestamp)
        .transpose()?
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    let fire_ref = AppReference::parse(
        row.pending_fire_ref
            .as_deref()
            .ok_or(AppBehaviorSchedulerError::CorruptState)?,
    )?;
    let historical_binding = historical_binding_from_head(
        &current_binding.installation_id,
        &current_binding.behavior_id,
        row,
    )?;
    if behavior_fire_ref(&historical_binding, scheduled_at)? != fire_ref {
        return Err(AppBehaviorSchedulerError::CorruptState);
    }
    let bytes = row
        .invocation_json
        .as_deref()
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    let expected_digest = row
        .invocation_digest
        .as_deref()
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    let persisted: PersistedBehaviorInvocation = serde_json::from_slice(bytes)?;
    validate_policy(&persisted.source_policy, &AppContractLimits::default())?;
    if AppDigest::blake3_canonical_json(&serde_json::to_value(&persisted)?)?.as_str()
        != expected_digest
        || !persisted_invocation_matches_historical_head(
            &persisted,
            &historical_binding,
            scope_binding_ref,
            &fire_ref,
        )?
    {
        return Err(AppBehaviorSchedulerError::CorruptState);
    }
    let launch_ref = AppReference::parse(format!("behavior-launch:{}", fire_ref.as_str()))?;
    let task_id = app_workflow_task_id_for_scope(
        scope,
        &historical_binding.installation_id,
        &persisted.invocation.action_id,
        &persisted.invocation.idempotency_key,
        Some(&launch_ref),
    )?;
    Ok(Some(task_id))
}

fn persisted_invocation_matches_historical_head(
    persisted: &PersistedBehaviorInvocation,
    binding: &AppBehaviorBinding,
    scope_binding_ref: &super::models::AppScopeBindingRef,
    fire_ref: &AppReference,
) -> Result<bool, AppBehaviorSchedulerError> {
    let invocation = &persisted.invocation;
    let source_policy_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&persisted.source_policy)?)?;
    Ok(invocation.input.value.is_null()
        && invocation.protocol_version == AppProtocolVersion::V1
        && invocation.idempotency_key.as_str() == format!("behavior-fire:{}", fire_ref.as_str())
        && invocation.action_revision.get() == binding.installation_generation
        && invocation.input.protocol_version == AppProtocolVersion::V1
        && invocation.input.source == AppDataSource::AppStore
        && &invocation.input.scope_binding_ref == scope_binding_ref
        && invocation.input.installation_id == binding.installation_id
        && invocation.input.package_revision_ref == binding.package_revision_ref
        && invocation.input.schema_revision == binding.schema_revision
        && invocation.input.grant_revision == binding.grant_revision
        && invocation.input.handling_labels.policy_digest == source_policy_digest
        && invocation.caller_surface_or_execution_ref.as_str()
            == format!("behavior:{}", binding.behavior_id.as_str())
        && matches!(invocation.input.source_refs.as_slice(), [source]
            if source.kind == AppSourceRefKind::EntityField
                && source.revision.is_some()
                && !source.fields.is_empty()))
}

fn persisted_invocation_matches(
    persisted: &PersistedBehaviorInvocation,
    binding: &AppBehaviorBinding,
    contract: &AppBehaviorInvocationContract,
    scope_binding_ref: &super::models::AppScopeBindingRef,
    fire_ref: &AppReference,
) -> Result<bool, AppBehaviorSchedulerError> {
    let invocation = &persisted.invocation;
    let selector_identity = AppDigest::blake3_canonical_json(&json!({
        "entity": &contract.selector.entity,
        "record_id": &contract.selector.record_id,
    }))?;
    let expected_reference = AppReference::parse(format!("record:{}", selector_identity.as_str()))?;
    let expected_fields = contract
        .selector
        .fields
        .iter()
        .map(|field| AppFieldPath::parse(field.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let source_policy_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&persisted.source_policy)?)?;
    Ok(invocation.input.value.is_null()
        && invocation.protocol_version == AppProtocolVersion::V1
        && invocation.idempotency_key.as_str() == format!("behavior-fire:{}", fire_ref.as_str())
        && invocation.action_id == contract.action_id
        && invocation.action_revision.get() == binding.installation_generation
        && invocation.input.protocol_version == AppProtocolVersion::V1
        && invocation.input.source == AppDataSource::AppStore
        && &invocation.input.scope_binding_ref == scope_binding_ref
        && invocation.input.installation_id == binding.installation_id
        && invocation.input.package_revision_ref == binding.package_revision_ref
        && invocation.input.schema_revision == binding.schema_revision
        && invocation.input.grant_revision == binding.grant_revision
        && invocation.input.value_schema_ref == contract.input_schema_ref
        && invocation.requested_result_schema_ref == contract.result_schema_ref
        && invocation.input.handling_labels.policy_digest == source_policy_digest
        && invocation.caller_surface_or_execution_ref.as_str()
            == format!("behavior:{}", binding.behavior_id.as_str())
        && matches!(invocation.input.source_refs.as_slice(), [source]
            if source.kind == AppSourceRefKind::EntityField
                && source.reference == expected_reference
                && source.revision.is_some()
                && source.fields == expected_fields))
}

fn persisted_invocation_evidence(
    invocation: &AppActionInvocation<Value>,
    source_policy: AppDataHandlingPolicy,
) -> PersistedBehaviorInvocation {
    let mut invocation = invocation.clone();
    invocation.input.value = Value::Null;
    PersistedBehaviorInvocation {
        invocation,
        source_policy,
    }
}

fn retire_pending_candidate(
    transaction: &rusqlite::Transaction<'_>,
    candidate: &AppBehaviorPendingAcceptanceCandidate,
    reason: &str,
    require_disabled: bool,
    now: DateTime<Utc>,
) -> Result<(), AppBehaviorSchedulerError> {
    let pending_fire_ref = candidate
        .row
        .pending_fire_ref
        .as_deref()
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    let invocation_digest = candidate
        .row
        .invocation_digest
        .as_deref()
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    let changed = transaction.execute(
        "DELETE FROM app_behavior_heads
          WHERE installation_id = ?1 AND behavior_id = ?2
            AND revision = ?3 AND fence = ?4 AND pending_fire_ref = ?5
            AND invocation_digest = ?6
            AND state = 'pending' AND available_at <= ?7
            AND (lease_expires_at IS NULL OR lease_expires_at <= ?7)
            AND (?8 = 0 OR EXISTS (
                SELECT 1 FROM app_installations i
                 WHERE i.installation_id = ?1
                   AND i.lifecycle_status <> 'enabled'
            ))",
        params![
            candidate.binding.installation_id.as_str(),
            candidate.binding.behavior_id.as_str(),
            candidate.row.revision,
            candidate.row.fence,
            pending_fire_ref,
            invocation_digest,
            timestamp(now),
            if require_disabled { 1_i64 } else { 0_i64 },
        ],
    )?;
    if changed != 1 {
        return Err(AppBehaviorSchedulerError::LeaseLost);
    }
    append_behavior_event(
        transaction,
        candidate.binding.installation_id.as_str(),
        Some(candidate.binding.behavior_id.as_str()),
        "retired",
        reason,
        now,
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn abandon_pending_source(
    transaction: &rusqlite::Transaction<'_>,
    binding: &AppBehaviorBinding,
    revision: i64,
    fence: i64,
    pending_fire_ref: &str,
    invocation_digest: &str,
    next_due_at: DateTime<Utc>,
    now: DateTime<Utc>,
    error: &str,
) -> Result<(), AppBehaviorSchedulerError> {
    let next_fence = fence
        .checked_add(1)
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    let changed = transaction.execute(
        "UPDATE app_behavior_heads
            SET state = 'idle', revision = revision + 1, fence = ?1,
                next_due_at = ?2, available_at = ?3,
                pending_scheduled_at = NULL, pending_fire_ref = NULL,
                lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL,
                invocation_json = NULL, invocation_digest = NULL,
                attempt_count = MIN(attempt_count + 1, 4294967295), last_error = ?4,
                consecutive_failures = MIN(consecutive_failures + 1, 64),
                updated_at = ?3
          WHERE installation_id = ?5 AND behavior_id = ?6
            AND revision = ?7 AND fence = ?8 AND pending_fire_ref = ?9
            AND invocation_digest = ?10 AND state = 'pending'",
        params![
            next_fence,
            timestamp(next_due_at),
            timestamp(now),
            error,
            binding.installation_id.as_str(),
            binding.behavior_id.as_str(),
            revision,
            fence,
            pending_fire_ref,
            invocation_digest,
        ],
    )?;
    if changed != 1 {
        return Err(AppBehaviorSchedulerError::LeaseLost);
    }
    Ok(())
}

fn behavior_fire_ref(
    binding: &AppBehaviorBinding,
    scheduled_at: DateTime<Utc>,
) -> Result<AppReference, AppBehaviorSchedulerError> {
    let digest = AppDigest::blake3_canonical_json(&json!({
        "schema": "magician.app-behavior-fire.v1",
        "installation_id": &binding.installation_id,
        "installation_generation": binding.installation_generation,
        "package_revision_ref": &binding.package_revision_ref,
        "grant_revision": binding.grant_revision,
        "behavior_id": &binding.behavior_id,
        "behavior_digest": &binding.behavior_digest,
        "scheduled_at": timestamp(scheduled_at),
    }))?;
    Ok(AppReference::parse(format!(
        "fire:{}",
        digest.as_str().trim_start_matches("blake3:")
    ))?)
}

fn load_scope_policy(
    connection: &rusqlite::Connection,
    now: DateTime<Utc>,
) -> Result<AppBehaviorScopePolicy, AppBehaviorSchedulerError> {
    let loaded = connection
        .query_row(
            "SELECT revision, paused, updated_at
               FROM app_behavior_scope_policy WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    match loaded {
        Some((revision, paused, updated_at)) => Ok(AppBehaviorScopePolicy {
            revision: u64::try_from(revision)
                .map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
            paused: match paused {
                0 => false,
                1 => true,
                _ => return Err(AppBehaviorSchedulerError::CorruptState),
            },
            updated_at: parse_timestamp(&updated_at)?,
        }),
        None => Ok(default_scope_policy(now)),
    }
}

fn default_scope_policy(now: DateTime<Utc>) -> AppBehaviorScopePolicy {
    AppBehaviorScopePolicy {
        revision: 0,
        paused: false,
        updated_at: now,
    }
}

fn retry_delay_seconds(
    base_seconds: u64,
    consecutive_failures: u32,
) -> Result<i64, AppBehaviorSchedulerError> {
    let exponent = consecutive_failures.saturating_sub(1).min(6);
    let multiplier = 1_u64
        .checked_shl(exponent)
        .ok_or(AppBehaviorSchedulerError::CorruptState)?;
    i64::try_from(base_seconds.saturating_mul(multiplier).min(3_600))
        .map_err(|_| AppBehaviorSchedulerError::CorruptState)
}

fn retry_blocked_launch_blocking(
    connection: &mut rusqlite::Connection,
    installation_id: &AppInstallationId,
    behavior_id: &AppName,
    expected_generation: u64,
    expected_revision: u64,
    now: DateTime<Utc>,
) -> Result<u64, AppBehaviorSchedulerError> {
    let generation = i64::try_from(expected_generation)
        .map_err(|_| AppBehaviorSchedulerError::InvalidRuntimePolicy)?;
    let revision = i64::try_from(expected_revision)
        .map_err(|_| AppBehaviorSchedulerError::InvalidRuntimePolicy)?;
    let next_revision = revision
        .checked_add(1)
        .filter(|_| generation > 0 && revision > 0)
        .ok_or(AppBehaviorSchedulerError::InvalidRuntimePolicy)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Older completion observations could overwrite a blocked launch's reason.
    // Recover that state only when the exact current occurrence is failed and
    // fully settled. Never turn an active, stale or unobserved root into a retry.
    let settled_failure =
        recurring::has_settled_current_failure(&transaction, installation_id, behavior_id)?;
    let changed = transaction.execute(
        "UPDATE app_behavior_heads
            SET state = 'idle', revision = ?1,
                available_at = ?2, last_error = NULL, updated_at = ?2
          WHERE installation_id = ?3 AND behavior_id = ?4
            AND installation_generation = ?5 AND revision = ?6
            AND state = 'blocked'
            AND (last_error = 'workflow_launch_blocked'
                 OR (last_error = 'workflow_execution_failed' AND ?7))
            AND pending_fire_ref IS NULL AND pending_scheduled_at IS NULL
            AND lease_owner IS NULL AND lease_token IS NULL
            AND lease_expires_at IS NULL AND invocation_json IS NULL
            AND invocation_digest IS NULL",
        params![
            next_revision,
            timestamp(now),
            installation_id.as_str(),
            behavior_id.as_str(),
            generation,
            revision,
            settled_failure
        ],
    )?;
    if changed != 1 {
        return Err(AppBehaviorSchedulerError::LeaseLost);
    }
    append_behavior_event(
        &transaction,
        installation_id.as_str(),
        Some(behavior_id.as_str()),
        "retry_requested",
        "owner_requested",
        now,
    )?;
    transaction.commit()?;
    Ok(next_revision as u64)
}

fn append_behavior_event(
    connection: &rusqlite::Connection,
    installation_id: &str,
    behavior_id: Option<&str>,
    kind: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<(), AppBehaviorSchedulerError> {
    connection.execute(
        "INSERT INTO app_behavior_events(
             installation_id, behavior_id, kind, reason, observed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![installation_id, behavior_id, kind, reason, timestamp(now)],
    )?;
    connection.execute(
        "DELETE FROM app_behavior_events
          WHERE event_id NOT IN (
              SELECT event_id FROM app_behavior_events
               ORDER BY event_id DESC LIMIT ?1
          )",
        params![APP_BEHAVIOR_EVENT_RETENTION],
    )?;
    Ok(())
}

fn decode_health_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppBehaviorHealthItem> {
    let installation_generation = row.get::<_, i64>(1)?;
    let revision = row.get::<_, i64>(5)?;
    let fence = row.get::<_, i64>(6)?;
    let interval = row.get::<_, i64>(7)?;
    let accepted_count = row.get::<_, i64>(10)?;
    let attempt_count = row.get::<_, i64>(11)?;
    let consecutive_failures = row.get::<_, i64>(16)?;
    let period_seconds = row.get::<_, i64>(18)?;
    let period_starts = row.get::<_, i64>(19)?;
    let max_starts_per_period = row.get::<_, i64>(20)?;
    let parse_error = |index: usize, error: &dyn std::error::Error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()).into(),
        )
    };
    let installation_id_raw = row.get::<_, String>(0)?;
    let behavior_id_raw = row.get::<_, String>(2)?;
    let package_ref_raw = row.get::<_, String>(3)?;
    let next_due_raw = row.get::<_, String>(8)?;
    let pending_fire_raw = row.get::<_, Option<String>>(9)?;
    let updated_at_raw = row.get::<_, String>(13)?;
    let available_at_raw = row.get::<_, String>(14)?;
    let lease_expires_at_raw = row.get::<_, Option<String>>(15)?;
    let period_started_at_raw = row.get::<_, String>(17)?;
    Ok(AppBehaviorHealthItem {
        recurring: None,
        installation_id: AppInstallationId::parse(installation_id_raw)
            .map_err(|error| parse_error(0, &error))?,
        installation_generation: u64::try_from(installation_generation)
            .map_err(|error| parse_error(1, &error))?,
        behavior_id: AppName::parse(behavior_id_raw).map_err(|error| parse_error(2, &error))?,
        package_revision_ref: AppReference::parse(package_ref_raw)
            .map_err(|error| parse_error(3, &error))?,
        state: row.get(4)?,
        revision: u64::try_from(revision).map_err(|error| parse_error(5, &error))?,
        fence: u64::try_from(fence).map_err(|error| parse_error(6, &error))?,
        effective_interval_seconds: u64::try_from(interval)
            .map_err(|error| parse_error(7, &error))?,
        next_due_at: parse_timestamp(&next_due_raw).map_err(|error| parse_error(8, &error))?,
        available_at: parse_timestamp(&available_at_raw)
            .map_err(|error| parse_error(14, &error))?,
        pending_fire_ref: pending_fire_raw
            .map(AppReference::parse)
            .transpose()
            .map_err(|error| parse_error(9, &error))?,
        lease_expires_at: lease_expires_at_raw
            .as_deref()
            .map(parse_timestamp)
            .transpose()
            .map_err(|error| parse_error(15, &error))?,
        accepted_count: u64::try_from(accepted_count).map_err(|error| parse_error(10, &error))?,
        attempt_count: u32::try_from(attempt_count).map_err(|error| parse_error(11, &error))?,
        consecutive_failures: u32::try_from(consecutive_failures)
            .map_err(|error| parse_error(16, &error))?,
        period_started_at: parse_timestamp(&period_started_at_raw)
            .map_err(|error| parse_error(17, &error))?,
        period_seconds: u64::try_from(period_seconds).map_err(|error| parse_error(18, &error))?,
        period_starts: u32::try_from(period_starts).map_err(|error| parse_error(19, &error))?,
        max_starts_per_period: u32::try_from(max_starts_per_period)
            .map_err(|error| parse_error(20, &error))?,
        last_error: row.get(12)?,
        updated_at: parse_timestamp(&updated_at_raw).map_err(|error| parse_error(13, &error))?,
    })
}

fn decode_health_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppBehaviorHealthEvent> {
    let event_id = row.get::<_, i64>(0)?;
    let installation_id = row.get::<_, String>(1)?;
    let behavior_id = row.get::<_, Option<String>>(2)?;
    let observed_at = row.get::<_, String>(5)?;
    let parse_error = |index: usize, error: &dyn std::error::Error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()).into(),
        )
    };
    Ok(AppBehaviorHealthEvent {
        event_id: u64::try_from(event_id).map_err(|error| parse_error(0, &error))?,
        installation_id: AppInstallationId::parse(installation_id)
            .map_err(|error| parse_error(1, &error))?,
        behavior_id: behavior_id
            .map(AppName::parse)
            .transpose()
            .map_err(|error| parse_error(2, &error))?,
        kind: row.get(3)?,
        reason: row.get(4)?,
        observed_at: parse_timestamp(&observed_at).map_err(|error| parse_error(5, &error))?,
    })
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, AppBehaviorSchedulerError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| AppBehaviorSchedulerError::CorruptState)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_runtime_concurrency_regression_owner_retry_preserves_fire_and_counters() {
        let (_root, mut db) =
            super::super::registry::tests::background_scheduler_database_fixture();
        let mut binding = binding();
        binding.installation_id = AppInstallationId::parse("install_legacy").unwrap();
        db.execute(
            "INSERT INTO app_behavior_heads(
                installation_id, behavior_id, installation_generation, package_revision_ref,
                schema_revision, grant_revision, behavior_digest, state, revision, fence,
                effective_interval_seconds, next_due_at, available_at, last_error, updated_at,
                accepted_count, attempt_count, consecutive_failures, period_starts,
                period_started_at, period_seconds, max_starts_per_period)
             VALUES('install_legacy', 'daily_digest', 7, 'package-revision:test',
                3, 5, ?1, 'blocked', 33, 17, 300,
                '2026-09-10T00:41:35Z', '2026-09-10T01:00:47Z',
                'workflow_launch_blocked', '2026-09-10T01:00:48Z',
                14, 16, 1, 2, '2026-09-10T00:31:31Z', 3600, 12)",
            params![binding.behavior_digest.as_str()],
        )
        .unwrap();
        let now = parse_timestamp("2026-09-10T02:00:00Z").unwrap();
        for (generation, revision) in [(6, 33), (7, 32)] {
            assert!(matches!(
                retry_blocked_launch_blocking(
                    &mut db,
                    &binding.installation_id,
                    &binding.behavior_id,
                    generation,
                    revision,
                    now,
                ),
                Err(AppBehaviorSchedulerError::LeaseLost)
            ));
        }
        db.execute_batch("CREATE TRIGGER reject_retry_event BEFORE INSERT ON app_behavior_events BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;").unwrap();
        assert!(retry_blocked_launch_blocking(
            &mut db,
            &binding.installation_id,
            &binding.behavior_id,
            7,
            33,
            now,
        )
        .is_err());
        assert_eq!(
            db.query_row("SELECT state FROM app_behavior_heads", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "blocked"
        );
        db.execute_batch("DROP TRIGGER reject_retry_event").unwrap();
        assert_eq!(
            retry_blocked_launch_blocking(
                &mut db,
                &binding.installation_id,
                &binding.behavior_id,
                7,
                33,
                now,
            )
            .unwrap(),
            34
        );
        let retained = db
            .query_row(
                "SELECT next_due_at, fence, accepted_count, attempt_count,
                    consecutive_failures, period_starts, period_started_at,
                    max_starts_per_period FROM app_behavior_heads",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u64>(1)?,
                        r.get::<_, u64>(2)?,
                        r.get::<_, u64>(3)?,
                        r.get::<_, u64>(4)?,
                        r.get::<_, u64>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, u64>(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            retained,
            (
                "2026-09-10T00:41:35Z".to_owned(),
                17,
                14,
                16,
                1,
                2,
                "2026-09-10T00:31:31Z".to_owned(),
                12
            )
        );
        assert_eq!(
            db.query_row("SELECT state FROM app_behavior_heads", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "idle"
        );
        assert_eq!(db.query_row("SELECT COUNT(*) FROM app_behavior_events WHERE kind='retry_requested' AND reason='owner_requested'", [], |r| r.get::<_, u64>(0)).unwrap(), 1);
        for revision in [33, 34] {
            assert!(matches!(
                retry_blocked_launch_blocking(
                    &mut db,
                    &binding.installation_id,
                    &binding.behavior_id,
                    7,
                    revision,
                    now,
                ),
                Err(AppBehaviorSchedulerError::LeaseLost)
            ));
        }
        db.execute(
            "UPDATE app_behavior_heads SET revision=35, fence=18, state='pending',
            pending_scheduled_at='2026-09-10T00:41:35Z', pending_fire_ref='fire:pending',
            invocation_json=?1, invocation_digest=?2,
            lease_owner='worker', lease_token=?2, lease_expires_at='2026-09-10T02:01:00Z'",
            params![b"{}".as_slice(), binding.behavior_digest.as_str()],
        )
        .unwrap();
        assert!(matches!(
            retry_blocked_launch_blocking(
                &mut db,
                &binding.installation_id,
                &binding.behavior_id,
                7,
                35,
                now
            ),
            Err(AppBehaviorSchedulerError::LeaseLost)
        ));
        db.execute(
            "UPDATE app_behavior_heads SET revision=36, state='blocked',
            pending_scheduled_at=NULL, pending_fire_ref=NULL, invocation_json=NULL,
            invocation_digest=NULL, lease_owner=NULL, lease_token=NULL,
            lease_expires_at=NULL, last_error='source_record_missing'",
            [],
        )
        .unwrap();
        assert!(matches!(
            retry_blocked_launch_blocking(
                &mut db,
                &binding.installation_id,
                &binding.behavior_id,
                7,
                36,
                now
            ),
            Err(AppBehaviorSchedulerError::LeaseLost)
        ));
    }

    fn binding() -> AppBehaviorBinding {
        AppBehaviorBinding {
            installation_id: AppInstallationId::parse("install_behavior_test").unwrap(),
            installation_generation: 7,
            package_revision_ref: AppReference::parse("package-revision:test").unwrap(),
            schema_revision: AppRevision::new(3).unwrap(),
            grant_revision: AppRevision::new(5).unwrap(),
            behavior_id: AppName::parse("daily_digest").unwrap(),
            behavior_digest: AppDigest::blake3(b"behavior-v1"),
            effective_interval_seconds: 900,
            max_starts_per_period: 4,
            period_seconds: 3_600,
        }
    }

    #[test]
    fn fire_identity_is_stable_and_binds_authority_and_schedule() {
        let scheduled_at = parse_timestamp("2026-09-02T10:00:00.000000Z").unwrap();
        let original = binding();
        let fire = behavior_fire_ref(&original, scheduled_at).unwrap();
        assert_eq!(fire, behavior_fire_ref(&original, scheduled_at).unwrap());

        let mut changed_authority = original.clone();
        changed_authority.behavior_digest = AppDigest::blake3(b"behavior-v2");
        assert_ne!(
            fire,
            behavior_fire_ref(&changed_authority, scheduled_at).unwrap()
        );
        assert_ne!(
            fire,
            behavior_fire_ref(
                &original,
                parse_timestamp("2026-09-02T10:15:00.000000Z").unwrap(),
            )
            .unwrap()
        );
    }

    #[test]
    fn retry_delay_is_bounded_exponential_backoff() {
        assert_eq!(retry_delay_seconds(30, 0).unwrap(), 30);
        assert_eq!(retry_delay_seconds(30, 1).unwrap(), 30);
        assert_eq!(retry_delay_seconds(30, 2).unwrap(), 60);
        assert_eq!(retry_delay_seconds(30, 7).unwrap(), 1_920);
        assert_eq!(retry_delay_seconds(120, u32::MAX).unwrap(), 3_600);
    }

    #[test]
    fn recurring_task_binding_without_exact_artifact_root_is_not_acceptance() {
        let task_id = "task:exact-fire";
        assert!(!exact_root_acceptance_proof(None, task_id, true));
        assert!(!exact_root_acceptance_proof(
            Some("task:another-fire"),
            task_id,
            true,
        ));
        assert!(!exact_root_acceptance_proof(Some(task_id), task_id, false));
        assert!(exact_root_acceptance_proof(Some(task_id), task_id, true));
    }

    #[test]
    fn live_lease_blocks_binding_reset_until_expiry() {
        let now = parse_timestamp("2026-09-02T10:00:00.000000Z").unwrap();
        assert!(
            !binding_reset_allowed_after_lease(Some("2026-09-02T10:00:00.000001Z"), now,).unwrap()
        );
        assert!(
            binding_reset_allowed_after_lease(Some("2026-09-02T10:00:00.000000Z"), now,).unwrap()
        );
        assert!(binding_reset_allowed_after_lease(None, now).unwrap());
    }

    #[test]
    fn reconciliation_batch_resumes_after_durable_cursor_without_wrapping() {
        assert!(AppName::parse(APP_BEHAVIOR_INSTALLATION_SCAN_SENTINEL).is_ok());
        let installations = ["install_a", "install_b", "install_c", "install_d"];
        assert_eq!(
            reconciliation_batch_bounds(&installations, None, 2),
            (0, 2, false)
        );
        assert_eq!(
            reconciliation_batch_bounds(&installations, Some("install_b"), 2),
            (2, 4, true)
        );
        assert_eq!(
            reconciliation_batch_bounds(&installations, Some("install_z"), 2),
            (4, 4, true)
        );
    }

    #[test]
    fn reconciliation_completion_requires_both_inventory_and_retirement_exhaustion() {
        assert!(!reconciliation_pass_complete(false, false, true));
        assert!(!reconciliation_pass_complete(true, false, true));
        assert!(!reconciliation_pass_complete(false, true, true));
        assert!(!reconciliation_pass_complete(true, true, false));
        assert!(reconciliation_pass_complete(true, true, true));
    }

    #[test]
    fn reconciliation_debt_is_deduplicated_and_cleared_by_a_later_visit() {
        let installation_id = AppInstallationId::parse("install_deferred").unwrap();
        let retained = reconciliation_deferred_after_visit(&[], &installation_id, true);
        assert_eq!(retained, vec![installation_id.clone()]);
        assert_eq!(
            reconciliation_deferred_after_visit(&retained, &installation_id, true),
            retained
        );
        assert!(reconciliation_deferred_after_visit(&retained, &installation_id, false).is_empty());
    }

    #[test]
    fn recurring_due_installation_page_includes_observation_and_applies_enabled_filter() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_installations(
                     installation_id TEXT PRIMARY KEY,
                     lifecycle_status TEXT NOT NULL
                 );
                 CREATE TABLE app_behavior_heads(
                     installation_id TEXT NOT NULL,
                     behavior_id TEXT NOT NULL DEFAULT 'daily_digest',
                     state TEXT NOT NULL,
                     next_due_at TEXT NOT NULL,
                     available_at TEXT NOT NULL,
                     lease_expires_at TEXT
                 );
                 CREATE TABLE app_behavior_execution_state(
                     installation_id TEXT NOT NULL,
                     behavior_id TEXT NOT NULL,
                     needs_observation INTEGER NOT NULL
                 );
                 INSERT INTO app_installations VALUES
                     ('install_a', 'enabled'),
                     ('install_b', 'disabled'),
                     ('install_c', 'enabled'),
                     ('install_d', 'enabled'),
                     ('install_e', 'enabled');
                 INSERT INTO app_behavior_execution_state VALUES
                     ('install_a', 'daily_digest', 1), ('install_b', 'daily_digest', 1),
                     ('install_e', 'daily_digest', 1), ('install_c', 'retired_behavior', 1);
                 INSERT INTO app_behavior_heads(installation_id, state, next_due_at, available_at, lease_expires_at) VALUES
                     ('install_a', 'idle', '2026-09-02T00:00:00Z', '2026-09-02T00:00:00Z', NULL),
                     ('install_b', 'idle', '2026-09-02T00:00:00Z', '2026-09-02T00:00:00Z', NULL),
                     ('install_c', 'idle', '2026-09-02T00:00:00Z', '2026-09-02T00:00:00Z', NULL),
                     ('install_d', 'pending', '2026-09-03T00:00:00Z', '2026-09-02T00:00:00Z', '2026-09-02T00:00:00Z'),
                     ('install_e', 'idle', '2026-09-03T00:00:00Z', '2026-09-03T00:00:00Z', NULL);",
            )
            .unwrap();
        let mut statement = connection
            .prepare(APP_BEHAVIOR_DUE_INSTALLATIONS_SQL)
            .unwrap();
        let rotated = statement
            .query_map(params!["2026-09-02T01:00:00Z", "install_c", 2_i64], |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rotated,
            vec!["install_d".to_owned(), "install_e".to_owned()]
        );
        let unrotated = statement
            .query_map(
                params!["2026-09-02T01:00:00Z", Option::<String>::None, 8_i64,],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            unrotated,
            vec![
                "install_a".to_owned(),
                "install_c".to_owned(),
                "install_d".to_owned(),
                "install_e".to_owned(),
            ]
        );
    }

    /// A budget too small to finish is worse than no budget at all.
    ///
    /// The scope window is shared, so the tail of a multi-app scope used to be
    /// handed whatever milliseconds remained — 492ms was observed — spend them,
    /// time out, and be recorded as an isolated installation. `None` makes the
    /// caller break and resume from the scan cursor next tick instead.
    #[test]
    fn a_budget_below_the_floor_defers_instead_of_starting_doomed_work() {
        let now = parse_timestamp("2026-09-02T10:00:00.000000Z").unwrap();
        let nearly_spent = parse_timestamp("2026-09-02T10:00:00.492000Z").unwrap();
        assert_eq!(
            installation_work_budget(nearly_spent, now, APP_BEHAVIOR_INSTALLATION_WORK_SECONDS),
            None,
            "492ms is the starved budget that produced an isolated installation"
        );

        // Exactly at the floor is workable, so the boundary does not silently
        // discard a tick's worth of progress.
        let at_floor = now + Duration::milliseconds(APP_BEHAVIOR_INSTALLATION_MIN_WORK_MILLIS);
        assert_eq!(
            installation_work_budget(at_floor, now, APP_BEHAVIOR_INSTALLATION_WORK_SECONDS),
            Some(StdDuration::from_millis(
                APP_BEHAVIOR_INSTALLATION_MIN_WORK_MILLIS as u64
            ))
        );

        // A healthy window is still capped by the per-installation maximum, so
        // one installation cannot consume the whole scope window.
        let generous = now + Duration::seconds(30);
        assert_eq!(
            installation_work_budget(generous, now, APP_BEHAVIOR_INSTALLATION_WORK_SECONDS),
            Some(StdDuration::from_secs(
                APP_BEHAVIOR_INSTALLATION_WORK_SECONDS as u64
            ))
        );
    }

    #[test]
    fn installation_work_budget_caps_one_item_and_refuses_expired_deadline() {
        let now = parse_timestamp("2026-09-02T10:00:00.000000Z").unwrap();
        let long_deadline = parse_timestamp("2026-09-02T10:00:10.000000Z").unwrap();
        let short_deadline = parse_timestamp("2026-09-02T10:00:01.500000Z").unwrap();
        assert_eq!(
            installation_work_budget(long_deadline, now, 3),
            Some(StdDuration::from_secs(3))
        );
        assert_eq!(
            installation_work_budget(short_deadline, now, 3),
            Some(StdDuration::from_millis(1_500))
        );
        assert_eq!(installation_work_budget(now, now, 3), None);
    }
}

fn begin_reconciliation_window_blocking(
    connection: &mut rusqlite::Connection,
    now: DateTime<Utc>,
) -> Result<Option<AppBehaviorReconciliationWindow>, AppBehaviorSchedulerError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior = transaction
        .query_row(
            "SELECT reconciled_at, pass_started_at, cursor_installation_id,
                    deferred_installation_ids_json
               FROM app_behavior_reconcile_state WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .optional()?;
    if let Some((_, Some(pass_started_at), cursor, deferred)) = prior.as_ref() {
        let window = AppBehaviorReconciliationWindow {
            pass_started_at: parse_timestamp(pass_started_at)?,
            after_installation_id: cursor.as_ref().map(AppInstallationId::parse).transpose()?,
            deferred_installation_ids: decode_reconciliation_deferred_installations(deferred)?,
        };
        transaction.commit()?;
        return Ok(Some(window));
    }
    let last_pass = prior
        .as_ref()
        .map(|(reconciled_at, _, _, _)| parse_timestamp(reconciled_at))
        .transpose()?;
    let changed_since_pass = if let Some(last_pass) = last_pass {
        transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM app_installations WHERE updated_at > ?1)",
            params![timestamp(last_pass)],
            |row| row.get::<_, bool>(0),
        )?
    } else {
        false
    };
    // Lifecycle changes invalidate the periodic inventory snapshot. They do
    // not change behavior cadence, consumed starts, grants or live leases.
    let due = changed_since_pass
        || last_pass.is_none_or(|last_pass| {
            now.signed_duration_since(last_pass).num_seconds() >= APP_BEHAVIOR_RECONCILE_SECONDS
        });
    if !due {
        transaction.commit()?;
        return Ok(None);
    }
    let pass_started_at = timestamp(now);
    if prior.is_some() {
        let changed = transaction.execute(
            "UPDATE app_behavior_reconcile_state
                SET pass_started_at = ?1, cursor_installation_id = NULL,
                    deferred_installation_ids_json = X'5b5d'
              WHERE singleton = 1 AND pass_started_at IS NULL",
            params![&pass_started_at],
        )?;
        if changed != 1 {
            return Err(AppBehaviorSchedulerError::LeaseLost);
        }
    } else {
        transaction.execute(
            "INSERT INTO app_behavior_reconcile_state(
                 singleton, reconciled_at, pass_started_at,
                 cursor_installation_id, deferred_installation_ids_json
             ) VALUES (1, ?1, ?2, NULL, X'5b5d')",
            params!["1970-01-01T00:00:00.000000Z", &pass_started_at],
        )?;
    }
    transaction.commit()?;
    Ok(Some(AppBehaviorReconciliationWindow {
        pass_started_at: now,
        after_installation_id: None,
        deferred_installation_ids: Vec::new(),
    }))
}

fn complete_reconciliation_window_blocking(
    connection: &rusqlite::Connection,
    pass_started_at: &str,
    expected_cursor: Option<&str>,
) -> Result<(), AppBehaviorSchedulerError> {
    let changed = connection.execute(
        "UPDATE app_behavior_reconcile_state
            SET reconciled_at = ?1, pass_started_at = NULL,
                cursor_installation_id = NULL,
                deferred_installation_ids_json = X'5b5d'
          WHERE singleton = 1 AND pass_started_at = ?2
            AND ((cursor_installation_id IS NULL AND ?3 IS NULL)
              OR cursor_installation_id = ?3)
            AND deferred_installation_ids_json = X'5b5d'",
        params![pass_started_at, pass_started_at, expected_cursor],
    )?;
    if changed != 1 {
        return Err(AppBehaviorSchedulerError::LeaseLost);
    }
    Ok(())
}

#[cfg(test)]
mod reconciliation_readiness_tests {
    use super::*;

    #[test]
    fn app_runtime_concurrency_regression_lifecycle_changes_do_not_wait_for_periodic_scan() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_behavior_reconcile_state(singleton INTEGER PRIMARY KEY, reconciled_at TEXT, pass_started_at TEXT, cursor_installation_id TEXT, deferred_installation_ids_json BLOB); CREATE TABLE app_installations(updated_at TEXT);").unwrap();
        let t = parse_timestamp("2026-09-09T10:00:00.000000Z").unwrap();
        let first = begin_reconciliation_window_blocking(&mut db, t)
            .unwrap()
            .unwrap();
        complete_reconciliation_window_blocking(&db, &timestamp(first.pass_started_at), None)
            .unwrap();
        assert!(
            begin_reconciliation_window_blocking(&mut db, t + Duration::seconds(30))
                .unwrap()
                .is_none()
        );
        db.execute(
            "INSERT INTO app_installations VALUES (?1)",
            params![timestamp(t + Duration::seconds(40))],
        )
        .unwrap();
        let next = begin_reconciliation_window_blocking(&mut db, t + Duration::seconds(60))
            .unwrap()
            .unwrap();
        // Another install changes behind an already-running pass. Finishing
        // that pass cannot acknowledge the later change as already examined.
        db.execute(
            "UPDATE app_installations SET updated_at = ?1",
            params![timestamp(t + Duration::seconds(70))],
        )
        .unwrap();
        complete_reconciliation_window_blocking(&db, &timestamp(next.pass_started_at), None)
            .unwrap();
        let followup = begin_reconciliation_window_blocking(&mut db, t + Duration::seconds(90))
            .unwrap()
            .unwrap();
        complete_reconciliation_window_blocking(&db, &timestamp(followup.pass_started_at), None)
            .unwrap();
        assert!(
            begin_reconciliation_window_blocking(&mut db, t + Duration::seconds(120))
                .unwrap()
                .is_none()
        );
        assert!(
            begin_reconciliation_window_blocking(&mut db, t + Duration::seconds(391))
                .unwrap()
                .is_some()
        );
    }
}
