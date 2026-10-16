//! Durable admission and dispatch for app-owned event behaviors.
//!
//! V1 deliberately consumes only the post-persistence canonical execution
//! terminal projection. The transport broadcaster is not an authority source:
//! every accepted fire is keyed by the journal-derived `source_event_ref`,
//! scoped to the installation encoded in the canonical task UI thread, and
//! committed before a projector receipt can complete.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock as StdRwLock, RwLockReadGuard,
};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use thiserror::Error;

use crate::magician_v2::json_traversal::canonical_json_bytes;

use super::authority::AuthenticatedAppScope;
use super::entity_store::{resolve_active_schema, AppEntityStoreError, AppEntityStoreService};
use super::lifecycle::AppInstallationStatus;
use super::manifest::{
    event_behavior_projection_schema, AppManifestBehaviorResources, AppManifestEventBehavior,
    AppManifestTrigger,
};
use super::models::{
    AppActionInvocation, AppContractError, AppDataClassification, AppDataEnvelope, AppDataSource,
    AppDigest, AppFieldPath, AppHandlingLabels, AppInstallationId, AppModelProcessing, AppName,
    AppProtocolVersion, AppReference, AppRevision, AppSourceRef, AppSourceRefKind,
};
use super::package_staging::{AppPackageStager, AppPackageStagingError};
use super::records::{
    app_event_behavior_request_digest, app_event_subscription_digest, AppBehaviorResourceCeiling,
    AppDataHandlingPolicy, AppEventBehaviorGrant, AppEventSubscriptionV1,
    AppEventTerminalOutcomeV1, AppExternalEgress, AppMemoryPromotion, AppPersonalAgentAccess,
};
use super::registry::{AppRegistryError, AppRegistryService};
use super::workflows::{
    AppBackgroundLaunchAuthority, AppCanonicalEventBehaviorSource, AppWorkflowError,
};

const EVENT_KIND_V1: &str = "installation_execution_terminal_v1";
const MAX_EVENT_FANOUT: usize = 32;
const MAX_EVENT_CLAIMS_PER_SCOPE: usize = 32;
const EVENT_LEASE_MIN_SECONDS: u64 = 30;
const EVENT_LEASE_MAX_SECONDS: u64 = 600;
const EVENT_RETRY_MIN_SECONDS: u64 = 5;
const EVENT_RETRY_MAX_SECONDS: u64 = 3_600;
const EVENT_MAX_ATTEMPTS: u32 = 64;
const EVENT_TERMINAL_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;
const EVENT_COMPACTION_LIMIT: i64 = 32;
const EVENT_COMPACTION_PROOF_LIMIT: i64 = (MAX_EVENT_FANOUT + 1) as i64;
const TERMINAL_COMPACTION_FORWARD_PAGES_PER_REVISIT: i64 = 64;

#[derive(Clone, Copy)]
enum TerminalCompactionRail {
    Forward,
    Revisit,
}

struct EventCompactionCursor {
    forward_after: Option<(String, String)>,
    forward_end: Option<(String, String)>,
    revisit_after: Option<(String, String)>,
    revisit_end: Option<(String, String)>,
    forward_pages_since_revisit: i64,
}

struct EventCompactionCandidate {
    installation_id: String,
    event_ref: String,
    event_kind: String,
    projection_digest: String,
    accepted_fanout: i64,
    created_at: String,
    projection_json: Vec<u8>,
}

/// Process-owned, non-serializable admission for canonical app-event ingress.
///
/// The broad authority lookup remains asynchronous and never holds this lock.
/// Only the final synchronous SQLite commit takes a read guard. Closing takes
/// the write side, which both prevents a later commit from entering and drains
/// any commit whose admission linearized before the close returned.
#[derive(Clone, Default)]
pub struct AppEventIngressAdmission {
    open: Arc<AtomicBool>,
    generation: Arc<StdRwLock<Option<Arc<()>>>>,
}

/// Cloneable only inside the host process; pointer identity binds an observer
/// to the exact supervisor generation it saw before asynchronous validation.
#[derive(Clone)]
pub struct AppEventIngressAdmissionEpoch(Arc<()>);

impl std::fmt::Debug for AppEventIngressAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppEventIngressAdmission")
            .field("open", &self.is_open())
            .finish()
    }
}

impl AppEventIngressAdmission {
    pub fn open(&self) {
        let mut generation = self
            .generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *generation = Some(Arc::new(()));
        self.open.store(true, Ordering::Release);
    }

    pub fn close_and_drain(&self) {
        // Refuse new commit entrants before waiting for the read side. The
        // second open check in `enter_commit` closes the check/lock race.
        self.open.store(false, Ordering::Release);
        let mut generation = self
            .generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *generation = None;
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    pub fn current_epoch(&self) -> Option<AppEventIngressAdmissionEpoch> {
        if !self.open.load(Ordering::Acquire) {
            return None;
        }
        let generation = self
            .generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.open.load(Ordering::Acquire) {
            return None;
        }
        generation
            .as_ref()
            .cloned()
            .map(AppEventIngressAdmissionEpoch)
    }

    fn enter_commit(
        &self,
        expected: &AppEventIngressAdmissionEpoch,
    ) -> Option<RwLockReadGuard<'_, Option<Arc<()>>>> {
        if !self.open.load(Ordering::Acquire) {
            return None;
        }
        let guard = self
            .generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.open.load(Ordering::Acquire)
            || !guard
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &expected.0))
        {
            return None;
        }
        Some(guard)
    }
}

#[derive(Debug, Error)]
pub enum AppEventBehaviorError {
    #[error("event-behavior runtime policy is outside supported bounds")]
    InvalidRuntimePolicy,
    #[error("canonical event identity or projection is invalid")]
    InvalidCanonicalEvent,
    #[error("the event-behavior binding is stale or substituted")]
    StaleBinding,
    #[error("the durable event-behavior lease was lost")]
    LeaseLost,
    #[error("durable event-behavior state is corrupt")]
    CorruptState,
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    EntityStore(#[from] AppEntityStoreError),
    #[error(transparent)]
    Package(#[from] AppPackageStagingError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Workflow(#[from] AppWorkflowError),
}

#[derive(Debug, Clone, Copy)]
pub struct AppEventBehaviorRuntimeLimits {
    pub max_claims_per_scope_tick: usize,
    pub lease_seconds: u64,
    pub retry_seconds: u64,
}

impl AppEventBehaviorRuntimeLimits {
    fn validate(self) -> Result<Self, AppEventBehaviorError> {
        if self.max_claims_per_scope_tick == 0
            || self.max_claims_per_scope_tick > MAX_EVENT_CLAIMS_PER_SCOPE
            || !(EVENT_LEASE_MIN_SECONDS..=EVENT_LEASE_MAX_SECONDS).contains(&self.lease_seconds)
            || !(EVENT_RETRY_MIN_SECONDS..=EVENT_RETRY_MAX_SECONDS).contains(&self.retry_seconds)
        {
            return Err(AppEventBehaviorError::InvalidRuntimePolicy);
        }
        Ok(self)
    }
}

impl Default for AppEventBehaviorRuntimeLimits {
    fn default() -> Self {
        Self {
            max_claims_per_scope_tick: MAX_EVENT_CLAIMS_PER_SCOPE,
            lease_seconds: 120,
            retry_seconds: 30,
        }
    }
}

/// The only app-visible execution-terminal fact in item 4 V1.
///
/// `source_event_ref` is the host-stamped journal identity. `execution_ref`
/// is intentionally a bounded opaque identifier rather than an authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppCanonicalExecutionTerminalV1 {
    installation_id: AppInstallationId,
    source_event_ref: String,
    execution_ref: String,
    outcome: AppEventTerminalOutcomeV1,
    recorded_at: DateTime<Utc>,
}

impl AppCanonicalExecutionTerminalV1 {
    pub fn from_host_projection(
        source: AppCanonicalEventBehaviorSource,
        source_event_ref: String,
        execution_ref: String,
        outcome: AppEventTerminalOutcomeV1,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, AppEventBehaviorError> {
        let (installation_id, background_origin) = source.into_parts();
        if background_origin {
            return Err(AppEventBehaviorError::InvalidCanonicalEvent);
        }
        let event = Self {
            installation_id,
            source_event_ref,
            execution_ref,
            outcome,
            recorded_at,
        };
        event.validate()?;
        Ok(event)
    }

    pub fn validate(&self) -> Result<(), AppEventBehaviorError> {
        if self.source_event_ref.is_empty()
            || self.source_event_ref.len() > 512
            || self
                .source_event_ref
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || self.execution_ref.is_empty()
            || self.execution_ref.len() > 256
            || self
                .execution_ref
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || self.outcome == AppEventTerminalOutcomeV1::Cancelled
        {
            return Err(AppEventBehaviorError::InvalidCanonicalEvent);
        }
        Ok(())
    }

    fn projection(&self) -> Value {
        json!({
            "event_ref": self.source_event_ref,
            "execution_ref": self.execution_ref,
            "outcome": terminal_outcome_name(self.outcome),
            "recorded_at": timestamp(self.recorded_at),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEventBehaviorSettlement {
    Accepted,
    Retry,
    Blocked,
}

#[derive(Debug, Clone)]
pub struct AppEventBehaviorDispatch {
    installation_id: AppInstallationId,
    event_behavior_id: AppName,
    invocation: AppActionInvocation<Value>,
    source_policy: AppDataHandlingPolicy,
    grant: AppEventBehaviorGrant,
    launch_ref: AppReference,
    lease: AppEventBehaviorLease,
    rate_claim: ClaimedEventFire,
}

impl AppEventBehaviorDispatch {
    pub fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }

    pub fn event_behavior_id(&self) -> &AppName {
        &self.event_behavior_id
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
struct AppEventBehaviorLease {
    event_ref: String,
    installation_generation: u64,
    fence: u64,
    token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct ClaimedEventFire {
    installation_id: AppInstallationId,
    event_behavior_id: AppName,
    event_ref: String,
    installation_generation: u64,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    grant_revision: AppRevision,
    reviewed_request_digest: AppDigest,
    projection: Value,
    projection_digest: AppDigest,
    fence: u64,
    token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct AppEventBehaviorService {
    registry: AppRegistryService,
    stager: AppPackageStager,
    entity_store: AppEntityStoreService,
    limits: AppEventBehaviorRuntimeLimits,
}

impl AppEventBehaviorService {
    pub fn new(
        registry: AppRegistryService,
        stager: AppPackageStager,
        limits: AppEventBehaviorRuntimeLimits,
    ) -> Result<Self, AppEventBehaviorError> {
        Ok(Self {
            entity_store: AppEntityStoreService::new(registry.clone()),
            registry,
            stager,
            limits: limits.validate()?,
        })
    }

    /// Durably fan out one canonical fact to the exact current declarations.
    /// Exact projector replay is a no-op; a changed projection under the same
    /// source reference is corruption and fails the projector receipt.
    pub async fn accept_canonical_execution_terminal(
        &self,
        authenticated: &AuthenticatedAppScope,
        event: AppCanonicalExecutionTerminalV1,
        ingress_admission: &AppEventIngressAdmission,
        ingress_epoch: AppEventIngressAdmissionEpoch,
        now: DateTime<Utc>,
    ) -> Result<usize, AppEventBehaviorError> {
        event.validate()?;
        let projection = event.projection();
        let projection_bytes = canonical_json_bytes(&projection)?;
        if projection_bytes.len() > 65_536 {
            return Err(AppEventBehaviorError::InvalidCanonicalEvent);
        }
        let projection_digest = AppDigest::blake3(&projection_bytes);
        let installation_id = event.installation_id.clone();
        if let Some(existing) = self
            .existing_acceptance(
                authenticated,
                &installation_id,
                &event.source_event_ref,
                &projection_bytes,
                &projection_digest,
                now,
            )
            .await?
        {
            return Ok(existing);
        }
        let binding_set = self
            .matching_live_bindings(authenticated, &installation_id, event.outcome, now)
            .await?;
        if binding_set.bindings.len() > MAX_EVENT_FANOUT {
            return Err(AppEventBehaviorError::CorruptState);
        }
        let expected_fanout = binding_set.bindings.len();
        let event_ref = event.source_event_ref;
        let receipt_installation_id = installation_id;
        let now_text = timestamp(now);
        let ingress_admission = ingress_admission.clone();
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, scope| {
                let Some(_ingress_admission) = ingress_admission.enter_commit(&ingress_epoch)
                else {
                    // The process owner closed while authority was being
                    // reopened. No receipt or fire may cross that close.
                    return Ok(0);
                };
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let prior_receipt = transaction
                    .query_row(
                        "SELECT event_kind, projection_json, projection_digest, accepted_fanout
                           FROM app_event_ingress_receipts
                          WHERE installation_id = ?1 AND event_ref = ?2",
                        params![receipt_installation_id.as_str(), event_ref],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Vec<u8>>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, i64>(3)?,
                            ))
                        },
                    )
                    .optional()?;
                if let Some(receipt) = prior_receipt {
                    let accepted = usize::try_from(receipt.3)
                        .map_err(|_| AppEventBehaviorError::CorruptState)?;
                    if receipt.0 != EVENT_KIND_V1
                        || receipt.1 != projection_bytes
                        || receipt.2 != projection_digest.as_str()
                        || accepted > MAX_EVENT_FANOUT
                    {
                        return Err(AppEventBehaviorError::CorruptState);
                    }
                    let fire_count: i64 = transaction.query_row(
                        "SELECT COUNT(*) FROM app_event_behavior_fires
                          WHERE installation_id = ?1 AND event_ref = ?2",
                        params![receipt_installation_id.as_str(), event_ref],
                        |row| row.get(0),
                    )?;
                    if usize::try_from(fire_count).ok() != Some(accepted) {
                        return Err(AppEventBehaviorError::CorruptState);
                    }
                    transaction.commit()?;
                    return Ok(accepted);
                }
                if let Some(accepted) = event_tombstone_acceptance(
                    &transaction,
                    &receipt_installation_id,
                    &event_ref,
                    &projection_digest,
                )? {
                    transaction.commit()?;
                    return Ok(accepted);
                }
                let lifecycle_status = transaction
                    .query_row(
                        "SELECT lifecycle_status FROM app_installations
                          WHERE installation_id = ?1",
                        params![receipt_installation_id.as_str()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if lifecycle_status.as_deref() != Some("enabled") {
                    // A purge/disable racing canonical replay is acknowledged
                    // without recreating projection bytes after the owner has
                    // removed or disabled the destination installation.
                    transaction.commit()?;
                    return Ok(0);
                }
                let LiveEventBindingSet {
                    authority: Some(authority),
                    bindings,
                } = binding_set
                else {
                    // The package scan observed no eligible installation, but
                    // this transaction observes it enabled. Retry from the
                    // projector rather than sealing an immutable zero-fanout
                    // receipt across an enable/reinstall boundary.
                    return Err(AppEventBehaviorError::StaleBinding);
                };
                let active = resolve_active_schema(&transaction, scope, &receipt_installation_id)?
                    .ok_or(AppEventBehaviorError::StaleBinding)?;
                if !authority.matches_active(&active) {
                    // This proof is required even when `bindings` is empty. A
                    // newly added matching grant must not be hidden forever by
                    // a zero-fanout receipt computed outside the transaction.
                    return Err(AppEventBehaviorError::StaleBinding);
                }
                transaction.execute(
                    "INSERT INTO app_event_ingress_receipts (
                        installation_id, event_ref, event_kind, projection_json,
                        projection_digest, accepted_fanout, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        receipt_installation_id.as_str(),
                        event_ref,
                        EVENT_KIND_V1,
                        projection_bytes,
                        projection_digest.as_str(),
                        i64::try_from(expected_fanout)
                            .map_err(|_| AppEventBehaviorError::CorruptState)?,
                        now_text,
                    ],
                )?;
                let mut accepted = 0usize;
                for binding in bindings {
                    if !binding.matches_active(&active) {
                        return Err(AppEventBehaviorError::StaleBinding);
                    }
                    let executable = binding.execution_ready;
                    let inserted = transaction.execute(
                        "INSERT OR IGNORE INTO app_event_behavior_fires (
                            installation_id, event_behavior_id, event_ref,
                            installation_generation, package_revision_ref,
                            schema_revision, grant_revision, reviewed_request_digest,
                            event_kind, projection_json, projection_digest,
                            causation_depth, causation_path_digest, state,
                            revision, fence, attempt_count, rate_admitted_at,
                            available_at, lease_owner, lease_token, lease_expires_at,
                            launch_ref, last_error, created_at, updated_at
                         ) VALUES (
                            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                            ?9, ?10, ?11, 0, ?12, ?13,
                            1, 0, 0, NULL, ?14, NULL, NULL, NULL,
                            NULL, ?15, ?14, ?14
                         )",
                        params![
                            binding.installation_id.as_str(),
                            binding.declaration.id.as_str(),
                            event_ref,
                            i64::try_from(binding.installation_generation)
                                .map_err(|_| AppEventBehaviorError::CorruptState)?,
                            binding.package_revision_ref.as_str(),
                            i64::try_from(binding.schema_revision.get())
                                .map_err(|_| AppEventBehaviorError::CorruptState)?,
                            i64::try_from(binding.grant_revision.get())
                                .map_err(|_| AppEventBehaviorError::CorruptState)?,
                            binding.grant.reviewed_request_digest.as_str(),
                            EVENT_KIND_V1,
                            projection_bytes,
                            projection_digest.as_str(),
                            AppDigest::blake3(format!("canonical-event\0{event_ref}").as_bytes())
                                .as_str(),
                            if executable { "pending" } else { "dead_letter" },
                            now_text,
                            if executable {
                                None::<&str>
                            } else {
                                Some("unsupported_execution_profile")
                            },
                        ],
                    )?;
                    if inserted == 1 {
                        accepted = accepted.saturating_add(1);
                        continue;
                    }
                    let existing = transaction
                        .query_row(
                            "SELECT installation_generation, package_revision_ref,
                                    schema_revision, grant_revision, reviewed_request_digest,
                                    event_kind, projection_json, projection_digest
                               FROM app_event_behavior_fires
                              WHERE installation_id = ?1
                                AND event_behavior_id = ?2 AND event_ref = ?3",
                            params![
                                binding.installation_id.as_str(),
                                binding.declaration.id.as_str(),
                                event_ref,
                            ],
                            |row| {
                                Ok((
                                    row.get::<_, i64>(0)?,
                                    row.get::<_, String>(1)?,
                                    row.get::<_, i64>(2)?,
                                    row.get::<_, i64>(3)?,
                                    row.get::<_, String>(4)?,
                                    row.get::<_, String>(5)?,
                                    row.get::<_, Vec<u8>>(6)?,
                                    row.get::<_, String>(7)?,
                                ))
                            },
                        )
                        .optional()?
                        .ok_or(AppEventBehaviorError::CorruptState)?;
                    let expected_generation = i64::try_from(binding.installation_generation)
                        .map_err(|_| AppEventBehaviorError::CorruptState)?;
                    let expected_schema = i64::try_from(binding.schema_revision.get())
                        .map_err(|_| AppEventBehaviorError::CorruptState)?;
                    let expected_grant = i64::try_from(binding.grant_revision.get())
                        .map_err(|_| AppEventBehaviorError::CorruptState)?;
                    if existing.0 != expected_generation
                        || existing.1 != binding.package_revision_ref.as_str()
                        || existing.2 != expected_schema
                        || existing.3 != expected_grant
                        || existing.4 != binding.grant.reviewed_request_digest.as_str()
                        || existing.5 != EVENT_KIND_V1
                        || existing.6 != projection_bytes
                        || existing.7 != projection_digest.as_str()
                    {
                        return Err(AppEventBehaviorError::CorruptState);
                    }
                    accepted = accepted.saturating_add(1);
                }
                if accepted != expected_fanout {
                    return Err(AppEventBehaviorError::CorruptState);
                }
                transaction.commit()?;
                Ok(accepted)
            })
            .await
    }

    /// A projector retry may arrive after disable/update/revocation. Once the
    /// whole fanout transaction exists, the immutable ingress receipt is the
    /// authority; current declarations must not reinterpret a zero-fanout or
    /// nonzero-fanout result after a crash and projector replay.
    async fn existing_acceptance(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        event_ref: &str,
        projection_bytes: &[u8],
        projection_digest: &AppDigest,
        now: DateTime<Utc>,
    ) -> Result<Option<usize>, AppEventBehaviorError> {
        let installation_id = installation_id.clone();
        let event_ref = event_ref.to_owned();
        let projection_bytes = projection_bytes.to_vec();
        let projection_digest = projection_digest.clone();
        self.registry
            .execute_scoped_typed_read(authenticated, &now, move |connection, _| {
                let receipt = connection
                    .query_row(
                        "SELECT event_kind, projection_json, projection_digest, accepted_fanout
                           FROM app_event_ingress_receipts
                          WHERE installation_id = ?1 AND event_ref = ?2",
                        params![installation_id.as_str(), event_ref],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Vec<u8>>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, i64>(3)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some(receipt) = receipt else {
                    return event_tombstone_acceptance(
                        connection,
                        &installation_id,
                        &event_ref,
                        &projection_digest,
                    );
                };
                let accepted =
                    usize::try_from(receipt.3).map_err(|_| AppEventBehaviorError::CorruptState)?;
                if receipt.0 != EVENT_KIND_V1
                    || receipt.1 != projection_bytes
                    || receipt.2 != projection_digest.as_str()
                    || accepted > MAX_EVENT_FANOUT
                {
                    return Err(AppEventBehaviorError::CorruptState);
                }
                let fire_count: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM app_event_behavior_fires
                      WHERE installation_id = ?1 AND event_ref = ?2",
                    params![installation_id.as_str(), event_ref],
                    |row| row.get(0),
                )?;
                if usize::try_from(fire_count).ok() != Some(accepted) {
                    return Err(AppEventBehaviorError::CorruptState);
                }
                Ok(Some(accepted))
            })
            .await
            .map(Option::flatten)
    }

    /// Replace a bounded page of coherent terminal fanouts with immutable
    /// digest-only replay authority. Receipt and fire payload bytes are removed
    /// in the same transaction which creates their tombstone.
    pub async fn compact_terminal(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<usize, AppEventBehaviorError> {
        let cutoff = now
            .checked_sub_signed(Duration::seconds(EVENT_TERMINAL_RETENTION_SECONDS))
            .ok_or(AppEventBehaviorError::CorruptState)?;
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                if event_compaction_is_idle_and_empty(connection)? {
                    return Ok(0);
                }
                let mut transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if event_compaction_raw_table_is_empty(&transaction)? {
                    reset_all_event_compaction_rails(&transaction, now)?;
                    transaction.commit()?;
                    return Ok(0);
                }
                let (rail, mut cursor, candidates) = load_event_compaction_page(&transaction, now)?;
                if candidates.is_empty() {
                    reset_event_compaction_rail(&mut cursor, rail);
                    store_event_compaction_cursor(&transaction, &cursor, now)?;
                    transaction.commit()?;
                    return Ok(0);
                }
                let mut compacted = 0usize;
                for candidate in &candidates {
                    let installation_id = &candidate.installation_id;
                    let event_ref = &candidate.event_ref;
                    let event_kind = &candidate.event_kind;
                    let digest = &candidate.projection_digest;
                    let fanout = candidate.accepted_fanout;
                    let created_at = &candidate.created_at;
                    let projection_json = &candidate.projection_json;
                    if event_compaction_candidate_is_quarantined(
                        &transaction,
                        installation_id,
                        event_ref,
                    )? {
                        continue;
                    }
                    let created_at_value = match parse_timestamp(created_at) {
                        Ok(value) => value,
                        Err(_) => {
                            quarantine_event_compaction_candidate(
                                &transaction,
                                installation_id,
                                event_ref,
                                "integrity_verification_failed",
                                now,
                            )?;
                            continue;
                        },
                    };
                    if created_at_value > now {
                        quarantine_event_compaction_candidate(
                            &transaction,
                            installation_id,
                            event_ref,
                            "integrity_verification_failed",
                            now,
                        )?;
                        continue;
                    }
                    let fire_proof: (i64, i64, i64) = transaction.query_row(
                        "SELECT COUNT(*),
                                COALESCE(SUM(CASE
                                    WHEN event_kind != ?3 OR projection_digest != ?4
                                      OR projection_json != ?5 THEN 1 ELSE 0 END), 0)
                               ,COALESCE(SUM(CASE
                                    WHEN state NOT IN ('accepted', 'dead_letter')
                                    THEN 1 ELSE 0 END), 0)
                           FROM (
                               SELECT state, event_kind, projection_digest, projection_json
                                 FROM app_event_behavior_fires
                                WHERE installation_id = ?1 AND event_ref = ?2
                                LIMIT ?6
                           ) bounded_fire_proof",
                        params![
                            installation_id,
                            event_ref,
                            event_kind,
                            digest,
                            projection_json,
                            EVENT_COMPACTION_PROOF_LIMIT,
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )?;
                    let validation = (|| {
                        let accepted = usize::try_from(fanout).ok()?;
                        let digest = AppDigest::parse(digest.clone()).ok()?;
                        let parsed_installation_id =
                            AppInstallationId::parse(installation_id.clone()).ok()?;
                        (accepted <= MAX_EVENT_FANOUT
                            && event_kind == EVENT_KIND_V1
                            && AppDigest::blake3(projection_json) == digest
                            && usize::try_from(fire_proof.0).ok() == Some(accepted)
                            && fire_proof.1 == 0)
                            .then_some((accepted, digest, parsed_installation_id))
                    })();
                    let Some((accepted, digest, parsed_installation_id)) = validation else {
                        quarantine_event_compaction_candidate(
                            &transaction,
                            &installation_id,
                            &event_ref,
                            "integrity_verification_failed",
                            now,
                        )?;
                        continue;
                    };
                    if created_at_value > cutoff {
                        continue;
                    }
                    if fire_proof.2 != 0 {
                        continue;
                    }
                    let mut savepoint = transaction.savepoint()?;
                    let row_result = (|| -> Result<(), AppEventBehaviorError> {
                        savepoint.execute(
                            "INSERT OR IGNORE INTO app_event_ingress_tombstones (
                                installation_id, event_ref, event_kind, projection_digest,
                                accepted_fanout, receipt_created_at, compacted_at
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                installation_id,
                                event_ref,
                                event_kind,
                                digest.as_str(),
                                fanout,
                                created_at,
                                timestamp(now),
                            ],
                        )?;
                        if !event_compaction_tombstone_matches(
                            &savepoint,
                            &parsed_installation_id,
                            event_ref,
                            &digest,
                            accepted,
                            created_at,
                            now,
                        )? {
                            return Err(AppEventBehaviorError::CorruptState);
                        }
                        let deleted_fires = savepoint.execute(
                            "DELETE FROM app_event_behavior_fires
                              WHERE installation_id = ?1 AND event_ref = ?2
                                AND state IN ('accepted', 'dead_letter')
                                AND event_kind = ?3 AND projection_digest = ?4
                                AND projection_json = ?5",
                            params![
                                installation_id,
                                event_ref,
                                event_kind,
                                digest.as_str(),
                                projection_json,
                            ],
                        )?;
                        if deleted_fires != accepted {
                            return Err(AppEventBehaviorError::CorruptState);
                        }
                        let deleted_receipt = savepoint.execute(
                            "DELETE FROM app_event_ingress_receipts
                              WHERE installation_id = ?1 AND event_ref = ?2
                                AND event_kind = ?3 AND projection_digest = ?4
                                AND projection_json = ?5
                                AND accepted_fanout = ?6 AND created_at = ?7",
                            params![
                                installation_id,
                                event_ref,
                                event_kind,
                                digest.as_str(),
                                projection_json,
                                fanout,
                                created_at,
                            ],
                        )?;
                        if deleted_receipt != 1 {
                            return Err(AppEventBehaviorError::CorruptState);
                        }
                        Ok(())
                    })();
                    match row_result {
                        Ok(()) => {
                            savepoint.commit()?;
                            compacted = compacted.saturating_add(1);
                        },
                        Err(AppEventBehaviorError::Sqlite(error))
                            if is_compaction_integrity_sqlite_error(&error) =>
                        {
                            savepoint.rollback()?;
                            savepoint.commit()?;
                            quarantine_event_compaction_candidate(
                                &transaction,
                                &installation_id,
                                &event_ref,
                                "compaction_state_conflict",
                                now,
                            )?;
                        },
                        Err(AppEventBehaviorError::Sqlite(error)) => {
                            savepoint.rollback()?;
                            savepoint.commit()?;
                            return Err(AppEventBehaviorError::Sqlite(error));
                        },
                        Err(_) => {
                            savepoint.rollback()?;
                            savepoint.commit()?;
                            quarantine_event_compaction_candidate(
                                &transaction,
                                &installation_id,
                                &event_ref,
                                "compaction_state_conflict",
                                now,
                            )?;
                        },
                    }
                }
                advance_event_compaction_cursor(&mut cursor, rail, &candidates)?;
                store_event_compaction_cursor(&transaction, &cursor, now)?;
                transaction.commit()?;
                Ok(compacted)
            })
            .await
    }

    /// Claim and fully re-authorize a bounded batch. Rate admission is stored
    /// on the fire row so lease expiry/replay never charges the period twice.
    pub async fn claim_due(
        &self,
        authenticated: &AuthenticatedAppScope,
        worker_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppEventBehaviorDispatch>, AppEventBehaviorError> {
        let claims = self.claim_rows(authenticated, worker_ref, now).await?;
        let mut dispatches = Vec::with_capacity(claims.len());
        for claim in claims {
            let boundary_now = Utc::now();
            match self
                .authorize_claim(authenticated, &claim, boundary_now)
                .await
            {
                Ok(Some(dispatch)) => dispatches.push(dispatch),
                Ok(None) => {},
                Err(AppEventBehaviorError::StaleBinding | AppEventBehaviorError::LeaseLost) => {},
                Err(
                    AppEventBehaviorError::CorruptState
                    | AppEventBehaviorError::InvalidCanonicalEvent
                    | AppEventBehaviorError::Contract(_)
                    | AppEventBehaviorError::Json(_),
                ) => {
                    let _ = self
                        .retire_claim(
                            &claim,
                            authenticated,
                            "corrupt_authorization_state",
                            Utc::now(),
                        )
                        .await;
                },
                Err(_) => {
                    let _ = self
                        .release_claim(&claim, authenticated, "authorization_retry", Utc::now())
                        .await;
                },
            }
        }
        Ok(dispatches)
    }

    /// Reopen the lease immediately before the workflow launch and mint a
    /// move-only event-behavior authority bound to the exact reviewed grant.
    pub async fn authorize_dispatch(
        &self,
        authenticated: &AuthenticatedAppScope,
        dispatch: &AppEventBehaviorDispatch,
        now: DateTime<Utc>,
    ) -> Result<Option<AppBackgroundLaunchAuthority>, AppEventBehaviorError> {
        let expected_projection =
            AppDigest::blake3_canonical_json(&dispatch.invocation.input.value)?;
        if expected_projection != dispatch.rate_claim.projection_digest {
            return Err(AppEventBehaviorError::CorruptState);
        }
        if !self
            .admit_rate_and_seal_launch(
                authenticated,
                &dispatch.rate_claim,
                &dispatch.grant.resources,
                dispatch.grant.min_interval_seconds,
                &dispatch.launch_ref,
                now,
            )
            .await?
        {
            return Ok(None);
        }
        Ok(Some(
            AppBackgroundLaunchAuthority::from_server_event_behavior(
                authenticated,
                dispatch.installation_id.clone(),
                dispatch.launch_ref.clone(),
                &dispatch.invocation.input,
                dispatch.grant.clone(),
                dispatch.source_policy.clone(),
                dispatch.lease.expires_at,
                now,
            )?,
        ))
    }

    pub async fn settle(
        &self,
        authenticated: &AuthenticatedAppScope,
        dispatch: &AppEventBehaviorDispatch,
        settlement: AppEventBehaviorSettlement,
        _error: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), AppEventBehaviorError> {
        let installation_id = dispatch.installation_id.clone();
        let behavior_id = dispatch.event_behavior_id.clone();
        let lease = dispatch.lease.clone();
        let launch_ref = dispatch.launch_ref.clone();
        let retry_at = now
            .checked_add_signed(Duration::seconds(
                i64::try_from(self.limits.retry_seconds)
                    .map_err(|_| AppEventBehaviorError::CorruptState)?,
            ))
            .ok_or(AppEventBehaviorError::CorruptState)?;
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let (state, available_at, launch, last_error) = match settlement {
                    AppEventBehaviorSettlement::Accepted => {
                        ("accepted", timestamp(now), Some(launch_ref.as_str()), None)
                    },
                    AppEventBehaviorSettlement::Retry => (
                        "pending",
                        timestamp(retry_at),
                        None,
                        Some("workflow_launch_retry"),
                    ),
                    AppEventBehaviorSettlement::Blocked => (
                        "dead_letter",
                        timestamp(now),
                        None,
                        Some("workflow_launch_blocked"),
                    ),
                };
                let changed = connection.execute(
                    "UPDATE app_event_behavior_fires
                        SET state = ?1, revision = revision + 1, available_at = ?2,
                            lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL,
                            launch_ref = ?3, last_error = ?4, updated_at = ?5
                      WHERE installation_id = ?6 AND event_behavior_id = ?7
                        AND event_ref = ?8 AND installation_generation = ?9
                        AND state = 'leased' AND fence = ?10 AND lease_token = ?11
                        AND lease_expires_at = ?12 AND lease_expires_at > ?5",
                    params![
                        state,
                        available_at,
                        launch,
                        last_error,
                        timestamp(now),
                        installation_id.as_str(),
                        behavior_id.as_str(),
                        lease.event_ref,
                        i64::try_from(lease.installation_generation)
                            .map_err(|_| AppEventBehaviorError::CorruptState)?,
                        i64::try_from(lease.fence)
                            .map_err(|_| AppEventBehaviorError::CorruptState)?,
                        lease.token,
                        timestamp(lease.expires_at),
                    ],
                )?;
                if changed != 1 {
                    return Err(AppEventBehaviorError::LeaseLost);
                }
                Ok(())
            })
            .await
    }

    async fn claim_rows(
        &self,
        authenticated: &AuthenticatedAppScope,
        worker_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<Vec<ClaimedEventFire>, AppEventBehaviorError> {
        let worker = worker_ref.to_string();
        let expires_at = now
            .checked_add_signed(Duration::seconds(
                i64::try_from(self.limits.lease_seconds)
                    .map_err(|_| AppEventBehaviorError::CorruptState)?,
            ))
            .ok_or(AppEventBehaviorError::CorruptState)?;
        let limit = i64::try_from(self.limits.max_claims_per_scope_tick)
            .map_err(|_| AppEventBehaviorError::CorruptState)?;
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                transaction.execute(
                    "WITH pending_terminal AS (
                         SELECT installation_id, event_behavior_id, event_ref,
                                attempt_count, fence, available_at AS due_at
                           FROM app_event_behavior_fires
                          WHERE state = 'pending' AND attempt_count >= ?2
                            AND available_at <= ?1
                          ORDER BY attempt_count, fence, available_at,
                                   installation_id, event_behavior_id, event_ref
                          LIMIT ?3
                     ), expired_terminal AS (
                         SELECT installation_id, event_behavior_id, event_ref,
                                attempt_count, fence, lease_expires_at AS due_at
                           FROM app_event_behavior_fires
                          WHERE state = 'leased' AND attempt_count >= ?2
                            AND lease_expires_at <= ?1
                          ORDER BY attempt_count, fence, lease_expires_at,
                                   installation_id, event_behavior_id, event_ref
                          LIMIT ?3
                     ), bounded_terminal AS (
                         SELECT * FROM pending_terminal
                         UNION ALL
                         SELECT * FROM expired_terminal
                     )
                     UPDATE app_event_behavior_fires
                        SET state = 'dead_letter', revision = revision + 1,
                            lease_owner = NULL, lease_token = NULL,
                            lease_expires_at = NULL,
                            last_error = 'attempt_limit_exhausted', updated_at = ?1
                      WHERE (installation_id, event_behavior_id, event_ref) IN (
                                SELECT installation_id, event_behavior_id, event_ref
                                  FROM bounded_terminal
                                 ORDER BY attempt_count, fence, due_at,
                                          installation_id, event_behavior_id, event_ref
                                 LIMIT ?3
                            )
                        AND attempt_count >= ?2
                        AND ((state = 'pending' AND available_at <= ?1)
                          OR (state = 'leased' AND lease_expires_at <= ?1))
                        AND COALESCE((SELECT paused FROM app_behavior_scope_policy
                                     WHERE singleton = 1), 0) = 0",
                    params![
                        timestamp(now),
                        i64::from(EVENT_MAX_ATTEMPTS),
                        limit,
                    ],
                )?;
                let keys = {
                    let mut statement = transaction.prepare(
                        "WITH pending_due AS (
                             SELECT installation_id, event_behavior_id, event_ref,
                                    attempt_count, fence, available_at AS due_at
                               FROM app_event_behavior_fires
                              WHERE state = 'pending' AND attempt_count < ?2
                                AND available_at <= ?1
                              ORDER BY attempt_count, fence, available_at,
                                       installation_id, event_behavior_id, event_ref
                              LIMIT ?3
                         ), expired_due AS (
                             SELECT installation_id, event_behavior_id, event_ref,
                                    attempt_count, fence, lease_expires_at AS due_at
                               FROM app_event_behavior_fires
                              WHERE state = 'leased' AND attempt_count < ?2
                                AND lease_expires_at <= ?1
                              ORDER BY attempt_count, fence, lease_expires_at,
                                       installation_id, event_behavior_id, event_ref
                              LIMIT ?3
                         ), bounded_due AS (
                             SELECT * FROM pending_due
                             UNION ALL
                             SELECT * FROM expired_due
                         )
                         SELECT installation_id, event_behavior_id, event_ref
                           FROM bounded_due
                          WHERE COALESCE((SELECT paused FROM app_behavior_scope_policy
                                         WHERE singleton = 1), 0) = 0
                          ORDER BY attempt_count, fence, due_at,
                                   installation_id, event_behavior_id, event_ref
                          LIMIT ?3",
                    )?;
                    let rows = statement
                        .query_map(
                            params![timestamp(now), i64::from(EVENT_MAX_ATTEMPTS), limit],
                            |row| {
                                Ok((
                                    row.get::<_, String>(0)?,
                                    row.get::<_, String>(1)?,
                                    row.get::<_, String>(2)?,
                                ))
                            },
                        )?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };
                let mut claims = Vec::with_capacity(keys.len());
                for (installation_id, behavior_id, event_ref) in keys {
                    let token = AppDigest::blake3(
                        format!(
                            "event-lease\0{worker}\0{installation_id}\0{behavior_id}\0{event_ref}\0{}",
                            timestamp(now)
                        )
                        .as_bytes(),
                    );
                    let changed = transaction.execute(
                        "UPDATE app_event_behavior_fires
                            SET state = 'leased', revision = revision + 1,
                                fence = fence + 1,
                                lease_owner = ?1, lease_token = ?2, lease_expires_at = ?3,
                                updated_at = ?4
                          WHERE installation_id = ?5 AND event_behavior_id = ?6
                            AND event_ref = ?7 AND attempt_count < ?8
                            AND ((state = 'pending' AND available_at <= ?4)
                              OR (state = 'leased' AND lease_expires_at <= ?4))
                            AND COALESCE((SELECT paused FROM app_behavior_scope_policy
                                         WHERE singleton = 1), 0) = 0",
                        params![
                            worker,
                            token.as_str(),
                            timestamp(expires_at),
                            timestamp(now),
                            installation_id,
                            behavior_id,
                            event_ref,
                            i64::from(EVENT_MAX_ATTEMPTS),
                        ],
                    )?;
                    if changed != 1 {
                        continue;
                    }
                    let claim = transaction.query_row(
                        "SELECT installation_generation, package_revision_ref,
                                schema_revision, grant_revision, reviewed_request_digest,
                                projection_json, projection_digest, fence
                           FROM app_event_behavior_fires
                          WHERE installation_id = ?1 AND event_behavior_id = ?2
                            AND event_ref = ?3",
                        params![installation_id, behavior_id, event_ref],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, String>(4)?,
                                row.get::<_, Vec<u8>>(5)?,
                                row.get::<_, String>(6)?,
                                row.get::<_, i64>(7)?,
                            ))
                        },
                    )?;
                    let parsed = (|| -> Result<ClaimedEventFire, AppEventBehaviorError> {
                        Ok(ClaimedEventFire {
                            installation_id: AppInstallationId::parse(installation_id.clone())?,
                            event_behavior_id: AppName::parse(behavior_id.clone())?,
                            event_ref: event_ref.clone(),
                            installation_generation: u64::try_from(claim.0)
                                .map_err(|_| AppEventBehaviorError::CorruptState)?,
                            package_revision_ref: AppReference::parse(claim.1.clone())?,
                            schema_revision: AppRevision::new(
                                u64::try_from(claim.2)
                                    .map_err(|_| AppEventBehaviorError::CorruptState)?,
                            )?,
                            grant_revision: AppRevision::new(
                                u64::try_from(claim.3)
                                    .map_err(|_| AppEventBehaviorError::CorruptState)?,
                            )?,
                            reviewed_request_digest: AppDigest::parse(claim.4.clone())?,
                            projection: serde_json::from_slice(&claim.5)?,
                            projection_digest: AppDigest::parse(claim.6.clone())?,
                            fence: u64::try_from(claim.7)
                                .map_err(|_| AppEventBehaviorError::CorruptState)?,
                            token: token.to_string(),
                            expires_at,
                        })
                    })();
                    match parsed {
                        Ok(claim) => claims.push(claim),
                        Err(_) => {
                            // Isolate a malformed durable row so it cannot
                            // roll back and permanently head-of-line block the
                            // rest of this bounded scope batch.
                            let quarantined = transaction.execute(
                                "UPDATE app_event_behavior_fires
                                    SET state = 'dead_letter', revision = revision + 1,
                                        lease_owner = NULL, lease_token = NULL,
                                        lease_expires_at = NULL,
                                        last_error = 'corrupt_durable_row', updated_at = ?1
                                  WHERE installation_id = ?2 AND event_behavior_id = ?3
                                    AND event_ref = ?4 AND state = 'leased'
                                    AND fence = ?5 AND lease_token = ?6",
                                params![
                                    timestamp(now),
                                    installation_id,
                                    behavior_id,
                                    event_ref,
                                    claim.7,
                                    token.as_str(),
                                ],
                            )?;
                            if quarantined != 1 {
                                return Err(AppEventBehaviorError::LeaseLost);
                            }
                        },
                    }
                }
                transaction.commit()?;
                Ok(claims)
            })
            .await
    }

    async fn authorize_claim(
        &self,
        authenticated: &AuthenticatedAppScope,
        claim: &ClaimedEventFire,
        now: DateTime<Utc>,
    ) -> Result<Option<AppEventBehaviorDispatch>, AppEventBehaviorError> {
        let outcome = projection_outcome(&claim.projection)?;
        let binding = match self
            .matching_live_bindings(authenticated, &claim.installation_id, outcome, now)
            .await?
            .bindings
            .into_iter()
            .find(|binding| binding.declaration.id == claim.event_behavior_id)
        {
            Some(binding) if binding.execution_ready => binding,
            _ => {
                self.retire_claim(claim, authenticated, "stale_binding", Utc::now())
                    .await?;
                return Err(AppEventBehaviorError::StaleBinding);
            },
        };
        if binding.installation_generation != claim.installation_generation
            || binding.package_revision_ref != claim.package_revision_ref
            || binding.schema_revision != claim.schema_revision
            || binding.grant_revision != claim.grant_revision
            || binding.grant.reviewed_request_digest != claim.reviewed_request_digest
            || AppDigest::blake3(&canonical_json_bytes(&claim.projection)?)
                != claim.projection_digest
        {
            self.retire_claim(claim, authenticated, "stale_binding", Utc::now())
                .await?;
            return Err(AppEventBehaviorError::StaleBinding);
        }
        let fire_digest = AppDigest::blake3(
            format!(
                "event-fire\0{}\0{}\0{}",
                claim.installation_id, claim.event_behavior_id, claim.event_ref
            )
            .as_bytes(),
        );
        let launch_ref = AppReference::parse(format!(
            "event-behavior-launch:{}",
            fire_digest.as_str().trim_start_matches("blake3:")
        ))?;
        let source_policy = event_source_policy();
        let source_policy_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&source_policy)?)?;
        let projection_bytes = canonical_json_bytes(&claim.projection)?;
        let source_reference = AppReference::parse(format!(
            "event-receipt:{}",
            AppDigest::blake3(claim.event_ref.as_bytes())
                .as_str()
                .trim_start_matches("blake3:")
        ))?;
        let recorded_at = projection_recorded_at(&claim.projection)?;
        let input = AppDataEnvelope {
            protocol_version: AppProtocolVersion::V1,
            source: AppDataSource::ExternalAdapter,
            scope_binding_ref: authenticated.scope_binding_ref().clone(),
            installation_id: claim.installation_id.clone(),
            package_revision_ref: claim.package_revision_ref.clone(),
            schema_revision: claim.schema_revision,
            grant_revision: claim.grant_revision,
            value_schema_ref: binding.input_schema_ref.clone(),
            value: claim.projection.clone(),
            source_refs: vec![AppSourceRef {
                kind: AppSourceRefKind::ExternalReceipt,
                reference: source_reference,
                revision: Some(AppRevision::new(1)?),
                fields: Vec::<AppFieldPath>::new(),
            }],
            handling_labels: AppHandlingLabels {
                classification: AppDataClassification::Sensitive,
                model_processing: AppModelProcessing::LocalOnly,
                policy_digest: source_policy_digest,
                provenance_digest: AppDigest::blake3(
                    format!("canonical-event\0{}", claim.event_ref).as_bytes(),
                ),
            },
            content_digest: AppDigest::blake3(&projection_bytes),
            produced_at: recorded_at,
            expires_at: None,
        };
        let invocation = AppActionInvocation {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse(format!(
                "event-behavior-fire:{}",
                fire_digest.as_str().trim_start_matches("blake3:")
            ))?,
            action_id: binding.declaration.action.clone(),
            action_revision: AppRevision::new(binding.installation_generation)?,
            input,
            requested_result_schema_ref: binding.result_schema_ref,
            caller_surface_or_execution_ref: AppReference::parse(format!(
                "event-behavior:{}",
                binding.declaration.id.as_str()
            ))?,
        };
        Ok(Some(AppEventBehaviorDispatch {
            installation_id: claim.installation_id.clone(),
            event_behavior_id: claim.event_behavior_id.clone(),
            invocation,
            source_policy,
            grant: binding.grant,
            launch_ref,
            lease: AppEventBehaviorLease {
                event_ref: claim.event_ref.clone(),
                installation_generation: claim.installation_generation,
                fence: claim.fence,
                token: claim.token.clone(),
                expires_at: claim.expires_at,
            },
            rate_claim: claim.clone(),
        }))
    }

    async fn admit_rate_and_seal_launch(
        &self,
        authenticated: &AuthenticatedAppScope,
        claim: &ClaimedEventFire,
        resources: &AppBehaviorResourceCeiling,
        min_interval_seconds: u64,
        launch_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<bool, AppEventBehaviorError> {
        let installation_id = claim.installation_id.clone();
        let behavior_id = claim.event_behavior_id.clone();
        let event_ref = claim.event_ref.clone();
        let digest = claim.reviewed_request_digest.clone();
        let token = claim.token.clone();
        let fence = claim.fence;
        let lease_expires_at = claim.expires_at;
        let launch_ref = launch_ref.clone();
        let mut period_seconds = resources.period_seconds;
        let max_starts = resources.max_starts_per_period;
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let rate_admitted = transaction
                    .query_row(
                        "SELECT rate_admitted_at FROM app_event_behavior_fires
                          WHERE installation_id = ?1 AND event_behavior_id = ?2
                            AND event_ref = ?3 AND state = 'leased' AND fence = ?4
                            AND lease_token = ?5 AND lease_expires_at = ?6
                            AND lease_expires_at > ?7
                            AND COALESCE((SELECT paused FROM app_behavior_scope_policy
                                         WHERE singleton = 1), 0) = 0",
                        params![
                            installation_id.as_str(),
                            behavior_id.as_str(),
                            event_ref,
                            i64::try_from(fence)
                                .map_err(|_| AppEventBehaviorError::CorruptState)?,
                            token,
                            timestamp(lease_expires_at),
                            timestamp(now),
                        ],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .ok_or(AppEventBehaviorError::LeaseLost)?;
                let already_rate_admitted = rate_admitted.is_some();
                let existing = if already_rate_admitted {
                    None
                } else {
                    transaction
                        .query_row(
                            "SELECT reviewed_request_digest, period_seconds,
                                period_started_at, starts, last_started_at
                           FROM app_event_behavior_periods
                          WHERE installation_id = ?1 AND event_behavior_id = ?2",
                            params![installation_id.as_str(), behavior_id.as_str()],
                            |row| {
                                Ok((
                                    row.get::<_, String>(0)?,
                                    row.get::<_, i64>(1)?,
                                    row.get::<_, String>(2)?,
                                    row.get::<_, i64>(3)?,
                                    row.get::<_, Option<String>>(4)?,
                                ))
                            },
                        )
                        .optional()?
                };
                let mut period_started = now;
                let mut starts = 0u32;
                let mut last_started = None;
                if let Some((_row_digest, row_period, row_started, row_starts, row_last)) = existing
                {
                    let parsed_started = parse_timestamp(&row_started)?;
                    let prior_period = u64::try_from(row_period)
                        .map_err(|_| AppEventBehaviorError::CorruptState)?;
                    let prior_period_end = parsed_started
                        .checked_add_signed(Duration::seconds(
                            i64::try_from(prior_period)
                                .map_err(|_| AppEventBehaviorError::CorruptState)?,
                        ))
                        .ok_or(AppEventBehaviorError::CorruptState)?;
                    // `min_interval_seconds` is an independent sliding floor,
                    // not part of the volume period. Preserve the last start
                    // even when the prior volume window has elapsed; otherwise
                    // a start immediately before a period boundary can be
                    // followed by another immediately after it.
                    last_started = row_last.as_deref().map(parse_timestamp).transpose()?;
                    if now < prior_period_end {
                        // Changing a request digest or narrowing/reshaping its
                        // period never resets an unexpired consumption window.
                        // Keep the stricter (longer) current window and apply
                        // the new start ceiling to already consumed starts.
                        period_seconds = period_seconds.max(prior_period);
                        period_started = parsed_started;
                        starts = u32::try_from(row_starts)
                            .map_err(|_| AppEventBehaviorError::CorruptState)?;
                    }
                }
                let interval_ready_at =
                    event_interval_ready_at(last_started, min_interval_seconds, now)?;
                let period_ready_at = period_started
                    .checked_add_signed(Duration::seconds(
                        i64::try_from(period_seconds)
                            .map_err(|_| AppEventBehaviorError::CorruptState)?,
                    ))
                    .ok_or(AppEventBehaviorError::CorruptState)?;
                if !already_rate_admitted && (starts >= max_starts || interval_ready_at > now) {
                    let available_at = if starts >= max_starts {
                        period_ready_at.max(interval_ready_at)
                    } else {
                        interval_ready_at
                    };
                    let changed = transaction.execute(
                        "UPDATE app_event_behavior_fires
                            SET state = 'pending', revision = revision + 1,
                                available_at = ?1, lease_owner = NULL,
                                lease_token = NULL, lease_expires_at = NULL,
                                launch_ref = NULL, last_error = 'rate_limited', updated_at = ?2
                          WHERE installation_id = ?3 AND event_behavior_id = ?4
                            AND event_ref = ?5 AND state = 'leased' AND fence = ?6
                            AND lease_token = ?7 AND lease_expires_at = ?8
                            AND lease_expires_at > ?2
                            AND COALESCE((SELECT paused FROM app_behavior_scope_policy
                                         WHERE singleton = 1), 0) = 0",
                        params![
                            timestamp(available_at),
                            timestamp(now),
                            installation_id.as_str(),
                            behavior_id.as_str(),
                            event_ref,
                            i64::try_from(fence)
                                .map_err(|_| AppEventBehaviorError::CorruptState)?,
                            token,
                            timestamp(lease_expires_at),
                        ],
                    )?;
                    if changed != 1 {
                        return Err(AppEventBehaviorError::LeaseLost);
                    }
                    transaction.commit()?;
                    return Ok(false);
                }
                if !already_rate_admitted {
                    transaction.execute(
                        "INSERT INTO app_event_behavior_periods (
                        installation_id, event_behavior_id, reviewed_request_digest,
                        period_seconds, period_started_at, starts, last_started_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)
                     ON CONFLICT(installation_id, event_behavior_id) DO UPDATE SET
                        reviewed_request_digest = excluded.reviewed_request_digest,
                        period_seconds = excluded.period_seconds,
                        period_started_at = excluded.period_started_at,
                        starts = excluded.starts,
                        last_started_at = excluded.last_started_at,
                        updated_at = excluded.updated_at",
                        params![
                            installation_id.as_str(),
                            behavior_id.as_str(),
                            digest.as_str(),
                            i64::try_from(period_seconds)
                                .map_err(|_| AppEventBehaviorError::CorruptState)?,
                            timestamp(period_started),
                            timestamp(now),
                        ],
                    )?;
                    if starts > 0 {
                        transaction.execute(
                            "UPDATE app_event_behavior_periods
                            SET starts = ?1, last_started_at = ?2, updated_at = ?2
                          WHERE installation_id = ?3 AND event_behavior_id = ?4",
                            params![
                                i64::from(starts.saturating_add(1)),
                                timestamp(now),
                                installation_id.as_str(),
                                behavior_id.as_str(),
                            ],
                        )?;
                    }
                }
                let changed = transaction.execute(
                    "UPDATE app_event_behavior_fires
                        SET revision = revision + 1,
                            rate_admitted_at = COALESCE(rate_admitted_at, ?1),
                            launch_ref = ?2, attempt_count = attempt_count + 1,
                            last_error = NULL, updated_at = ?1
                      WHERE installation_id = ?3 AND event_behavior_id = ?4
                        AND event_ref = ?5 AND state = 'leased' AND fence = ?6
                        AND lease_token = ?7 AND lease_expires_at = ?8
                        AND lease_expires_at > ?1 AND attempt_count < ?9
                        AND (launch_ref IS NULL OR launch_ref = ?2)
                        AND COALESCE((SELECT paused FROM app_behavior_scope_policy
                                     WHERE singleton = 1), 0) = 0",
                    params![
                        timestamp(now),
                        launch_ref.as_str(),
                        installation_id.as_str(),
                        behavior_id.as_str(),
                        event_ref,
                        i64::try_from(fence).map_err(|_| AppEventBehaviorError::CorruptState)?,
                        token,
                        timestamp(lease_expires_at),
                        i64::from(EVENT_MAX_ATTEMPTS),
                    ],
                )?;
                if changed != 1 {
                    return Err(AppEventBehaviorError::LeaseLost);
                }
                transaction.commit()?;
                Ok(true)
            })
            .await
    }

    async fn retire_claim(
        &self,
        claim: &ClaimedEventFire,
        authenticated: &AuthenticatedAppScope,
        reason: &'static str,
        now: DateTime<Utc>,
    ) -> Result<(), AppEventBehaviorError> {
        let installation_id = claim.installation_id.clone();
        let behavior_id = claim.event_behavior_id.clone();
        let event_ref = claim.event_ref.clone();
        let fence = claim.fence;
        let token = claim.token.clone();
        let lease_expires_at = claim.expires_at;
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let changed = connection.execute(
                    "UPDATE app_event_behavior_fires
                        SET state = 'dead_letter', revision = revision + 1,
                            lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL,
                            last_error = ?1, updated_at = ?2
                      WHERE installation_id = ?3 AND event_behavior_id = ?4
                        AND event_ref = ?5 AND state = 'leased' AND fence = ?6
                        AND lease_token = ?7 AND lease_expires_at = ?8
                        AND lease_expires_at > ?2
                        AND COALESCE((SELECT paused FROM app_behavior_scope_policy
                                     WHERE singleton = 1), 0) = 0",
                    params![
                        reason,
                        timestamp(now),
                        installation_id.as_str(),
                        behavior_id.as_str(),
                        event_ref,
                        i64::try_from(fence).map_err(|_| AppEventBehaviorError::CorruptState)?,
                        token,
                        timestamp(lease_expires_at),
                    ],
                )?;
                if changed != 1 {
                    return Err(AppEventBehaviorError::LeaseLost);
                }
                Ok(())
            })
            .await
    }

    async fn release_claim(
        &self,
        claim: &ClaimedEventFire,
        authenticated: &AuthenticatedAppScope,
        reason: &'static str,
        now: DateTime<Utc>,
    ) -> Result<(), AppEventBehaviorError> {
        let installation_id = claim.installation_id.clone();
        let behavior_id = claim.event_behavior_id.clone();
        let event_ref = claim.event_ref.clone();
        let fence = claim.fence;
        let token = claim.token.clone();
        let lease_expires_at = claim.expires_at;
        let retry_at = now
            .checked_add_signed(Duration::seconds(
                i64::try_from(self.limits.retry_seconds)
                    .map_err(|_| AppEventBehaviorError::CorruptState)?,
            ))
            .ok_or(AppEventBehaviorError::CorruptState)?;
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                let changed = connection.execute(
                    "UPDATE app_event_behavior_fires
                        SET state = 'pending', revision = revision + 1,
                            available_at = ?1, lease_owner = NULL, lease_token = NULL,
                            lease_expires_at = NULL, last_error = ?2, updated_at = ?3
                      WHERE installation_id = ?4 AND event_behavior_id = ?5
                        AND event_ref = ?6 AND state = 'leased' AND fence = ?7
                        AND lease_token = ?8 AND lease_expires_at = ?9
                        AND lease_expires_at > ?3
                        AND COALESCE((SELECT paused FROM app_behavior_scope_policy
                                     WHERE singleton = 1), 0) = 0",
                    params![
                        timestamp(retry_at),
                        reason,
                        timestamp(now),
                        installation_id.as_str(),
                        behavior_id.as_str(),
                        event_ref,
                        i64::try_from(fence).map_err(|_| AppEventBehaviorError::CorruptState)?,
                        token,
                        timestamp(lease_expires_at),
                    ],
                )?;
                if changed != 1 {
                    return Err(AppEventBehaviorError::LeaseLost);
                }
                Ok(())
            })
            .await
    }

    async fn matching_live_bindings(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        outcome: AppEventTerminalOutcomeV1,
        now: DateTime<Utc>,
    ) -> Result<LiveEventBindingSet, AppEventBehaviorError> {
        let Some(installation) = self
            .registry
            .installation(authenticated, installation_id, now)
            .await?
        else {
            // A canonical terminal event can race uninstall/retirement. It is
            // still durably acknowledged by the canonical projector, but it
            // has no destination authority and therefore fans out to nothing.
            return Ok(LiveEventBindingSet::ineligible());
        };
        if installation.lifecycle.status != AppInstallationStatus::Enabled {
            return Ok(LiveEventBindingSet::ineligible());
        }
        let active = self
            .entity_store
            .active_schema(authenticated, installation_id, now)
            .await?
            .ok_or(AppEventBehaviorError::StaleBinding)?;
        let package = self
            .registry
            .package_revision(authenticated, &installation.package_revision_ref, now)
            .await?
            .ok_or(AppEventBehaviorError::StaleBinding)?;
        let staged = self
            .stager
            .load_staged_package(authenticated, package.content_digest.clone(), now)
            .await?;
        if staged.storage_digest() != &package.content_digest
            || staged.candidate().bundle_digest() != &package.content_digest
        {
            return Err(AppEventBehaviorError::StaleBinding);
        }
        let manifest = staged.candidate().manifest().manifest();
        let mut bindings = Vec::new();
        for declaration in &manifest.app.event_behaviors {
            let AppEventSubscriptionV1::InstallationExecutionTerminal { outcomes } =
                &declaration.subscription;
            if !outcomes.contains(&outcome) {
                continue;
            }
            let Some(grant) = active
                .grant()
                .granted_event_behavior_grants
                .iter()
                .find(|grant| grant.event_behavior_id == declaration.id)
            else {
                continue;
            };
            let resources = manifest
                .app
                .resources
                .event_behaviors
                .get(&declaration.id)
                .ok_or(AppEventBehaviorError::StaleBinding)?;
            validate_live_event_binding(declaration, resources, grant)?;
            let action = manifest
                .app
                .actions
                .get(&declaration.action)
                .ok_or(AppEventBehaviorError::StaleBinding)?;
            let workflow = manifest
                .app
                .workflows
                .get(&action.workflow)
                .ok_or(AppEventBehaviorError::StaleBinding)?;
            if workflow.trigger != AppManifestTrigger::Event {
                return Err(AppEventBehaviorError::StaleBinding);
            }
            bindings.push(LiveEventBinding {
                installation_id: installation.installation_id.clone(),
                installation_generation: installation.lifecycle.generation,
                package_revision_ref: installation.package_revision_ref.clone(),
                schema_revision: active.schema_revision(),
                grant_revision: active.grant_revision(),
                declaration: declaration.clone(),
                grant: grant.clone(),
                input_schema_ref: action.input_from.clone(),
                result_schema_ref: action.result_from.clone(),
                execution_ready: super::behavior_recipe::behavior_execution_ready(
                    workflow.runner,
                    &declaration.operations,
                    &declaration.steps,
                ),
            });
        }
        Ok(LiveEventBindingSet {
            authority: Some(LiveEventAuthoritySnapshot {
                installation_generation: installation.lifecycle.generation,
                package_revision_ref: installation.package_revision_ref,
                schema_revision: active.schema_revision(),
                grant_revision: active.grant_revision(),
            }),
            bindings,
        })
    }
}

fn load_event_compaction_page(
    transaction: &Transaction<'_>,
    now: DateTime<Utc>,
) -> Result<
    (
        TerminalCompactionRail,
        EventCompactionCursor,
        Vec<EventCompactionCandidate>,
    ),
    AppEventBehaviorError,
> {
    transaction.execute(
        "INSERT OR IGNORE INTO app_terminal_compaction_cursors (
             candidate_kind, forward_pages_since_revisit, updated_at
         ) VALUES ('event_ingress', 0, ?1)",
        params![timestamp(now)],
    )?;
    let mut cursor = transaction.query_row(
        "SELECT forward_after_installation_id, forward_after_candidate_ref,
                forward_epoch_end_installation_id, forward_epoch_end_candidate_ref,
                revisit_after_installation_id, revisit_after_candidate_ref,
                revisit_epoch_end_installation_id, revisit_epoch_end_candidate_ref,
                forward_pages_since_revisit
           FROM app_terminal_compaction_cursors
          WHERE candidate_kind = 'event_ingress'",
        [],
        |row| {
            let pair = |left: Option<String>, right: Option<String>| match (left, right) {
                (Some(left), Some(right)) => Some((left, right)),
                _ => None,
            };
            Ok(EventCompactionCursor {
                forward_after: pair(row.get(0)?, row.get(1)?),
                forward_end: pair(row.get(2)?, row.get(3)?),
                revisit_after: pair(row.get(4)?, row.get(5)?),
                revisit_end: pair(row.get(6)?, row.get(7)?),
                forward_pages_since_revisit: row.get(8)?,
            })
        },
    )?;
    if !(0..=TERMINAL_COMPACTION_FORWARD_PAGES_PER_REVISIT)
        .contains(&cursor.forward_pages_since_revisit)
    {
        return Err(AppEventBehaviorError::CorruptState);
    }
    let rail =
        if cursor.forward_pages_since_revisit >= TERMINAL_COMPACTION_FORWARD_PAGES_PER_REVISIT {
            TerminalCompactionRail::Revisit
        } else {
            TerminalCompactionRail::Forward
        };
    let end = match rail {
        TerminalCompactionRail::Forward => &mut cursor.forward_end,
        TerminalCompactionRail::Revisit => &mut cursor.revisit_end,
    };
    if end.is_none() {
        *end = transaction
            .query_row(
                "SELECT installation_id, event_ref
                   FROM app_event_ingress_receipts
                  ORDER BY installation_id DESC, event_ref DESC
                  LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
    }
    let Some((end_installation_id, end_event_ref)) = end.clone() else {
        return Ok((rail, cursor, Vec::new()));
    };
    let after = match rail {
        TerminalCompactionRail::Forward => cursor.forward_after.as_ref(),
        TerminalCompactionRail::Revisit => cursor.revisit_after.as_ref(),
    };
    let candidates = if let Some((after_installation_id, after_event_ref)) = after {
        let mut statement = transaction.prepare(
            "SELECT installation_id, event_ref, event_kind, projection_digest,
                    accepted_fanout, created_at, projection_json
               FROM app_event_ingress_receipts
              WHERE (installation_id, event_ref) > (?1, ?2)
                AND (installation_id, event_ref) <= (?3, ?4)
              ORDER BY installation_id, event_ref
              LIMIT ?5",
        )?;
        let rows = statement.query_map(
            params![
                after_installation_id,
                after_event_ref,
                end_installation_id,
                end_event_ref,
                EVENT_COMPACTION_LIMIT,
            ],
            |row| {
                Ok(EventCompactionCandidate {
                    installation_id: row.get(0)?,
                    event_ref: row.get(1)?,
                    event_kind: row.get(2)?,
                    projection_digest: row.get(3)?,
                    accepted_fanout: row.get(4)?,
                    created_at: row.get(5)?,
                    projection_json: row.get(6)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        let mut statement = transaction.prepare(
            "SELECT installation_id, event_ref, event_kind, projection_digest,
                    accepted_fanout, created_at, projection_json
               FROM app_event_ingress_receipts
              WHERE (installation_id, event_ref) <= (?1, ?2)
              ORDER BY installation_id, event_ref
              LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![end_installation_id, end_event_ref, EVENT_COMPACTION_LIMIT],
            |row| {
                Ok(EventCompactionCandidate {
                    installation_id: row.get(0)?,
                    event_ref: row.get(1)?,
                    event_kind: row.get(2)?,
                    projection_digest: row.get(3)?,
                    accepted_fanout: row.get(4)?,
                    created_at: row.get(5)?,
                    projection_json: row.get(6)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    Ok((rail, cursor, candidates))
}

fn event_compaction_is_idle_and_empty(
    connection: &rusqlite::Connection,
) -> Result<bool, AppEventBehaviorError> {
    let idle_and_empty: i64 = connection.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM app_event_ingress_receipts LIMIT 1)
                AND NOT EXISTS(
                    SELECT 1 FROM app_terminal_compaction_cursors
                     WHERE candidate_kind = 'event_ingress'
                       AND (forward_after_installation_id IS NOT NULL
                         OR forward_after_candidate_ref IS NOT NULL
                         OR forward_epoch_end_installation_id IS NOT NULL
                         OR forward_epoch_end_candidate_ref IS NOT NULL
                         OR revisit_after_installation_id IS NOT NULL
                         OR revisit_after_candidate_ref IS NOT NULL
                         OR revisit_epoch_end_installation_id IS NOT NULL
                         OR revisit_epoch_end_candidate_ref IS NOT NULL
                         OR forward_pages_since_revisit != 0)
                )",
        [],
        |row| row.get(0),
    )?;
    Ok(idle_and_empty == 1)
}

fn event_compaction_raw_table_is_empty(
    connection: &rusqlite::Connection,
) -> Result<bool, AppEventBehaviorError> {
    let empty: i64 = connection.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM app_event_ingress_receipts LIMIT 1)",
        [],
        |row| row.get(0),
    )?;
    Ok(empty == 1)
}

fn reset_all_event_compaction_rails(
    connection: &rusqlite::Connection,
    now: DateTime<Utc>,
) -> Result<(), AppEventBehaviorError> {
    let changed = connection.execute(
        "UPDATE app_terminal_compaction_cursors
            SET forward_after_installation_id = NULL,
                forward_after_candidate_ref = NULL,
                forward_epoch_end_installation_id = NULL,
                forward_epoch_end_candidate_ref = NULL,
                revisit_after_installation_id = NULL,
                revisit_after_candidate_ref = NULL,
                revisit_epoch_end_installation_id = NULL,
                revisit_epoch_end_candidate_ref = NULL,
                forward_pages_since_revisit = 0, updated_at = ?1
          WHERE candidate_kind = 'event_ingress'
            AND (forward_after_installation_id IS NOT NULL
              OR forward_after_candidate_ref IS NOT NULL
              OR forward_epoch_end_installation_id IS NOT NULL
              OR forward_epoch_end_candidate_ref IS NOT NULL
              OR revisit_after_installation_id IS NOT NULL
              OR revisit_after_candidate_ref IS NOT NULL
              OR revisit_epoch_end_installation_id IS NOT NULL
              OR revisit_epoch_end_candidate_ref IS NOT NULL
              OR forward_pages_since_revisit != 0)",
        params![timestamp(now)],
    )?;
    if changed > 1 {
        return Err(AppEventBehaviorError::CorruptState);
    }
    Ok(())
}

fn reset_event_compaction_rail(cursor: &mut EventCompactionCursor, rail: TerminalCompactionRail) {
    match rail {
        TerminalCompactionRail::Forward => {
            cursor.forward_after = None;
            cursor.forward_end = None;
        },
        TerminalCompactionRail::Revisit => {
            cursor.revisit_after = None;
            cursor.revisit_end = None;
            cursor.forward_pages_since_revisit = 0;
        },
    }
}

fn advance_event_compaction_cursor(
    cursor: &mut EventCompactionCursor,
    rail: TerminalCompactionRail,
    candidates: &[EventCompactionCandidate],
) -> Result<(), AppEventBehaviorError> {
    let last = candidates
        .last()
        .ok_or(AppEventBehaviorError::CorruptState)?;
    let key = (last.installation_id.clone(), last.event_ref.clone());
    match rail {
        TerminalCompactionRail::Forward => {
            cursor.forward_after = Some(key);
            cursor.forward_pages_since_revisit = cursor
                .forward_pages_since_revisit
                .checked_add(1)
                .ok_or(AppEventBehaviorError::CorruptState)?;
        },
        TerminalCompactionRail::Revisit => {
            cursor.revisit_after = Some(key);
            cursor.forward_pages_since_revisit = 0;
        },
    }
    Ok(())
}

fn store_event_compaction_cursor(
    transaction: &Transaction<'_>,
    cursor: &EventCompactionCursor,
    now: DateTime<Utc>,
) -> Result<(), AppEventBehaviorError> {
    // A nested `fn` rather than a closure: the return borrows from the
    // argument, and a closure cannot say that those are the same lifetime
    // without a higher-ranked annotation. A `fn` gets it by elision.
    fn split(value: &Option<(String, String)>) -> (Option<&str>, Option<&str>) {
        value
            .as_ref()
            .map(|(left, right)| (Some(left.as_str()), Some(right.as_str())))
            .unwrap_or((None, None))
    }
    let (forward_after_installation_id, forward_after_candidate_ref) = split(&cursor.forward_after);
    let (forward_end_installation_id, forward_end_candidate_ref) = split(&cursor.forward_end);
    let (revisit_after_installation_id, revisit_after_candidate_ref) = split(&cursor.revisit_after);
    let (revisit_end_installation_id, revisit_end_candidate_ref) = split(&cursor.revisit_end);
    let changed = transaction.execute(
        "UPDATE app_terminal_compaction_cursors
            SET forward_after_installation_id = ?1,
                forward_after_candidate_ref = ?2,
                forward_epoch_end_installation_id = ?3,
                forward_epoch_end_candidate_ref = ?4,
                revisit_after_installation_id = ?5,
                revisit_after_candidate_ref = ?6,
                revisit_epoch_end_installation_id = ?7,
                revisit_epoch_end_candidate_ref = ?8,
                forward_pages_since_revisit = ?9, updated_at = ?10
          WHERE candidate_kind = 'event_ingress'",
        params![
            forward_after_installation_id,
            forward_after_candidate_ref,
            forward_end_installation_id,
            forward_end_candidate_ref,
            revisit_after_installation_id,
            revisit_after_candidate_ref,
            revisit_end_installation_id,
            revisit_end_candidate_ref,
            cursor.forward_pages_since_revisit,
            timestamp(now),
        ],
    )?;
    if changed != 1 {
        return Err(AppEventBehaviorError::CorruptState);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingress_admission_epochs_are_generation_scoped_and_fail_closed() {
        let admission = AppEventIngressAdmission::default();
        assert!(!admission.is_open());
        assert!(admission.current_epoch().is_none());

        admission.open();
        let first = admission.current_epoch().expect("open generation");
        assert!(admission.enter_commit(&first).is_some());

        admission.close_and_drain();
        assert!(!admission.is_open());
        assert!(admission.current_epoch().is_none());
        assert!(
            admission.enter_commit(&first).is_none(),
            "an observer admitted by a drained supervisor generation must not commit"
        );

        admission.open();
        let second = admission.current_epoch().expect("replacement generation");
        assert!(admission.enter_commit(&second).is_some());
        assert!(
            admission.enter_commit(&first).is_none(),
            "reopening must mint a fresh pointer-identity epoch"
        );
    }

    #[test]
    fn minimum_interval_survives_a_volume_period_boundary() {
        let last_started = DateTime::parse_from_rfc3339("2026-01-01T00:00:59Z")
            .unwrap()
            .with_timezone(&Utc);
        let next_period = DateTime::parse_from_rfc3339("2026-01-01T00:01:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let ready = event_interval_ready_at(Some(last_started), 60, next_period).unwrap();
        assert_eq!(
            ready,
            DateTime::parse_from_rfc3339("2026-01-01T00:01:59Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        assert!(ready > next_period);
    }
}

fn event_compaction_candidate_is_quarantined(
    transaction: &Transaction<'_>,
    installation_id: &str,
    event_ref: &str,
) -> Result<bool, AppEventBehaviorError> {
    transaction
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM app_terminal_compaction_quarantine
                  WHERE candidate_kind = 'event_ingress'
                    AND installation_id = ?1 AND candidate_ref = ?2
             )",
            params![installation_id, event_ref],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn event_compaction_tombstone_matches(
    connection: &rusqlite::Connection,
    installation_id: &AppInstallationId,
    event_ref: &str,
    projection_digest: &AppDigest,
    accepted_fanout: usize,
    receipt_created_at: &str,
    now: DateTime<Utc>,
) -> Result<bool, AppEventBehaviorError> {
    let retained = connection
        .query_row(
            "SELECT event_kind, projection_digest, accepted_fanout,
                    receipt_created_at, compacted_at
               FROM app_event_ingress_tombstones
              WHERE installation_id = ?1 AND event_ref = ?2",
            params![installation_id.as_str(), event_ref],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((event_kind, digest, fanout, created_at, compacted_at)) = retained else {
        return Ok(false);
    };
    let retained_fanout = usize::try_from(fanout).ok();
    let created_at_value = parse_timestamp(&created_at).ok();
    let compacted_at_value = parse_timestamp(&compacted_at).ok();
    Ok(event_kind == EVENT_KIND_V1
        && digest == projection_digest.as_str()
        && retained_fanout == Some(accepted_fanout)
        && created_at == receipt_created_at
        && created_at_value.is_some()
        && compacted_at_value
            .zip(created_at_value)
            .is_some_and(|(compacted, created)| compacted >= created && compacted <= now))
}

fn event_tombstone_acceptance(
    connection: &rusqlite::Connection,
    installation_id: &AppInstallationId,
    event_ref: &str,
    projection_digest: &AppDigest,
) -> Result<Option<usize>, AppEventBehaviorError> {
    let tombstone = connection
        .query_row(
            "SELECT event_kind, projection_digest, accepted_fanout
               FROM app_event_ingress_tombstones
              WHERE installation_id = ?1 AND event_ref = ?2",
            params![installation_id.as_str(), event_ref],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((event_kind, retained_digest, retained_fanout)) = tombstone else {
        return Ok(None);
    };
    let accepted =
        usize::try_from(retained_fanout).map_err(|_| AppEventBehaviorError::CorruptState)?;
    if event_kind != EVENT_KIND_V1
        || retained_digest != projection_digest.as_str()
        || accepted > MAX_EVENT_FANOUT
    {
        return Err(AppEventBehaviorError::CorruptState);
    }
    Ok(Some(accepted))
}

fn quarantine_event_compaction_candidate(
    connection: &rusqlite::Connection,
    installation_id: &str,
    event_ref: &str,
    reason_code: &'static str,
    now: DateTime<Utc>,
) -> Result<(), AppEventBehaviorError> {
    connection.execute(
        "INSERT OR IGNORE INTO app_terminal_compaction_quarantine (
             candidate_kind, installation_id, candidate_ref, reason_code, quarantined_at
         ) VALUES ('event_ingress', ?1, ?2, ?3, ?4)",
        params![installation_id, event_ref, reason_code, timestamp(now)],
    )?;
    let retained: Option<String> = connection
        .query_row(
            "SELECT reason_code FROM app_terminal_compaction_quarantine
              WHERE candidate_kind = 'event_ingress'
                AND installation_id = ?1 AND candidate_ref = ?2",
            params![installation_id, event_ref],
            |row| row.get(0),
        )
        .optional()?;
    if retained.is_none() {
        return Err(AppEventBehaviorError::CorruptState);
    }
    Ok(())
}

fn is_compaction_integrity_sqlite_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

struct LiveEventBindingSet {
    authority: Option<LiveEventAuthoritySnapshot>,
    bindings: Vec<LiveEventBinding>,
}

impl LiveEventBindingSet {
    fn ineligible() -> Self {
        Self {
            authority: None,
            bindings: Vec::new(),
        }
    }
}

struct LiveEventAuthoritySnapshot {
    installation_generation: u64,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    grant_revision: AppRevision,
}

impl LiveEventAuthoritySnapshot {
    fn matches_active(&self, active: &super::entity_store::ActiveAppEntitySchema) -> bool {
        active.installation_generation() == self.installation_generation
            && active.package_revision_ref() == &self.package_revision_ref
            && active.schema_revision() == self.schema_revision
            && active.grant_revision() == self.grant_revision
            && active.grant().revoked_at.is_none()
    }
}

#[derive(Debug, Clone)]
struct LiveEventBinding {
    installation_id: AppInstallationId,
    installation_generation: u64,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    grant_revision: AppRevision,
    declaration: AppManifestEventBehavior,
    grant: AppEventBehaviorGrant,
    input_schema_ref: AppReference,
    result_schema_ref: AppReference,
    execution_ready: bool,
}

impl LiveEventBinding {
    fn matches_active(&self, active: &super::entity_store::ActiveAppEntitySchema) -> bool {
        active.installation_generation() == self.installation_generation
            && active.package_revision_ref() == &self.package_revision_ref
            && active.schema_revision() == self.schema_revision
            && active.grant_revision() == self.grant_revision
            && active.grant().revoked_at.is_none()
            && active
                .grant()
                .granted_event_behavior_grants
                .iter()
                .any(|grant| grant == &self.grant)
    }
}

pub(crate) fn validate_live_event_binding(
    declaration: &AppManifestEventBehavior,
    declared_resources: &AppManifestBehaviorResources,
    grant: &AppEventBehaviorGrant,
) -> Result<(), AppEventBehaviorError> {
    let subscription_digest = app_event_subscription_digest(&declaration.subscription)?;
    let projection_schema_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(
        event_behavior_projection_schema(&declaration.subscription),
    )?)?;
    let output_schema_digest = declaration
        .output_schema
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?
        .as_ref()
        .map(AppDigest::blake3_canonical_json)
        .transpose()?;
    let requested_resources = AppBehaviorResourceCeiling {
        max_tokens_per_run: declared_resources.per_run.max_tokens,
        max_cost_microusd_per_run: declared_resources.per_run.max_cost_usd.microusd(),
        max_active_seconds_per_run: declared_resources.per_run.max_active_seconds,
        max_tokens_per_month: declared_resources.monthly.max_tokens,
        max_cost_microusd_per_month: declared_resources.monthly.max_cost_usd.microusd(),
        max_starts_per_period: declared_resources.max_starts_per_period,
        period_seconds: declared_resources.period_seconds,
        max_causation_depth: declared_resources.max_causation_depth,
        max_spend_depth: declared_resources.max_spend_depth,
        max_contribution_proposals_per_run: declared_resources.max_contribution_proposals_per_run,
    };
    let steps_digest =
        super::manifest::app_behavior_steps_digest(&declaration.steps).map_err(|error| {
            AppEventBehaviorError::Contract(AppContractError::invalid(
                "event_behavior_steps",
                error.to_string(),
            ))
        })?;
    let expected_review = app_event_behavior_request_digest(
        &declaration.id,
        &declaration.purpose,
        &declaration.action,
        &subscription_digest,
        &projection_schema_digest,
        &declaration.operations,
        steps_digest.as_ref(),
        output_schema_digest.as_ref(),
        declaration.min_interval_seconds,
        &requested_resources,
    )?;
    if grant.event_behavior_id != declaration.id
        || grant.purpose != declaration.purpose
        || grant.action != declaration.action
        || grant.subscription != declaration.subscription
        || grant.subscription_digest != subscription_digest
        || grant.projection_schema_digest != projection_schema_digest
        || grant.operations != declaration.operations
        || grant.steps_digest != steps_digest
        || grant.output_schema_digest != output_schema_digest
        || grant.min_interval_seconds < declaration.min_interval_seconds
        || !event_resources_narrow(&grant.resources, &requested_resources)
        || grant.reviewed_request_digest != expected_review
    {
        return Err(AppEventBehaviorError::StaleBinding);
    }
    Ok(())
}

fn event_resources_narrow(
    granted: &AppBehaviorResourceCeiling,
    requested: &AppBehaviorResourceCeiling,
) -> bool {
    granted.max_tokens_per_run <= requested.max_tokens_per_run
        && granted.max_cost_microusd_per_run <= requested.max_cost_microusd_per_run
        && granted.max_active_seconds_per_run <= requested.max_active_seconds_per_run
        && granted.max_tokens_per_month <= requested.max_tokens_per_month
        && granted.max_cost_microusd_per_month <= requested.max_cost_microusd_per_month
        && granted.max_starts_per_period <= requested.max_starts_per_period
        && granted.period_seconds == requested.period_seconds
        && granted.max_causation_depth <= requested.max_causation_depth
        && granted.max_spend_depth <= requested.max_spend_depth
        && granted.max_contribution_proposals_per_run
            <= requested.max_contribution_proposals_per_run
}

fn event_source_policy() -> AppDataHandlingPolicy {
    AppDataHandlingPolicy {
        classification_floor: AppDataClassification::Sensitive,
        model_processing: AppModelProcessing::LocalOnly,
        personal_agent_access: AppPersonalAgentAccess::Denied,
        memory_promotion: AppMemoryPromotion::Denied,
        external_egress: AppExternalEgress::Denied,
        approved_destinations: Vec::new(),
    }
}

fn projection_outcome(value: &Value) -> Result<AppEventTerminalOutcomeV1, AppEventBehaviorError> {
    match value.get("outcome").and_then(Value::as_str) {
        Some("succeeded") => Ok(AppEventTerminalOutcomeV1::Succeeded),
        Some("failed") => Ok(AppEventTerminalOutcomeV1::Failed),
        _ => Err(AppEventBehaviorError::CorruptState),
    }
}

fn projection_recorded_at(value: &Value) -> Result<DateTime<Utc>, AppEventBehaviorError> {
    value
        .get("recorded_at")
        .and_then(Value::as_str)
        .ok_or(AppEventBehaviorError::CorruptState)
        .and_then(parse_timestamp)
}

fn terminal_outcome_name(outcome: AppEventTerminalOutcomeV1) -> &'static str {
    match outcome {
        AppEventTerminalOutcomeV1::Succeeded => "succeeded",
        AppEventTerminalOutcomeV1::Failed => "failed",
        AppEventTerminalOutcomeV1::Cancelled => "cancelled",
    }
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, AppEventBehaviorError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| AppEventBehaviorError::CorruptState)
}

fn event_interval_ready_at(
    last_started: Option<DateTime<Utc>>,
    min_interval_seconds: u64,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, AppEventBehaviorError> {
    let Some(last_started) = last_started else {
        return Ok(now);
    };
    let seconds =
        i64::try_from(min_interval_seconds).map_err(|_| AppEventBehaviorError::CorruptState)?;
    last_started
        .checked_add_signed(Duration::seconds(seconds))
        .ok_or(AppEventBehaviorError::CorruptState)
}
