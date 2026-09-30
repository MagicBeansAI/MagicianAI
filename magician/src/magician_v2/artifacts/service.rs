//! # Artifact Lifecycle Service
//!
//! Facade wrapping the catalog, policy engine, sanitization gateway, and
//! lifecycle scheduler into a single injectable service.
//!
//! This service is the primary integration point for REST API handlers and
//! production write-path hooks. It provides a simplified interface that
//! delegates to the underlying lifecycle components while keeping the
//! coordination logic in one place.

use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;

use super::catalog::{ArtifactCatalog, CatalogError, InMemoryCatalog};
use super::policy::{PolicyEngine, ScopeEvent};
use super::sanitization::{SanitizationError, SanitizationGateway};
use super::scheduler::LifecycleScheduler;
use super::types::{
    ArtifactMetadata, ArtifactProjection, ArtifactReference, CatalogQuery, CatalogStats,
    LifecycleDecision, LifecycleState, ProjectionSurface, SweepConfig, SweepResult,
    TransitionReason,
};

// ---------------------------------------------------------------------------
// LifecycleServiceError
// ---------------------------------------------------------------------------

/// Errors that can occur during lifecycle service operations.
#[derive(Debug)]
pub enum LifecycleServiceError {
    /// An error originating from the artifact catalog.
    Catalog(CatalogError),
    /// An error originating from the sanitization gateway.
    Sanitization(SanitizationError),
    /// The requested artifact was not found in the catalog.
    NotFound { uid: String },
}

impl std::fmt::Display for LifecycleServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleServiceError::Catalog(e) => write!(f, "catalog error: {}", e),
            LifecycleServiceError::Sanitization(e) => write!(f, "sanitization error: {}", e),
            LifecycleServiceError::NotFound { uid } => {
                write!(f, "artifact not found: {}", uid)
            },
        }
    }
}

impl std::error::Error for LifecycleServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LifecycleServiceError::Catalog(e) => Some(e),
            LifecycleServiceError::Sanitization(e) => Some(e),
            LifecycleServiceError::NotFound { .. } => None,
        }
    }
}

impl From<CatalogError> for LifecycleServiceError {
    fn from(err: CatalogError) -> Self {
        LifecycleServiceError::Catalog(err)
    }
}

impl From<SanitizationError> for LifecycleServiceError {
    fn from(err: SanitizationError) -> Self {
        LifecycleServiceError::Sanitization(err)
    }
}

// ---------------------------------------------------------------------------
// LifecycleService
// ---------------------------------------------------------------------------

/// Facade service that coordinates all artifact lifecycle components.
///
/// Holds shared references to the catalog (trait object for flexibility),
/// policy engine, sanitization gateway, and lifecycle scheduler. Designed
/// to be wrapped in an `Arc` and injected into API handlers.
pub struct LifecycleService {
    /// The artifact catalog (trait object for backend flexibility).
    pub catalog: Arc<dyn ArtifactCatalog>,
    /// The policy engine used for lifecycle evaluations.
    pub policy_engine: Arc<PolicyEngine>,
    /// The sanitization gateway for building surface-specific projections.
    pub sanitization: SanitizationGateway,
    /// The lifecycle scheduler for sweep and scope-cleanup operations.
    pub scheduler: LifecycleScheduler,
    /// Counter for opportunistic sweep triggering.
    registration_count: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for LifecycleService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifecycleService")
            .field(
                "registration_count",
                &self
                    .registration_count
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl LifecycleService {
    /// Create a new `LifecycleService` with safe defaults.
    ///
    /// Uses an in-memory catalog, the canonical policy engine, the default
    /// sanitization gateway, and a sweep configuration with `dry_run = true`
    /// for safety.
    pub fn new() -> Self {
        Self::with_catalog(Arc::new(InMemoryCatalog::new()))
    }

    /// Create a `LifecycleService` backed by the supplied catalog.
    ///
    /// Sweep runs in **dry-run mode** — transitions are evaluated but not applied.
    /// Use [`with_catalog_live`] for production where sweeps should take effect.
    pub fn with_catalog(catalog: Arc<dyn ArtifactCatalog>) -> Self {
        Self::build(catalog, true)
    }

    /// Create a production `LifecycleService` with live sweeps enabled.
    ///
    /// Same as [`with_catalog`] but sweep `dry_run` is `false` — transitions
    /// and deletions are applied for real.
    pub fn with_catalog_live(catalog: Arc<dyn ArtifactCatalog>) -> Self {
        Self::build(catalog, false)
    }

    fn build(catalog: Arc<dyn ArtifactCatalog>, dry_run: bool) -> Self {
        let policy_engine = Arc::new(PolicyEngine::new());
        let sanitization = SanitizationGateway::new();
        let sweep_config = SweepConfig {
            dry_run,
            ..SweepConfig::default()
        };
        let scheduler = LifecycleScheduler::new(sweep_config, Arc::clone(&policy_engine));

        Self {
            catalog,
            policy_engine,
            sanitization,
            scheduler,
            registration_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    // -----------------------------------------------------------------------
    // Catalog operations
    // -----------------------------------------------------------------------

    /// Register a new artifact in the catalog.
    ///
    /// Delegates directly to the underlying catalog's `register` method.
    pub fn register(&self, metadata: ArtifactMetadata) -> Result<(), CatalogError> {
        self.catalog.register(metadata)
    }

    /// Look up an artifact by its UID.
    ///
    /// Returns `Ok(None)` if the artifact does not exist.
    pub fn get(&self, uid: &str) -> Result<Option<ArtifactMetadata>, CatalogError> {
        self.catalog.get(uid)
    }

    /// Remove an artifact from the catalog.
    ///
    /// Primarily used for rollback when a caller must undo a registration claim
    /// after a later persistence step fails.
    pub fn remove(&self, uid: &str) -> Result<Option<ArtifactMetadata>, CatalogError> {
        self.catalog.remove(uid)
    }

    /// Query the catalog with filters and pagination.
    pub fn list(&self, query: &CatalogQuery) -> Result<Vec<ArtifactMetadata>, CatalogError> {
        self.catalog.query(query)
    }

    /// Compute aggregate statistics over all registered artifacts.
    pub fn stats(&self) -> Result<CatalogStats, CatalogError> {
        self.catalog.stats()
    }

    // -----------------------------------------------------------------------
    // Projection
    // -----------------------------------------------------------------------

    /// Get a sanitized projection of an artifact for a specific surface.
    ///
    /// Fetches the artifact metadata from the catalog, then delegates to the
    /// sanitization gateway to build the projection.
    pub fn get_projection(
        &self,
        uid: &str,
        surface: ProjectionSurface,
        content: &Value,
    ) -> Result<ArtifactProjection, LifecycleServiceError> {
        let metadata = self
            .catalog
            .get(uid)?
            .ok_or_else(|| LifecycleServiceError::NotFound {
                uid: uid.to_string(),
            })?;

        let projection = self.sanitization.project(&metadata, content, surface)?;
        Ok(projection)
    }

    // -----------------------------------------------------------------------
    // Lifecycle transitions
    // -----------------------------------------------------------------------

    /// Mark an artifact as stale via operator action.
    ///
    /// Transitions the artifact to `LifecycleState::Stale` with
    /// `TransitionReason::OperatorAction`.
    pub fn mark_stale(&self, uid: &str) -> Result<(), CatalogError> {
        self.catalog.transition(
            uid,
            LifecycleState::Stale,
            TransitionReason::OperatorAction {
                reason: "marked stale by operator".to_string(),
            },
        )
    }

    /// Mark an artifact as revalidated, transitioning it back to active.
    ///
    /// Transitions the artifact to `LifecycleState::Active` with
    /// `TransitionReason::Revalidated`.
    pub fn mark_revalidated(&self, uid: &str) -> Result<(), CatalogError> {
        self.catalog
            .transition(uid, LifecycleState::Active, TransitionReason::Revalidated)
    }

    /// Transition an artifact to the Superseded state.
    ///
    /// Used when a newer version of the same logical artifact replaces this one.
    /// The artifact is retained for audit but is no longer eligible for
    /// consumption or display.
    pub fn transition_to_superseded(&self, uid: &str) -> Result<(), CatalogError> {
        self.catalog.transition(
            uid,
            LifecycleState::Superseded,
            TransitionReason::Superseded,
        )
    }

    // -----------------------------------------------------------------------
    // Reference management
    // -----------------------------------------------------------------------

    /// Add a reference from another entity to an artifact.
    ///
    /// Delegates to the catalog's atomic add_reference operation.
    pub fn add_reference(
        &self,
        uid: &str,
        reference: ArtifactReference,
    ) -> Result<(), CatalogError> {
        self.catalog.add_reference(uid, reference)
    }

    /// Release a reference by referrer_id from an artifact.
    ///
    /// Delegates to the catalog's atomic release_reference operation.
    pub fn release_reference(&self, uid: &str, referrer_id: &str) -> Result<(), CatalogError> {
        self.catalog.release_reference(uid, referrer_id)
    }

    // -----------------------------------------------------------------------
    // Freshness check
    // -----------------------------------------------------------------------

    /// Check if an artifact is consumable (Active state in the lifecycle catalog).
    ///
    /// Returns `true` if: artifact not in catalog, or state is consumable.
    /// Returns `false` only when the catalog explicitly says the artifact is
    /// Stale/Expired/Quarantined/etc. Fail-open: if the catalog is unavailable,
    /// allow consumption.
    pub fn is_consumable(&self, uid: &str) -> bool {
        match self.catalog.get(uid) {
            Ok(Some(meta)) => meta.lifecycle_state.is_consumable(),
            _ => true,
        }
    }

    // -----------------------------------------------------------------------
    // Policy evaluation
    // -----------------------------------------------------------------------

    /// Evaluate the lifecycle policy for a single artifact.
    ///
    /// Fetches the artifact metadata from the catalog and evaluates it
    /// against the policy engine at the current time.
    pub fn evaluate(&self, uid: &str) -> Result<LifecycleDecision, LifecycleServiceError> {
        let metadata = self
            .catalog
            .get(uid)?
            .ok_or_else(|| LifecycleServiceError::NotFound {
                uid: uid.to_string(),
            })?;

        let now = Utc::now();
        let decision = self.policy_engine.evaluate(&metadata, now);
        Ok(decision)
    }

    // -----------------------------------------------------------------------
    // Cleanup operations
    // -----------------------------------------------------------------------

    /// React to a scope closure event by running scope-specific cleanup.
    ///
    /// Delegates to the lifecycle scheduler's `run_scope_cleanup` method.
    pub fn request_cleanup(&self, event: ScopeEvent) -> SweepResult {
        let now = Utc::now();
        self.scheduler
            .run_scope_cleanup(self.catalog.as_ref(), &event, now)
    }

    /// Run a full lifecycle sweep over all non-terminal artifacts in the
    /// catalog.
    ///
    /// Delegates to the lifecycle scheduler's `run_full_sweep` method.
    pub fn run_sweep(&self) -> SweepResult {
        let now = Utc::now();
        self.scheduler.run_full_sweep(self.catalog.as_ref(), now)
    }

    // -----------------------------------------------------------------------
    // Fire-and-forget registration
    // -----------------------------------------------------------------------

    /// Fire-and-forget registration for integration hooks.
    ///
    /// Attempts to register the artifact metadata in the catalog. If
    /// registration fails, the error is logged but not propagated. This is
    /// intended for use in production write-path hooks where the caller
    /// should not be blocked by catalog errors.
    pub fn try_register(&self, metadata: ArtifactMetadata) {
        let uid = metadata.artifact_uid.clone();
        if let Err(e) = self.catalog.register(metadata) {
            tracing::warn!(
                artifact_uid = %uid,
                error = %e,
                "try_register: failed to register artifact (non-fatal)"
            );
            return;
        }

        // Opportunistic sweep every 50 registrations.
        // fetch_add returns the previous value, so add 1 to get the current count.
        let count = self
            .registration_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if count.is_multiple_of(50) {
            tracing::debug!(registration_count = count, "opportunistic sweep triggered");
            let _ = self.run_sweep();
        }
    }
}

// ---------------------------------------------------------------------------
// LifecycleBackgroundSweep
// ---------------------------------------------------------------------------

/// Background sweep task that periodically runs lifecycle evaluation.
///
/// Spawns a tokio task that runs `LifecycleService::run_sweep()` on a
/// configurable interval. The task holds a `Weak<LifecycleService>` so it
/// automatically stops when the service is dropped.
pub struct LifecycleBackgroundSweep {
    cancel: tokio::sync::watch::Sender<bool>,
}

impl LifecycleBackgroundSweep {
    /// Start a background sweep task.
    ///
    /// `interval_secs`: How often to run the sweep (e.g., 300 = every 5 min).
    /// Returns a handle that cancels the task on drop.
    pub fn start(service: Arc<LifecycleService>, interval_secs: u64) -> Self {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let weak = Arc::downgrade(&service);

        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
            interval.tick().await; // skip immediate first tick

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let Some(svc) = weak.upgrade() else {
                            tracing::info!("lifecycle background sweep: service dropped, stopping");
                            break;
                        };
                        let result = svc.run_sweep();
                        tracing::info!(
                            candidates = result.candidates_evaluated,
                            transitions = result.transitions_applied,
                            deletions = result.deletions_performed,
                            blocked = result.deletions_blocked,
                            errors = result.errors.len(),
                            dry_run = result.dry_run,
                            "lifecycle background sweep completed"
                        );
                    }
                    _ = cancel_rx.changed() => {
                        tracing::info!("lifecycle background sweep: cancelled");
                        break;
                    }
                }
            }
        });

        Self { cancel: cancel_tx }
    }

    /// Stop the background sweep task.
    pub fn stop(self) {
        let _ = self.cancel.send(true);
    }
}

impl Drop for LifecycleBackgroundSweep {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifacts::types::{
        ArtifactDomain, OwnershipScope, PhysicalLocator, PolicyBindings, ProducerInfo,
    };
    use chrono::Utc;
    use serde_json::json;
    use std::collections::VecDeque;

    // -----------------------------------------------------------------------
    // Test helper: build a minimal valid ArtifactMetadata
    // -----------------------------------------------------------------------

    fn make_metadata(uid: &str, domain: ArtifactDomain) -> ArtifactMetadata {
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
                produced_at: Utc::now(),
            },
            policy: PolicyBindings::default(),
            lifecycle_state: LifecycleState::Active,
            last_validated_at: Utc::now(),
            expires_at: None,
            references: vec![],
            render_hints: None,
            transition_log: VecDeque::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Test: new() creates a service with working defaults
    // -----------------------------------------------------------------------

    #[test]
    fn new_creates_service_with_working_defaults() {
        let service = LifecycleService::new();

        // The scheduler should be in dry-run mode for safety.
        assert!(service.scheduler.config.dry_run);

        // Stats should return an empty catalog.
        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 0);
    }

    // -----------------------------------------------------------------------
    // Test: register + get roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn register_and_get_roundtrip() {
        let service = LifecycleService::new();
        let meta = make_metadata("svc-art-1", ArtifactDomain::Pipeline);

        service.register(meta).unwrap();

        let retrieved = service.get("svc-art-1").unwrap().expect("should exist");
        assert_eq!(retrieved.artifact_uid, "svc-art-1");
        assert_eq!(retrieved.domain, ArtifactDomain::Pipeline);
        assert_eq!(retrieved.lifecycle_state, LifecycleState::Active);
    }

    // -----------------------------------------------------------------------
    // Test: mark_stale transitions state
    // -----------------------------------------------------------------------

    #[test]
    fn mark_stale_transitions_state() {
        let service = LifecycleService::new();
        let meta = make_metadata("svc-stale-1", ArtifactDomain::Workflow);
        service.register(meta).unwrap();

        service.mark_stale("svc-stale-1").unwrap();

        let updated = service.get("svc-stale-1").unwrap().unwrap();
        assert_eq!(updated.lifecycle_state, LifecycleState::Stale);
        assert_eq!(updated.transition_log.len(), 1);
        assert_eq!(updated.transition_log[0].from_state, LifecycleState::Active);
        assert_eq!(updated.transition_log[0].to_state, LifecycleState::Stale);
    }

    // -----------------------------------------------------------------------
    // Test: mark_revalidated transitions back to active
    // -----------------------------------------------------------------------

    #[test]
    fn mark_revalidated_transitions_to_active() {
        let service = LifecycleService::new();
        let meta = make_metadata("svc-reval-1", ArtifactDomain::Execution);
        service.register(meta).unwrap();

        // First mark stale, then revalidate.
        service.mark_stale("svc-reval-1").unwrap();
        service.mark_revalidated("svc-reval-1").unwrap();

        let updated = service.get("svc-reval-1").unwrap().unwrap();
        assert_eq!(updated.lifecycle_state, LifecycleState::Active);
        assert_eq!(updated.transition_log.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Test: stats returns correct counts
    // -----------------------------------------------------------------------

    #[test]
    fn stats_returns_correct_counts() {
        let service = LifecycleService::new();

        service
            .register(make_metadata("stats-1", ArtifactDomain::Pipeline))
            .unwrap();
        service
            .register(make_metadata("stats-2", ArtifactDomain::Workflow))
            .unwrap();
        service
            .register(make_metadata("stats-3", ArtifactDomain::Episode))
            .unwrap();

        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 3);
        assert_eq!(stats.by_domain.get("pipeline"), Some(&1));
        assert_eq!(stats.by_domain.get("workflow"), Some(&1));
        assert_eq!(stats.by_domain.get("episode"), Some(&1));
        assert_eq!(stats.by_state.get("active"), Some(&3));
    }

    // -----------------------------------------------------------------------
    // Test: try_register doesn't panic on error
    // -----------------------------------------------------------------------

    #[test]
    fn try_register_does_not_panic_on_error() {
        let service = LifecycleService::new();
        let meta = make_metadata("try-reg-1", ArtifactDomain::Pipeline);

        // First registration succeeds.
        service.try_register(meta.clone());
        assert!(service.get("try-reg-1").unwrap().is_some());

        // Duplicate registration would fail, but try_register should not panic.
        service.try_register(meta);

        // Catalog should still have exactly one entry with that UID.
        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 1);
    }

    // -----------------------------------------------------------------------
    // Test: try_register with invalid metadata doesn't panic
    // -----------------------------------------------------------------------

    #[test]
    fn try_register_with_invalid_metadata_does_not_panic() {
        let service = LifecycleService::new();

        // Empty UID should fail validation, but try_register absorbs the error.
        let invalid = make_metadata("", ArtifactDomain::Pipeline);
        service.try_register(invalid);

        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 0);
    }

    // -----------------------------------------------------------------------
    // Test: evaluate returns a valid decision
    // -----------------------------------------------------------------------

    #[test]
    fn evaluate_returns_valid_decision() {
        let service = LifecycleService::new();
        let meta = make_metadata("eval-1", ArtifactDomain::Episode);
        service.register(meta).unwrap();

        let decision = service.evaluate("eval-1").unwrap();

        assert_eq!(decision.artifact_uid, "eval-1");
        assert_eq!(decision.current_state, LifecycleState::Active);
        // Episode domain is evergreen, so it should recommend Active.
        assert_eq!(decision.recommended_state, LifecycleState::Active);
        assert!(!decision.explanation.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: is_consumable freshness check
    // -----------------------------------------------------------------------

    #[test]
    fn is_consumable_returns_true_for_active_artifact() {
        let service = LifecycleService::new();
        let meta = make_metadata("consumable-1", ArtifactDomain::Pipeline);
        service.register(meta).unwrap();

        assert!(service.is_consumable("consumable-1"));
    }

    #[test]
    fn is_consumable_returns_false_for_stale_artifact() {
        let service = LifecycleService::new();
        let meta = make_metadata("consumable-stale-1", ArtifactDomain::Pipeline);
        service.register(meta).unwrap();
        service.mark_stale("consumable-stale-1").unwrap();

        assert!(!service.is_consumable("consumable-stale-1"));
    }

    #[test]
    fn is_consumable_returns_true_for_unknown_artifact() {
        let service = LifecycleService::new();
        // Artifact not in catalog — fail-open
        assert!(service.is_consumable("nonexistent-uid"));
    }

    // -----------------------------------------------------------------------
    // Test: evaluate returns NotFound for missing artifact
    // -----------------------------------------------------------------------

    #[test]
    fn evaluate_returns_not_found_for_missing_artifact() {
        let service = LifecycleService::new();

        let result = service.evaluate("nonexistent");
        assert!(result.is_err());
        match result.unwrap_err() {
            LifecycleServiceError::NotFound { uid } => {
                assert_eq!(uid, "nonexistent");
            },
            other => panic!("expected NotFound, got: {}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Test: get_projection works for valid artifact
    // -----------------------------------------------------------------------

    #[test]
    fn get_projection_works_for_valid_artifact() {
        let service = LifecycleService::new();
        let meta = make_metadata("proj-1", ArtifactDomain::Episode);
        service.register(meta).unwrap();

        let content = json!({"summary": "test data", "count": 42});
        let projection = service
            .get_projection("proj-1", ProjectionSurface::InternalSanitized, &content)
            .unwrap();

        assert_eq!(projection.artifact_uid, "proj-1");
        assert_eq!(projection.surface, ProjectionSurface::InternalSanitized);
        assert_eq!(projection.content["summary"], "test data");
        assert_eq!(projection.content["count"], 42);
    }

    // -----------------------------------------------------------------------
    // Test: get_projection returns NotFound for missing artifact
    // -----------------------------------------------------------------------

    #[test]
    fn get_projection_returns_not_found_for_missing_artifact() {
        let service = LifecycleService::new();
        let content = json!({"data": 1});

        let result = service.get_projection("missing", ProjectionSurface::InternalRaw, &content);
        assert!(result.is_err());
        match result.unwrap_err() {
            LifecycleServiceError::NotFound { uid } => {
                assert_eq!(uid, "missing");
            },
            other => panic!("expected NotFound, got: {}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Test: list delegates to catalog query
    // -----------------------------------------------------------------------

    #[test]
    fn list_delegates_to_catalog_query() {
        let service = LifecycleService::new();

        service
            .register(make_metadata("list-1", ArtifactDomain::Pipeline))
            .unwrap();
        service
            .register(make_metadata("list-2", ArtifactDomain::Workflow))
            .unwrap();
        service
            .register(make_metadata("list-3", ArtifactDomain::Pipeline))
            .unwrap();

        let query = CatalogQuery {
            domain: Some(ArtifactDomain::Pipeline),
            ..Default::default()
        };
        let results = service.list(&query).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|m| m.domain == ArtifactDomain::Pipeline));
    }

    // -----------------------------------------------------------------------
    // Test: run_sweep executes without errors on empty catalog
    // -----------------------------------------------------------------------

    #[test]
    fn run_sweep_on_empty_catalog() {
        let service = LifecycleService::new();
        let result = service.run_sweep();

        assert_eq!(result.candidates_evaluated, 0);
        assert_eq!(result.transitions_applied, 0);
        assert!(result.errors.is_empty());
        assert!(result.dry_run);
    }

    // -----------------------------------------------------------------------
    // Test: request_cleanup handles scope event
    // -----------------------------------------------------------------------

    #[test]
    fn request_cleanup_handles_scope_event() {
        let service = LifecycleService::new();

        let event = ScopeEvent::GoalCompleted {
            goal_id: "goal-42".to_string(),
        };

        // Should not panic even with an empty catalog.
        let result = service.request_cleanup(event);
        assert!(result.errors.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: service-level add/release reference roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn add_and_release_reference_roundtrip() {
        use crate::magician_v2::artifacts::types::ArtifactReference;

        let service = LifecycleService::new();
        let meta = make_metadata("ref-svc-1", ArtifactDomain::Pipeline);
        service.register(meta).unwrap();

        let reference = ArtifactReference {
            referrer_id: "chain-test".to_string(),
            referrer_type: "pipeline_chain".to_string(),
            established_at: Utc::now(),
        };
        service.add_reference("ref-svc-1", reference).unwrap();

        let meta = service.get("ref-svc-1").unwrap().unwrap();
        assert_eq!(meta.ref_count(), 1);

        service
            .release_reference("ref-svc-1", "chain-test")
            .unwrap();

        let meta = service.get("ref-svc-1").unwrap().unwrap();
        assert_eq!(meta.ref_count(), 0);
    }

    // -----------------------------------------------------------------------
    // Test: LifecycleServiceError Display implementations
    // -----------------------------------------------------------------------

    #[test]
    fn error_display_catalog() {
        let err = LifecycleServiceError::Catalog(CatalogError::NotFound {
            uid: "x".to_string(),
        });
        let msg = format!("{}", err);
        assert!(msg.contains("catalog error"));
        assert!(msg.contains("not found"));
    }

    #[test]
    fn error_display_sanitization() {
        let err = LifecycleServiceError::Sanitization(SanitizationError::InvalidContent {
            reason: "bad json".to_string(),
        });
        let msg = format!("{}", err);
        assert!(msg.contains("sanitization error"));
        assert!(msg.contains("bad json"));
    }

    #[test]
    fn error_display_not_found() {
        let err = LifecycleServiceError::NotFound {
            uid: "missing-uid".to_string(),
        };
        let msg = format!("{}", err);
        assert!(msg.contains("artifact not found"));
        assert!(msg.contains("missing-uid"));
    }

    // -----------------------------------------------------------------------
    // Test: From impls for LifecycleServiceError
    // -----------------------------------------------------------------------

    #[test]
    fn from_catalog_error() {
        let catalog_err = CatalogError::Internal {
            message: "oops".to_string(),
        };
        let service_err: LifecycleServiceError = catalog_err.into();
        match service_err {
            LifecycleServiceError::Catalog(_) => {},
            other => panic!("expected Catalog variant, got: {}", other),
        }
    }

    #[test]
    fn from_sanitization_error() {
        let san_err = SanitizationError::InvalidContent {
            reason: "bad".to_string(),
        };
        let service_err: LifecycleServiceError = san_err.into();
        match service_err {
            LifecycleServiceError::Sanitization(_) => {},
            other => panic!("expected Sanitization variant, got: {}", other),
        }
    }
}

// ===========================================================================
// Integration Tests — full lifecycle flow
// ===========================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod integration_tests {
    use std::collections::VecDeque;
    use std::sync::Arc;

    use chrono::{Duration, Utc};
    use serde_json::json;

    use crate::magician_v2::agents::memory::EpisodeOutcome;
    use crate::magician_v2::agents::types::{ArtifactProvenance, StepArtifact};
    use crate::magician_v2::artifact_v2::memory::V3EpisodeRecord;
    use crate::magician_v2::artifacts::{
        bridge,
        catalog::InMemoryCatalog,
        policy::{PolicyEngine, ScopeEvent},
        sanitization::SanitizationGateway,
        scheduler::LifecycleScheduler,
        service::LifecycleService,
        types::{
            ArtifactDomain, ArtifactMetadata, CatalogQuery, CutoverFence, LifecycleState,
            OwnershipScope, PhysicalLocator, PolicyBindings, ProducerInfo, ProjectionSurface,
            SweepConfig,
        },
    };

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn make_metadata(uid: &str, domain: ArtifactDomain) -> ArtifactMetadata {
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
                produced_at: Utc::now(),
            },
            policy: PolicyBindings::default(),
            lifecycle_state: LifecycleState::Active,
            last_validated_at: Utc::now(),
            expires_at: None,
            references: vec![],
            render_hints: None,
            transition_log: VecDeque::new(),
        }
    }

    fn make_episode(with_artifact: bool) -> V3EpisodeRecord {
        let completed_at = Utc::now();
        V3EpisodeRecord::new_memory_episode(
            Some(("anonymous", "default")),
            "agent-integ",
            "ep-integ-1",
            "goal-integ",
            "manual",
            1,
            completed_at,
            None,
            completed_at,
            completed_at,
            &EpisodeOutcome::GoalAchieved {
                summary: "integration test".to_string(),
            },
            vec![],
            vec![],
            vec![],
            None,
            None,
            if with_artifact {
                Some(json!({"emails": [1, 2, 3], "count": 3}))
            } else {
                None
            },
        )
    }

    fn service_with_live_sweep() -> LifecycleService {
        let catalog = Arc::new(InMemoryCatalog::new());
        let policy_engine = Arc::new(PolicyEngine::new());
        let sanitization = SanitizationGateway::new();
        let sweep_config = SweepConfig {
            dry_run: false,
            ..SweepConfig::default()
        };
        let scheduler = LifecycleScheduler::new(sweep_config, Arc::clone(&policy_engine));

        LifecycleService {
            catalog,
            policy_engine,
            sanitization,
            scheduler,
            registration_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    // -----------------------------------------------------------------------
    // 1. Register → Evaluate → Sweep
    // -----------------------------------------------------------------------

    #[test]
    fn register_evaluate_sweep_full_flow() {
        let service = LifecycleService::new();

        // Register artifacts across multiple domains.
        service
            .register(make_metadata("flow-pipe-1", ArtifactDomain::Pipeline))
            .unwrap();
        service
            .register(make_metadata("flow-wf-1", ArtifactDomain::Workflow))
            .unwrap();
        service
            .register(make_metadata("flow-ep-1", ArtifactDomain::Episode))
            .unwrap();

        // Stats should reflect 3 artifacts.
        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 3);

        // Evaluate each — all should be Active (freshly registered).
        let d1 = service.evaluate("flow-pipe-1").unwrap();
        assert_eq!(d1.current_state, LifecycleState::Active);
        assert_eq!(d1.recommended_state, LifecycleState::Active);
        assert_eq!(d1.policy_version, 1);

        let d2 = service.evaluate("flow-wf-1").unwrap();
        assert_eq!(d2.current_state, LifecycleState::Active);
        assert_eq!(d2.policy_version, 1);

        let d3 = service.evaluate("flow-ep-1").unwrap();
        assert_eq!(d3.current_state, LifecycleState::Active);
        // Episode domain is permanent+evergreen → stays Active.
        assert_eq!(d3.recommended_state, LifecycleState::Active);

        // Run sweep — in dry-run mode (default), no transitions applied.
        let sweep = service.run_sweep();
        assert!(sweep.dry_run);
        assert!(sweep.errors.is_empty());
    }

    // -----------------------------------------------------------------------
    // 2. Episode round-trip via bridge
    // -----------------------------------------------------------------------

    #[test]
    fn episode_roundtrip_via_bridge() {
        let service = LifecycleService::new();
        let episode = make_episode(true);
        let ownership = OwnershipScope {
            execution_id: Some("thread-integ".to_string()),
            ..Default::default()
        };

        // Register via bridge.
        bridge::register_episode(&service, &episode, &ownership);

        // Query catalog — should have one Episode artifact.
        let query = CatalogQuery {
            domain: Some(ArtifactDomain::Episode),
            ..Default::default()
        };
        let results = service.list(&query).unwrap();
        assert_eq!(results.len(), 1);

        let meta = &results[0];
        assert_eq!(meta.domain, ArtifactDomain::Episode);
        assert_eq!(meta.producer.producer_agent_id, "agent-integ");
        assert_eq!(meta.lifecycle_state, LifecycleState::Active);
    }

    #[test]
    fn episode_without_artifact_output_registers_nothing() {
        let service = LifecycleService::new();
        let episode = make_episode(false);
        let ownership = OwnershipScope::default();

        bridge::register_episode(&service, &episode, &ownership);

        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 0);
    }

    // -----------------------------------------------------------------------
    // 3. Workflow artifacts batch
    // -----------------------------------------------------------------------

    #[test]
    fn workflow_artifacts_batch_registration_and_stats() {
        let service = LifecycleService::new();

        let artifacts = vec![
            StepArtifact {
                name: "step-out-1".to_string(),
                artifact_type: "data_bundle".to_string(),
                content: json!({"data": "alpha"}),
                provenance: ArtifactProvenance {
                    workflow_instance_id: Some("wf-inst-1".to_string()),
                    step_name: Some("step-summarize".to_string()),
                    agent_id: "summarizer-agent".to_string(),
                    source_agent_ids: vec![],
                    produced_at: Utc::now(),
                },
                content_type: None,
                render_hints: None,
            },
            StepArtifact {
                name: "step-out-2".to_string(),
                artifact_type: "data_bundle".to_string(),
                content: json!({"data": "beta"}),
                provenance: ArtifactProvenance {
                    workflow_instance_id: Some("wf-inst-1".to_string()),
                    step_name: Some("step-summarize".to_string()),
                    agent_id: "summarizer-agent".to_string(),
                    source_agent_ids: vec![],
                    produced_at: Utc::now(),
                },
                content_type: None,
                render_hints: None,
            },
            StepArtifact {
                name: "step-out-3".to_string(),
                artifact_type: "data_bundle".to_string(),
                content: json!({"data": "gamma"}),
                provenance: ArtifactProvenance {
                    workflow_instance_id: Some("wf-inst-1".to_string()),
                    step_name: Some("step-summarize".to_string()),
                    agent_id: "summarizer-agent".to_string(),
                    source_agent_ids: vec![],
                    produced_at: Utc::now(),
                },
                content_type: None,
                render_hints: None,
            },
        ];
        let ownership = OwnershipScope {
            workflow_instance_id: Some("wf-inst-1".to_string()),
            ..Default::default()
        };

        bridge::register_workflow_artifacts(
            &service,
            &artifacts,
            "wf-inst-1",
            "step-summarize",
            "summarizer-agent",
            &ownership,
        );

        // Should have 3 workflow artifacts.
        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 3);
        assert_eq!(stats.by_domain.get("workflow"), Some(&3));

        // Run scope cleanup with RunCompleted event.
        let result = service.request_cleanup(ScopeEvent::RunCompleted {
            run_id: "wf-inst-1".to_string(),
        });
        assert!(result.errors.is_empty());
    }

    // -----------------------------------------------------------------------
    // 4. Cutover fence integration
    // -----------------------------------------------------------------------

    #[test]
    fn cutover_fence_rejects_pre_cutover_accepts_post() {
        let cutover_time = Utc::now();
        let catalog = Arc::new(InMemoryCatalog::with_cutover(CutoverFence::new(
            cutover_time,
        )));
        let policy_engine = Arc::new(PolicyEngine::new());
        let sanitization = SanitizationGateway::new();
        let sweep_config = SweepConfig {
            dry_run: true,
            ..SweepConfig::default()
        };
        let scheduler = LifecycleScheduler::new(sweep_config, Arc::clone(&policy_engine));

        let service = LifecycleService {
            catalog,
            policy_engine,
            sanitization,
            scheduler,
            registration_count: std::sync::atomic::AtomicU64::new(0),
        };

        // Pre-cutover artifact should be rejected.
        let mut pre_meta = make_metadata("pre-cut-1", ArtifactDomain::Pipeline);
        pre_meta.producer.produced_at = cutover_time - Duration::hours(1);
        let result = service.register(pre_meta);
        assert!(result.is_err());

        // Post-cutover artifact should be accepted.
        let post_meta = make_metadata("post-cut-1", ArtifactDomain::Pipeline);
        // produced_at defaults to Utc::now() which is after cutover
        let result = service.register(post_meta);
        assert!(result.is_ok());

        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 1);
    }

    // -----------------------------------------------------------------------
    // 5. Sanitization projection per surface
    // -----------------------------------------------------------------------

    #[test]
    fn sanitization_projection_per_surface() {
        let service = LifecycleService::new();
        let meta = make_metadata("proj-integ-1", ArtifactDomain::Execution);
        service.register(meta).unwrap();

        let content = json!({
            "summary": "analysis complete",
            "password": "hunter2",
            "api_key": "sk-test-1234",
            "result": {"count": 42}
        });

        // InternalRaw — no sanitization (secrets remain).
        let raw = service
            .get_projection("proj-integ-1", ProjectionSurface::InternalRaw, &content)
            .unwrap();
        assert_eq!(raw.surface, ProjectionSurface::InternalRaw);
        assert_eq!(raw.content["password"], "hunter2");

        // InternalSanitized — secrets stripped.
        let sanitized = service
            .get_projection(
                "proj-integ-1",
                ProjectionSurface::InternalSanitized,
                &content,
            )
            .unwrap();
        assert_eq!(sanitized.surface, ProjectionSurface::InternalSanitized);
        assert_eq!(sanitized.content["summary"], "analysis complete");
        assert_ne!(sanitized.content["password"], "hunter2");

        // Gaui — secrets and file paths stripped, sensitive fields redacted.
        // Note: Execution domain default exposure is InternalSanitized, which
        // does not permit Gaui surface. So this should return ExposureDenied.
        let gaui_result = service.get_projection("proj-integ-1", ProjectionSurface::Gaui, &content);
        assert!(gaui_result.is_err());

        // Test with Episode domain (GauiSanitized exposure permits Gaui surface).
        let mut ep_meta = make_metadata("proj-ep-1", ArtifactDomain::Episode);
        ep_meta.policy.exposure_class =
            crate::magician_v2::artifacts::types::ExposureClass::GauiSanitized;
        service.register(ep_meta).unwrap();
        let gaui = service
            .get_projection("proj-ep-1", ProjectionSurface::Gaui, &content)
            .unwrap();
        assert_eq!(gaui.surface, ProjectionSurface::Gaui);
        assert_ne!(gaui.content["password"], "hunter2");
    }

    // -----------------------------------------------------------------------
    // 6. Live sweep transitions stale artifacts
    // -----------------------------------------------------------------------

    #[test]
    fn live_sweep_transitions_stale_workflow_artifacts() {
        let service = service_with_live_sweep();

        // Register a workflow artifact with a produced_at far in the past
        // so the Window freshness makes it stale.
        let mut meta = make_metadata("sweep-wf-1", ArtifactDomain::Workflow);
        // Workflow default: fresh_seconds=300, stale_seconds=600.
        // Set produced_at to 10 minutes ago → should be stale.
        meta.producer.produced_at = Utc::now() - Duration::minutes(10);
        meta.last_validated_at = meta.producer.produced_at;
        service.register(meta).unwrap();

        // Evaluate should recommend Stale.
        let decision = service.evaluate("sweep-wf-1").unwrap();
        assert_eq!(decision.recommended_state, LifecycleState::Stale);

        // Run live sweep — should apply the transition.
        let sweep = service.run_sweep();
        assert!(!sweep.dry_run);
        assert!(sweep.candidates_evaluated > 0);
        assert!(sweep.transitions_applied > 0);

        // Verify the artifact is now Stale.
        let updated = service.get("sweep-wf-1").unwrap().unwrap();
        assert_eq!(updated.lifecycle_state, LifecycleState::Stale);
    }

    // -----------------------------------------------------------------------
    // 7. Policy version propagation
    // -----------------------------------------------------------------------

    #[test]
    fn policy_version_propagated_in_decisions() {
        let service = LifecycleService::new();
        service
            .register(make_metadata("pv-1", ArtifactDomain::Pipeline))
            .unwrap();
        service
            .register(make_metadata("pv-2", ArtifactDomain::Episode))
            .unwrap();

        let d1 = service.evaluate("pv-1").unwrap();
        let d2 = service.evaluate("pv-2").unwrap();

        assert_eq!(d1.policy_version, 1);
        assert_eq!(d2.policy_version, 1);
    }
}
