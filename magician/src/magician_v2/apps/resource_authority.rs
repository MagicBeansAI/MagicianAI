//! Durable canonical resource authority for app execution trees.
//!
//! The Phase-0 contract owns evaluation semantics. This Phase-4B adopter
//! persists roots, nodes, reservations, observations, settlement and closure
//! in the existing registry-owned scoped transaction. The append-only journal
//! is authoritative; trigger-maintained period totals are the installation
//! authority, while scalar tree state and `app_resource_usage_projection` are
//! rebuildable indexes/read models and are never consulted for admission.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet, VecDeque},
    fmt,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Datelike, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{
    authority::{AppAuthorityCeiling, AuthenticatedAppScope, ResolvedAppAuthority},
    effect_kernel::AppEffectAbortProof,
    lifecycle::AppInstallationStatus,
    models::{
        decode_app_contract, AppContractError, AppContractLimits, AppDigest, AppInstallationId,
        AppMutationCommand, AppReference, AppRevision, AppScopeBindingRef, ValidateAppContract,
    },
    package_lock::validate_accepted_cleanup_package_lock,
    package_staging::{AppPackageStager, AppPackageStagingError, StagedAppPackage},
    records::{
        AppGrantRevision, AppInstallation, AppMutationOrigin, AppMutationReceipt,
        AppPackageRevision, AppResourceBehaviorIdentity, AppRunBinding, AppSchemaRevision,
        AppScope,
    },
    registry::{canonical_package_revision_ref, AppRegistryError, AppRegistryService},
    resource_contract::{
        assess_app_resource_journal, assess_app_resource_journal_with_recovery,
        decode_app_resource_journal, evaluate_app_resource_candidate_period,
        evaluate_app_resource_reservation, evaluate_app_resource_root_admission,
        project_admitted_active_elapsed_ms, AppActiveInterval, AppCapabilityResourceQuantity,
        AppEffectDispatchAbortReason, AppResourceAssessment, AppResourceBehaviorPeriodBaseline,
        AppResourceBreach, AppResourceContractError, AppResourceEffectResult,
        AppResourceEnforcementPolicy, AppResourceExecutionLane, AppResourceJournal,
        AppResourceJournalEvent, AppResourceNodeKind, AppResourceObservationSource,
        AppResourcePeriodBaseline, AppResourceQuantity, AppResourceSettlementOutcome,
        AppResourceTreeIdentity, CurrentAppResourceAuthority, TrustedAppResourceRecoveryEvidence,
        HARD_MAX_JOURNAL_EVENTS,
    },
};
use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace, json_traversal::canonical_json_bytes,
};

const MAX_STORED_RESOURCE_RECORD_BYTES: usize = 256 * 1024;
const MAX_RETIREMENT_TREES_PER_TRANSACTION: u16 = 32;
const MAX_RETIREMENT_TRANSACTION_BYTES: usize = 16 * 1024 * 1024;

/// Current non-app owners' contribution to root admission.
///
/// Fields are private and this type is intentionally not deserializable. The
/// scheduler and package-store adapters are the only intended constructors;
/// the scheduler adapter must retain its admission guard until `admit_root`
/// returns. Persisted app bytes cannot manufacture capacity or package size.
pub struct AppResourceAdmissionSnapshot {
    period_ref: AppReference,
    period_ends_at_elapsed_ms: u64,
    package_revision_ref: AppReference,
    package_bytes: u64,
    scheduler_foreground_runs_excluding_root: u16,
    scheduler_background_runs_excluding_root: u16,
    observed_at: DateTime<Utc>,
    scheduler_guard: AppResourceSchedulerAdmissionGuard,
}

impl AppResourceAdmissionSnapshot {
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn from_authoritative_owners(
        period_ref: AppReference,
        period_ends_at_elapsed_ms: u64,
        package: AppResourcePackageMeasurement,
        scheduler_guard: AppResourceSchedulerAdmissionGuard,
        observed_at: DateTime<Utc>,
    ) -> Result<Self, AppResourceAuthorityError> {
        if period_ends_at_elapsed_ms == 0 {
            return Err(AppResourceAuthorityError::InvalidAdmissionSnapshot(
                "period end must be greater than zero",
            ));
        }
        Ok(Self {
            period_ref,
            period_ends_at_elapsed_ms,
            package_revision_ref: package.package_revision_ref,
            package_bytes: package.package_bytes,
            scheduler_foreground_runs_excluding_root: scheduler_guard
                .foreground_runs_excluding_root,
            scheduler_background_runs_excluding_root: scheduler_guard
                .background_runs_excluding_root,
            observed_at,
            scheduler_guard,
        })
    }
}

/// Fresh package/scheduler facts used after root admission. Unlike the root
/// snapshot this carries no additional capacity permit: the execution retains
/// its original [`AppResourceRootDispatchLease`] for the whole tree.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceCurrentSnapshot {
    period_ref: AppReference,
    period_ends_at_elapsed_ms: u64,
    package_revision_ref: AppReference,
    package_bytes: u64,
    scheduler_foreground_runs_excluding_root: u16,
    scheduler_background_runs_excluding_root: u16,
    observed_at: DateTime<Utc>,
}

impl AppResourceCurrentSnapshot {
    #[cfg(any(test, feature = "test-fixtures"))]
    #[allow(clippy::too_many_arguments)]
    pub fn from_authoritative_owners(
        period_ref: AppReference,
        period_ends_at_elapsed_ms: u64,
        package: AppResourcePackageMeasurement,
        scheduler_foreground_runs_excluding_root: u16,
        scheduler_background_runs_excluding_root: u16,
        observed_at: DateTime<Utc>,
    ) -> Result<Self, AppResourceAuthorityError> {
        if period_ends_at_elapsed_ms == 0 {
            return Err(AppResourceAuthorityError::InvalidAdmissionSnapshot(
                "period end must be greater than zero",
            ));
        }
        Ok(Self {
            period_ref,
            period_ends_at_elapsed_ms,
            package_revision_ref: package.package_revision_ref,
            package_bytes: package.package_bytes,
            scheduler_foreground_runs_excluding_root,
            scheduler_background_runs_excluding_root,
            observed_at,
        })
    }
}

/// Package-size evidence minted by the canonical package owner. The type has
/// no transport deserializer, so package/app bytes cannot choose this value.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourcePackageMeasurement {
    package_revision_ref: AppReference,
    package_bytes: u64,
}

impl AppResourcePackageMeasurement {
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn from_package_store(package_revision_ref: AppReference, package_bytes: u64) -> Self {
        Self {
            package_revision_ref,
            package_bytes,
        }
    }

    fn from_staged_package(
        package_revision_ref: AppReference,
        staged: &StagedAppPackage,
    ) -> Result<Self, AppResourceAuthorityError> {
        let package_bytes =
            staged
                .candidate()
                .members()
                .iter()
                .try_fold(0_u64, |total, member| {
                    let byte_len = u64::try_from(member.bytes().len())
                        .map_err(|_| AppResourceAuthorityError::IntegerRange("package_bytes"))?;
                    total
                        .checked_add(byte_len)
                        .ok_or(AppResourceAuthorityError::IntegerRange("package_bytes"))
                })?;
        Ok(Self {
            package_revision_ref,
            package_bytes,
        })
    }
}

#[derive(Debug, Default)]
struct AppResourceSchedulerCounts {
    foreground: u16,
    background: u16,
    active_roots: HashSet<String>,
}

/// Move-only scheduler evidence. A foreground run holds one global capacity
/// permit. A background run additionally holds a permit from the scheduler's
/// background semaphore, whose size leaves the foreground reserve untouched.
pub struct AppResourceSchedulerAdmissionGuard {
    lane: AppResourceExecutionLane,
    foreground_runs_excluding_root: u16,
    background_runs_excluding_root: u16,
    _total_permit: OwnedSemaphorePermit,
    _background_permit: Option<OwnedSemaphorePermit>,
    runtime_counts: Option<Arc<Mutex<AppResourceSchedulerCounts>>>,
    active_root_key: Option<String>,
}

impl std::fmt::Debug for AppResourceSchedulerAdmissionGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppResourceSchedulerAdmissionGuard")
            .field("lane", &self.lane)
            .field(
                "foreground_runs_excluding_root",
                &self.foreground_runs_excluding_root,
            )
            .field(
                "background_runs_excluding_root",
                &self.background_runs_excluding_root,
            )
            .finish_non_exhaustive()
    }
}

impl AppResourceSchedulerAdmissionGuard {
    fn from_scheduler(
        lane: AppResourceExecutionLane,
        foreground_runs_excluding_root: u16,
        background_runs_excluding_root: u16,
        total_permit: OwnedSemaphorePermit,
        background_permit: Option<OwnedSemaphorePermit>,
        runtime_counts: Arc<Mutex<AppResourceSchedulerCounts>>,
        active_root_key: String,
    ) -> Self {
        debug_assert_eq!(
            matches!(lane, AppResourceExecutionLane::Background),
            background_permit.is_some()
        );
        Self {
            lane,
            foreground_runs_excluding_root,
            background_runs_excluding_root,
            _total_permit: total_permit,
            _background_permit: background_permit,
            runtime_counts: Some(runtime_counts),
            active_root_key: Some(active_root_key),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn isolated(
        lane: AppResourceExecutionLane,
        foreground_runs_excluding_root: u16,
        background_runs_excluding_root: u16,
    ) -> Self {
        use std::sync::Arc;

        use tokio::sync::Semaphore;

        let total_permit = Arc::new(Semaphore::new(1))
            .try_acquire_owned()
            .expect("isolated scheduler capacity");
        let background_permit = matches!(lane, AppResourceExecutionLane::Background).then(|| {
            Arc::new(Semaphore::new(1))
                .try_acquire_owned()
                .expect("isolated background capacity")
        });
        Self {
            lane,
            foreground_runs_excluding_root,
            background_runs_excluding_root,
            _total_permit: total_permit,
            _background_permit: background_permit,
            runtime_counts: None,
            active_root_key: None,
        }
    }
}

impl Drop for AppResourceSchedulerAdmissionGuard {
    fn drop(&mut self) {
        let Some(runtime_counts) = self.runtime_counts.as_ref() else {
            return;
        };
        let mut counts = runtime_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(active_root_key) = self.active_root_key.as_ref() {
            counts.active_roots.remove(active_root_key);
        }
        match self.lane {
            AppResourceExecutionLane::Foreground => {
                counts.foreground = counts.foreground.saturating_sub(1);
            },
            AppResourceExecutionLane::Background => {
                counts.background = counts.background.saturating_sub(1);
            },
        }
    }
}

/// Move-only root dispatch authority returned only after durable admission.
/// Runtime work retains it for the complete root lifetime.
pub struct AppResourceRootDispatchLease {
    identity: AppResourceTreeIdentity,
    live: Arc<()>,
    /// Durable UTC origin written with the canonical root admission. All
    /// elapsed resource observations are measured from this instant, including
    /// after a crash/resume. Keeping the origin in the move-only lease prevents
    /// a restarted executor from silently resetting lifetime/no-progress
    /// clocks.
    started_at: DateTime<Utc>,
    period_ends_at_elapsed_ms: u64,
    package_revision_ref: AppReference,
    package_bytes: u64,
    _scheduler_guard: AppResourceSchedulerAdmissionGuard,
}

impl std::fmt::Debug for AppResourceRootDispatchLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppResourceRootDispatchLease")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl AppResourceRootDispatchLease {
    pub fn identity(&self) -> &AppResourceTreeIdentity {
        &self.identity
    }

    /// Canonical elapsed time for an observation emitted at `now`.
    ///
    /// Execution adapters must use this value rather than a process-local
    /// `Instant`: the latter restarts at zero after recovery and would extend
    /// durable lifetime, reservation and no-progress limits.
    pub fn elapsed_at(&self, now: DateTime<Utc>) -> Result<u64, AppResourceAuthorityError> {
        elapsed_since_root(self.started_at, now)
    }

    pub(crate) fn dispatch_clock(&self) -> AppResourceRootDispatchClock {
        AppResourceRootDispatchClock {
            live: Arc::downgrade(&self.live),
            started_at: self.started_at,
        }
    }

    /// Convert an already admitted reservation's elapsed deadline to UTC.
    /// Recovery and preflight waits never restart or extend this clock.
    pub(crate) fn deadline_at(
        &self,
        expires_at_elapsed_ms: u64,
    ) -> Result<DateTime<Utc>, AppResourceAuthorityError> {
        super::effect_deadline::absolute_deadline(self.started_at, expires_at_elapsed_ms).ok_or(
            AppResourceAuthorityError::InvalidObservation(
                "resource deadline exceeds the supported timestamp range",
            ),
        )
    }

    /// Settlement cannot become impossible because wall clock moved backward
    /// after physical I/O. The workflow root supplies its last accepted
    /// elapsed high-water; a valid later UTC sample may advance it, while an
    /// invalid/backward sample is clamped to that durable runtime floor.
    pub(crate) fn settlement_elapsed_at(
        &self,
        now: DateTime<Utc>,
        elapsed_high_water_ms: u64,
    ) -> u64 {
        elapsed_since_root(self.started_at, now)
            .unwrap_or(elapsed_high_water_ms)
            .max(elapsed_high_water_ms)
    }
}

/// Read-only final-start clock tied to one live dispatch lease. It owns no
/// scheduler capacity and cannot reserve or spend resources. A physical permit
/// can check its immutable deadline after async disclosure checks without
/// retaining the resource mutex across callbacks that acquire the task lock.
pub(crate) struct AppResourceRootDispatchClock {
    live: std::sync::Weak<()>,
    started_at: DateTime<Utc>,
}

impl AppResourceRootDispatchClock {
    pub(crate) fn elapsed_at(&self, now: DateTime<Utc>) -> Result<u64, AppResourceAuthorityError> {
        if self.live.strong_count() == 0 {
            return Err(AppResourceAuthorityError::InvalidObservation(
                "resource dispatch lease was released before physical start",
            ));
        }
        elapsed_since_root(self.started_at, now)
    }
}

/// Exact accepted authority material needed to prove the identity of a
/// previously admitted root after a process restart. This is evidence, not a
/// dispatch capability: the canonical digest stored in the immutable resource
/// tree must match these fields before a cleanup lease can be minted.
///
/// The type is intentionally non-deserializable. The workflow owner rebuilds
/// it from its sealed task binding; the registry supplies the historical grant
/// digest and independently verifies every immutable package/grant/schema row.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceAcceptedCleanupSnapshot {
    scope_binding_ref: AppScopeBindingRef,
    installation_generation: u64,
    surface_revision: Option<AppRevision>,
    ceiling: AppAuthorityCeiling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppResourceAcceptedCleanupReason {
    Terminal,
    Cancelled,
    CrashRecovery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AppResourceAcceptedCleanupScope {
    WholeTree,
    ExecutionSubtree(AppReference),
}

impl AppResourceAcceptedCleanupScope {
    fn requires_terminal_settlement(&self) -> bool {
        matches!(self, Self::WholeTree)
    }
}

/// Content-free inspection state for one exact reservation which still
/// prevents an accepted execution tree from becoming terminal. These rows are
/// rebuilt from the bounded canonical journal while the cleanup lease is
/// minted; they grant no dispatch or settlement authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppResourcePendingReservationStatus {
    Held,
    OutcomeUncertain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Recovery adapters inspect only the fields relevant to their proof class.
pub struct AppResourcePendingReservation {
    pub node_id: AppReference,
    pub reservation_id: AppReference,
    pub operation_key: AppReference,
    pub requested: AppResourceQuantity,
    pub capability_requests: Vec<AppCapabilityResourceQuantity>,
    pub effect_binding_digest: Option<AppDigest>,
    pub effect_dispatch_started_at_elapsed_ms: Option<u64>,
    pub status: AppResourcePendingReservationStatus,
    pub created_at_elapsed_ms: u64,
    pub expires_at_elapsed_ms: u64,
    /// Journal-owned execution lifetime bound for late settlement. Closing a
    /// node does not erase uncertain spend, but no active interval may extend
    /// beyond that node's recorded close.
    pub node_closed_at_elapsed_ms: Option<u64>,
}

/// Non-transport proof that an independent canonical owner has decided this
/// exact task execution may be cleaned up. A maintenance candidate is not such
/// a proof: the workflow terminal/cancel owner or crash reconciler must mint
/// this value after validating its own durable marker and the current resource
/// journal revision.
pub struct AppResourceAcceptedCleanupProof {
    binding_digest: AppDigest,
    expected_journal_revision: u64,
    evidence_ref: AppReference,
    evidence_revision: AppRevision,
    reason: AppResourceAcceptedCleanupReason,
    cleanup_scope: AppResourceAcceptedCleanupScope,
}

impl fmt::Debug for AppResourceAcceptedCleanupProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppResourceAcceptedCleanupProof")
            .field("expected_journal_revision", &self.expected_journal_revision)
            .field("evidence_ref", &self.evidence_ref)
            .field("evidence_revision", &self.evidence_revision)
            .field("reason", &self.reason)
            .field("cleanup_scope", &self.cleanup_scope)
            .finish_non_exhaustive()
    }
}

impl AppResourceAcceptedCleanupProof {
    pub fn from_terminal_owner(
        binding: &AppRunBinding,
        expected_journal_revision: u64,
        evidence_ref: AppReference,
        evidence_revision: AppRevision,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::from_owner(
            binding,
            expected_journal_revision,
            evidence_ref,
            evidence_revision,
            AppResourceAcceptedCleanupReason::Terminal,
            AppResourceAcceptedCleanupScope::WholeTree,
        )
    }

    pub fn from_cancel_owner(
        binding: &AppRunBinding,
        expected_journal_revision: u64,
        evidence_ref: AppReference,
        evidence_revision: AppRevision,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::from_owner(
            binding,
            expected_journal_revision,
            evidence_ref,
            evidence_revision,
            AppResourceAcceptedCleanupReason::Cancelled,
            AppResourceAcceptedCleanupScope::WholeTree,
        )
    }

    pub fn from_crash_reconciler(
        binding: &AppRunBinding,
        expected_journal_revision: u64,
        evidence_ref: AppReference,
        evidence_revision: AppRevision,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::from_owner(
            binding,
            expected_journal_revision,
            evidence_ref,
            evidence_revision,
            AppResourceAcceptedCleanupReason::CrashRecovery,
            AppResourceAcceptedCleanupScope::WholeTree,
        )
    }

    pub fn from_cancelled_execution_owner(
        binding: &AppRunBinding,
        expected_journal_revision: u64,
        evidence_ref: AppReference,
        evidence_revision: AppRevision,
        execution_node_id: AppReference,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::from_owner(
            binding,
            expected_journal_revision,
            evidence_ref,
            evidence_revision,
            AppResourceAcceptedCleanupReason::Cancelled,
            AppResourceAcceptedCleanupScope::ExecutionSubtree(execution_node_id),
        )
    }

    pub fn from_crashed_execution_reconciler(
        binding: &AppRunBinding,
        expected_journal_revision: u64,
        evidence_ref: AppReference,
        evidence_revision: AppRevision,
        execution_node_id: AppReference,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::from_owner(
            binding,
            expected_journal_revision,
            evidence_ref,
            evidence_revision,
            AppResourceAcceptedCleanupReason::CrashRecovery,
            AppResourceAcceptedCleanupScope::ExecutionSubtree(execution_node_id),
        )
    }

    fn from_owner(
        binding: &AppRunBinding,
        expected_journal_revision: u64,
        evidence_ref: AppReference,
        evidence_revision: AppRevision,
        reason: AppResourceAcceptedCleanupReason,
        cleanup_scope: AppResourceAcceptedCleanupScope,
    ) -> Result<Self, AppResourceAuthorityError> {
        if expected_journal_revision == 0 {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "cleanup proof must bind a positive journal revision",
            ));
        }
        binding.validate_app_contract(&AppContractLimits::default())?;
        Ok(Self {
            binding_digest: AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(
                binding,
            )?)?),
            expected_journal_revision,
            evidence_ref,
            evidence_revision,
            reason,
            cleanup_scope,
        })
    }
}

impl AppResourceAcceptedCleanupSnapshot {
    pub fn from_execution_owner(
        scope_binding_ref: AppScopeBindingRef,
        installation_generation: u64,
        surface_revision: Option<AppRevision>,
        ceiling: AppAuthorityCeiling,
    ) -> Result<Self, AppResourceAuthorityError> {
        if installation_generation == 0 {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "installation generation must be positive",
            ));
        }
        Ok(Self {
            scope_binding_ref,
            installation_generation,
            surface_revision,
            ceiling,
        })
    }
}

/// Move-only cleanup ownership for one exact accepted execution tree.
///
/// This deliberately is not a `AppResourceRootDispatchLease`, implements no
/// conversion to one, and exposes no open/reserve/progress operation. It can
/// only be consumed by the cleanup/recovery methods below. Consequently a
/// revoked app can finish accounting for work already accepted before the
/// restart without regaining any authority to perform new work.
pub struct AppResourceAcceptedCleanupLease {
    identity: AppResourceTreeIdentity,
    binding: AppRunBinding,
    resolved: ResolvedAppAuthority,
    policy: AppResourceEnforcementPolicy,
    fence: AppResourceMutationFence,
    started_at: DateTime<Utc>,
    /// Monotonic canonical elapsed time. Wall-clock rollback after restart may
    /// never make accepted cleanup events precede already-durable journal
    /// state or strand the leaf-first close plan.
    elapsed_high_water_ms: u64,
    period_ends_at_elapsed_ms: u64,
    package_bytes: u64,
    open_nodes_leaf_first: VecDeque<AppReference>,
    pending_reservations: Vec<AppResourcePendingReservation>,
    unscoped_pending_reservations: usize,
    cleanup_reason: AppResourceAcceptedCleanupReason,
    cleanup_scope: AppResourceAcceptedCleanupScope,
    cleanup_binding_digest: AppDigest,
    cleanup_evidence_ref: AppReference,
    cleanup_evidence_revision: AppRevision,
    _scheduler_guard: AppResourceSchedulerAdmissionGuard,
}

impl fmt::Debug for AppResourceAcceptedCleanupLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppResourceAcceptedCleanupLease")
            .field("identity", &self.identity)
            .field("pending_node_closures", &self.open_nodes_leaf_first.len())
            .field("pending_reservations", &self.pending_reservations.len())
            .field("cleanup_reason", &self.cleanup_reason)
            .finish_non_exhaustive()
    }
}

impl AppResourceAcceptedCleanupLease {
    pub fn has_pending_node_closures(&self) -> bool {
        !self.open_nodes_leaf_first.is_empty()
    }

    pub fn pending_reservations(&self) -> &[AppResourcePendingReservation] {
        &self.pending_reservations
    }

    pub fn has_pending_reservations(&self) -> bool {
        !self.pending_reservations.is_empty()
    }

    fn clamp_elapsed(&mut self, now: DateTime<Utc>) -> u64 {
        let sampled = now
            .signed_duration_since(self.started_at)
            .num_milliseconds()
            .max(0) as u64;
        self.elapsed_high_water_ms = self.elapsed_high_water_ms.max(sampled);
        self.elapsed_high_water_ms
    }
}

/// Internal evidence for charging the complete reserved upper bound when a
/// crashed dispatch owner cannot prove either the provider result or absence
/// of I/O. It can be constructed only from a crash-authorized accepted-cleanup
/// lease and is revalidated against the exact pre-append journal revision.
/// Nothing in the durable task payload can deserialize or mint this value.
struct AppResourceConservativePostIoCharge {
    node_id: AppReference,
    reservation_id: AppReference,
    operation_key: AppReference,
    requested: AppResourceQuantity,
    capability_requests: Vec<AppCapabilityResourceQuantity>,
    effect_binding_digest: Option<AppDigest>,
    cleanup_binding_digest: AppDigest,
    accepted_identity_digest: AppDigest,
    expected_journal_revision: u64,
    evidence_ref: AppReference,
    evidence_revision: AppRevision,
    active_intervals: Vec<AppActiveInterval>,
    at_elapsed_ms: u64,
    proof_digest: AppDigest,
    observation_id: AppReference,
}

impl fmt::Debug for AppResourceConservativePostIoCharge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppResourceConservativePostIoCharge")
            .field("verified", &true)
            .finish_non_exhaustive()
    }
}

pub struct AppResourceAcceptedCleanupReceipt {
    pub state: AppResourceTreeState,
    cleanup_lease: Option<AppResourceAcceptedCleanupLease>,
}

impl fmt::Debug for AppResourceAcceptedCleanupReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppResourceAcceptedCleanupReceipt")
            .field("state", &self.state)
            .field("has_cleanup_lease", &self.cleanup_lease.is_some())
            .finish()
    }
}

impl AppResourceAcceptedCleanupReceipt {
    pub fn into_cleanup_lease(
        mut self,
    ) -> Result<AppResourceAcceptedCleanupLease, AppResourceAuthorityError> {
        self.cleanup_lease
            .take()
            .ok_or(AppResourceAuthorityError::CleanupAlreadyComplete)
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppResourceRootAdmissionOutcome {
    Created,
    AlreadyPresent,
}

/// Rebuildable tree state returned to trusted runtime adapters. It is useful
/// for diagnostics and projection, but never grants another reservation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceTreeState {
    pub period_revision: AppRevision,
    pub journal_revision: u64,
    pub committed: AppResourceQuantity,
    pub outstanding_reserved: AppResourceQuantity,
    pub active_elapsed_ms: u64,
    pub admitted_active_elapsed_ms: u64,
    pub lifetime_elapsed_ms: u64,
    pub evaluated_at_elapsed_ms: u64,
    pub last_progress_elapsed_ms: u64,
    pub open_nodes: u32,
    pub held_reservations: u32,
    pub uncertain_reservations: u32,
    pub root_closed: bool,
    pub terminally_settled: bool,
    pub breaches: Vec<AppResourceBreach>,
}

impl AppResourceTreeState {
    fn from_assessment(assessment: &AppResourceAssessment, journal_revision: u64) -> Self {
        Self {
            period_revision: assessment.period_revision(),
            journal_revision,
            committed: assessment.committed(),
            outstanding_reserved: assessment.outstanding_reserved(),
            active_elapsed_ms: assessment.active_elapsed_ms(),
            admitted_active_elapsed_ms: assessment.admitted_active_elapsed_ms(),
            lifetime_elapsed_ms: assessment.lifetime_elapsed_ms(),
            evaluated_at_elapsed_ms: assessment.evaluated_at_elapsed_ms(),
            last_progress_elapsed_ms: assessment.last_progress_elapsed_ms(),
            open_nodes: assessment.open_nodes(),
            held_reservations: assessment.held_reservations(),
            uncertain_reservations: assessment.uncertain_reservations(),
            root_closed: assessment.root_closed(),
            terminally_settled: assessment.terminally_settled(),
            breaches: assessment.breaches().iter().copied().collect(),
        }
    }
}

pub struct AppResourceRootAdmissionReceipt {
    pub outcome: AppResourceRootAdmissionOutcome,
    pub identity: AppResourceTreeIdentity,
    /// Revision of the period snapshot under which the root was admitted.
    pub admitted_period_revision: AppRevision,
    /// Period revision after the root admission was atomically made visible.
    pub current_period_revision: AppRevision,
    pub state: AppResourceTreeState,
    dispatch_lease: Option<AppResourceRootDispatchLease>,
}

impl std::fmt::Debug for AppResourceRootAdmissionReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppResourceRootAdmissionReceipt")
            .field("outcome", &self.outcome)
            .field("identity", &self.identity)
            .field("admitted_period_revision", &self.admitted_period_revision)
            .field("current_period_revision", &self.current_period_revision)
            .field("state", &self.state)
            .field("has_dispatch_lease", &self.dispatch_lease.is_some())
            .finish()
    }
}

impl AppResourceRootAdmissionReceipt {
    pub fn into_dispatch_lease(
        mut self,
    ) -> Result<AppResourceRootDispatchLease, AppResourceAuthorityError> {
        self.dispatch_lease
            .take()
            .ok_or(AppResourceAuthorityError::Contract(
                AppResourceContractError::RootAlreadyClosed,
            ))
    }
}

/// Current durable revisions carried between authority calls. They are only a
/// CAS floor: every operation still re-resolves authenticated app authority
/// and current installation-period totals inside the write transaction.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceMutationFence {
    pub expected_journal_revision: u64,
    pub minimum_period_revision: AppRevision,
}

/// One freshly resolved mutation boundary. It is intentionally move-only and
/// non-deserializable; callers rebuild it from current authenticated authority
/// for every durable event.
pub struct AppResourceMutationAuthority {
    resolved: ResolvedAppAuthority,
    binding: AppRunBinding,
    policy: AppResourceEnforcementPolicy,
    snapshot: AppResourceCurrentSnapshot,
    fence: AppResourceMutationFence,
    /// Present only when the process-owned coordinator anchored this mutation
    /// to the durable root clock. Exact journal replays may carry an older
    /// elapsed value; new events must match this value inside the CAS.
    authoritative_elapsed_ms: Option<u64>,
    /// Present only for settlement/closure of work already admitted by this
    /// process. It permits lifecycle revocation after dispatch while binding
    /// the mutation to the exact immutable tree that minted the root lease.
    accepted_identity: Option<AppResourceTreeIdentity>,
}

impl AppResourceMutationAuthority {
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn from_current_owners(
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        snapshot: AppResourceCurrentSnapshot,
        fence: AppResourceMutationFence,
    ) -> Self {
        Self {
            resolved,
            binding,
            policy,
            snapshot,
            fence,
            authoritative_elapsed_ms: None,
            accepted_identity: None,
        }
    }

    fn from_runtime_owners(
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        snapshot: AppResourceCurrentSnapshot,
        fence: AppResourceMutationFence,
        authoritative_elapsed_ms: u64,
    ) -> Self {
        Self {
            resolved,
            binding,
            policy,
            snapshot,
            fence,
            authoritative_elapsed_ms: Some(authoritative_elapsed_ms),
            accepted_identity: None,
        }
    }

    fn from_accepted_runtime_owners(
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        snapshot: AppResourceCurrentSnapshot,
        fence: AppResourceMutationFence,
        authoritative_elapsed_ms: u64,
        accepted_identity: AppResourceTreeIdentity,
    ) -> Self {
        Self {
            resolved,
            binding,
            policy,
            snapshot,
            fence,
            authoritative_elapsed_ms: Some(authoritative_elapsed_ms),
            accepted_identity: Some(accepted_identity),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppResourceAppendOutcome {
    Created,
    AlreadyPresent,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceMutationReceipt {
    pub outcome: AppResourceAppendOutcome,
    pub previous_period_revision: AppRevision,
    pub current_period_revision: AppRevision,
    pub state: AppResourceTreeState,
    #[serde(skip)]
    reservation_dispatchable: bool,
}

/// Exact child-node identity supplied by the execution owner. Constructors
/// name every supported continuation kind so callers cannot infer a kind from
/// strings or from execution-id shape.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceNodeAdmission {
    node_id: AppReference,
    execution_ref: AppReference,
    parent_node_id: AppReference,
    node_kind: AppResourceNodeKind,
    at_elapsed_ms: u64,
}

impl AppResourceNodeAdmission {
    fn child(
        node_id: AppReference,
        execution_ref: AppReference,
        parent_node_id: AppReference,
        node_kind: AppResourceNodeKind,
        at_elapsed_ms: u64,
    ) -> Self {
        Self {
            node_id,
            execution_ref,
            parent_node_id,
            node_kind,
            at_elapsed_ms,
        }
    }

    pub fn delegated_child(
        node_id: AppReference,
        execution_ref: AppReference,
        parent_node_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::child(
            node_id,
            execution_ref,
            parent_node_id,
            AppResourceNodeKind::DelegatedChild,
            at_elapsed_ms,
        )
    }

    pub fn retry(
        node_id: AppReference,
        execution_ref: AppReference,
        parent_node_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::child(
            node_id,
            execution_ref,
            parent_node_id,
            AppResourceNodeKind::Retry,
            at_elapsed_ms,
        )
    }

    pub fn resume(
        node_id: AppReference,
        execution_ref: AppReference,
        parent_node_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::child(
            node_id,
            execution_ref,
            parent_node_id,
            AppResourceNodeKind::Resume,
            at_elapsed_ms,
        )
    }

    pub fn repair(
        node_id: AppReference,
        execution_ref: AppReference,
        parent_node_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::child(
            node_id,
            execution_ref,
            parent_node_id,
            AppResourceNodeKind::Repair,
            at_elapsed_ms,
        )
    }

    pub fn tool_call(
        node_id: AppReference,
        execution_ref: AppReference,
        parent_node_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::child(
            node_id,
            execution_ref,
            parent_node_id,
            AppResourceNodeKind::ToolCall,
            at_elapsed_ms,
        )
    }

    pub fn browser_or_network(
        node_id: AppReference,
        execution_ref: AppReference,
        parent_node_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::child(
            node_id,
            execution_ref,
            parent_node_id,
            AppResourceNodeKind::BrowserOrNetwork,
            at_elapsed_ms,
        )
    }

    pub fn synthesis(
        node_id: AppReference,
        execution_ref: AppReference,
        parent_node_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::child(
            node_id,
            execution_ref,
            parent_node_id,
            AppResourceNodeKind::Synthesis,
            at_elapsed_ms,
        )
    }

    pub fn reflection(
        node_id: AppReference,
        execution_ref: AppReference,
        parent_node_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::child(
            node_id,
            execution_ref,
            parent_node_id,
            AppResourceNodeKind::Reflection,
            at_elapsed_ms,
        )
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceProgressObservation {
    node_id: AppReference,
    progress_id: AppReference,
    at_elapsed_ms: u64,
}

impl AppResourceProgressObservation {
    pub fn from_execution_owner(
        node_id: AppReference,
        progress_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Self {
        Self {
            node_id,
            progress_id,
            at_elapsed_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceReservationRequest {
    node_id: AppReference,
    reservation_id: AppReference,
    operation_key: AppReference,
    requested: AppResourceQuantity,
    capability_requests: Vec<AppCapabilityResourceQuantity>,
    at_elapsed_ms: u64,
    expires_at_elapsed_ms: u64,
}

impl AppResourceReservationRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn from_dispatch_owner(
        node_id: AppReference,
        reservation_id: AppReference,
        operation_key: AppReference,
        requested: AppResourceQuantity,
        capability_requests: Vec<AppCapabilityResourceQuantity>,
        at_elapsed_ms: u64,
        expires_at_elapsed_ms: u64,
    ) -> Self {
        Self {
            node_id,
            reservation_id,
            operation_key,
            requested,
            capability_requests,
            at_elapsed_ms,
            expires_at_elapsed_ms,
        }
    }
}

/// Normalized exact observation. Named constructors map current meter owners
/// to the canonical source enum without string heuristics or double-counting.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceSettlementObservation {
    node_id: AppReference,
    reservation_id: AppReference,
    observation_id: AppReference,
    outcome: AppResourceSettlementOutcome,
    observation_sources: Vec<AppResourceObservationSource>,
    actual: AppResourceQuantity,
    capability_usage: Vec<AppCapabilityResourceQuantity>,
    active_intervals: Vec<AppActiveInterval>,
    effect_binding_digest: Option<AppDigest>,
    effect_result: Option<AppResourceEffectResult>,
    at_elapsed_ms: u64,
}

impl AppResourceSettlementObservation {
    #[allow(clippy::too_many_arguments)]
    fn committed(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        observation_sources: Vec<AppResourceObservationSource>,
        actual: AppResourceQuantity,
        capability_usage: Vec<AppCapabilityResourceQuantity>,
        active_intervals: Vec<AppActiveInterval>,
        at_elapsed_ms: u64,
    ) -> Self {
        Self {
            node_id,
            reservation_id,
            observation_id,
            outcome: AppResourceSettlementOutcome::Committed,
            observation_sources,
            actual,
            capability_usage,
            active_intervals,
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_llm_task_ledger(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        actual: AppResourceQuantity,
        active_intervals: Vec<AppActiveInterval>,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::committed(
            node_id,
            reservation_id,
            observation_id,
            vec![
                AppResourceObservationSource::ExecutionTokenMeter,
                AppResourceObservationSource::LlmTaskLedger,
            ],
            actual,
            Vec::new(),
            active_intervals,
            at_elapsed_ms,
        )
    }

    pub fn from_coding_task_active_time(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        active_intervals: Vec<AppActiveInterval>,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::committed(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::CodingTaskActiveTime],
            AppResourceQuantity::default(),
            Vec::new(),
            active_intervals,
            at_elapsed_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_tool_runtime(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        actual: AppResourceQuantity,
        capability_usage: Vec<AppCapabilityResourceQuantity>,
        active_intervals: Vec<AppActiveInterval>,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::committed(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::ToolRuntime],
            actual,
            capability_usage,
            active_intervals,
            at_elapsed_ms,
        )
    }

    pub fn from_browser_runtime(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        actual: AppResourceQuantity,
        active_intervals: Vec<AppActiveInterval>,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::committed(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::BrowserRuntime],
            actual,
            Vec::new(),
            active_intervals,
            at_elapsed_ms,
        )
    }

    pub fn from_app_store_transaction(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        actual: AppResourceQuantity,
        active_intervals: Vec<AppActiveInterval>,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::committed(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::AppStoreTransaction],
            actual,
            Vec::new(),
            active_intervals,
            at_elapsed_ms,
        )
    }

    pub fn from_attachment_store(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        actual: AppResourceQuantity,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::committed(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::AttachmentStore],
            actual,
            Vec::new(),
            Vec::new(),
            at_elapsed_ms,
        )
    }

    pub fn from_package_store(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        actual: AppResourceQuantity,
        at_elapsed_ms: u64,
    ) -> Self {
        Self::committed(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::PackageStore],
            actual,
            Vec::new(),
            Vec::new(),
            at_elapsed_ms,
        )
    }

    fn uncertain(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        observation_sources: Vec<AppResourceObservationSource>,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        if observation_sources.is_empty()
            || observation_sources.contains(&AppResourceObservationSource::CrashReconciler)
        {
            return Err(AppResourceAuthorityError::InvalidObservation(
                "uncertain observations require a concrete non-recovery owner",
            ));
        }
        Ok(Self {
            node_id,
            reservation_id,
            observation_id,
            outcome: AppResourceSettlementOutcome::OutcomeUncertain,
            observation_sources,
            actual: AppResourceQuantity::default(),
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms,
        })
    }

    /// Infallible common-owner downgrade used only after physical I/O (or a
    /// durable start whose abort is unresolved). The source is selected by a
    /// trusted workflow owner rather than transport bytes, so none of the
    /// generic constructor's caller-validation branches can apply.
    pub(crate) fn accepted_uncertain_effect(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        app_store_operation: bool,
        at_elapsed_ms: u64,
    ) -> Self {
        Self {
            node_id,
            reservation_id,
            observation_id,
            outcome: AppResourceSettlementOutcome::OutcomeUncertain,
            observation_sources: vec![if app_store_operation {
                AppResourceObservationSource::AppStoreTransaction
            } else {
                AppResourceObservationSource::ToolRuntime
            }],
            actual: AppResourceQuantity::default(),
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms,
        }
    }

    /// Infallible post-I/O downgrade for the retained LLM physical-attempt
    /// owner. All transport counters are discarded when any timestamp,
    /// identity or meter validation is unavailable; the canonical reservation
    /// still closes as uncertain under the accepted-settlement capability.
    pub(crate) fn accepted_uncertain_llm(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Self {
        Self {
            node_id,
            reservation_id,
            observation_id,
            outcome: AppResourceSettlementOutcome::OutcomeUncertain,
            observation_sources: vec![
                AppResourceObservationSource::ExecutionTokenMeter,
                AppResourceObservationSource::LlmTaskLedger,
            ],
            actual: AppResourceQuantity::default(),
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms,
        }
    }

    pub(crate) fn bind_accepted_effect_unchecked_by_transport(
        mut self,
        effect_binding_digest: AppDigest,
    ) -> Self {
        self.effect_binding_digest = Some(effect_binding_digest);
        self
    }

    pub(crate) fn bind_accepted_effect_result_unchecked_by_transport(
        mut self,
        result_digest: AppDigest,
        result_bytes: u64,
    ) -> Self {
        self.effect_result = Some(AppResourceEffectResult {
            result_digest,
            result_bytes,
        });
        self
    }

    /// Bind a common Apps effect identity to the canonical durable settlement
    /// event. Only tool/browser owners may add it; other resource domains keep
    /// their existing settlement contract unchanged.
    #[allow(dead_code)] // Checked convenience constructor retained for alternate physical owners.
    pub(crate) fn with_app_effect_binding(
        mut self,
        effect_binding_digest: AppDigest,
    ) -> Result<Self, AppResourceAuthorityError> {
        if !self.observation_sources.iter().any(|source| {
            matches!(
                source,
                AppResourceObservationSource::ToolRuntime
                    | AppResourceObservationSource::BrowserRuntime
            )
        }) {
            return Err(AppResourceAuthorityError::InvalidObservation(
                "an app effect binding requires a tool or browser observation owner",
            ));
        }
        self.effect_binding_digest = Some(effect_binding_digest);
        Ok(self)
    }

    #[allow(dead_code)] // Checked convenience constructor retained for alternate physical owners.
    pub(crate) fn with_app_effect_result(
        mut self,
        result_digest: AppDigest,
        result_bytes: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        if self.effect_binding_digest.is_none()
            || self.outcome != AppResourceSettlementOutcome::Committed
            || result_bytes > 16 * 1024 * 1024
        {
            return Err(AppResourceAuthorityError::InvalidObservation(
                "an effect result requires a bounded committed effect settlement",
            ));
        }
        self.effect_result = Some(AppResourceEffectResult {
            result_digest,
            result_bytes,
        });
        Ok(self)
    }

    pub fn uncertain_from_llm_task_ledger(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::uncertain(
            node_id,
            reservation_id,
            observation_id,
            vec![
                AppResourceObservationSource::ExecutionTokenMeter,
                AppResourceObservationSource::LlmTaskLedger,
            ],
            at_elapsed_ms,
        )
    }

    pub fn uncertain_from_coding_task(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::uncertain(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::CodingTaskActiveTime],
            at_elapsed_ms,
        )
    }

    pub fn uncertain_from_tool_runtime(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::uncertain(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::ToolRuntime],
            at_elapsed_ms,
        )
    }

    pub fn uncertain_from_browser_runtime(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::uncertain(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::BrowserRuntime],
            at_elapsed_ms,
        )
    }

    pub fn uncertain_from_app_store(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::uncertain(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::AppStoreTransaction],
            at_elapsed_ms,
        )
    }

    pub fn uncertain_from_attachment_store(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::uncertain(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::AttachmentStore],
            at_elapsed_ms,
        )
    }

    pub fn uncertain_from_package_store(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        Self::uncertain(
            node_id,
            reservation_id,
            observation_id,
            vec![AppResourceObservationSource::PackageStore],
            at_elapsed_ms,
        )
    }
}

/// Trusted proof that an uncertain/held reservation never dispatched.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceCrashReconciliation {
    node_id: AppReference,
    reservation_id: AppReference,
    observation_id: AppReference,
    reconciliation_ref: AppReference,
    reconciliation_revision: AppRevision,
    reconciled_at_elapsed_ms: u64,
}

/// Trusted proof emitted only by the dispatch owner before it performs any
/// provider, tool, network or entity-store I/O. The proof has no deserializer
/// and is accepted only while consuming the move-only reservation permit, so a
/// durable replay cannot manufacture an unspent release.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourcePreIoUnspentReconciliation {
    observation_id: AppReference,
    reconciliation_ref: AppReference,
    reconciliation_revision: AppRevision,
    reconciled_at_elapsed_ms: u64,
}

impl AppResourcePreIoUnspentReconciliation {
    pub fn from_dispatch_owner(
        observation_id: AppReference,
        reconciliation_ref: AppReference,
        reconciliation_revision: AppRevision,
        reconciled_at_elapsed_ms: u64,
    ) -> Self {
        Self {
            observation_id,
            reconciliation_ref,
            reconciliation_revision,
            reconciled_at_elapsed_ms,
        }
    }
}

/// Trusted evidence that the entity store committed after the resource
/// reservation was durable but before its move-only dispatch permit could be
/// settled. This proof is deliberately neither serializable nor
/// deserializable. Only the crate-owned receipt loader may construct it from a
/// canonical mutation receipt, and its `Debug` output reveals no receipt or
/// intent content.
pub struct AppResourceCommittedRecovery {
    node_id: AppReference,
    reservation_id: AppReference,
    operation_key: AppReference,
    intent_digest: AppDigest,
    origin: AppMutationOrigin,
    mutation_key: AppDigest,
    batch_digest: AppDigest,
    receipt_id: AppReference,
    receipt_digest: AppDigest,
    installation_id: AppInstallationId,
    actual: AppResourceQuantity,
    at_elapsed_ms: u64,
    proof_digest: AppDigest,
    observation_id: AppReference,
}

impl fmt::Debug for AppResourceCommittedRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppResourceCommittedRecovery")
            .field("verified", &true)
            .finish_non_exhaustive()
    }
}

impl AppResourceCommittedRecovery {
    /// Construct committed-recovery evidence from a receipt that was loaded
    /// through the canonical entity-mutation receipt boundary. The redundant
    /// expected values make a stale or rebound workflow intent fail here,
    /// before it can reach the resource journal.
    #[allow(clippy::too_many_arguments)]
    pub fn from_verified_mutation_receipt(
        node_id: AppReference,
        reservation_id: AppReference,
        operation_key: AppReference,
        intent_digest: AppDigest,
        expected_origin: AppMutationOrigin,
        expected_mutation_key: AppDigest,
        expected_batch_digest: AppDigest,
        receipt: &AppMutationReceipt,
        actual: AppResourceQuantity,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        receipt.validate_app_contract(&AppContractLimits::default())?;
        if receipt.origin != expected_origin {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "mutation receipt origin does not match the persisted intent",
            ));
        }
        if receipt.mutation_key != expected_mutation_key {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "mutation receipt key does not match the persisted intent",
            ));
        }
        if receipt.batch_digest != expected_batch_digest {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "mutation receipt batch does not match the persisted intent",
            ));
        }
        let committed_count = u64::try_from(receipt.committed_record_revisions.len())
            .map_err(|_| AppResourceAuthorityError::IntegerRange("committed record count"))?;
        let change_count = receipt
            .change_seq_range
            .last
            .checked_sub(receipt.change_seq_range.first)
            .and_then(|span| span.checked_add(1))
            .ok_or(AppResourceAuthorityError::InvalidCommittedRecovery(
                "mutation receipt change range is invalid",
            ))?;
        if committed_count != change_count {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "mutation receipt change range does not match committed records",
            ));
        }
        if actual.records != committed_count {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "resource quantity does not match committed record count",
            ));
        }
        let mutation_hex = expected_mutation_key
            .as_str()
            .strip_prefix("blake3:")
            .ok_or(AppResourceAuthorityError::InvalidCommittedRecovery(
                "mutation receipt key is not canonical",
            ))?;
        let expected_receipt_id = AppReference::parse(format!("app-receipt:{mutation_hex}"))?;
        if receipt.receipt_id != expected_receipt_id {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "mutation receipt identity is not canonical for its key",
            ));
        }

        let receipt_bytes = canonical_json_bytes(&serde_json::to_value(receipt)?)?;
        ensure_stored_record_bounded("committed recovery receipt", &receipt_bytes)?;
        let receipt_digest = AppDigest::blake3(&receipt_bytes);
        let proof_digest = canonical_committed_recovery_proof_digest(
            &node_id,
            &reservation_id,
            &operation_key,
            &intent_digest,
            &expected_origin,
            &expected_mutation_key,
            &expected_batch_digest,
            &receipt.receipt_id,
            &receipt_digest,
            &receipt.installation_id,
            actual,
            at_elapsed_ms,
        )?;
        let observation_id = committed_recovery_observation_id(&proof_digest)?;

        Ok(Self {
            node_id,
            reservation_id,
            operation_key,
            intent_digest,
            origin: expected_origin,
            mutation_key: expected_mutation_key,
            batch_digest: expected_batch_digest,
            receipt_id: receipt.receipt_id.clone(),
            receipt_digest,
            installation_id: receipt.installation_id.clone(),
            actual,
            at_elapsed_ms,
            proof_digest,
            observation_id,
        })
    }
}

/// Trusted proof that a persisted terminal intent required no entity-store
/// mutation. Like receipt recovery, this type is move-only and has no wire
/// representation; only server code which holds the exact validated empty
/// command can construct it.
pub struct AppResourceNoEffectCommittedRecovery {
    node_id: AppReference,
    reservation_id: AppReference,
    operation_key: AppReference,
    installation_id: AppInstallationId,
    intent_digest: AppDigest,
    intent_payload_bytes: u64,
    command_digest: AppDigest,
    response_digest: AppDigest,
    response_payload_bytes: u64,
    actual: AppResourceQuantity,
    at_elapsed_ms: u64,
    proof_digest: AppDigest,
    observation_id: AppReference,
}

impl fmt::Debug for AppResourceNoEffectCommittedRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppResourceNoEffectCommittedRecovery")
            .field("verified", &true)
            .finish_non_exhaustive()
    }
}

impl AppResourceNoEffectCommittedRecovery {
    #[allow(clippy::too_many_arguments)]
    pub fn from_verified_empty_mutation_command(
        node_id: AppReference,
        reservation_id: AppReference,
        operation_key: AppReference,
        installation_id: AppInstallationId,
        canonical_intent_bytes: &[u8],
        command: &AppMutationCommand,
        canonical_response_bytes: &[u8],
        actual: AppResourceQuantity,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        if !command.operations.is_empty() || !command.expected_record_revisions.is_empty() {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "no-effect recovery requires an empty mutation command",
            ));
        }
        if actual.input_tokens != 0
            || actual.cached_input_tokens != 0
            || actual.output_tokens != 0
            || actual.cost_microusd != 0
            || actual.paid_tool_invocations != 0
            || actual.browser_network_actions != 0
            || actual.records != 0
            || actual.attachment_bytes != 0
        {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "no-effect recovery may report only exact canonical payload bytes",
            ));
        }
        ensure_stored_record_bounded("no-effect recovery intent", canonical_intent_bytes)?;
        ensure_stored_record_bounded("no-effect recovery response", canonical_response_bytes)?;
        ensure_canonical_recovery_json(
            "no-effect recovery intent is not canonical JSON",
            canonical_intent_bytes,
        )?;
        ensure_canonical_recovery_json(
            "no-effect recovery response is not canonical JSON",
            canonical_response_bytes,
        )?;
        let intent_payload_bytes = u64::try_from(canonical_intent_bytes.len())
            .map_err(|_| AppResourceAuthorityError::IntegerRange("intent_payload_bytes"))?;
        let response_payload_bytes = u64::try_from(canonical_response_bytes.len())
            .map_err(|_| AppResourceAuthorityError::IntegerRange("response_payload_bytes"))?;
        let exact_payload_bytes = intent_payload_bytes
            .checked_add(response_payload_bytes)
            .ok_or(AppResourceAuthorityError::IntegerRange(
                "no_effect_payload_bytes",
            ))?;
        if actual.payload_bytes != exact_payload_bytes {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "no-effect recovery payload does not match canonical intent and response bytes",
            ));
        }
        let intent_digest = AppDigest::blake3(canonical_intent_bytes);
        let response_digest = AppDigest::blake3(canonical_response_bytes);
        let command_bytes = canonical_json_bytes(&serde_json::to_value(command)?)?;
        ensure_stored_record_bounded("no-effect recovery command", &command_bytes)?;
        let command_digest = AppDigest::blake3(&command_bytes);
        let proof_digest = canonical_no_effect_recovery_proof_digest(
            &node_id,
            &reservation_id,
            &operation_key,
            &installation_id,
            &intent_digest,
            intent_payload_bytes,
            &command_digest,
            &response_digest,
            response_payload_bytes,
            actual,
            at_elapsed_ms,
        )?;
        let observation_id = no_effect_recovery_observation_id(&proof_digest)?;
        Ok(Self {
            node_id,
            reservation_id,
            operation_key,
            installation_id,
            intent_digest,
            intent_payload_bytes,
            command_digest,
            response_digest,
            response_payload_bytes,
            actual,
            at_elapsed_ms,
            proof_digest,
            observation_id,
        })
    }
}

fn ensure_canonical_recovery_json(
    error: &'static str,
    bytes: &[u8],
) -> Result<(), AppResourceAuthorityError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| AppResourceAuthorityError::InvalidCommittedRecovery(error))?;
    if canonical_json_bytes(&value)? != bytes {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(error));
    }
    Ok(())
}

/// Trusted negative receipt claim for the ambiguous crash window around the
/// atomic entity mutation. Canonical append rechecks absence under the same
/// immediate SQLite transaction that records release, so a generic lookup
/// failure can never become durable ProvenUnspent evidence.
pub struct AppResourceReceiptAbsentReconciliation {
    node_id: AppReference,
    reservation_id: AppReference,
    operation_key: AppReference,
    installation_id: AppInstallationId,
    intent_digest: AppDigest,
    origin: AppMutationOrigin,
    mutation_key: AppDigest,
    batch_digest: AppDigest,
    actual: AppResourceQuantity,
    reconciled_at_elapsed_ms: u64,
    proof_digest: AppDigest,
    observation_id: AppReference,
    reconciliation_ref: AppReference,
    reconciliation_revision: AppRevision,
}

impl fmt::Debug for AppResourceReceiptAbsentReconciliation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppResourceReceiptAbsentReconciliation")
            .field("verified", &true)
            .finish_non_exhaustive()
    }
}

impl AppResourceReceiptAbsentReconciliation {
    #[allow(clippy::too_many_arguments)]
    pub fn from_verified_receipt_absence(
        node_id: AppReference,
        reservation_id: AppReference,
        operation_key: AppReference,
        installation_id: AppInstallationId,
        intent_digest: AppDigest,
        expected_origin: AppMutationOrigin,
        expected_mutation_key: AppDigest,
        expected_batch_digest: AppDigest,
        actual: AppResourceQuantity,
        reconciled_at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        if actual != AppResourceQuantity::default() {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "receipt-absence recovery cannot report committed resource usage",
            ));
        }
        let proof_digest = canonical_receipt_absence_proof_digest(
            &node_id,
            &reservation_id,
            &operation_key,
            &installation_id,
            &intent_digest,
            &expected_origin,
            &expected_mutation_key,
            &expected_batch_digest,
            actual,
            reconciled_at_elapsed_ms,
        )?;
        let proof_hex = proof_digest.as_str().strip_prefix("blake3:").ok_or(
            AppResourceAuthorityError::InvalidCommittedRecovery(
                "receipt-absence proof digest is not canonical",
            ),
        )?;
        let observation_id = AppReference::parse(format!("resource-receipt-absent:{proof_hex}"))?;
        let reconciliation_ref =
            AppReference::parse(format!("resource-receipt-absence-proof:{proof_hex}"))?;
        let reconciliation_revision = AppRevision::new(1)?;
        Ok(Self {
            node_id,
            reservation_id,
            operation_key,
            installation_id,
            intent_digest,
            origin: expected_origin,
            mutation_key: expected_mutation_key,
            batch_digest: expected_batch_digest,
            actual,
            reconciled_at_elapsed_ms,
            proof_digest,
            observation_id,
            reconciliation_ref,
            reconciliation_revision,
        })
    }

    fn as_crash_reconciliation(&self) -> AppResourceCrashReconciliation {
        AppResourceCrashReconciliation {
            node_id: self.node_id.clone(),
            reservation_id: self.reservation_id.clone(),
            observation_id: self.observation_id.clone(),
            reconciliation_ref: self.reconciliation_ref.clone(),
            reconciliation_revision: self.reconciliation_revision,
            reconciled_at_elapsed_ms: self.reconciled_at_elapsed_ms,
        }
    }
}

impl AppResourceCrashReconciliation {
    pub fn from_crash_reconciler(
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        reconciliation_ref: AppReference,
        reconciliation_revision: AppRevision,
        reconciled_at_elapsed_ms: u64,
    ) -> Self {
        Self {
            node_id,
            reservation_id,
            observation_id,
            reconciliation_ref,
            reconciliation_revision,
            reconciled_at_elapsed_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceNodeClose {
    node_id: AppReference,
    at_elapsed_ms: u64,
}

impl AppResourceNodeClose {
    pub fn from_execution_owner(node_id: AppReference, at_elapsed_ms: u64) -> Self {
        Self {
            node_id,
            at_elapsed_ms,
        }
    }
}

/// Persisted reservation authority. Providers receive this only after the
/// reservation event and installation-period revision commit atomically.
#[derive(Debug)]
pub struct AppResourceOperationDispatchPermit {
    identity: AppResourceTreeIdentity,
    node_id: AppReference,
    reservation_id: AppReference,
    operation_key: AppReference,
    journal_revision: u64,
    created_at_elapsed_ms: u64,
    expires_at_elapsed_ms: u64,
}

impl AppResourceOperationDispatchPermit {
    pub(crate) fn expires_at_elapsed_ms(&self) -> u64 {
        self.expires_at_elapsed_ms
    }
}

/// Settlement-only recovery owner reconstructed from an exact durable
/// Reserved -> EffectDispatchStarted journal prefix. It intentionally exposes
/// no dispatch/start/live-for-I/O method, so sealed completion replay cannot
/// become another provider permit.
pub(crate) struct AppResourceEffectCompletionPermit {
    dispatch_permit: AppResourceOperationDispatchPermit,
    effect_binding_digest: AppDigest,
    requested: AppResourceQuantity,
    capability_requests: Vec<AppCapabilityResourceQuantity>,
    dispatch_started_at_elapsed_ms: u64,
    provider_completed_at_elapsed_ms: u64,
}

impl AppResourceEffectCompletionPermit {
    /// Rebuild the exact normal tool-runtime charge from canonical journal
    /// material. A sealed completion replay must not undercharge by trusting a
    /// sidecar/default quantity or by starting its active interval at recovery.
    pub(crate) fn committed_tool_observation(
        &self,
        observation_id: AppReference,
        at_elapsed_ms: u64,
    ) -> AppResourceSettlementObservation {
        let mut actual = self.requested;
        // Compiled-provider transport is not durable app-store payload. Its
        // reviewed input/result ceilings are enforced by the effect owner.
        actual.payload_bytes = 0;
        let active_intervals = (self.provider_completed_at_elapsed_ms
            > self.dispatch_started_at_elapsed_ms)
            .then_some(AppActiveInterval {
                start_elapsed_ms: self.dispatch_started_at_elapsed_ms,
                end_elapsed_ms: self.provider_completed_at_elapsed_ms,
            })
            .into_iter()
            .collect();
        AppResourceSettlementObservation::from_tool_runtime(
            self.dispatch_permit.node_id.clone(),
            self.dispatch_permit.reservation_id.clone(),
            observation_id,
            actual,
            self.capability_requests.clone(),
            active_intervals,
            at_elapsed_ms,
        )
    }

    pub(crate) fn dispatch_started_at_elapsed_ms(&self) -> u64 {
        self.dispatch_started_at_elapsed_ms
    }
}

pub(crate) enum AppResourceEffectCompletionRecovery {
    NeedsSettlement {
        permit: AppResourceEffectCompletionPermit,
        journal_revision: u64,
    },
    AlreadySettled {
        journal_revision: u64,
    },
}

/// Read-only durable state for one exact reservation/effect identity. This is
/// deliberately evidence only: none of the variants can authorize provider
/// I/O or construct a settlement without the original move-only owner. An
/// abort also returns the exact cacheable admitted-active projection rebuilt
/// from the same bounded canonical journal.
pub(crate) enum AppResourceEffectDispatchCheckpoint {
    Held {
        journal_revision: u64,
    },
    Started {
        journal_revision: u64,
    },
    AbortedBeforeIo {
        journal_revision: u64,
        admitted_active_elapsed_ms: u64,
    },
    Settled,
}

/// Move-only capability for settling one exact previously admitted
/// reservation after the originating credential expires. It is never Serde
/// and has no public constructor. The registry can read only its sealed scope;
/// the resource owner separately checks every root/reservation identity before
/// entering the accepted-settlement write lane.
pub(super) struct AppResourceAcceptedSettlementPermit {
    authenticated_scope: AuthenticatedAppScope,
    identity: AppResourceTreeIdentity,
    node_id: AppReference,
    reservation_id: AppReference,
}

impl AppResourceAcceptedSettlementPermit {
    fn from_runtime_owners(
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: &AppResourceOperationDispatchPermit,
        binding: &AppRunBinding,
    ) -> Result<Self, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, binding)?;
        if dispatch_permit.identity != root_lease.identity
            || dispatch_permit.identity.budget_ledger_ref != binding.budget_ledger_ref
            || authenticated_scope.scope() != &binding.scope
        {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "accepted settlement permit",
                identity: dispatch_permit.reservation_id.to_string(),
            });
        }
        Ok(Self {
            authenticated_scope: authenticated_scope.clone(),
            identity: root_lease.identity.clone(),
            node_id: dispatch_permit.node_id.clone(),
            reservation_id: dispatch_permit.reservation_id.clone(),
        })
    }

    pub(super) fn authenticated_scope(&self) -> &AuthenticatedAppScope {
        &self.authenticated_scope
    }
}

impl AppResourceOperationDispatchPermit {
    pub fn identity(&self) -> &AppResourceTreeIdentity {
        &self.identity
    }

    pub fn reservation_id(&self) -> &AppReference {
        &self.reservation_id
    }

    pub fn operation_key(&self) -> &AppReference {
        &self.operation_key
    }

    pub fn journal_revision(&self) -> u64 {
        self.journal_revision
    }

    /// Final provider-I/O fence. Call this after every engagement/trust await
    /// and immediately before the physical provider or tool invocation. The
    /// move-only permit remains consumable by settlement after expiry so
    /// already-incurred usage can never be stranded outside the ledger.
    pub fn ensure_live_for_io(
        &self,
        current_elapsed_ms: u64,
    ) -> Result<(), AppResourceAuthorityError> {
        if current_elapsed_ms >= self.expires_at_elapsed_ms {
            return Err(AppResourceAuthorityError::DispatchPermitExpired {
                expires_at_elapsed_ms: self.expires_at_elapsed_ms,
                observed_at_elapsed_ms: current_elapsed_ms,
            });
        }
        Ok(())
    }
}

/// Runtime-only evidence written at the final Apps effect boundary. It is
/// deliberately non-Serde: only the common effect kernel can supply the
/// digest, and only the resource owner can bind it to the held reservation.
pub(crate) struct AppResourceEffectDispatchStart {
    effect_binding_digest: AppDigest,
    at_elapsed_ms: u64,
}

impl AppResourceEffectDispatchStart {
    pub(crate) fn from_effect_kernel(effect_binding_digest: AppDigest, at_elapsed_ms: u64) -> Self {
        Self {
            effect_binding_digest,
            at_elapsed_ms,
        }
    }
}

/// Move-only result emitted by a trusted in-process runtime after it has
/// dispatched an operation but deterministically proves that the operation
/// produced no external effect and consumed no additive provider/tool
/// quantity. The exact bounded in-process active interval is still charged.
/// This is a committed no-effect outcome, not `ProvenUnspent`: dispatch did
/// begin, so the pre-I/O and crash-absence proofs are intentionally
/// inapplicable.
pub struct AppResourceSafeLocalNoEffect {
    dispatch_permit: AppResourceOperationDispatchPermit,
    observation_id: AppReference,
    started_at_elapsed_ms: u64,
    at_elapsed_ms: u64,
}

impl fmt::Debug for AppResourceSafeLocalNoEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppResourceSafeLocalNoEffect")
            .field("verified", &true)
            .finish_non_exhaustive()
    }
}

impl AppResourceSafeLocalNoEffect {
    /// The caller must be the typed safe-local runtime adapter. Generic tool,
    /// process, browser, network and provider errors cannot use this seam.
    pub fn from_dispatch_owner(
        dispatch_permit: AppResourceOperationDispatchPermit,
        observation_id: AppReference,
        started_at_elapsed_ms: u64,
        at_elapsed_ms: u64,
    ) -> Result<Self, AppResourceAuthorityError> {
        if started_at_elapsed_ms < dispatch_permit.created_at_elapsed_ms
            || started_at_elapsed_ms > at_elapsed_ms
            || at_elapsed_ms > dispatch_permit.expires_at_elapsed_ms
        {
            return Err(AppResourceAuthorityError::InvalidObservation(
                "safe-local active interval is outside its reservation",
            ));
        }
        Ok(Self {
            dispatch_permit,
            observation_id,
            started_at_elapsed_ms,
            at_elapsed_ms,
        })
    }
}

#[derive(Debug)]
pub struct AppResourceReservationReceipt {
    pub mutation: AppResourceMutationReceipt,
    dispatch_permit: Option<AppResourceOperationDispatchPermit>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceUsageProjection {
    pub schema_version: u16,
    pub installation_id: super::models::AppInstallationId,
    pub installation_generation: u64,
    pub period_ref: AppReference,
    pub authority_revision: AppRevision,
    pub committed_tokens: u64,
    pub outstanding_tokens: u64,
    pub committed_cost_microusd: u64,
    pub outstanding_cost_microusd: u64,
    pub background_starts: u64,
    pub foreground_runs: u16,
    pub background_runs: u16,
    pub admissions_closed: bool,
    pub projected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppResourceMaintenanceOutcome {
    Created,
    AlreadyPresent,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourcePeriodRolloverReceipt {
    pub outcome: AppResourceMaintenanceOutcome,
    pub previous_revision: AppRevision,
    pub current_revision: AppRevision,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceRetentionReceipt {
    pub retired_trees: u16,
    pub has_more: bool,
}

const MAX_RESOURCE_MAINTENANCE_BATCH_ITEMS: u16 = 64;

/// Independent keyset cursors for the two bounded maintenance lanes. They are
/// exact canonical keys rather than offsets, so concurrent inserts cannot
/// cause a scan to grow or skip already-returned work.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceMaintenanceCursor {
    pub after_budget_ledger_ref: Option<AppReference>,
    pub after_period_ref: Option<AppReference>,
    pub cleanup_exhausted: bool,
    pub periods_exhausted: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceMaintenanceBatchRequest {
    pub cursor: AppResourceMaintenanceCursor,
    pub max_cleanup_candidates: u16,
    pub max_periods: u16,
}

impl AppResourceMaintenanceBatchRequest {
    pub fn from_driver(
        cursor: AppResourceMaintenanceCursor,
        max_cleanup_candidates: u16,
        max_periods: u16,
    ) -> Result<Self, AppResourceAuthorityError> {
        if max_cleanup_candidates == 0
            || max_cleanup_candidates > MAX_RESOURCE_MAINTENANCE_BATCH_ITEMS
            || max_periods == 0
            || max_periods > MAX_RESOURCE_MAINTENANCE_BATCH_ITEMS
        {
            return Err(AppResourceAuthorityError::InvalidMaintenance(
                "maintenance page sizes must be between one and 64",
            ));
        }
        if (cursor.cleanup_exhausted && cursor.after_budget_ledger_ref.is_some())
            || (cursor.periods_exhausted && cursor.after_period_ref.is_some())
        {
            return Err(AppResourceAuthorityError::InvalidMaintenance(
                "an exhausted maintenance lane cannot carry a continuation key",
            ));
        }
        Ok(Self {
            cursor,
            max_cleanup_candidates,
            max_periods,
        })
    }
}

/// A non-terminal canonical tree which the workflow owner may correlate with
/// its cleanup-pending sidecar. Presence here is not proof of a crash and never
/// releases a reservation on its own.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceCleanupCandidate {
    pub identity: AppResourceTreeIdentity,
    pub journal_revision: u64,
    pub updated_at: DateTime<Utc>,
}

/// Bounded installation-period work. The driver may invoke the existing exact
/// rollover/projection/retention APIs; these booleans are hints derived from
/// canonical authority rows and never grant dispatch authority.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourcePeriodMaintenanceCandidate {
    pub period_ref: AppReference,
    pub revision: AppRevision,
    pub rollover_due: bool,
    pub projection_stale: bool,
    pub retention_due: bool,
    pub has_terminal_trees: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceMaintenanceBatch {
    pub cleanup_candidates: Vec<AppResourceCleanupCandidate>,
    pub periods: Vec<AppResourcePeriodMaintenanceCandidate>,
    pub next_cursor: AppResourceMaintenanceCursor,
    pub cleanup_has_more: bool,
    pub periods_have_more: bool,
}

impl AppResourceReservationReceipt {
    pub fn into_dispatch_permit(
        mut self,
    ) -> Result<AppResourceOperationDispatchPermit, AppResourceAuthorityError> {
        self.dispatch_permit
            .take()
            .ok_or(AppResourceAuthorityError::DispatchPermitUnavailable)
    }
}

#[derive(Debug, Error)]
pub enum AppResourceAuthorityError {
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Contract(#[from] AppResourceContractError),
    #[error(transparent)]
    AppContract(#[from] AppContractError),
    #[error(transparent)]
    Package(#[from] AppPackageStagingError),
    #[error("failed to access the canonical app resource SQLite authority: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("failed to encode canonical app resource state: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("invalid current app resource-admission snapshot: {0}")]
    InvalidAdmissionSnapshot(&'static str),
    #[error("current app authority no longer matches the durable registry: {0}")]
    StaleRegistryAuthority(&'static str),
    #[error("app resource authority identity conflict for {entity} `{identity}`")]
    IdentityConflict {
        entity: &'static str,
        identity: String,
    },
    #[error("corrupt canonical app resource authority: {0}")]
    CorruptAuthority(String),
    #[error("app resource authority integer exceeds SQLite range: {0}")]
    IntegerRange(&'static str),
    #[error("app resource tree has been terminally retired and cannot dispatch again")]
    RetiredTree,
    #[error(
        "app resource journal compare-and-swap failed: expected revision {expected}, current \
         revision {actual}"
    )]
    JournalRevisionConflict { expected: u64, actual: u64 },
    #[error(
        "app resource installation-period revision regressed: minimum {minimum}, current {actual}"
    )]
    PeriodRevisionConflict { minimum: u64, actual: u64 },
    #[error("app resource installation period is closed to new admissions")]
    PeriodClosed,
    #[error("invalid canonical app resource observation: {0}")]
    InvalidObservation(&'static str),
    #[error("invalid committed app resource recovery proof: {0}")]
    InvalidCommittedRecovery(&'static str),
    #[error("durable reservation replay cannot mint another dispatch permit")]
    DispatchPermitUnavailable,
    #[error(
        "app resource dispatch permit expired at elapsed millisecond {expires_at_elapsed_ms} \
         before provider I/O at {observed_at_elapsed_ms}"
    )]
    DispatchPermitExpired {
        expires_at_elapsed_ms: u64,
        observed_at_elapsed_ms: u64,
    },
    #[error("invalid app resource retention request: {0}")]
    InvalidRetention(&'static str),
    #[error("invalid app resource maintenance request: {0}")]
    InvalidMaintenance(&'static str),
    #[error("invalid accepted app resource cleanup authority: {0}")]
    InvalidCleanupAuthority(&'static str),
    #[error("the accepted app resource tree is already terminally settled")]
    CleanupAlreadyComplete,
    #[error("accepted app resource cleanup still has {count} unreconciled reservation(s)")]
    CleanupReservationsPending { count: usize },
    #[error("app resource usage projection is already newer than this period snapshot")]
    ProjectionSuperseded,
    #[error("app resource scheduler has no capacity for the requested execution lane")]
    SchedulerCapacityUnavailable,
    #[error("the same app resource root is already active in this runtime")]
    RootRuntimeAlreadyActive,
    #[error("app resource runtime configuration is invalid: {0}")]
    InvalidRuntimeConfiguration(&'static str),
    #[error("the exact immutable app package is unavailable for resource admission")]
    PackageUnavailable,
}

/// Process-owned adapter between Phase-4A workflow dispatch and the single
/// durable resource authority. Clones share the same scheduler semaphores and
/// live counters; the application state must construct this once and clone it
/// rather than creating one gate per request.
#[derive(Clone)]
pub struct AppResourceRuntimeCoordinator {
    authority: AppResourceAuthorityService,
    package_stager: AppPackageStager,
    scheduler_capacity: u16,
    foreground_reserved_slots: u16,
    total_slots: Arc<Semaphore>,
    background_slots: Arc<Semaphore>,
    scheduler_counts: Arc<Mutex<AppResourceSchedulerCounts>>,
}

impl std::fmt::Debug for AppResourceRuntimeCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppResourceRuntimeCoordinator")
            .field("scheduler_capacity", &self.scheduler_capacity)
            .field("foreground_reserved_slots", &self.foreground_reserved_slots)
            .field(
                "available_total_slots",
                &self.total_slots.available_permits(),
            )
            .field(
                "available_background_slots",
                &self.background_slots.available_permits(),
            )
            .finish_non_exhaustive()
    }
}

impl AppResourceRuntimeCoordinator {
    /// Return the exact bounded accepted journal after checking its durable
    /// identity against the sealed run binding. This is read-only accounting
    /// evidence for native participant settlement and the Artifact-owned
    /// `agent_as_tool` carrier; it cannot mint a reservation, dispatch permit
    /// or cleanup lease.
    pub(crate) async fn accepted_journal(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: &AppRunBinding,
        now: DateTime<Utc>,
    ) -> Result<AppResourceJournal, AppResourceAuthorityError> {
        let journal = self
            .authority
            .journal(authenticated_scope, &binding.budget_ledger_ref, now)
            .await?
            .ok_or(AppResourceAuthorityError::InvalidCleanupAuthority(
                "accepted resource tree is unavailable",
            ))?;
        if journal.identity.scope != binding.scope
            || journal.identity.installation_id != binding.installation_id
            || journal.identity.package_revision_ref != binding.package_revision_ref
            || journal.identity.schema_revision != binding.schema_revision
            || journal.identity.authority_digest != binding.authority_digest
            || journal.identity.behavior_resource_identity != binding.behavior_resource_identity
            || journal.identity.root_execution_id != binding.execution_id
            || journal.identity.budget_ledger_ref != binding.budget_ledger_ref
        {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "accepted resource tree identity changed",
            ));
        }
        Ok(journal)
    }

    /// Read the current revision of one exact accepted tree without requiring
    /// its grant to remain dispatch-eligible. This is metadata-only evidence
    /// used to bind a terminal/cancel/crash cleanup proof; it cannot mint a
    /// cleanup or dispatch lease by itself.
    pub async fn accepted_cleanup_journal_revision(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: &AppRunBinding,
        now: DateTime<Utc>,
    ) -> Result<u64, AppResourceAuthorityError> {
        let journal = self
            .authority
            .journal(authenticated_scope, &binding.budget_ledger_ref, now)
            .await?
            .ok_or(AppResourceAuthorityError::InvalidCleanupAuthority(
                "accepted cleanup tree is unavailable",
            ))?;
        if journal.identity.scope != binding.scope
            || journal.identity.installation_id != binding.installation_id
            || journal.identity.package_revision_ref != binding.package_revision_ref
            || journal.identity.schema_revision != binding.schema_revision
            || journal.identity.authority_digest != binding.authority_digest
            || journal.identity.behavior_resource_identity != binding.behavior_resource_identity
            || journal.identity.root_execution_id != binding.execution_id
            || journal.identity.budget_ledger_ref != binding.budget_ledger_ref
        {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "accepted cleanup tree identity changed",
            ));
        }
        u64::try_from(journal.events.len())
            .map_err(|_| AppResourceAuthorityError::IntegerRange("journal_revision"))
    }

    /// Only the workflow owner may use absence to finish a failed, pristine
    /// pre-admission shell. Live trees cannot disappear without an immutable
    /// retirement tombstone; either identity match rules out this proof.
    pub(crate) async fn root_was_never_admitted(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: &AppRunBinding,
        now: DateTime<Utc>,
    ) -> Result<bool, AppResourceAuthorityError> {
        if authenticated_scope.scope() != &binding.scope {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "pre-admission cleanup scope changed",
            ));
        }
        let binding = binding.clone();
        self.authority
            .registry
            .execute_scoped_typed_read(authenticated_scope, &now, move |connection, _| {
                let exists: bool = connection.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM app_resource_trees
                         WHERE budget_ledger_ref=?1 OR root_execution_id=?2
                        UNION ALL
                        SELECT 1 FROM app_resource_retired_trees
                         WHERE budget_ledger_ref=?1 OR root_execution_id=?2
                    )",
                    params![
                        binding.budget_ledger_ref.as_str(),
                        binding.execution_id.as_str()
                    ],
                    |row| row.get(0),
                )?;
                Ok::<bool, AppResourceAuthorityError>(!exists)
            })
            .await?
            .ok_or(AppResourceAuthorityError::InvalidCleanupAuthority(
                "pre-admission cleanup registry is unavailable",
            ))
    }

    /// Marker-independent startup/admission probe. The workflow sidecar is a
    /// durable scheduling hint, not the source of truth for whether the
    /// canonical resource tree still owns a Held/Started reservation.
    pub(crate) async fn accepted_pending_reservation_count(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: &AppRunBinding,
        max_reservations: u32,
        now: DateTime<Utc>,
    ) -> Result<usize, AppResourceAuthorityError> {
        let journal = self
            .authority
            .journal(authenticated_scope, &binding.budget_ledger_ref, now)
            .await?
            .ok_or(AppResourceAuthorityError::InvalidCleanupAuthority(
                "accepted cleanup tree is unavailable",
            ))?;
        if journal.identity.scope != binding.scope
            || journal.identity.installation_id != binding.installation_id
            || journal.identity.package_revision_ref != binding.package_revision_ref
            || journal.identity.schema_revision != binding.schema_revision
            || journal.identity.authority_digest != binding.authority_digest
            || journal.identity.behavior_resource_identity != binding.behavior_resource_identity
            || journal.identity.root_execution_id != binding.execution_id
            || journal.identity.budget_ledger_ref != binding.budget_ledger_ref
        {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "accepted cleanup tree identity changed",
            ));
        }
        Ok(pending_reservations_for_cleanup(&journal, max_reservations)?.len())
    }

    /// Read-after-write recovery for an ambiguous durable dispatch-start
    /// response. This cannot mint dispatch authority; it only confirms that
    /// the exact effect identity is already present and returns the journal
    /// revision a later canonical settlement must fence against.
    pub(crate) async fn observe_effect_dispatch_checkpoint(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: &AppRunBinding,
        reservation_id: &AppReference,
        effect_binding_digest: &AppDigest,
        now: DateTime<Utc>,
    ) -> Result<AppResourceEffectDispatchCheckpoint, AppResourceAuthorityError> {
        let journal = self
            .authority
            .journal(authenticated_scope, &binding.budget_ledger_ref, now)
            .await?
            .ok_or(AppResourceAuthorityError::InvalidCleanupAuthority(
                "effect dispatch tree is unavailable",
            ))?;
        if journal.identity.scope != binding.scope
            || journal.identity.installation_id != binding.installation_id
            || journal.identity.package_revision_ref != binding.package_revision_ref
            || journal.identity.schema_revision != binding.schema_revision
            || journal.identity.authority_digest != binding.authority_digest
            || journal.identity.behavior_resource_identity != binding.behavior_resource_identity
            || journal.identity.root_execution_id != binding.execution_id
            || journal.identity.budget_ledger_ref != binding.budget_ledger_ref
        {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "effect dispatch tree identity changed",
            ));
        }
        let mut reserved = false;
        let mut started = false;
        let mut aborted = false;
        let mut settled = false;
        for event in &journal.events {
            match event {
                AppResourceJournalEvent::Reserved {
                    reservation_id: observed_reservation,
                    ..
                } if observed_reservation == reservation_id => {
                    if reserved {
                        return Err(AppResourceAuthorityError::IdentityConflict {
                            entity: "effect dispatch checkpoint",
                            identity: reservation_id.to_string(),
                        });
                    }
                    reserved = true;
                },
                AppResourceJournalEvent::EffectDispatchStarted {
                    reservation_id: observed_reservation,
                    effect_binding_digest: observed_digest,
                    ..
                } if observed_reservation == reservation_id => {
                    if observed_digest != effect_binding_digest || started || !reserved {
                        return Err(AppResourceAuthorityError::IdentityConflict {
                            entity: "effect dispatch checkpoint",
                            identity: reservation_id.to_string(),
                        });
                    }
                    started = true;
                },
                AppResourceJournalEvent::EffectDispatchAbortedBeforeIo {
                    reservation_id: observed_reservation,
                    effect_binding_digest: observed_digest,
                    ..
                } if observed_reservation == reservation_id => {
                    if observed_digest != effect_binding_digest || !started || aborted || settled {
                        return Err(AppResourceAuthorityError::IdentityConflict {
                            entity: "effect dispatch checkpoint",
                            identity: reservation_id.to_string(),
                        });
                    }
                    aborted = true;
                },
                AppResourceJournalEvent::Settled {
                    reservation_id: observed_reservation,
                    effect_binding_digest: observed_digest,
                    ..
                } if observed_reservation == reservation_id => {
                    if settled
                        || aborted
                        || (started && observed_digest.as_ref() != Some(effect_binding_digest))
                    {
                        return Err(AppResourceAuthorityError::IdentityConflict {
                            entity: "effect dispatch checkpoint",
                            identity: reservation_id.to_string(),
                        });
                    }
                    settled = true;
                },
                _ => {},
            }
        }
        if !reserved {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "effect dispatch reservation",
                identity: reservation_id.to_string(),
            });
        }
        let journal_revision = u64::try_from(journal.events.len())
            .map_err(|_| AppResourceAuthorityError::IntegerRange("journal_revision"))?;
        Ok(if settled {
            AppResourceEffectDispatchCheckpoint::Settled
        } else if aborted {
            AppResourceEffectDispatchCheckpoint::AbortedBeforeIo {
                journal_revision,
                admitted_active_elapsed_ms: project_admitted_active_elapsed_ms(&journal)?,
            }
        } else if started {
            AppResourceEffectDispatchCheckpoint::Started { journal_revision }
        } else {
            AppResourceEffectDispatchCheckpoint::Held { journal_revision }
        })
    }

    /// Reconstruct a settlement-only owner for a sealed provider completion.
    /// Exact reservation/action identity and the dispatch-start digest are
    /// loaded from the canonical journal; terminal or merely-Held rows are not
    /// recoverable through this path.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn recover_effect_completion_permit(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: &AppRunBinding,
        node_id: &AppReference,
        reservation_id: &AppReference,
        operation_key: &AppReference,
        effect_binding_digest: &AppDigest,
        result_digest: &AppDigest,
        result_bytes: u64,
        sealed_dispatch_started_at_elapsed_ms: u64,
        provider_completed_at_elapsed_ms: u64,
        now: DateTime<Utc>,
    ) -> Result<AppResourceEffectCompletionRecovery, AppResourceAuthorityError> {
        let journal = self
            .authority
            .journal(authenticated_scope, &binding.budget_ledger_ref, now)
            .await?
            .ok_or(AppResourceAuthorityError::InvalidCleanupAuthority(
                "effect completion tree is unavailable",
            ))?;
        if journal.identity.scope != binding.scope
            || journal.identity.installation_id != binding.installation_id
            || journal.identity.package_revision_ref != binding.package_revision_ref
            || journal.identity.schema_revision != binding.schema_revision
            || journal.identity.authority_digest != binding.authority_digest
            || journal.identity.behavior_resource_identity != binding.behavior_resource_identity
            || journal.identity.root_execution_id != binding.execution_id
            || journal.identity.budget_ledger_ref != binding.budget_ledger_ref
        {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "effect completion tree identity changed",
            ));
        }
        let mut reserved = None;
        let mut started_at_elapsed_ms = None;
        let mut aborted = false;
        let mut settled = false;
        for event in &journal.events {
            match event {
                AppResourceJournalEvent::Reserved {
                    node_id: observed_node,
                    reservation_id: observed_reservation,
                    operation_key: observed_operation,
                    requested,
                    capability_requests,
                    at_elapsed_ms,
                    expires_at_elapsed_ms,
                    ..
                } if observed_reservation == reservation_id => {
                    if reserved.is_some()
                        || observed_node != node_id
                        || observed_operation != operation_key
                    {
                        return Err(AppResourceAuthorityError::IdentityConflict {
                            entity: "effect completion reservation",
                            identity: reservation_id.to_string(),
                        });
                    }
                    reserved = Some((
                        *requested,
                        capability_requests.clone(),
                        *at_elapsed_ms,
                        *expires_at_elapsed_ms,
                    ));
                },
                AppResourceJournalEvent::EffectDispatchStarted {
                    reservation_id: observed_reservation,
                    effect_binding_digest: observed_digest,
                    at_elapsed_ms,
                    ..
                } if observed_reservation == reservation_id => {
                    if started_at_elapsed_ms.is_some() || observed_digest != effect_binding_digest {
                        return Err(AppResourceAuthorityError::IdentityConflict {
                            entity: "effect completion dispatch",
                            identity: reservation_id.to_string(),
                        });
                    }
                    started_at_elapsed_ms = Some(*at_elapsed_ms);
                },
                AppResourceJournalEvent::EffectDispatchAbortedBeforeIo {
                    reservation_id: observed_reservation,
                    ..
                } if observed_reservation == reservation_id => aborted = true,
                AppResourceJournalEvent::Settled {
                    reservation_id: observed_reservation,
                    outcome,
                    effect_binding_digest: observed_effect,
                    effect_result: observed_result,
                    ..
                } if observed_reservation == reservation_id => {
                    if *outcome != AppResourceSettlementOutcome::Committed
                        || observed_effect.as_ref() != Some(effect_binding_digest)
                        || observed_result.as_ref().is_none_or(|result| {
                            &result.result_digest != result_digest
                                || result.result_bytes != result_bytes
                        })
                    {
                        return Err(AppResourceAuthorityError::IdentityConflict {
                            entity: "effect completion settlement",
                            identity: reservation_id.to_string(),
                        });
                    }
                    settled = true;
                },
                _ => {},
            }
        }
        let (requested, capability_requests, created_at_elapsed_ms, expires_at_elapsed_ms) =
            reserved.ok_or_else(|| AppResourceAuthorityError::IdentityConflict {
                entity: "effect completion reservation",
                identity: reservation_id.to_string(),
            })?;
        let Some(dispatch_started_at_elapsed_ms) = started_at_elapsed_ms else {
            return Err(AppResourceAuthorityError::DispatchPermitUnavailable);
        };
        if dispatch_started_at_elapsed_ms != sealed_dispatch_started_at_elapsed_ms
            || provider_completed_at_elapsed_ms < dispatch_started_at_elapsed_ms
            || provider_completed_at_elapsed_ms > expires_at_elapsed_ms
        {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "effect completion timing",
                identity: reservation_id.to_string(),
            });
        }
        if aborted {
            return Err(AppResourceAuthorityError::DispatchPermitUnavailable);
        }
        let journal_revision = u64::try_from(journal.events.len())
            .map_err(|_| AppResourceAuthorityError::IntegerRange("journal_revision"))?;
        if settled {
            return Ok(AppResourceEffectCompletionRecovery::AlreadySettled { journal_revision });
        }
        Ok(AppResourceEffectCompletionRecovery::NeedsSettlement {
            permit: AppResourceEffectCompletionPermit {
                dispatch_permit: AppResourceOperationDispatchPermit {
                    identity: journal.identity,
                    node_id: node_id.clone(),
                    reservation_id: reservation_id.clone(),
                    operation_key: operation_key.clone(),
                    journal_revision,
                    created_at_elapsed_ms,
                    expires_at_elapsed_ms,
                },
                effect_binding_digest: effect_binding_digest.clone(),
                requested,
                capability_requests,
                dispatch_started_at_elapsed_ms,
                provider_completed_at_elapsed_ms,
            },
            journal_revision,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn settle_recovered_effect_completion(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        completion_permit: &AppResourceEffectCompletionPermit,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        observation: AppResourceSettlementObservation,
        authoritative_elapsed_ms: u64,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        if observation.effect_binding_digest.as_ref()
            != Some(&completion_permit.effect_binding_digest)
        {
            return Err(AppResourceAuthorityError::InvalidObservation(
                "recovered effect completion digest changed",
            ));
        }
        self.settle_accepted_operation(
            authenticated_scope,
            root_lease,
            &completion_permit.dispatch_permit,
            resolved,
            binding,
            policy,
            fence,
            observation,
            authoritative_elapsed_ms,
            now,
        )
        .await
    }

    /// Construct the process-owned scheduler and durable adapter from the
    /// exact server policy used by every later decision. Keeping this as the
    /// config adoption seam prevents scheduler capacity from being copied
    /// independently from the enforcement policy.
    pub fn from_policy(
        workspace: ArtifactV2Workspace,
        policy: AppResourceEnforcementPolicy,
    ) -> Result<Self, AppResourceAuthorityError> {
        let policy = policy.validate_server_configuration()?;
        Self::new(
            workspace,
            policy.scheduler_capacity,
            policy.foreground_reserved_slots,
        )
    }

    fn new(
        workspace: ArtifactV2Workspace,
        scheduler_capacity: u16,
        foreground_reserved_slots: u16,
    ) -> Result<Self, AppResourceAuthorityError> {
        if scheduler_capacity == 0 || foreground_reserved_slots > scheduler_capacity {
            return Err(AppResourceAuthorityError::InvalidRuntimeConfiguration(
                "scheduler capacity must be positive and cover the foreground reserve",
            ));
        }
        let background_capacity = scheduler_capacity - foreground_reserved_slots;
        let registry = AppRegistryService::new(workspace.clone());
        Ok(Self {
            authority: AppResourceAuthorityService::new(registry),
            package_stager: AppPackageStager::new(workspace),
            scheduler_capacity,
            foreground_reserved_slots,
            total_slots: Arc::new(Semaphore::new(usize::from(scheduler_capacity))),
            background_slots: Arc::new(Semaphore::new(usize::from(background_capacity))),
            scheduler_counts: Arc::new(Mutex::new(AppResourceSchedulerCounts::default())),
        })
    }

    /// Phase-4A adoption seam for `AppWorkflowService::tool_for_execution`.
    /// Package loading/measurement and scheduler admission happen here before
    /// the root journal insert. The returned receipt is move-only; only its
    /// dispatch lease may accompany the launched or resumed orchestrator run.
    #[allow(clippy::too_many_arguments)]
    pub async fn admit_workflow_root(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        binding: &AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        lane: AppResourceExecutionLane,
        root_node_id: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppResourceRootAdmissionReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let package_revision = self
            .authority
            .registry
            .package_revision(authenticated_scope, &resolved.package_revision_ref, now)
            .await?
            .ok_or(AppResourceAuthorityError::PackageUnavailable)?;
        let staged = self
            .package_stager
            .load_staged_package(
                authenticated_scope,
                package_revision.content_digest.clone(),
                now,
            )
            .await?;
        if staged.storage_digest() != &package_revision.content_digest {
            return Err(AppResourceAuthorityError::PackageUnavailable);
        }
        let package = AppResourcePackageMeasurement::from_staged_package(
            resolved.package_revision_ref.clone(),
            &staged,
        )?;
        let scheduler_root_key = scheduler_root_key(authenticated_scope, binding)?;
        let scheduler_guard = self.acquire_scheduler_guard(lane, scheduler_root_key)?;
        let (period_ref, period_ends_at_elapsed_ms) = self
            .root_admission_period(authenticated_scope, binding, now)
            .await?;
        let snapshot = AppResourceAdmissionSnapshot {
            period_ref,
            period_ends_at_elapsed_ms,
            package_revision_ref: package.package_revision_ref,
            package_bytes: package.package_bytes,
            scheduler_foreground_runs_excluding_root: scheduler_guard
                .foreground_runs_excluding_root,
            scheduler_background_runs_excluding_root: scheduler_guard
                .background_runs_excluding_root,
            observed_at: now,
            scheduler_guard,
        };
        self.authority
            .admit_root(
                authenticated_scope,
                resolved,
                binding,
                policy,
                snapshot,
                lane,
                root_node_id,
                now,
            )
            .await
    }

    /// Reacquire exclusive ownership of an already accepted tree solely for
    /// terminal closure and crash reconciliation. Current lifecycle status and
    /// active grant pointers are intentionally irrelevant; the immutable tree,
    /// historical grant/schema rows and canonical accepted-authority digest are
    /// checked instead. The returned lease cannot authorize new work.
    pub async fn reacquire_accepted_workflow_root_for_cleanup(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: AppRunBinding,
        accepted: AppResourceAcceptedCleanupSnapshot,
        proof: AppResourceAcceptedCleanupProof,
        policy: AppResourceEnforcementPolicy,
        now: DateTime<Utc>,
    ) -> Result<AppResourceAcceptedCleanupReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        authenticated_scope
            .ensure_live_at(&now)
            .map_err(AppRegistryError::from)?;
        let lane = self
            .accepted_cleanup_root_lane(authenticated_scope, &binding, now)
            .await?;
        let scheduler_root_key = scheduler_root_key(authenticated_scope, &binding)?;
        let scheduler_guard = self.acquire_scheduler_guard(lane, scheduler_root_key)?;
        let authenticated_scope = authenticated_scope.clone();
        let operation_scope = authenticated_scope.clone();
        self.authority
            .registry
            .execute_scoped_typed_write(&authenticated_scope, &now, move |connection, scope| {
                reacquire_accepted_cleanup_blocking(
                    connection,
                    scope,
                    &operation_scope,
                    binding,
                    accepted,
                    proof,
                    policy,
                    scheduler_guard,
                    now,
                )
            })
            .await
    }

    async fn accepted_cleanup_root_lane(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: &AppRunBinding,
        now: DateTime<Utc>,
    ) -> Result<AppResourceExecutionLane, AppResourceAuthorityError> {
        let binding = binding.clone();
        self.authority
            .registry
            .execute_scoped_typed_read(authenticated_scope, &now, move |connection, scope| {
                accepted_cleanup_root_lane_blocking(connection, scope, &binding)
            })
            .await?
            .flatten()
            .ok_or(AppResourceAuthorityError::StaleRegistryAuthority(
                "accepted cleanup root",
            ))
    }

    /// Append at most one close event chosen from the canonical leaf-first
    /// cleanup plan. Repeated calls are bounded and crash-safe: a restart
    /// derives the remaining plan from the append-only journal.
    pub async fn close_next_accepted_execution_node(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        cleanup: &mut AppResourceAcceptedCleanupLease,
        now: DateTime<Utc>,
    ) -> Result<Option<AppResourceMutationReceipt>, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(cleanup.policy)?;
        let Some(node_id) = cleanup.open_nodes_leaf_first.front().cloned() else {
            return Ok(None);
        };
        let node_pending_reservations = cleanup
            .pending_reservations
            .iter()
            .filter(|reservation| reservation.node_id == node_id)
            .count();
        let final_node_with_other_pending =
            cleanup.open_nodes_leaf_first.len() == 1 && cleanup.has_pending_reservations();
        if node_pending_reservations != 0 || final_node_with_other_pending {
            return Err(AppResourceAuthorityError::CleanupReservationsPending {
                count: if final_node_with_other_pending {
                    cleanup.pending_reservations.len()
                } else {
                    node_pending_reservations
                },
            });
        }
        let elapsed = cleanup.clamp_elapsed(now);
        let authority = self.accepted_cleanup_mutation_authority(cleanup, now, elapsed)?;
        let receipt = self
            .authority
            .close_accepted_node(
                authenticated_scope,
                authority,
                AppResourceNodeClose::from_execution_owner(node_id, elapsed),
                now,
            )
            .await?;
        cleanup.open_nodes_leaf_first.pop_front();
        update_cleanup_fence(cleanup, &receipt);
        let closed_final_node = cleanup.open_nodes_leaf_first.is_empty();
        if closed_final_node
            && cleanup.cleanup_scope.requires_terminal_settlement()
            && !receipt.state.terminally_settled
        {
            return Err(AppResourceAuthorityError::CorruptAuthority(
                "accepted cleanup closed its final node without terminal resource settlement"
                    .to_owned(),
            ));
        }
        Ok(Some(receipt))
    }

    /// Conservatively account an unverifiable post-I/O operation after the
    /// canonical crash owner has claimed this exact accepted tree. The full
    /// reserved quantity and per-capability ceilings are committed. Active
    /// time is conservatively bounded to the journal-owned reservation window;
    /// it is never inferred from analytics or caller-supplied timestamps.
    /// Terminal/cancel cleanup leases cannot use this crash-only escape hatch.
    pub async fn conservatively_charge_accepted_post_io_reservation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        cleanup: &mut AppResourceAcceptedCleanupLease,
        reservation_id: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        let proof = conservative_post_io_charge_from_cleanup(cleanup, reservation_id, now)?;
        let settled_reservation_id = proof.reservation_id.clone();
        let authority =
            self.accepted_cleanup_mutation_authority(cleanup, now, proof.at_elapsed_ms)?;
        let receipt = self
            .authority
            .reconcile_conservative_post_io_charge(authenticated_scope, authority, proof, now)
            .await?;
        update_cleanup_after_reservation_reconciliation(
            cleanup,
            &receipt,
            &settled_reservation_id,
        )?;
        Ok(receipt)
    }

    /// Settle a control-plane-sealed provider completion after process-local
    /// root ownership was lost. The cleanup lease cannot dispatch; exact
    /// requested/capability quantities and dispatch timing come from the
    /// canonical journal, while the sealed completion contributes only the
    /// bounded raw-result identity and completion timestamp.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn settle_effect_completion_from_cleanup(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        cleanup: &mut AppResourceAcceptedCleanupLease,
        reservation_id: &AppReference,
        operation_key: &AppReference,
        effect_binding_digest: &AppDigest,
        result_digest: AppDigest,
        result_bytes: u64,
        sealed_dispatch_started_at_elapsed_ms: u64,
        provider_completed_at_elapsed_ms: u64,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        if cleanup.cleanup_reason != AppResourceAcceptedCleanupReason::CrashRecovery {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "effect completion requires crash-recovery ownership",
            ));
        }
        let pending = cleanup
            .pending_reservations
            .iter()
            .find(|pending| &pending.reservation_id == reservation_id)
            .cloned()
            .ok_or_else(|| AppResourceAuthorityError::IdentityConflict {
                entity: "accepted effect completion reservation",
                identity: reservation_id.to_string(),
            })?;
        if &pending.operation_key != operation_key
            || pending.status != AppResourcePendingReservationStatus::OutcomeUncertain
            || pending.effect_binding_digest.as_ref() != Some(effect_binding_digest)
            || pending.effect_dispatch_started_at_elapsed_ms
                != Some(sealed_dispatch_started_at_elapsed_ms)
            || provider_completed_at_elapsed_ms < sealed_dispatch_started_at_elapsed_ms
            || provider_completed_at_elapsed_ms > pending.expires_at_elapsed_ms
        {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "sealed effect completion does not match the accepted journal",
            ));
        }
        let elapsed = cleanup
            .clamp_elapsed(now)
            .max(provider_completed_at_elapsed_ms);
        cleanup.elapsed_high_water_ms = elapsed;
        let active_intervals = (provider_completed_at_elapsed_ms
            > sealed_dispatch_started_at_elapsed_ms)
            .then_some(AppActiveInterval {
                start_elapsed_ms: sealed_dispatch_started_at_elapsed_ms,
                end_elapsed_ms: provider_completed_at_elapsed_ms,
            })
            .into_iter()
            .collect();
        let mut actual = pending.requested;
        actual.payload_bytes = 0;
        let observation_digest = AppDigest::blake3(reservation_id.as_str().as_bytes());
        let observation_id = AppReference::parse(format!(
            "effect-completion-observation:{}",
            observation_digest.as_str().trim_start_matches("blake3:")
        ))?;
        let observation = AppResourceSettlementObservation::from_tool_runtime(
            pending.node_id,
            pending.reservation_id.clone(),
            observation_id,
            actual,
            pending.capability_requests,
            active_intervals,
            elapsed,
        )
        .bind_accepted_effect_unchecked_by_transport(effect_binding_digest.clone())
        .bind_accepted_effect_result_unchecked_by_transport(result_digest, result_bytes);
        let authority = self.accepted_cleanup_mutation_authority(cleanup, now, elapsed)?;
        let receipt = self
            .authority
            .settle_accepted_cleanup_effect_completion(
                authenticated_scope,
                authority,
                observation,
                now,
            )
            .await?;
        update_cleanup_after_reservation_reconciliation(
            cleanup,
            &receipt,
            &pending.reservation_id,
        )?;
        Ok(receipt)
    }

    /// A resumed root remains in the period under which its first durable
    /// admission was evaluated. Deriving today's month for a replay would
    /// reject a legitimate cross-month resume (or, worse, try to give the
    /// existing tree a fresh period deadline). New roots alone derive the
    /// current server-owned UTC period.
    async fn root_admission_period(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: &AppRunBinding,
        now: DateTime<Utc>,
    ) -> Result<(AppReference, u64), AppResourceAuthorityError> {
        let budget_ledger_ref = binding.budget_ledger_ref.clone();
        let installation_id = binding.installation_id.clone();
        let execution_id = binding.execution_id.clone();
        let durable = self
            .authority
            .registry
            .execute_scoped_typed_read(authenticated_scope, &now, move |connection, _scope| {
                connection
                    .query_row(
                        "SELECT period_ref, period_ends_at_elapsed_ms
                               FROM app_resource_trees
                              WHERE budget_ledger_ref = ?1
                                AND installation_id = ?2
                                AND root_execution_id = ?3",
                        params![
                            budget_ledger_ref.as_str(),
                            installation_id.as_str(),
                            execution_id.as_str(),
                        ],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
                    )
                    .optional()
                    .map_err(AppResourceAuthorityError::from)
            })
            .await?
            .flatten();
        let Some((period_ref, period_ends_at_elapsed_ms)) = durable else {
            return canonical_monthly_period(now);
        };
        let period_ends_at_elapsed_ms = period_ends_at_elapsed_ms
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "resource root is missing its immutable period deadline".to_owned(),
                )
            })
            .and_then(|value| from_sql_u64(value, "period_ends_at_elapsed_ms"))?;
        if period_ends_at_elapsed_ms == 0 {
            return Err(AppResourceAuthorityError::CorruptAuthority(
                "resource root has an empty immutable period deadline".to_owned(),
            ));
        }
        Ok((AppReference::parse(period_ref)?, period_ends_at_elapsed_ms))
    }

    /// Phase-4A execution-tree seam. Every child/retry/resume/repair/tool,
    /// browser, synthesis and reflection node is appended beneath the retained
    /// root lease before that branch starts.
    #[allow(clippy::too_many_arguments)]
    pub async fn open_execution_node(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        node: AppResourceNodeAdmission,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority =
            self.mutation_authority(root_lease, resolved, binding, policy, fence, now)?;
        self.authority
            .open_node(authenticated_scope, root_lease, authority, node, now)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn observe_execution_progress(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        progress: AppResourceProgressObservation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority =
            self.mutation_authority(root_lease, resolved, binding, policy, fence, now)?;
        self.authority
            .observe_progress(authenticated_scope, root_lease, authority, progress, now)
            .await
    }

    /// Phase-4A adoption seam for `AppWorkflowService::authorize_action`.
    /// The reservation commits before a move-only provider permit is returned.
    #[allow(clippy::too_many_arguments)]
    pub async fn reserve_operation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        reservation: AppResourceReservationRequest,
        now: DateTime<Utc>,
    ) -> Result<AppResourceReservationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority =
            self.mutation_authority(root_lease, resolved, binding, policy, fence, now)?;
        self.authority
            .reserve(authenticated_scope, root_lease, authority, reservation, now)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn settle_operation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: AppResourceOperationDispatchPermit,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        observation: AppResourceSettlementObservation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority =
            self.accepted_mutation_authority(root_lease, resolved, binding, policy, fence, now)?;
        self.authority
            .settle(
                authenticated_scope,
                root_lease,
                dispatch_permit,
                authority,
                observation,
                now,
            )
            .await
    }

    /// Settlement-only counterpart that remains usable after the short-lived
    /// task credential expires. The caller supplies a monotonic elapsed value
    /// clamped by the retained root owner; the private permit bypasses no
    /// identity, quantity or CAS validation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn settle_accepted_operation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: &AppResourceOperationDispatchPermit,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        observation: AppResourceSettlementObservation,
        authoritative_elapsed_ms: u64,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let settlement_permit = AppResourceAcceptedSettlementPermit::from_runtime_owners(
            authenticated_scope,
            root_lease,
            dispatch_permit,
            &binding,
        )?;
        let authority = self.mutation_authority_with_mode(
            root_lease,
            resolved,
            binding,
            policy,
            fence,
            now,
            true,
            Some(authoritative_elapsed_ms),
        )?;
        self.authority
            .settle_accepted(
                settlement_permit,
                root_lease,
                dispatch_permit,
                authority,
                observation,
                now,
            )
            .await
    }

    /// Persist the exact effect identity immediately before provider I/O. The
    /// dispatch permit remains move-only and is consumed later by settlement;
    /// after this checkpoint, proven-unspent release is rejected by journal
    /// replay even across a process crash.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn start_effect_dispatch(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: &AppResourceOperationDispatchPermit,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        start: AppResourceEffectDispatchStart,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority =
            self.mutation_authority(root_lease, resolved, binding, policy, fence, now)?;
        self.authority
            .start_effect_dispatch(
                authenticated_scope,
                root_lease,
                dispatch_permit,
                authority,
                start,
                now,
            )
            .await
    }

    /// Release a started effect only when the common owner still holds the
    /// original move-only dispatch permit and proves that the provider was not
    /// polled. Exact replay is idempotent; a lost response remains unresolved
    /// and must be recovered from the canonical journal.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn abort_effect_dispatch_before_io(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: &AppResourceOperationDispatchPermit,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        abort_proof: &AppEffectAbortProof,
        at_elapsed_ms: u64,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let settlement_permit = AppResourceAcceptedSettlementPermit::from_runtime_owners(
            authenticated_scope,
            root_lease,
            dispatch_permit,
            &binding,
        )?;
        let authority = self.mutation_authority_with_mode(
            root_lease,
            resolved,
            binding,
            policy,
            fence,
            now,
            true,
            Some(at_elapsed_ms),
        )?;
        self.authority
            .abort_effect_dispatch_before_io_accepted(
                settlement_permit,
                root_lease,
                dispatch_permit,
                authority,
                abort_proof,
                at_elapsed_ms,
                now,
            )
            .await
    }

    /// Consume a dispatched reservation as an exact zero-usage committed
    /// outcome. Only a typed deterministic in-process runtime may mint the
    /// move-only proof; callers must not infer this from an error string or a
    /// provider/tool name.
    #[allow(clippy::too_many_arguments)]
    pub async fn settle_safe_local_no_effect_operation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        proof: AppResourceSafeLocalNoEffect,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority =
            self.accepted_mutation_authority(root_lease, resolved, binding, policy, fence, now)?;
        self.authority
            .settle_safe_local_no_effect(authenticated_scope, root_lease, authority, proof, now)
            .await
    }

    /// Release a persisted reservation at the only runtime boundary that can
    /// prove provider I/O never began. Consuming the move-only dispatch permit
    /// prevents the caller from subsequently using that same reservation.
    #[allow(clippy::too_many_arguments)]
    pub async fn release_pre_io_reservation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: AppResourceOperationDispatchPermit,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        proof: AppResourcePreIoUnspentReconciliation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority =
            self.accepted_mutation_authority(root_lease, resolved, binding, policy, fence, now)?;
        self.authority
            .release_pre_io(
                authenticated_scope,
                root_lease,
                dispatch_permit,
                authority,
                proof,
                now,
            )
            .await
    }

    /// Accepted-closure counterpart for a retained pre-I/O attempt. This
    /// bypasses only credential liveness and consumes no new admission: the
    /// private settlement permit, exact dispatch permit, reconciliation proof,
    /// root identity and CAS fence must all match the original reservation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn release_pre_io_reservation_accepted(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: &AppResourceOperationDispatchPermit,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        proof: &AppResourcePreIoUnspentReconciliation,
        authoritative_elapsed_ms: u64,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let settlement_permit = AppResourceAcceptedSettlementPermit::from_runtime_owners(
            authenticated_scope,
            root_lease,
            dispatch_permit,
            &binding,
        )?;
        let authority = self.mutation_authority_with_mode(
            root_lease,
            resolved,
            binding,
            policy,
            fence,
            now,
            true,
            Some(authoritative_elapsed_ms),
        )?;
        self.authority
            .release_pre_io_accepted(
                settlement_permit,
                root_lease,
                dispatch_permit,
                authority,
                proof,
                now,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn close_execution_node(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        close: AppResourceNodeClose,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority =
            self.accepted_mutation_authority(root_lease, resolved, binding, policy, fence, now)?;
        self.authority
            .close_node(authenticated_scope, root_lease, authority, close, now)
            .await
    }

    /// Crash-only settlement seam. It reconstructs the immutable root
    /// baseline from the canonical tree, not from the failed process, then the
    /// normal journal CAS accepts only trusted proven-unspent evidence.
    #[allow(clippy::too_many_arguments)]
    pub async fn reconcile_crashed_operation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        recovery: AppResourceCrashReconciliation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority = self
            .recovered_mutation_authority(
                authenticated_scope,
                resolved,
                binding,
                policy,
                fence,
                now,
            )
            .await?;
        self.authority
            .reconcile_proven_unspent(authenticated_scope, authority, recovery, now)
            .await
    }

    /// Crash-only committed settlement. The caller must first load and verify
    /// the canonical entity-mutation receipt, then construct the move-only,
    /// non-deserializable proof. No provider permit is reminted and no entity
    /// mutation is replayed.
    #[allow(clippy::too_many_arguments)]
    pub async fn reconcile_committed_operation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        proof: AppResourceCommittedRecovery,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority = self
            .recovered_mutation_authority(
                authenticated_scope,
                resolved,
                binding,
                policy,
                fence,
                now,
            )
            .await?;
        self.authority
            .reconcile_committed(authenticated_scope, authority, proof, now)
            .await
    }

    /// Settle a server-verified read-only terminal intent after a crash. The
    /// proof constructor rejects any command containing mutation operations or
    /// revision expectations, so this cannot disguise an ambiguous store call
    /// as a successful no-op.
    #[allow(clippy::too_many_arguments)]
    pub async fn reconcile_no_effect_committed_operation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        proof: AppResourceNoEffectCommittedRecovery,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority = self
            .recovered_mutation_authority(
                authenticated_scope,
                resolved,
                binding,
                policy,
                fence,
                now,
            )
            .await?;
        self.authority
            .reconcile_no_effect_committed(authenticated_scope, authority, proof, now)
            .await
    }

    /// Release an ambiguous post-crash reservation only after the canonical
    /// entity receipt reader positively proves that the exact mutation
    /// origin/key/batch has no receipt. Lookup errors must never call this API.
    #[allow(clippy::too_many_arguments)]
    pub async fn reconcile_verified_receipt_absence(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        proof: AppResourceReceiptAbsentReconciliation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        self.ensure_scheduler_policy(policy)?;
        let authority = self
            .recovered_mutation_authority(
                authenticated_scope,
                resolved,
                binding,
                policy,
                fence,
                now,
            )
            .await?;
        self.authority
            .reconcile_verified_receipt_absence(authenticated_scope, authority, proof, now)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn recovered_mutation_authority(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationAuthority, AppResourceAuthorityError> {
        let ledger_ref = binding.budget_ledger_ref.clone();
        let installation_id = binding.installation_id.clone();
        let execution_id = binding.execution_id.clone();
        let durable_baseline = self
            .authority
            .registry
            .execute_scoped_typed_read(authenticated_scope, &now, move |connection, _scope| {
                connection
                    .query_row(
                        "SELECT period_ref, period_ends_at_elapsed_ms, package_bytes,
                                    created_at, lane, identity_json
                               FROM app_resource_trees
                              WHERE budget_ledger_ref = ?1
                                AND installation_id = ?2
                                AND root_execution_id = ?3",
                        params![
                            ledger_ref.as_str(),
                            installation_id.as_str(),
                            execution_id.as_str(),
                        ],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Option<i64>>(1)?,
                                row.get::<_, Option<i64>>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, String>(4)?,
                                row.get::<_, Vec<u8>>(5)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(AppResourceAuthorityError::from)
            })
            .await?
            .flatten()
            .ok_or(AppResourceAuthorityError::StaleRegistryAuthority(
                "resource root",
            ))?;
        let period_ends_at_elapsed_ms = durable_baseline
            .1
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "resource root is missing its period deadline".to_owned(),
                )
            })
            .and_then(|value| from_sql_u64(value, "period_ends_at_elapsed_ms"))?;
        let package_bytes = durable_baseline
            .2
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "resource root is missing its package measurement".to_owned(),
                )
            })
            .and_then(|value| from_sql_u64(value, "package_bytes"))?;
        let started_at = parse_resource_timestamp(&durable_baseline.3)?;
        let canonical_elapsed_ms = elapsed_since_root(started_at, now)?;
        let lane = parse_lane(&durable_baseline.4)?;
        let accepted_identity: AppResourceTreeIdentity =
            decode_app_contract(&durable_baseline.5, &AppContractLimits::default())?;
        if accepted_identity.scope != binding.scope
            || accepted_identity.installation_id != binding.installation_id
            || accepted_identity.root_execution_id != binding.execution_id
            || accepted_identity.budget_ledger_ref != binding.budget_ledger_ref
            || accepted_identity.package_revision_ref != binding.package_revision_ref
            || accepted_identity.schema_revision != binding.schema_revision
            || accepted_identity.authority_digest != binding.authority_digest
            || accepted_identity.behavior_resource_identity != binding.behavior_resource_identity
        {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "recovered resource tree",
                identity: binding.budget_ledger_ref.to_string(),
            });
        }
        let active_root_key = scheduler_root_key(authenticated_scope, &binding)?;
        let counts = self
            .scheduler_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root_is_active = counts.active_roots.contains(&active_root_key);
        let scheduler_foreground_runs_excluding_root = counts
            .foreground
            .checked_sub(
                if root_is_active && lane == AppResourceExecutionLane::Foreground {
                    1
                } else {
                    0
                },
            )
            .ok_or(AppResourceAuthorityError::InvalidRuntimeConfiguration(
                "active recovered foreground root is absent from scheduler counts",
            ))?;
        let scheduler_background_runs_excluding_root = counts
            .background
            .checked_sub(
                if root_is_active && lane == AppResourceExecutionLane::Background {
                    1
                } else {
                    0
                },
            )
            .ok_or(AppResourceAuthorityError::InvalidRuntimeConfiguration(
                "active recovered background root is absent from scheduler counts",
            ))?;
        let snapshot = AppResourceCurrentSnapshot {
            period_ref: AppReference::parse(durable_baseline.0)?,
            period_ends_at_elapsed_ms,
            package_revision_ref: resolved.package_revision_ref.clone(),
            package_bytes,
            scheduler_foreground_runs_excluding_root,
            scheduler_background_runs_excluding_root,
            observed_at: now,
        };
        drop(counts);
        Ok(AppResourceMutationAuthority::from_accepted_runtime_owners(
            resolved,
            binding,
            policy,
            snapshot,
            fence,
            canonical_elapsed_ms,
            accepted_identity,
        ))
    }

    /// Read one bounded maintenance page from the canonical authority. The
    /// workflow owner must correlate cleanup candidates with its independent
    /// crash marker; this projection alone can never release or charge usage.
    pub async fn inspect_maintenance_batch(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        request: AppResourceMaintenanceBatchRequest,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMaintenanceBatch, AppResourceAuthorityError> {
        self.authority
            .inspect_maintenance_batch(authenticated_scope, resolved, request, now)
            .await
    }

    pub async fn rollover_maintenance_period(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        period_ref: &AppReference,
        expected_revision: AppRevision,
        retention_until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<AppResourcePeriodRolloverReceipt, AppResourceAuthorityError> {
        self.authority
            .rollover_period(
                authenticated_scope,
                resolved,
                period_ref,
                expected_revision,
                retention_until,
                now,
            )
            .await
    }

    pub async fn rebuild_maintenance_projection(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        period_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppResourceUsageProjection, AppResourceAuthorityError> {
        self.authority
            .rebuild_usage_projection(authenticated_scope, resolved, period_ref, now)
            .await
    }

    pub async fn retire_maintenance_trees(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        period_ref: &AppReference,
        max_trees: u16,
        now: DateTime<Utc>,
    ) -> Result<AppResourceRetentionReceipt, AppResourceAuthorityError> {
        self.authority
            .retire_terminal_trees(authenticated_scope, resolved, period_ref, max_trees, now)
            .await
    }

    fn ensure_scheduler_policy(
        &self,
        policy: AppResourceEnforcementPolicy,
    ) -> Result<(), AppResourceAuthorityError> {
        if policy.scheduler_capacity != self.scheduler_capacity
            || policy.foreground_reserved_slots != self.foreground_reserved_slots
        {
            return Err(AppResourceAuthorityError::InvalidRuntimeConfiguration(
                "resource policy and process scheduler configuration differ",
            ));
        }
        Ok(())
    }

    fn acquire_scheduler_guard(
        &self,
        lane: AppResourceExecutionLane,
        active_root_key: String,
    ) -> Result<AppResourceSchedulerAdmissionGuard, AppResourceAuthorityError> {
        // Background capacity is taken first so a failed reserve check never
        // transiently consumes a foreground-capable global slot.
        let background_permit = if lane == AppResourceExecutionLane::Background {
            Some(
                Arc::clone(&self.background_slots)
                    .try_acquire_owned()
                    .map_err(|_| AppResourceAuthorityError::SchedulerCapacityUnavailable)?,
            )
        } else {
            None
        };
        let total_permit = Arc::clone(&self.total_slots)
            .try_acquire_owned()
            .map_err(|_| AppResourceAuthorityError::SchedulerCapacityUnavailable)?;
        let mut counts = self
            .scheduler_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !counts.active_roots.insert(active_root_key.clone()) {
            return Err(AppResourceAuthorityError::RootRuntimeAlreadyActive);
        }
        let foreground_runs_excluding_root = counts.foreground;
        let background_runs_excluding_root = counts.background;
        let updated_count = match lane {
            AppResourceExecutionLane::Foreground => counts.foreground.checked_add(1),
            AppResourceExecutionLane::Background => counts.background.checked_add(1),
        };
        let Some(updated_count) = updated_count else {
            counts.active_roots.remove(&active_root_key);
            return Err(AppResourceAuthorityError::InvalidRuntimeConfiguration(
                "scheduler counter overflowed",
            ));
        };
        match lane {
            AppResourceExecutionLane::Foreground => counts.foreground = updated_count,
            AppResourceExecutionLane::Background => counts.background = updated_count,
        }
        drop(counts);
        Ok(AppResourceSchedulerAdmissionGuard::from_scheduler(
            lane,
            foreground_runs_excluding_root,
            background_runs_excluding_root,
            total_permit,
            background_permit,
            Arc::clone(&self.scheduler_counts),
            active_root_key,
        ))
    }

    fn mutation_authority(
        &self,
        root_lease: &AppResourceRootDispatchLease,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationAuthority, AppResourceAuthorityError> {
        self.mutation_authority_with_mode(
            root_lease, resolved, binding, policy, fence, now, false, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn accepted_mutation_authority(
        &self,
        root_lease: &AppResourceRootDispatchLease,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationAuthority, AppResourceAuthorityError> {
        self.mutation_authority_with_mode(
            root_lease, resolved, binding, policy, fence, now, true, None,
        )
    }

    fn accepted_cleanup_mutation_authority(
        &self,
        cleanup: &AppResourceAcceptedCleanupLease,
        now: DateTime<Utc>,
        authoritative_elapsed_ms: u64,
    ) -> Result<AppResourceMutationAuthority, AppResourceAuthorityError> {
        let Some(runtime_counts) = cleanup._scheduler_guard.runtime_counts.as_ref() else {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "cleanup lease was not minted by the process scheduler",
            ));
        };
        if !Arc::ptr_eq(runtime_counts, &self.scheduler_counts) {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "cleanup lease belongs to another process scheduler",
            ));
        }
        let counts = runtime_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let foreground_runs_excluding_root = counts
            .foreground
            .checked_sub(
                if cleanup._scheduler_guard.lane == AppResourceExecutionLane::Foreground {
                    1
                } else {
                    0
                },
            )
            .ok_or(AppResourceAuthorityError::InvalidRuntimeConfiguration(
                "accepted cleanup foreground root is absent from scheduler counts",
            ))?;
        let background_runs_excluding_root = counts
            .background
            .checked_sub(
                if cleanup._scheduler_guard.lane == AppResourceExecutionLane::Background {
                    1
                } else {
                    0
                },
            )
            .ok_or(AppResourceAuthorityError::InvalidRuntimeConfiguration(
                "accepted cleanup background root is absent from scheduler counts",
            ))?;
        drop(counts);
        Ok(AppResourceMutationAuthority::from_accepted_runtime_owners(
            cleanup.resolved.clone(),
            cleanup.binding.clone(),
            cleanup.policy,
            AppResourceCurrentSnapshot {
                period_ref: cleanup.identity.installation_period_ref.clone(),
                period_ends_at_elapsed_ms: cleanup.period_ends_at_elapsed_ms,
                package_revision_ref: cleanup.identity.package_revision_ref.clone(),
                package_bytes: cleanup.package_bytes,
                scheduler_foreground_runs_excluding_root: foreground_runs_excluding_root,
                scheduler_background_runs_excluding_root: background_runs_excluding_root,
                observed_at: now,
            },
            cleanup.fence,
            authoritative_elapsed_ms,
            cleanup.identity.clone(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn mutation_authority_with_mode(
        &self,
        root_lease: &AppResourceRootDispatchLease,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        fence: AppResourceMutationFence,
        now: DateTime<Utc>,
        accepted_execution: bool,
        authoritative_elapsed_override: Option<u64>,
    ) -> Result<AppResourceMutationAuthority, AppResourceAuthorityError> {
        let Some(runtime_counts) = root_lease._scheduler_guard.runtime_counts.as_ref() else {
            return Err(AppResourceAuthorityError::InvalidAdmissionSnapshot(
                "root lease was not minted by the process runtime coordinator",
            ));
        };
        if !Arc::ptr_eq(runtime_counts, &self.scheduler_counts) {
            return Err(AppResourceAuthorityError::InvalidAdmissionSnapshot(
                "root lease belongs to another process scheduler gate",
            ));
        }
        let counts = runtime_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let foreground_runs_excluding_root = counts
            .foreground
            .checked_sub(
                if root_lease._scheduler_guard.lane == AppResourceExecutionLane::Foreground {
                    1
                } else {
                    0
                },
            )
            .ok_or(AppResourceAuthorityError::InvalidRuntimeConfiguration(
                "retained foreground root is absent from scheduler counts",
            ))?;
        let background_runs_excluding_root = counts
            .background
            .checked_sub(
                if root_lease._scheduler_guard.lane == AppResourceExecutionLane::Background {
                    1
                } else {
                    0
                },
            )
            .ok_or(AppResourceAuthorityError::InvalidRuntimeConfiguration(
                "retained background root is absent from scheduler counts",
            ))?;
        drop(counts);
        let snapshot = AppResourceCurrentSnapshot {
            period_ref: root_lease.identity.installation_period_ref.clone(),
            period_ends_at_elapsed_ms: root_lease.period_ends_at_elapsed_ms,
            package_revision_ref: root_lease.package_revision_ref.clone(),
            package_bytes: root_lease.package_bytes,
            scheduler_foreground_runs_excluding_root: foreground_runs_excluding_root,
            scheduler_background_runs_excluding_root: background_runs_excluding_root,
            observed_at: now,
        };
        let elapsed = match authoritative_elapsed_override {
            Some(elapsed) if accepted_execution => elapsed,
            Some(_) => {
                return Err(AppResourceAuthorityError::InvalidObservation(
                    "elapsed override is settlement-only",
                ));
            },
            None => root_lease.elapsed_at(now)?,
        };
        Ok(if accepted_execution {
            AppResourceMutationAuthority::from_accepted_runtime_owners(
                resolved,
                binding,
                policy,
                snapshot,
                fence,
                elapsed,
                root_lease.identity.clone(),
            )
        } else {
            AppResourceMutationAuthority::from_runtime_owners(
                resolved, binding, policy, snapshot, fence, elapsed,
            )
        })
    }
}

/// The instant the resource period containing `now` ends: the first of the
/// next UTC month. Ceilings named `monthly_*` reset there, so a background
/// behavior parked on one of them is due again exactly then.
pub fn resource_period_end(now: DateTime<Utc>) -> Result<DateTime<Utc>, AppResourceAuthorityError> {
    let (next_year, next_month) = if now.month() == 12 {
        (
            now.year().checked_add(1).ok_or(
                AppResourceAuthorityError::InvalidRuntimeConfiguration(
                    "calendar year overflowed while deriving the resource period",
                ),
            )?,
            1,
        )
    } else {
        (now.year(), now.month() + 1)
    };
    Utc.with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
        .single()
        .ok_or(AppResourceAuthorityError::InvalidRuntimeConfiguration(
            "resource period boundary is not a unique UTC instant",
        ))
}

fn canonical_monthly_period(
    now: DateTime<Utc>,
) -> Result<(AppReference, u64), AppResourceAuthorityError> {
    let (next_year, next_month) = if now.month() == 12 {
        (
            now.year().checked_add(1).ok_or(
                AppResourceAuthorityError::InvalidRuntimeConfiguration(
                    "calendar year overflowed while deriving the resource period",
                ),
            )?,
            1,
        )
    } else {
        (now.year(), now.month() + 1)
    };
    let period_end = Utc
        .with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
        .single()
        .ok_or(AppResourceAuthorityError::InvalidRuntimeConfiguration(
            "resource period boundary is not a unique UTC instant",
        ))?;
    let remaining = period_end - now;
    let remaining_milliseconds = remaining.num_milliseconds();
    let period_ends_at_elapsed_ms = if remaining_milliseconds > 0 {
        u64::try_from(remaining_milliseconds).map_err(|_| {
            AppResourceAuthorityError::InvalidRuntimeConfiguration(
                "resource period duration exceeds the runtime range",
            )
        })?
    } else if remaining > chrono::Duration::zero() {
        // The canonical contract speaks in milliseconds. Preserve the final
        // positive sub-millisecond sliver of a month instead of truncating it
        // to the special invalid/expired value zero.
        1
    } else {
        return Err(AppResourceAuthorityError::InvalidRuntimeConfiguration(
            "resource period boundary is not after the current time",
        ));
    };
    let period_ref = AppReference::parse(format!("period:{:04}-{:02}", now.year(), now.month()))?;
    Ok((period_ref, period_ends_at_elapsed_ms))
}

fn scheduler_root_key(
    authenticated_scope: &AuthenticatedAppScope,
    binding: &AppRunBinding,
) -> Result<String, AppResourceAuthorityError> {
    let material = canonical_json_bytes(&serde_json::to_value((
        authenticated_scope.scope(),
        &binding.installation_id,
        &binding.execution_id,
        &binding.budget_ledger_ref,
    ))?)?;
    Ok(AppDigest::blake3(&material).as_str().to_owned())
}

#[derive(Clone, Debug)]
pub struct AppResourceAuthorityService {
    registry: AppRegistryService,
}

impl AppResourceAuthorityService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self { registry }
    }

    /// Atomically admit one execution root and append its canonical first
    /// event. The pure Phase-0 verdict and durable insert share the exact same
    /// installation-period revision; no caller can dispatch from the verdict
    /// alone.
    #[allow(clippy::too_many_arguments)]
    pub async fn admit_root(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        binding: &AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        snapshot: AppResourceAdmissionSnapshot,
        lane: AppResourceExecutionLane,
        root_node_id: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppResourceRootAdmissionReceipt, AppResourceAuthorityError> {
        authenticated_scope
            .ensure_live_at(&now)
            .map_err(AppRegistryError::from)?;
        if snapshot.observed_at != now {
            return Err(AppResourceAuthorityError::InvalidAdmissionSnapshot(
                "scheduler/package observations must be resolved at the admission boundary",
            ));
        }
        let authenticated_scope = authenticated_scope.clone();
        let operation_scope = authenticated_scope.clone();
        let resolved = resolved.clone();
        let binding = binding.clone();
        let operation_now = now;
        self.registry
            .execute_scoped_typed_write(&authenticated_scope, &now, move |connection, scope| {
                admit_root_blocking(
                    connection,
                    scope,
                    &operation_scope,
                    &resolved,
                    &binding,
                    policy,
                    snapshot,
                    lane,
                    root_node_id,
                    operation_now,
                )
            })
            .await
    }

    /// Load the bounded canonical journal for crash recovery. Missing scopes
    /// and unknown tree identities remain distinct from corrupt durable bytes.
    pub async fn journal(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        budget_ledger_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<Option<AppResourceJournal>, AppResourceAuthorityError> {
        let budget_ledger_ref = budget_ledger_ref.clone();
        self.registry
            .execute_scoped_typed_read(authenticated_scope, &now, move |connection, _scope| {
                // Header and events must describe one committed revision even
                // while other participants append to this root. A pooled read
                // connection otherwise runs each SELECT in a fresh snapshot.
                let transaction = connection.unchecked_transaction()?;
                let journal = load_journal(&transaction, &budget_ledger_ref)?;
                transaction.commit()?;
                Ok(journal)
            })
            .await
            .map(Option::flatten)
    }

    pub async fn inspect_maintenance_batch(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        request: AppResourceMaintenanceBatchRequest,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMaintenanceBatch, AppResourceAuthorityError> {
        // Defend direct struct construction inside the crate as well as the
        // public constructor; both query limits must remain hard-bounded.
        AppResourceMaintenanceBatchRequest::from_driver(
            request.cursor.clone(),
            request.max_cleanup_candidates,
            request.max_periods,
        )?;
        ensure_accepted_authenticated_snapshot(authenticated_scope, resolved, &now)?;
        let operation_scope = authenticated_scope.clone();
        let resolved = resolved.clone();
        self.registry
            .execute_scoped_typed_read(authenticated_scope, &now, move |connection, scope| {
                inspect_maintenance_batch_blocking(
                    connection,
                    scope,
                    &operation_scope,
                    &resolved,
                    request,
                    now,
                )
            })
            .await?
            .ok_or(AppResourceAuthorityError::StaleRegistryAuthority(
                "resource maintenance scope",
            ))
    }

    pub async fn open_node(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        authority: AppResourceMutationAuthority,
        node: AppResourceNodeAdmission,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::NodeOpened(node),
            None,
            now,
        )
        .await
    }

    pub async fn observe_progress(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        authority: AppResourceMutationAuthority,
        progress: AppResourceProgressObservation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::ProgressObserved(progress),
            None,
            now,
        )
        .await
    }

    pub async fn reserve(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        authority: AppResourceMutationAuthority,
        reservation: AppResourceReservationRequest,
        now: DateTime<Utc>,
    ) -> Result<AppResourceReservationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        let identity = root_lease.identity.clone();
        let node_id = reservation.node_id.clone();
        let reservation_id = reservation.reservation_id.clone();
        let operation_key = reservation.operation_key.clone();
        let expires_at_elapsed_ms = reservation.expires_at_elapsed_ms;
        let created_at_elapsed_ms = reservation.at_elapsed_ms;
        let mutation = self
            .append_event(
                authenticated_scope,
                authority,
                PendingResourceEvent::Reserved(reservation),
                None,
                now,
            )
            .await?;
        let dispatch_permit =
            mutation
                .reservation_dispatchable
                .then_some(AppResourceOperationDispatchPermit {
                    identity,
                    node_id,
                    reservation_id,
                    operation_key,
                    journal_revision: mutation.state.journal_revision,
                    created_at_elapsed_ms,
                    expires_at_elapsed_ms,
                });
        Ok(AppResourceReservationReceipt {
            mutation,
            dispatch_permit,
        })
    }

    pub async fn settle(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: AppResourceOperationDispatchPermit,
        authority: AppResourceMutationAuthority,
        observation: AppResourceSettlementObservation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        ensure_operation_permit_matches(
            &dispatch_permit,
            root_lease,
            &authority.binding,
            &observation,
        )?;
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::Settled(observation),
            None,
            now,
        )
        .await
    }

    async fn settle_accepted(
        &self,
        settlement_permit: AppResourceAcceptedSettlementPermit,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: &AppResourceOperationDispatchPermit,
        authority: AppResourceMutationAuthority,
        observation: AppResourceSettlementObservation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        ensure_operation_permit_matches(
            dispatch_permit,
            root_lease,
            &authority.binding,
            &observation,
        )?;
        if settlement_permit.identity != root_lease.identity
            || settlement_permit.node_id != dispatch_permit.node_id
            || settlement_permit.reservation_id != dispatch_permit.reservation_id
        {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "accepted settlement permit",
                identity: dispatch_permit.reservation_id.to_string(),
            });
        }
        self.append_accepted_settlement_event(
            settlement_permit,
            authority,
            PendingResourceEvent::Settled(observation),
            None,
            now,
        )
        .await
    }

    pub async fn settle_safe_local_no_effect(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        authority: AppResourceMutationAuthority,
        proof: AppResourceSafeLocalNoEffect,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        let AppResourceSafeLocalNoEffect {
            dispatch_permit,
            observation_id,
            started_at_elapsed_ms,
            at_elapsed_ms,
        } = proof;
        let observation = AppResourceSettlementObservation::committed(
            dispatch_permit.node_id.clone(),
            dispatch_permit.reservation_id.clone(),
            observation_id,
            vec![AppResourceObservationSource::WorkflowNoEffect],
            AppResourceQuantity::default(),
            Vec::new(),
            (at_elapsed_ms > started_at_elapsed_ms)
                .then_some(AppActiveInterval {
                    start_elapsed_ms: started_at_elapsed_ms,
                    end_elapsed_ms: at_elapsed_ms,
                })
                .into_iter()
                .collect(),
            at_elapsed_ms,
        );
        ensure_operation_permit_matches(
            &dispatch_permit,
            root_lease,
            &authority.binding,
            &observation,
        )?;
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::Settled(observation),
            None,
            now,
        )
        .await
    }

    pub async fn release_pre_io(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: AppResourceOperationDispatchPermit,
        authority: AppResourceMutationAuthority,
        proof: AppResourcePreIoUnspentReconciliation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        if dispatch_permit.identity != root_lease.identity {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "operation dispatch permit",
                identity: dispatch_permit.reservation_id.to_string(),
            });
        }
        let recovery = AppResourceCrashReconciliation::from_crash_reconciler(
            dispatch_permit.node_id,
            dispatch_permit.reservation_id,
            proof.observation_id,
            proof.reconciliation_ref,
            proof.reconciliation_revision,
            proof.reconciled_at_elapsed_ms,
        );
        self.reconcile_proven_unspent(authenticated_scope, authority, recovery, now)
            .await
    }

    async fn release_pre_io_accepted(
        &self,
        settlement_permit: AppResourceAcceptedSettlementPermit,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: &AppResourceOperationDispatchPermit,
        authority: AppResourceMutationAuthority,
        proof: &AppResourcePreIoUnspentReconciliation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        if dispatch_permit.identity != root_lease.identity
            || settlement_permit.identity != root_lease.identity
            || settlement_permit.node_id != dispatch_permit.node_id
            || settlement_permit.reservation_id != dispatch_permit.reservation_id
        {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "accepted pre-I/O release permit",
                identity: dispatch_permit.reservation_id.to_string(),
            });
        }
        let recovery = AppResourceCrashReconciliation::from_crash_reconciler(
            dispatch_permit.node_id.clone(),
            dispatch_permit.reservation_id.clone(),
            proof.observation_id.clone(),
            proof.reconciliation_ref.clone(),
            proof.reconciliation_revision,
            proof.reconciled_at_elapsed_ms,
        );
        let observation = AppResourceSettlementObservation {
            node_id: recovery.node_id.clone(),
            reservation_id: recovery.reservation_id.clone(),
            observation_id: recovery.observation_id.clone(),
            outcome: AppResourceSettlementOutcome::ProvenUnspent,
            observation_sources: vec![AppResourceObservationSource::CrashReconciler],
            actual: AppResourceQuantity::default(),
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms: recovery.reconciled_at_elapsed_ms,
        };
        self.append_accepted_settlement_event(
            settlement_permit,
            authority,
            PendingResourceEvent::Settled(observation),
            Some(PendingResourceRecovery::ProvenUnspent(recovery)),
            now,
        )
        .await
    }

    pub(crate) async fn start_effect_dispatch(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: &AppResourceOperationDispatchPermit,
        authority: AppResourceMutationAuthority,
        start: AppResourceEffectDispatchStart,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        if dispatch_permit.identity != root_lease.identity
            || dispatch_permit.identity.budget_ledger_ref != authority.binding.budget_ledger_ref
        {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "effect dispatch permit",
                identity: dispatch_permit.reservation_id.to_string(),
            });
        }
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::EffectDispatchStarted {
                node_id: dispatch_permit.node_id.clone(),
                reservation_id: dispatch_permit.reservation_id.clone(),
                effect_binding_digest: start.effect_binding_digest,
                at_elapsed_ms: start.at_elapsed_ms,
            },
            None,
            now,
        )
        .await
    }

    async fn abort_effect_dispatch_before_io_accepted(
        &self,
        settlement_permit: AppResourceAcceptedSettlementPermit,
        root_lease: &AppResourceRootDispatchLease,
        dispatch_permit: &AppResourceOperationDispatchPermit,
        authority: AppResourceMutationAuthority,
        abort_proof: &AppEffectAbortProof,
        at_elapsed_ms: u64,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        if dispatch_permit.identity != root_lease.identity
            || dispatch_permit.identity.budget_ledger_ref != authority.binding.budget_ledger_ref
        {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "effect dispatch abort permit",
                identity: dispatch_permit.reservation_id.to_string(),
            });
        }
        if settlement_permit.identity != root_lease.identity
            || settlement_permit.node_id != dispatch_permit.node_id
            || settlement_permit.reservation_id != dispatch_permit.reservation_id
        {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "accepted effect abort permit",
                identity: dispatch_permit.reservation_id.to_string(),
            });
        }
        self.append_accepted_settlement_event(
            settlement_permit,
            authority,
            PendingResourceEvent::EffectDispatchAbortedBeforeIo {
                node_id: dispatch_permit.node_id.clone(),
                reservation_id: dispatch_permit.reservation_id.clone(),
                effect_binding_digest: abort_proof.effect_binding_digest().clone(),
                reason: abort_proof.reason(),
                at_elapsed_ms,
            },
            None,
            now,
        )
        .await
    }

    pub async fn reconcile_proven_unspent(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppResourceMutationAuthority,
        recovery: AppResourceCrashReconciliation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        let observation = AppResourceSettlementObservation {
            node_id: recovery.node_id.clone(),
            reservation_id: recovery.reservation_id.clone(),
            observation_id: recovery.observation_id.clone(),
            outcome: AppResourceSettlementOutcome::ProvenUnspent,
            observation_sources: vec![AppResourceObservationSource::CrashReconciler],
            actual: AppResourceQuantity::default(),
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms: recovery.reconciled_at_elapsed_ms,
        };
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::Settled(observation),
            Some(PendingResourceRecovery::ProvenUnspent(recovery)),
            now,
        )
        .await
    }

    pub async fn reconcile_committed(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppResourceMutationAuthority,
        proof: AppResourceCommittedRecovery,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        let observation = AppResourceSettlementObservation::from_app_store_transaction(
            proof.node_id.clone(),
            proof.reservation_id.clone(),
            proof.observation_id.clone(),
            proof.actual,
            Vec::new(),
            proof.at_elapsed_ms,
        );
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::Settled(observation),
            Some(PendingResourceRecovery::Committed(proof)),
            now,
        )
        .await
    }

    async fn reconcile_conservative_post_io_charge(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppResourceMutationAuthority,
        proof: AppResourceConservativePostIoCharge,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        let mut observation = AppResourceSettlementObservation::committed(
            proof.node_id.clone(),
            proof.reservation_id.clone(),
            proof.observation_id.clone(),
            vec![AppResourceObservationSource::CrashReconciler],
            proof.requested,
            proof.capability_requests.clone(),
            proof.active_intervals.clone(),
            proof.at_elapsed_ms,
        );
        if let Some(effect_binding_digest) = proof.effect_binding_digest.as_ref() {
            observation = observation
                .bind_accepted_effect_unchecked_by_transport(effect_binding_digest.clone());
        }
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::Settled(observation),
            Some(PendingResourceRecovery::ConservativePostIo(proof)),
            now,
        )
        .await
    }

    async fn settle_accepted_cleanup_effect_completion(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppResourceMutationAuthority,
        observation: AppResourceSettlementObservation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        if authority.accepted_identity.is_none()
            || observation.outcome != AppResourceSettlementOutcome::Committed
            || observation.effect_binding_digest.is_none()
            || observation.effect_result.is_none()
            || !matches!(
                observation.observation_sources.as_slice(),
                [AppResourceObservationSource::ToolRuntime]
            )
        {
            return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
                "accepted cleanup effect completion is not exact",
            ));
        }
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::Settled(observation),
            None,
            now,
        )
        .await
    }

    pub async fn reconcile_no_effect_committed(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppResourceMutationAuthority,
        proof: AppResourceNoEffectCommittedRecovery,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        let observation = AppResourceSettlementObservation::committed(
            proof.node_id.clone(),
            proof.reservation_id.clone(),
            proof.observation_id.clone(),
            vec![AppResourceObservationSource::WorkflowNoEffect],
            proof.actual,
            Vec::new(),
            Vec::new(),
            proof.at_elapsed_ms,
        );
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::Settled(observation),
            Some(PendingResourceRecovery::NoEffectCommitted(proof)),
            now,
        )
        .await
    }

    pub async fn reconcile_verified_receipt_absence(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppResourceMutationAuthority,
        proof: AppResourceReceiptAbsentReconciliation,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        let observation = AppResourceSettlementObservation {
            node_id: proof.node_id.clone(),
            reservation_id: proof.reservation_id.clone(),
            observation_id: proof.observation_id.clone(),
            outcome: AppResourceSettlementOutcome::ProvenUnspent,
            observation_sources: vec![AppResourceObservationSource::CrashReconciler],
            actual: AppResourceQuantity::default(),
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms: proof.reconciled_at_elapsed_ms,
        };
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::Settled(observation),
            Some(PendingResourceRecovery::ReceiptAbsent(proof)),
            now,
        )
        .await
    }

    pub async fn close_node(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        root_lease: &AppResourceRootDispatchLease,
        authority: AppResourceMutationAuthority,
        close: AppResourceNodeClose,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        ensure_root_lease_matches(root_lease, &authority.binding)?;
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::NodeClosed(close),
            None,
            now,
        )
        .await
    }

    async fn close_accepted_node(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppResourceMutationAuthority,
        close: AppResourceNodeClose,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        if authority.accepted_identity.is_none() {
            return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                "cleanup close requires immutable accepted-tree identity",
            ));
        }
        self.append_event(
            authenticated_scope,
            authority,
            PendingResourceEvent::NodeClosed(close),
            None,
            now,
        )
        .await
    }

    async fn append_event(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppResourceMutationAuthority,
        pending: PendingResourceEvent,
        recovery: Option<PendingResourceRecovery>,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        authenticated_scope
            .ensure_live_at(&now)
            .map_err(AppRegistryError::from)?;
        if authority.snapshot.observed_at != now {
            return Err(AppResourceAuthorityError::InvalidAdmissionSnapshot(
                "resource observations must be resolved at the mutation boundary",
            ));
        }
        let operation_scope = authenticated_scope.clone();
        self.registry
            .execute_scoped_typed_write(authenticated_scope, &now, move |connection, scope| {
                append_event_blocking(
                    connection,
                    scope,
                    &operation_scope,
                    authority,
                    pending,
                    recovery,
                    now,
                )
            })
            .await
    }

    async fn append_accepted_settlement_event(
        &self,
        settlement_permit: AppResourceAcceptedSettlementPermit,
        authority: AppResourceMutationAuthority,
        pending: PendingResourceEvent,
        recovery: Option<PendingResourceRecovery>,
        now: DateTime<Utc>,
    ) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
        if authority.accepted_identity.is_none()
            || !matches!(
                pending,
                PendingResourceEvent::Settled(_)
                    | PendingResourceEvent::EffectDispatchAbortedBeforeIo { .. }
            )
        {
            return Err(AppResourceAuthorityError::InvalidObservation(
                "accepted settlement capability can only settle or abort-before-I/O",
            ));
        }
        if authority.snapshot.observed_at != now {
            return Err(AppResourceAuthorityError::InvalidAdmissionSnapshot(
                "accepted settlement observation must match its mutation boundary",
            ));
        }
        let operation_scope = settlement_permit.authenticated_scope.clone();
        self.registry
            .execute_scoped_typed_accepted_settlement_write(
                &settlement_permit,
                move |connection, scope| {
                    append_event_blocking(
                        connection,
                        scope,
                        &operation_scope,
                        authority,
                        pending,
                        recovery,
                        now,
                    )
                },
            )
            .await
    }

    /// Close a period to new root/reservation admissions. Existing trees may
    /// still settle and close; their cost remains attributed to this period.
    pub async fn rollover_period(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        period_ref: &AppReference,
        expected_revision: AppRevision,
        retention_until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<AppResourcePeriodRolloverReceipt, AppResourceAuthorityError> {
        if retention_until <= now {
            return Err(AppResourceAuthorityError::InvalidRetention(
                "retention deadline must be after rollover",
            ));
        }
        let operation_scope = authenticated_scope.clone();
        let resolved = resolved.clone();
        let period_ref = period_ref.clone();
        self.registry
            .execute_scoped_typed_background_write(
                authenticated_scope,
                &now,
                move |connection, scope| {
                    rollover_period_blocking(
                        connection,
                        scope,
                        &operation_scope,
                        &resolved,
                        &period_ref,
                        expected_revision,
                        retention_until,
                        now,
                    )
                },
            )
            .await
    }

    /// Rebuild the presentation-only usage row from canonical period totals.
    /// Enforcement never reads this table, so deleting or lagging this row
    /// cannot mint budget.
    pub async fn rebuild_usage_projection(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        period_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppResourceUsageProjection, AppResourceAuthorityError> {
        let operation_scope = authenticated_scope.clone();
        let resolved = resolved.clone();
        let period_ref = period_ref.clone();
        self.registry
            .execute_scoped_typed_background_write(
                authenticated_scope,
                &now,
                move |connection, scope| {
                    rebuild_usage_projection_blocking(
                        connection,
                        scope,
                        &operation_scope,
                        &resolved,
                        &period_ref,
                        now,
                    )
                },
            )
            .await
    }

    /// Compact terminal trees only after their closed period's retention
    /// deadline. An immutable tombstone is inserted before journal deletion,
    /// preserving replay denial and final evidence identity.
    pub async fn retire_terminal_trees(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        period_ref: &AppReference,
        max_trees: u16,
        now: DateTime<Utc>,
    ) -> Result<AppResourceRetentionReceipt, AppResourceAuthorityError> {
        if max_trees == 0 || max_trees > MAX_RETIREMENT_TREES_PER_TRANSACTION {
            return Err(AppResourceAuthorityError::InvalidRetention(
                "retirement batch exceeds the bounded transaction limit",
            ));
        }
        let operation_scope = authenticated_scope.clone();
        let resolved = resolved.clone();
        let period_ref = period_ref.clone();
        self.registry
            .execute_scoped_typed_background_write(
                authenticated_scope,
                &now,
                move |connection, scope| {
                    retire_terminal_trees_blocking(
                        connection,
                        scope,
                        &operation_scope,
                        &resolved,
                        &period_ref,
                        max_trees,
                        now,
                    )
                },
            )
            .await
    }
}

#[derive(Debug, Clone)]
enum PendingResourceEvent {
    NodeOpened(AppResourceNodeAdmission),
    ProgressObserved(AppResourceProgressObservation),
    Reserved(AppResourceReservationRequest),
    EffectDispatchStarted {
        node_id: AppReference,
        reservation_id: AppReference,
        effect_binding_digest: AppDigest,
        at_elapsed_ms: u64,
    },
    EffectDispatchAbortedBeforeIo {
        node_id: AppReference,
        reservation_id: AppReference,
        effect_binding_digest: AppDigest,
        reason: AppEffectDispatchAbortReason,
        at_elapsed_ms: u64,
    },
    Settled(AppResourceSettlementObservation),
    NodeClosed(AppResourceNodeClose),
}

enum PendingResourceRecovery {
    ProvenUnspent(AppResourceCrashReconciliation),
    Committed(AppResourceCommittedRecovery),
    ConservativePostIo(AppResourceConservativePostIoCharge),
    NoEffectCommitted(AppResourceNoEffectCommittedRecovery),
    ReceiptAbsent(AppResourceReceiptAbsentReconciliation),
}

impl PendingResourceEvent {
    fn materialize(
        &self,
        sequence: u64,
        lane: AppResourceExecutionLane,
    ) -> AppResourceJournalEvent {
        match self {
            Self::NodeOpened(node) => AppResourceJournalEvent::NodeOpened {
                sequence,
                node_id: node.node_id.clone(),
                execution_ref: node.execution_ref.clone(),
                parent_node_id: Some(node.parent_node_id.clone()),
                node_kind: node.node_kind,
                lane,
                at_elapsed_ms: node.at_elapsed_ms,
            },
            Self::ProgressObserved(progress) => AppResourceJournalEvent::ProgressObserved {
                sequence,
                node_id: progress.node_id.clone(),
                progress_id: progress.progress_id.clone(),
                at_elapsed_ms: progress.at_elapsed_ms,
            },
            Self::Reserved(reservation) => AppResourceJournalEvent::Reserved {
                sequence,
                node_id: reservation.node_id.clone(),
                reservation_id: reservation.reservation_id.clone(),
                operation_key: reservation.operation_key.clone(),
                requested: reservation.requested,
                capability_requests: reservation.capability_requests.clone(),
                at_elapsed_ms: reservation.at_elapsed_ms,
                expires_at_elapsed_ms: reservation.expires_at_elapsed_ms,
            },
            Self::EffectDispatchStarted {
                node_id,
                reservation_id,
                effect_binding_digest,
                at_elapsed_ms,
            } => AppResourceJournalEvent::EffectDispatchStarted {
                sequence,
                node_id: node_id.clone(),
                reservation_id: reservation_id.clone(),
                effect_binding_digest: effect_binding_digest.clone(),
                at_elapsed_ms: *at_elapsed_ms,
            },
            Self::EffectDispatchAbortedBeforeIo {
                node_id,
                reservation_id,
                effect_binding_digest,
                reason,
                at_elapsed_ms,
            } => AppResourceJournalEvent::EffectDispatchAbortedBeforeIo {
                sequence,
                node_id: node_id.clone(),
                reservation_id: reservation_id.clone(),
                effect_binding_digest: effect_binding_digest.clone(),
                reason: *reason,
                at_elapsed_ms: *at_elapsed_ms,
            },
            Self::Settled(observation) => AppResourceJournalEvent::Settled {
                sequence,
                node_id: observation.node_id.clone(),
                reservation_id: observation.reservation_id.clone(),
                observation_id: observation.observation_id.clone(),
                outcome: observation.outcome,
                observation_sources: observation.observation_sources.clone(),
                actual: observation.actual,
                capability_usage: observation.capability_usage.clone(),
                active_intervals: observation.active_intervals.clone(),
                effect_binding_digest: observation.effect_binding_digest.clone(),
                effect_result: observation.effect_result.clone(),
                at_elapsed_ms: observation.at_elapsed_ms,
            },
            Self::NodeClosed(close) => AppResourceJournalEvent::NodeClosed {
                sequence,
                node_id: close.node_id.clone(),
                at_elapsed_ms: close.at_elapsed_ms,
            },
        }
    }

    fn at_elapsed_ms(&self) -> u64 {
        match self {
            Self::NodeOpened(value) => value.at_elapsed_ms,
            Self::ProgressObserved(value) => value.at_elapsed_ms,
            Self::Reserved(value) => value.at_elapsed_ms,
            Self::EffectDispatchStarted { at_elapsed_ms, .. } => *at_elapsed_ms,
            Self::EffectDispatchAbortedBeforeIo { at_elapsed_ms, .. } => *at_elapsed_ms,
            Self::Settled(value) => value.at_elapsed_ms,
            Self::NodeClosed(value) => value.at_elapsed_ms,
        }
    }

    /// Return `Some(sequence)` for an exact replay, `None` for a new identity,
    /// and fail for a rebound identity. This iterative scan is bounded by the
    /// hard journal event limit and never follows attacker-controlled links.
    fn find_exact(
        &self,
        journal: &AppResourceJournal,
        lane: AppResourceExecutionLane,
    ) -> Result<Option<u64>, AppResourceAuthorityError> {
        for event in &journal.events {
            let identity_collision = match (self, event) {
                (
                    Self::NodeOpened(candidate),
                    AppResourceJournalEvent::NodeOpened {
                        node_id,
                        execution_ref,
                        ..
                    },
                ) => candidate.node_id == *node_id || candidate.execution_ref == *execution_ref,
                (
                    Self::ProgressObserved(candidate),
                    AppResourceJournalEvent::ProgressObserved { progress_id, .. },
                ) => candidate.progress_id == *progress_id,
                (
                    Self::Reserved(candidate),
                    AppResourceJournalEvent::Reserved {
                        reservation_id,
                        operation_key,
                        ..
                    },
                ) => {
                    candidate.reservation_id == *reservation_id
                        || candidate.operation_key == *operation_key
                },
                (
                    Self::EffectDispatchStarted {
                        reservation_id: candidate_reservation_id,
                        ..
                    },
                    AppResourceJournalEvent::EffectDispatchStarted { reservation_id, .. },
                ) => candidate_reservation_id == reservation_id,
                (
                    Self::EffectDispatchAbortedBeforeIo {
                        reservation_id: candidate_reservation_id,
                        ..
                    },
                    AppResourceJournalEvent::EffectDispatchAbortedBeforeIo {
                        reservation_id, ..
                    },
                ) => candidate_reservation_id == reservation_id,
                (
                    Self::Settled(candidate),
                    AppResourceJournalEvent::Settled { observation_id, .. },
                ) => candidate.observation_id == *observation_id,
                (
                    Self::NodeClosed(candidate),
                    AppResourceJournalEvent::NodeClosed { node_id, .. },
                ) => candidate.node_id == *node_id,
                _ => false,
            };
            if !identity_collision {
                continue;
            }
            let expected = self.materialize(event_sequence(event), lane);
            if expected == *event {
                if let Self::EffectDispatchStarted { reservation_id, .. } = self {
                    let terminal_transition_exists =
                        journal.events.iter().any(|later| match later {
                            AppResourceJournalEvent::EffectDispatchAbortedBeforeIo {
                                reservation_id: later_reservation,
                                ..
                            }
                            | AppResourceJournalEvent::Settled {
                                reservation_id: later_reservation,
                                ..
                            } => later_reservation == reservation_id,
                            _ => false,
                        });
                    if terminal_transition_exists {
                        return Err(AppResourceAuthorityError::IdentityConflict {
                            entity: "terminal effect dispatch",
                            identity: reservation_id.to_string(),
                        });
                    }
                }
                return Ok(Some(event_sequence(event)));
            }
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: self.identity_label(),
                identity: self.identity_value().to_owned(),
            });
        }
        Ok(None)
    }

    fn identity_label(&self) -> &'static str {
        match self {
            Self::NodeOpened(_) => "node or execution",
            Self::ProgressObserved(_) => "progress observation",
            Self::Reserved(_) => "reservation or operation",
            Self::EffectDispatchStarted { .. } => "effect dispatch",
            Self::EffectDispatchAbortedBeforeIo { .. } => "effect dispatch abort",
            Self::Settled(_) => "resource observation",
            Self::NodeClosed(_) => "node closure",
        }
    }

    fn identity_value(&self) -> &str {
        match self {
            Self::NodeOpened(value) => value.node_id.as_str(),
            Self::ProgressObserved(value) => value.progress_id.as_str(),
            Self::Reserved(value) => value.reservation_id.as_str(),
            Self::EffectDispatchStarted { reservation_id, .. } => reservation_id.as_str(),
            Self::EffectDispatchAbortedBeforeIo { reservation_id, .. } => reservation_id.as_str(),
            Self::Settled(value) => value.observation_id.as_str(),
            Self::NodeClosed(value) => value.node_id.as_str(),
        }
    }
}

fn event_sequence(event: &AppResourceJournalEvent) -> u64 {
    match event {
        AppResourceJournalEvent::NodeOpened { sequence, .. }
        | AppResourceJournalEvent::ProgressObserved { sequence, .. }
        | AppResourceJournalEvent::Reserved { sequence, .. }
        | AppResourceJournalEvent::EffectDispatchStarted { sequence, .. }
        | AppResourceJournalEvent::EffectDispatchAbortedBeforeIo { sequence, .. }
        | AppResourceJournalEvent::Settled { sequence, .. }
        | AppResourceJournalEvent::NodeClosed { sequence, .. } => *sequence,
    }
}

fn ensure_root_lease_matches(
    root_lease: &AppResourceRootDispatchLease,
    binding: &AppRunBinding,
) -> Result<(), AppResourceAuthorityError> {
    if root_lease.identity.budget_ledger_ref != binding.budget_ledger_ref
        || root_lease.identity.root_execution_id != binding.execution_id
        || root_lease.identity.installation_id != binding.installation_id
        || root_lease.identity.behavior_resource_identity != binding.behavior_resource_identity
    {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "root dispatch lease",
            identity: binding.budget_ledger_ref.to_string(),
        });
    }
    Ok(())
}

fn ensure_operation_permit_matches(
    permit: &AppResourceOperationDispatchPermit,
    root_lease: &AppResourceRootDispatchLease,
    binding: &AppRunBinding,
    observation: &AppResourceSettlementObservation,
) -> Result<(), AppResourceAuthorityError> {
    if permit.identity != root_lease.identity
        || permit.identity.budget_ledger_ref != binding.budget_ledger_ref
        || permit.node_id != observation.node_id
        || permit.reservation_id != observation.reservation_id
    {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "operation dispatch permit",
            identity: observation.reservation_id.to_string(),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn canonical_conservative_post_io_charge_digest(
    node_id: &AppReference,
    reservation_id: &AppReference,
    operation_key: &AppReference,
    requested: AppResourceQuantity,
    capability_requests: &[AppCapabilityResourceQuantity],
    effect_binding_digest: Option<&AppDigest>,
    cleanup_binding_digest: &AppDigest,
    accepted_identity_digest: &AppDigest,
    expected_journal_revision: u64,
    evidence_ref: &AppReference,
    evidence_revision: AppRevision,
    active_intervals: &[AppActiveInterval],
    at_elapsed_ms: u64,
) -> Result<AppDigest, AppResourceAuthorityError> {
    let proof_bytes = canonical_json_bytes(&serde_json::json!({
        "schema": "app-resource-conservative-post-io-charge/v1",
        "reason": "crash_recovery",
        "node_id": node_id,
        "reservation_id": reservation_id,
        "operation_key": operation_key,
        "requested": requested,
        "capability_requests": capability_requests,
        "effect_binding_digest": effect_binding_digest,
        "cleanup_binding_digest": cleanup_binding_digest,
        "accepted_identity_digest": accepted_identity_digest,
        "expected_journal_revision": expected_journal_revision,
        "evidence_ref": evidence_ref,
        "evidence_revision": evidence_revision,
        "active_intervals": active_intervals,
        "at_elapsed_ms": at_elapsed_ms,
    }))?;
    ensure_stored_record_bounded("conservative post-I/O charge proof", &proof_bytes)?;
    Ok(AppDigest::blake3(&proof_bytes))
}

fn conservative_post_io_observation_id(
    proof_digest: &AppDigest,
) -> Result<AppReference, AppResourceAuthorityError> {
    let proof_hex = proof_digest.as_str().strip_prefix("blake3:").ok_or(
        AppResourceAuthorityError::InvalidCommittedRecovery(
            "conservative post-I/O proof digest is not canonical",
        ),
    )?;
    Ok(AppReference::parse(format!(
        "resource-conservative-post-io:{proof_hex}"
    ))?)
}

#[allow(clippy::too_many_arguments)]
fn canonical_committed_recovery_proof_digest(
    node_id: &AppReference,
    reservation_id: &AppReference,
    operation_key: &AppReference,
    intent_digest: &AppDigest,
    origin: &AppMutationOrigin,
    mutation_key: &AppDigest,
    batch_digest: &AppDigest,
    receipt_id: &AppReference,
    receipt_digest: &AppDigest,
    installation_id: &AppInstallationId,
    actual: AppResourceQuantity,
    at_elapsed_ms: u64,
) -> Result<AppDigest, AppResourceAuthorityError> {
    let proof_bytes = canonical_json_bytes(&serde_json::json!({
        "schema": "app-resource-committed-recovery/v1",
        "node_id": node_id,
        "reservation_id": reservation_id,
        "operation_key": operation_key,
        "intent_digest": intent_digest,
        "origin": origin,
        "mutation_key": mutation_key,
        "batch_digest": batch_digest,
        "receipt_id": receipt_id,
        "receipt_digest": receipt_digest,
        "installation_id": installation_id,
        "actual": actual,
        "at_elapsed_ms": at_elapsed_ms,
    }))?;
    ensure_stored_record_bounded("committed recovery proof", &proof_bytes)?;
    Ok(AppDigest::blake3(&proof_bytes))
}

fn committed_recovery_observation_id(
    proof_digest: &AppDigest,
) -> Result<AppReference, AppResourceAuthorityError> {
    let proof_hex = proof_digest.as_str().strip_prefix("blake3:").ok_or(
        AppResourceAuthorityError::InvalidCommittedRecovery(
            "committed recovery proof digest is not canonical",
        ),
    )?;
    Ok(AppReference::parse(format!(
        "resource-committed-recovery:{proof_hex}"
    ))?)
}

#[allow(clippy::too_many_arguments)]
fn canonical_no_effect_recovery_proof_digest(
    node_id: &AppReference,
    reservation_id: &AppReference,
    operation_key: &AppReference,
    installation_id: &AppInstallationId,
    intent_digest: &AppDigest,
    intent_payload_bytes: u64,
    command_digest: &AppDigest,
    response_digest: &AppDigest,
    response_payload_bytes: u64,
    actual: AppResourceQuantity,
    at_elapsed_ms: u64,
) -> Result<AppDigest, AppResourceAuthorityError> {
    let proof_bytes = canonical_json_bytes(&serde_json::json!({
        "schema": "app-resource-no-effect-committed-recovery/v1",
        "node_id": node_id,
        "reservation_id": reservation_id,
        "operation_key": operation_key,
        "installation_id": installation_id,
        "intent_digest": intent_digest,
        "intent_payload_bytes": intent_payload_bytes,
        "command_digest": command_digest,
        "response_digest": response_digest,
        "response_payload_bytes": response_payload_bytes,
        "actual": actual,
        "at_elapsed_ms": at_elapsed_ms,
    }))?;
    ensure_stored_record_bounded("no-effect committed recovery proof", &proof_bytes)?;
    Ok(AppDigest::blake3(&proof_bytes))
}

fn no_effect_recovery_observation_id(
    proof_digest: &AppDigest,
) -> Result<AppReference, AppResourceAuthorityError> {
    let proof_hex = proof_digest.as_str().strip_prefix("blake3:").ok_or(
        AppResourceAuthorityError::InvalidCommittedRecovery(
            "no-effect recovery proof digest is not canonical",
        ),
    )?;
    Ok(AppReference::parse(format!(
        "resource-no-effect-recovery:{proof_hex}"
    ))?)
}

#[allow(clippy::too_many_arguments)]
fn canonical_receipt_absence_proof_digest(
    node_id: &AppReference,
    reservation_id: &AppReference,
    operation_key: &AppReference,
    installation_id: &AppInstallationId,
    intent_digest: &AppDigest,
    origin: &AppMutationOrigin,
    mutation_key: &AppDigest,
    batch_digest: &AppDigest,
    actual: AppResourceQuantity,
    reconciled_at_elapsed_ms: u64,
) -> Result<AppDigest, AppResourceAuthorityError> {
    let proof_bytes = canonical_json_bytes(&serde_json::json!({
        "schema": "app-resource-receipt-absence-recovery/v1",
        "node_id": node_id,
        "reservation_id": reservation_id,
        "operation_key": operation_key,
        "installation_id": installation_id,
        "intent_digest": intent_digest,
        "origin": origin,
        "mutation_key": mutation_key,
        "batch_digest": batch_digest,
        "actual": actual,
        "reconciled_at_elapsed_ms": reconciled_at_elapsed_ms,
    }))?;
    ensure_stored_record_bounded("receipt-absence recovery proof", &proof_bytes)?;
    Ok(AppDigest::blake3(&proof_bytes))
}

fn ensure_conservative_post_io_charge_matches(
    journal: &AppResourceJournal,
    binding: &AppRunBinding,
    pending: &PendingResourceEvent,
    proof: &AppResourceConservativePostIoCharge,
) -> Result<(), AppResourceAuthorityError> {
    let binding_digest = AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(binding)?)?);
    let identity_digest = AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(
        &journal.identity,
    )?)?);
    let journal_revision = u64::try_from(journal.events.len())
        .map_err(|_| AppResourceAuthorityError::IntegerRange("journal_revision"))?;
    let expected_digest = canonical_conservative_post_io_charge_digest(
        &proof.node_id,
        &proof.reservation_id,
        &proof.operation_key,
        proof.requested,
        &proof.capability_requests,
        proof.effect_binding_digest.as_ref(),
        &proof.cleanup_binding_digest,
        &proof.accepted_identity_digest,
        proof.expected_journal_revision,
        &proof.evidence_ref,
        proof.evidence_revision,
        &proof.active_intervals,
        proof.at_elapsed_ms,
    )?;
    if proof.cleanup_binding_digest != binding_digest
        || proof.accepted_identity_digest != identity_digest
        || proof.expected_journal_revision != journal_revision
        || proof.proof_digest != expected_digest
        || proof.observation_id != conservative_post_io_observation_id(&expected_digest)?
    {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "conservative post-I/O charge proof does not match accepted cleanup authority",
        ));
    }
    let PendingResourceEvent::Settled(observation) = pending else {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "conservative post-I/O charge must append a settlement",
        ));
    };
    if observation.node_id != proof.node_id
        || observation.reservation_id != proof.reservation_id
        || observation.observation_id != proof.observation_id
        || observation.outcome != AppResourceSettlementOutcome::Committed
        || !matches!(
            observation.observation_sources.as_slice(),
            [AppResourceObservationSource::CrashReconciler]
        )
        || observation.actual != proof.requested
        || observation.capability_usage != proof.capability_requests
        || observation.effect_binding_digest != proof.effect_binding_digest
        || observation.active_intervals != proof.active_intervals
        || observation.at_elapsed_ms != proof.at_elapsed_ms
    {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "conservative post-I/O settlement does not match its proof",
        ));
    }
    let reserved = exact_recovery_reservation(
        journal,
        &proof.node_id,
        &proof.reservation_id,
        &proof.operation_key,
        "conservative post-I/O reservation",
    )?;
    if reserved.requested != proof.requested
        || reserved.capability_requests != proof.capability_requests
        || proof.active_intervals
            != conservative_active_intervals(
                reserved.created_at_elapsed_ms,
                reserved.expires_at_elapsed_ms,
                proof.at_elapsed_ms,
                reserved.node_closed_at_elapsed_ms,
            )
    {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "conservative post-I/O charge is not the exact reserved upper bound",
        ));
    }
    Ok(())
}

fn ensure_committed_recovery_matches(
    journal: &AppResourceJournal,
    binding: &AppRunBinding,
    pending: &PendingResourceEvent,
    proof: &AppResourceCommittedRecovery,
) -> Result<(), AppResourceAuthorityError> {
    if proof.installation_id != binding.installation_id {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "committed recovery installation",
            identity: proof.installation_id.to_string(),
        });
    }
    ensure_workflow_recovery_origin_matches(&proof.origin, binding, "committed recovery origin")?;
    let expected_digest = canonical_committed_recovery_proof_digest(
        &proof.node_id,
        &proof.reservation_id,
        &proof.operation_key,
        &proof.intent_digest,
        &proof.origin,
        &proof.mutation_key,
        &proof.batch_digest,
        &proof.receipt_id,
        &proof.receipt_digest,
        &proof.installation_id,
        proof.actual,
        proof.at_elapsed_ms,
    )?;
    if proof.proof_digest != expected_digest
        || proof.observation_id != committed_recovery_observation_id(&expected_digest)?
    {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "committed recovery proof digest does not match its contents",
        ));
    }
    let PendingResourceEvent::Settled(observation) = pending else {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "committed recovery must append a settlement",
        ));
    };
    let reserved = exact_recovery_reservation(
        journal,
        &proof.node_id,
        &proof.reservation_id,
        &proof.operation_key,
        "committed recovery reservation",
    )?;
    let expected_active_intervals = conservative_active_intervals(
        reserved.created_at_elapsed_ms,
        reserved.expires_at_elapsed_ms,
        proof.at_elapsed_ms,
        reserved.node_closed_at_elapsed_ms,
    );
    if observation.node_id != proof.node_id
        || observation.reservation_id != proof.reservation_id
        || observation.observation_id != proof.observation_id
        || observation.outcome != AppResourceSettlementOutcome::Committed
        || !matches!(
            observation.observation_sources.as_slice(),
            [AppResourceObservationSource::AppStoreTransaction]
        )
        || observation.actual != proof.actual
        || !observation.capability_usage.is_empty()
        || observation.active_intervals != expected_active_intervals
        || observation.at_elapsed_ms != proof.at_elapsed_ms
    {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "committed recovery settlement does not match its proof",
        ));
    }

    Ok(())
}

fn ensure_no_effect_recovery_matches(
    journal: &AppResourceJournal,
    binding: &AppRunBinding,
    pending: &PendingResourceEvent,
    proof: &AppResourceNoEffectCommittedRecovery,
) -> Result<(), AppResourceAuthorityError> {
    if proof.installation_id != binding.installation_id {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "no-effect recovery installation",
            identity: proof.installation_id.to_string(),
        });
    }
    if proof
        .intent_payload_bytes
        .checked_add(proof.response_payload_bytes)
        != Some(proof.actual.payload_bytes)
    {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "no-effect recovery payload does not match its proof-bound byte lengths",
        ));
    }
    let expected_digest = canonical_no_effect_recovery_proof_digest(
        &proof.node_id,
        &proof.reservation_id,
        &proof.operation_key,
        &proof.installation_id,
        &proof.intent_digest,
        proof.intent_payload_bytes,
        &proof.command_digest,
        &proof.response_digest,
        proof.response_payload_bytes,
        proof.actual,
        proof.at_elapsed_ms,
    )?;
    if proof.proof_digest != expected_digest
        || proof.observation_id != no_effect_recovery_observation_id(&expected_digest)?
    {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "no-effect recovery proof digest does not match its contents",
        ));
    }
    let PendingResourceEvent::Settled(observation) = pending else {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "no-effect recovery must append a settlement",
        ));
    };
    let reserved = exact_recovery_reservation(
        journal,
        &proof.node_id,
        &proof.reservation_id,
        &proof.operation_key,
        "no-effect recovery reservation",
    )?;
    let expected_active_intervals = conservative_active_intervals(
        reserved.created_at_elapsed_ms,
        reserved.expires_at_elapsed_ms,
        proof.at_elapsed_ms,
        reserved.node_closed_at_elapsed_ms,
    );
    if observation.node_id != proof.node_id
        || observation.reservation_id != proof.reservation_id
        || observation.observation_id != proof.observation_id
        || observation.outcome != AppResourceSettlementOutcome::Committed
        || !matches!(
            observation.observation_sources.as_slice(),
            [AppResourceObservationSource::WorkflowNoEffect]
        )
        || observation.actual != proof.actual
        || !observation.capability_usage.is_empty()
        || observation.active_intervals != expected_active_intervals
        || observation.at_elapsed_ms != proof.at_elapsed_ms
    {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "no-effect settlement does not match its proof",
        ));
    }
    Ok(())
}

fn ensure_receipt_absence_recovery_matches(
    transaction: &Transaction<'_>,
    journal: &AppResourceJournal,
    binding: &AppRunBinding,
    pending: &PendingResourceEvent,
    proof: &AppResourceReceiptAbsentReconciliation,
) -> Result<(), AppResourceAuthorityError> {
    if proof.installation_id != binding.installation_id {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "receipt-absence recovery installation",
            identity: proof.installation_id.to_string(),
        });
    }
    ensure_workflow_recovery_origin_matches(
        &proof.origin,
        binding,
        "receipt-absence recovery origin",
    )?;
    let receipt_present = transaction
        .query_row(
            "SELECT 1
               FROM app_mutation_receipts
              WHERE installation_id = ?1 AND idempotency_key = ?2
              LIMIT 1",
            params![proof.installation_id.as_str(), proof.mutation_key.as_str(),],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if receipt_present {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "canonical mutation receipt lookup did not prove absence",
        ));
    }
    let expected_digest = canonical_receipt_absence_proof_digest(
        &proof.node_id,
        &proof.reservation_id,
        &proof.operation_key,
        &proof.installation_id,
        &proof.intent_digest,
        &proof.origin,
        &proof.mutation_key,
        &proof.batch_digest,
        proof.actual,
        proof.reconciled_at_elapsed_ms,
    )?;
    if proof.proof_digest != expected_digest {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "receipt-absence proof digest does not match its contents",
        ));
    }
    let expected_hex = expected_digest.as_str().strip_prefix("blake3:").ok_or(
        AppResourceAuthorityError::InvalidCommittedRecovery(
            "receipt-absence proof digest is not canonical",
        ),
    )?;
    if proof.observation_id.as_str() != format!("resource-receipt-absent:{expected_hex}")
        || proof.reconciliation_ref.as_str()
            != format!("resource-receipt-absence-proof:{expected_hex}")
        || proof.reconciliation_revision.get() != 1
    {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "receipt-absence proof identity does not match its digest",
        ));
    }
    let PendingResourceEvent::Settled(observation) = pending else {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "receipt-absence recovery must append a settlement",
        ));
    };
    if observation.node_id != proof.node_id
        || observation.reservation_id != proof.reservation_id
        || observation.observation_id != proof.observation_id
        || observation.outcome != AppResourceSettlementOutcome::ProvenUnspent
        || !matches!(
            observation.observation_sources.as_slice(),
            [AppResourceObservationSource::CrashReconciler]
        )
        || observation.actual != AppResourceQuantity::default()
        || !observation.capability_usage.is_empty()
        || !observation.active_intervals.is_empty()
        || observation.at_elapsed_ms != proof.reconciled_at_elapsed_ms
    {
        return Err(AppResourceAuthorityError::InvalidCommittedRecovery(
            "receipt-absence settlement does not match its proof",
        ));
    }
    ensure_exact_recovery_reservation(
        journal,
        &proof.node_id,
        &proof.reservation_id,
        &proof.operation_key,
        "receipt-absence recovery reservation",
    )?;
    Ok(())
}

fn ensure_workflow_recovery_origin_matches(
    origin: &AppMutationOrigin,
    binding: &AppRunBinding,
    entity: &'static str,
) -> Result<(), AppResourceAuthorityError> {
    if !matches!(
        origin,
        AppMutationOrigin::Workflow { execution_id, .. }
            if execution_id == &binding.execution_id
    ) {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity,
            identity: binding.execution_id.to_string(),
        });
    }
    Ok(())
}

fn ensure_exact_recovery_reservation(
    journal: &AppResourceJournal,
    expected_node_id: &AppReference,
    expected_reservation_id: &AppReference,
    expected_operation_key: &AppReference,
    entity: &'static str,
) -> Result<AppResourceQuantity, AppResourceAuthorityError> {
    Ok(exact_recovery_reservation(
        journal,
        expected_node_id,
        expected_reservation_id,
        expected_operation_key,
        entity,
    )?
    .requested)
}

fn exact_recovery_reservation(
    journal: &AppResourceJournal,
    expected_node_id: &AppReference,
    expected_reservation_id: &AppReference,
    expected_operation_key: &AppReference,
    entity: &'static str,
) -> Result<AppResourcePendingReservation, AppResourceAuthorityError> {
    for event in &journal.events {
        let AppResourceJournalEvent::Reserved {
            node_id,
            reservation_id,
            operation_key,
            requested,
            capability_requests,
            at_elapsed_ms,
            expires_at_elapsed_ms,
            ..
        } = event
        else {
            continue;
        };
        if reservation_id != expected_reservation_id && operation_key != expected_operation_key {
            continue;
        }
        if node_id != expected_node_id
            || reservation_id != expected_reservation_id
            || operation_key != expected_operation_key
        {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity,
                identity: expected_reservation_id.to_string(),
            });
        }
        return Ok(AppResourcePendingReservation {
            node_id: node_id.clone(),
            reservation_id: reservation_id.clone(),
            operation_key: operation_key.clone(),
            requested: *requested,
            capability_requests: capability_requests.clone(),
            effect_binding_digest: journal.events.iter().find_map(|event| match event {
                AppResourceJournalEvent::EffectDispatchStarted {
                    reservation_id: started_reservation_id,
                    effect_binding_digest,
                    ..
                } if started_reservation_id == reservation_id => {
                    Some(effect_binding_digest.clone())
                },
                _ => None,
            }),
            effect_dispatch_started_at_elapsed_ms: journal.events.iter().find_map(|event| {
                match event {
                    AppResourceJournalEvent::EffectDispatchStarted {
                        reservation_id: started_reservation_id,
                        at_elapsed_ms,
                        ..
                    } if started_reservation_id == reservation_id => Some(*at_elapsed_ms),
                    _ => None,
                }
            }),
            status: AppResourcePendingReservationStatus::Held,
            created_at_elapsed_ms: *at_elapsed_ms,
            expires_at_elapsed_ms: *expires_at_elapsed_ms,
            node_closed_at_elapsed_ms: journal.events.iter().find_map(|event| match event {
                AppResourceJournalEvent::NodeClosed {
                    node_id: closed_node_id,
                    at_elapsed_ms,
                    ..
                } if closed_node_id == node_id => Some(*at_elapsed_ms),
                _ => None,
            }),
        });
    }
    Err(AppResourceAuthorityError::StaleRegistryAuthority(entity))
}

fn conservative_active_intervals(
    created_at_elapsed_ms: u64,
    expires_at_elapsed_ms: u64,
    observed_at_elapsed_ms: u64,
    node_closed_at_elapsed_ms: Option<u64>,
) -> Vec<AppActiveInterval> {
    let end_elapsed_ms = expires_at_elapsed_ms
        .min(observed_at_elapsed_ms)
        .min(node_closed_at_elapsed_ms.unwrap_or(u64::MAX));
    (end_elapsed_ms > created_at_elapsed_ms)
        .then_some(AppActiveInterval {
            start_elapsed_ms: created_at_elapsed_ms,
            end_elapsed_ms,
        })
        .into_iter()
        .collect()
}

fn update_cleanup_fence(
    cleanup: &mut AppResourceAcceptedCleanupLease,
    receipt: &AppResourceMutationReceipt,
) {
    cleanup.fence = AppResourceMutationFence {
        expected_journal_revision: receipt.state.journal_revision,
        minimum_period_revision: receipt.current_period_revision,
    };
    cleanup.elapsed_high_water_ms = cleanup
        .elapsed_high_water_ms
        .max(receipt.state.evaluated_at_elapsed_ms);
}

fn update_cleanup_after_reservation_reconciliation(
    cleanup: &mut AppResourceAcceptedCleanupLease,
    receipt: &AppResourceMutationReceipt,
    reservation_id: &AppReference,
) -> Result<(), AppResourceAuthorityError> {
    update_cleanup_fence(cleanup, receipt);
    let index = cleanup
        .pending_reservations
        .iter()
        .position(|pending| &pending.reservation_id == reservation_id)
        .ok_or_else(|| AppResourceAuthorityError::IdentityConflict {
            entity: "accepted cleanup reservation",
            identity: reservation_id.to_string(),
        })?;
    cleanup.pending_reservations.remove(index);
    let expected_pending = usize::try_from(
        receipt
            .state
            .held_reservations
            .checked_add(receipt.state.uncertain_reservations)
            .ok_or(AppResourceAuthorityError::IntegerRange(
                "pending_reservation_count",
            ))?,
    )
    .map_err(|_| AppResourceAuthorityError::IntegerRange("pending_reservation_count"))?;
    let scoped_and_unscoped = cleanup
        .pending_reservations
        .len()
        .checked_add(cleanup.unscoped_pending_reservations)
        .ok_or(AppResourceAuthorityError::IntegerRange(
            "pending_reservation_count",
        ))?;
    if scoped_and_unscoped != expected_pending {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "accepted cleanup pending reservation index diverged from canonical state".to_owned(),
        ));
    }
    Ok(())
}

fn pending_reservations_for_cleanup(
    journal: &AppResourceJournal,
    max_reservations: u32,
) -> Result<Vec<AppResourcePendingReservation>, AppResourceAuthorityError> {
    let mut pending = BTreeMap::<AppReference, AppResourcePendingReservation>::new();
    for event in &journal.events {
        match event {
            AppResourceJournalEvent::Reserved {
                node_id,
                reservation_id,
                operation_key,
                requested,
                capability_requests,
                at_elapsed_ms,
                expires_at_elapsed_ms,
                ..
            } => {
                if pending.len()
                    >= usize::try_from(max_reservations)
                        .map_err(|_| AppResourceAuthorityError::IntegerRange("max_reservations"))?
                {
                    return Err(AppResourceAuthorityError::CorruptAuthority(
                        "accepted cleanup reservation index exceeds its bounded policy".to_owned(),
                    ));
                }
                if pending
                    .insert(
                        reservation_id.clone(),
                        AppResourcePendingReservation {
                            node_id: node_id.clone(),
                            reservation_id: reservation_id.clone(),
                            operation_key: operation_key.clone(),
                            requested: *requested,
                            capability_requests: capability_requests.clone(),
                            effect_binding_digest: None,
                            effect_dispatch_started_at_elapsed_ms: None,
                            status: AppResourcePendingReservationStatus::Held,
                            created_at_elapsed_ms: *at_elapsed_ms,
                            expires_at_elapsed_ms: *expires_at_elapsed_ms,
                            node_closed_at_elapsed_ms: None,
                        },
                    )
                    .is_some()
                {
                    return Err(AppResourceAuthorityError::CorruptAuthority(
                        "accepted cleanup journal reuses a reservation identity".to_owned(),
                    ));
                }
            },
            AppResourceJournalEvent::NodeClosed {
                node_id,
                at_elapsed_ms,
                ..
            } => {
                for reservation in pending
                    .values_mut()
                    .filter(|pending| &pending.node_id == node_id)
                {
                    reservation.node_closed_at_elapsed_ms = Some(*at_elapsed_ms);
                }
            },
            AppResourceJournalEvent::EffectDispatchStarted {
                reservation_id,
                effect_binding_digest,
                at_elapsed_ms,
                ..
            } => {
                let reservation = pending.get_mut(reservation_id).ok_or_else(|| {
                    AppResourceAuthorityError::CorruptAuthority(
                        "accepted cleanup journal starts an unknown effect reservation".to_owned(),
                    )
                })?;
                if reservation
                    .effect_binding_digest
                    .replace(effect_binding_digest.clone())
                    .is_some()
                {
                    return Err(AppResourceAuthorityError::CorruptAuthority(
                        "accepted cleanup journal duplicates an effect dispatch".to_owned(),
                    ));
                }
                reservation.effect_dispatch_started_at_elapsed_ms = Some(*at_elapsed_ms);
                reservation.status = AppResourcePendingReservationStatus::OutcomeUncertain;
            },
            AppResourceJournalEvent::EffectDispatchAbortedBeforeIo {
                reservation_id,
                effect_binding_digest,
                ..
            } => {
                let reservation = pending.remove(reservation_id).ok_or_else(|| {
                    AppResourceAuthorityError::CorruptAuthority(
                        "accepted cleanup journal aborts an unknown effect reservation".to_owned(),
                    )
                })?;
                if reservation.status != AppResourcePendingReservationStatus::OutcomeUncertain
                    || reservation.effect_binding_digest.as_ref() != Some(effect_binding_digest)
                {
                    return Err(AppResourceAuthorityError::CorruptAuthority(
                        "accepted cleanup effect abort does not match its dispatch start"
                            .to_owned(),
                    ));
                }
            },
            AppResourceJournalEvent::Settled {
                reservation_id,
                outcome,
                ..
            } => match outcome {
                AppResourceSettlementOutcome::OutcomeUncertain => {
                    let reservation = pending.get_mut(reservation_id).ok_or_else(|| {
                        AppResourceAuthorityError::CorruptAuthority(
                            "accepted cleanup journal marks an unknown reservation uncertain"
                                .to_owned(),
                        )
                    })?;
                    reservation.status = AppResourcePendingReservationStatus::OutcomeUncertain;
                },
                AppResourceSettlementOutcome::Committed
                | AppResourceSettlementOutcome::ProvenUnspent => {
                    if pending.remove(reservation_id).is_none() {
                        return Err(AppResourceAuthorityError::CorruptAuthority(
                            "accepted cleanup journal settles an unknown reservation".to_owned(),
                        ));
                    }
                },
            },
            _ => {},
        }
    }
    Ok(pending.into_values().collect())
}

fn conservative_post_io_charge_from_cleanup(
    cleanup: &mut AppResourceAcceptedCleanupLease,
    reservation_id: &AppReference,
    now: DateTime<Utc>,
) -> Result<AppResourceConservativePostIoCharge, AppResourceAuthorityError> {
    if cleanup.cleanup_reason != AppResourceAcceptedCleanupReason::CrashRecovery {
        return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
            "conservative post-I/O charge requires canonical crash-recovery ownership",
        ));
    }
    let pending = cleanup
        .pending_reservations
        .iter()
        .find(|pending| &pending.reservation_id == reservation_id)
        .cloned()
        .ok_or_else(|| AppResourceAuthorityError::IdentityConflict {
            entity: "accepted cleanup reservation",
            identity: reservation_id.to_string(),
        })?;
    let at_elapsed_ms = cleanup.clamp_elapsed(now);
    let active_intervals = conservative_active_intervals(
        pending.created_at_elapsed_ms,
        pending.expires_at_elapsed_ms,
        at_elapsed_ms,
        pending.node_closed_at_elapsed_ms,
    );
    let accepted_identity_digest = AppDigest::blake3(&canonical_json_bytes(
        &serde_json::to_value(&cleanup.identity)?,
    )?);
    let proof_digest = canonical_conservative_post_io_charge_digest(
        &pending.node_id,
        &pending.reservation_id,
        &pending.operation_key,
        pending.requested,
        &pending.capability_requests,
        pending.effect_binding_digest.as_ref(),
        &cleanup.cleanup_binding_digest,
        &accepted_identity_digest,
        cleanup.fence.expected_journal_revision,
        &cleanup.cleanup_evidence_ref,
        cleanup.cleanup_evidence_revision,
        &active_intervals,
        at_elapsed_ms,
    )?;
    let observation_id = conservative_post_io_observation_id(&proof_digest)?;
    Ok(AppResourceConservativePostIoCharge {
        node_id: pending.node_id.clone(),
        reservation_id: pending.reservation_id.clone(),
        operation_key: pending.operation_key.clone(),
        requested: pending.requested,
        capability_requests: pending.capability_requests.clone(),
        effect_binding_digest: pending.effect_binding_digest.clone(),
        cleanup_binding_digest: cleanup.cleanup_binding_digest.clone(),
        accepted_identity_digest,
        expected_journal_revision: cleanup.fence.expected_journal_revision,
        evidence_ref: cleanup.cleanup_evidence_ref.clone(),
        evidence_revision: cleanup.cleanup_evidence_revision,
        active_intervals,
        at_elapsed_ms,
        proof_digest,
        observation_id,
    })
}

fn ensure_binding_matches_accepted_identity(
    scope: &AppScope,
    binding: &AppRunBinding,
    identity: &AppResourceTreeIdentity,
) -> Result<(), AppResourceAuthorityError> {
    binding.validate_app_contract(&AppContractLimits::default())?;
    if identity.scope != *scope
        || binding.scope != *scope
        || identity.installation_id != binding.installation_id
        || identity.package_revision_ref != binding.package_revision_ref
        || identity.schema_revision != binding.schema_revision
        || identity.authority_digest != binding.authority_digest
        || identity.behavior_resource_identity != binding.behavior_resource_identity
        || identity.root_execution_id != binding.execution_id
        || identity.budget_ledger_ref != binding.budget_ledger_ref
    {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "accepted cleanup root",
            identity: binding.budget_ledger_ref.to_string(),
        });
    }
    Ok(())
}

fn accepted_cleanup_root_lane_blocking(
    connection: &Connection,
    scope: &AppScope,
    binding: &AppRunBinding,
) -> Result<Option<AppResourceExecutionLane>, AppResourceAuthorityError> {
    let row = connection
        .query_row(
            "SELECT lane, substr(identity_json, 1, ?4), length(identity_json)
               FROM app_resource_trees
              WHERE budget_ledger_ref = ?1
                AND installation_id = ?2
                AND root_execution_id = ?3",
            params![
                binding.budget_ledger_ref.as_str(),
                binding.installation_id.as_str(),
                binding.execution_id.as_str(),
                i64::try_from(MAX_STORED_RESOURCE_RECORD_BYTES + 1).map_err(|_| {
                    AppResourceAuthorityError::IntegerRange("resource_record_bytes")
                })?,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((lane, identity_json, identity_len)) = row else {
        return Ok(None);
    };
    ensure_stored_record_length_bounded("accepted cleanup identity", identity_len)?;
    ensure_stored_record_bounded("accepted cleanup identity", &identity_json)?;
    let identity: AppResourceTreeIdentity =
        decode_app_contract(&identity_json, &AppContractLimits::default())?;
    ensure_binding_matches_accepted_identity(scope, binding, &identity)?;
    Ok(Some(parse_lane(&lane)?))
}

fn accepted_cleanup_resolved_authority(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    binding: &AppRunBinding,
    accepted: &AppResourceAcceptedCleanupSnapshot,
    identity: &AppResourceTreeIdentity,
    now: DateTime<Utc>,
) -> Result<ResolvedAppAuthority, AppResourceAuthorityError> {
    ensure_binding_matches_accepted_identity(scope, binding, identity)?;
    if accepted.scope_binding_ref != *authenticated_scope.scope_binding_ref()
        || accepted.installation_generation != identity.installation_generation
        || accepted.surface_revision.is_some()
    {
        return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
            "accepted cleanup snapshot does not match the immutable workflow root",
        ));
    }
    let grant_authority_digest = transaction
        .query_row(
            "SELECT authority_digest FROM app_grant_revisions
              WHERE installation_id = ?1 AND revision = ?2",
            params![
                identity.installation_id.as_str(),
                to_sql_i64(identity.grant_revision.get(), "grant_revision")?,
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(AppResourceAuthorityError::StaleRegistryAuthority(
            "accepted cleanup grant",
        ))?;
    let resolved = ResolvedAppAuthority {
        scope_binding_ref: accepted.scope_binding_ref.clone(),
        actor_ref: authenticated_scope.actor_ref().clone(),
        session_ref: authenticated_scope.session_ref().clone(),
        authentication: authenticated_scope.authentication(),
        authentication_revision: authenticated_scope.authentication_revision(),
        installation_id: identity.installation_id.clone(),
        installation_generation: identity.installation_generation,
        package_revision_ref: identity.package_revision_ref.clone(),
        grant_revision: identity.grant_revision,
        grant_authority_digest: AppDigest::parse(grant_authority_digest)?,
        schema_revision: identity.schema_revision,
        surface_revision: None,
        authority_digest: identity.authority_digest.clone(),
        effective_tools: accepted.ceiling.tools.clone(),
        effective_context_reads: accepted.ceiling.context_reads.clone(),
        effective_data_handling_policy: accepted.ceiling.data_handling_policy.clone(),
        effective_background_execution: accepted.ceiling.background_execution.clone(),
        effective_network_policy: accepted.ceiling.network_policy.clone(),
        effective_resources: accepted.ceiling.resources.clone(),
        effective_any_public_host: false,
        resolved_at: now,
    };
    if resolved
        .canonical_authority_digest()
        .map_err(|error| AppResourceAuthorityError::CorruptAuthority(error.to_string()))?
        != identity.authority_digest
    {
        return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
            "accepted cleanup ceiling does not match the canonical root digest",
        ));
    }
    ensure_registry_accepted_authority_identity(transaction, scope, &resolved)?;
    Ok(resolved)
}

/// Validate the immutable package row without consulting an active
/// installation pointer. Cleanup can outlive revocation or an upgrade, but it
/// must remain bound to the exact content and dependency lock that were
/// admitted with the accepted resource tree.
fn ensure_historical_package_identity(
    transaction: &Transaction<'_>,
    expected_package_revision_ref: &AppReference,
) -> Result<(), AppResourceAuthorityError> {
    let (
        stored_package_id,
        stored_semantic_version,
        stored_content_digest,
        stored_publisher_identity,
        stored_lock_digest,
        package_bytes,
        dependency_lock_bytes,
    ) = transaction
        .query_row(
            "SELECT package_id, semantic_version, content_digest,
                    publisher_identity, dependency_lock_digest, record_json,
                    dependency_lock_json
               FROM app_package_revisions
              WHERE package_revision_ref = ?1",
            params![expected_package_revision_ref.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                ))
            },
        )
        .optional()?
        .ok_or(AppResourceAuthorityError::StaleRegistryAuthority(
            "accepted package revision",
        ))?;
    let package: AppPackageRevision =
        decode_app_contract(&package_bytes, &AppContractLimits::default())?;
    validate_accepted_cleanup_package_lock(
        &dependency_lock_bytes,
        &package.dependency_lock_digest,
        &package.content_digest,
    )
    .map_err(|error| AppResourceAuthorityError::CorruptAuthority(error.to_string()))?;
    let canonical_ref = canonical_package_revision_ref(&package)?;
    if canonical_ref != *expected_package_revision_ref
        || stored_package_id != package.package_id.as_str()
        || stored_semantic_version != package.semantic_version
        || stored_content_digest != package.content_digest.as_str()
        || stored_publisher_identity != package.publisher_identity.as_str()
        || stored_lock_digest != package.dependency_lock_digest.as_str()
    {
        return Err(AppResourceAuthorityError::StaleRegistryAuthority(
            "accepted package identity",
        ));
    }
    Ok(())
}

fn cleanup_nodes(
    journal: &AppResourceJournal,
    cleanup_scope: &AppResourceAcceptedCleanupScope,
) -> Result<(VecDeque<AppReference>, BTreeSet<AppReference>), AppResourceAuthorityError> {
    let mut nodes = BTreeMap::<AppReference, (u16, bool, Option<AppReference>)>::new();
    for event in &journal.events {
        match event {
            AppResourceJournalEvent::NodeOpened {
                node_id,
                parent_node_id,
                ..
            } => {
                let depth = match parent_node_id {
                    Some(parent) => nodes
                        .get(parent)
                        .ok_or_else(|| {
                            AppResourceAuthorityError::CorruptAuthority(
                                "accepted cleanup node precedes its parent".to_owned(),
                            )
                        })?
                        .0
                        .checked_add(1)
                        .ok_or(AppResourceAuthorityError::IntegerRange(
                            "accepted_cleanup_depth",
                        ))?,
                    None => 0,
                };
                if nodes
                    .insert(node_id.clone(), (depth, true, parent_node_id.clone()))
                    .is_some()
                {
                    return Err(AppResourceAuthorityError::CorruptAuthority(
                        "accepted cleanup journal reopens a node identity".to_owned(),
                    ));
                }
            },
            AppResourceJournalEvent::NodeClosed { node_id, .. } => {
                let node = nodes.get_mut(node_id).ok_or_else(|| {
                    AppResourceAuthorityError::CorruptAuthority(
                        "accepted cleanup journal closes an unknown node".to_owned(),
                    )
                })?;
                node.1 = false;
            },
            _ => {},
        }
    }
    let selected = match cleanup_scope {
        AppResourceAcceptedCleanupScope::WholeTree => nodes.keys().cloned().collect(),
        AppResourceAcceptedCleanupScope::ExecutionSubtree(target) => {
            if !nodes.contains_key(target) {
                return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
                    "accepted cleanup execution node is unavailable",
                ));
            }
            let mut selected = BTreeSet::new();
            for node_id in nodes.keys() {
                let mut cursor = Some(node_id);
                let mut remaining = nodes.len().saturating_add(1);
                while let Some(current) = cursor {
                    if current == target {
                        selected.insert(node_id.clone());
                        break;
                    }
                    if remaining == 0 {
                        return Err(AppResourceAuthorityError::CorruptAuthority(
                            "accepted cleanup node ancestry is cyclic".to_owned(),
                        ));
                    }
                    remaining -= 1;
                    cursor = nodes
                        .get(current)
                        .ok_or_else(|| {
                            AppResourceAuthorityError::CorruptAuthority(
                                "accepted cleanup node ancestry is incomplete".to_owned(),
                            )
                        })?
                        .2
                        .as_ref();
                }
            }
            selected
        },
    };
    let mut open = nodes
        .iter()
        .filter_map(|(node_id, (depth, is_open, _))| {
            (*is_open && selected.contains(node_id)).then_some((*depth, node_id.clone()))
        })
        .collect::<Vec<_>>();
    open.sort_unstable_by(|(left_depth, left_id), (right_depth, right_id)| {
        right_depth
            .cmp(left_depth)
            .then_with(|| left_id.cmp(right_id))
    });
    Ok((
        open.into_iter().map(|(_, node_id)| node_id).collect(),
        selected,
    ))
}

#[allow(clippy::too_many_arguments)]
fn reacquire_accepted_cleanup_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    binding: AppRunBinding,
    accepted: AppResourceAcceptedCleanupSnapshot,
    proof: AppResourceAcceptedCleanupProof,
    policy: AppResourceEnforcementPolicy,
    scheduler_guard: AppResourceSchedulerAdmissionGuard,
    now: DateTime<Utc>,
) -> Result<AppResourceAcceptedCleanupReceipt, AppResourceAuthorityError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = load_existing_root(&transaction, &binding.budget_ledger_ref)?.ok_or(
        AppResourceAuthorityError::StaleRegistryAuthority("accepted cleanup root"),
    )?;
    let journal_revision = u64::try_from(existing.journal.events.len())
        .map_err(|_| AppResourceAuthorityError::IntegerRange("journal_revision"))?;
    let binding_digest =
        AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(&binding)?)?);
    if proof.binding_digest != binding_digest || proof.expected_journal_revision != journal_revision
    {
        return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
            "cleanup proof is stale or belongs to another task execution",
        ));
    }
    // The move-only proof is an admission credential, not persisted state. Its
    // typed owner has already bound independent terminal/cancel/crash evidence
    // to this exact task and journal revision. The narrower lease retains only
    // this content-free owner identity so crash cleanup can conservatively
    // account an otherwise unverifiable post-I/O reservation. It remains
    // non-serializable and grants no dispatch authority.
    let cleanup_reason = proof.reason;
    let cleanup_scope = proof.cleanup_scope;
    let cleanup_binding_digest = proof.binding_digest;
    let cleanup_evidence_ref = proof.evidence_ref;
    let cleanup_evidence_revision = proof.evidence_revision;
    if scheduler_guard.lane != existing.lane {
        return Err(AppResourceAuthorityError::InvalidCleanupAuthority(
            "accepted cleanup scheduler lane changed during reacquisition",
        ));
    }
    let resolved = accepted_cleanup_resolved_authority(
        &transaction,
        scope,
        authenticated_scope,
        &binding,
        &accepted,
        &existing.journal.identity,
        now,
    )?;
    let (period_revision, _) = load_period_revision(
        &transaction,
        &resolved,
        &existing.journal.identity.installation_period_ref,
    )?;
    let totals = load_period_totals(
        &transaction,
        &resolved,
        &existing.journal.identity.installation_period_ref,
        Some(&existing),
    )?;
    let behavior_period = load_behavior_period_baseline(
        &transaction,
        &resolved,
        &existing.journal.identity.installation_period_ref,
        binding.behavior_resource_identity.as_ref(),
        Some(&existing),
    )?;
    let baseline = AppResourcePeriodBaseline::from_resource_store(
        scope.clone(),
        resolved.installation_id.clone(),
        resolved.installation_generation,
        binding.budget_ledger_ref.clone(),
        existing.journal.identity.installation_period_ref.clone(),
        period_revision,
        existing.period_ends_at_elapsed_ms,
        totals.committed_tokens,
        totals.outstanding_tokens,
        totals.committed_cost_microusd,
        totals.outstanding_cost_microusd,
        behavior_period,
        totals.background_starts,
        totals.foreground_runs,
        totals.background_runs,
        scheduler_guard.foreground_runs_excluding_root,
        scheduler_guard.background_runs_excluding_root,
        existing.package_bytes,
    );
    let current = CurrentAppResourceAuthority::from_accepted_execution(
        authenticated_scope,
        &resolved,
        &binding,
        policy,
        baseline,
        &existing.journal.identity,
    )?;
    let recovery = load_recovery_evidence(&transaction, &binding.budget_ledger_ref)?;
    let recovery_refs = recovery.iter().collect::<Vec<_>>();
    let assessment =
        assess_app_resource_journal_with_recovery(&existing.journal, &current, &recovery_refs)?;
    let state = AppResourceTreeState::from_assessment(&assessment, journal_revision);
    ensure_tree_index_matches(&existing, &state)?;
    let (open_nodes_leaf_first, cleanup_node_ids) =
        cleanup_nodes(&existing.journal, &cleanup_scope)?;
    let all_pending_reservations =
        pending_reservations_for_cleanup(&existing.journal, policy.max_reservations)?;
    let expected_pending = usize::try_from(
        state
            .held_reservations
            .checked_add(state.uncertain_reservations)
            .ok_or(AppResourceAuthorityError::IntegerRange(
                "pending_reservation_count",
            ))?,
    )
    .map_err(|_| AppResourceAuthorityError::IntegerRange("pending_reservation_count"))?;
    if all_pending_reservations.len() != expected_pending {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "accepted cleanup pending reservation index does not match canonical assessment"
                .to_owned(),
        ));
    }
    let (pending_reservations, unscoped_pending_reservations): (Vec<_>, Vec<_>) =
        all_pending_reservations
            .into_iter()
            .partition(|pending| cleanup_node_ids.contains(&pending.node_id));
    let identity = existing.journal.identity;
    let cleanup_complete = open_nodes_leaf_first.is_empty() && pending_reservations.is_empty();
    if cleanup_scope.requires_terminal_settlement() && cleanup_complete && !state.terminally_settled
    {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "whole-tree cleanup completed without terminal resource settlement".to_owned(),
        ));
    }
    let cleanup_lease = (!cleanup_complete).then_some(AppResourceAcceptedCleanupLease {
        identity,
        binding,
        resolved,
        policy,
        fence: AppResourceMutationFence {
            expected_journal_revision: state.journal_revision,
            minimum_period_revision: period_revision,
        },
        started_at: existing.started_at,
        elapsed_high_water_ms: state.evaluated_at_elapsed_ms,
        period_ends_at_elapsed_ms: existing.period_ends_at_elapsed_ms,
        package_bytes: existing.package_bytes,
        open_nodes_leaf_first,
        pending_reservations,
        unscoped_pending_reservations: unscoped_pending_reservations.len(),
        cleanup_reason,
        cleanup_scope,
        cleanup_binding_digest,
        cleanup_evidence_ref,
        cleanup_evidence_revision,
        _scheduler_guard: scheduler_guard,
    });
    transaction.commit()?;
    Ok(AppResourceAcceptedCleanupReceipt {
        state,
        cleanup_lease,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_event_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    authority: AppResourceMutationAuthority,
    mut pending: PendingResourceEvent,
    recovery: Option<PendingResourceRecovery>,
    now: DateTime<Utc>,
) -> Result<AppResourceMutationReceipt, AppResourceAuthorityError> {
    let AppResourceMutationAuthority {
        resolved,
        binding,
        policy,
        snapshot,
        fence,
        authoritative_elapsed_ms,
        accepted_identity,
    } = authority;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if accepted_identity.is_some() {
        if !matches!(
            &pending,
            PendingResourceEvent::Settled(_)
                | PendingResourceEvent::EffectDispatchAbortedBeforeIo { .. }
                | PendingResourceEvent::NodeClosed(_)
        ) {
            return Err(AppResourceAuthorityError::InvalidObservation(
                "accepted execution authority may only settle or close existing work",
            ));
        }
        ensure_registry_accepted_authority_identity(&transaction, scope, &resolved)?;
    } else {
        ensure_registry_authority_current(&transaction, scope, &resolved)?;
    }
    if snapshot.package_revision_ref != resolved.package_revision_ref {
        return Err(AppResourceAuthorityError::InvalidAdmissionSnapshot(
            "package measurement does not match current package authority",
        ));
    }
    let existing = load_existing_root(&transaction, &binding.budget_ledger_ref)?.ok_or(
        AppResourceAuthorityError::StaleRegistryAuthority("resource root"),
    )?;
    if existing.period_ends_at_elapsed_ms != snapshot.period_ends_at_elapsed_ms
        || existing.package_bytes != snapshot.package_bytes
    {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "resource root runtime baseline",
            identity: binding.budget_ledger_ref.to_string(),
        });
    }
    if accepted_identity
        .as_ref()
        .is_some_and(|accepted| accepted != &existing.journal.identity)
    {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "accepted resource tree",
            identity: binding.budget_ledger_ref.to_string(),
        });
    }
    if existing.journal.identity.scope != *scope
        || existing.journal.identity.installation_id != resolved.installation_id
        || existing.journal.identity.installation_generation != resolved.installation_generation
        || existing.journal.identity.package_revision_ref != resolved.package_revision_ref
        || existing.journal.identity.grant_revision != resolved.grant_revision
        || existing.journal.identity.schema_revision != resolved.schema_revision
        || existing.journal.identity.authority_digest != resolved.authority_digest
        || existing.journal.identity.behavior_resource_identity
            != binding.behavior_resource_identity
        || existing.journal.identity.root_execution_id != binding.execution_id
        || existing.journal.identity.installation_period_ref != snapshot.period_ref
    {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "budget ledger",
            identity: binding.budget_ledger_ref.to_string(),
        });
    }
    let (period_revision, admissions_closed) =
        load_period_revision(&transaction, &resolved, &snapshot.period_ref)?;
    let minimum_period_revision = fence.minimum_period_revision.get();
    if period_revision.get() < minimum_period_revision {
        return Err(AppResourceAuthorityError::PeriodRevisionConflict {
            minimum: minimum_period_revision,
            actual: period_revision.get(),
        });
    }
    let totals = load_period_totals(
        &transaction,
        &resolved,
        &snapshot.period_ref,
        Some(&existing),
    )?;
    let behavior_period = load_behavior_period_baseline(
        &transaction,
        &resolved,
        &snapshot.period_ref,
        binding.behavior_resource_identity.as_ref(),
        Some(&existing),
    )?;
    let baseline = AppResourcePeriodBaseline::from_resource_store(
        scope.clone(),
        resolved.installation_id.clone(),
        resolved.installation_generation,
        binding.budget_ledger_ref.clone(),
        snapshot.period_ref.clone(),
        period_revision,
        existing.period_ends_at_elapsed_ms,
        totals.committed_tokens,
        totals.outstanding_tokens,
        totals.committed_cost_microusd,
        totals.outstanding_cost_microusd,
        behavior_period,
        totals.background_starts,
        totals.foreground_runs,
        totals.background_runs,
        snapshot.scheduler_foreground_runs_excluding_root,
        snapshot.scheduler_background_runs_excluding_root,
        existing.package_bytes,
    );
    let current = if let Some(accepted_identity) = accepted_identity.as_ref() {
        CurrentAppResourceAuthority::from_accepted_execution(
            authenticated_scope,
            &resolved,
            &binding,
            policy,
            baseline,
            accepted_identity,
        )?
    } else {
        CurrentAppResourceAuthority::from_current_authority(
            authenticated_scope,
            &resolved,
            &binding,
            policy,
            baseline,
            now,
        )?
    };
    if &existing.journal.identity != current.identity() {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "budget ledger",
            identity: binding.budget_ledger_ref.to_string(),
        });
    }
    let stored_recovery = load_recovery_evidence(&transaction, &binding.budget_ledger_ref)?;
    let stored_recovery_refs: Vec<_> = stored_recovery.iter().collect();
    let current_revision = u64::try_from(existing.journal.events.len())
        .map_err(|_| AppResourceAuthorityError::IntegerRange("journal_revision"))?;
    let lane = existing.lane;

    if let (Some(PendingResourceRecovery::Committed(proof)), PendingResourceEvent::Settled(value)) =
        (recovery.as_ref(), &mut pending)
    {
        let reserved = exact_recovery_reservation(
            &existing.journal,
            &proof.node_id,
            &proof.reservation_id,
            &proof.operation_key,
            "committed recovery reservation",
        )?;
        value.active_intervals = conservative_active_intervals(
            reserved.created_at_elapsed_ms,
            reserved.expires_at_elapsed_ms,
            proof.at_elapsed_ms,
            reserved.node_closed_at_elapsed_ms,
        );
    }
    if let (
        Some(PendingResourceRecovery::NoEffectCommitted(proof)),
        PendingResourceEvent::Settled(value),
    ) = (recovery.as_ref(), &mut pending)
    {
        let reserved = exact_recovery_reservation(
            &existing.journal,
            &proof.node_id,
            &proof.reservation_id,
            &proof.operation_key,
            "no-effect recovery reservation",
        )?;
        value.active_intervals = conservative_active_intervals(
            reserved.created_at_elapsed_ms,
            reserved.expires_at_elapsed_ms,
            proof.at_elapsed_ms,
            reserved.node_closed_at_elapsed_ms,
        );
    }
    if let Some(PendingResourceRecovery::Committed(proof)) = recovery.as_ref() {
        ensure_committed_recovery_matches(&existing.journal, &binding, &pending, proof)?;
    }
    if let Some(PendingResourceRecovery::ConservativePostIo(proof)) = recovery.as_ref() {
        ensure_conservative_post_io_charge_matches(&existing.journal, &binding, &pending, proof)?;
    }
    if let Some(PendingResourceRecovery::NoEffectCommitted(proof)) = recovery.as_ref() {
        ensure_no_effect_recovery_matches(&existing.journal, &binding, &pending, proof)?;
    }
    if let Some(PendingResourceRecovery::ReceiptAbsent(proof)) = recovery.as_ref() {
        ensure_receipt_absence_recovery_matches(
            &transaction,
            &existing.journal,
            &binding,
            &pending,
            proof,
        )?;
    }

    if pending.find_exact(&existing.journal, lane)?.is_some() {
        if let Some(PendingResourceRecovery::ProvenUnspent(recovery)) = recovery.as_ref() {
            ensure_recovery_replay_matches(&transaction, &binding.budget_ledger_ref, recovery)?;
        }
        if let Some(PendingResourceRecovery::ReceiptAbsent(proof)) = recovery.as_ref() {
            let evidence = proof.as_crash_reconciliation();
            ensure_recovery_replay_matches(&transaction, &binding.budget_ledger_ref, &evidence)?;
        }
        let replay_assessment = assess_app_resource_journal_with_recovery(
            &existing.journal,
            &current,
            &stored_recovery_refs,
        )?;
        let state = AppResourceTreeState::from_assessment(&replay_assessment, current_revision);
        ensure_tree_index_matches(&existing, &state)?;
        transaction.commit()?;
        return Ok(AppResourceMutationReceipt {
            outcome: AppResourceAppendOutcome::AlreadyPresent,
            previous_period_revision: period_revision,
            current_period_revision: period_revision,
            state,
            // Replaying durable bytes proves admission, but it cannot prove
            // that the original bearer permit was not already dispatched.
            // Crash recovery must settle/release the held reservation and
            // create a new exact operation instead of minting a second permit.
            reservation_dispatchable: false,
        });
    }
    if authoritative_elapsed_ms.is_some_and(|elapsed| elapsed != pending.at_elapsed_ms()) {
        return Err(AppResourceAuthorityError::InvalidObservation(
            "resource observation elapsed time is not anchored to the durable root",
        ));
    }
    if fence.expected_journal_revision != current_revision {
        return Err(AppResourceAuthorityError::JournalRevisionConflict {
            expected: fence.expected_journal_revision,
            actual: current_revision,
        });
    }
    if admissions_closed
        && matches!(
            &pending,
            PendingResourceEvent::NodeOpened(_) | PendingResourceEvent::Reserved(_)
        )
    {
        return Err(AppResourceAuthorityError::PeriodClosed);
    }

    // Work-expanding events must be admitted against the state that existed
    // immediately before the candidate event. In particular, a late
    // ProgressObserved event or a newly-held reservation must not erase a
    // NoProgress breach that was already incurred at the same monotonic time.
    if matches!(
        &pending,
        PendingResourceEvent::NodeOpened(_)
            | PendingResourceEvent::ProgressObserved(_)
            | PendingResourceEvent::Reserved(_)
    ) {
        let mut preflight_journal = existing.journal.clone();
        preflight_journal.evaluated_at_elapsed_ms = pending.at_elapsed_ms();
        let preflight = assess_app_resource_journal_with_recovery(
            &preflight_journal,
            &current,
            &stored_recovery_refs,
        )?;
        if let PendingResourceEvent::Reserved(reservation) = &pending {
            evaluate_app_resource_reservation(
                &preflight,
                reservation.requested,
                &reservation.capability_requests,
                reservation.at_elapsed_ms,
                reservation.expires_at_elapsed_ms,
                &current,
            )?;
        } else if let Some(breach) = preflight.breaches().iter().find(|breach| {
            !matches!(
                breach,
                AppResourceBreach::MonthlyTokens | AppResourceBreach::MonthlyCost
            )
        }) {
            return Err(AppResourceContractError::ReservationDenied(*breach).into());
        }
    }

    let next_sequence = current_revision
        .checked_add(1)
        .ok_or(AppResourceAuthorityError::IntegerRange("journal_revision"))?;
    let event = pending.materialize(next_sequence, lane);
    let mut candidate = existing.journal;
    candidate.evaluated_at_elapsed_ms = pending.at_elapsed_ms();
    candidate.events.push(event.clone());
    candidate.validate_app_contract(&AppContractLimits::default())?;

    let trusted_recovery = match recovery.as_ref() {
        Some(PendingResourceRecovery::ProvenUnspent(value)) => {
            if !matches!(
                &pending,
                PendingResourceEvent::Settled(observation)
                    if observation.outcome == AppResourceSettlementOutcome::ProvenUnspent
            ) {
                return Err(AppResourceAuthorityError::InvalidObservation(
                    "trusted unspent recovery can accompany only proven-unspent settlement",
                ));
            }
            Some(TrustedAppResourceRecoveryEvidence::from_crash_reconciler(
                value.reservation_id.clone(),
                value.observation_id.clone(),
                value.reconciliation_ref.clone(),
                value.reconciliation_revision,
                value.reconciled_at_elapsed_ms,
            ))
        },
        Some(PendingResourceRecovery::ReceiptAbsent(value)) => {
            if !matches!(
                &pending,
                PendingResourceEvent::Settled(observation)
                    if observation.outcome == AppResourceSettlementOutcome::ProvenUnspent
            ) {
                return Err(AppResourceAuthorityError::InvalidObservation(
                    "trusted receipt absence can accompany only proven-unspent settlement",
                ));
            }
            let evidence = value.as_crash_reconciliation();
            Some(TrustedAppResourceRecoveryEvidence::from_crash_reconciler(
                evidence.reservation_id,
                evidence.observation_id,
                evidence.reconciliation_ref,
                evidence.reconciliation_revision,
                evidence.reconciled_at_elapsed_ms,
            ))
        },
        Some(PendingResourceRecovery::Committed(_))
        | Some(PendingResourceRecovery::ConservativePostIo(_))
        | Some(PendingResourceRecovery::NoEffectCommitted(_))
        | None => None,
    };
    let mut all_recovery_refs = stored_recovery_refs;
    if let Some(value) = trusted_recovery.as_ref() {
        all_recovery_refs.push(value);
    }
    let assessment =
        assess_app_resource_journal_with_recovery(&candidate, &current, &all_recovery_refs)?;
    if matches!(&pending, PendingResourceEvent::Reserved(_)) {
        evaluate_app_resource_candidate_period(&assessment, &current)?;
    }
    let state = AppResourceTreeState::from_assessment(&assessment, next_sequence);
    let event_json = canonical_json_bytes(&serde_json::to_value(&event)?)?;
    ensure_stored_record_bounded("event", &event_json)?;
    let event_digest = AppDigest::blake3(&event_json);
    let terminal_state_json = state
        .terminally_settled
        .then(|| canonical_json_bytes(&serde_json::to_value(&state)?))
        .transpose()?;
    if let Some(bytes) = terminal_state_json.as_deref() {
        ensure_stored_record_bounded("terminal state", bytes)?;
    }
    let updated = transaction.execute(
        "UPDATE app_resource_trees
            SET last_event_sequence = ?1,
                evaluated_at_elapsed_ms = ?2,
                committed_tokens = ?3,
                outstanding_tokens = ?4,
                committed_cost_microusd = ?5,
                outstanding_cost_microusd = ?6,
                terminally_settled = ?7,
                terminal_state_json = ?8,
                updated_at = ?9
          WHERE budget_ledger_ref = ?10 AND last_event_sequence = ?11",
        params![
            to_sql_i64(next_sequence, "journal_revision")?,
            to_sql_i64(state.evaluated_at_elapsed_ms, "evaluated_at_elapsed_ms")?,
            to_sql_i64(total_tokens(state.committed)?, "committed_tokens")?,
            to_sql_i64(
                total_tokens(state.outstanding_reserved)?,
                "outstanding_tokens",
            )?,
            to_sql_i64(state.committed.cost_microusd, "committed_cost_microusd")?,
            to_sql_i64(
                state.outstanding_reserved.cost_microusd,
                "outstanding_cost_microusd",
            )?,
            if state.terminally_settled {
                1_i64
            } else {
                0_i64
            },
            terminal_state_json,
            format_timestamp(&now),
            binding.budget_ledger_ref.as_str(),
            to_sql_i64(current_revision, "journal_revision")?,
        ],
    )?;
    if updated != 1 {
        return Err(AppResourceAuthorityError::JournalRevisionConflict {
            expected: current_revision,
            actual: load_tree_revision(&transaction, &binding.budget_ledger_ref)?,
        });
    }
    if let (Some(PendingResourceRecovery::ProvenUnspent(recovery)), Some(trusted)) =
        (recovery.as_ref(), trusted_recovery.as_ref())
    {
        insert_recovery_evidence(
            &transaction,
            &binding.budget_ledger_ref,
            recovery,
            trusted,
            &now,
        )?;
    }
    if let (Some(PendingResourceRecovery::ReceiptAbsent(proof)), Some(trusted)) =
        (recovery.as_ref(), trusted_recovery.as_ref())
    {
        let evidence = proof.as_crash_reconciliation();
        insert_recovery_evidence(
            &transaction,
            &binding.budget_ledger_ref,
            &evidence,
            trusted,
            &now,
        )?;
    }
    transaction.execute(
        "INSERT INTO app_resource_tree_events (
             budget_ledger_ref, sequence, event_digest, event_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            binding.budget_ledger_ref.as_str(),
            to_sql_i64(next_sequence, "journal_revision")?,
            event_digest.as_str(),
            event_json,
            format_timestamp(&now),
        ],
    )?;
    let expected_next_period = period_revision
        .get()
        .checked_add(1)
        .ok_or(AppResourceAuthorityError::IntegerRange("period_revision"))?;
    let (stored_period_revision, _) =
        load_period_revision(&transaction, &resolved, &snapshot.period_ref)?;
    if stored_period_revision.get() != expected_next_period {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "event append did not advance exactly one installation-period revision".to_owned(),
        ));
    }
    transaction.commit()?;
    Ok(AppResourceMutationReceipt {
        outcome: AppResourceAppendOutcome::Created,
        previous_period_revision: period_revision,
        current_period_revision: stored_period_revision,
        state,
        reservation_dispatchable: matches!(pending, PendingResourceEvent::Reserved(_)),
    })
}

#[allow(clippy::too_many_arguments)]
fn inspect_maintenance_batch_blocking(
    connection: &Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    resolved: &ResolvedAppAuthority,
    request: AppResourceMaintenanceBatchRequest,
    now: DateTime<Utc>,
) -> Result<AppResourceMaintenanceBatch, AppResourceAuthorityError> {
    ensure_accepted_authenticated_snapshot(authenticated_scope, resolved, &now)?;
    let transaction = connection.unchecked_transaction()?;
    ensure_registry_accepted_authority_identity(&transaction, scope, resolved)?;

    let mut cleanup_candidates = Vec::with_capacity(usize::from(
        request.max_cleanup_candidates.saturating_add(1),
    ));
    if !request.cursor.cleanup_exhausted {
        let cleanup_limit = i64::from(request.max_cleanup_candidates) + 1;
        let after_ledger = request
            .cursor
            .after_budget_ledger_ref
            .as_ref()
            .map(|value| value.as_str());
        let mut cleanup_statement = transaction.prepare(
            "SELECT substr(identity_json, 1, ?5), length(identity_json),
                    last_event_sequence, updated_at
               FROM app_resource_trees
              WHERE installation_id = ?1
                AND installation_generation = ?2
                AND terminally_settled = 0
                AND (?3 IS NULL OR budget_ledger_ref > ?3)
              ORDER BY budget_ledger_ref ASC
              LIMIT ?4",
        )?;
        let mut cleanup_rows = cleanup_statement.query(params![
            resolved.installation_id.as_str(),
            to_sql_i64(resolved.installation_generation, "installation_generation")?,
            after_ledger,
            cleanup_limit,
            i64::try_from(MAX_STORED_RESOURCE_RECORD_BYTES + 1)
                .map_err(|_| AppResourceAuthorityError::IntegerRange("resource_record_bytes"))?,
        ])?;
        while let Some(row) = cleanup_rows.next()? {
            let identity_bytes = row.get::<_, Vec<u8>>(0)?;
            ensure_stored_record_length_bounded(
                "maintenance tree identity",
                row.get::<_, i64>(1)?,
            )?;
            ensure_stored_record_bounded("maintenance tree identity", &identity_bytes)?;
            let identity: AppResourceTreeIdentity =
                decode_app_contract(&identity_bytes, &AppContractLimits::default())?;
            if identity.scope != *scope
                || identity.installation_id != resolved.installation_id
                || identity.installation_generation != resolved.installation_generation
            {
                return Err(AppResourceAuthorityError::CorruptAuthority(
                    "maintenance tree escaped its canonical installation scope".to_owned(),
                ));
            }
            cleanup_candidates.push(AppResourceCleanupCandidate {
                identity,
                journal_revision: from_sql_u64(row.get::<_, i64>(2)?, "journal_revision")?,
                updated_at: parse_resource_timestamp(&row.get::<_, String>(3)?)?,
            });
        }
    }
    let cleanup_has_more = cleanup_candidates.len() > usize::from(request.max_cleanup_candidates);
    if cleanup_has_more {
        cleanup_candidates.pop();
    }
    let next_cleanup_cursor = cleanup_has_more
        .then(|| {
            cleanup_candidates
                .last()
                .map(|candidate| candidate.identity.budget_ledger_ref.clone())
        })
        .flatten();

    let mut periods = Vec::with_capacity(usize::from(request.max_periods.saturating_add(1)));
    if !request.cursor.periods_exhausted {
        let period_limit = i64::from(request.max_periods) + 1;
        let after_period = request
            .cursor
            .after_period_ref
            .as_ref()
            .map(|value| value.as_str());
        let latest_period: Option<String> = transaction.query_row(
            "SELECT MAX(period_ref) FROM app_resource_periods
              WHERE installation_id = ?1 AND installation_generation = ?2",
            params![
                resolved.installation_id.as_str(),
                to_sql_i64(resolved.installation_generation, "installation_generation")?,
            ],
            |row| row.get(0),
        )?;
        let projection: Option<(Option<String>, i64)> = transaction
            .query_row(
                "SELECT period_ref, authority_revision
                   FROM app_resource_usage_projection
                  WHERE installation_id = ?1",
                params![resolved.installation_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let current_period_ref = canonical_monthly_period(now)?.0;
        let mut period_statement = transaction.prepare(
            "SELECT period.period_ref, period.revision,
                    period.admissions_closed_at, period.retention_until,
                    EXISTS(SELECT 1 FROM app_resource_trees tree
                      WHERE tree.installation_id = period.installation_id
                        AND tree.installation_generation = period.installation_generation
                        AND tree.period_ref = period.period_ref
                        AND tree.terminally_settled = 1)
               FROM app_resource_periods period
              WHERE period.installation_id = ?1
                AND period.installation_generation = ?2
                AND (?3 IS NULL OR period.period_ref > ?3)
              ORDER BY period.period_ref ASC
              LIMIT ?4",
        )?;
        let mut period_rows = period_statement.query(params![
            resolved.installation_id.as_str(),
            to_sql_i64(resolved.installation_generation, "installation_generation")?,
            after_period,
            period_limit,
        ])?;
        while let Some(row) = period_rows.next()? {
            let period_ref = AppReference::parse(row.get::<_, String>(0)?)?;
            let revision =
                AppRevision::new(from_sql_u64(row.get::<_, i64>(1)?, "period_revision")?)?;
            let admissions_closed_at = row.get::<_, Option<String>>(2)?;
            let retention_until = row
                .get::<_, Option<String>>(3)?
                .map(|value| parse_resource_timestamp(&value))
                .transpose()?;
            if admissions_closed_at.is_some() != retention_until.is_some() {
                return Err(AppResourceAuthorityError::CorruptAuthority(
                    "resource period closure and retention deadline are not total".to_owned(),
                ));
            }
            let is_latest = latest_period.as_deref() == Some(period_ref.as_str());
            let projection_stale = is_latest
                && projection
                    .as_ref()
                    .is_none_or(|(projected_period, projected_revision)| {
                        projected_period.as_deref() != Some(period_ref.as_str())
                            || u64::try_from(*projected_revision).ok() != Some(revision.get())
                    });
            periods.push(AppResourcePeriodMaintenanceCandidate {
                rollover_due: admissions_closed_at.is_none()
                    && period_ref.as_str() < current_period_ref.as_str(),
                projection_stale,
                retention_due: retention_until.is_some_and(|deadline| deadline <= now),
                has_terminal_trees: row.get::<_, i64>(4)? != 0,
                period_ref,
                revision,
            });
        }
        drop(period_rows);
        drop(period_statement);
    }
    let periods_have_more = periods.len() > usize::from(request.max_periods);
    if periods_have_more {
        periods.pop();
    }
    let next_period_cursor = periods_have_more
        .then(|| periods.last().map(|candidate| candidate.period_ref.clone()))
        .flatten();
    transaction.commit()?;
    Ok(AppResourceMaintenanceBatch {
        cleanup_candidates,
        periods,
        next_cursor: AppResourceMaintenanceCursor {
            after_budget_ledger_ref: next_cleanup_cursor,
            after_period_ref: next_period_cursor,
            cleanup_exhausted: request.cursor.cleanup_exhausted || !cleanup_has_more,
            periods_exhausted: request.cursor.periods_exhausted || !periods_have_more,
        },
        cleanup_has_more,
        periods_have_more,
    })
}

#[allow(clippy::too_many_arguments)]
fn rollover_period_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    resolved: &ResolvedAppAuthority,
    period_ref: &AppReference,
    expected_revision: AppRevision,
    retention_until: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<AppResourcePeriodRolloverReceipt, AppResourceAuthorityError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_accepted_authenticated_snapshot(authenticated_scope, resolved, &now)?;
    ensure_registry_accepted_authority_identity(&transaction, scope, resolved)?;
    let row: (i64, Option<String>, Option<String>) = transaction
        .query_row(
            "SELECT revision, admissions_closed_at, retention_until
               FROM app_resource_periods
              WHERE installation_id = ?1
                AND installation_generation = ?2
                AND period_ref = ?3",
            params![
                resolved.installation_id.as_str(),
                to_sql_i64(resolved.installation_generation, "installation_generation")?,
                period_ref.as_str(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(AppResourceAuthorityError::StaleRegistryAuthority(
            "installation period",
        ))?;
    let current_revision = AppRevision::new(from_sql_u64(row.0, "period_revision")?)?;
    let retention_text = format_timestamp(&retention_until);
    if row.1.is_some() {
        if row.2.as_deref() != Some(retention_text.as_str()) {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "period rollover",
                identity: period_ref.to_string(),
            });
        }
        transaction.commit()?;
        return Ok(AppResourcePeriodRolloverReceipt {
            outcome: AppResourceMaintenanceOutcome::AlreadyPresent,
            previous_revision: current_revision,
            current_revision,
        });
    }
    if current_revision != expected_revision {
        return Err(AppResourceAuthorityError::PeriodRevisionConflict {
            minimum: expected_revision.get(),
            actual: current_revision.get(),
        });
    }
    let next_revision = current_revision
        .get()
        .checked_add(1)
        .ok_or(AppResourceAuthorityError::IntegerRange("period_revision"))?;
    let updated = transaction.execute(
        "UPDATE app_resource_periods
            SET revision = ?1, admissions_closed_at = ?2,
                retention_until = ?3, updated_at = ?2
          WHERE installation_id = ?4
            AND installation_generation = ?5
            AND period_ref = ?6
            AND revision = ?7
            AND admissions_closed_at IS NULL",
        params![
            to_sql_i64(next_revision, "period_revision")?,
            format_timestamp(&now),
            retention_text,
            resolved.installation_id.as_str(),
            to_sql_i64(resolved.installation_generation, "installation_generation")?,
            period_ref.as_str(),
            to_sql_i64(current_revision.get(), "period_revision")?,
        ],
    )?;
    if updated != 1 {
        let (actual, _) = load_period_revision(&transaction, resolved, period_ref)?;
        return Err(AppResourceAuthorityError::PeriodRevisionConflict {
            minimum: current_revision.get(),
            actual: actual.get(),
        });
    }
    transaction.commit()?;
    Ok(AppResourcePeriodRolloverReceipt {
        outcome: AppResourceMaintenanceOutcome::Created,
        previous_revision: current_revision,
        current_revision: AppRevision::new(next_revision)?,
    })
}

fn rebuild_usage_projection_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    resolved: &ResolvedAppAuthority,
    period_ref: &AppReference,
    now: DateTime<Utc>,
) -> Result<AppResourceUsageProjection, AppResourceAuthorityError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_accepted_authenticated_snapshot(authenticated_scope, resolved, &now)?;
    ensure_registry_accepted_authority_identity(&transaction, scope, resolved)?;
    let row: (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<String>,
        String,
    ) = transaction
        .query_row(
            "SELECT revision, committed_tokens, outstanding_tokens,
                    committed_cost_microusd, outstanding_cost_microusd,
                    background_starts, foreground_runs, background_runs,
                    admissions_closed_at, created_at
               FROM app_resource_periods
              WHERE installation_id = ?1
                AND installation_generation = ?2
                AND period_ref = ?3",
            params![
                resolved.installation_id.as_str(),
                to_sql_i64(resolved.installation_generation, "installation_generation")?,
                period_ref.as_str(),
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?
        .ok_or(AppResourceAuthorityError::StaleRegistryAuthority(
            "installation period",
        ))?;
    let projection = AppResourceUsageProjection {
        schema_version: 1,
        installation_id: resolved.installation_id.clone(),
        installation_generation: resolved.installation_generation,
        period_ref: period_ref.clone(),
        authority_revision: AppRevision::new(from_sql_u64(row.0, "period_revision")?)?,
        committed_tokens: from_sql_u64(row.1, "committed_tokens")?,
        outstanding_tokens: from_sql_u64(row.2, "outstanding_tokens")?,
        committed_cost_microusd: from_sql_u64(row.3, "committed_cost_microusd")?,
        outstanding_cost_microusd: from_sql_u64(row.4, "outstanding_cost_microusd")?,
        background_starts: from_sql_u64(row.5, "background_starts")?,
        foreground_runs: from_sql_u16(row.6, "foreground_runs")?,
        background_runs: from_sql_u16(row.7, "background_runs")?,
        admissions_closed: row.8.is_some(),
        projected_at: now,
    };
    let projection_json = canonical_json_bytes(&serde_json::to_value(&projection)?)?;
    ensure_stored_record_bounded("usage projection", &projection_json)?;
    let written = transaction.execute(
        "INSERT INTO app_resource_usage_projection (
             installation_id, authority_revision, projection_json, projected_at,
             period_ref, period_created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(installation_id) DO UPDATE SET
             authority_revision = excluded.authority_revision,
             projection_json = excluded.projection_json,
             projected_at = excluded.projected_at,
             period_ref = excluded.period_ref,
             period_created_at = excluded.period_created_at
         WHERE app_resource_usage_projection.period_created_at IS NULL
            OR excluded.period_created_at > app_resource_usage_projection.period_created_at
            OR (
                excluded.period_ref = app_resource_usage_projection.period_ref
                AND excluded.authority_revision
                    >= app_resource_usage_projection.authority_revision
            )",
        params![
            resolved.installation_id.as_str(),
            to_sql_i64(projection.authority_revision.get(), "period_revision")?,
            projection_json,
            format_timestamp(&now),
            period_ref.as_str(),
            row.9,
        ],
    )?;
    if written != 1 {
        return Err(AppResourceAuthorityError::ProjectionSuperseded);
    }
    transaction.commit()?;
    Ok(projection)
}

fn retire_terminal_trees_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    resolved: &ResolvedAppAuthority,
    period_ref: &AppReference,
    max_trees: u16,
    now: DateTime<Utc>,
) -> Result<AppResourceRetentionReceipt, AppResourceAuthorityError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_accepted_authenticated_snapshot(authenticated_scope, resolved, &now)?;
    ensure_registry_accepted_authority_identity(&transaction, scope, resolved)?;
    let (closed_at, retention_until): (Option<String>, Option<String>) = transaction
        .query_row(
            "SELECT admissions_closed_at, retention_until
               FROM app_resource_periods
              WHERE installation_id = ?1
                AND installation_generation = ?2
                AND period_ref = ?3",
            params![
                resolved.installation_id.as_str(),
                to_sql_i64(resolved.installation_generation, "installation_generation")?,
                period_ref.as_str(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(AppResourceAuthorityError::StaleRegistryAuthority(
            "installation period",
        ))?;
    if closed_at.is_none() {
        return Err(AppResourceAuthorityError::InvalidRetention(
            "period must be closed before retirement",
        ));
    }
    let retention_until = retention_until.ok_or(AppResourceAuthorityError::InvalidRetention(
        "closed period is missing its retention deadline",
    ))?;
    let retention_until = DateTime::parse_from_rfc3339(&retention_until)
        .map_err(|_| {
            AppResourceAuthorityError::CorruptAuthority(
                "period retention deadline is not canonical RFC3339".to_owned(),
            )
        })?
        .with_timezone(&Utc);
    if retention_until > now {
        return Err(AppResourceAuthorityError::InvalidRetention(
            "period retention deadline has not elapsed",
        ));
    }

    let mut statement = transaction.prepare(
        "SELECT budget_ledger_ref,
                substr(identity_json, 1, ?5), length(identity_json),
                substr(terminal_state_json, 1, ?5), length(terminal_state_json),
                root_execution_id, last_event_sequence
           FROM app_resource_trees
          WHERE installation_id = ?1
            AND installation_generation = ?2
            AND period_ref = ?3
            AND terminally_settled = 1
          ORDER BY budget_ledger_ref ASC
          LIMIT ?4",
    )?;
    let mut rows = statement.query(params![
        resolved.installation_id.as_str(),
        to_sql_i64(resolved.installation_generation, "installation_generation")?,
        period_ref.as_str(),
        i64::from(max_trees),
        i64::try_from(MAX_STORED_RESOURCE_RECORD_BYTES + 1)
            .map_err(|_| AppResourceAuthorityError::IntegerRange("resource_record_bytes"))?,
    ])?;
    let mut candidates = Vec::with_capacity(usize::from(max_trees));
    while let Some(row) = rows.next()? {
        candidates.push((
            AppReference::parse(row.get::<_, String>(0)?)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, Option<Vec<u8>>>(3)?.ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "terminal tree is missing its final state".to_owned(),
                )
            })?,
            row.get::<_, Option<i64>>(4)?.ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "terminal tree is missing its final-state byte length".to_owned(),
                )
            })?,
            AppReference::parse(row.get::<_, String>(5)?)?,
            from_sql_u64(row.get::<_, i64>(6)?, "journal_revision")?,
        ));
    }
    drop(rows);
    drop(statement);

    let mut retired_trees = 0_u16;
    let mut retained_bytes = 0_usize;
    for (
        ledger_ref,
        identity_json,
        identity_len,
        terminal_state_json,
        terminal_state_len,
        root_execution_id,
        sequence,
    ) in &candidates
    {
        ensure_stored_record_length_bounded("identity", *identity_len)?;
        ensure_stored_record_length_bounded("terminal state", *terminal_state_len)?;
        ensure_stored_record_bounded("identity", identity_json)?;
        ensure_stored_record_bounded("terminal state", terminal_state_json)?;
        let journal = load_journal(&transaction, ledger_ref)?.ok_or_else(|| {
            AppResourceAuthorityError::CorruptAuthority(
                "terminal tree disappeared during retained compaction".to_owned(),
            )
        })?;
        let journal_bytes = canonical_json_bytes(&serde_json::to_value(&journal)?)?;
        let candidate_bytes = identity_json
            .len()
            .checked_add(terminal_state_json.len())
            .and_then(|value| value.checked_add(journal_bytes.len()))
            .ok_or(AppResourceAuthorityError::IntegerRange(
                "retirement_transaction_bytes",
            ))?;
        let next_retained_bytes = retained_bytes.checked_add(candidate_bytes).ok_or(
            AppResourceAuthorityError::IntegerRange("retirement_transaction_bytes"),
        )?;
        if retired_trees != 0 && next_retained_bytes > MAX_RETIREMENT_TRANSACTION_BYTES {
            break;
        }
        if next_retained_bytes > MAX_RETIREMENT_TRANSACTION_BYTES {
            return Err(AppResourceAuthorityError::CorruptAuthority(
                "one terminal resource tree exceeds the retirement transaction byte bound"
                    .to_owned(),
            ));
        }
        let journal_digest = AppDigest::blake3(&journal_bytes);
        let final_state_digest = AppDigest::blake3(terminal_state_json);
        transaction.execute(
            "INSERT INTO app_resource_retired_trees (
                 budget_ledger_ref, installation_id, installation_generation,
                 root_execution_id, period_ref, identity_json, final_state_json,
                 final_state_digest, final_event_sequence, final_journal_digest, retired_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                ledger_ref.as_str(),
                resolved.installation_id.as_str(),
                to_sql_i64(resolved.installation_generation, "installation_generation")?,
                root_execution_id.as_str(),
                period_ref.as_str(),
                identity_json,
                terminal_state_json,
                final_state_digest.as_str(),
                to_sql_i64(*sequence, "journal_revision")?,
                journal_digest.as_str(),
                format_timestamp(&now),
            ],
        )?;
        let deleted = transaction.execute(
            "DELETE FROM app_resource_trees WHERE budget_ledger_ref = ?1",
            params![ledger_ref.as_str()],
        )?;
        if deleted != 1 {
            return Err(AppResourceAuthorityError::CorruptAuthority(
                "terminal tree retirement lost its canonical row".to_owned(),
            ));
        }
        retained_bytes = next_retained_bytes;
        retired_trees = retired_trees
            .checked_add(1)
            .ok_or(AppResourceAuthorityError::IntegerRange("retired_trees"))?;
    }
    let has_more: i64 = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_resource_trees
          WHERE installation_id = ?1
            AND installation_generation = ?2
            AND period_ref = ?3
            AND terminally_settled = 1)",
        params![
            resolved.installation_id.as_str(),
            to_sql_i64(resolved.installation_generation, "installation_generation")?,
            period_ref.as_str(),
        ],
        |row| row.get(0),
    )?;
    transaction.commit()?;
    Ok(AppResourceRetentionReceipt {
        retired_trees,
        has_more: has_more != 0,
    })
}

#[derive(Debug, Clone, Copy)]
struct PeriodTotals {
    committed_tokens: u64,
    outstanding_tokens: u64,
    committed_cost_microusd: u64,
    outstanding_cost_microusd: u64,
    background_starts: u64,
    foreground_runs: u16,
    background_runs: u16,
}

#[allow(clippy::too_many_arguments)]
fn admit_root_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    resolved: &ResolvedAppAuthority,
    binding: &AppRunBinding,
    policy: AppResourceEnforcementPolicy,
    snapshot: AppResourceAdmissionSnapshot,
    lane: AppResourceExecutionLane,
    root_node_id: AppReference,
    now: DateTime<Utc>,
) -> Result<AppResourceRootAdmissionReceipt, AppResourceAuthorityError> {
    // Normalize once before any write so the in-memory lease and durable
    // `created_at` use the same microsecond-precision origin. No fallible work
    // remains after the admission transaction commits.
    let durable_started_at = canonical_resource_timestamp(now)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_registry_authority_current(&transaction, scope, resolved)?;
    if snapshot.scheduler_guard.lane != lane {
        return Err(AppResourceAuthorityError::InvalidAdmissionSnapshot(
            "scheduler permit lane does not match the root lane",
        ));
    }
    if snapshot.package_revision_ref != resolved.package_revision_ref {
        return Err(AppResourceAuthorityError::InvalidAdmissionSnapshot(
            "package measurement does not match current package authority",
        ));
    }
    if retired_tree_exists(
        &transaction,
        &binding.budget_ledger_ref,
        &resolved.installation_id,
        &binding.execution_id,
    )? {
        return Err(AppResourceAuthorityError::RetiredTree);
    }
    let period_revision = ensure_period(&transaction, resolved, &snapshot, &now)?;
    let existing = load_existing_root(&transaction, &binding.budget_ledger_ref)?;
    let (_, admissions_closed) =
        load_period_revision(&transaction, resolved, &snapshot.period_ref)?;
    if admissions_closed && existing.is_none() {
        return Err(AppResourceAuthorityError::PeriodClosed);
    }
    if existing.as_ref().is_some_and(|existing| {
        existing.journal.identity.scope != *scope
            || existing.journal.identity.installation_id != resolved.installation_id
            || existing.journal.identity.installation_generation != resolved.installation_generation
            || existing.journal.identity.package_revision_ref != resolved.package_revision_ref
            || existing.journal.identity.grant_revision != resolved.grant_revision
            || existing.journal.identity.schema_revision != resolved.schema_revision
            || existing.journal.identity.authority_digest != resolved.authority_digest
            || existing.journal.identity.behavior_resource_identity
                != binding.behavior_resource_identity
            || existing.journal.identity.root_execution_id != binding.execution_id
            || existing.journal.identity.installation_period_ref != snapshot.period_ref
    }) {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "budget ledger",
            identity: binding.budget_ledger_ref.to_string(),
        });
    }
    if existing
        .as_ref()
        .is_some_and(|existing| existing.package_bytes != snapshot.package_bytes)
    {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "resource root runtime baseline",
            identity: binding.budget_ledger_ref.to_string(),
        });
    }
    if existing.as_ref().is_some_and(|existing| {
        !matches!(
            existing.journal.events.first(),
            Some(AppResourceJournalEvent::NodeOpened { lane, .. }) if *lane == existing.lane
        )
    }) {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "tree lane index does not match its canonical root event".to_owned(),
        ));
    }
    let totals = load_period_totals(
        &transaction,
        resolved,
        &snapshot.period_ref,
        existing.as_ref(),
    )?;
    let behavior_period = load_behavior_period_baseline(
        &transaction,
        resolved,
        &snapshot.period_ref,
        binding.behavior_resource_identity.as_ref(),
        existing.as_ref(),
    )?;
    let baseline = AppResourcePeriodBaseline::from_resource_store(
        scope.clone(),
        resolved.installation_id.clone(),
        resolved.installation_generation,
        binding.budget_ledger_ref.clone(),
        snapshot.period_ref.clone(),
        period_revision,
        existing
            .as_ref()
            .map_or(snapshot.period_ends_at_elapsed_ms, |root| {
                root.period_ends_at_elapsed_ms
            }),
        totals.committed_tokens,
        totals.outstanding_tokens,
        totals.committed_cost_microusd,
        totals.outstanding_cost_microusd,
        behavior_period,
        totals.background_starts,
        totals.foreground_runs,
        totals.background_runs,
        snapshot.scheduler_foreground_runs_excluding_root,
        snapshot.scheduler_background_runs_excluding_root,
        existing
            .as_ref()
            .map_or(snapshot.package_bytes, |root| root.package_bytes),
    );
    let current = CurrentAppResourceAuthority::from_current_authority(
        authenticated_scope,
        resolved,
        binding,
        policy,
        baseline,
        now,
    )?;
    let root_event = AppResourceJournalEvent::NodeOpened {
        sequence: 1,
        node_id: root_node_id,
        execution_ref: binding.execution_id.clone(),
        parent_node_id: None,
        node_kind: AppResourceNodeKind::Root,
        lane,
        at_elapsed_ms: 0,
    };
    if let Some(existing) = existing {
        if &existing.journal.identity != current.identity()
            || existing.journal.events.first() != Some(&root_event)
        {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "budget ledger",
                identity: binding.budget_ledger_ref.to_string(),
            });
        }
        // Historical roots have already consumed their atomic admission. A
        // replay refreshes current authority and period facts, but must not
        // re-run lane/frequency admission against today's scheduler state.
        let recovery = load_recovery_evidence(&transaction, &binding.budget_ledger_ref)?;
        let recovery_refs: Vec<_> = recovery.iter().collect();
        let assessment =
            assess_app_resource_journal_with_recovery(&existing.journal, &current, &recovery_refs)?;
        let journal_revision = u64::try_from(existing.journal.events.len())
            .map_err(|_| AppResourceAuthorityError::IntegerRange("journal_revision"))?;
        let state = AppResourceTreeState::from_assessment(&assessment, journal_revision);
        ensure_tree_index_matches(&existing, &state)?;
        transaction.commit()?;
        let dispatch_lease = (!state.root_closed).then_some(AppResourceRootDispatchLease {
            identity: existing.journal.identity.clone(),
            live: Arc::new(()),
            started_at: existing.started_at,
            period_ends_at_elapsed_ms: existing.period_ends_at_elapsed_ms,
            package_revision_ref: snapshot.package_revision_ref.clone(),
            package_bytes: existing.package_bytes,
            _scheduler_guard: snapshot.scheduler_guard,
        });
        return Ok(AppResourceRootAdmissionReceipt {
            outcome: AppResourceRootAdmissionOutcome::AlreadyPresent,
            identity: existing.journal.identity,
            admitted_period_revision: existing.admitted_period_revision,
            current_period_revision: period_revision,
            state,
            dispatch_lease,
        });
    }
    evaluate_app_resource_root_admission(&current, lane).inspect_err(|error| {
        tracing::warn!(
            installation_id = %resolved.installation_id,
            installation_generation = resolved.installation_generation,
            execution_id = %binding.execution_id,
            period_ref = %snapshot.period_ref,
            installation_foreground_runs = totals.foreground_runs,
            installation_background_runs = totals.background_runs,
            scheduler_foreground_runs = snapshot.scheduler_foreground_runs_excluding_root,
            scheduler_background_runs = snapshot.scheduler_background_runs_excluding_root,
            error = %error,
            "app resource root admission denied"
        );
    })?;
    let journal = AppResourceJournal {
        identity: current.identity().clone(),
        evaluated_at_elapsed_ms: 0,
        events: vec![root_event.clone()],
    };
    journal.validate_app_contract(&AppContractLimits::default())?;
    let assessment = assess_app_resource_journal(&journal, &current)?;
    let state = AppResourceTreeState::from_assessment(&assessment, 1);
    if root_execution_exists(
        &transaction,
        &resolved.installation_id,
        &binding.execution_id,
    )? {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "root execution",
            identity: binding.execution_id.to_string(),
        });
    }

    let identity_json = canonical_json_bytes(&serde_json::to_value(&journal.identity)?)?;
    let event_json = canonical_json_bytes(&serde_json::to_value(&root_event)?)?;
    ensure_stored_record_bounded("identity", &identity_json)?;
    ensure_stored_record_bounded("event", &event_json)?;
    let committed_tokens = total_tokens(state.committed)?;
    let outstanding_tokens = total_tokens(state.outstanding_reserved)?;
    let lane_label = lane_label(lane);
    let event_digest = AppDigest::blake3(&event_json);
    transaction.execute(
        "INSERT INTO app_resource_trees (
             budget_ledger_ref, installation_id, installation_generation,
             root_execution_id, period_ref, admitted_period_revision, lane,
             behavior_ledger_digest, identity_json,
             last_event_sequence, evaluated_at_elapsed_ms,
             committed_tokens, outstanding_tokens, committed_cost_microusd,
             outstanding_cost_microusd, terminally_settled,
             created_at, updated_at, period_ends_at_elapsed_ms, package_bytes
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 0,
             ?10, ?11, ?12, ?13, ?14, ?15, ?15, ?16, ?17
         )",
        params![
            binding.budget_ledger_ref.as_str(),
            resolved.installation_id.as_str(),
            to_sql_i64(resolved.installation_generation, "installation_generation")?,
            binding.execution_id.as_str(),
            snapshot.period_ref.as_str(),
            to_sql_i64(period_revision.get(), "period_revision")?,
            lane_label,
            binding
                .behavior_resource_identity
                .as_ref()
                .map(|identity| identity.ledger_dimension_digest().as_str()),
            identity_json,
            to_sql_i64(committed_tokens, "committed_tokens")?,
            to_sql_i64(outstanding_tokens, "outstanding_tokens")?,
            to_sql_i64(state.committed.cost_microusd, "committed_cost_microusd")?,
            to_sql_i64(
                state.outstanding_reserved.cost_microusd,
                "outstanding_cost_microusd",
            )?,
            if state.terminally_settled {
                1_i64
            } else {
                0_i64
            },
            format_timestamp(&now),
            to_sql_i64(
                snapshot.period_ends_at_elapsed_ms,
                "period_ends_at_elapsed_ms",
            )?,
            to_sql_i64(snapshot.package_bytes, "package_bytes")?,
        ],
    )?;
    transaction.execute(
        "INSERT INTO app_resource_tree_events (
             budget_ledger_ref, sequence, event_digest, event_json, created_at
         ) VALUES (?1, 1, ?2, ?3, ?4)",
        params![
            binding.budget_ledger_ref.as_str(),
            event_digest.as_str(),
            event_json,
            format_timestamp(&now),
        ],
    )?;
    let next_period_revision = period_revision
        .get()
        .checked_add(1)
        .ok_or(AppResourceAuthorityError::IntegerRange("period_revision"))?;
    let stored_period_revision: i64 = transaction.query_row(
        "SELECT revision FROM app_resource_periods
          WHERE installation_id = ?1
            AND installation_generation = ?2
            AND period_ref = ?3",
        params![
            resolved.installation_id.as_str(),
            to_sql_i64(resolved.installation_generation, "installation_generation")?,
            snapshot.period_ref.as_str(),
        ],
        |row| row.get(0),
    )?;
    if from_sql_u64(stored_period_revision, "period_revision")? != next_period_revision {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "tree insert did not advance exactly one installation-period revision".to_owned(),
        ));
    }
    transaction.commit()?;

    let dispatch_lease = AppResourceRootDispatchLease {
        identity: journal.identity.clone(),
        live: Arc::new(()),
        started_at: durable_started_at,
        period_ends_at_elapsed_ms: snapshot.period_ends_at_elapsed_ms,
        package_revision_ref: snapshot.package_revision_ref,
        package_bytes: snapshot.package_bytes,
        _scheduler_guard: snapshot.scheduler_guard,
    };
    Ok(AppResourceRootAdmissionReceipt {
        outcome: AppResourceRootAdmissionOutcome::Created,
        identity: journal.identity,
        admitted_period_revision: period_revision,
        current_period_revision: AppRevision::new(next_period_revision)?,
        state,
        dispatch_lease: Some(dispatch_lease),
    })
}

fn ensure_registry_authority_current(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    resolved: &ResolvedAppAuthority,
) -> Result<(), AppResourceAuthorityError> {
    let canonical_authority_digest = resolved
        .canonical_authority_digest()
        .map_err(|error| AppResourceAuthorityError::CorruptAuthority(error.to_string()))?;
    if canonical_authority_digest != resolved.authority_digest {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "resolved app authority digest does not match its canonical fields".to_owned(),
        ));
    }
    let (stored_status, stored_generation, stored_package, installation_bytes) = transaction
        .query_row(
            "SELECT lifecycle_status, lifecycle_generation, package_revision_ref, record_json
               FROM app_installations WHERE installation_id = ?1",
            params![resolved.installation_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(AppResourceAuthorityError::StaleRegistryAuthority(
            "installation",
        ))?;
    let installation: AppInstallation =
        decode_app_contract(&installation_bytes, &AppContractLimits::default())?;
    if stored_status != "enabled"
        || from_sql_u64(stored_generation, "installation_generation")?
            != resolved.installation_generation
        || stored_package != resolved.package_revision_ref.as_str()
        || installation.installation_id != resolved.installation_id
        || installation.scope != *scope
        || installation.lifecycle.status != AppInstallationStatus::Enabled
        || installation.lifecycle.generation != resolved.installation_generation
        || installation.package_revision_ref != resolved.package_revision_ref
        || installation.grant_revision != Some(resolved.grant_revision)
        || installation.active_schema_revision != Some(resolved.schema_revision)
    {
        return Err(AppResourceAuthorityError::StaleRegistryAuthority(
            "installation revision",
        ));
    }

    let (stored_grant_package, stored_authority_digest, stored_revoked_at, grant_bytes) =
        transaction
            .query_row(
                "SELECT package_revision_ref, authority_digest, revoked_at, record_json
               FROM app_grant_revisions
              WHERE installation_id = ?1 AND revision = ?2",
                params![
                    resolved.installation_id.as_str(),
                    to_sql_i64(resolved.grant_revision.get(), "grant_revision")?,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AppResourceAuthorityError::StaleRegistryAuthority("grant"))?;
    let grant: AppGrantRevision = decode_app_contract(&grant_bytes, &AppContractLimits::default())?;
    if stored_grant_package != resolved.package_revision_ref.as_str()
        || stored_authority_digest != resolved.grant_authority_digest.as_str()
        || stored_revoked_at.is_some()
        || grant.installation_id != resolved.installation_id
        || grant.revision != resolved.grant_revision
        || grant.package_revision_ref != resolved.package_revision_ref
        || grant.authority_digest != resolved.grant_authority_digest
        || grant.revoked_at.is_some()
    {
        return Err(AppResourceAuthorityError::StaleRegistryAuthority(
            "grant revision",
        ));
    }

    let (stored_schema_package, schema_bytes) = transaction
        .query_row(
            "SELECT package_revision_ref, record_json FROM app_schema_revisions
              WHERE installation_id = ?1 AND revision = ?2",
            params![
                resolved.installation_id.as_str(),
                to_sql_i64(resolved.schema_revision.get(), "schema_revision")?,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?
        .ok_or(AppResourceAuthorityError::StaleRegistryAuthority("schema"))?;
    let schema: AppSchemaRevision =
        decode_app_contract(&schema_bytes, &AppContractLimits::default())?;
    if stored_schema_package != resolved.package_revision_ref.as_str()
        || schema.installation_id != resolved.installation_id
        || schema.revision != resolved.schema_revision
        || schema.package_revision_ref != resolved.package_revision_ref
    {
        return Err(AppResourceAuthorityError::StaleRegistryAuthority(
            "schema revision",
        ));
    }
    Ok(())
}

/// Historical identity check for settlement/closure only. Current lifecycle
/// status and active grant/schema pointers are deliberately not consulted: a
/// revocation must stop new dispatch without erasing a charge or preventing
/// closure of work that already owns process-minted authority. Immutable scope,
/// package, grant and schema records are still decoded and matched exactly.
fn ensure_registry_accepted_authority_identity(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    resolved: &ResolvedAppAuthority,
) -> Result<(), AppResourceAuthorityError> {
    let canonical_authority_digest = resolved
        .canonical_authority_digest()
        .map_err(|error| AppResourceAuthorityError::CorruptAuthority(error.to_string()))?;
    if canonical_authority_digest != resolved.authority_digest {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "accepted app authority digest does not match its canonical fields".to_owned(),
        ));
    }
    ensure_historical_package_identity(transaction, &resolved.package_revision_ref)?;
    let installation_bytes = transaction
        .query_row(
            "SELECT record_json FROM app_installations WHERE installation_id = ?1",
            params![resolved.installation_id.as_str()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(AppResourceAuthorityError::StaleRegistryAuthority(
            "installation",
        ))?;
    let installation: AppInstallation =
        decode_app_contract(&installation_bytes, &AppContractLimits::default())?;
    if installation.installation_id != resolved.installation_id || installation.scope != *scope {
        return Err(AppResourceAuthorityError::StaleRegistryAuthority(
            "accepted installation identity",
        ));
    }

    let (grant_package, grant_digest, grant_bytes) = transaction
        .query_row(
            "SELECT package_revision_ref, authority_digest, record_json
               FROM app_grant_revisions
              WHERE installation_id = ?1 AND revision = ?2",
            params![
                resolved.installation_id.as_str(),
                to_sql_i64(resolved.grant_revision.get(), "grant_revision")?,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(AppResourceAuthorityError::StaleRegistryAuthority("grant"))?;
    let grant: AppGrantRevision = decode_app_contract(&grant_bytes, &AppContractLimits::default())?;
    if grant_package != resolved.package_revision_ref.as_str()
        || grant_digest != resolved.grant_authority_digest.as_str()
        || grant.installation_id != resolved.installation_id
        || grant.revision != resolved.grant_revision
        || grant.package_revision_ref != resolved.package_revision_ref
        || grant.authority_digest != resolved.grant_authority_digest
    {
        return Err(AppResourceAuthorityError::StaleRegistryAuthority(
            "accepted grant identity",
        ));
    }

    let (schema_package, schema_bytes) = transaction
        .query_row(
            "SELECT package_revision_ref, record_json FROM app_schema_revisions
              WHERE installation_id = ?1 AND revision = ?2",
            params![
                resolved.installation_id.as_str(),
                to_sql_i64(resolved.schema_revision.get(), "schema_revision")?,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?
        .ok_or(AppResourceAuthorityError::StaleRegistryAuthority("schema"))?;
    let schema: AppSchemaRevision =
        decode_app_contract(&schema_bytes, &AppContractLimits::default())?;
    if schema_package != resolved.package_revision_ref.as_str()
        || schema.installation_id != resolved.installation_id
        || schema.revision != resolved.schema_revision
        || schema.package_revision_ref != resolved.package_revision_ref
    {
        return Err(AppResourceAuthorityError::StaleRegistryAuthority(
            "accepted schema identity",
        ));
    }
    Ok(())
}

fn ensure_accepted_authenticated_snapshot(
    authenticated_scope: &AuthenticatedAppScope,
    resolved: &ResolvedAppAuthority,
    now: &DateTime<Utc>,
) -> Result<(), AppResourceAuthorityError> {
    authenticated_scope
        .ensure_live_at(now)
        .map_err(AppRegistryError::from)?;
    let canonical_authority_digest = resolved
        .canonical_authority_digest()
        .map_err(|error| AppResourceAuthorityError::CorruptAuthority(error.to_string()))?;
    if &resolved.scope_binding_ref != authenticated_scope.scope_binding_ref()
        || &resolved.actor_ref != authenticated_scope.actor_ref()
        || &resolved.session_ref != authenticated_scope.session_ref()
        || resolved.authentication != authenticated_scope.authentication()
        || resolved.authentication_revision != authenticated_scope.authentication_revision()
        || resolved.authority_digest != canonical_authority_digest
    {
        return Err(AppResourceAuthorityError::StaleRegistryAuthority(
            "accepted authenticated authority snapshot",
        ));
    }
    Ok(())
}

fn ensure_period(
    transaction: &Transaction<'_>,
    resolved: &ResolvedAppAuthority,
    snapshot: &AppResourceAdmissionSnapshot,
    now: &DateTime<Utc>,
) -> Result<AppRevision, AppResourceAuthorityError> {
    transaction.execute(
        "INSERT INTO app_resource_periods (
             installation_id, installation_generation, period_ref, revision,
             created_at, updated_at
         ) VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(installation_id, installation_generation, period_ref) DO NOTHING",
        params![
            resolved.installation_id.as_str(),
            to_sql_i64(resolved.installation_generation, "installation_generation")?,
            snapshot.period_ref.as_str(),
            format_timestamp(now),
        ],
    )?;
    let revision: i64 = transaction.query_row(
        "SELECT revision
           FROM app_resource_periods
          WHERE installation_id = ?1
            AND installation_generation = ?2
            AND period_ref = ?3",
        params![
            resolved.installation_id.as_str(),
            to_sql_i64(resolved.installation_generation, "installation_generation")?,
            snapshot.period_ref.as_str(),
        ],
        |row| row.get(0),
    )?;
    AppRevision::new(from_sql_u64(revision, "period_revision")?)
        .map_err(AppResourceAuthorityError::from)
}

fn load_period_revision(
    transaction: &Transaction<'_>,
    resolved: &ResolvedAppAuthority,
    period_ref: &AppReference,
) -> Result<(AppRevision, bool), AppResourceAuthorityError> {
    let (revision, admissions_closed_at): (i64, Option<String>) = transaction
        .query_row(
            "SELECT revision, admissions_closed_at
               FROM app_resource_periods
              WHERE installation_id = ?1
                AND installation_generation = ?2
                AND period_ref = ?3",
            params![
                resolved.installation_id.as_str(),
                to_sql_i64(resolved.installation_generation, "installation_generation")?,
                period_ref.as_str(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| {
            AppResourceAuthorityError::CorruptAuthority(
                "resource tree references a missing installation period".to_owned(),
            )
        })?;
    Ok((
        AppRevision::new(from_sql_u64(revision, "period_revision")?)?,
        admissions_closed_at.is_some(),
    ))
}

fn load_tree_revision(
    transaction: &Transaction<'_>,
    budget_ledger_ref: &AppReference,
) -> Result<u64, AppResourceAuthorityError> {
    let revision: i64 = transaction.query_row(
        "SELECT last_event_sequence FROM app_resource_trees
          WHERE budget_ledger_ref = ?1",
        params![budget_ledger_ref.as_str()],
        |row| row.get(0),
    )?;
    from_sql_u64(revision, "journal_revision")
}

fn load_recovery_evidence(
    transaction: &Transaction<'_>,
    budget_ledger_ref: &AppReference,
) -> Result<Vec<TrustedAppResourceRecoveryEvidence>, AppResourceAuthorityError> {
    let mut statement = transaction.prepare(
        "SELECT reservation_id, observation_id, reconciliation_ref,
                reconciliation_revision, reconciled_at_elapsed_ms, evidence_digest
           FROM app_resource_recovery_evidence
          WHERE budget_ledger_ref = ?1
          ORDER BY observation_id ASC
          LIMIT ?2",
    )?;
    let limit = i64::from(HARD_MAX_JOURNAL_EVENTS) + 1;
    let mut rows = statement.query(params![budget_ledger_ref.as_str(), limit])?;
    let mut evidence = Vec::new();
    while let Some(row) = rows.next()? {
        if evidence.len() >= HARD_MAX_JOURNAL_EVENTS as usize {
            return Err(AppResourceAuthorityError::CorruptAuthority(
                "trusted recovery evidence exceeds the journal event bound".to_owned(),
            ));
        }
        let reservation_id = AppReference::parse(row.get::<_, String>(0)?)?;
        let observation_id = AppReference::parse(row.get::<_, String>(1)?)?;
        let reconciliation_ref = AppReference::parse(row.get::<_, String>(2)?)?;
        let reconciliation_revision = AppRevision::new(from_sql_u64(
            row.get::<_, i64>(3)?,
            "reconciliation_revision",
        )?)?;
        let reconciled_at_elapsed_ms =
            from_sql_u64(row.get::<_, i64>(4)?, "reconciled_at_elapsed_ms")?;
        let stored_digest = row.get::<_, String>(5)?;
        let value = TrustedAppResourceRecoveryEvidence::from_crash_reconciler(
            reservation_id,
            observation_id,
            reconciliation_ref,
            reconciliation_revision,
            reconciled_at_elapsed_ms,
        );
        if recovery_evidence_digest(budget_ledger_ref, &value)?.as_str() != stored_digest {
            return Err(AppResourceAuthorityError::CorruptAuthority(
                "trusted recovery evidence digest does not match its columns".to_owned(),
            ));
        }
        evidence.push(value);
    }
    Ok(evidence)
}

fn insert_recovery_evidence(
    transaction: &Transaction<'_>,
    budget_ledger_ref: &AppReference,
    recovery: &AppResourceCrashReconciliation,
    trusted: &TrustedAppResourceRecoveryEvidence,
    now: &DateTime<Utc>,
) -> Result<(), AppResourceAuthorityError> {
    let bytes = canonical_json_bytes(&serde_json::to_value(trusted)?)?;
    ensure_stored_record_bounded("recovery evidence", &bytes)?;
    let digest = recovery_evidence_digest(budget_ledger_ref, trusted)?;
    transaction.execute(
        "INSERT INTO app_resource_recovery_evidence (
             budget_ledger_ref, observation_id, reservation_id,
             reconciliation_ref, reconciliation_revision,
             reconciled_at_elapsed_ms, evidence_digest, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            budget_ledger_ref.as_str(),
            recovery.observation_id.as_str(),
            recovery.reservation_id.as_str(),
            recovery.reconciliation_ref.as_str(),
            to_sql_i64(
                recovery.reconciliation_revision.get(),
                "reconciliation_revision",
            )?,
            to_sql_i64(
                recovery.reconciled_at_elapsed_ms,
                "reconciled_at_elapsed_ms",
            )?,
            digest.as_str(),
            format_timestamp(now),
        ],
    )?;
    Ok(())
}

fn recovery_evidence_digest(
    budget_ledger_ref: &AppReference,
    evidence: &TrustedAppResourceRecoveryEvidence,
) -> Result<AppDigest, AppResourceAuthorityError> {
    let material = canonical_json_bytes(&serde_json::to_value((budget_ledger_ref, evidence))?)?;
    ensure_stored_record_bounded("recovery evidence identity", &material)?;
    Ok(AppDigest::blake3(&material))
}

fn ensure_recovery_replay_matches(
    transaction: &Transaction<'_>,
    budget_ledger_ref: &AppReference,
    recovery: &AppResourceCrashReconciliation,
) -> Result<(), AppResourceAuthorityError> {
    let stored: Option<(String, String, String, i64, i64)> = transaction
        .query_row(
            "SELECT reservation_id, observation_id, reconciliation_ref,
                    reconciliation_revision, reconciled_at_elapsed_ms
               FROM app_resource_recovery_evidence
              WHERE budget_ledger_ref = ?1 AND observation_id = ?2",
            params![budget_ledger_ref.as_str(), recovery.observation_id.as_str(),],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some(stored) = stored else {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "proven-unspent event is missing durable recovery evidence".to_owned(),
        ));
    };
    if stored.0 != recovery.reservation_id.as_str()
        || stored.1 != recovery.observation_id.as_str()
        || stored.2 != recovery.reconciliation_ref.as_str()
        || from_sql_u64(stored.3, "reconciliation_revision")?
            != recovery.reconciliation_revision.get()
        || from_sql_u64(stored.4, "reconciled_at_elapsed_ms")? != recovery.reconciled_at_elapsed_ms
    {
        return Err(AppResourceAuthorityError::IdentityConflict {
            entity: "crash reconciliation",
            identity: recovery.observation_id.to_string(),
        });
    }
    Ok(())
}

fn load_period_totals(
    transaction: &Transaction<'_>,
    resolved: &ResolvedAppAuthority,
    period_ref: &AppReference,
    excluded_root: Option<&ExistingRoot>,
) -> Result<PeriodTotals, AppResourceAuthorityError> {
    let row = transaction.query_row(
        "SELECT
             committed_tokens, outstanding_tokens,
             committed_cost_microusd, outstanding_cost_microusd,
             background_starts, foreground_runs, background_runs
         FROM app_resource_periods
         WHERE installation_id = ?1
           AND installation_generation = ?2
           AND period_ref = ?3",
        params![
            resolved.installation_id.as_str(),
            to_sql_i64(resolved.installation_generation, "installation_generation")?,
            period_ref.as_str(),
        ],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        },
    )?;
    let mut totals = PeriodTotals {
        committed_tokens: from_sql_u64(row.0, "committed_tokens")?,
        outstanding_tokens: from_sql_u64(row.1, "outstanding_tokens")?,
        committed_cost_microusd: from_sql_u64(row.2, "committed_cost_microusd")?,
        outstanding_cost_microusd: from_sql_u64(row.3, "outstanding_cost_microusd")?,
        background_starts: from_sql_u64(row.4, "background_starts")?,
        foreground_runs: from_sql_u16(row.5, "foreground_runs")?,
        background_runs: from_sql_u16(row.6, "background_runs")?,
    };
    if let Some(root) = excluded_root {
        totals.committed_tokens = totals
            .committed_tokens
            .checked_sub(root.committed_tokens)
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "period committed tokens are below the excluded root".to_owned(),
                )
            })?;
        totals.outstanding_tokens = totals
            .outstanding_tokens
            .checked_sub(root.outstanding_tokens)
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "period outstanding tokens are below the excluded root".to_owned(),
                )
            })?;
        totals.committed_cost_microusd = totals
            .committed_cost_microusd
            .checked_sub(root.committed_cost_microusd)
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "period committed cost is below the excluded root".to_owned(),
                )
            })?;
        totals.outstanding_cost_microusd = totals
            .outstanding_cost_microusd
            .checked_sub(root.outstanding_cost_microusd)
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "period outstanding cost is below the excluded root".to_owned(),
                )
            })?;
        match root.lane {
            AppResourceExecutionLane::Foreground if !root.terminally_settled => {
                totals.foreground_runs =
                    totals.foreground_runs.checked_sub(1).ok_or_else(|| {
                        AppResourceAuthorityError::CorruptAuthority(
                            "period foreground count is below the excluded root".to_owned(),
                        )
                    })?;
            },
            AppResourceExecutionLane::Background => {
                totals.background_starts =
                    totals.background_starts.checked_sub(1).ok_or_else(|| {
                        AppResourceAuthorityError::CorruptAuthority(
                            "period background starts are below the excluded root".to_owned(),
                        )
                    })?;
                if !root.terminally_settled {
                    totals.background_runs =
                        totals.background_runs.checked_sub(1).ok_or_else(|| {
                            AppResourceAuthorityError::CorruptAuthority(
                                "period background count is below the excluded root".to_owned(),
                            )
                        })?;
                }
            },
            AppResourceExecutionLane::Foreground => {},
        }
    }
    Ok(totals)
}

/// Read the second monthly dimension only for a scheduler-sealed behavior
/// root. Installation totals above remain trigger-maintained and continue to
/// cover every root; this indexed sum isolates the exact granted behavior so
/// another behavior cannot consume its local allowance.
fn load_behavior_period_baseline(
    transaction: &Transaction<'_>,
    resolved: &ResolvedAppAuthority,
    period_ref: &AppReference,
    identity: Option<&AppResourceBehaviorIdentity>,
    excluded_root: Option<&ExistingRoot>,
) -> Result<Option<AppResourceBehaviorPeriodBaseline>, AppResourceAuthorityError> {
    let Some(identity) = identity else {
        return Ok(None);
    };
    identity.validate_app_contract(&AppContractLimits::default())?;
    let row: (i64, i64, i64, i64) = transaction.query_row(
        "SELECT
             COALESCE(SUM(committed_tokens), 0),
             COALESCE(SUM(outstanding_tokens), 0),
             COALESCE(SUM(committed_cost_microusd), 0),
             COALESCE(SUM(outstanding_cost_microusd), 0)
           FROM app_resource_trees
          WHERE installation_id = ?1
            AND installation_generation = ?2
            AND period_ref = ?3
            AND behavior_ledger_digest = ?4",
        params![
            resolved.installation_id.as_str(),
            to_sql_i64(resolved.installation_generation, "installation_generation")?,
            period_ref.as_str(),
            identity.ledger_dimension_digest().as_str(),
        ],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let mut committed_tokens = from_sql_u64(row.0, "behavior_committed_tokens")?;
    let mut outstanding_tokens = from_sql_u64(row.1, "behavior_outstanding_tokens")?;
    let mut committed_cost = from_sql_u64(row.2, "behavior_committed_cost_microusd")?;
    let mut outstanding_cost = from_sql_u64(row.3, "behavior_outstanding_cost_microusd")?;
    if let Some(root) = excluded_root {
        if root.journal.identity.behavior_resource_identity.as_ref() != Some(identity) {
            return Err(AppResourceAuthorityError::IdentityConflict {
                entity: "behavior resource ledger",
                identity: root.journal.identity.budget_ledger_ref.to_string(),
            });
        }
        committed_tokens = committed_tokens
            .checked_sub(root.committed_tokens)
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "behavior period committed tokens are below the excluded root".to_owned(),
                )
            })?;
        outstanding_tokens = outstanding_tokens
            .checked_sub(root.outstanding_tokens)
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "behavior period outstanding tokens are below the excluded root".to_owned(),
                )
            })?;
        committed_cost = committed_cost
            .checked_sub(root.committed_cost_microusd)
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "behavior period committed cost is below the excluded root".to_owned(),
                )
            })?;
        outstanding_cost = outstanding_cost
            .checked_sub(root.outstanding_cost_microusd)
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "behavior period outstanding cost is below the excluded root".to_owned(),
                )
            })?;
    }
    Ok(Some(
        AppResourceBehaviorPeriodBaseline::from_resource_store(
            identity,
            committed_tokens,
            outstanding_tokens,
            committed_cost,
            outstanding_cost,
        ),
    ))
}

#[derive(Debug)]
struct ExistingRoot {
    journal: AppResourceJournal,
    started_at: DateTime<Utc>,
    admitted_period_revision: AppRevision,
    lane: AppResourceExecutionLane,
    committed_tokens: u64,
    outstanding_tokens: u64,
    committed_cost_microusd: u64,
    outstanding_cost_microusd: u64,
    terminally_settled: bool,
    period_ends_at_elapsed_ms: u64,
    package_bytes: u64,
}

fn load_existing_root(
    transaction: &Transaction<'_>,
    budget_ledger_ref: &AppReference,
) -> Result<Option<ExistingRoot>, AppResourceAuthorityError> {
    let Some((
        admitted_revision,
        lane,
        behavior_ledger_digest,
        committed_tokens,
        outstanding_tokens,
        committed_cost,
        outstanding_cost,
        terminally_settled,
        period_ends_at_elapsed_ms,
        package_bytes,
        created_at,
    )) = transaction
        .query_row(
            "SELECT admitted_period_revision, lane, behavior_ledger_digest,
                    committed_tokens, outstanding_tokens,
                    committed_cost_microusd, outstanding_cost_microusd,
                    terminally_settled, period_ends_at_elapsed_ms, package_bytes, created_at
               FROM app_resource_trees WHERE budget_ledger_ref = ?1",
            params![budget_ledger_ref.as_str()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    let journal = load_journal(transaction, budget_ledger_ref)?.ok_or_else(|| {
        AppResourceAuthorityError::CorruptAuthority(
            "tree exists without its canonical event journal".to_owned(),
        )
    })?;
    let expected_behavior_digest = journal
        .identity
        .behavior_resource_identity
        .as_ref()
        .map(|identity| identity.ledger_dimension_digest().as_str());
    if behavior_ledger_digest.as_deref() != expected_behavior_digest {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "behavior ledger discriminator does not match canonical tree identity".to_owned(),
        ));
    }
    Ok(Some(ExistingRoot {
        journal,
        started_at: parse_resource_timestamp(&created_at)?,
        admitted_period_revision: AppRevision::new(from_sql_u64(
            admitted_revision,
            "admitted_period_revision",
        )?)?,
        lane: parse_lane(&lane)?,
        committed_tokens: from_sql_u64(committed_tokens, "committed_tokens")?,
        outstanding_tokens: from_sql_u64(outstanding_tokens, "outstanding_tokens")?,
        committed_cost_microusd: from_sql_u64(committed_cost, "committed_cost_microusd")?,
        outstanding_cost_microusd: from_sql_u64(outstanding_cost, "outstanding_cost_microusd")?,
        terminally_settled: match terminally_settled {
            0 => false,
            1 => true,
            _ => {
                return Err(AppResourceAuthorityError::CorruptAuthority(
                    "tree terminal state is not boolean".to_owned(),
                ));
            },
        },
        period_ends_at_elapsed_ms: period_ends_at_elapsed_ms
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "resource root is missing its period deadline".to_owned(),
                )
            })
            .and_then(|value| from_sql_u64(value, "period_ends_at_elapsed_ms"))?,
        package_bytes: package_bytes
            .ok_or_else(|| {
                AppResourceAuthorityError::CorruptAuthority(
                    "resource root is missing its package measurement".to_owned(),
                )
            })
            .and_then(|value| from_sql_u64(value, "package_bytes"))?,
    }))
}

fn ensure_tree_index_matches(
    existing: &ExistingRoot,
    rebuilt: &AppResourceTreeState,
) -> Result<(), AppResourceAuthorityError> {
    if existing.committed_tokens != total_tokens(rebuilt.committed)?
        || existing.outstanding_tokens != total_tokens(rebuilt.outstanding_reserved)?
        || existing.committed_cost_microusd != rebuilt.committed.cost_microusd
        || existing.outstanding_cost_microusd != rebuilt.outstanding_reserved.cost_microusd
        || existing.terminally_settled != rebuilt.terminally_settled
    {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "tree scalar index does not match the canonical event journal".to_owned(),
        ));
    }
    Ok(())
}

fn root_execution_exists(
    transaction: &Transaction<'_>,
    installation_id: &super::models::AppInstallationId,
    root_execution_id: &AppReference,
) -> Result<bool, AppResourceAuthorityError> {
    let exists: i64 = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_resource_trees
          WHERE installation_id = ?1 AND root_execution_id = ?2)",
        params![installation_id.as_str(), root_execution_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

fn retired_tree_exists(
    transaction: &Transaction<'_>,
    budget_ledger_ref: &AppReference,
    installation_id: &super::models::AppInstallationId,
    root_execution_id: &AppReference,
) -> Result<bool, AppResourceAuthorityError> {
    let exists: i64 = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_resource_retired_trees
          WHERE budget_ledger_ref = ?1
             OR (installation_id = ?2 AND root_execution_id = ?3))",
        params![
            budget_ledger_ref.as_str(),
            installation_id.as_str(),
            root_execution_id.as_str(),
        ],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

fn load_journal(
    connection: &Connection,
    budget_ledger_ref: &AppReference,
) -> Result<Option<AppResourceJournal>, AppResourceAuthorityError> {
    let Some((identity_json, identity_len, evaluated_at, last_sequence)) = connection
        .query_row(
            "SELECT substr(identity_json, 1, ?2), length(identity_json),
                    evaluated_at_elapsed_ms, last_event_sequence
               FROM app_resource_trees WHERE budget_ledger_ref = ?1",
            params![
                budget_ledger_ref.as_str(),
                i64::try_from(MAX_STORED_RESOURCE_RECORD_BYTES + 1).map_err(|_| {
                    AppResourceAuthorityError::IntegerRange("resource_record_bytes")
                })?,
            ],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
    else {
        let retired: i64 = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM app_resource_retired_trees
              WHERE budget_ledger_ref = ?1)",
            params![budget_ledger_ref.as_str()],
            |row| row.get(0),
        )?;
        return if retired == 0 {
            Ok(None)
        } else {
            Err(AppResourceAuthorityError::RetiredTree)
        };
    };
    ensure_stored_record_length_bounded("identity", identity_len)?;
    ensure_stored_record_bounded("identity", &identity_json)?;
    let last_sequence = from_sql_u64(last_sequence, "last_event_sequence")?;
    if last_sequence == 0 || last_sequence > u64::from(HARD_MAX_JOURNAL_EVENTS) {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "stored journal sequence exceeds the hard event bound".to_owned(),
        ));
    }
    let query_limit =
        last_sequence
            .checked_add(1)
            .ok_or(AppResourceAuthorityError::IntegerRange(
                "last_event_sequence",
            ))?;
    let mut statement = connection.prepare(
        "SELECT sequence, event_digest, substr(event_json, 1, ?3), length(event_json)
           FROM app_resource_tree_events
          WHERE budget_ledger_ref = ?1
          ORDER BY sequence ASC
          LIMIT ?2",
    )?;
    let mut rows = statement.query(params![
        budget_ledger_ref.as_str(),
        to_sql_i64(query_limit, "last_event_sequence")?,
        i64::try_from(MAX_STORED_RESOURCE_RECORD_BYTES + 1)
            .map_err(|_| AppResourceAuthorityError::IntegerRange("resource_record_bytes"))?,
    ])?;
    let capacity = usize::try_from(last_sequence)
        .map_err(|_| AppResourceAuthorityError::IntegerRange("last_event_sequence"))?;
    let document_limit = AppContractLimits::default().max_document_bytes();
    let evaluated_at = from_sql_u64(evaluated_at, "evaluated_at_elapsed_ms")?;
    let evaluated_at = evaluated_at.to_string();
    let mut journal_bytes = Vec::with_capacity(identity_json.len().saturating_add(256));
    append_journal_bytes(&mut journal_bytes, b"{\"identity\":", document_limit)?;
    append_journal_bytes(&mut journal_bytes, &identity_json, document_limit)?;
    append_journal_bytes(
        &mut journal_bytes,
        b",\"evaluated_at_elapsed_ms\":",
        document_limit,
    )?;
    append_journal_bytes(&mut journal_bytes, evaluated_at.as_bytes(), document_limit)?;
    append_journal_bytes(&mut journal_bytes, b",\"events\":[", document_limit)?;
    let mut event_count = 0_usize;
    while let Some(row) = rows.next()? {
        let stored_sequence = from_sql_u64(row.get::<_, i64>(0)?, "event_sequence")?;
        let expected_sequence = u64::try_from(event_count)
            .map_err(|_| AppResourceAuthorityError::IntegerRange("event_sequence"))?
            .checked_add(1)
            .ok_or(AppResourceAuthorityError::IntegerRange("event_sequence"))?;
        if stored_sequence != expected_sequence {
            return Err(AppResourceAuthorityError::CorruptAuthority(
                "stored journal event sequence is not contiguous".to_owned(),
            ));
        }
        let event_digest = row.get::<_, String>(1)?;
        let event_json = row.get::<_, Vec<u8>>(2)?;
        ensure_stored_record_length_bounded("event", row.get::<_, i64>(3)?)?;
        ensure_stored_record_bounded("event", &event_json)?;
        ensure_event_digest(&event_digest, &event_json)?;
        if event_count != 0 {
            append_journal_bytes(&mut journal_bytes, b",", document_limit)?;
        }
        append_journal_bytes(&mut journal_bytes, &event_json, document_limit)?;
        event_count = event_count
            .checked_add(1)
            .ok_or(AppResourceAuthorityError::IntegerRange("event_count"))?;
    }
    if event_count != capacity {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "stored journal event count does not match its revision".to_owned(),
        ));
    }
    append_journal_bytes(&mut journal_bytes, b"]}", document_limit)?;
    let journal = decode_app_resource_journal(&journal_bytes)
        .map_err(|error| AppResourceAuthorityError::CorruptAuthority(error.to_string()))?;
    Ok(Some(journal))
}

fn append_journal_bytes(
    destination: &mut Vec<u8>,
    fragment: &[u8],
    limit: usize,
) -> Result<(), AppResourceAuthorityError> {
    let next_len = destination
        .len()
        .checked_add(fragment.len())
        .ok_or_else(|| {
            AppResourceAuthorityError::CorruptAuthority(
                "stored journal byte count overflowed".to_owned(),
            )
        })?;
    if next_len > limit {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "stored journal exceeds the canonical document byte bound".to_owned(),
        ));
    }
    destination.extend_from_slice(fragment);
    Ok(())
}

fn total_tokens(quantity: AppResourceQuantity) -> Result<u64, AppResourceAuthorityError> {
    quantity
        .input_tokens
        .checked_add(quantity.output_tokens)
        .ok_or(AppResourceAuthorityError::Contract(
            AppResourceContractError::ArithmeticOverflow("total_tokens"),
        ))
}

fn lane_label(lane: AppResourceExecutionLane) -> &'static str {
    match lane {
        AppResourceExecutionLane::Foreground => "foreground",
        AppResourceExecutionLane::Background => "background",
    }
}

fn parse_lane(value: &str) -> Result<AppResourceExecutionLane, AppResourceAuthorityError> {
    match value {
        "foreground" => Ok(AppResourceExecutionLane::Foreground),
        "background" => Ok(AppResourceExecutionLane::Background),
        _ => Err(AppResourceAuthorityError::CorruptAuthority(
            "tree lane is not canonical".to_owned(),
        )),
    }
}

fn ensure_stored_record_bounded(
    label: &'static str,
    bytes: &[u8],
) -> Result<(), AppResourceAuthorityError> {
    if bytes.is_empty() || bytes.len() > MAX_STORED_RESOURCE_RECORD_BYTES {
        return Err(AppResourceAuthorityError::CorruptAuthority(format!(
            "{label} bytes are empty or exceed the per-record bound"
        )));
    }
    Ok(())
}

fn ensure_stored_record_length_bounded(
    label: &'static str,
    stored_len: i64,
) -> Result<(), AppResourceAuthorityError> {
    let stored_len = usize::try_from(stored_len)
        .map_err(|_| AppResourceAuthorityError::IntegerRange("resource_record_bytes"))?;
    if stored_len == 0 || stored_len > MAX_STORED_RESOURCE_RECORD_BYTES {
        return Err(AppResourceAuthorityError::CorruptAuthority(format!(
            "{label} stored length is empty or exceeds the per-record bound"
        )));
    }
    Ok(())
}

fn ensure_event_digest(
    stored_digest: &str,
    event_json: &[u8],
) -> Result<(), AppResourceAuthorityError> {
    if AppDigest::blake3(event_json).as_str() != stored_digest {
        return Err(AppResourceAuthorityError::CorruptAuthority(
            "stored journal event digest does not match its stored bytes".to_owned(),
        ));
    }
    Ok(())
}

fn to_sql_i64(value: u64, field: &'static str) -> Result<i64, AppResourceAuthorityError> {
    i64::try_from(value).map_err(|_| AppResourceAuthorityError::IntegerRange(field))
}

fn from_sql_u64(value: i64, field: &'static str) -> Result<u64, AppResourceAuthorityError> {
    u64::try_from(value).map_err(|_| AppResourceAuthorityError::IntegerRange(field))
}

fn from_sql_u16(value: i64, field: &'static str) -> Result<u16, AppResourceAuthorityError> {
    u16::try_from(value).map_err(|_| AppResourceAuthorityError::IntegerRange(field))
}

pub fn format_timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn parse_resource_timestamp(value: &str) -> Result<DateTime<Utc>, AppResourceAuthorityError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| {
            AppResourceAuthorityError::CorruptAuthority(format!(
                "resource timestamp is not canonical RFC 3339: {error}"
            ))
        })
}

fn canonical_resource_timestamp(
    value: DateTime<Utc>,
) -> Result<DateTime<Utc>, AppResourceAuthorityError> {
    parse_resource_timestamp(&format_timestamp(&value))
}

fn elapsed_since_root(
    started_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<u64, AppResourceAuthorityError> {
    let elapsed_ms = now.signed_duration_since(started_at).num_milliseconds();
    u64::try_from(elapsed_ms).map_err(|_| {
        AppResourceAuthorityError::InvalidObservation(
            "resource observation precedes the durable root admission",
        )
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;
    use crate::magician_v2::{
        apps::{
            authority::AppScopeAuthentication,
            lifecycle::AppInstallationLifecycle,
            models::{
                AppDataClassification, AppExpectedRecordRevision, AppModelProcessing,
                AppMutationAtomicity, AppName, AppProtocolVersion, AppRecordId,
            },
            records::{
                AppBackgroundExecution, AppChangeSequenceRange, AppCommittedRecordRevision,
                AppDataHandlingPolicy, AppExternalEgress, AppMemoryPromotion, AppNetworkPolicy,
                AppPersonalAgentAccess, AppResourceCeiling, AppSchemaCompatibility,
            },
            registry::tests::{
                authenticated_scope, canonical_tempdir, publication, reference, time,
            },
        },
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    struct Fixture {
        _temporary: tempfile::TempDir,
        service: AppResourceAuthorityService,
        coordinator: AppResourceRuntimeCoordinator,
        authenticated: AuthenticatedAppScope,
        resolved: ResolvedAppAuthority,
        binding: AppRunBinding,
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).unwrap()
    }

    fn milliseconds_after(base: DateTime<Utc>, elapsed_ms: i64) -> DateTime<Utc> {
        base + chrono::Duration::milliseconds(elapsed_ms)
    }

    fn resolved_at(resolved: &ResolvedAppAuthority, now: DateTime<Utc>) -> ResolvedAppAuthority {
        let mut resolved = resolved.clone();
        resolved.resolved_at = now;
        resolved
    }

    fn handling_policy() -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Ordinary,
            model_processing: AppModelProcessing::None,
            personal_agent_access: AppPersonalAgentAccess::Denied,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn resources(concurrency: u16) -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: 10_000,
            max_output_tokens: 10_000,
            max_cost_microusd: 10_000,
            max_paid_tool_invocations: 100,
            max_active_seconds: 100,
            max_lifetime_seconds: 100,
            max_browser_network_actions: 100,
            max_concurrent_foreground_runs: concurrency,
            max_concurrent_background_runs: concurrency,
            max_records: 10_000,
            max_payload_bytes: 1_000_000,
            max_attachment_bytes: 1_000_000,
            max_monthly_tokens: 100_000,
            max_monthly_cost_microusd: 100_000,
        }
    }

    fn enforcement_policy() -> AppResourceEnforcementPolicy {
        AppResourceEnforcementPolicy {
            max_tree_nodes: 64,
            max_tree_depth: 8,
            max_journal_events: 256,
            max_reservations: 64,
            max_active_intervals: 64,
            max_capability_families: 16,
            max_no_progress_seconds: 30,
            max_package_bytes: 10 * 1_024 * 1_024,
            max_background_starts_per_period: 20,
            scheduler_capacity: 8,
            foreground_reserved_slots: 2,
        }
    }

    fn admission_snapshot(fixture: &Fixture) -> AppResourceAdmissionSnapshot {
        admission_snapshot_with_scheduler(fixture, 0, 0)
    }

    fn admission_snapshot_with_scheduler(
        fixture: &Fixture,
        foreground_runs: u16,
        background_runs: u16,
    ) -> AppResourceAdmissionSnapshot {
        AppResourceAdmissionSnapshot::from_authoritative_owners(
            reference("period:2026-08"),
            100_000,
            AppResourcePackageMeasurement::from_package_store(
                fixture.resolved.package_revision_ref.clone(),
                100,
            ),
            AppResourceSchedulerAdmissionGuard::isolated(
                AppResourceExecutionLane::Foreground,
                foreground_runs,
                background_runs,
            ),
            time(4),
        )
        .unwrap()
    }

    async fn fixture(concurrency: u16) -> Fixture {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let registry = AppRegistryService::new(workspace.clone());
        let coordinator = AppResourceRuntimeCoordinator::new(workspace.clone(), 8, 2).unwrap();
        let authenticated = authenticated_scope("anonymous", "default");
        let publication = publication(
            &workspace,
            "anonymous",
            "default",
            "attempt:resource",
            "install_resource",
        );
        let publication_receipt = registry
            .publish_ready_for_review(&authenticated, publication, time(2))
            .await
            .unwrap();
        let installation_id = publication_receipt.installation_id.clone();
        let package_revision_ref = publication_receipt.package_revision_ref.clone();
        let grant_authority_digest = AppDigest::blake3(b"resource-grant-authority");
        let policy = handling_policy();
        let resource_ceiling = resources(concurrency);
        let grant = AppGrantRevision {
            installation_id: installation_id.clone(),
            revision: revision(1),
            package_revision_ref: package_revision_ref.clone(),
            requested_tools: Vec::new(),
            granted_tools: Vec::new(),
            requested_agents: Vec::new(),
            granted_agents: Vec::new(),
            requested_personalities: Vec::new(),
            granted_personalities: Vec::new(),
            requested_interactive_capabilities: Vec::new(),
            granted_interactive_capabilities: Vec::new(),
            granted_custom_surface_entry_points: Vec::new(),
            requested_behavior_grants: Vec::new(),
            granted_behavior_grants: Vec::new(),
            requested_event_behavior_grants: Vec::new(),
            granted_event_behavior_grants: Vec::new(),
            requested_notification_grants: Vec::new(),
            granted_notification_grants: Vec::new(),
            requested_memory_read: None,
            granted_memory_read: None,
            requested_secret_uses: None,
            granted_secret_uses: None,
            granted_any_public_host: false,
            requested_context_reads: Vec::new(),
            granted_context_reads: Vec::new(),
            requested_personal_agent_data_access: Vec::new(),
            granted_personal_agent_data_access: Vec::new(),
            requested_data_handling_policy: policy.clone(),
            granted_data_handling_policy: policy.clone(),
            granted_data_handling_policy_digest: AppDigest::blake3(b"policy"),
            requested_background_execution: AppBackgroundExecution::Denied,
            granted_background_execution: AppBackgroundExecution::Denied,
            requested_network_policy: AppNetworkPolicy::Denied,
            granted_network_policy: AppNetworkPolicy::Denied,
            requested_resource_ceiling: resource_ceiling.clone(),
            granted_resource_ceiling: resource_ceiling.clone(),
            approved_by: reference("actor:owner"),
            approved_at: time(2),
            authority_digest: grant_authority_digest.clone(),
            revoked_at: None,
        };
        let schema = AppSchemaRevision {
            installation_id: installation_id.clone(),
            revision: revision(1),
            package_revision_ref: package_revision_ref.clone(),
            canonical_entity_schema: json!({"note": {"title": "text"}}),
            canonical_data_handling_policy: policy.clone(),
            compiled_validation_schema: json!({"type": "object"}),
            compiled_index_plan: json!({}),
            compatibility_with_previous: AppSchemaCompatibility::Initial,
            migration_plan_ref: None,
            created_at: time(2),
        };
        let mut installation = registry
            .installation(&authenticated, &installation_id, time(2))
            .await
            .unwrap()
            .unwrap();
        installation.lifecycle = AppInstallationLifecycle {
            status: AppInstallationStatus::Enabled,
            generation: 1,
            update_return_status: None,
        };
        installation.grant_revision = Some(revision(1));
        installation.active_schema_revision = Some(revision(1));
        installation.active_surface_revision = Some(revision(1));
        installation.updated_at = time(2);
        let grant_json = canonical_json_bytes(&serde_json::to_value(&grant).unwrap()).unwrap();
        let schema_json = canonical_json_bytes(&serde_json::to_value(&schema).unwrap()).unwrap();
        let installation_json =
            canonical_json_bytes(&serde_json::to_value(&installation).unwrap()).unwrap();
        let insert_installation_id = installation_id.clone();
        let insert_package_ref = package_revision_ref.clone();
        let insert_authority_digest = grant_authority_digest.clone();
        registry
            .execute_scoped_write(&authenticated, &time(2), move |connection, _scope| {
                connection.execute(
                    "INSERT INTO app_grant_revisions (
                         installation_id, revision, package_revision_ref, authority_digest,
                         granted_data_policy_digest, revoked_at, record_json, created_at
                     ) VALUES (?1, 1, ?2, ?3, ?4, NULL, ?5, ?6)",
                    params![
                        insert_installation_id.as_str(),
                        insert_package_ref.as_str(),
                        insert_authority_digest.as_str(),
                        grant.granted_data_handling_policy_digest.as_str(),
                        grant_json,
                        format_timestamp(&time(2)),
                    ],
                )?;
                connection.execute(
                    "INSERT INTO app_schema_revisions (
                         installation_id, revision, package_revision_ref, record_json, created_at
                     ) VALUES (?1, 1, ?2, ?3, ?4)",
                    params![
                        insert_installation_id.as_str(),
                        insert_package_ref.as_str(),
                        schema_json,
                        format_timestamp(&time(2)),
                    ],
                )?;
                connection.execute(
                    "UPDATE app_installations
                        SET lifecycle_status = 'enabled', lifecycle_generation = 1,
                            record_json = ?1, updated_at = ?2
                      WHERE installation_id = ?3",
                    params![
                        installation_json,
                        format_timestamp(&time(2)),
                        insert_installation_id.as_str(),
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let mut resolved = ResolvedAppAuthority {
            scope_binding_ref: authenticated.scope_binding_ref().clone(),
            actor_ref: authenticated.actor_ref().clone(),
            session_ref: authenticated.session_ref().clone(),
            authentication: AppScopeAuthentication::AuthenticatedSession,
            authentication_revision: authenticated.authentication_revision(),
            installation_id: installation_id.clone(),
            installation_generation: 1,
            package_revision_ref: package_revision_ref.clone(),
            grant_revision: revision(1),
            grant_authority_digest,
            schema_revision: revision(1),
            surface_revision: None,
            authority_digest: AppDigest::blake3(b"pending-effective-authority"),
            effective_tools: BTreeSet::new(),
            effective_context_reads: BTreeSet::new(),
            effective_data_handling_policy: policy,
            effective_background_execution: AppBackgroundExecution::Denied,
            effective_network_policy: AppNetworkPolicy::Denied,
            effective_resources: resource_ceiling,
            effective_any_public_host: false,
            resolved_at: time(4),
        };
        resolved.authority_digest = resolved.canonical_authority_digest().unwrap();
        let authority_digest = resolved.authority_digest.clone();
        let binding = AppRunBinding {
            scope: authenticated.scope().clone(),
            installation_id,
            package_revision_ref,
            workflow_id: AppName::parse("summarize").unwrap(),
            behavior_resource_identity: None,
            execution_id: reference("execution:root"),
            resolved_agent_id: reference("agent:assistant"),
            authority_digest,
            budget_ledger_ref: reference("ledger:root"),
            schema_revision: revision(1),
        };
        Fixture {
            _temporary: temporary,
            service: AppResourceAuthorityService::new(registry),
            coordinator,
            authenticated,
            resolved,
            binding,
        }
    }

    async fn revoke_fixture_grant(fixture: &Fixture, now: DateTime<Utc>) {
        let installation_id = fixture.resolved.installation_id.clone();
        fixture
            .service
            .registry
            .execute_scoped_write(&fixture.authenticated, &now, move |connection, _| {
                connection.execute(
                    "UPDATE app_grant_revisions SET revoked_at = ?1
                      WHERE installation_id = ?2 AND revision = 1",
                    params![format_timestamp(&now), installation_id.as_str()],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    fn accepted_cleanup_snapshot(fixture: &Fixture) -> AppResourceAcceptedCleanupSnapshot {
        AppResourceAcceptedCleanupSnapshot::from_execution_owner(
            fixture.resolved.scope_binding_ref.clone(),
            fixture.resolved.installation_generation,
            fixture.resolved.surface_revision,
            AppAuthorityCeiling {
                tools: fixture.resolved.effective_tools.clone(),
                context_reads: fixture.resolved.effective_context_reads.clone(),
                data_handling_policy: fixture.resolved.effective_data_handling_policy.clone(),
                background_execution: fixture.resolved.effective_background_execution.clone(),
                network_policy: fixture.resolved.effective_network_policy.clone(),
                resources: fixture.resolved.effective_resources.clone(),
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn runtime_coordinator_owns_package_period_scheduler_and_durable_root_admission() {
        let fixture = fixture(2).await;
        let receipt = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(receipt.outcome, AppResourceRootAdmissionOutcome::Created);
        assert_eq!(
            receipt.identity.installation_period_ref,
            reference("period:2026-08")
        );
        let root_lease = receipt.into_dispatch_lease().unwrap();
        assert_eq!(
            root_lease.package_revision_ref,
            fixture.resolved.package_revision_ref
        );
        assert!(root_lease.package_bytes > 0);
        assert!(root_lease.period_ends_at_elapsed_ms > 0);
        let reserved_at = milliseconds_after(time(4), 1);
        let reservation = AppResourceReservationRequest::from_dispatch_owner(
            reference("node:root"),
            reference("reservation:coordinator"),
            reference("operation:coordinator"),
            AppResourceQuantity {
                input_tokens: 10,
                output_tokens: 5,
                cost_microusd: 20,
                ..AppResourceQuantity::default()
            },
            Vec::new(),
            1,
            50_000,
        );
        let reserved = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                reservation.clone(),
                reserved_at,
            )
            .await
            .unwrap();
        let replayed_at = milliseconds_after(time(4), 2);
        let replay = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, replayed_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                reservation,
                replayed_at,
            )
            .await
            .unwrap();
        assert_eq!(
            replay.mutation.outcome,
            AppResourceAppendOutcome::AlreadyPresent
        );
        assert!(matches!(
            replay.into_dispatch_permit(),
            Err(AppResourceAuthorityError::DispatchPermitUnavailable)
        ));
        let permit = reserved.into_dispatch_permit().unwrap();
        let settled_at = milliseconds_after(time(4), 3);
        let settled = fixture
            .coordinator
            .settle_operation(
                &fixture.authenticated,
                &root_lease,
                permit,
                resolved_at(&fixture.resolved, settled_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                AppResourceSettlementObservation::from_llm_task_ledger(
                    reference("node:root"),
                    reference("reservation:coordinator"),
                    reference("observation:coordinator"),
                    AppResourceQuantity {
                        input_tokens: 8,
                        output_tokens: 4,
                        cost_microusd: 16,
                        ..AppResourceQuantity::default()
                    },
                    vec![AppActiveInterval {
                        start_elapsed_ms: 1,
                        end_elapsed_ms: 3,
                    }],
                    3,
                ),
                settled_at,
            )
            .await
            .unwrap();
        assert_eq!(settled.state.committed.input_tokens, 8);
        let uncertain_reserved_at = milliseconds_after(time(4), 4);
        let uncertain_reservation = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, uncertain_reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 3,
                    minimum_period_revision: revision(4),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:coordinator-crash"),
                    reference("operation:coordinator-crash"),
                    AppResourceQuantity {
                        paid_tool_invocations: 1,
                        cost_microusd: 5,
                        ..AppResourceQuantity::default()
                    },
                    vec![AppCapabilityResourceQuantity {
                        capability_family: AppName::parse("search").unwrap(),
                        paid_invocations: 1,
                        cost_microusd: 5,
                    }],
                    4,
                    50_000,
                ),
                uncertain_reserved_at,
            )
            .await
            .unwrap();
        let uncertain_permit = uncertain_reservation.into_dispatch_permit().unwrap();
        let uncertain_at = milliseconds_after(time(4), 5);
        let uncertain = fixture
            .coordinator
            .settle_operation(
                &fixture.authenticated,
                &root_lease,
                uncertain_permit,
                resolved_at(&fixture.resolved, uncertain_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 4,
                    minimum_period_revision: revision(5),
                },
                AppResourceSettlementObservation::uncertain_from_tool_runtime(
                    reference("node:root"),
                    reference("reservation:coordinator-crash"),
                    reference("observation:coordinator-crash-uncertain"),
                    5,
                )
                .unwrap(),
                uncertain_at,
            )
            .await
            .unwrap();
        assert_eq!(uncertain.state.uncertain_reservations, 1);
        drop(root_lease);
        let reconciled_at = milliseconds_after(time(4), 6);
        let reconciled = fixture
            .coordinator
            .reconcile_crashed_operation(
                &fixture.authenticated,
                resolved_at(&fixture.resolved, reconciled_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 5,
                    minimum_period_revision: revision(6),
                },
                AppResourceCrashReconciliation::from_crash_reconciler(
                    reference("node:root"),
                    reference("reservation:coordinator-crash"),
                    reference("observation:coordinator-crash-recovered"),
                    reference("reconciliation:coordinator-crash"),
                    revision(1),
                    6,
                ),
                reconciled_at,
            )
            .await
            .unwrap();
        assert_eq!(reconciled.state.uncertain_reservations, 0);
    }

    #[tokio::test]
    async fn app_runtime_concurrency_regression_unadmitted_root_requires_absent_canonical_identity()
    {
        let fixture = fixture(2).await;
        assert!(fixture
            .coordinator
            .root_was_never_admitted(&fixture.authenticated, &fixture.binding, time(4))
            .await
            .unwrap());
        let mut foreign = fixture.binding.clone();
        foreign.scope.workspace = reference("other");
        assert!(fixture
            .coordinator
            .root_was_never_admitted(&fixture.authenticated, &foreign, time(4))
            .await
            .is_err());
        let _root = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        assert!(!fixture
            .coordinator
            .root_was_never_admitted(&fixture.authenticated, &fixture.binding, time(4))
            .await
            .unwrap());
        let mut wrong_ledger = fixture.binding.clone();
        wrong_ledger.budget_ledger_ref = reference("ledger:other");
        assert!(!fixture
            .coordinator
            .root_was_never_admitted(&fixture.authenticated, &wrong_ledger, time(4))
            .await
            .unwrap());
        let mut wrong_root = fixture.binding.clone();
        wrong_root.execution_id = reference("exec_other");
        assert!(!fixture
            .coordinator
            .root_was_never_admitted(&fixture.authenticated, &wrong_root, time(4))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn app_runtime_concurrency_regression_dispatch_clock_allows_task_then_root() {
        let fixture = fixture(2).await;
        let receipt = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        let root = tokio::sync::Mutex::new(Some(receipt.into_dispatch_lease().unwrap()));
        let task = tokio::sync::Mutex::new(());
        let held_task = task.lock().await;
        let clock = {
            let root = root.lock().await;
            root.as_ref().unwrap().dispatch_clock()
        };
        // Model readiness is complete; its final claim callback needs task.
        // Meanwhile a commit already owns task and needs root. The old
        // retained root guard made these two awaits a permanent cycle.
        let commit = async {
            let _root = root.lock().await;
            drop(held_task);
        };
        let disclose_and_start = async {
            let _task = task.lock().await;
            assert_eq!(
                clock.elapsed_at(milliseconds_after(time(4), 37)).unwrap(),
                37
            );
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(commit, disclose_and_start);
        })
        .await
        .expect("disclosure and a same-root commit must both progress");
        assert!(clock.elapsed_at(milliseconds_after(time(4), -1)).is_err());
        assert_eq!(
            clock
                .elapsed_at(milliseconds_after(time(4), 60_000))
                .unwrap(),
            60_000
        );
        let lease = root.lock().await.take().unwrap();
        drop(lease);
        assert!(clock
            .elapsed_at(milliseconds_after(time(4), 60_001))
            .is_err());

        // Reacquisition does not revive an old physical start witness.
        let replacement = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        assert!(clock.elapsed_at(time(4)).is_err());
        assert_eq!(replacement.dispatch_clock().elapsed_at(time(4)).unwrap(), 0);
    }

    #[tokio::test]
    async fn app_runtime_concurrency_regression_journal_reads_during_progress() {
        let fixture = fixture(2).await;
        let root = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        let mut fence = AppResourceMutationFence {
            expected_journal_revision: root.state.journal_revision,
            minimum_period_revision: root.current_period_revision,
        };
        let lease = root.into_dispatch_lease().unwrap();
        let writer = async {
            for index in 1..=64 {
                let now = milliseconds_after(time(4), index);
                let receipt = fixture
                    .coordinator
                    .observe_execution_progress(
                        &fixture.authenticated,
                        &lease,
                        resolved_at(&fixture.resolved, now),
                        fixture.binding.clone(),
                        enforcement_policy(),
                        fence,
                        AppResourceProgressObservation::from_execution_owner(
                            reference("node:root"),
                            reference(&format!("progress:concurrent-{index}")),
                            u64::try_from(index).unwrap(),
                        ),
                        now,
                    )
                    .await
                    .unwrap();
                fence = AppResourceMutationFence {
                    expected_journal_revision: receipt.state.journal_revision,
                    minimum_period_revision: receipt.current_period_revision,
                };
            }
        };
        let readers = futures_util::future::join_all((0..4).map(|_| async {
            let mut previous_count = 0;
            for _ in 0..64 {
                let journal = fixture
                    .service
                    .journal(
                        &fixture.authenticated,
                        &fixture.binding.budget_ledger_ref,
                        time(4),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                assert!(journal.events.len() >= previous_count);
                previous_count = journal.events.len();
                journal
                    .validate_app_contract(&AppContractLimits::default())
                    .unwrap();
                let last_elapsed = match journal.events.last().unwrap() {
                    AppResourceJournalEvent::NodeOpened { at_elapsed_ms, .. }
                    | AppResourceJournalEvent::ProgressObserved { at_elapsed_ms, .. } => {
                        *at_elapsed_ms
                    },
                    _ => panic!("unexpected journal event in progress-only scenario"),
                };
                assert_eq!(last_elapsed, journal.evaluated_at_elapsed_ms);
            }
        }));
        tokio::join!(writer, readers);
        let final_journal = fixture
            .service
            .journal(
                &fixture.authenticated,
                &fixture.binding.budget_ledger_ref,
                time(4),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(final_journal.events.len(), 65);
        assert_eq!(final_journal.evaluated_at_elapsed_ms, 64);
    }

    #[tokio::test]
    async fn runtime_coordinator_owns_the_complete_execution_node_lifecycle() {
        let fixture = fixture(2).await;
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let opened_at = milliseconds_after(time(4), 1);
        let opened = fixture
            .coordinator
            .open_execution_node(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, opened_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceNodeAdmission::resume(
                    reference("node:resume"),
                    reference("execution:resume"),
                    reference("node:root"),
                    1,
                ),
                opened_at,
            )
            .await
            .unwrap();
        assert_eq!(opened.state.open_nodes, 2);
        let progress_at = milliseconds_after(time(4), 2);
        let progress = fixture
            .coordinator
            .observe_execution_progress(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, progress_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                AppResourceProgressObservation::from_execution_owner(
                    reference("node:resume"),
                    reference("progress:resume"),
                    2,
                ),
                progress_at,
            )
            .await
            .unwrap();
        assert_eq!(progress.state.last_progress_elapsed_ms, 2);
        let child_close_at = milliseconds_after(time(4), 3);
        fixture
            .coordinator
            .close_execution_node(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, child_close_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 3,
                    minimum_period_revision: revision(4),
                },
                AppResourceNodeClose::from_execution_owner(reference("node:resume"), 3),
                child_close_at,
            )
            .await
            .unwrap();
        let root_close_at = milliseconds_after(time(4), 4);
        let terminal = fixture
            .coordinator
            .close_execution_node(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, root_close_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 4,
                    minimum_period_revision: revision(5),
                },
                AppResourceNodeClose::from_execution_owner(reference("node:root"), 4),
                root_close_at,
            )
            .await
            .unwrap();
        assert!(terminal.state.terminally_settled);
    }

    #[tokio::test]
    async fn late_progress_cannot_erase_an_already_incurred_no_progress_breach() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let extended_authentication = AuthenticatedAppScope::from_verified_session(
            fixture.authenticated.scope().clone(),
            fixture.authenticated.scope_binding_ref().clone(),
            fixture.authenticated.actor_ref().clone(),
            fixture.authenticated.session_ref().clone(),
            fixture.authenticated.authentication_revision(),
            time(0),
            admitted_at + chrono::Duration::minutes(2),
        )
        .unwrap();
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &extended_authentication,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let late_at = milliseconds_after(admitted_at, 30_001);
        let error = fixture
            .coordinator
            .observe_execution_progress(
                &extended_authentication,
                &root_lease,
                resolved_at(&fixture.resolved, late_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceProgressObservation::from_execution_owner(
                    reference("node:root"),
                    reference("progress:too-late"),
                    30_001,
                ),
                late_at,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppResourceAuthorityError::Contract(AppResourceContractError::ReservationDenied(
                AppResourceBreach::NoProgress
            ))
        ));

        let close_at = milliseconds_after(admitted_at, 30_002);
        let closed = fixture
            .coordinator
            .close_execution_node(
                &extended_authentication,
                &root_lease,
                resolved_at(&fixture.resolved, close_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceNodeClose::from_execution_owner(reference("node:root"), 30_002),
                close_at,
            )
            .await
            .unwrap();
        assert_eq!(closed.state.journal_revision, 2);
        assert!(closed.state.terminally_settled);
    }

    #[tokio::test]
    async fn accepted_cleanup_reacquires_after_revocation_and_can_only_close_existing_work() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let admitted = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap();
        assert_eq!(admitted.state.journal_revision, 1);
        drop(admitted.into_dispatch_lease().unwrap());
        revoke_fixture_grant(&fixture, milliseconds_after(admitted_at, 1)).await;

        let proof = AppResourceAcceptedCleanupProof::from_terminal_owner(
            &fixture.binding,
            1,
            reference("task-terminal:root"),
            revision(1),
        )
        .unwrap();
        let receipt = fixture
            .coordinator
            .reacquire_accepted_workflow_root_for_cleanup(
                &fixture.authenticated,
                fixture.binding.clone(),
                accepted_cleanup_snapshot(&fixture),
                proof,
                enforcement_policy(),
                milliseconds_after(admitted_at, 2),
            )
            .await
            .unwrap();
        assert!(!receipt.state.terminally_settled);
        let mut cleanup = receipt.into_cleanup_lease().unwrap();
        let closed = fixture
            .coordinator
            .close_next_accepted_execution_node(
                &fixture.authenticated,
                &mut cleanup,
                milliseconds_after(admitted_at, 3),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(closed.state.terminally_settled);
        assert!(!cleanup.has_pending_node_closures());
        assert!(fixture
            .coordinator
            .close_next_accepted_execution_node(
                &fixture.authenticated,
                &mut cleanup,
                milliseconds_after(admitted_at, 4),
            )
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn app_runtime_concurrency_regression_closed_node_uncertain_spend_recovers() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let requested = AppResourceQuantity {
            input_tokens: 32,
            output_tokens: 8,
            cost_microusd: 17,
            ..AppResourceQuantity::default()
        };
        let reserved_at = milliseconds_after(admitted_at, 1);
        let reserved = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:closed-node-uncertain"),
                    reference("operation:closed-node-uncertain"),
                    requested,
                    Vec::new(),
                    1,
                    60_000,
                ),
                reserved_at,
            )
            .await
            .unwrap();
        let uncertain_at = milliseconds_after(admitted_at, 2);
        fixture
            .coordinator
            .settle_operation(
                &fixture.authenticated,
                &root_lease,
                reserved.into_dispatch_permit().unwrap(),
                resolved_at(&fixture.resolved, uncertain_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                AppResourceSettlementObservation::uncertain_from_tool_runtime(
                    reference("node:root"),
                    reference("reservation:closed-node-uncertain"),
                    reference("observation:closed-node-uncertain"),
                    2,
                )
                .unwrap(),
                uncertain_at,
            )
            .await
            .unwrap();
        let closed_at = milliseconds_after(admitted_at, 10);
        let closed = fixture
            .coordinator
            .close_execution_node(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, closed_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 3,
                    minimum_period_revision: revision(4),
                },
                AppResourceNodeClose::from_execution_owner(reference("node:root"), 10),
                closed_at,
            )
            .await
            .unwrap();
        assert!(closed.state.root_closed);
        assert!(!closed.state.terminally_settled);
        assert_eq!(closed.state.outstanding_reserved, requested);
        drop(root_lease);
        revoke_fixture_grant(&fixture, milliseconds_after(admitted_at, 11)).await;

        let recovery_at = milliseconds_after(admitted_at, 20);
        let mut cleanup = fixture
            .coordinator
            .reacquire_accepted_workflow_root_for_cleanup(
                &fixture.authenticated,
                fixture.binding.clone(),
                accepted_cleanup_snapshot(&fixture),
                AppResourceAcceptedCleanupProof::from_crash_reconciler(
                    &fixture.binding,
                    4,
                    reference("task-crash:closed-node-uncertain"),
                    revision(1),
                )
                .unwrap(),
                enforcement_policy(),
                recovery_at,
            )
            .await
            .unwrap()
            .into_cleanup_lease()
            .unwrap();
        assert!(!cleanup.has_pending_node_closures());
        let pending = &cleanup.pending_reservations()[0];
        assert_eq!(pending.expires_at_elapsed_ms, 60_000);
        assert_eq!(pending.node_closed_at_elapsed_ms, Some(10));
        let charged = fixture
            .coordinator
            .conservatively_charge_accepted_post_io_reservation(
                &fixture.authenticated,
                &mut cleanup,
                &reference("reservation:closed-node-uncertain"),
                recovery_at,
            )
            .await
            .expect("closed-node recovery must not charge active time after closure");
        assert!(charged.state.terminally_settled);
        assert_eq!(charged.state.committed, requested);
        assert_eq!(
            charged.state.outstanding_reserved,
            AppResourceQuantity::default()
        );
        assert_eq!(charged.state.active_elapsed_ms, 9);
        assert_eq!(charged.state.lifetime_elapsed_ms, 10);
        assert!(!cleanup.has_pending_reservations());
    }

    #[tokio::test]
    async fn accepted_crash_effect_cleanup_releases_concurrency_and_preserves_journal() {
        for cost in [0, 17] {
            let mut fixture = fixture(1).await;
            let admitted_at = time(4);
            fixture.authenticated = AuthenticatedAppScope::from_verified_session(
                fixture.authenticated.scope().clone(),
                fixture.authenticated.scope_binding_ref().clone(),
                fixture.authenticated.actor_ref().clone(),
                fixture.authenticated.session_ref().clone(),
                fixture.authenticated.authentication_revision(),
                admitted_at,
                admitted_at + chrono::Duration::minutes(10),
            )
            .unwrap();
            let at = |ms| milliseconds_after(admitted_at, ms);
            let fence = |journal, period| AppResourceMutationFence {
                expected_journal_revision: journal,
                minimum_period_revision: revision(period),
            };
            let root = fixture
                .coordinator
                .admit_workflow_root(
                    &fixture.authenticated,
                    &fixture.resolved,
                    &fixture.binding,
                    enforcement_policy(),
                    AppResourceExecutionLane::Foreground,
                    reference("node:root"),
                    admitted_at,
                )
                .await
                .unwrap()
                .into_dispatch_lease()
                .unwrap();
            let requested = AppResourceQuantity {
                cost_microusd: cost,
                paid_tool_invocations: u64::from(cost != 0),
                ..AppResourceQuantity::default()
            };
            let capabilities = vec![AppCapabilityResourceQuantity {
                capability_family: AppName::parse("internal_data").unwrap(),
                paid_invocations: requested.paid_tool_invocations,
                cost_microusd: cost,
            }];
            let reservation = reference("reservation:failed-source");
            let effect = AppDigest::blake3(b"failed-source-effect");
            let permit = fixture
                .coordinator
                .reserve_operation(
                    &fixture.authenticated,
                    &root,
                    resolved_at(&fixture.resolved, at(1130)),
                    fixture.binding.clone(),
                    enforcement_policy(),
                    fence(1, 2),
                    AppResourceReservationRequest::from_dispatch_owner(
                        reference("node:root"),
                        reservation.clone(),
                        reference("operation:failed-source"),
                        requested,
                        capabilities.clone(),
                        1130,
                        31_130,
                    ),
                    at(1130),
                )
                .await
                .unwrap()
                .into_dispatch_permit()
                .unwrap();
            fixture
                .coordinator
                .start_effect_dispatch(
                    &fixture.authenticated,
                    &root,
                    &permit,
                    resolved_at(&fixture.resolved, at(8075)),
                    fixture.binding.clone(),
                    enforcement_policy(),
                    fence(2, 3),
                    AppResourceEffectDispatchStart::from_effect_kernel(effect.clone(), 8075),
                    at(8075),
                )
                .await
                .unwrap();
            fixture
                .coordinator
                .settle_operation(
                    &fixture.authenticated,
                    &root,
                    permit,
                    resolved_at(&fixture.resolved, at(8371)),
                    fixture.binding.clone(),
                    enforcement_policy(),
                    fence(3, 4),
                    AppResourceSettlementObservation::uncertain_from_tool_runtime(
                        reference("node:root"),
                        reservation.clone(),
                        reference("observation:failed-source"),
                        8371,
                    )
                    .unwrap()
                    .with_app_effect_binding(effect.clone())
                    .unwrap(),
                    at(8371),
                )
                .await
                .unwrap();
            let before = fixture
                .service
                .journal(
                    &fixture.authenticated,
                    &fixture.binding.budget_ledger_ref,
                    at(8371),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(before.events.len(), 4);
            drop(root);

            let mut replacement = fixture.binding.clone();
            replacement.execution_id = reference("execution:after-cleanup");
            replacement.budget_ledger_ref = reference("ledger:after-cleanup");
            let denied = fixture
                .coordinator
                .admit_workflow_root(
                    &fixture.authenticated,
                    &resolved_at(&fixture.resolved, at(61_000)),
                    &replacement,
                    enforcement_policy(),
                    AppResourceExecutionLane::Foreground,
                    reference("node:replacement"),
                    at(61_000),
                )
                .await
                .unwrap_err();
            assert!(matches!(
                denied,
                AppResourceAuthorityError::Contract(
                    AppResourceContractError::ForegroundConcurrencyDenied
                )
            ));
            let mut cleanup = fixture
                .coordinator
                .reacquire_accepted_workflow_root_for_cleanup(
                    &fixture.authenticated,
                    fixture.binding.clone(),
                    accepted_cleanup_snapshot(&fixture),
                    AppResourceAcceptedCleanupProof::from_crash_reconciler(
                        &fixture.binding,
                        4,
                        reference("task-crash:failed-source"),
                        revision(1),
                    )
                    .unwrap(),
                    enforcement_policy(),
                    at(61_000),
                )
                .await
                .unwrap()
                .into_cleanup_lease()
                .unwrap();
            let charged = fixture
                .coordinator
                .conservatively_charge_accepted_post_io_reservation(
                    &fixture.authenticated,
                    &mut cleanup,
                    &reservation,
                    at(61_000),
                )
                .await
                .expect(
                    "a crashed bound effect must retain its identity during conservative cleanup",
                );
            assert_eq!(charged.state.committed, requested);
            assert_eq!(
                charged.state.outstanding_reserved,
                AppResourceQuantity::default()
            );
            let closed = fixture
                .coordinator
                .close_next_accepted_execution_node(
                    &fixture.authenticated,
                    &mut cleanup,
                    at(61_000),
                )
                .await
                .unwrap()
                .unwrap();
            assert!(closed.state.terminally_settled);
            drop(cleanup);
            let after = fixture
                .service
                .journal(
                    &fixture.authenticated,
                    &fixture.binding.budget_ledger_ref,
                    at(61_000),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                &after.events[..before.events.len()],
                before.events.as_slice()
            );
            assert_eq!(after.events.len(), 6);
            match &after.events[4] {
                AppResourceJournalEvent::Settled {
                    outcome: AppResourceSettlementOutcome::Committed,
                    observation_sources,
                    actual,
                    capability_usage,
                    effect_binding_digest: Some(retained),
                    effect_result: None,
                    ..
                } => {
                    assert_eq!(
                        observation_sources,
                        &[AppResourceObservationSource::CrashReconciler]
                    );
                    assert_eq!(*actual, requested);
                    assert_eq!(*capability_usage, capabilities);
                    assert_eq!(*retained, effect);
                },
                _ => {
                    panic!("cleanup must not invent a result or rewrite the uncertain observation")
                },
            }
            let replay = fixture
                .coordinator
                .reacquire_accepted_workflow_root_for_cleanup(
                    &fixture.authenticated,
                    fixture.binding.clone(),
                    accepted_cleanup_snapshot(&fixture),
                    AppResourceAcceptedCleanupProof::from_crash_reconciler(
                        &fixture.binding,
                        6,
                        reference("task-crash:failed-source"),
                        revision(1),
                    )
                    .unwrap(),
                    enforcement_policy(),
                    at(61_001),
                )
                .await
                .unwrap();
            assert!(replay.state.terminally_settled);
            drop(replay);
            fixture
                .coordinator
                .admit_workflow_root(
                    &fixture.authenticated,
                    &resolved_at(&fixture.resolved, at(61_001)),
                    &replacement,
                    enforcement_policy(),
                    AppResourceExecutionLane::Foreground,
                    reference("node:replacement"),
                    at(61_001),
                )
                .await
                .expect("canonical cleanup releases the installation's concurrency slot");
        }
    }

    #[tokio::test]
    async fn accepted_crash_cleanup_charges_full_upper_bound_before_terminal_close() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let requested = AppResourceQuantity {
            cost_microusd: 17,
            paid_tool_invocations: 1,
            payload_bytes: 256,
            ..AppResourceQuantity::default()
        };
        let capability_requests = vec![AppCapabilityResourceQuantity {
            capability_family: AppName::parse("search").unwrap(),
            paid_invocations: 1,
            cost_microusd: 17,
        }];
        let reserved_at = milliseconds_after(admitted_at, 1);
        let reserved = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:cleanup-unknown-post-io"),
                    reference("operation:cleanup-unknown-post-io"),
                    requested,
                    capability_requests.clone(),
                    1,
                    60_000,
                ),
                reserved_at,
            )
            .await
            .unwrap();
        let held_reserved_at = milliseconds_after(admitted_at, 2);
        fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, held_reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:cleanup-held-post-io"),
                    reference("operation:cleanup-held-post-io"),
                    requested,
                    capability_requests.clone(),
                    2,
                    60_000,
                ),
                held_reserved_at,
            )
            .await
            .unwrap();
        let uncertain_at = milliseconds_after(admitted_at, 3);
        fixture
            .coordinator
            .settle_operation(
                &fixture.authenticated,
                &root_lease,
                reserved.into_dispatch_permit().unwrap(),
                resolved_at(&fixture.resolved, uncertain_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 3,
                    minimum_period_revision: revision(4),
                },
                AppResourceSettlementObservation::uncertain_from_tool_runtime(
                    reference("node:root"),
                    reference("reservation:cleanup-unknown-post-io"),
                    reference("observation:cleanup-unknown-post-io"),
                    3,
                )
                .unwrap(),
                uncertain_at,
            )
            .await
            .unwrap();
        drop(root_lease);

        let cleanup_at = milliseconds_after(admitted_at, 4);
        let terminal_receipt = fixture
            .coordinator
            .reacquire_accepted_workflow_root_for_cleanup(
                &fixture.authenticated,
                fixture.binding.clone(),
                accepted_cleanup_snapshot(&fixture),
                AppResourceAcceptedCleanupProof::from_terminal_owner(
                    &fixture.binding,
                    4,
                    reference("task-terminal:cleanup-upper-bound"),
                    revision(1),
                )
                .unwrap(),
                enforcement_policy(),
                cleanup_at,
            )
            .await
            .unwrap();
        let mut terminal_cleanup = terminal_receipt.into_cleanup_lease().unwrap();
        let terminal_owner_error = fixture
            .coordinator
            .conservatively_charge_accepted_post_io_reservation(
                &fixture.authenticated,
                &mut terminal_cleanup,
                &reference("reservation:cleanup-unknown-post-io"),
                milliseconds_after(admitted_at, 5),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            terminal_owner_error,
            AppResourceAuthorityError::InvalidCleanupAuthority(
                "conservative post-I/O charge requires canonical crash-recovery ownership"
            )
        ));
        drop(terminal_cleanup);

        let receipt = fixture
            .coordinator
            .reacquire_accepted_workflow_root_for_cleanup(
                &fixture.authenticated,
                fixture.binding.clone(),
                accepted_cleanup_snapshot(&fixture),
                AppResourceAcceptedCleanupProof::from_crash_reconciler(
                    &fixture.binding,
                    4,
                    reference("task-crash:cleanup-upper-bound"),
                    revision(1),
                )
                .unwrap(),
                enforcement_policy(),
                milliseconds_after(admitted_at, 6),
            )
            .await
            .unwrap();
        let mut cleanup = receipt.into_cleanup_lease().unwrap();
        assert_eq!(cleanup.pending_reservations().len(), 2);
        let uncertain_pending = cleanup
            .pending_reservations()
            .iter()
            .find(|pending| {
                pending.reservation_id == reference("reservation:cleanup-unknown-post-io")
            })
            .unwrap();
        assert_eq!(uncertain_pending.requested, requested);
        assert_eq!(uncertain_pending.capability_requests, capability_requests);
        assert_eq!(
            uncertain_pending.status,
            AppResourcePendingReservationStatus::OutcomeUncertain
        );
        let held_pending = cleanup
            .pending_reservations()
            .iter()
            .find(|pending| pending.reservation_id == reference("reservation:cleanup-held-post-io"))
            .unwrap();
        assert_eq!(
            held_pending.status,
            AppResourcePendingReservationStatus::Held
        );
        let close_error = fixture
            .coordinator
            .close_next_accepted_execution_node(
                &fixture.authenticated,
                &mut cleanup,
                milliseconds_after(admitted_at, 7),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            close_error,
            AppResourceAuthorityError::CleanupReservationsPending { count: 2 }
        ));

        let charged = fixture
            .coordinator
            .conservatively_charge_accepted_post_io_reservation(
                &fixture.authenticated,
                &mut cleanup,
                &reference("reservation:cleanup-unknown-post-io"),
                milliseconds_after(admitted_at, 8),
            )
            .await
            .unwrap();
        assert_eq!(charged.state.committed, requested);
        assert_eq!(charged.state.held_reservations, 1);
        let charged = fixture
            .coordinator
            .conservatively_charge_accepted_post_io_reservation(
                &fixture.authenticated,
                &mut cleanup,
                &reference("reservation:cleanup-held-post-io"),
                milliseconds_after(admitted_at, 9),
            )
            .await
            .unwrap();
        assert_eq!(
            charged.state.committed,
            AppResourceQuantity {
                cost_microusd: 34,
                paid_tool_invocations: 2,
                payload_bytes: 512,
                ..AppResourceQuantity::default()
            }
        );
        assert_eq!(charged.state.active_elapsed_ms, 8);
        assert_eq!(
            charged.state.outstanding_reserved,
            AppResourceQuantity::default()
        );
        assert_eq!(charged.state.uncertain_reservations, 0);
        assert!(!cleanup.has_pending_reservations());

        let closed = fixture
            .coordinator
            .close_next_accepted_execution_node(
                &fixture.authenticated,
                &mut cleanup,
                milliseconds_after(admitted_at, 10),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(closed.state.terminally_settled);
        assert!(!cleanup.has_pending_node_closures());
    }

    #[tokio::test]
    async fn safe_local_no_effect_consumes_dispatch_permit_and_allows_terminal_close() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let reserved_at = milliseconds_after(admitted_at, 1);
        let reserved = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:safe-local-no-effect"),
                    reference("operation:safe-local-no-effect"),
                    AppResourceQuantity {
                        paid_tool_invocations: 1,
                        ..AppResourceQuantity::default()
                    },
                    vec![AppCapabilityResourceQuantity {
                        capability_family: AppName::parse("time_math").unwrap(),
                        paid_invocations: 1,
                        cost_microusd: 0,
                    }],
                    1,
                    60_000,
                ),
                reserved_at,
            )
            .await
            .unwrap();
        let settled_at = milliseconds_after(admitted_at, 2);
        let proof = AppResourceSafeLocalNoEffect::from_dispatch_owner(
            reserved.into_dispatch_permit().unwrap(),
            reference("observation:safe-local-no-effect"),
            1,
            2,
        )
        .unwrap();
        let settled = fixture
            .coordinator
            .settle_safe_local_no_effect_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, settled_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                proof,
                settled_at,
            )
            .await
            .unwrap();
        assert_eq!(settled.state.committed, AppResourceQuantity::default());
        assert_eq!(settled.state.active_elapsed_ms, 1);
        assert_eq!(settled.state.held_reservations, 0);

        let closed_at = milliseconds_after(admitted_at, 3);
        let closed = fixture
            .coordinator
            .close_execution_node(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, closed_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 3,
                    minimum_period_revision: revision(4),
                },
                AppResourceNodeClose::from_execution_owner(reference("node:root"), 3),
                closed_at,
            )
            .await
            .unwrap();
        assert!(closed.state.terminally_settled);
    }

    #[tokio::test]
    async fn accepted_cleanup_requires_exclusive_root_ownership_and_exact_journal_cas() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let admitted = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap();
        let live_lease = admitted.into_dispatch_lease().unwrap();
        let live_error = fixture
            .coordinator
            .reacquire_accepted_workflow_root_for_cleanup(
                &fixture.authenticated,
                fixture.binding.clone(),
                accepted_cleanup_snapshot(&fixture),
                AppResourceAcceptedCleanupProof::from_cancel_owner(
                    &fixture.binding,
                    1,
                    reference("task-cancel:root"),
                    revision(1),
                )
                .unwrap(),
                enforcement_policy(),
                milliseconds_after(admitted_at, 1),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            live_error,
            AppResourceAuthorityError::RootRuntimeAlreadyActive
        ));
        drop(live_lease);

        let stale_error = fixture
            .coordinator
            .reacquire_accepted_workflow_root_for_cleanup(
                &fixture.authenticated,
                fixture.binding.clone(),
                accepted_cleanup_snapshot(&fixture),
                AppResourceAcceptedCleanupProof::from_crash_reconciler(
                    &fixture.binding,
                    2,
                    reference("task-crash:root"),
                    revision(1),
                )
                .unwrap(),
                enforcement_policy(),
                milliseconds_after(admitted_at, 2),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            stale_error,
            AppResourceAuthorityError::InvalidCleanupAuthority(
                "cleanup proof is stale or belongs to another task execution"
            )
        ));
    }

    #[tokio::test]
    async fn accepted_cleanup_derives_one_bounded_leaf_first_close_plan() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        for (expected_revision, node, execution, parent) in [
            (1, "node:child", "execution:child", "node:root"),
            (2, "node:grandchild", "execution:grandchild", "node:child"),
        ] {
            let at = milliseconds_after(admitted_at, expected_revision);
            fixture
                .coordinator
                .open_execution_node(
                    &fixture.authenticated,
                    &root_lease,
                    resolved_at(&fixture.resolved, at),
                    fixture.binding.clone(),
                    enforcement_policy(),
                    AppResourceMutationFence {
                        expected_journal_revision: expected_revision as u64,
                        minimum_period_revision: revision(expected_revision as u64 + 1),
                    },
                    AppResourceNodeAdmission::delegated_child(
                        reference(node),
                        reference(execution),
                        reference(parent),
                        expected_revision as u64,
                    ),
                    at,
                )
                .await
                .unwrap();
        }
        drop(root_lease);
        let receipt = fixture
            .coordinator
            .reacquire_accepted_workflow_root_for_cleanup(
                &fixture.authenticated,
                fixture.binding.clone(),
                accepted_cleanup_snapshot(&fixture),
                AppResourceAcceptedCleanupProof::from_terminal_owner(
                    &fixture.binding,
                    3,
                    reference("task-terminal:tree"),
                    revision(1),
                )
                .unwrap(),
                enforcement_policy(),
                milliseconds_after(admitted_at, 3),
            )
            .await
            .unwrap();
        let mut cleanup = receipt.into_cleanup_lease().unwrap();
        for (offset, expected_node) in [(4, "node:grandchild"), (5, "node:child"), (6, "node:root")]
        {
            fixture
                .coordinator
                .close_next_accepted_execution_node(
                    &fixture.authenticated,
                    &mut cleanup,
                    milliseconds_after(admitted_at, offset),
                )
                .await
                .unwrap()
                .unwrap();
            let journal = fixture
                .service
                .journal(
                    &fixture.authenticated,
                    &fixture.binding.budget_ledger_ref,
                    milliseconds_after(admitted_at, offset),
                )
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                journal.events.last(),
                Some(AppResourceJournalEvent::NodeClosed { node_id, .. })
                    if node_id == &reference(expected_node)
            ));
        }
    }

    #[tokio::test]
    async fn accepted_child_cleanup_closes_only_the_exact_execution_subtree() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        for (sequence, node, execution) in [
            (1_u64, "node:child", "execution:child"),
            (2_u64, "node:sibling", "execution:sibling"),
        ] {
            let at = milliseconds_after(admitted_at, i64::try_from(sequence).unwrap());
            fixture
                .coordinator
                .open_execution_node(
                    &fixture.authenticated,
                    &root_lease,
                    resolved_at(&fixture.resolved, at),
                    fixture.binding.clone(),
                    enforcement_policy(),
                    AppResourceMutationFence {
                        expected_journal_revision: sequence,
                        minimum_period_revision: revision(sequence + 1),
                    },
                    AppResourceNodeAdmission::delegated_child(
                        reference(node),
                        reference(execution),
                        reference("node:root"),
                        sequence,
                    ),
                    at,
                )
                .await
                .unwrap();
        }
        drop(root_lease);

        let receipt = fixture
            .coordinator
            .reacquire_accepted_workflow_root_for_cleanup(
                &fixture.authenticated,
                fixture.binding.clone(),
                accepted_cleanup_snapshot(&fixture),
                AppResourceAcceptedCleanupProof::from_crashed_execution_reconciler(
                    &fixture.binding,
                    3,
                    reference("task-crash:child"),
                    revision(1),
                    reference("node:child"),
                )
                .unwrap(),
                enforcement_policy(),
                milliseconds_after(admitted_at, 3),
            )
            .await
            .unwrap();
        let mut cleanup = receipt.into_cleanup_lease().unwrap();
        let closed = fixture
            .coordinator
            .close_next_accepted_execution_node(
                &fixture.authenticated,
                &mut cleanup,
                milliseconds_after(admitted_at, 4),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!closed.state.terminally_settled);
        assert_eq!(closed.state.open_nodes, 2);
        assert!(!cleanup.has_pending_node_closures());

        let journal = fixture
            .service
            .journal(
                &fixture.authenticated,
                &fixture.binding.budget_ledger_ref,
                milliseconds_after(admitted_at, 4),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            journal.events.last(),
            Some(AppResourceJournalEvent::NodeClosed { node_id, .. })
                if node_id == &reference("node:child")
        ));
    }

    #[tokio::test]
    async fn accepted_cleanup_rejects_tampered_historical_package_identity() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let admitted = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap();
        drop(admitted.into_dispatch_lease().unwrap());
        let package_revision_ref = fixture.resolved.package_revision_ref.clone();
        let tampered_digest = AppDigest::blake3(b"tampered package");
        fixture
            .service
            .registry
            .execute_scoped_write(
                &fixture.authenticated,
                &milliseconds_after(admitted_at, 1),
                move |connection, _| {
                    connection.execute(
                        "UPDATE app_package_revisions SET content_digest = ?1
                          WHERE package_revision_ref = ?2",
                        params![tampered_digest.as_str(), package_revision_ref.as_str()],
                    )?;
                    Ok(())
                },
            )
            .await
            .unwrap();
        let error = fixture
            .coordinator
            .reacquire_accepted_workflow_root_for_cleanup(
                &fixture.authenticated,
                fixture.binding.clone(),
                accepted_cleanup_snapshot(&fixture),
                AppResourceAcceptedCleanupProof::from_terminal_owner(
                    &fixture.binding,
                    1,
                    reference("task-terminal:tampered-package"),
                    revision(1),
                )
                .unwrap(),
                enforcement_policy(),
                milliseconds_after(admitted_at, 2),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppResourceAuthorityError::StaleRegistryAuthority("accepted package identity")
        ));
    }

    #[test]
    fn cloned_runtime_coordinators_share_capacity_and_preserve_foreground_reserve() {
        let temporary = canonical_tempdir();
        let coordinator =
            AppResourceRuntimeCoordinator::new(ArtifactV2Workspace::new(temporary.path()), 2, 1)
                .unwrap();
        let clone = coordinator.clone();
        let background = coordinator
            .acquire_scheduler_guard(
                AppResourceExecutionLane::Background,
                "root:background-one".to_owned(),
            )
            .unwrap();
        assert!(matches!(
            clone.acquire_scheduler_guard(
                AppResourceExecutionLane::Background,
                "root:background-two".to_owned(),
            ),
            Err(AppResourceAuthorityError::SchedulerCapacityUnavailable)
        ));
        let foreground = clone
            .acquire_scheduler_guard(
                AppResourceExecutionLane::Foreground,
                "root:foreground-one".to_owned(),
            )
            .unwrap();
        assert!(matches!(
            coordinator.acquire_scheduler_guard(
                AppResourceExecutionLane::Foreground,
                "root:foreground-two".to_owned(),
            ),
            Err(AppResourceAuthorityError::SchedulerCapacityUnavailable)
        ));
        drop(background);
        drop(foreground);
        assert!(clone
            .acquire_scheduler_guard(
                AppResourceExecutionLane::Background,
                "root:background-three".to_owned(),
            )
            .is_ok());
    }

    #[test]
    fn runtime_scheduler_never_mints_two_live_leases_for_one_root() {
        let temporary = canonical_tempdir();
        let coordinator =
            AppResourceRuntimeCoordinator::new(ArtifactV2Workspace::new(temporary.path()), 2, 1)
                .unwrap();
        let first = coordinator
            .acquire_scheduler_guard(AppResourceExecutionLane::Foreground, "root:same".to_owned())
            .unwrap();
        assert!(matches!(
            coordinator.acquire_scheduler_guard(
                AppResourceExecutionLane::Foreground,
                "root:same".to_owned(),
            ),
            Err(AppResourceAuthorityError::RootRuntimeAlreadyActive)
        ));
        drop(first);
        assert!(coordinator
            .acquire_scheduler_guard(AppResourceExecutionLane::Foreground, "root:same".to_owned(),)
            .is_ok());
    }

    #[test]
    fn resource_period_end_is_the_first_of_the_next_utc_month() {
        // The period a run's ceilings are counted in; a behavior parked on a
        // monthly ceiling waits for exactly this instant.
        let september = Utc
            .with_ymd_and_hms(2026, 9, 17, 19, 21, 44)
            .single()
            .unwrap();
        assert_eq!(
            resource_period_end(september).unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).single().unwrap()
        );
        let december = Utc
            .with_ymd_and_hms(2026, 12, 31, 23, 59, 59)
            .single()
            .unwrap();
        assert_eq!(
            resource_period_end(december).unwrap(),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).single().unwrap()
        );
    }

    #[test]
    fn utc_month_period_is_server_derived_and_crosses_year_without_reset_ambiguity() {
        let august = canonical_monthly_period(time(4)).unwrap();
        assert_eq!(august.0, reference("period:2026-08"));
        assert!(august.1 > 0);

        let december = Utc
            .with_ymd_and_hms(2026, 12, 31, 23, 59, 59)
            .single()
            .unwrap();
        let december_period = canonical_monthly_period(december).unwrap();
        assert_eq!(december_period.0, reference("period:2026-12"));
        assert_eq!(december_period.1, 1_000);

        let final_half_millisecond = Utc
            .with_ymd_and_hms(2026, 12, 31, 23, 59, 59)
            .single()
            .unwrap()
            + chrono::Duration::microseconds(999_500);
        assert_eq!(
            canonical_monthly_period(final_half_millisecond).unwrap().1,
            1
        );
    }

    #[tokio::test]
    async fn resumed_root_keeps_its_original_period_across_a_calendar_rollover() {
        let fixture = fixture(2).await;
        let receipt = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        let original_deadline = receipt
            .dispatch_lease
            .as_ref()
            .unwrap()
            .period_ends_at_elapsed_ms;
        drop(receipt.into_dispatch_lease().unwrap());

        let september_first = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 1).single().unwrap();
        let september_second = Utc.with_ymd_and_hms(2026, 9, 2, 0, 0, 0).single().unwrap();
        let extended_authentication = AuthenticatedAppScope::from_verified_session(
            fixture.authenticated.scope().clone(),
            fixture.authenticated.scope_binding_ref().clone(),
            fixture.authenticated.actor_ref().clone(),
            fixture.authenticated.session_ref().clone(),
            fixture.authenticated.authentication_revision(),
            time(0),
            september_second,
        )
        .unwrap();
        let period = fixture
            .coordinator
            .root_admission_period(&extended_authentication, &fixture.binding, september_first)
            .await
            .unwrap();
        assert_eq!(period.0, reference("period:2026-08"));
        assert_eq!(period.1, original_deadline);
    }

    #[tokio::test]
    async fn resumed_root_keeps_its_durable_elapsed_origin_and_rejects_a_reset_clock() {
        let fixture = fixture(2).await;
        let initial = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        drop(initial.into_dispatch_lease().unwrap());

        let resumed_at = time(5);
        let resumed_authority = resolved_at(&fixture.resolved, resumed_at);
        let resumed = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &resumed_authority,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                resumed_at,
            )
            .await
            .unwrap();
        assert_eq!(
            resumed.outcome,
            AppResourceRootAdmissionOutcome::AlreadyPresent
        );
        let resumed_lease = resumed.into_dispatch_lease().unwrap();
        assert_eq!(resumed_lease.elapsed_at(resumed_at).unwrap(), 1_000);

        let error = fixture
            .coordinator
            .open_execution_node(
                &fixture.authenticated,
                &resumed_lease,
                resumed_authority,
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceNodeAdmission::resume(
                    reference("node:resume"),
                    reference("execution:resume"),
                    reference("node:root"),
                    0,
                ),
                resumed_at,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppResourceAuthorityError::InvalidObservation(
                "resource observation elapsed time is not anchored to the durable root"
            )
        ));
    }

    #[tokio::test]
    async fn root_admission_is_atomic_replayable_and_leaves_usage_projection_empty() {
        let fixture = fixture(2).await;
        let receipt = fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(receipt.outcome, AppResourceRootAdmissionOutcome::Created);
        assert_eq!(receipt.admitted_period_revision, revision(1));
        assert_eq!(receipt.current_period_revision, revision(2));

        let replay = fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(
            replay.outcome,
            AppResourceRootAdmissionOutcome::AlreadyPresent
        );
        let journal = fixture
            .service
            .journal(
                &fixture.authenticated,
                &fixture.binding.budget_ledger_ref,
                time(4),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(journal.events.len(), 1);

        let projection_rows = fixture
            .service
            .registry
            .execute_scoped_read(&fixture.authenticated, &time(4), |connection, _scope| {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM app_resource_usage_projection",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(AppRegistryError::from)
            })
            .await
            .unwrap();
        assert_eq!(projection_rows, Some(0));
        let period_state = fixture
            .service
            .registry
            .execute_scoped_read(&fixture.authenticated, &time(4), |connection, _scope| {
                connection
                    .query_row(
                        "SELECT revision, foreground_runs, background_runs,
                                background_starts, committed_tokens, outstanding_tokens
                           FROM app_resource_periods",
                        [],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, i64>(5)?,
                            ))
                        },
                    )
                    .map_err(AppRegistryError::from)
            })
            .await
            .unwrap();
        assert_eq!(period_state, Some((2, 1, 0, 0, 0, 0)));

        let mutation_error = fixture
            .service
            .registry
            .execute_scoped_write(&fixture.authenticated, &time(4), |connection, _scope| {
                connection.execute(
                    "UPDATE app_resource_tree_events SET event_json = X'7B7D'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(matches!(mutation_error, AppRegistryError::Sqlite(_)));

        let unadvanced_event_error = fixture
            .service
            .registry
            .execute_scoped_write(&fixture.authenticated, &time(4), |connection, _scope| {
                let event_json = canonical_json_bytes(&serde_json::to_value(
                    AppResourceJournalEvent::ProgressObserved {
                        sequence: 2,
                        node_id: reference("node:root"),
                        progress_id: reference("progress:one"),
                        at_elapsed_ms: 1,
                    },
                )?)?;
                let event_digest = AppDigest::blake3(&event_json);
                connection.execute(
                    "INSERT INTO app_resource_tree_events (
                         budget_ledger_ref, sequence, event_digest, event_json, created_at
                     ) VALUES (?1, 2, ?2, ?3, ?4)",
                    params![
                        "ledger:root",
                        event_digest.as_str(),
                        event_json,
                        format_timestamp(&time(4)),
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(matches!(
            unadvanced_event_error,
            AppRegistryError::Sqlite(_)
        ));
    }

    #[tokio::test]
    async fn journal_read_rejects_oversized_stored_event_before_full_blob_materialization() {
        let fixture = fixture(2).await;
        fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        fixture
            .service
            .registry
            .execute_scoped_write(&fixture.authenticated, &time(4), |connection, _scope| {
                connection.execute_batch("DROP TRIGGER app_resource_tree_event_update_guard")?;
                connection.execute(
                    "UPDATE app_resource_tree_events SET event_json = zeroblob(?1)",
                    params![i64::try_from(MAX_STORED_RESOURCE_RECORD_BYTES + 1).unwrap()],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let error = fixture
            .service
            .journal(
                &fixture.authenticated,
                &fixture.binding.budget_ledger_ref,
                time(4),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppResourceAuthorityError::CorruptAuthority(message)
                if message.contains("event stored length")
        ));
    }

    #[tokio::test]
    async fn open_root_consumes_installation_concurrency_without_a_projection_ledger() {
        let fixture = fixture(1).await;
        fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        let mut second_binding = fixture.binding.clone();
        second_binding.execution_id = reference("execution:second");
        second_binding.budget_ledger_ref = reference("ledger:second");
        let error = fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &second_binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:second"),
                time(4),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppResourceAuthorityError::Contract(
                AppResourceContractError::ForegroundConcurrencyDenied
            )
        ));
    }

    #[tokio::test]
    async fn accepted_root_replay_does_not_reapply_current_scheduler_admission() {
        let fixture = fixture(1).await;
        fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();

        let replay = fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot_with_scheduler(&fixture, 8, 0),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(
            replay.outcome,
            AppResourceRootAdmissionOutcome::AlreadyPresent
        );
        assert_eq!(replay.current_period_revision, revision(2));
    }

    #[tokio::test]
    async fn stale_durable_grant_fails_before_creating_a_resource_tree() {
        let fixture = fixture(2).await;
        let installation_id = fixture.resolved.installation_id.clone();
        fixture
            .service
            .registry
            .execute_scoped_write(
                &fixture.authenticated,
                &time(3),
                move |connection, _scope| {
                    connection.execute(
                        "UPDATE app_grant_revisions SET revoked_at = ?1
                          WHERE installation_id = ?2 AND revision = 1",
                        params![format_timestamp(&time(3)), installation_id.as_str()],
                    )?;
                    Ok(())
                },
            )
            .await
            .unwrap();
        let error = fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppResourceAuthorityError::StaleRegistryAuthority("grant revision")
        ));
        let journal = fixture
            .service
            .journal(
                &fixture.authenticated,
                &fixture.binding.budget_ledger_ref,
                time(4),
            )
            .await
            .unwrap();
        assert!(journal.is_none());
    }

    fn current_snapshot(fixture: &Fixture) -> AppResourceCurrentSnapshot {
        AppResourceCurrentSnapshot::from_authoritative_owners(
            reference("period:2026-08"),
            100_000,
            AppResourcePackageMeasurement::from_package_store(
                fixture.resolved.package_revision_ref.clone(),
                100,
            ),
            0,
            0,
            time(4),
        )
        .unwrap()
    }

    fn mutation_authority(
        fixture: &Fixture,
        journal_revision: u64,
        period_revision: u64,
    ) -> AppResourceMutationAuthority {
        AppResourceMutationAuthority::from_current_owners(
            fixture.resolved.clone(),
            fixture.binding.clone(),
            enforcement_policy(),
            current_snapshot(fixture),
            AppResourceMutationFence {
                expected_journal_revision: journal_revision,
                minimum_period_revision: revision(period_revision),
            },
        )
    }

    fn committed_mutation_receipt(
        fixture: &Fixture,
        origin: AppMutationOrigin,
        mutation_key: AppDigest,
        batch_digest: AppDigest,
        committed_at: DateTime<Utc>,
    ) -> AppMutationReceipt {
        let mutation_hex = mutation_key.as_str().strip_prefix("blake3:").unwrap();
        AppMutationReceipt {
            receipt_id: reference(&format!("app-receipt:{mutation_hex}")),
            installation_id: fixture.binding.installation_id.clone(),
            origin,
            mutation_key,
            batch_digest,
            committed_record_revisions: vec![AppCommittedRecordRevision {
                entity: AppName::parse("note").unwrap(),
                record_id: AppRecordId::parse("rec_committed_recovery").unwrap(),
                revision: revision(1),
            }],
            change_seq_range: AppChangeSequenceRange { first: 1, last: 1 },
            committed_at,
        }
    }

    fn empty_mutation_command(fixture: &Fixture, suffix: &str) -> AppMutationCommand {
        AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: reference(&format!("mutation:{suffix}")),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: fixture.binding.schema_revision,
            operations: Vec::new(),
            expected_record_revisions: Vec::new(),
        }
    }

    #[test]
    fn node_adapters_cover_every_non_root_execution_kind_without_string_inference() {
        let constructors = [
            AppResourceNodeAdmission::delegated_child(
                reference("node:child"),
                reference("execution:child"),
                reference("node:root"),
                1,
            ),
            AppResourceNodeAdmission::retry(
                reference("node:retry"),
                reference("execution:retry"),
                reference("node:root"),
                1,
            ),
            AppResourceNodeAdmission::resume(
                reference("node:resume"),
                reference("execution:resume"),
                reference("node:root"),
                1,
            ),
            AppResourceNodeAdmission::repair(
                reference("node:repair"),
                reference("execution:repair"),
                reference("node:root"),
                1,
            ),
            AppResourceNodeAdmission::tool_call(
                reference("node:tool"),
                reference("execution:tool"),
                reference("node:root"),
                1,
            ),
            AppResourceNodeAdmission::browser_or_network(
                reference("node:browser"),
                reference("execution:browser"),
                reference("node:root"),
                1,
            ),
            AppResourceNodeAdmission::synthesis(
                reference("node:synthesis"),
                reference("execution:synthesis"),
                reference("node:root"),
                1,
            ),
            AppResourceNodeAdmission::reflection(
                reference("node:reflection"),
                reference("execution:reflection"),
                reference("node:root"),
                1,
            ),
        ];
        let kinds: Vec<_> = constructors
            .into_iter()
            .map(|value| value.node_kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                AppResourceNodeKind::DelegatedChild,
                AppResourceNodeKind::Retry,
                AppResourceNodeKind::Resume,
                AppResourceNodeKind::Repair,
                AppResourceNodeKind::ToolCall,
                AppResourceNodeKind::BrowserOrNetwork,
                AppResourceNodeKind::Synthesis,
                AppResourceNodeKind::Reflection,
            ]
        );
    }

    #[test]
    fn uncertain_observation_adapters_name_each_authoritative_owner_exactly() {
        type UncertainAdapter =
            fn(
                AppReference,
                AppReference,
                AppReference,
                u64,
            ) -> Result<AppResourceSettlementObservation, AppResourceAuthorityError>;
        let make = |suffix: &str| {
            (
                reference("node:root"),
                reference(&format!("reservation:{suffix}")),
                reference(&format!("observation:{suffix}")),
            )
        };
        let (node, reservation, observation) = make("llm");
        let llm = AppResourceSettlementObservation::uncertain_from_llm_task_ledger(
            node,
            reservation,
            observation,
            1,
        )
        .unwrap();
        assert_eq!(
            llm.observation_sources,
            vec![
                AppResourceObservationSource::ExecutionTokenMeter,
                AppResourceObservationSource::LlmTaskLedger,
            ]
        );

        let constructors: [(&str, AppResourceObservationSource, UncertainAdapter); 6] = [
            (
                "coding",
                AppResourceObservationSource::CodingTaskActiveTime,
                AppResourceSettlementObservation::uncertain_from_coding_task,
            ),
            (
                "tool",
                AppResourceObservationSource::ToolRuntime,
                AppResourceSettlementObservation::uncertain_from_tool_runtime,
            ),
            (
                "browser",
                AppResourceObservationSource::BrowserRuntime,
                AppResourceSettlementObservation::uncertain_from_browser_runtime,
            ),
            (
                "app-store",
                AppResourceObservationSource::AppStoreTransaction,
                AppResourceSettlementObservation::uncertain_from_app_store,
            ),
            (
                "attachment",
                AppResourceObservationSource::AttachmentStore,
                AppResourceSettlementObservation::uncertain_from_attachment_store,
            ),
            (
                "package",
                AppResourceObservationSource::PackageStore,
                AppResourceSettlementObservation::uncertain_from_package_store,
            ),
        ];
        for (suffix, source, constructor) in constructors {
            let (node, reservation, observation) = make(suffix);
            let uncertain = constructor(node, reservation, observation, 1).unwrap();
            assert_eq!(
                uncertain.outcome,
                AppResourceSettlementOutcome::OutcomeUncertain
            );
            assert_eq!(uncertain.observation_sources, vec![source]);
        }
    }

    #[tokio::test]
    async fn reservation_receipt_is_the_dispatch_permit_and_exact_settlement_charges_once() {
        let fixture = fixture(2).await;
        let root = fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        let root_lease = root.into_dispatch_lease().unwrap();
        let reservation = AppResourceReservationRequest::from_dispatch_owner(
            reference("node:root"),
            reference("reservation:llm"),
            reference("operation:llm"),
            AppResourceQuantity {
                input_tokens: 20,
                cached_input_tokens: 20,
                output_tokens: 10,
                cost_microusd: 20,
                ..AppResourceQuantity::default()
            },
            Vec::new(),
            1,
            50_000,
        );
        let reserved = fixture
            .service
            .reserve(
                &fixture.authenticated,
                &root_lease,
                mutation_authority(&fixture, 1, 2),
                reservation.clone(),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(reserved.mutation.current_period_revision, revision(3));
        let permit = reserved.into_dispatch_permit().unwrap();
        let replayed_before_dispatch = fixture
            .service
            .reserve(
                &fixture.authenticated,
                &root_lease,
                mutation_authority(&fixture, 1, 2),
                reservation.clone(),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(
            replayed_before_dispatch.mutation.outcome,
            AppResourceAppendOutcome::AlreadyPresent
        );
        assert!(matches!(
            replayed_before_dispatch.into_dispatch_permit(),
            Err(AppResourceAuthorityError::DispatchPermitUnavailable)
        ));
        let observation = AppResourceSettlementObservation::from_llm_task_ledger(
            reference("node:root"),
            reference("reservation:llm"),
            reference("observation:llm"),
            AppResourceQuantity {
                input_tokens: 12,
                cached_input_tokens: 5,
                output_tokens: 7,
                cost_microusd: 11,
                ..AppResourceQuantity::default()
            },
            vec![AppActiveInterval {
                start_elapsed_ms: 1,
                end_elapsed_ms: 3,
            }],
            3,
        );
        let settled = fixture
            .service
            .settle(
                &fixture.authenticated,
                &root_lease,
                permit,
                mutation_authority(&fixture, 2, 3),
                observation,
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(settled.state.committed.input_tokens, 12);
        assert_eq!(settled.state.committed.cached_input_tokens, 5);
        assert_eq!(settled.state.active_elapsed_ms, 2);

        let replayed_reservation = fixture
            .service
            .reserve(
                &fixture.authenticated,
                &root_lease,
                mutation_authority(&fixture, 1, 2),
                reservation,
                time(4),
            )
            .await
            .unwrap();
        assert!(matches!(
            replayed_reservation.into_dispatch_permit(),
            Err(AppResourceAuthorityError::DispatchPermitUnavailable)
        ));
    }

    #[tokio::test]
    async fn uncertain_effect_stays_held_until_durable_trusted_reconciliation() {
        let fixture = fixture(2).await;
        let root_lease = fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let reserved = fixture
            .service
            .reserve(
                &fixture.authenticated,
                &root_lease,
                mutation_authority(&fixture, 1, 2),
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:tool"),
                    reference("operation:tool"),
                    AppResourceQuantity {
                        paid_tool_invocations: 1,
                        cost_microusd: 9,
                        ..AppResourceQuantity::default()
                    },
                    vec![AppCapabilityResourceQuantity {
                        capability_family: AppName::parse("search").unwrap(),
                        paid_invocations: 1,
                        cost_microusd: 9,
                    }],
                    1,
                    50_000,
                ),
                time(4),
            )
            .await
            .unwrap();
        let permit = reserved.into_dispatch_permit().unwrap();
        let uncertain = AppResourceSettlementObservation::uncertain_from_tool_runtime(
            reference("node:root"),
            reference("reservation:tool"),
            reference("observation:uncertain"),
            2,
        )
        .unwrap();
        let held = fixture
            .service
            .settle(
                &fixture.authenticated,
                &root_lease,
                permit,
                mutation_authority(&fixture, 2, 3),
                uncertain,
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(held.state.uncertain_reservations, 1);
        assert_eq!(held.state.outstanding_reserved.cost_microusd, 9);

        let recovered = fixture
            .service
            .reconcile_proven_unspent(
                &fixture.authenticated,
                mutation_authority(&fixture, 3, 4),
                AppResourceCrashReconciliation::from_crash_reconciler(
                    reference("node:root"),
                    reference("reservation:tool"),
                    reference("observation:recovered"),
                    reference("reconciliation:tool"),
                    revision(1),
                    3,
                ),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(recovered.state.uncertain_reservations, 0);
        assert_eq!(recovered.state.outstanding_reserved.cost_microusd, 0);
        let rebound = fixture
            .service
            .reconcile_proven_unspent(
                &fixture.authenticated,
                mutation_authority(&fixture, 3, 4),
                AppResourceCrashReconciliation::from_crash_reconciler(
                    reference("node:root"),
                    reference("reservation:tool"),
                    reference("observation:recovered"),
                    reference("reconciliation:rebound"),
                    revision(1),
                    3,
                ),
                time(4),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            rebound,
            AppResourceAuthorityError::IdentityConflict {
                entity: "crash reconciliation",
                ..
            }
        ));

        let replay = fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(replay.state.outstanding_reserved.cost_microusd, 0);
    }

    #[test]
    fn recovery_evidence_digest_binds_the_canonical_ledger_identity() {
        let evidence = TrustedAppResourceRecoveryEvidence::from_crash_reconciler(
            reference("reservation:tool"),
            reference("observation:recovered"),
            reference("reconciliation:tool"),
            revision(1),
            3,
        );
        assert_ne!(
            recovery_evidence_digest(&reference("ledger:one"), &evidence).unwrap(),
            recovery_evidence_digest(&reference("ledger:two"), &evidence).unwrap()
        );
    }

    #[tokio::test]
    async fn projection_rollover_and_retirement_preserve_authority_and_replay_denial() {
        let fixture = fixture(2).await;
        let root_lease = fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let baseline_tamper = fixture
            .service
            .registry
            .execute_scoped_write(&fixture.authenticated, &time(4), |connection, _| {
                connection.execute(
                    "UPDATE app_resource_trees SET package_bytes = package_bytes + 1
                      WHERE budget_ledger_ref = ?1",
                    params!["ledger:root"],
                )?;
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(matches!(baseline_tamper, AppRegistryError::Sqlite(_)));
        let terminal = fixture
            .service
            .close_node(
                &fixture.authenticated,
                &root_lease,
                mutation_authority(&fixture, 1, 2),
                AppResourceNodeClose::from_execution_owner(reference("node:root"), 1),
                time(4),
            )
            .await
            .unwrap();
        assert!(terminal.state.terminally_settled);
        let projection = fixture
            .service
            .rebuild_usage_projection(
                &fixture.authenticated,
                &fixture.resolved,
                &reference("period:2026-08"),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(projection.foreground_runs, 0);
        assert_eq!(projection.authority_revision, revision(3));
        let rollover = fixture
            .service
            .rollover_period(
                &fixture.authenticated,
                &fixture.resolved,
                &reference("period:2026-08"),
                revision(3),
                time(10),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(rollover.current_revision, revision(4));
        let rollover_replay = fixture
            .service
            .rollover_period(
                &fixture.authenticated,
                &fixture.resolved,
                &reference("period:2026-08"),
                revision(3),
                time(10),
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(
            rollover_replay.outcome,
            AppResourceMaintenanceOutcome::AlreadyPresent
        );
        let retired = fixture
            .service
            .retire_terminal_trees(
                &fixture.authenticated,
                &fixture.resolved,
                &reference("period:2026-08"),
                16,
                time(11),
            )
            .await
            .unwrap();
        assert_eq!(retired.retired_trees, 1);
        assert!(!retired.has_more);
        assert!(!fixture
            .coordinator
            .root_was_never_admitted(&fixture.authenticated, &fixture.binding, time(11))
            .await
            .unwrap());
        let retained_rows = fixture
            .service
            .registry
            .execute_scoped_read(&fixture.authenticated, &time(11), |connection, _| {
                connection
                    .query_row(
                        "SELECT
                            (SELECT COUNT(*) FROM app_resource_trees),
                            (SELECT COUNT(*) FROM app_resource_retired_trees)",
                        [],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .map_err(AppRegistryError::from)
            })
            .await
            .unwrap();
        assert_eq!(retained_rows, Some((0, 1)));
        let tombstone_delete = fixture
            .service
            .registry
            .execute_scoped_write(&fixture.authenticated, &time(11), |connection, _| {
                connection.execute("DELETE FROM app_resource_retired_trees", [])?;
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(matches!(tombstone_delete, AppRegistryError::Sqlite(_)));
        assert!(matches!(
            fixture
                .service
                .journal(
                    &fixture.authenticated,
                    &fixture.binding.budget_ledger_ref,
                    time(11),
                )
                .await,
            Err(AppResourceAuthorityError::RetiredTree)
        ));
    }

    #[tokio::test]
    async fn closed_period_denies_new_nodes_and_reservations_but_keeps_settlement_open() {
        let fixture = fixture(2).await;
        let root_lease = fixture
            .service
            .admit_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                admission_snapshot(&fixture),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                time(4),
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        fixture
            .service
            .rollover_period(
                &fixture.authenticated,
                &fixture.resolved,
                &reference("period:2026-08"),
                revision(2),
                time(10),
                time(4),
            )
            .await
            .unwrap();
        let node_error = fixture
            .service
            .open_node(
                &fixture.authenticated,
                &root_lease,
                mutation_authority(&fixture, 1, 2),
                AppResourceNodeAdmission::retry(
                    reference("node:late-retry"),
                    reference("execution:late-retry"),
                    reference("node:root"),
                    1,
                ),
                time(4),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            node_error,
            AppResourceAuthorityError::PeriodClosed
        ));
        let error = fixture
            .service
            .reserve(
                &fixture.authenticated,
                &root_lease,
                mutation_authority(&fixture, 1, 2),
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:late"),
                    reference("operation:late"),
                    AppResourceQuantity::default(),
                    Vec::new(),
                    1,
                    50_000,
                ),
                time(4),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AppResourceAuthorityError::PeriodClosed));
        let closed = fixture
            .service
            .close_node(
                &fixture.authenticated,
                &root_lease,
                mutation_authority(&fixture, 1, 3),
                AppResourceNodeClose::from_execution_owner(reference("node:root"), 2),
                time(4),
            )
            .await
            .unwrap();
        assert!(closed.state.terminally_settled);
    }

    #[tokio::test]
    async fn accepted_permit_settles_and_root_closes_after_grant_revocation() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let reserved_at = milliseconds_after(admitted_at, 1);
        let permit = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:accepted"),
                    reference("operation:accepted"),
                    AppResourceQuantity {
                        paid_tool_invocations: 1,
                        cost_microusd: 9,
                        ..AppResourceQuantity::default()
                    },
                    vec![AppCapabilityResourceQuantity {
                        capability_family: AppName::parse("search").unwrap(),
                        paid_invocations: 1,
                        cost_microusd: 9,
                    }],
                    1,
                    50_000,
                ),
                reserved_at,
            )
            .await
            .unwrap()
            .into_dispatch_permit()
            .unwrap();
        let revoked_at = milliseconds_after(admitted_at, 2);
        revoke_fixture_grant(&fixture, revoked_at).await;

        let denied = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, revoked_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:denied-after-revoke"),
                    reference("operation:denied-after-revoke"),
                    AppResourceQuantity::default(),
                    Vec::new(),
                    2,
                    50_000,
                ),
                revoked_at,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            denied,
            AppResourceAuthorityError::StaleRegistryAuthority("grant revision")
        ));

        let settled_at = milliseconds_after(admitted_at, 3);
        let settled = fixture
            .coordinator
            .settle_operation(
                &fixture.authenticated,
                &root_lease,
                permit,
                resolved_at(&fixture.resolved, settled_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                AppResourceSettlementObservation::from_tool_runtime(
                    reference("node:root"),
                    reference("reservation:accepted"),
                    reference("observation:accepted"),
                    AppResourceQuantity {
                        paid_tool_invocations: 1,
                        cost_microusd: 7,
                        ..AppResourceQuantity::default()
                    },
                    vec![AppCapabilityResourceQuantity {
                        capability_family: AppName::parse("search").unwrap(),
                        paid_invocations: 1,
                        cost_microusd: 7,
                    }],
                    Vec::new(),
                    3,
                ),
                settled_at,
            )
            .await
            .unwrap();
        assert_eq!(settled.state.committed.cost_microusd, 7);

        let closed_at = milliseconds_after(admitted_at, 4);
        let closed = fixture
            .coordinator
            .close_execution_node(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, closed_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 3,
                    minimum_period_revision: revision(4),
                },
                AppResourceNodeClose::from_execution_owner(reference("node:root"), 4),
                closed_at,
            )
            .await
            .unwrap();
        assert!(closed.state.terminally_settled);
    }

    #[tokio::test]
    async fn dispatch_permit_expires_at_io_boundary_but_remains_settleable() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let extended_authentication = AuthenticatedAppScope::from_verified_session(
            fixture.authenticated.scope().clone(),
            fixture.authenticated.scope_binding_ref().clone(),
            fixture.authenticated.actor_ref().clone(),
            fixture.authenticated.session_ref().clone(),
            fixture.authenticated.authentication_revision(),
            time(0),
            admitted_at + chrono::Duration::minutes(2),
        )
        .unwrap();
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &extended_authentication,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let reserved_at = milliseconds_after(admitted_at, 1);
        let permit = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:expiry-fence"),
                    reference("operation:expiry-fence"),
                    AppResourceQuantity {
                        paid_tool_invocations: 1,
                        ..AppResourceQuantity::default()
                    },
                    vec![AppCapabilityResourceQuantity {
                        capability_family: AppName::parse("expiry-test").unwrap(),
                        paid_invocations: 1,
                        cost_microusd: 0,
                    }],
                    1,
                    50_000,
                ),
                reserved_at,
            )
            .await
            .unwrap()
            .into_dispatch_permit()
            .unwrap();
        assert!(permit.ensure_live_for_io(49_999).is_ok());
        assert!(matches!(
            permit.ensure_live_for_io(50_000),
            Err(AppResourceAuthorityError::DispatchPermitExpired {
                expires_at_elapsed_ms: 50_000,
                observed_at_elapsed_ms: 50_000,
            })
        ));

        let settled_at = milliseconds_after(admitted_at, 50_001);
        let settled = fixture
            .coordinator
            .settle_operation(
                &extended_authentication,
                &root_lease,
                permit,
                resolved_at(&fixture.resolved, settled_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                AppResourceSettlementObservation::from_tool_runtime(
                    reference("node:root"),
                    reference("reservation:expiry-fence"),
                    reference("observation:expiry-fence"),
                    AppResourceQuantity {
                        paid_tool_invocations: 1,
                        ..AppResourceQuantity::default()
                    },
                    vec![AppCapabilityResourceQuantity {
                        capability_family: AppName::parse("expiry-test").unwrap(),
                        paid_invocations: 1,
                        cost_microusd: 0,
                    }],
                    Vec::new(),
                    50_001,
                ),
                settled_at,
            )
            .await
            .unwrap();
        assert_eq!(settled.state.held_reservations, 0);
    }

    #[tokio::test]
    async fn pre_io_release_consumes_exact_permit_and_persists_proven_unspent() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let reserved_at = milliseconds_after(admitted_at, 1);
        let permit = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:pre-io"),
                    reference("operation:pre-io"),
                    AppResourceQuantity {
                        payload_bytes: 64,
                        ..AppResourceQuantity::default()
                    },
                    Vec::new(),
                    1,
                    50_000,
                ),
                reserved_at,
            )
            .await
            .unwrap()
            .into_dispatch_permit()
            .unwrap();
        let released_at = milliseconds_after(admitted_at, 2);
        let released = fixture
            .coordinator
            .release_pre_io_reservation(
                &fixture.authenticated,
                &root_lease,
                permit,
                resolved_at(&fixture.resolved, released_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                AppResourcePreIoUnspentReconciliation::from_dispatch_owner(
                    reference("observation:pre-io"),
                    reference("reconciliation:pre-io"),
                    revision(1),
                    2,
                ),
                released_at,
            )
            .await
            .unwrap();
        assert_eq!(released.state.held_reservations, 0);
        assert_eq!(released.state.outstanding_reserved.payload_bytes, 0);
        let journal = fixture
            .service
            .journal(
                &fixture.authenticated,
                &fixture.binding.budget_ledger_ref,
                released_at,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            journal.events.last(),
            Some(AppResourceJournalEvent::Settled {
                outcome: AppResourceSettlementOutcome::ProvenUnspent,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn committed_receipt_recovery_binds_exact_persisted_reservation_without_a_permit() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let reserved_at = milliseconds_after(admitted_at, 1);
        let permit = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:committed-recovery"),
                    reference("operation:committed-recovery"),
                    AppResourceQuantity {
                        records: 1,
                        payload_bytes: 256,
                        ..AppResourceQuantity::default()
                    },
                    Vec::new(),
                    1,
                    50_000,
                ),
                reserved_at,
            )
            .await
            .unwrap()
            .into_dispatch_permit()
            .unwrap();
        // Simulate the process dying after entity-store I/O: recovery has the
        // canonical receipt, but it cannot and must not recreate this permit.
        drop(permit);
        drop(root_lease);
        revoke_fixture_grant(&fixture, milliseconds_after(admitted_at, 2)).await;

        let recovered_at = milliseconds_after(admitted_at, 2);
        let origin = AppMutationOrigin::Workflow {
            execution_id: fixture.binding.execution_id.clone(),
            output_revision: revision(1),
            source_artifact_refs: Vec::new(),
        };
        let mutation_key = AppDigest::blake3(b"committed-recovery-mutation");
        let batch_digest = AppDigest::blake3(b"committed-recovery-batch");
        let intent_digest = AppDigest::blake3(b"committed-recovery-intent");
        let receipt = committed_mutation_receipt(
            &fixture,
            origin.clone(),
            mutation_key.clone(),
            batch_digest.clone(),
            recovered_at,
        );
        let actual = AppResourceQuantity {
            records: 1,
            payload_bytes: 128,
            ..AppResourceQuantity::default()
        };

        let mismatched = AppResourceCommittedRecovery::from_verified_mutation_receipt(
            reference("node:root"),
            reference("reservation:committed-recovery"),
            reference("operation:rebound"),
            intent_digest.clone(),
            origin.clone(),
            mutation_key.clone(),
            batch_digest.clone(),
            &receipt,
            actual,
            2,
        )
        .unwrap();
        let error = fixture
            .coordinator
            .reconcile_committed_operation(
                &fixture.authenticated,
                resolved_at(&fixture.resolved, recovered_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                mismatched,
                recovered_at,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppResourceAuthorityError::IdentityConflict {
                entity: "committed recovery reservation",
                ..
            }
        ));

        let proof = AppResourceCommittedRecovery::from_verified_mutation_receipt(
            reference("node:root"),
            reference("reservation:committed-recovery"),
            reference("operation:committed-recovery"),
            intent_digest.clone(),
            origin.clone(),
            mutation_key.clone(),
            batch_digest.clone(),
            &receipt,
            actual,
            2,
        )
        .unwrap();
        let debug = format!("{proof:?}");
        assert!(!debug.contains(receipt.receipt_id.as_str()));
        assert!(!debug.contains("committed-recovery-intent"));
        let recovered = fixture
            .coordinator
            .reconcile_committed_operation(
                &fixture.authenticated,
                resolved_at(&fixture.resolved, recovered_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                proof,
                recovered_at,
            )
            .await
            .unwrap();
        assert_eq!(recovered.state.held_reservations, 0);
        assert_eq!(recovered.state.committed.records, 1);
        assert_eq!(recovered.state.committed.payload_bytes, 128);
        let journal = fixture
            .service
            .journal(
                &fixture.authenticated,
                &fixture.binding.budget_ledger_ref,
                recovered_at,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            journal.events.last(),
            Some(AppResourceJournalEvent::Settled {
                reservation_id,
                outcome: AppResourceSettlementOutcome::Committed,
                observation_sources,
                actual,
                ..
            }) if reservation_id == &reference("reservation:committed-recovery")
                && observation_sources == &[AppResourceObservationSource::AppStoreTransaction]
                && actual.records == 1
                && actual.payload_bytes == 128
        ));

        let replay_proof = AppResourceCommittedRecovery::from_verified_mutation_receipt(
            reference("node:root"),
            reference("reservation:committed-recovery"),
            reference("operation:committed-recovery"),
            intent_digest,
            origin,
            mutation_key,
            batch_digest,
            &receipt,
            actual,
            2,
        )
        .unwrap();
        let replay = fixture
            .coordinator
            .reconcile_committed_operation(
                &fixture.authenticated,
                resolved_at(&fixture.resolved, recovered_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                replay_proof,
                recovered_at,
            )
            .await
            .unwrap();
        assert_eq!(replay.outcome, AppResourceAppendOutcome::AlreadyPresent);
        assert_eq!(replay.state.journal_revision, 3);
    }

    #[tokio::test]
    async fn committed_recovery_constructor_rejects_rebound_receipt_and_quantity() {
        let fixture = fixture(2).await;
        let origin = AppMutationOrigin::Workflow {
            execution_id: fixture.binding.execution_id.clone(),
            output_revision: revision(1),
            source_artifact_refs: Vec::new(),
        };
        let mutation_key = AppDigest::blake3(b"constructor-mutation");
        let batch_digest = AppDigest::blake3(b"constructor-batch");
        let receipt = committed_mutation_receipt(
            &fixture,
            origin.clone(),
            mutation_key.clone(),
            batch_digest.clone(),
            time(4),
        );
        let rebound = AppResourceCommittedRecovery::from_verified_mutation_receipt(
            reference("node:root"),
            reference("reservation:constructor"),
            reference("operation:constructor"),
            AppDigest::blake3(b"constructor-intent"),
            origin.clone(),
            mutation_key.clone(),
            AppDigest::blake3(b"other-batch"),
            &receipt,
            AppResourceQuantity {
                records: 1,
                ..AppResourceQuantity::default()
            },
            1,
        )
        .unwrap_err();
        assert!(matches!(
            rebound,
            AppResourceAuthorityError::InvalidCommittedRecovery(
                "mutation receipt batch does not match the persisted intent"
            )
        ));

        let wrong_quantity = AppResourceCommittedRecovery::from_verified_mutation_receipt(
            reference("node:root"),
            reference("reservation:constructor"),
            reference("operation:constructor"),
            AppDigest::blake3(b"constructor-intent"),
            origin,
            mutation_key,
            batch_digest,
            &receipt,
            AppResourceQuantity::default(),
            1,
        )
        .unwrap_err();
        assert!(matches!(
            wrong_quantity,
            AppResourceAuthorityError::InvalidCommittedRecovery(
                "resource quantity does not match committed record count"
            )
        ));
    }

    #[tokio::test]
    async fn no_effect_recovery_requires_a_verified_empty_command_and_settles_once() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let reserved_at = milliseconds_after(admitted_at, 1);
        let permit = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:no-effect"),
                    reference("operation:no-effect"),
                    AppResourceQuantity {
                        payload_bytes: 256,
                        ..AppResourceQuantity::default()
                    },
                    Vec::new(),
                    1,
                    50_000,
                ),
                reserved_at,
            )
            .await
            .unwrap()
            .into_dispatch_permit()
            .unwrap();
        drop(permit);
        drop(root_lease);

        let command = empty_mutation_command(&fixture, "no-effect");
        let canonical_intent_bytes = br#"{"intent":"no-effect"}"#;
        let canonical_response_bytes = br#"{"result":"already-complete"}"#;
        let actual_payload_bytes =
            u64::try_from(canonical_intent_bytes.len() + canonical_response_bytes.len()).unwrap();
        let mut effectful_command = command.clone();
        effectful_command
            .expected_record_revisions
            .push(AppExpectedRecordRevision {
                entity: AppName::parse("note").unwrap(),
                record_id: AppRecordId::parse("record_one").unwrap(),
                revision: revision(1),
            });
        let effectful = AppResourceNoEffectCommittedRecovery::from_verified_empty_mutation_command(
            reference("node:root"),
            reference("reservation:no-effect"),
            reference("operation:no-effect"),
            fixture.binding.installation_id.clone(),
            canonical_intent_bytes,
            &effectful_command,
            canonical_response_bytes,
            AppResourceQuantity {
                payload_bytes: actual_payload_bytes,
                ..AppResourceQuantity::default()
            },
            2,
        )
        .unwrap_err();
        assert!(matches!(
            effectful,
            AppResourceAuthorityError::InvalidCommittedRecovery(
                "no-effect recovery requires an empty mutation command"
            )
        ));
        let invalid = AppResourceNoEffectCommittedRecovery::from_verified_empty_mutation_command(
            reference("node:root"),
            reference("reservation:no-effect"),
            reference("operation:no-effect"),
            fixture.binding.installation_id.clone(),
            canonical_intent_bytes,
            &command,
            canonical_response_bytes,
            AppResourceQuantity {
                records: 1,
                ..AppResourceQuantity::default()
            },
            2,
        )
        .unwrap_err();
        assert!(matches!(
            invalid,
            AppResourceAuthorityError::InvalidCommittedRecovery(
                "no-effect recovery may report only exact canonical payload bytes"
            )
        ));
        let active_intervals = vec![AppActiveInterval {
            start_elapsed_ms: 1,
            end_elapsed_ms: 2,
        }];
        let mismatched_payload =
            AppResourceNoEffectCommittedRecovery::from_verified_empty_mutation_command(
                reference("node:root"),
                reference("reservation:no-effect"),
                reference("operation:no-effect"),
                fixture.binding.installation_id.clone(),
                canonical_intent_bytes,
                &command,
                canonical_response_bytes,
                AppResourceQuantity {
                    payload_bytes: actual_payload_bytes - 1,
                    ..AppResourceQuantity::default()
                },
                2,
            )
            .unwrap_err();
        assert!(matches!(
            mismatched_payload,
            AppResourceAuthorityError::InvalidCommittedRecovery(
                "no-effect recovery payload does not match canonical intent and response bytes"
            )
        ));
        let noncanonical_response = br#"{ "result": "already-complete" }"#;
        let noncanonical =
            AppResourceNoEffectCommittedRecovery::from_verified_empty_mutation_command(
                reference("node:root"),
                reference("reservation:no-effect"),
                reference("operation:no-effect"),
                fixture.binding.installation_id.clone(),
                canonical_intent_bytes,
                &command,
                noncanonical_response,
                AppResourceQuantity {
                    payload_bytes: u64::try_from(
                        canonical_intent_bytes.len() + noncanonical_response.len(),
                    )
                    .unwrap(),
                    ..AppResourceQuantity::default()
                },
                2,
            )
            .unwrap_err();
        assert!(matches!(
            noncanonical,
            AppResourceAuthorityError::InvalidCommittedRecovery(
                "no-effect recovery response is not canonical JSON"
            )
        ));
        let oversized_response = canonical_json_bytes(&serde_json::json!({
            "result": "x".repeat(256),
        }))
        .unwrap();
        let oversized_payload_bytes =
            u64::try_from(canonical_intent_bytes.len() + oversized_response.len()).unwrap();
        let oversized_proof =
            AppResourceNoEffectCommittedRecovery::from_verified_empty_mutation_command(
                reference("node:root"),
                reference("reservation:no-effect"),
                reference("operation:no-effect"),
                fixture.binding.installation_id.clone(),
                canonical_intent_bytes,
                &command,
                oversized_response.as_slice(),
                AppResourceQuantity {
                    payload_bytes: oversized_payload_bytes,
                    ..AppResourceQuantity::default()
                },
                2,
            )
            .unwrap();
        let recovered_at = milliseconds_after(admitted_at, 2);
        let oversized_error = fixture
            .coordinator
            .reconcile_no_effect_committed_operation(
                &fixture.authenticated,
                resolved_at(&fixture.resolved, recovered_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                oversized_proof,
                recovered_at,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            oversized_error,
            AppResourceAuthorityError::Contract(
                AppResourceContractError::SettlementExceedsReservation {
                    field: "payload_bytes"
                }
            )
        ));
        let proof = AppResourceNoEffectCommittedRecovery::from_verified_empty_mutation_command(
            reference("node:root"),
            reference("reservation:no-effect"),
            reference("operation:no-effect"),
            fixture.binding.installation_id.clone(),
            canonical_intent_bytes,
            &command,
            canonical_response_bytes,
            AppResourceQuantity {
                payload_bytes: actual_payload_bytes,
                ..AppResourceQuantity::default()
            },
            2,
        )
        .unwrap();
        let proof_debug = format!("{proof:?}");
        assert!(!proof_debug.contains("no-effect"));
        assert!(!proof_debug.contains("already-complete"));
        let settled = fixture
            .coordinator
            .reconcile_no_effect_committed_operation(
                &fixture.authenticated,
                resolved_at(&fixture.resolved, recovered_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                proof,
                recovered_at,
            )
            .await
            .unwrap();
        assert_eq!(settled.state.held_reservations, 0);
        assert_eq!(settled.state.committed.payload_bytes, actual_payload_bytes);
        let journal = fixture
            .service
            .journal(
                &fixture.authenticated,
                &fixture.binding.budget_ledger_ref,
                recovered_at,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            journal.events.last(),
            Some(AppResourceJournalEvent::Settled {
                outcome: AppResourceSettlementOutcome::Committed,
                observation_sources,
                actual,
                ..
            }) if observation_sources == &[AppResourceObservationSource::WorkflowNoEffect]
                && actual.payload_bytes == actual_payload_bytes
        ));
        assert_eq!(
            journal.events.last().and_then(|event| match event {
                AppResourceJournalEvent::Settled {
                    active_intervals, ..
                } => Some(active_intervals.as_slice()),
                _ => None,
            }),
            Some(active_intervals.as_slice())
        );
    }

    #[tokio::test]
    async fn verified_receipt_absence_releases_ambiguous_reservation_for_fresh_work() {
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let root_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let reserved_at = milliseconds_after(admitted_at, 1);
        let permit = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &root_lease,
                resolved_at(&fixture.resolved, reserved_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 1,
                    minimum_period_revision: revision(2),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:ambiguous"),
                    reference("operation:ambiguous"),
                    AppResourceQuantity {
                        records: 1,
                        ..AppResourceQuantity::default()
                    },
                    Vec::new(),
                    1,
                    50_000,
                ),
                reserved_at,
            )
            .await
            .unwrap()
            .into_dispatch_permit()
            .unwrap();
        drop(permit);
        drop(root_lease);

        let origin = AppMutationOrigin::Workflow {
            execution_id: fixture.binding.execution_id.clone(),
            output_revision: revision(1),
            source_artifact_refs: Vec::new(),
        };
        let recovered_at = milliseconds_after(admitted_at, 2);
        let mutation_key = AppDigest::blake3(b"ambiguous-mutation");
        let batch_digest = AppDigest::blake3(b"ambiguous-batch");
        let stored_installation = fixture.binding.installation_id.clone();
        let stored_mutation_key = mutation_key.clone();
        let stored_batch_digest = batch_digest.clone();
        let stored_committed_at = format_timestamp(&recovered_at);
        fixture
            .service
            .registry
            .execute_scoped_write(
                &fixture.authenticated,
                &recovered_at,
                move |connection, _scope| {
                    connection.execute(
                        "INSERT INTO app_mutation_receipts (
                             receipt_id, installation_id, idempotency_key, mutation_digest,
                             first_change_seq, last_change_seq, record_json, committed_at
                         ) VALUES (?1, ?2, ?3, ?4, 1, 1, ?5, ?6)",
                        params![
                            "receipt:present",
                            stored_installation.as_str(),
                            stored_mutation_key.as_str(),
                            stored_batch_digest.as_str(),
                            b"{}".as_slice(),
                            stored_committed_at,
                        ],
                    )?;
                    Ok(())
                },
            )
            .await
            .unwrap();
        let present_proof = AppResourceReceiptAbsentReconciliation::from_verified_receipt_absence(
            reference("node:root"),
            reference("reservation:ambiguous"),
            reference("operation:ambiguous"),
            fixture.binding.installation_id.clone(),
            AppDigest::blake3(b"ambiguous-intent"),
            origin.clone(),
            mutation_key.clone(),
            batch_digest.clone(),
            AppResourceQuantity::default(),
            2,
        )
        .unwrap();
        let present_error = fixture
            .coordinator
            .reconcile_verified_receipt_absence(
                &fixture.authenticated,
                resolved_at(&fixture.resolved, recovered_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                present_proof,
                recovered_at,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            present_error,
            AppResourceAuthorityError::InvalidCommittedRecovery(
                "canonical mutation receipt lookup did not prove absence"
            )
        ));
        let stored_mutation_key = mutation_key.clone();
        let removed_installation = fixture.binding.installation_id.clone();
        fixture
            .service
            .registry
            .execute_scoped_write(
                &fixture.authenticated,
                &recovered_at,
                move |connection, _scope| {
                    connection.execute(
                        "DELETE FROM app_mutation_receipts
                          WHERE installation_id = ?1 AND idempotency_key = ?2",
                        params![removed_installation.as_str(), stored_mutation_key.as_str(),],
                    )?;
                    Ok(())
                },
            )
            .await
            .unwrap();
        let rebound_origin = AppResourceReceiptAbsentReconciliation::from_verified_receipt_absence(
            reference("node:root"),
            reference("reservation:ambiguous"),
            reference("operation:ambiguous"),
            fixture.binding.installation_id.clone(),
            AppDigest::blake3(b"ambiguous-intent"),
            AppMutationOrigin::Workflow {
                execution_id: reference("execution:other"),
                output_revision: revision(1),
                source_artifact_refs: Vec::new(),
            },
            mutation_key.clone(),
            batch_digest.clone(),
            AppResourceQuantity::default(),
            2,
        )
        .unwrap();
        let rebound_error = fixture
            .coordinator
            .reconcile_verified_receipt_absence(
                &fixture.authenticated,
                resolved_at(&fixture.resolved, recovered_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                rebound_origin,
                recovered_at,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            rebound_error,
            AppResourceAuthorityError::IdentityConflict {
                entity: "receipt-absence recovery origin",
                ..
            }
        ));
        let proof = AppResourceReceiptAbsentReconciliation::from_verified_receipt_absence(
            reference("node:root"),
            reference("reservation:ambiguous"),
            reference("operation:ambiguous"),
            fixture.binding.installation_id.clone(),
            AppDigest::blake3(b"ambiguous-intent"),
            origin,
            mutation_key,
            batch_digest,
            AppResourceQuantity::default(),
            2,
        )
        .unwrap();
        let released = fixture
            .coordinator
            .reconcile_verified_receipt_absence(
                &fixture.authenticated,
                resolved_at(&fixture.resolved, recovered_at),
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 2,
                    minimum_period_revision: revision(3),
                },
                proof,
                recovered_at,
            )
            .await
            .unwrap();
        assert_eq!(released.state.held_reservations, 0);
        assert_eq!(released.state.outstanding_reserved.records, 0);

        let resumed_at = milliseconds_after(admitted_at, 3);
        let resumed_resolved = resolved_at(&fixture.resolved, resumed_at);
        let resumed_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &resumed_resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                resumed_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let fresh = fixture
            .coordinator
            .reserve_operation(
                &fixture.authenticated,
                &resumed_lease,
                resumed_resolved,
                fixture.binding.clone(),
                enforcement_policy(),
                AppResourceMutationFence {
                    expected_journal_revision: 3,
                    minimum_period_revision: revision(4),
                },
                AppResourceReservationRequest::from_dispatch_owner(
                    reference("node:root"),
                    reference("reservation:fresh-after-absence"),
                    reference("operation:fresh-after-absence"),
                    AppResourceQuantity {
                        records: 1,
                        ..AppResourceQuantity::default()
                    },
                    Vec::new(),
                    3,
                    50_000,
                ),
                resumed_at,
            )
            .await
            .unwrap();
        assert!(fresh.into_dispatch_permit().is_ok());
    }

    #[tokio::test]
    async fn maintenance_inspection_is_keyset_bounded_and_projection_only() {
        assert!(matches!(
            AppResourceMaintenanceBatchRequest::from_driver(
                AppResourceMaintenanceCursor {
                    after_budget_ledger_ref: Some(reference("ledger:stale-cursor")),
                    after_period_ref: None,
                    cleanup_exhausted: true,
                    periods_exhausted: false,
                },
                1,
                1,
            ),
            Err(AppResourceAuthorityError::InvalidMaintenance(_))
        ));
        let fixture = fixture(2).await;
        let admitted_at = time(4);
        let first_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &fixture.binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:root"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();
        let mut second_binding = fixture.binding.clone();
        second_binding.execution_id = reference("execution:second");
        second_binding.budget_ledger_ref = reference("ledger:second");
        let second_lease = fixture
            .coordinator
            .admit_workflow_root(
                &fixture.authenticated,
                &fixture.resolved,
                &second_binding,
                enforcement_policy(),
                AppResourceExecutionLane::Foreground,
                reference("node:second"),
                admitted_at,
            )
            .await
            .unwrap()
            .into_dispatch_lease()
            .unwrap();

        let first = fixture
            .coordinator
            .inspect_maintenance_batch(
                &fixture.authenticated,
                &fixture.resolved,
                AppResourceMaintenanceBatchRequest::from_driver(
                    AppResourceMaintenanceCursor::default(),
                    1,
                    1,
                )
                .unwrap(),
                admitted_at,
            )
            .await
            .unwrap();
        assert_eq!(first.cleanup_candidates.len(), 1);
        assert!(first.cleanup_has_more);
        assert_eq!(first.periods.len(), 1);
        assert!(first.periods[0].projection_stale);
        assert!(!first.periods[0].rollover_due);
        assert!(!first.periods[0].has_terminal_trees);
        let second = fixture
            .coordinator
            .inspect_maintenance_batch(
                &fixture.authenticated,
                &fixture.resolved,
                AppResourceMaintenanceBatchRequest::from_driver(first.next_cursor, 1, 1).unwrap(),
                admitted_at,
            )
            .await
            .unwrap();
        assert_eq!(second.cleanup_candidates.len(), 1);
        assert!(!second.cleanup_has_more);
        assert!(second.periods.is_empty());
        assert!(second.next_cursor.cleanup_exhausted);
        assert!(second.next_cursor.periods_exhausted);
        drop(first_lease);
        drop(second_lease);
    }

    #[tokio::test]
    async fn registry_dispatch_revalidation_is_exact_and_revocation_fails_closed() {
        let fixture = fixture(2).await;
        let receipt = fixture
            .service
            .registry
            .revalidate_current_authority(&fixture.authenticated, &fixture.resolved, time(4))
            .await
            .unwrap();
        assert_eq!(receipt.installation_id, fixture.resolved.installation_id);
        assert_eq!(
            receipt.grant_authority_digest,
            fixture.resolved.grant_authority_digest
        );
        assert_eq!(receipt.authority_digest, fixture.resolved.authority_digest);
        assert_ne!(receipt.grant_authority_digest, receipt.authority_digest);
        let callback_receipt = fixture
            .service
            .registry
            .revalidate_current_authority(&fixture.authenticated, &fixture.resolved, time(5))
            .await
            .unwrap();
        assert_eq!(callback_receipt.authority_digest, receipt.authority_digest);

        let mut future_dated = fixture.resolved.clone();
        future_dated.resolved_at = time(6);
        assert!(matches!(
            fixture
                .service
                .registry
                .revalidate_current_authority(&fixture.authenticated, &future_dated, time(5))
                .await,
            Err(AppRegistryError::StateConflict(_))
        ));

        let mut tampered = fixture.resolved.clone();
        tampered.effective_resources.max_input_tokens -= 1;
        assert!(matches!(
            fixture
                .service
                .registry
                .revalidate_current_authority(&fixture.authenticated, &tampered, time(4))
                .await,
            Err(AppRegistryError::StateConflict(_))
        ));
        let mut wrong_authentication_class = fixture.resolved.clone();
        wrong_authentication_class.authentication = AppScopeAuthentication::TaskExecution;
        wrong_authentication_class.authority_digest = wrong_authentication_class
            .canonical_authority_digest()
            .unwrap();
        assert!(matches!(
            fixture
                .service
                .registry
                .revalidate_current_authority(
                    &fixture.authenticated,
                    &wrong_authentication_class,
                    time(4),
                )
                .await,
            Err(AppRegistryError::StateConflict(_))
        ));
        let installation_id = fixture.resolved.installation_id.clone();
        fixture
            .service
            .registry
            .execute_scoped_write(&fixture.authenticated, &time(4), move |connection, _| {
                connection.execute(
                    "UPDATE app_grant_revisions SET revoked_at = ?1
                      WHERE installation_id = ?2 AND revision = 1",
                    params![format_timestamp(&time(4)), installation_id.as_str()],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .service
                .registry
                .revalidate_current_authority(&fixture.authenticated, &fixture.resolved, time(4))
                .await,
            Err(AppRegistryError::StateConflict(_))
        ));
    }
}
