//! # Artifact Catalog
//!
//! Logical registry of artifact metadata across domains. The catalog stores
//! identity, scope, policy bindings, freshness timestamps, reference links,
//! and classification. Existing stores remain source-of-truth for payload
//! bytes/content; the catalog is source-of-truth for lifecycle control
//! decisions.
//!
//! Provides both an in-memory implementation (`InMemoryCatalog`) and an
//! optional file-backed wrapper (`FileCatalog`) that persists the catalog
//! to JSON on every mutation.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::types::{
    ArtifactMetadata, ArtifactReference, ArtifactUid, CatalogQuery, CatalogStats, CutoverFence,
    LifecycleState, TransitionReason,
};
use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;

// ---------------------------------------------------------------------------
// CatalogError
// ---------------------------------------------------------------------------

/// Errors that can occur during catalog operations.
#[derive(Debug)]
pub enum CatalogError {
    /// An artifact with the given UID is already registered.
    AlreadyExists { uid: String },
    /// No artifact with the given UID exists in the catalog.
    NotFound { uid: String },
    /// The artifact was produced before the cutover timestamp and is not
    /// eligible for lifecycle management.
    PreCutover { uid: String },
    /// The artifact metadata fails validation (missing or invalid fields).
    ValidationFailed { uid: String, reason: String },
    /// An internal / unexpected error.
    Internal { message: String },
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::AlreadyExists { uid } => {
                write!(f, "artifact already exists in catalog: {}", uid)
            },
            CatalogError::NotFound { uid } => {
                write!(f, "artifact not found in catalog: {}", uid)
            },
            CatalogError::PreCutover { uid } => {
                write!(
                    f,
                    "artifact produced before cutover fence, rejected: {}",
                    uid
                )
            },
            CatalogError::ValidationFailed { uid, reason } => {
                write!(f, "metadata validation failed for {}: {}", uid, reason)
            },
            CatalogError::Internal { message } => {
                write!(f, "internal catalog error: {}", message)
            },
        }
    }
}

impl std::error::Error for CatalogError {}

// ---------------------------------------------------------------------------
// ArtifactCatalog trait
// ---------------------------------------------------------------------------

/// Trait defining the artifact catalog interface.
///
/// All methods take `&self` (interior mutability via `RwLock` or similar) so
/// that the catalog can be shared across threads behind an `Arc`.
pub trait ArtifactCatalog: Send + Sync {
    /// Register a new artifact in the catalog.
    ///
    /// Fails with `AlreadyExists` if the UID is already registered.
    /// Validates required metadata fields and enforces the cutover fence.
    fn register(&self, metadata: ArtifactMetadata) -> Result<(), CatalogError>;

    /// Look up an artifact by its UID.
    fn get(&self, uid: &str) -> Result<Option<ArtifactMetadata>, CatalogError>;

    /// Query the catalog with filters and pagination.
    fn query(&self, query: &CatalogQuery) -> Result<Vec<ArtifactMetadata>, CatalogError>;

    /// Update an existing artifact entry.
    ///
    /// Fails with `NotFound` if the UID is not registered.
    fn update(&self, uid: &str, metadata: ArtifactMetadata) -> Result<(), CatalogError>;

    /// Remove an artifact from the catalog, returning the removed metadata.
    ///
    /// Returns `Ok(None)` if the UID was not present.
    fn remove(&self, uid: &str) -> Result<Option<ArtifactMetadata>, CatalogError>;

    /// Compute aggregate statistics over all registered artifacts.
    fn stats(&self) -> Result<CatalogStats, CatalogError>;

    /// Apply a lifecycle state transition to the artifact with the given UID.
    ///
    /// Fails with `NotFound` if the UID is not registered.
    fn transition(
        &self,
        uid: &str,
        new_state: LifecycleState,
        reason: TransitionReason,
    ) -> Result<(), CatalogError>;

    /// Add a reference from another entity to the artifact.
    ///
    /// Idempotent: adding the same reference twice is a no-op.
    /// Fails with `NotFound` if the UID is not registered.
    fn add_reference(&self, uid: &str, reference: ArtifactReference) -> Result<(), CatalogError>;

    /// Release a reference by referrer_id from the artifact.
    ///
    /// No-op if the referrer is not present. Fails with `NotFound` if the
    /// UID is not registered.
    fn release_reference(&self, uid: &str, referrer_id: &str) -> Result<(), CatalogError>;
}

// ---------------------------------------------------------------------------
// InMemoryCatalog
// ---------------------------------------------------------------------------

/// Thread-safe, in-memory artifact catalog backed by a `RwLock<HashMap>`.
pub struct InMemoryCatalog {
    entries: RwLock<HashMap<ArtifactUid, ArtifactMetadata>>,
    cutover_fence: Option<CutoverFence>,
}

impl InMemoryCatalog {
    /// Create an empty catalog with no cutover fence.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            cutover_fence: None,
        }
    }

    /// Create an empty catalog with a cutover fence. Artifacts produced before
    /// the fence timestamp will be rejected at registration time.
    pub fn with_cutover(fence: CutoverFence) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            cutover_fence: Some(fence),
        }
    }

    /// Create a catalog pre-loaded with entries (used by `FileCatalog` on
    /// restore).
    fn from_entries(
        entries: HashMap<ArtifactUid, ArtifactMetadata>,
        cutover_fence: Option<CutoverFence>,
    ) -> Self {
        Self {
            entries: RwLock::new(entries),
            cutover_fence,
        }
    }

    /// Validate that an `ArtifactMetadata` record satisfies the required
    /// contract. Returns `Ok(())` if valid, or a `ValidationFailed` error
    /// describing the first violation found.
    fn validate_metadata(metadata: &ArtifactMetadata) -> Result<(), CatalogError> {
        if metadata.artifact_uid.is_empty() {
            return Err(CatalogError::ValidationFailed {
                uid: metadata.artifact_uid.clone(),
                reason: "artifact_uid must not be empty".to_string(),
            });
        }
        if metadata.producer.producer_agent_id.is_empty() {
            return Err(CatalogError::ValidationFailed {
                uid: metadata.artifact_uid.clone(),
                reason: "producer.producer_agent_id must not be empty".to_string(),
            });
        }
        Ok(())
    }

    /// Lock helper — acquires a read lock or returns an `Internal` error.
    fn read_lock(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, HashMap<ArtifactUid, ArtifactMetadata>>, CatalogError>
    {
        self.entries.read().map_err(|e| CatalogError::Internal {
            message: format!("read lock poisoned: {}", e),
        })
    }

    /// Lock helper — acquires a write lock or returns an `Internal` error.
    fn write_lock(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, HashMap<ArtifactUid, ArtifactMetadata>>, CatalogError>
    {
        self.entries.write().map_err(|e| CatalogError::Internal {
            message: format!("write lock poisoned: {}", e),
        })
    }

    /// Check whether the given `produced_at` timestamp falls before the
    /// cutover fence (if one is configured). Returns `Err(PreCutover)` when
    /// rejected.
    fn check_cutover(&self, uid: &str, produced_at: &DateTime<Utc>) -> Result<(), CatalogError> {
        if let Some(ref fence) = self.cutover_fence {
            if !fence.is_post_cutover(produced_at) {
                tracing::warn!(
                    artifact_uid = %uid,
                    produced_at = %produced_at,
                    cutover = %fence.cutover_utc,
                    "rejecting pre-cutover artifact"
                );
                return Err(CatalogError::PreCutover {
                    uid: uid.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Filter predicate: returns `true` if `metadata` matches all non-`None`
    /// fields in `query`.
    fn matches(metadata: &ArtifactMetadata, query: &CatalogQuery) -> bool {
        if let Some(ref domain) = query.domain {
            if metadata.domain != *domain {
                return false;
            }
        }
        if let Some(ref artifact_type) = query.artifact_type {
            match metadata.artifact_type.as_ref() {
                Some(value) if value == artifact_type => {},
                _ => return false,
            }
        }
        if let Some(ref route_target) = query.route_target {
            match metadata.route_target.as_ref() {
                Some(value) if value == route_target => {},
                _ => return false,
            }
        }
        if let Some(ref execution_id) = query.execution_id {
            match metadata.ownership.execution_id {
                Some(ref value) if value == execution_id => {},
                _ => return false,
            }
        }
        if let Some(ref task_id) = query.task_id {
            match metadata.ownership.task_id {
                Some(ref value) if value == task_id => {},
                _ => return false,
            }
        }
        if let Some(ref agent_id) = query.agent_id {
            if metadata.producer.producer_agent_id != *agent_id {
                return false;
            }
        }
        if let Some(ref wf_id) = query.workflow_instance_id {
            match metadata.ownership.workflow_instance_id {
                Some(ref wid) if wid == wf_id => {},
                _ => return false,
            }
        }
        if let Some(ref run_id) = query.run_id {
            match metadata.ownership.run_id {
                Some(ref value) if value == run_id => {},
                _ => return false,
            }
        }
        if let Some(ref cycle_id) = query.cycle_id {
            match metadata.ownership.cycle_id {
                Some(ref value) if value == cycle_id => {},
                _ => return false,
            }
        }
        if !query.lifecycle_states.is_empty()
            && !query.lifecycle_states.contains(&metadata.lifecycle_state)
        {
            return false;
        }
        if let Some(ref after) = query.produced_after {
            if metadata.producer.produced_at < *after {
                return false;
            }
        }
        if let Some(ref before) = query.produced_before {
            if metadata.producer.produced_at > *before {
                return false;
            }
        }
        true
    }
}

impl Default for InMemoryCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtifactCatalog for InMemoryCatalog {
    fn register(&self, metadata: ArtifactMetadata) -> Result<(), CatalogError> {
        // Validate required fields.
        Self::validate_metadata(&metadata)?;

        // Enforce cutover fence.
        self.check_cutover(&metadata.artifact_uid, &metadata.producer.produced_at)?;

        let mut entries = self.write_lock()?;
        if entries.contains_key(&metadata.artifact_uid) {
            return Err(CatalogError::AlreadyExists {
                uid: metadata.artifact_uid.clone(),
            });
        }
        entries.insert(metadata.artifact_uid.clone(), metadata);
        Ok(())
    }

    fn get(&self, uid: &str) -> Result<Option<ArtifactMetadata>, CatalogError> {
        let entries = self.read_lock()?;
        Ok(entries.get(uid).cloned())
    }

    fn query(&self, query: &CatalogQuery) -> Result<Vec<ArtifactMetadata>, CatalogError> {
        let entries = self.read_lock()?;
        let offset = query.offset.unwrap_or(0);
        let limit = query.limit.unwrap_or(usize::MAX);

        // Collect and sort by artifact_uid for deterministic pagination.
        // HashMap iteration order is arbitrary, so without sorting,
        // offset/limit would return different results across calls.
        let mut results: Vec<ArtifactMetadata> = entries
            .values()
            .filter(|m| Self::matches(m, query))
            .cloned()
            .collect();
        results.sort_by(|a, b| a.artifact_uid.cmp(&b.artifact_uid));

        let results: Vec<ArtifactMetadata> = results.into_iter().skip(offset).take(limit).collect();

        Ok(results)
    }

    fn update(&self, uid: &str, metadata: ArtifactMetadata) -> Result<(), CatalogError> {
        let mut entries = self.write_lock()?;
        if !entries.contains_key(uid) {
            return Err(CatalogError::NotFound {
                uid: uid.to_string(),
            });
        }
        entries.insert(uid.to_string(), metadata);
        Ok(())
    }

    fn remove(&self, uid: &str) -> Result<Option<ArtifactMetadata>, CatalogError> {
        let mut entries = self.write_lock()?;
        Ok(entries.remove(uid))
    }

    fn stats(&self) -> Result<CatalogStats, CatalogError> {
        let entries = self.read_lock()?;

        let mut by_domain: HashMap<String, usize> = HashMap::new();
        let mut by_state: HashMap<String, usize> = HashMap::new();
        let mut protected_count: usize = 0;
        let mut referenced_count: usize = 0;
        let mut cleanup_candidates: usize = 0;

        for meta in entries.values() {
            *by_domain.entry(meta.domain.to_string()).or_insert(0) += 1;
            *by_state
                .entry(meta.lifecycle_state.to_string())
                .or_insert(0) += 1;

            if meta.is_protected() {
                protected_count += 1;
            }
            if !meta.references.is_empty() {
                referenced_count += 1;
            }
            if matches!(
                meta.lifecycle_state,
                LifecycleState::Expired
                    | LifecycleState::Superseded
                    | LifecycleState::DeletePending
            ) {
                cleanup_candidates += 1;
            }
        }

        Ok(CatalogStats {
            total_artifacts: entries.len(),
            by_domain,
            by_state,
            protected_count,
            referenced_count,
            cleanup_candidates,
        })
    }

    fn transition(
        &self,
        uid: &str,
        new_state: LifecycleState,
        reason: TransitionReason,
    ) -> Result<(), CatalogError> {
        let mut entries = self.write_lock()?;
        let entry = entries.get_mut(uid).ok_or_else(|| CatalogError::NotFound {
            uid: uid.to_string(),
        })?;
        let from_state = entry.lifecycle_state.to_string();
        entry.transition_to(new_state, reason);
        crate::magician_v2::analytics::emit(
            crate::magician_v2::analytics::event_sink::AnalyticsEvent::artifact_transition(
                uid,
                &from_state,
                &new_state.to_string(),
            ),
        );
        Ok(())
    }

    fn add_reference(&self, uid: &str, reference: ArtifactReference) -> Result<(), CatalogError> {
        let mut entries = self.write_lock()?;
        let entry = entries.get_mut(uid).ok_or_else(|| CatalogError::NotFound {
            uid: uid.to_string(),
        })?;
        entry.add_reference(reference);
        Ok(())
    }

    fn release_reference(&self, uid: &str, referrer_id: &str) -> Result<(), CatalogError> {
        let mut entries = self.write_lock()?;
        let entry = entries.get_mut(uid).ok_or_else(|| CatalogError::NotFound {
            uid: uid.to_string(),
        })?;
        entry.release_reference(referrer_id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FileCatalog
// ---------------------------------------------------------------------------

/// Persistent JSON representation stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedCatalog {
    entries: HashMap<ArtifactUid, ArtifactMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cutover_fence: Option<CutoverFence>,
}

/// File-backed catalog that wraps `InMemoryCatalog` and persists to a JSON
/// file on every mutation.
pub struct FileCatalog {
    inner: InMemoryCatalog,
    path: PathBuf,
}

impl FileCatalog {
    /// Open (or create) a file-backed catalog at `base_path/catalog.json`.
    ///
    /// If the file exists, its contents are loaded into memory. If it does not
    /// exist, an empty catalog is created.
    pub fn open(
        base_path: PathBuf,
        cutover_fence: Option<CutoverFence>,
    ) -> Result<Self, CatalogError> {
        let path = base_path.join("catalog.json");

        let inner = if path.exists() {
            let data = std::fs::read_to_string(&path).map_err(|e| CatalogError::Internal {
                message: format!("failed to read catalog file {}: {}", path.display(), e),
            })?;
            let persisted: PersistedCatalog =
                serde_json::from_str(&data).map_err(|e| CatalogError::Internal {
                    message: format!(
                        "failed to deserialize catalog file {}: {}",
                        path.display(),
                        e
                    ),
                })?;
            // Prefer the fence supplied at construction over whatever was persisted.
            let fence = cutover_fence.or(persisted.cutover_fence);
            InMemoryCatalog::from_entries(persisted.entries, fence)
        } else {
            match cutover_fence {
                Some(f) => InMemoryCatalog::with_cutover(f),
                None => InMemoryCatalog::new(),
            }
        };

        Ok(Self { inner, path })
    }

    /// Persist the current in-memory state to disk.
    ///
    /// Serializes directly from the read-lock guard to avoid cloning the
    /// entire HashMap on every mutation.
    fn persist(&self) -> Result<(), CatalogError> {
        let entries = self.inner.read_lock()?;

        // Borrow-only view avoids cloning the HashMap.
        #[derive(Serialize)]
        struct PersistedCatalogRef<'a> {
            entries: &'a HashMap<ArtifactUid, ArtifactMetadata>,
            #[serde(skip_serializing_if = "Option::is_none")]
            cutover_fence: &'a Option<CutoverFence>,
        }

        let persisted = PersistedCatalogRef {
            entries: &entries,
            cutover_fence: &self.inner.cutover_fence,
        };
        let data =
            serde_json::to_string_pretty(&persisted).map_err(|e| CatalogError::Internal {
                message: format!("failed to serialize catalog: {}", e),
            })?;

        // Release the read lock before disk I/O so writers aren't blocked
        // during file operations.
        drop(entries);

        // Publish through the shared durable writer. The hand-rolled version
        // staged under a fixed `catalog.json.tmp` — shared by every concurrent
        // writer of this catalog — and neither fsynced the staging file nor the
        // parent directory, so a crash could publish a truncated catalog.
        write_bytes_durably_sync(&self.path, data.as_bytes()).map_err(|e| CatalogError::Internal {
            message: format!(
                "failed to write catalog file {}: {}",
                self.path.display(),
                e
            ),
        })
    }
}

impl ArtifactCatalog for FileCatalog {
    fn register(&self, metadata: ArtifactMetadata) -> Result<(), CatalogError> {
        self.inner.register(metadata)?;
        self.persist()
    }

    fn get(&self, uid: &str) -> Result<Option<ArtifactMetadata>, CatalogError> {
        self.inner.get(uid)
    }

    fn query(&self, query: &CatalogQuery) -> Result<Vec<ArtifactMetadata>, CatalogError> {
        self.inner.query(query)
    }

    fn update(&self, uid: &str, metadata: ArtifactMetadata) -> Result<(), CatalogError> {
        self.inner.update(uid, metadata)?;
        self.persist()
    }

    fn remove(&self, uid: &str) -> Result<Option<ArtifactMetadata>, CatalogError> {
        let removed = self.inner.remove(uid)?;
        if removed.is_some() {
            self.persist()?;
        }
        Ok(removed)
    }

    fn stats(&self) -> Result<CatalogStats, CatalogError> {
        self.inner.stats()
    }

    fn transition(
        &self,
        uid: &str,
        new_state: LifecycleState,
        reason: TransitionReason,
    ) -> Result<(), CatalogError> {
        self.inner.transition(uid, new_state, reason)?;
        self.persist()
    }

    fn add_reference(&self, uid: &str, reference: ArtifactReference) -> Result<(), CatalogError> {
        self.inner.add_reference(uid, reference)?;
        self.persist()
    }

    fn release_reference(&self, uid: &str, referrer_id: &str) -> Result<(), CatalogError> {
        self.inner.release_reference(uid, referrer_id)?;
        self.persist()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifacts::types::{
        ArtifactDomain, ArtifactReference, OwnershipScope, PhysicalLocator, PolicyBindings,
        ProducerInfo,
    };
    use chrono::Duration;
    use std::collections::HashSet;
    use std::collections::VecDeque;

    /// Helper: build a minimal valid `ArtifactMetadata` with sensible defaults.
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
                producer_agent_id: "agent-1".to_string(),
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

    /// Helper: build metadata with explicit ownership fields.
    fn make_metadata_with_scope(
        uid: &str,
        domain: ArtifactDomain,
        execution_id: Option<&str>,
        agent_id: &str,
        workflow_instance_id: Option<&str>,
    ) -> ArtifactMetadata {
        let mut meta = make_metadata(uid, domain);
        meta.ownership.execution_id = execution_id.map(|s| s.to_string());
        meta.ownership.workflow_instance_id = workflow_instance_id.map(|s| s.to_string());
        meta.producer.producer_agent_id = agent_id.to_string();
        meta
    }

    // -----------------------------------------------------------------------
    // Register + Get roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn register_and_get_roundtrip() {
        let catalog = InMemoryCatalog::new();
        let meta = make_metadata("art-1", ArtifactDomain::Pipeline);

        catalog.register(meta.clone()).unwrap();
        let retrieved = catalog.get("art-1").unwrap().expect("should exist");
        assert_eq!(retrieved.artifact_uid, "art-1");
        assert_eq!(retrieved.domain, ArtifactDomain::Pipeline);
    }

    // -----------------------------------------------------------------------
    // Duplicate registration
    // -----------------------------------------------------------------------

    #[test]
    fn duplicate_registration_fails() {
        let catalog = InMemoryCatalog::new();
        let meta = make_metadata("art-dup", ArtifactDomain::Workflow);

        catalog.register(meta.clone()).unwrap();
        let err = catalog.register(meta).unwrap_err();
        match err {
            CatalogError::AlreadyExists { uid } => assert_eq!(uid, "art-dup"),
            other => panic!("expected AlreadyExists, got: {}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Get missing artifact returns None
    // -----------------------------------------------------------------------

    #[test]
    fn get_missing_returns_none() {
        let catalog = InMemoryCatalog::new();
        assert!(catalog.get("nonexistent").unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // Update existing + update missing
    // -----------------------------------------------------------------------

    #[test]
    fn update_existing_artifact() {
        let catalog = InMemoryCatalog::new();
        let mut meta = make_metadata("art-upd", ArtifactDomain::Execution);
        catalog.register(meta.clone()).unwrap();

        meta.lifecycle_state = LifecycleState::Stale;
        catalog.update("art-upd", meta).unwrap();

        let retrieved = catalog.get("art-upd").unwrap().unwrap();
        assert_eq!(retrieved.lifecycle_state, LifecycleState::Stale);
    }

    #[test]
    fn update_missing_artifact_fails() {
        let catalog = InMemoryCatalog::new();
        let meta = make_metadata("missing", ArtifactDomain::Episode);
        let err = catalog.update("missing", meta).unwrap_err();
        match err {
            CatalogError::NotFound { uid } => assert_eq!(uid, "missing"),
            other => panic!("expected NotFound, got: {}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Remove
    // -----------------------------------------------------------------------

    #[test]
    fn remove_existing_artifact() {
        let catalog = InMemoryCatalog::new();
        let meta = make_metadata("art-rm", ArtifactDomain::Pipeline);
        catalog.register(meta).unwrap();

        let removed = catalog.remove("art-rm").unwrap();
        assert!(removed.is_some());
        assert!(catalog.get("art-rm").unwrap().is_none());
    }

    #[test]
    fn remove_missing_returns_none() {
        let catalog = InMemoryCatalog::new();
        assert!(catalog.remove("nope").unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // Query filtering by domain
    // -----------------------------------------------------------------------

    #[test]
    fn query_filter_by_domain() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata("p1", ArtifactDomain::Pipeline))
            .unwrap();
        catalog
            .register(make_metadata("w1", ArtifactDomain::Workflow))
            .unwrap();
        catalog
            .register(make_metadata("p2", ArtifactDomain::Pipeline))
            .unwrap();

        let query = CatalogQuery {
            domain: Some(ArtifactDomain::Pipeline),
            ..Default::default()
        };
        let results = catalog.query(&query).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|m| m.domain == ArtifactDomain::Pipeline));
    }

    // -----------------------------------------------------------------------
    // Query filtering by lifecycle state
    // -----------------------------------------------------------------------

    #[test]
    fn query_filter_by_state() {
        let catalog = InMemoryCatalog::new();

        let mut stale = make_metadata("s1", ArtifactDomain::Execution);
        stale.lifecycle_state = LifecycleState::Stale;
        catalog.register(stale).unwrap();

        let active = make_metadata("a1", ArtifactDomain::Execution);
        catalog.register(active).unwrap();

        let mut states = HashSet::new();
        states.insert(LifecycleState::Stale);

        let query = CatalogQuery {
            lifecycle_states: states,
            ..Default::default()
        };
        let results = catalog.query(&query).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].artifact_uid, "s1");
    }

    // -----------------------------------------------------------------------
    // Query filtering by execution_id
    // -----------------------------------------------------------------------

    #[test]
    fn query_filter_by_execution_id() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata_with_scope(
                "t1",
                ArtifactDomain::Pipeline,
                Some("exec-A"),
                "agent-1",
                None,
            ))
            .unwrap();
        catalog
            .register(make_metadata_with_scope(
                "t2",
                ArtifactDomain::Pipeline,
                Some("exec-B"),
                "agent-1",
                None,
            ))
            .unwrap();
        catalog
            .register(make_metadata_with_scope(
                "t3",
                ArtifactDomain::Pipeline,
                None,
                "agent-1",
                None,
            ))
            .unwrap();

        let query = CatalogQuery {
            execution_id: Some("exec-A".to_string()),
            ..Default::default()
        };
        let results = catalog.query(&query).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].artifact_uid, "t1");
    }

    // -----------------------------------------------------------------------
    // Query filtering by agent_id
    // -----------------------------------------------------------------------

    #[test]
    fn query_filter_by_agent_id() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata_with_scope(
                "ag1",
                ArtifactDomain::Episode,
                None,
                "agent-alpha",
                None,
            ))
            .unwrap();
        catalog
            .register(make_metadata_with_scope(
                "ag2",
                ArtifactDomain::Episode,
                None,
                "agent-beta",
                None,
            ))
            .unwrap();

        let query = CatalogQuery {
            agent_id: Some("agent-beta".to_string()),
            ..Default::default()
        };
        let results = catalog.query(&query).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].artifact_uid, "ag2");
    }

    // -----------------------------------------------------------------------
    // Query filtering by workflow_instance_id
    // -----------------------------------------------------------------------

    #[test]
    fn query_filter_by_workflow_instance_id() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata_with_scope(
                "wf1",
                ArtifactDomain::Workflow,
                None,
                "agent-1",
                Some("wf-instance-A"),
            ))
            .unwrap();
        catalog
            .register(make_metadata_with_scope(
                "wf2",
                ArtifactDomain::Workflow,
                None,
                "agent-1",
                Some("wf-instance-B"),
            ))
            .unwrap();

        let query = CatalogQuery {
            workflow_instance_id: Some("wf-instance-A".to_string()),
            ..Default::default()
        };
        let results = catalog.query(&query).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].artifact_uid, "wf1");
    }

    #[test]
    fn query_filter_by_task_run_cycle_route_and_artifact_type() {
        let catalog = InMemoryCatalog::new();

        let mut surface = make_metadata("surface-1", ArtifactDomain::Durable);
        surface.artifact_type = Some("custom:surface_manifest".to_string());
        surface.route_target = Some("/briefing".to_string());
        surface.ownership.task_id = Some("task-123".to_string());
        surface.ownership.run_id = Some("run-abc".to_string());
        surface.ownership.cycle_id = Some("cycle-9".to_string());
        catalog.register(surface).unwrap();

        let mut other_surface = make_metadata("surface-2", ArtifactDomain::Durable);
        other_surface.artifact_type = Some("custom:surface_manifest".to_string());
        other_surface.route_target = Some("/presto/veil".to_string());
        other_surface.ownership.task_id = Some("task-456".to_string());
        other_surface.ownership.run_id = Some("run-def".to_string());
        other_surface.ownership.cycle_id = Some("cycle-10".to_string());
        catalog.register(other_surface).unwrap();

        let query = CatalogQuery {
            domain: Some(ArtifactDomain::Durable),
            artifact_type: Some("custom:surface_manifest".to_string()),
            route_target: Some("/briefing".to_string()),
            task_id: Some("task-123".to_string()),
            run_id: Some("run-abc".to_string()),
            cycle_id: Some("cycle-9".to_string()),
            ..Default::default()
        };

        let results = catalog.query(&query).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].artifact_uid, "surface-1");
    }

    // -----------------------------------------------------------------------
    // Query filtering by produced_after / produced_before
    // -----------------------------------------------------------------------

    #[test]
    fn query_filter_by_time_range() {
        let catalog = InMemoryCatalog::new();
        let now = Utc::now();

        let mut old = make_metadata("old", ArtifactDomain::Pipeline);
        old.producer.produced_at = now - Duration::hours(2);
        catalog.register(old).unwrap();

        let mut recent = make_metadata("recent", ArtifactDomain::Pipeline);
        recent.producer.produced_at = now - Duration::minutes(10);
        catalog.register(recent).unwrap();

        let query = CatalogQuery {
            produced_after: Some(now - Duration::hours(1)),
            ..Default::default()
        };
        let results = catalog.query(&query).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].artifact_uid, "recent");

        let query_before = CatalogQuery {
            produced_before: Some(now - Duration::hours(1)),
            ..Default::default()
        };
        let results_before = catalog.query(&query_before).unwrap();
        assert_eq!(results_before.len(), 1);
        assert_eq!(results_before[0].artifact_uid, "old");
    }

    // -----------------------------------------------------------------------
    // Query combined filters
    // -----------------------------------------------------------------------

    #[test]
    fn query_combined_filters() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata_with_scope(
                "combo-1",
                ArtifactDomain::Pipeline,
                Some("t1"),
                "agent-x",
                None,
            ))
            .unwrap();
        catalog
            .register(make_metadata_with_scope(
                "combo-2",
                ArtifactDomain::Workflow,
                Some("t1"),
                "agent-x",
                None,
            ))
            .unwrap();
        catalog
            .register(make_metadata_with_scope(
                "combo-3",
                ArtifactDomain::Pipeline,
                Some("t2"),
                "agent-x",
                None,
            ))
            .unwrap();

        let query = CatalogQuery {
            domain: Some(ArtifactDomain::Pipeline),
            execution_id: Some("t1".to_string()),
            ..Default::default()
        };
        let results = catalog.query(&query).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].artifact_uid, "combo-1");
    }

    // -----------------------------------------------------------------------
    // Pagination (limit / offset)
    // -----------------------------------------------------------------------

    #[test]
    fn query_pagination_limit_offset() {
        let catalog = InMemoryCatalog::new();
        // Insert enough artifacts to paginate through.
        for i in 0..10 {
            catalog
                .register(make_metadata(
                    &format!("pg-{:02}", i),
                    ArtifactDomain::Episode,
                ))
                .unwrap();
        }

        // First page: limit 3
        let page1 = CatalogQuery {
            limit: Some(3),
            offset: Some(0),
            ..Default::default()
        };
        let results1 = catalog.query(&page1).unwrap();
        assert_eq!(results1.len(), 3);

        // Second page: offset 3, limit 3
        let page2 = CatalogQuery {
            limit: Some(3),
            offset: Some(3),
            ..Default::default()
        };
        let results2 = catalog.query(&page2).unwrap();
        assert_eq!(results2.len(), 3);

        // Pages should not overlap (deterministic ordering by artifact_uid).
        let page1_uids: Vec<&str> = results1.iter().map(|m| m.artifact_uid.as_str()).collect();
        let page2_uids: Vec<&str> = results2.iter().map(|m| m.artifact_uid.as_str()).collect();
        for uid in &page1_uids {
            assert!(
                !page2_uids.contains(uid),
                "page overlap detected for {}",
                uid
            );
        }

        // Results within each page should be sorted by UID.
        assert_eq!(page1_uids, {
            let mut sorted = page1_uids.clone();
            sorted.sort();
            sorted
        });

        // Large offset past end
        let past_end = CatalogQuery {
            limit: Some(5),
            offset: Some(100),
            ..Default::default()
        };
        let results_empty = catalog.query(&past_end).unwrap();
        assert!(results_empty.is_empty());
    }

    // -----------------------------------------------------------------------
    // Stats computation
    // -----------------------------------------------------------------------

    #[test]
    fn stats_computation() {
        let catalog = InMemoryCatalog::new();

        // Two pipeline-active, one workflow-stale, one execution-expired,
        // one episode-delete_pending with a reference.
        catalog
            .register(make_metadata("s-p1", ArtifactDomain::Pipeline))
            .unwrap();
        catalog
            .register(make_metadata("s-p2", ArtifactDomain::Pipeline))
            .unwrap();

        let mut stale = make_metadata("s-w1", ArtifactDomain::Workflow);
        stale.lifecycle_state = LifecycleState::Stale;
        catalog.register(stale).unwrap();

        let mut expired = make_metadata("s-e1", ArtifactDomain::Execution);
        expired.lifecycle_state = LifecycleState::Expired;
        catalog.register(expired).unwrap();

        let mut dp = make_metadata("s-ep1", ArtifactDomain::Episode);
        dp.lifecycle_state = LifecycleState::DeletePending;
        dp.references.push(ArtifactReference {
            referrer_id: "cycle-1".to_string(),
            referrer_type: "goal_cycle".to_string(),
            established_at: Utc::now(),
        });
        catalog.register(dp).unwrap();

        let stats = catalog.stats().unwrap();
        assert_eq!(stats.total_artifacts, 5);
        assert_eq!(stats.by_domain.get("pipeline"), Some(&2));
        assert_eq!(stats.by_domain.get("workflow"), Some(&1));
        assert_eq!(stats.by_domain.get("execution"), Some(&1));
        assert_eq!(stats.by_domain.get("episode"), Some(&1));
        assert_eq!(stats.by_state.get("active"), Some(&2));
        assert_eq!(stats.by_state.get("stale"), Some(&1));
        assert_eq!(stats.by_state.get("expired"), Some(&1));
        assert_eq!(stats.by_state.get("delete_pending"), Some(&1));
        assert_eq!(stats.referenced_count, 1);
        // dp is protected (has references), so protected_count = 1
        assert_eq!(stats.protected_count, 1);
        // cleanup_candidates = expired + delete_pending = 2
        assert_eq!(stats.cleanup_candidates, 2);
    }

    // -----------------------------------------------------------------------
    // Lifecycle transitions
    // -----------------------------------------------------------------------

    #[test]
    fn lifecycle_transition() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata("trans-1", ArtifactDomain::Pipeline))
            .unwrap();

        catalog
            .transition(
                "trans-1",
                LifecycleState::Stale,
                TransitionReason::FreshnessExpired,
            )
            .unwrap();

        let meta = catalog.get("trans-1").unwrap().unwrap();
        assert_eq!(meta.lifecycle_state, LifecycleState::Stale);
        assert_eq!(meta.transition_log.len(), 1);
        assert_eq!(meta.transition_log[0].from_state, LifecycleState::Active);
        assert_eq!(meta.transition_log[0].to_state, LifecycleState::Stale);
    }

    #[test]
    fn transition_missing_artifact_fails() {
        let catalog = InMemoryCatalog::new();
        let err = catalog
            .transition(
                "ghost",
                LifecycleState::Expired,
                TransitionReason::StaleWindowExpired,
            )
            .unwrap_err();
        match err {
            CatalogError::NotFound { uid } => assert_eq!(uid, "ghost"),
            other => panic!("expected NotFound, got: {}", other),
        }
    }

    #[test]
    fn multiple_transitions_accumulate_log() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata("multi-t", ArtifactDomain::Execution))
            .unwrap();

        catalog
            .transition(
                "multi-t",
                LifecycleState::Stale,
                TransitionReason::FreshnessExpired,
            )
            .unwrap();
        catalog
            .transition(
                "multi-t",
                LifecycleState::Expired,
                TransitionReason::StaleWindowExpired,
            )
            .unwrap();
        catalog
            .transition(
                "multi-t",
                LifecycleState::DeletePending,
                TransitionReason::SweeperSelected,
            )
            .unwrap();

        let meta = catalog.get("multi-t").unwrap().unwrap();
        assert_eq!(meta.lifecycle_state, LifecycleState::DeletePending);
        assert_eq!(meta.transition_log.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Pre-cutover rejection
    // -----------------------------------------------------------------------

    #[test]
    fn pre_cutover_artifact_rejected() {
        let cutover_time = Utc::now();
        let catalog = InMemoryCatalog::with_cutover(CutoverFence::new(cutover_time));

        let mut old = make_metadata("pre-cut", ArtifactDomain::Pipeline);
        old.producer.produced_at = cutover_time - Duration::hours(1);

        let err = catalog.register(old).unwrap_err();
        match err {
            CatalogError::PreCutover { uid } => assert_eq!(uid, "pre-cut"),
            other => panic!("expected PreCutover, got: {}", other),
        }
    }

    #[test]
    fn post_cutover_artifact_accepted() {
        let cutover_time = Utc::now() - Duration::hours(1);
        let catalog = InMemoryCatalog::with_cutover(CutoverFence::new(cutover_time));

        let meta = make_metadata("post-cut", ArtifactDomain::Pipeline);
        // produced_at defaults to Utc::now() which is after cutover
        catalog.register(meta).unwrap();
        assert!(catalog.get("post-cut").unwrap().is_some());
    }

    #[test]
    fn no_cutover_fence_allows_all() {
        let catalog = InMemoryCatalog::new();
        let mut old = make_metadata("no-fence", ArtifactDomain::Pipeline);
        old.producer.produced_at = Utc::now() - Duration::days(365);
        catalog.register(old).unwrap();
        assert!(catalog.get("no-fence").unwrap().is_some());
    }

    // -----------------------------------------------------------------------
    // Metadata validation (quarantine)
    // -----------------------------------------------------------------------

    #[test]
    fn validation_rejects_empty_uid() {
        let catalog = InMemoryCatalog::new();
        let meta = make_metadata("", ArtifactDomain::Pipeline);
        let err = catalog.register(meta).unwrap_err();
        match err {
            CatalogError::ValidationFailed { uid, reason } => {
                assert_eq!(uid, "");
                assert!(reason.contains("artifact_uid"));
            },
            other => panic!("expected ValidationFailed, got: {}", other),
        }
    }

    #[test]
    fn validation_rejects_empty_producer_agent_id() {
        let catalog = InMemoryCatalog::new();
        let mut meta = make_metadata("valid-uid", ArtifactDomain::Pipeline);
        meta.producer.producer_agent_id = String::new();

        let err = catalog.register(meta).unwrap_err();
        match err {
            CatalogError::ValidationFailed { uid, reason } => {
                assert_eq!(uid, "valid-uid");
                assert!(reason.contains("producer_agent_id"));
            },
            other => panic!("expected ValidationFailed, got: {}", other),
        }
    }

    #[test]
    fn validation_passes_for_complete_metadata() {
        let catalog = InMemoryCatalog::new();
        let meta = make_metadata("ok-meta", ArtifactDomain::Episode);
        catalog.register(meta).unwrap();
        assert!(catalog.get("ok-meta").unwrap().is_some());
    }

    // -----------------------------------------------------------------------
    // CatalogError Display
    // -----------------------------------------------------------------------

    #[test]
    fn catalog_error_display() {
        let err = CatalogError::AlreadyExists {
            uid: "x".to_string(),
        };
        assert!(format!("{}", err).contains("already exists"));

        let err = CatalogError::NotFound {
            uid: "y".to_string(),
        };
        assert!(format!("{}", err).contains("not found"));

        let err = CatalogError::PreCutover {
            uid: "z".to_string(),
        };
        assert!(format!("{}", err).contains("cutover"));

        let err = CatalogError::ValidationFailed {
            uid: "w".to_string(),
            reason: "bad field".to_string(),
        };
        assert!(format!("{}", err).contains("validation failed"));

        let err = CatalogError::Internal {
            message: "boom".to_string(),
        };
        assert!(format!("{}", err).contains("internal"));
    }

    // -----------------------------------------------------------------------
    // FileCatalog persistence roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn file_catalog_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("artifact_catalog_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Write phase
        {
            let catalog = FileCatalog::open(dir.clone(), None).unwrap();
            catalog
                .register(make_metadata("file-1", ArtifactDomain::Pipeline))
                .unwrap();
            catalog
                .register(make_metadata("file-2", ArtifactDomain::Workflow))
                .unwrap();
            catalog
                .transition(
                    "file-1",
                    LifecycleState::Stale,
                    TransitionReason::FreshnessExpired,
                )
                .unwrap();
        }

        // Read phase — reconstruct from disk
        {
            let catalog = FileCatalog::open(dir.clone(), None).unwrap();
            let m1 = catalog.get("file-1").unwrap().unwrap();
            assert_eq!(m1.lifecycle_state, LifecycleState::Stale);
            assert_eq!(m1.transition_log.len(), 1);

            let m2 = catalog.get("file-2").unwrap().unwrap();
            assert_eq!(m2.lifecycle_state, LifecycleState::Active);

            let stats = catalog.stats().unwrap();
            assert_eq!(stats.total_artifacts, 2);
        }

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_catalog_remove_persists() {
        let dir =
            std::env::temp_dir().join(format!("artifact_catalog_rm_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        {
            let catalog = FileCatalog::open(dir.clone(), None).unwrap();
            catalog
                .register(make_metadata("rm-1", ArtifactDomain::Episode))
                .unwrap();
            catalog.remove("rm-1").unwrap();
        }

        {
            let catalog = FileCatalog::open(dir.clone(), None).unwrap();
            assert!(catalog.get("rm-1").unwrap().is_none());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two threads persisting the same catalog must leave a parseable
    /// `catalog.json` and no staging file.
    ///
    /// `FileCatalog::persist` is unlocked between serialize and rename, so one
    /// of the two registrations may be lost — that read-modify-write race is
    /// Phase 3's problem and is deliberately not asserted. What must hold is
    /// that a reader never finds a half-written catalog or a leftover temp.
    #[test]
    fn file_catalog_concurrent_persist_leaves_no_staging_file() {
        use std::sync::Arc;
        use std::thread;

        let dir = std::env::temp_dir().join(format!(
            "artifact_catalog_staging_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let catalog = Arc::new(FileCatalog::open(dir.clone(), None).unwrap());
        let handles: Vec<_> = ["cc-1", "cc-2"]
            .into_iter()
            .map(|uid| {
                let catalog = Arc::clone(&catalog);
                thread::spawn(move || {
                    catalog
                        .register(make_metadata(uid, ArtifactDomain::Pipeline))
                        .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let staging: Vec<String> = std::fs::read_dir(&dir)
            .expect("catalog directory listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            staging.is_empty(),
            "durable writes must leave no staging file, found {staging:?}"
        );

        let published = std::fs::read_to_string(dir.join("catalog.json"))
            .expect("catalog file should be readable");
        let parsed: PersistedCatalog =
            serde_json::from_str(&published).expect("published catalog should parse");
        assert!(
            !parsed.entries.is_empty(),
            "a completed registration must survive the concurrent write"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Thread safety smoke test
    // -----------------------------------------------------------------------

    #[test]
    fn thread_safety_smoke() {
        use std::sync::Arc;
        use std::thread;

        let catalog = Arc::new(InMemoryCatalog::new());
        let mut handles = vec![];

        // Spawn 10 threads, each registering one artifact.
        for i in 0..10 {
            let catalog = Arc::clone(&catalog);
            handles.push(thread::spawn(move || {
                let meta = make_metadata(&format!("thr-{}", i), ArtifactDomain::Pipeline);
                catalog.register(meta).unwrap();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let stats = catalog.stats().unwrap();
        assert_eq!(stats.total_artifacts, 10);
    }

    // -----------------------------------------------------------------------
    // Stats with protection flags
    // -----------------------------------------------------------------------

    #[test]
    fn stats_counts_protection_flags() {
        let catalog = InMemoryCatalog::new();

        let mut protected = make_metadata("prot-1", ArtifactDomain::Pipeline);
        protected.policy.protection_flags.legal_hold = true;
        catalog.register(protected).unwrap();

        let unprotected = make_metadata("unprot-1", ArtifactDomain::Pipeline);
        catalog.register(unprotected).unwrap();

        let stats = catalog.stats().unwrap();
        assert_eq!(stats.protected_count, 1);
    }

    // -----------------------------------------------------------------------
    // Empty catalog stats
    // -----------------------------------------------------------------------

    #[test]
    fn stats_empty_catalog() {
        let catalog = InMemoryCatalog::new();
        let stats = catalog.stats().unwrap();
        assert_eq!(stats.total_artifacts, 0);
        assert!(stats.by_domain.is_empty());
        assert!(stats.by_state.is_empty());
        assert_eq!(stats.protected_count, 0);
        assert_eq!(stats.referenced_count, 0);
        assert_eq!(stats.cleanup_candidates, 0);
    }

    // -----------------------------------------------------------------------
    // Query with empty filter returns all
    // -----------------------------------------------------------------------

    #[test]
    fn query_empty_filter_returns_all() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata("all-1", ArtifactDomain::Pipeline))
            .unwrap();
        catalog
            .register(make_metadata("all-2", ArtifactDomain::Workflow))
            .unwrap();

        let query = CatalogQuery::default();
        let results = catalog.query(&query).unwrap();
        assert_eq!(results.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Reference management via catalog trait
    // -----------------------------------------------------------------------

    #[test]
    fn add_reference_increments_ref_count() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata("ref-1", ArtifactDomain::Pipeline))
            .unwrap();

        let reference = ArtifactReference {
            referrer_id: "chain-abc".to_string(),
            referrer_type: "pipeline_chain".to_string(),
            established_at: Utc::now(),
        };
        catalog.add_reference("ref-1", reference).unwrap();

        let meta = catalog.get("ref-1").unwrap().unwrap();
        assert_eq!(meta.ref_count(), 1);
    }

    #[test]
    fn release_reference_decrements_ref_count() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata("ref-2", ArtifactDomain::Pipeline))
            .unwrap();

        let reference = ArtifactReference {
            referrer_id: "chain-xyz".to_string(),
            referrer_type: "pipeline_chain".to_string(),
            established_at: Utc::now(),
        };
        catalog.add_reference("ref-2", reference).unwrap();
        assert_eq!(catalog.get("ref-2").unwrap().unwrap().ref_count(), 1);

        catalog.release_reference("ref-2", "chain-xyz").unwrap();
        assert_eq!(catalog.get("ref-2").unwrap().unwrap().ref_count(), 0);
    }

    #[test]
    fn idempotent_add_reference() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata("ref-3", ArtifactDomain::Pipeline))
            .unwrap();

        let reference = ArtifactReference {
            referrer_id: "chain-dup".to_string(),
            referrer_type: "pipeline_chain".to_string(),
            established_at: Utc::now(),
        };
        catalog.add_reference("ref-3", reference.clone()).unwrap();
        catalog.add_reference("ref-3", reference).unwrap();

        let meta = catalog.get("ref-3").unwrap().unwrap();
        assert_eq!(meta.ref_count(), 1);
    }

    #[test]
    fn release_nonexistent_referrer_is_noop() {
        let catalog = InMemoryCatalog::new();
        catalog
            .register(make_metadata("ref-4", ArtifactDomain::Pipeline))
            .unwrap();

        // Release a referrer that was never added — should succeed silently.
        catalog.release_reference("ref-4", "never-added").unwrap();

        let meta = catalog.get("ref-4").unwrap().unwrap();
        assert_eq!(meta.ref_count(), 0);
    }

    #[test]
    fn file_catalog_reference_survives_reload() {
        let dir =
            std::env::temp_dir().join(format!("artifact_catalog_ref_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Write phase: register artifact, add reference
        {
            let catalog = FileCatalog::open(dir.clone(), None).unwrap();
            catalog
                .register(make_metadata("ref-file-1", ArtifactDomain::Pipeline))
                .unwrap();
            let reference = ArtifactReference {
                referrer_id: "chain-abc".to_string(),
                referrer_type: "pipeline_chain".to_string(),
                established_at: Utc::now(),
            };
            catalog.add_reference("ref-file-1", reference).unwrap();
            assert_eq!(catalog.get("ref-file-1").unwrap().unwrap().ref_count(), 1);
        }

        // Read phase: reload from disk, verify reference persisted
        {
            let catalog = FileCatalog::open(dir.clone(), None).unwrap();
            let meta = catalog.get("ref-file-1").unwrap().unwrap();
            assert_eq!(meta.ref_count(), 1);
            assert_eq!(meta.references[0].referrer_id, "chain-abc");

            // Release and verify
            catalog
                .release_reference("ref-file-1", "chain-abc")
                .unwrap();
            assert_eq!(catalog.get("ref-file-1").unwrap().unwrap().ref_count(), 0);
        }

        // Final reload: verify release persisted
        {
            let catalog = FileCatalog::open(dir.clone(), None).unwrap();
            assert_eq!(catalog.get("ref-file-1").unwrap().unwrap().ref_count(), 0);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_reference_to_missing_artifact_fails() {
        let catalog = InMemoryCatalog::new();
        let reference = ArtifactReference {
            referrer_id: "chain-1".to_string(),
            referrer_type: "pipeline_chain".to_string(),
            established_at: Utc::now(),
        };
        let err = catalog.add_reference("ghost", reference).unwrap_err();
        match err {
            CatalogError::NotFound { uid } => assert_eq!(uid, "ghost"),
            other => panic!("expected NotFound, got: {}", other),
        }
    }
}
