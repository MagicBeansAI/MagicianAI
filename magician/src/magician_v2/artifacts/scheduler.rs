//! # Lifecycle Scheduler
//!
//! Runs mark-and-sweep cleanup with safety guards and dry-run support (Section
//! 13 of the artifact lifecycle plan).
//!
//! ## Mark Phase
//! - Evaluate policy + state + references for each artifact.
//! - Produce a candidate set with reason codes.
//!
//! ## Sweep Phase
//! - Apply bounded deletes (rate-limited per run).
//! - Respect protection flags and references.
//! - Emit tombstones where required.
//!
//! ## Trigger Model
//! - Periodic background sweep.
//! - Opportunistic small sweep on writes.
//! - Immediate scope cleanup on explicit lifecycle events (task/thread close).
//!
//! ## Safety Controls
//! - Dry-run mode.
//! - Per-domain kill switch.
//! - Maximum deletes per sweep.
//! - Rollback path for accidental policy misconfiguration.

use super::catalog::ArtifactCatalog;
use super::policy::{PolicyEngine, ScopeEvent};
use super::types::{
    ArtifactDomain, ArtifactMetadata, ArtifactUid, CatalogQuery, LifecycleState, SweepConfig,
    SweepResult, TransitionReason,
};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashSet;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Lifecycle state severity ordering (for sweep-only-forward logic)
// ---------------------------------------------------------------------------

/// Returns a numeric severity for a lifecycle state. Higher values indicate
/// more degraded / terminal states. The scheduler only produces candidates
/// that move to a *higher* severity, preventing accidental reactivation
/// during cleanup sweeps.
fn state_severity(state: LifecycleState) -> u8 {
    match state {
        LifecycleState::Active => 0,
        LifecycleState::Stale => 1,
        LifecycleState::Quarantined => 2,
        LifecycleState::Expired => 3,
        LifecycleState::Superseded => 4,
        LifecycleState::DeletePending => 5,
        LifecycleState::Deleted => 6,
    }
}

// ---------------------------------------------------------------------------
// SweepCandidate
// ---------------------------------------------------------------------------

/// A single artifact identified during the mark phase as needing a lifecycle
/// state transition.
#[derive(Debug, Clone)]
pub struct SweepCandidate {
    /// UID of the artifact.
    pub artifact_uid: ArtifactUid,
    /// Current lifecycle state at the time of evaluation.
    pub current_state: LifecycleState,
    /// Recommended new state from the policy engine.
    pub recommended_state: LifecycleState,
    /// Reason code for the recommended transition.
    pub reason: TransitionReason,
    /// Whether deletion is blocked by protection flags or references.
    pub deletion_blocked: bool,
    /// Domain the artifact belongs to.
    pub domain: ArtifactDomain,
}

// ---------------------------------------------------------------------------
// LifecycleScheduler
// ---------------------------------------------------------------------------

/// The lifecycle scheduler orchestrates mark-and-sweep cleanup cycles.
///
/// It does **not** own the catalog. The catalog is passed as a parameter to
/// sweep methods, keeping the scheduler stateless with respect to storage.
pub struct LifecycleScheduler {
    /// Sweep configuration (limits, dry-run, kill switches).
    pub config: SweepConfig,
    /// Shared reference to the policy engine used for evaluations.
    pub policy_engine: Arc<PolicyEngine>,
    /// Lock preventing concurrent sweep runs from racing.
    sweep_lock: std::sync::Mutex<()>,
}

impl LifecycleScheduler {
    /// Construct a new scheduler with the given configuration and policy
    /// engine.
    pub fn new(config: SweepConfig, policy_engine: Arc<PolicyEngine>) -> Self {
        Self {
            config,
            policy_engine,
            sweep_lock: std::sync::Mutex::new(()),
        }
    }

    // -----------------------------------------------------------------------
    // Mark phase
    // -----------------------------------------------------------------------

    /// Evaluate each artifact against the policy engine and return candidates
    /// whose recommended state differs from their current state.
    ///
    /// Artifacts in domains with an active kill switch are skipped.
    pub fn mark(&self, artifacts: &[&ArtifactMetadata], now: DateTime<Utc>) -> Vec<SweepCandidate> {
        tracing::info!(
            artifact_count = artifacts.len(),
            dry_run = self.config.dry_run,
            "mark phase starting"
        );

        let mut candidates = Vec::new();

        for artifact in artifacts {
            // Check domain kill switch.
            let domain_key = artifact.domain.to_string();
            if self
                .config
                .domain_kill_switches
                .get(&domain_key)
                .copied()
                .unwrap_or(false)
            {
                tracing::debug!(
                    artifact_uid = %artifact.artifact_uid,
                    domain = %artifact.domain,
                    "skipping artifact — domain kill switch active"
                );
                continue;
            }

            // Evaluate policy.
            let decision = self.policy_engine.evaluate(artifact, now);

            // Only include candidates where the policy engine recommends a
            // forward (more degraded) transition. The scheduler is a cleanup
            // mechanism — it never reactivates artifacts.
            let forward =
                state_severity(decision.recommended_state) > state_severity(decision.current_state);
            if forward {
                tracing::debug!(
                    artifact_uid = %artifact.artifact_uid,
                    current = %decision.current_state,
                    recommended = %decision.recommended_state,
                    deletion_blocked = decision.deletion_blocked,
                    "candidate identified for transition"
                );

                candidates.push(SweepCandidate {
                    artifact_uid: decision.artifact_uid,
                    current_state: decision.current_state,
                    recommended_state: decision.recommended_state,
                    reason: decision.reason,
                    deletion_blocked: decision.deletion_blocked,
                    domain: artifact.domain,
                });
            }
        }

        tracing::info!(candidates = candidates.len(), "mark phase complete");

        candidates
    }

    // -----------------------------------------------------------------------
    // Sweep phase
    // -----------------------------------------------------------------------

    /// Apply transitions from the candidate set, respecting rate limits,
    /// dry-run mode, grace periods, protection, and references.
    ///
    /// Returns a `SweepResult` summarising what was (or would be) done.
    pub fn sweep(
        &self,
        candidates: &[SweepCandidate],
        catalog: &dyn ArtifactCatalog,
    ) -> SweepResult {
        tracing::info!(
            candidates = candidates.len(),
            max_deletes = self.config.max_deletes_per_sweep,
            dry_run = self.config.dry_run,
            "sweep phase starting"
        );

        let mut result = SweepResult {
            candidates_evaluated: candidates.len(),
            dry_run: self.config.dry_run,
            ..Default::default()
        };

        let mut applied_count: usize = 0;

        for candidate in candidates {
            // Enforce the per-sweep transition limit.
            if applied_count >= self.config.max_deletes_per_sweep {
                tracing::info!(
                    limit = self.config.max_deletes_per_sweep,
                    "max transitions per sweep reached — stopping"
                );
                break;
            }

            // Determine whether this is a deletion (transition to Deleted)
            // versus an intermediate state change.
            let is_deletion = candidate.recommended_state == LifecycleState::Deleted;

            if is_deletion {
                // Deletions require additional safety checks.
                match self.attempt_deletion(candidate, catalog, &mut result) {
                    DeletionOutcome::Applied => {
                        applied_count += 1;
                    },
                    DeletionOutcome::Blocked => {
                        // Already accounted for in result.deletions_blocked.
                    },
                    DeletionOutcome::Error => {
                        // Already recorded in result.errors.
                        result.retry_candidates.push(candidate.artifact_uid.clone());
                    },
                }
            } else {
                // Intermediate transition (e.g. Active -> Stale).
                if self.config.dry_run {
                    tracing::debug!(
                        artifact_uid = %candidate.artifact_uid,
                        from = %candidate.current_state,
                        to = %candidate.recommended_state,
                        "dry-run: would transition"
                    );
                    result.transitions_applied += 1;
                    applied_count += 1;
                } else {
                    match catalog.transition(
                        &candidate.artifact_uid,
                        candidate.recommended_state,
                        candidate.reason.clone(),
                    ) {
                        Ok(()) => {
                            tracing::debug!(
                                artifact_uid = %candidate.artifact_uid,
                                from = %candidate.current_state,
                                to = %candidate.recommended_state,
                                "transition applied"
                            );
                            result.transitions_applied += 1;
                            applied_count += 1;
                        },
                        Err(e) => {
                            tracing::warn!(
                                artifact_uid = %candidate.artifact_uid,
                                error = %e,
                                "transition failed"
                            );
                            result.errors.push(format!(
                                "transition {} -> {}: {}",
                                candidate.artifact_uid, candidate.recommended_state, e
                            ));
                        },
                    }
                }
            }
        }

        tracing::info!(
            transitions = result.transitions_applied,
            deletions = result.deletions_performed,
            blocked = result.deletions_blocked,
            errors = result.errors.len(),
            dry_run = result.dry_run,
            "sweep phase complete"
        );

        result
    }

    // -----------------------------------------------------------------------
    // Full sweep (mark + sweep over entire catalog)
    // -----------------------------------------------------------------------

    /// Load all non-terminal artifacts from the catalog, run mark, then sweep.
    pub fn run_full_sweep(&self, catalog: &dyn ArtifactCatalog, now: DateTime<Utc>) -> SweepResult {
        let _guard = match self.sweep_lock.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::warn!("lifecycle sweep already in progress, skipping");
                return SweepResult {
                    errors: vec!["sweep already in progress".to_string()],
                    ..Default::default()
                };
            },
        };
        tracing::info!("full sweep starting");

        // Query all non-terminal states.
        let mut states = HashSet::new();
        states.insert(LifecycleState::Active);
        states.insert(LifecycleState::Stale);
        states.insert(LifecycleState::Expired);
        states.insert(LifecycleState::Quarantined);
        states.insert(LifecycleState::Superseded);
        states.insert(LifecycleState::DeletePending);

        let query = CatalogQuery {
            lifecycle_states: states,
            ..Default::default()
        };

        let artifacts = match catalog.query(&query) {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "failed to query catalog for full sweep");
                return SweepResult {
                    errors: vec![format!("catalog query failed: {}", e)],
                    dry_run: self.config.dry_run,
                    ..Default::default()
                };
            },
        };

        let refs: Vec<&ArtifactMetadata> = artifacts.iter().collect();
        let candidates = self.mark(&refs, now);
        self.sweep(&candidates, catalog)
    }

    // -----------------------------------------------------------------------
    // Scope cleanup
    // -----------------------------------------------------------------------

    /// React to a scope closure event by expiring matching artifacts and then
    /// sweeping them.
    ///
    /// For `ThreadClosed`, loads artifacts by `execution_id`.
    /// For `TaskClosed`, loads artifacts by `task_id` (uses execution scope
    /// when task_id is stored there).
    /// For `RunCompleted`, loads artifacts by `run_id` (workflow_instance_id).
    /// For `GoalCompleted`, loads all non-terminal artifacts (goal scope is
    /// broad).
    pub fn run_scope_cleanup(
        &self,
        catalog: &dyn ArtifactCatalog,
        scope_event: &ScopeEvent,
        now: DateTime<Utc>,
    ) -> SweepResult {
        let _guard = match self.sweep_lock.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::warn!("lifecycle sweep already in progress, skipping scope cleanup");
                return SweepResult {
                    errors: vec!["sweep already in progress".to_string()],
                    ..Default::default()
                };
            },
        };
        tracing::info!(scope_event = ?scope_event, "scope cleanup starting");

        let non_terminal_states = {
            let mut s = HashSet::new();
            s.insert(LifecycleState::Active);
            s.insert(LifecycleState::Stale);
            s.insert(LifecycleState::Expired);
            s.insert(LifecycleState::Quarantined);
            s.insert(LifecycleState::Superseded);
            s.insert(LifecycleState::DeletePending);
            s
        };

        let query = match scope_event {
            ScopeEvent::ThreadClosed { thread_id } => CatalogQuery {
                execution_id: Some(thread_id.clone()),
                lifecycle_states: non_terminal_states,
                ..Default::default()
            },
            ScopeEvent::TaskClosed { task_id } => {
                // Tasks are currently stored under execution scope in ownership
                // scope. Fall back to broad query filtered by policy.
                CatalogQuery {
                    execution_id: Some(task_id.clone()),
                    lifecycle_states: non_terminal_states,
                    ..Default::default()
                }
            },
            ScopeEvent::RunCompleted { run_id } => CatalogQuery {
                workflow_instance_id: Some(run_id.clone()),
                lifecycle_states: non_terminal_states,
                ..Default::default()
            },
            ScopeEvent::GoalCompleted { .. } => CatalogQuery {
                lifecycle_states: non_terminal_states,
                ..Default::default()
            },
        };

        let artifacts = match catalog.query(&query) {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "failed to query catalog for scope cleanup");
                return SweepResult {
                    errors: vec![format!("catalog query failed: {}", e)],
                    dry_run: self.config.dry_run,
                    ..Default::default()
                };
            },
        };

        // Phase 1: Expire artifacts whose scope has ended.
        let mut expired_count: usize = 0;
        for artifact in &artifacts {
            if self.policy_engine.is_scope_expired(artifact, scope_event) {
                // Only expire if the artifact is in a non-expired, non-terminal
                // state.
                if matches!(
                    artifact.lifecycle_state,
                    LifecycleState::Active | LifecycleState::Stale
                ) {
                    if self.config.dry_run {
                        tracing::debug!(
                            artifact_uid = %artifact.artifact_uid,
                            "dry-run: would expire due to scope closure"
                        );
                        expired_count += 1;
                    } else {
                        let reason = TransitionReason::ScopeBoundary {
                            event: format!("{:?}", scope_event),
                        };
                        match catalog.transition(
                            &artifact.artifact_uid,
                            LifecycleState::Expired,
                            reason,
                        ) {
                            Ok(()) => {
                                tracing::debug!(
                                    artifact_uid = %artifact.artifact_uid,
                                    "expired due to scope closure"
                                );
                                expired_count += 1;
                            },
                            Err(e) => {
                                tracing::warn!(
                                    artifact_uid = %artifact.artifact_uid,
                                    error = %e,
                                    "failed to expire artifact on scope closure"
                                );
                            },
                        }
                    }
                }
            }
        }

        tracing::info!(
            expired_count,
            "scope expiration phase complete, running sweep"
        );

        // Phase 2: Re-query to pick up newly expired/superseded artifacts, then sweep.
        let updated_query = match scope_event {
            ScopeEvent::ThreadClosed { thread_id } => CatalogQuery {
                execution_id: Some(thread_id.clone()),
                lifecycle_states: {
                    let mut s = HashSet::new();
                    s.insert(LifecycleState::Expired);
                    s.insert(LifecycleState::Superseded);
                    s.insert(LifecycleState::DeletePending);
                    s
                },
                ..Default::default()
            },
            ScopeEvent::TaskClosed { task_id } => CatalogQuery {
                execution_id: Some(task_id.clone()),
                lifecycle_states: {
                    let mut s = HashSet::new();
                    s.insert(LifecycleState::Expired);
                    s.insert(LifecycleState::Superseded);
                    s.insert(LifecycleState::DeletePending);
                    s
                },
                ..Default::default()
            },
            ScopeEvent::RunCompleted { run_id } => CatalogQuery {
                workflow_instance_id: Some(run_id.clone()),
                lifecycle_states: {
                    let mut s = HashSet::new();
                    s.insert(LifecycleState::Expired);
                    s.insert(LifecycleState::Superseded);
                    s.insert(LifecycleState::DeletePending);
                    s
                },
                ..Default::default()
            },
            ScopeEvent::GoalCompleted { .. } => CatalogQuery {
                lifecycle_states: {
                    let mut s = HashSet::new();
                    s.insert(LifecycleState::Expired);
                    s.insert(LifecycleState::Superseded);
                    s.insert(LifecycleState::DeletePending);
                    s
                },
                ..Default::default()
            },
        };

        let updated_artifacts = match catalog.query(&updated_query) {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "failed to re-query after scope expiration");
                return SweepResult {
                    transitions_applied: expired_count,
                    errors: vec![format!("re-query after scope expiration failed: {}", e)],
                    dry_run: self.config.dry_run,
                    ..Default::default()
                };
            },
        };

        let refs: Vec<&ArtifactMetadata> = updated_artifacts.iter().collect();
        let candidates = self.mark(&refs, now);
        let mut sweep_result = self.sweep(&candidates, catalog);

        // Include the scope-expiration transitions in the total count.
        sweep_result.transitions_applied += expired_count;

        sweep_result
    }

    // -----------------------------------------------------------------------
    // Internal: deletion attempt with safety checks
    // -----------------------------------------------------------------------

    /// Attempt to delete (transition to Deleted) a single candidate.
    ///
    /// Checks:
    /// 1. Policy-level deletion_blocked flag (from mark phase).
    /// 2. Protection flags on the live artifact.
    /// 3. Active references (ref_count > 0).
    /// 4. Grace period (time since entering DeletePending).
    fn attempt_deletion(
        &self,
        candidate: &SweepCandidate,
        catalog: &dyn ArtifactCatalog,
        result: &mut SweepResult,
    ) -> DeletionOutcome {
        // Check 1: mark-phase deletion_blocked flag.
        if candidate.deletion_blocked {
            tracing::warn!(
                artifact_uid = %candidate.artifact_uid,
                "deletion blocked by policy evaluation"
            );
            result.deletions_blocked += 1;
            return DeletionOutcome::Blocked;
        }

        // Fetch the live artifact for runtime checks.
        let artifact = match catalog.get(&candidate.artifact_uid) {
            Ok(Some(a)) => a,
            Ok(None) => {
                tracing::warn!(
                    artifact_uid = %candidate.artifact_uid,
                    "artifact not found during deletion attempt"
                );
                result.errors.push(format!(
                    "artifact {} not found during deletion",
                    candidate.artifact_uid
                ));
                return DeletionOutcome::Error;
            },
            Err(e) => {
                tracing::warn!(
                    artifact_uid = %candidate.artifact_uid,
                    error = %e,
                    "catalog error during deletion attempt"
                );
                result.errors.push(format!(
                    "catalog error for {}: {}",
                    candidate.artifact_uid, e
                ));
                return DeletionOutcome::Error;
            },
        };

        // Check 2: protection flags.
        if artifact.policy.protection_flags.is_protected() {
            tracing::warn!(
                artifact_uid = %candidate.artifact_uid,
                "deletion blocked by protection flags"
            );
            result.deletions_blocked += 1;
            return DeletionOutcome::Blocked;
        }

        // Check 3: active references.
        if artifact.ref_count() > 0 {
            tracing::warn!(
                artifact_uid = %candidate.artifact_uid,
                ref_count = artifact.ref_count(),
                "deletion blocked by active references"
            );
            result.deletions_blocked += 1;
            return DeletionOutcome::Blocked;
        }

        // Check 4: grace period.
        if !self.grace_period_elapsed(&artifact) {
            tracing::debug!(
                artifact_uid = %candidate.artifact_uid,
                grace_period_seconds = self.config.grace_period_seconds,
                "deletion deferred — grace period not yet elapsed"
            );
            result.deletions_blocked += 1;
            return DeletionOutcome::Blocked;
        }

        // All checks pass — apply the deletion.
        if self.config.dry_run {
            tracing::debug!(
                artifact_uid = %candidate.artifact_uid,
                "dry-run: would delete"
            );
            result.deletions_performed += 1;
            return DeletionOutcome::Applied;
        }

        match catalog.transition(
            &candidate.artifact_uid,
            LifecycleState::Deleted,
            TransitionReason::GracePeriodCompleted,
        ) {
            Ok(()) => {
                tracing::debug!(
                    artifact_uid = %candidate.artifact_uid,
                    "artifact deleted"
                );
                result.deletions_performed += 1;
                DeletionOutcome::Applied
            },
            Err(e) => {
                tracing::warn!(
                    artifact_uid = %candidate.artifact_uid,
                    error = %e,
                    "deletion transition failed"
                );
                result.errors.push(format!(
                    "deletion of {} failed: {}",
                    candidate.artifact_uid, e
                ));
                DeletionOutcome::Error
            },
        }
    }

    /// Determine whether the grace period has elapsed for an artifact in the
    /// DeletePending state.
    ///
    /// Looks at the transition_log for the entry where the artifact entered
    /// DeletePending. If no such log entry exists, falls back to
    /// `last_validated_at`.
    fn grace_period_elapsed(&self, artifact: &ArtifactMetadata) -> bool {
        let grace = Duration::seconds(self.config.grace_period_seconds as i64);
        let now = Utc::now();

        // Find when the artifact entered DeletePending.
        let entered_delete_pending_at = artifact
            .transition_log
            .iter()
            .rev()
            .find(|t| t.to_state == LifecycleState::DeletePending)
            .map(|t| t.transitioned_at)
            .unwrap_or(artifact.last_validated_at);

        entered_delete_pending_at + grace <= now
    }
}

// ---------------------------------------------------------------------------
// Internal helper enum
// ---------------------------------------------------------------------------

/// Outcome of a single deletion attempt.
enum DeletionOutcome {
    /// Deletion was applied (or would be in dry-run).
    Applied,
    /// Deletion was blocked by a safety check.
    Blocked,
    /// An error occurred during the deletion.
    Error,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifacts::catalog::{ArtifactCatalog, InMemoryCatalog};
    use crate::magician_v2::artifacts::policy::{PolicyEngine, ScopeEvent};
    use crate::magician_v2::artifacts::types::{
        ArtifactDomain, ArtifactMetadata, ArtifactReference, CatalogQuery, FreshnessClass,
        LifecycleState, LifecycleTransition, OwnershipScope, PhysicalLocator, PolicyBindings,
        ProducerInfo, ProtectionFlags, RetentionClass, SweepConfig, TransitionReason,
    };
    use chrono::{Duration, Utc};
    use std::collections::HashMap;
    use std::collections::VecDeque;
    use std::sync::Arc;

    // -----------------------------------------------------------------------
    // TestCatalog — thin wrapper around InMemoryCatalog
    // -----------------------------------------------------------------------

    /// Test helper wrapping `InMemoryCatalog` for test isolation.
    struct TestCatalog {
        inner: InMemoryCatalog,
    }

    impl TestCatalog {
        fn new() -> Self {
            Self {
                inner: InMemoryCatalog::new(),
            }
        }

        /// Register an artifact directly for test setup.
        fn register(&self, metadata: ArtifactMetadata) {
            self.inner.register(metadata).expect("test register failed");
        }

        /// Read the current state of an artifact.
        fn get(&self, uid: &str) -> Option<ArtifactMetadata> {
            self.inner.get(uid).expect("test get failed")
        }
    }

    impl ArtifactCatalog for TestCatalog {
        fn register(
            &self,
            metadata: ArtifactMetadata,
        ) -> Result<(), crate::magician_v2::artifacts::catalog::CatalogError> {
            self.inner.register(metadata)
        }

        fn get(
            &self,
            uid: &str,
        ) -> Result<Option<ArtifactMetadata>, crate::magician_v2::artifacts::catalog::CatalogError>
        {
            self.inner.get(uid)
        }

        fn query(
            &self,
            query: &CatalogQuery,
        ) -> Result<Vec<ArtifactMetadata>, crate::magician_v2::artifacts::catalog::CatalogError>
        {
            self.inner.query(query)
        }

        fn update(
            &self,
            uid: &str,
            metadata: ArtifactMetadata,
        ) -> Result<(), crate::magician_v2::artifacts::catalog::CatalogError> {
            self.inner.update(uid, metadata)
        }

        fn remove(
            &self,
            uid: &str,
        ) -> Result<Option<ArtifactMetadata>, crate::magician_v2::artifacts::catalog::CatalogError>
        {
            self.inner.remove(uid)
        }

        fn stats(
            &self,
        ) -> Result<
            crate::magician_v2::artifacts::types::CatalogStats,
            crate::magician_v2::artifacts::catalog::CatalogError,
        > {
            self.inner.stats()
        }

        fn transition(
            &self,
            uid: &str,
            new_state: LifecycleState,
            reason: TransitionReason,
        ) -> Result<(), crate::magician_v2::artifacts::catalog::CatalogError> {
            self.inner.transition(uid, new_state, reason)
        }

        fn add_reference(
            &self,
            uid: &str,
            reference: crate::magician_v2::artifacts::types::ArtifactReference,
        ) -> Result<(), crate::magician_v2::artifacts::catalog::CatalogError> {
            self.inner.add_reference(uid, reference)
        }

        fn release_reference(
            &self,
            uid: &str,
            referrer_id: &str,
        ) -> Result<(), crate::magician_v2::artifacts::catalog::CatalogError> {
            self.inner.release_reference(uid, referrer_id)
        }
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn default_sweep_config() -> SweepConfig {
        SweepConfig {
            max_deletes_per_sweep: 100,
            grace_period_seconds: 60,
            dry_run: false,
            domain_kill_switches: HashMap::new(),
        }
    }

    fn make_scheduler(config: SweepConfig) -> LifecycleScheduler {
        let engine = Arc::new(PolicyEngine::new());
        LifecycleScheduler::new(config, engine)
    }

    fn make_artifact(uid: &str, domain: ArtifactDomain, state: LifecycleState) -> ArtifactMetadata {
        let now = Utc::now();
        ArtifactMetadata {
            artifact_uid: uid.to_string(),
            domain,
            artifact_type: None,
            physical_locator: PhysicalLocator::InMemory {
                key: uid.to_string(),
            },
            route_target: None,
            ownership: OwnershipScope::default(),
            producer: ProducerInfo {
                producer_agent_id: "test-agent".to_string(),
                producer_stage: None,
                produced_at: now,
            },
            policy: PolicyBindings::default(),
            lifecycle_state: state,
            last_validated_at: now,
            expires_at: None,
            references: vec![],
            render_hints: None,
            transition_log: VecDeque::new(),
        }
    }

    fn make_artifact_with_thread(
        uid: &str,
        domain: ArtifactDomain,
        state: LifecycleState,
        thread_id: &str,
    ) -> ArtifactMetadata {
        let mut a = make_artifact(uid, domain, state);
        a.ownership.execution_id = Some(thread_id.to_string());
        a
    }

    // -----------------------------------------------------------------------
    // Test: Mark phase identifies stale artifacts
    // -----------------------------------------------------------------------

    #[test]
    fn mark_identifies_stale_artifacts() {
        let scheduler = make_scheduler(default_sweep_config());
        let now = Utc::now();

        // Workflow domain: fresh_seconds=300, stale_seconds=600.
        // Artifact produced 400s ago should be marked Stale.
        let mut artifact =
            make_artifact("wf-stale", ArtifactDomain::Workflow, LifecycleState::Active);
        artifact.producer.produced_at = now - Duration::seconds(400);
        artifact.last_validated_at = now - Duration::seconds(400);

        let candidates = scheduler.mark(&[&artifact], now);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].artifact_uid, "wf-stale");
        assert_eq!(candidates[0].current_state, LifecycleState::Active);
        assert_eq!(candidates[0].recommended_state, LifecycleState::Stale);
    }

    // -----------------------------------------------------------------------
    // Test: Mark phase skips artifacts already in recommended state
    // -----------------------------------------------------------------------

    #[test]
    fn mark_skips_artifact_already_in_recommended_state() {
        let scheduler = make_scheduler(default_sweep_config());
        let now = Utc::now();

        // Evergreen, episode domain: should stay Active.
        let artifact = make_artifact("ep-ok", ArtifactDomain::Episode, LifecycleState::Active);

        let candidates = scheduler.mark(&[&artifact], now);
        assert!(candidates.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: Kill switch skips domain
    // -----------------------------------------------------------------------

    #[test]
    fn kill_switch_skips_domain() {
        let mut config = default_sweep_config();
        config
            .domain_kill_switches
            .insert("workflow".to_string(), true);

        let scheduler = make_scheduler(config);
        let now = Utc::now();

        // This would normally be stale, but kill switch is on.
        let mut artifact =
            make_artifact("wf-kill", ArtifactDomain::Workflow, LifecycleState::Active);
        artifact.producer.produced_at = now - Duration::seconds(400);
        artifact.last_validated_at = now - Duration::seconds(400);

        let candidates = scheduler.mark(&[&artifact], now);
        assert!(candidates.is_empty());
    }

    #[test]
    fn kill_switch_false_does_not_skip() {
        let mut config = default_sweep_config();
        config
            .domain_kill_switches
            .insert("workflow".to_string(), false);

        let scheduler = make_scheduler(config);
        let now = Utc::now();

        let mut artifact = make_artifact(
            "wf-notkill",
            ArtifactDomain::Workflow,
            LifecycleState::Active,
        );
        artifact.producer.produced_at = now - Duration::seconds(400);
        artifact.last_validated_at = now - Duration::seconds(400);

        let candidates = scheduler.mark(&[&artifact], now);
        assert_eq!(candidates.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Test: Dry-run mode does not apply changes
    // -----------------------------------------------------------------------

    #[test]
    fn dry_run_does_not_apply_changes() {
        let mut config = default_sweep_config();
        config.dry_run = true;

        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();

        let now = Utc::now();
        let mut artifact =
            make_artifact("wf-dry", ArtifactDomain::Workflow, LifecycleState::Active);
        artifact.producer.produced_at = now - Duration::seconds(400);
        artifact.last_validated_at = now - Duration::seconds(400);

        catalog.register(artifact.clone());

        let candidates = scheduler.mark(&[&artifact], now);
        assert!(!candidates.is_empty());

        let result = scheduler.sweep(&candidates, &catalog);

        // Result should report transitions.
        assert!(result.transitions_applied > 0);
        assert!(result.dry_run);

        // But catalog should be unchanged.
        let live = catalog.get("wf-dry").unwrap();
        assert_eq!(live.lifecycle_state, LifecycleState::Active);
        assert!(live.transition_log.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: Max deletes per sweep limit
    // -----------------------------------------------------------------------

    #[test]
    fn max_deletes_per_sweep_limits_transitions() {
        let mut config = default_sweep_config();
        config.max_deletes_per_sweep = 2;

        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        // Create 5 workflow artifacts that will all become stale.
        let mut artifacts = Vec::new();
        for i in 0..5 {
            let mut a = make_artifact(
                &format!("wf-limit-{}", i),
                ArtifactDomain::Workflow,
                LifecycleState::Active,
            );
            a.producer.produced_at = now - Duration::seconds(400);
            a.last_validated_at = now - Duration::seconds(400);
            catalog.register(a.clone());
            artifacts.push(a);
        }

        let refs: Vec<&ArtifactMetadata> = artifacts.iter().collect();
        let candidates = scheduler.mark(&refs, now);
        assert_eq!(candidates.len(), 5);

        let result = scheduler.sweep(&candidates, &catalog);

        // Only 2 should have been applied.
        assert_eq!(result.transitions_applied, 2);
    }

    // -----------------------------------------------------------------------
    // Test: Grace period blocks premature deletion
    // -----------------------------------------------------------------------

    #[test]
    fn grace_period_blocks_premature_deletion() {
        let mut config = default_sweep_config();
        config.grace_period_seconds = 3600; // 1 hour

        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        // Artifact in DeletePending that entered that state only 10 seconds ago.
        let mut artifact = make_artifact(
            "dp-young",
            ArtifactDomain::Execution,
            LifecycleState::DeletePending,
        );
        // Use a TTL policy that has expired, so policy engine will recommend
        // Expired (or possibly keep state). We need to construct this so the
        // candidate recommends Deleted.
        artifact.producer.produced_at = now - Duration::seconds(10000);
        artifact.last_validated_at = now - Duration::seconds(10);
        artifact.policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 100 },
            freshness_class: FreshnessClass::Evergreen,
            ..PolicyBindings::default()
        };
        // Add a transition log entry showing it entered DeletePending 10s ago.
        artifact.transition_log.push_back(LifecycleTransition {
            from_state: LifecycleState::Expired,
            to_state: LifecycleState::DeletePending,
            reason: TransitionReason::SweeperSelected,
            transitioned_at: now - Duration::seconds(10),
        });

        catalog.register(artifact.clone());

        // Manually construct a candidate that wants to delete.
        let candidate = SweepCandidate {
            artifact_uid: "dp-young".to_string(),
            current_state: LifecycleState::DeletePending,
            recommended_state: LifecycleState::Deleted,
            reason: TransitionReason::GracePeriodCompleted,
            deletion_blocked: false,
            domain: ArtifactDomain::Execution,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        // Deletion should be blocked by grace period.
        assert_eq!(result.deletions_performed, 0);
        assert_eq!(result.deletions_blocked, 1);

        // Artifact should still be DeletePending.
        let live = catalog.get("dp-young").unwrap();
        assert_eq!(live.lifecycle_state, LifecycleState::DeletePending);
    }

    // -----------------------------------------------------------------------
    // Test: Grace period allows deletion after elapsed
    // -----------------------------------------------------------------------

    #[test]
    fn grace_period_allows_deletion_when_elapsed() {
        let mut config = default_sweep_config();
        config.grace_period_seconds = 60;

        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let mut artifact = make_artifact(
            "dp-old",
            ArtifactDomain::Execution,
            LifecycleState::DeletePending,
        );
        artifact.producer.produced_at = now - Duration::seconds(10000);
        artifact.last_validated_at = now - Duration::seconds(200);
        artifact.policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 100 },
            freshness_class: FreshnessClass::Evergreen,
            ..PolicyBindings::default()
        };
        // Entered DeletePending 200 seconds ago — well past 60s grace period.
        artifact.transition_log.push_back(LifecycleTransition {
            from_state: LifecycleState::Expired,
            to_state: LifecycleState::DeletePending,
            reason: TransitionReason::SweeperSelected,
            transitioned_at: now - Duration::seconds(200),
        });

        catalog.register(artifact.clone());

        let candidate = SweepCandidate {
            artifact_uid: "dp-old".to_string(),
            current_state: LifecycleState::DeletePending,
            recommended_state: LifecycleState::Deleted,
            reason: TransitionReason::GracePeriodCompleted,
            deletion_blocked: false,
            domain: ArtifactDomain::Execution,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        assert_eq!(result.deletions_performed, 1);
        assert_eq!(result.deletions_blocked, 0);

        let live = catalog.get("dp-old").unwrap();
        assert_eq!(live.lifecycle_state, LifecycleState::Deleted);
    }

    // -----------------------------------------------------------------------
    // Test: Grace period fallback to last_validated_at
    // -----------------------------------------------------------------------

    #[test]
    fn grace_period_falls_back_to_last_validated_at() {
        let mut config = default_sweep_config();
        config.grace_period_seconds = 60;

        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        // DeletePending artifact with NO transition log (edge case).
        let mut artifact = make_artifact(
            "dp-nolog",
            ArtifactDomain::Execution,
            LifecycleState::DeletePending,
        );
        artifact.producer.produced_at = now - Duration::seconds(10000);
        // last_validated_at is 200s ago — past grace period.
        artifact.last_validated_at = now - Duration::seconds(200);
        artifact.policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 100 },
            freshness_class: FreshnessClass::Evergreen,
            ..PolicyBindings::default()
        };
        // No transition_log entries.

        catalog.register(artifact.clone());

        let candidate = SweepCandidate {
            artifact_uid: "dp-nolog".to_string(),
            current_state: LifecycleState::DeletePending,
            recommended_state: LifecycleState::Deleted,
            reason: TransitionReason::GracePeriodCompleted,
            deletion_blocked: false,
            domain: ArtifactDomain::Execution,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        // Should pass because fallback (last_validated_at) is past grace period.
        assert_eq!(result.deletions_performed, 1);
    }

    // -----------------------------------------------------------------------
    // Test: Protected artifacts blocked from deletion
    // -----------------------------------------------------------------------

    #[test]
    fn protected_artifacts_blocked_from_deletion() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let mut artifact = make_artifact(
            "dp-protected",
            ArtifactDomain::Execution,
            LifecycleState::DeletePending,
        );
        artifact.producer.produced_at = now - Duration::seconds(10000);
        artifact.last_validated_at = now - Duration::seconds(200);
        artifact.policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 100 },
            freshness_class: FreshnessClass::Evergreen,
            protection_flags: ProtectionFlags {
                replan_safe: true,
                ..ProtectionFlags::default()
            },
            ..PolicyBindings::default()
        };
        artifact.transition_log.push_back(LifecycleTransition {
            from_state: LifecycleState::Expired,
            to_state: LifecycleState::DeletePending,
            reason: TransitionReason::SweeperSelected,
            transitioned_at: now - Duration::seconds(200),
        });

        catalog.register(artifact.clone());

        let candidate = SweepCandidate {
            artifact_uid: "dp-protected".to_string(),
            current_state: LifecycleState::DeletePending,
            recommended_state: LifecycleState::Deleted,
            reason: TransitionReason::GracePeriodCompleted,
            deletion_blocked: false, // Mark phase did not block, but runtime check will
            domain: ArtifactDomain::Execution,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        assert_eq!(result.deletions_performed, 0);
        assert_eq!(result.deletions_blocked, 1);

        let live = catalog.get("dp-protected").unwrap();
        assert_eq!(live.lifecycle_state, LifecycleState::DeletePending);
    }

    #[test]
    fn legal_hold_blocks_deletion_at_sweep() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let mut artifact = make_artifact(
            "dp-legal",
            ArtifactDomain::Execution,
            LifecycleState::DeletePending,
        );
        artifact.producer.produced_at = now - Duration::seconds(10000);
        artifact.last_validated_at = now - Duration::seconds(200);
        artifact.policy.protection_flags.legal_hold = true;
        artifact.transition_log.push_back(LifecycleTransition {
            from_state: LifecycleState::Expired,
            to_state: LifecycleState::DeletePending,
            reason: TransitionReason::SweeperSelected,
            transitioned_at: now - Duration::seconds(200),
        });

        catalog.register(artifact.clone());

        let candidate = SweepCandidate {
            artifact_uid: "dp-legal".to_string(),
            current_state: LifecycleState::DeletePending,
            recommended_state: LifecycleState::Deleted,
            reason: TransitionReason::GracePeriodCompleted,
            deletion_blocked: false,
            domain: ArtifactDomain::Execution,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        assert_eq!(result.deletions_performed, 0);
        assert_eq!(result.deletions_blocked, 1);
    }

    // -----------------------------------------------------------------------
    // Test: Referenced artifacts blocked from deletion
    // -----------------------------------------------------------------------

    #[test]
    fn referenced_artifacts_blocked_from_deletion() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let mut artifact = make_artifact(
            "dp-refed",
            ArtifactDomain::Execution,
            LifecycleState::DeletePending,
        );
        artifact.producer.produced_at = now - Duration::seconds(10000);
        artifact.last_validated_at = now - Duration::seconds(200);
        artifact.policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 100 },
            freshness_class: FreshnessClass::Evergreen,
            ..PolicyBindings::default()
        };
        artifact.references.push(ArtifactReference {
            referrer_id: "goal-cycle-42".to_string(),
            referrer_type: "goal_cycle".to_string(),
            established_at: now,
        });
        artifact.transition_log.push_back(LifecycleTransition {
            from_state: LifecycleState::Expired,
            to_state: LifecycleState::DeletePending,
            reason: TransitionReason::SweeperSelected,
            transitioned_at: now - Duration::seconds(200),
        });

        catalog.register(artifact.clone());

        let candidate = SweepCandidate {
            artifact_uid: "dp-refed".to_string(),
            current_state: LifecycleState::DeletePending,
            recommended_state: LifecycleState::Deleted,
            reason: TransitionReason::GracePeriodCompleted,
            deletion_blocked: false,
            domain: ArtifactDomain::Execution,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        assert_eq!(result.deletions_performed, 0);
        assert_eq!(result.deletions_blocked, 1);

        let live = catalog.get("dp-refed").unwrap();
        assert_eq!(live.lifecycle_state, LifecycleState::DeletePending);
    }

    // -----------------------------------------------------------------------
    // Test: Sweep applies intermediate transitions
    // -----------------------------------------------------------------------

    #[test]
    fn sweep_applies_intermediate_transitions() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let mut artifact =
            make_artifact("wf-sweep", ArtifactDomain::Workflow, LifecycleState::Active);
        artifact.producer.produced_at = now - Duration::seconds(400);
        artifact.last_validated_at = now - Duration::seconds(400);

        catalog.register(artifact.clone());

        let candidates = scheduler.mark(&[&artifact], now);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].recommended_state, LifecycleState::Stale);

        let result = scheduler.sweep(&candidates, &catalog);

        assert_eq!(result.transitions_applied, 1);
        assert_eq!(result.deletions_performed, 0);

        let live = catalog.get("wf-sweep").unwrap();
        assert_eq!(live.lifecycle_state, LifecycleState::Stale);
    }

    // -----------------------------------------------------------------------
    // Test: Scope cleanup expires matching artifacts
    // -----------------------------------------------------------------------

    #[test]
    fn scope_cleanup_expires_matching_artifacts() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        // Pipeline domain has TaskLifetime retention — should expire on ThreadClosed.
        let a1 = make_artifact_with_thread(
            "pipe-scoped",
            ArtifactDomain::Pipeline,
            LifecycleState::Active,
            "exec-99",
        );
        catalog.register(a1);

        // Episode domain has Permanent retention — should NOT expire.
        let a2 = make_artifact_with_thread(
            "ep-scoped",
            ArtifactDomain::Episode,
            LifecycleState::Active,
            "exec-99",
        );
        catalog.register(a2);

        let event = ScopeEvent::ThreadClosed {
            thread_id: "exec-99".to_string(),
        };

        let result = scheduler.run_scope_cleanup(&catalog, &event, now);

        // The pipeline artifact should have been expired.
        let pipe = catalog.get("pipe-scoped").unwrap();
        assert_eq!(pipe.lifecycle_state, LifecycleState::Expired);

        // The episode artifact should still be Active.
        let ep = catalog.get("ep-scoped").unwrap();
        assert_eq!(ep.lifecycle_state, LifecycleState::Active);

        // Result should reflect at least one transition.
        assert!(result.transitions_applied >= 1);
    }

    // -----------------------------------------------------------------------
    // Test: Scope cleanup does not expire already-expired artifacts again
    // -----------------------------------------------------------------------

    #[test]
    fn scope_cleanup_skips_already_expired() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let a = make_artifact_with_thread(
            "pipe-already-expired",
            ArtifactDomain::Pipeline,
            LifecycleState::Expired,
            "exec-100",
        );
        catalog.register(a);

        let event = ScopeEvent::ThreadClosed {
            thread_id: "exec-100".to_string(),
        };

        let _result = scheduler.run_scope_cleanup(&catalog, &event, now);

        // Should not double-expire.
        let live = catalog.get("pipe-already-expired").unwrap();
        assert_eq!(live.lifecycle_state, LifecycleState::Expired);
        // No new transition_log entry from scope cleanup (only mark+sweep may add).
        // The artifact was already Expired, so scope cleanup phase 1 skips it.
    }

    // -----------------------------------------------------------------------
    // Test: Full sweep processes all non-terminal artifacts
    // -----------------------------------------------------------------------

    #[test]
    fn full_sweep_processes_all_non_terminal() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        // Active workflow artifact that should become stale (400s old).
        let mut wf = make_artifact("full-wf", ArtifactDomain::Workflow, LifecycleState::Active);
        wf.producer.produced_at = now - Duration::seconds(400);
        wf.last_validated_at = now - Duration::seconds(400);
        catalog.register(wf);

        // Active episode artifact (evergreen) — should stay Active.
        let ep = make_artifact("full-ep", ArtifactDomain::Episode, LifecycleState::Active);
        catalog.register(ep);

        // Deleted artifact — should NOT be included.
        let del = make_artifact(
            "full-del",
            ArtifactDomain::Pipeline,
            LifecycleState::Deleted,
        );
        catalog.register(del);

        let result = scheduler.run_full_sweep(&catalog, now);

        // The workflow artifact should have been transitioned.
        let wf_live = catalog.get("full-wf").unwrap();
        assert_eq!(wf_live.lifecycle_state, LifecycleState::Stale);

        // Episode stays active.
        let ep_live = catalog.get("full-ep").unwrap();
        assert_eq!(ep_live.lifecycle_state, LifecycleState::Active);

        // Deleted stays deleted (was not included in sweep).
        let del_live = catalog.get("full-del").unwrap();
        assert_eq!(del_live.lifecycle_state, LifecycleState::Deleted);

        assert!(result.transitions_applied >= 1);
        assert_eq!(result.errors.len(), 0);
    }

    // -----------------------------------------------------------------------
    // Test: Full sweep with empty catalog
    // -----------------------------------------------------------------------

    #[test]
    fn full_sweep_empty_catalog() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let result = scheduler.run_full_sweep(&catalog, now);

        assert_eq!(result.candidates_evaluated, 0);
        assert_eq!(result.transitions_applied, 0);
        assert_eq!(result.deletions_performed, 0);
        assert!(result.errors.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: Dry-run deletion counts but does not delete
    // -----------------------------------------------------------------------

    #[test]
    fn dry_run_deletion_counts_but_no_delete() {
        let mut config = default_sweep_config();
        config.dry_run = true;
        config.grace_period_seconds = 0; // zero grace so it would delete if not dry-run

        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let mut artifact = make_artifact(
            "dp-dry",
            ArtifactDomain::Execution,
            LifecycleState::DeletePending,
        );
        artifact.producer.produced_at = now - Duration::seconds(10000);
        artifact.last_validated_at = now - Duration::seconds(200);
        artifact.policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 100 },
            freshness_class: FreshnessClass::Evergreen,
            ..PolicyBindings::default()
        };
        artifact.transition_log.push_back(LifecycleTransition {
            from_state: LifecycleState::Expired,
            to_state: LifecycleState::DeletePending,
            reason: TransitionReason::SweeperSelected,
            transitioned_at: now - Duration::seconds(200),
        });

        catalog.register(artifact.clone());

        let candidate = SweepCandidate {
            artifact_uid: "dp-dry".to_string(),
            current_state: LifecycleState::DeletePending,
            recommended_state: LifecycleState::Deleted,
            reason: TransitionReason::GracePeriodCompleted,
            deletion_blocked: false,
            domain: ArtifactDomain::Execution,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        assert!(result.dry_run);
        assert_eq!(result.deletions_performed, 1); // counted
        assert_eq!(result.deletions_blocked, 0);

        // But catalog unchanged.
        let live = catalog.get("dp-dry").unwrap();
        assert_eq!(live.lifecycle_state, LifecycleState::DeletePending);
    }

    // -----------------------------------------------------------------------
    // Test: Candidate with deletion_blocked flag is blocked at sweep
    // -----------------------------------------------------------------------

    #[test]
    fn deletion_blocked_flag_from_mark_blocks_sweep() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let mut artifact = make_artifact(
            "dp-flagged",
            ArtifactDomain::Execution,
            LifecycleState::DeletePending,
        );
        artifact.producer.produced_at = now - Duration::seconds(10000);
        artifact.last_validated_at = now - Duration::seconds(200);
        artifact.transition_log.push_back(LifecycleTransition {
            from_state: LifecycleState::Expired,
            to_state: LifecycleState::DeletePending,
            reason: TransitionReason::SweeperSelected,
            transitioned_at: now - Duration::seconds(200),
        });

        catalog.register(artifact.clone());

        let candidate = SweepCandidate {
            artifact_uid: "dp-flagged".to_string(),
            current_state: LifecycleState::DeletePending,
            recommended_state: LifecycleState::Deleted,
            reason: TransitionReason::GracePeriodCompleted,
            deletion_blocked: true, // blocked from mark phase
            domain: ArtifactDomain::Execution,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        assert_eq!(result.deletions_performed, 0);
        assert_eq!(result.deletions_blocked, 1);
    }

    // -----------------------------------------------------------------------
    // Test: Multiple domains in a single sweep
    // -----------------------------------------------------------------------

    #[test]
    fn mixed_domain_sweep() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        // Workflow: should become stale (400s old).
        let mut wf = make_artifact("mix-wf", ArtifactDomain::Workflow, LifecycleState::Active);
        wf.producer.produced_at = now - Duration::seconds(400);
        wf.last_validated_at = now - Duration::seconds(400);
        catalog.register(wf.clone());

        // Episode: evergreen, should stay Active.
        let ep = make_artifact("mix-ep", ArtifactDomain::Episode, LifecycleState::Active);
        catalog.register(ep.clone());

        let candidates = scheduler.mark(&[&wf, &ep], now);
        assert_eq!(candidates.len(), 1); // only the workflow artifact

        let result = scheduler.sweep(&candidates, &catalog);
        assert_eq!(result.transitions_applied, 1);

        let wf_live = catalog.get("mix-wf").unwrap();
        assert_eq!(wf_live.lifecycle_state, LifecycleState::Stale);

        let ep_live = catalog.get("mix-ep").unwrap();
        assert_eq!(ep_live.lifecycle_state, LifecycleState::Active);
    }

    // -----------------------------------------------------------------------
    // Test: Kill switch for one domain, not another
    // -----------------------------------------------------------------------

    #[test]
    fn kill_switch_selective_domain() {
        let mut config = default_sweep_config();
        config
            .domain_kill_switches
            .insert("workflow".to_string(), true);

        let scheduler = make_scheduler(config);
        let now = Utc::now();

        // Workflow: kill switch on — should be skipped.
        let mut wf = make_artifact("ks-wf", ArtifactDomain::Workflow, LifecycleState::Active);
        wf.producer.produced_at = now - Duration::seconds(400);
        wf.last_validated_at = now - Duration::seconds(400);

        // Pipeline: validated long ago, should become stale via
        // RevalidateOnResume (3600s threshold).
        let mut pipe = make_artifact("ks-pipe", ArtifactDomain::Pipeline, LifecycleState::Active);
        pipe.producer.produced_at = now - Duration::seconds(5000);
        pipe.last_validated_at = now - Duration::seconds(5000);

        let candidates = scheduler.mark(&[&wf, &pipe], now);

        // Workflow skipped, pipeline included.
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].artifact_uid, "ks-pipe");
    }

    // -----------------------------------------------------------------------
    // Test: SweepResult accurately reports counts
    // -----------------------------------------------------------------------

    #[test]
    fn sweep_result_counts_are_accurate() {
        let mut config = default_sweep_config();
        config.grace_period_seconds = 0;

        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        // 1. Intermediate transition.
        let mut wf = make_artifact("count-wf", ArtifactDomain::Workflow, LifecycleState::Active);
        wf.producer.produced_at = now - Duration::seconds(400);
        wf.last_validated_at = now - Duration::seconds(400);
        catalog.register(wf);

        // 2. Deletion that should succeed.
        let mut dp_ok = make_artifact(
            "count-dp-ok",
            ArtifactDomain::Execution,
            LifecycleState::DeletePending,
        );
        dp_ok.producer.produced_at = now - Duration::seconds(10000);
        dp_ok.last_validated_at = now - Duration::seconds(200);
        dp_ok.policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 100 },
            freshness_class: FreshnessClass::Evergreen,
            ..PolicyBindings::default()
        };
        dp_ok.transition_log.push_back(LifecycleTransition {
            from_state: LifecycleState::Expired,
            to_state: LifecycleState::DeletePending,
            reason: TransitionReason::SweeperSelected,
            transitioned_at: now - Duration::seconds(200),
        });
        catalog.register(dp_ok);

        // 3. Deletion that should be blocked (has reference).
        let mut dp_blocked = make_artifact(
            "count-dp-blocked",
            ArtifactDomain::Execution,
            LifecycleState::DeletePending,
        );
        dp_blocked.producer.produced_at = now - Duration::seconds(10000);
        dp_blocked.last_validated_at = now - Duration::seconds(200);
        dp_blocked.policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 100 },
            freshness_class: FreshnessClass::Evergreen,
            ..PolicyBindings::default()
        };
        dp_blocked.references.push(ArtifactReference {
            referrer_id: "blocker".to_string(),
            referrer_type: "test".to_string(),
            established_at: now,
        });
        dp_blocked.transition_log.push_back(LifecycleTransition {
            from_state: LifecycleState::Expired,
            to_state: LifecycleState::DeletePending,
            reason: TransitionReason::SweeperSelected,
            transitioned_at: now - Duration::seconds(200),
        });
        catalog.register(dp_blocked);

        let candidates = vec![
            SweepCandidate {
                artifact_uid: "count-wf".to_string(),
                current_state: LifecycleState::Active,
                recommended_state: LifecycleState::Stale,
                reason: TransitionReason::FreshnessExpired,
                deletion_blocked: false,
                domain: ArtifactDomain::Workflow,
            },
            SweepCandidate {
                artifact_uid: "count-dp-ok".to_string(),
                current_state: LifecycleState::DeletePending,
                recommended_state: LifecycleState::Deleted,
                reason: TransitionReason::GracePeriodCompleted,
                deletion_blocked: false,
                domain: ArtifactDomain::Execution,
            },
            SweepCandidate {
                artifact_uid: "count-dp-blocked".to_string(),
                current_state: LifecycleState::DeletePending,
                recommended_state: LifecycleState::Deleted,
                reason: TransitionReason::GracePeriodCompleted,
                deletion_blocked: false,
                domain: ArtifactDomain::Execution,
            },
        ];

        let result = scheduler.sweep(&candidates, &catalog);

        assert_eq!(result.candidates_evaluated, 3);
        assert_eq!(result.transitions_applied, 1);
        assert_eq!(result.deletions_performed, 1);
        assert_eq!(result.deletions_blocked, 1);
        assert!(result.errors.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: Sweep with empty candidates
    // -----------------------------------------------------------------------

    #[test]
    fn sweep_empty_candidates() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();

        let result = scheduler.sweep(&[], &catalog);

        assert_eq!(result.candidates_evaluated, 0);
        assert_eq!(result.transitions_applied, 0);
        assert_eq!(result.deletions_performed, 0);
        assert_eq!(result.deletions_blocked, 0);
        assert!(result.errors.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: Mark with empty artifact list
    // -----------------------------------------------------------------------

    #[test]
    fn mark_empty_artifacts() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let now = Utc::now();

        let candidates = scheduler.mark(&[], now);
        assert!(candidates.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: Scope cleanup with RunCompleted event
    // -----------------------------------------------------------------------

    #[test]
    fn scope_cleanup_run_completed() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        // Workflow domain has RunLifetime retention.
        let mut wf = make_artifact("wf-run", ArtifactDomain::Workflow, LifecycleState::Active);
        wf.ownership.workflow_instance_id = Some("run-42".to_string());
        catalog.register(wf);

        let event = ScopeEvent::RunCompleted {
            run_id: "run-42".to_string(),
        };

        let result = scheduler.run_scope_cleanup(&catalog, &event, now);

        let live = catalog.get("wf-run").unwrap();
        assert_eq!(live.lifecycle_state, LifecycleState::Expired);
        assert!(result.transitions_applied >= 1);
    }

    // -----------------------------------------------------------------------
    // Test: Expired workflow artifact detected in full sweep
    // -----------------------------------------------------------------------

    #[test]
    fn full_sweep_detects_expired_workflow() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        // Workflow with 300s fresh, 600s stale. Produced 700s ago -> Expired.
        let mut wf = make_artifact("wf-exp", ArtifactDomain::Workflow, LifecycleState::Active);
        wf.producer.produced_at = now - Duration::seconds(700);
        wf.last_validated_at = now - Duration::seconds(700);
        catalog.register(wf);

        let result = scheduler.run_full_sweep(&catalog, now);

        let live = catalog.get("wf-exp").unwrap();
        assert_eq!(live.lifecycle_state, LifecycleState::Expired);
        assert!(result.transitions_applied >= 1);
    }

    // -----------------------------------------------------------------------
    // Test: Scope cleanup does not affect unrelated threads
    // -----------------------------------------------------------------------

    #[test]
    fn scope_cleanup_does_not_affect_other_threads() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let a1 = make_artifact_with_thread(
            "t1-art",
            ArtifactDomain::Pipeline,
            LifecycleState::Active,
            "exec-A",
        );
        let a2 = make_artifact_with_thread(
            "t2-art",
            ArtifactDomain::Pipeline,
            LifecycleState::Active,
            "exec-B",
        );
        catalog.register(a1);
        catalog.register(a2);

        let event = ScopeEvent::ThreadClosed {
            thread_id: "exec-A".to_string(),
        };

        scheduler.run_scope_cleanup(&catalog, &event, now);

        // exec-A artifact should be expired.
        let live_a = catalog.get("t1-art").unwrap();
        assert_eq!(live_a.lifecycle_state, LifecycleState::Expired);

        // exec-B artifact should be untouched.
        let live_b = catalog.get("t2-art").unwrap();
        assert_eq!(live_b.lifecycle_state, LifecycleState::Active);
    }

    // -----------------------------------------------------------------------
    // Test: Quarantined artifacts included in full sweep
    // -----------------------------------------------------------------------

    #[test]
    fn full_sweep_includes_quarantined() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();
        let now = Utc::now();

        let q = make_artifact(
            "q-art",
            ArtifactDomain::Execution,
            LifecycleState::Quarantined,
        );
        catalog.register(q);

        let result = scheduler.run_full_sweep(&catalog, now);

        // Quarantined is a non-terminal state, so it should be queried.
        // The policy engine may or may not recommend a transition depending
        // on the artifact details. The key point is no error.
        assert!(result.errors.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: Deletion of artifact not found in catalog during sweep
    // -----------------------------------------------------------------------

    #[test]
    fn deletion_of_missing_artifact_records_error() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();

        // Candidate references an artifact not in the catalog.
        let candidate = SweepCandidate {
            artifact_uid: "ghost".to_string(),
            current_state: LifecycleState::DeletePending,
            recommended_state: LifecycleState::Deleted,
            reason: TransitionReason::GracePeriodCompleted,
            deletion_blocked: false,
            domain: ArtifactDomain::Execution,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        assert_eq!(result.deletions_performed, 0);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("ghost"));
    }

    // -----------------------------------------------------------------------
    // Test: Intermediate transition for missing artifact records error
    // -----------------------------------------------------------------------

    #[test]
    fn intermediate_transition_missing_artifact_records_error() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();

        let candidate = SweepCandidate {
            artifact_uid: "no-such".to_string(),
            current_state: LifecycleState::Active,
            recommended_state: LifecycleState::Stale,
            reason: TransitionReason::FreshnessExpired,
            deletion_blocked: false,
            domain: ArtifactDomain::Workflow,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        assert_eq!(result.transitions_applied, 0);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("no-such"));
    }

    // -----------------------------------------------------------------------
    // Test: Concurrent sweep returns "already in progress"
    // -----------------------------------------------------------------------

    #[test]
    fn concurrent_sweep_returns_already_in_progress() {
        let config = default_sweep_config();
        let engine = Arc::new(PolicyEngine::new());
        let scheduler = Arc::new(LifecycleScheduler::new(config, engine));
        let catalog = Arc::new(TestCatalog::new());

        // Hold the sweep lock manually to simulate a concurrent sweep.
        let _guard = scheduler.sweep_lock.lock().unwrap();

        // A second sweep attempt should return an error.
        let now = Utc::now();
        let result = scheduler.run_full_sweep(catalog.as_ref(), now);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("already in progress"));
        assert_eq!(result.candidates_evaluated, 0);
    }

    // -----------------------------------------------------------------------
    // Test: retry_candidates populated on catalog error during deletion
    // -----------------------------------------------------------------------

    #[test]
    fn retry_candidates_populated_on_deletion_error() {
        let config = default_sweep_config();
        let scheduler = make_scheduler(config);
        let catalog = TestCatalog::new();

        // Candidate references an artifact not in the catalog — deletion
        // will fail with a "not found" error.
        let candidate = SweepCandidate {
            artifact_uid: "ghost-retry".to_string(),
            current_state: LifecycleState::DeletePending,
            recommended_state: LifecycleState::Deleted,
            reason: TransitionReason::GracePeriodCompleted,
            deletion_blocked: false,
            domain: ArtifactDomain::Execution,
        };

        let result = scheduler.sweep(&[candidate], &catalog);

        assert_eq!(result.deletions_performed, 0);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.retry_candidates.len(), 1);
        assert_eq!(result.retry_candidates[0], "ghost-retry");
    }
}
