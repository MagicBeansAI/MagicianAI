//! Projection pipeline state — wires `ProjectionStore`, projection
//! JSON records, and the per-scope `ProjectionMetrics` into a single
//! handle the executor + agent tools use.
//!
//! Lifecycle on a successful replay:
//! 1. Executor calls `ingest_response(capability_id, url_template,
//!    response_body)` from the response path (PL Task 17 grouped
//!    wiring).
//! 2. Pipeline resolves `(origin, resource_label)` from the URL
//!    template via [`derive_resource_label`].
//! 3. If no projection exists for that `(origin, resource)`, infer
//!    one from the sample, write it as Pending. Counter:
//!    `ProjectionEvent::PendingCreated`.
//! 4. If a Pending projection exists, no row ingest happens until an
//!    operator approves it.
//! 5. If an Approved projection exists, extract rows from the
//!    sample, insert into the SQLite table. Counter:
//!    `ProjectionEvent::RowsIngested`. The first ingest after
//!    approval bumps lifecycle to Live.
//! 6. Schema convergence: if the sample exposes columns absent from
//!    the projection's schema, call `migrate_table`. Counter:
//!    `ProjectionEvent::SchemaConverged`. Non-additive changes flip
//!    `MigrationRejected`.
//!
//! On `query_known_resource`:
//! - Resolve the projection by `(origin, resource_label)`.
//! - If it's Live and within TTL, return rows.
//!   `ProjectionEvent::RowsServedFromStore`.
//! - If past TTL, still return rows but flag `StaleProjection`.
//!   `ProjectionEvent::StaleServed`.
//! - Otherwise return a typed error variant — see
//!   [`QueryKnownResourceError`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::magician_v2::artifact_v2::{
    workspace::{ArtifactV2Workspace, WorkspaceFileEntry},
    ArtifactV2Error,
};

use super::metrics::{ProjectionEvent, ProjectionMetrics};
use super::projection::{
    derive_resource_label, extract_rows, infer_projection_from_sample, ProjectionLifecycle,
    ProjectionStore, ResourceProjection,
};

/// Per-scope projection pipeline. Lives behind an `Arc<Mutex<_>>` so
/// the executor's hot path and the agent tool's query path share one
/// serialized view. The mutex window is tiny — projection record
/// I/O is single small JSON read/write, SQLite row I/O is bounded
/// by the per-call batch size.
pub struct ProjectionPipelineState {
    /// Directory containing the projection JSON records per
    /// projection id.
    records_dir: PathBuf,
    workspace_layout: ArtifactV2Workspace,
    /// SQLite store for rows. One DB per scope.
    store: ProjectionStore,
    /// Shared per-scope counter pool.
    metrics: Arc<ProjectionMetrics>,
    /// Per-projection ingest serialization. Multiple concurrent
    /// replays of the SAME `(origin, resource)` would otherwise race
    /// the load-modify-save of the projection JSON record — last
    /// write wins, intermediate state lost. We serialize via a
    /// keyed-mutex map: each `(origin, resource_label)` gets its own
    /// `Mutex<()>` lazily on first ingest, and `ingest_response`
    /// holds it for the duration of load + SQLite insert + save.
    /// Different projections don't block each other.
    ingest_locks:
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>,
}

#[derive(Debug)]
pub struct ProjectionOriginPurgeError {
    pub projections_deleted: usize,
    pub rows_deleted: usize,
    pub errors: Vec<String>,
}

impl std::fmt::Display for ProjectionOriginPurgeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.errors.join("; "))
    }
}

impl std::error::Error for ProjectionOriginPurgeError {}

/// JSON serializable index entry — used by HTTP endpoints to list
/// projections without loading every record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectionIndexEntry {
    pub id: String,
    pub capability_id: String,
    pub origin: String,
    pub resource_label: String,
    pub lifecycle: ProjectionLifecycle,
    pub last_ingested_at: Option<i64>,
    pub row_count: usize,
}

/// Where the rows came from when `query_known_resource` resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServedFrom {
    /// Fresh rows (within TTL) served from the local SQLite table.
    Projection,
    /// Past TTL but still served. Caller decides whether to refresh.
    StaleProjection,
}

/// Typed error variants for `query_known_resource` — surfaced to
/// the agent so its prompt can react (refresh / skip / fall back).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum QueryKnownResourceError {
    /// No projection exists for the requested `(origin, resource)`.
    NoProjection { origin: String, resource: String },
    /// Projection exists but is still Pending — operator hasn't
    /// approved it yet.
    PendingApproval { projection_id: String },
    /// SQLite or I/O error.
    StoreError { details: String },
}

/// Outcome of an `ingest_response` call. The caller (executor) emits
/// a tracing span with these fields so operators can correlate
/// "replay succeeded" with "rows landed".
#[derive(Debug, Clone)]
pub enum IngestOutcome {
    /// No array-of-objects in the response — projection inference
    /// declined to materialize anything. Common for endpoints
    /// returning singleton objects.
    NotProjectable,
    /// First time seeing this `(origin, resource)`. A Pending
    /// projection record was written; no rows ingested yet.
    PendingCreated { projection_id: String },
    /// Pending projection exists, awaiting operator approval. No
    /// new rows.
    PendingExisting { projection_id: String },
    /// Rows ingested into an Approved/Live projection.
    Ingested {
        projection_id: String,
        row_count: usize,
        schema_added_columns: usize,
    },
    /// Schema change rejected by migrate_table (column drop or type
    /// change). Operator must purge_rows first.
    MigrationRejected {
        projection_id: String,
        reason: String,
    },
    /// Persistence error.
    Failed { reason: String },
}

impl ProjectionPipelineState {
    /// Construct the pipeline state for a given scope. Creates the
    /// records directory + SQLite DB if missing. The SQLite file
    /// lives at `<records_dir>/_rows.db`; per-projection JSON
    /// records live at `<records_dir>/<projection_id>.json`.
    pub fn open(records_dir: PathBuf, metrics: Arc<ProjectionMetrics>) -> Result<Self, String> {
        let workspace_layout = ArtifactV2Workspace::with_local_file_provider(&records_dir);
        workspace_layout
            .create_dir_all_path_sync(&records_dir)
            .map_err(|e| format!("create projection records dir {records_dir:?}: {e}"))?;
        let db_name =
            Path::new(crate::magician_v2::database_owners::DatabaseOwner::ApiMining.sample_rel())
                .file_name()
                .ok_or_else(|| "api mining sample_rel missing file name".to_string())?;
        let db_path = records_dir.join(db_name);
        let store = ProjectionStore::open(&db_path)?;
        Ok(Self {
            records_dir,
            workspace_layout,
            store,
            metrics,
            ingest_locks: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Resolve (or lazily allocate) the per-projection ingest mutex.
    /// Returned guard must be held for the duration of the
    /// load-modify-save sequence on the JSON record. Separate
    /// projections take separate mutexes so writes to one don't
    /// block ingest into another.
    fn ingest_lock_for(&self, projection_id: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
        let mut map = self
            .ingest_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.entry(projection_id.to_string())
            .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
            .clone()
    }

    /// Shared counter pool. Tests use this to inspect after
    /// operations.
    pub fn metrics(&self) -> Arc<ProjectionMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Ingest a fresh response sample for the given capability.
    /// Resolves projection by `(origin, resource)` derived from
    /// `url_template`; creates Pending on first sight, inserts rows
    /// when Approved.
    ///
    /// Serializes per-projection so concurrent replays for the same
    /// `(origin, resource)` don't race the load-modify-save of the
    /// JSON record. Different projections proceed in parallel.
    pub fn ingest_response(
        &self,
        capability_id: &str,
        url_template: &str,
        response: &Value,
    ) -> IngestOutcome {
        let origin = extract_origin(url_template);
        let resource_label = derive_resource_label(url_template);
        let projection_id = format!(
            "{}_{}",
            crate::magician_v2::api_mining::projection::sanitize_origin_key(&origin),
            crate::magician_v2::api_mining::projection::sanitize_origin_key(&resource_label),
        );
        let lock = self.ingest_lock_for(&projection_id);
        let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        let record_path = self.records_dir.join(format!("{projection_id}.json"));

        match self.load_record(&record_path) {
            Ok(Some(mut existing)) => self.ingest_into_existing(
                &mut existing,
                projection_id,
                capability_id,
                response,
                &record_path,
            ),
            Ok(None) => self.create_pending(
                capability_id,
                &origin,
                resource_label,
                response,
                &record_path,
                projection_id,
            ),
            Err(e) => IngestOutcome::Failed { reason: e },
        }
    }

    /// Query the projection associated with `(origin, resource)`.
    /// Returns rows + `ServedFrom` on success, typed error otherwise.
    pub fn query_known_resource(
        &self,
        origin: &str,
        resource: &str,
        where_clause: Option<&str>,
        params: &[Value],
    ) -> Result<(Vec<serde_json::Map<String, Value>>, ServedFrom), QueryKnownResourceError> {
        let projection_id = format!(
            "{}_{}",
            crate::magician_v2::api_mining::projection::sanitize_origin_key(origin),
            crate::magician_v2::api_mining::projection::sanitize_origin_key(resource),
        );
        let record_path = self.records_dir.join(format!("{projection_id}.json"));
        let projection = match self.load_record(&record_path) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return Err(QueryKnownResourceError::NoProjection {
                    origin: origin.to_string(),
                    resource: resource.to_string(),
                });
            },
            Err(e) => return Err(QueryKnownResourceError::StoreError { details: e }),
        };
        if matches!(projection.lifecycle, ProjectionLifecycle::Pending) {
            return Err(QueryKnownResourceError::PendingApproval {
                projection_id: projection.id.clone(),
            });
        }
        let rows = self
            .store
            .query_rows(&projection, where_clause, params)
            .map_err(|e| QueryKnownResourceError::StoreError { details: e })?;
        let now = chrono::Utc::now().timestamp_millis();
        let is_stale = projection
            .last_ingested_at
            .map(|ts| (now - ts) / 1000 > projection.ttl_seconds)
            .unwrap_or(true);
        if is_stale {
            self.metrics.record(ProjectionEvent::StaleServed);
            Ok((rows, ServedFrom::StaleProjection))
        } else {
            self.metrics.record(ProjectionEvent::RowsServedFromStore);
            Ok((rows, ServedFrom::Projection))
        }
    }

    /// Approve a pending projection. Idempotent on already-approved.
    pub fn approve_projection(&self, projection_id: &str) -> Result<(), String> {
        let lock = self.ingest_lock_for(projection_id);
        let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        let record_path = self.records_dir.join(format!("{projection_id}.json"));
        let mut projection = self
            .load_record(&record_path)?
            .ok_or_else(|| format!("projection {projection_id} not found"))?;
        if matches!(projection.lifecycle, ProjectionLifecycle::Pending) {
            projection.lifecycle = ProjectionLifecycle::Approved;
            projection.updated_at = chrono::Utc::now().timestamp_millis();
            self.save_record(&record_path, &projection)?;
            self.metrics.record(ProjectionEvent::Approved);
        }
        Ok(())
    }

    /// Operator-driven purge: drop all rows for a projection so the
    /// schema can be re-shaped on the next ingest (e.g., after
    /// rejecting a non-additive migration).
    pub fn purge_rows(&self, projection_id: &str) -> Result<usize, String> {
        let lock = self.ingest_lock_for(projection_id);
        let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        let record_path = self.records_dir.join(format!("{projection_id}.json"));
        let projection = self
            .load_record(&record_path)?
            .ok_or_else(|| format!("projection {projection_id} not found"))?;
        let purged = self.store.purge_rows(&projection)?;
        self.metrics.record(ProjectionEvent::RowsPurged);
        Ok(purged)
    }

    /// Destructively remove every projection derived from one origin. This is
    /// used only by origin purge while the caller holds the scope mutation
    /// lock. Rows and their JSON records are removed under the same per-record
    /// locks used by ingestion, so a concurrent replay cannot resurrect them
    /// between the two operations.
    pub fn purge_origin(&self, origin: &str) -> Result<(usize, usize), ProjectionOriginPurgeError> {
        let entries = match self.workspace_layout.read_dir_path_sync(&self.records_dir) {
            Ok(entries) => entries,
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((0, 0));
            },
            Err(error) => {
                return Err(ProjectionOriginPurgeError {
                    projections_deleted: 0,
                    rows_deleted: 0,
                    errors: vec![format!("read {:?}: {error}", self.records_dir)],
                });
            },
        };
        let mut projections_deleted = 0usize;
        let mut rows_deleted = 0usize;
        let mut errors = Vec::new();
        for entry in entries.into_iter().filter(|entry| entry.is_file) {
            let path = self.entry_path(&entry);
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let projection = match self.load_record(&path) {
                Ok(Some(projection)) if projection.origin == origin => projection,
                Ok(_) => continue,
                Err(error) => {
                    errors.push(error);
                    continue;
                },
            };
            let lock = self.ingest_lock_for(&projection.id);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let projection = match self.load_record(&path) {
                Ok(Some(projection)) if projection.origin == origin => projection,
                Ok(_) => continue,
                Err(error) => {
                    errors.push(error);
                    continue;
                },
            };
            match self.store.drop_projection(&projection) {
                Ok(rows) => rows_deleted = rows_deleted.saturating_add(rows),
                Err(error) => {
                    errors.push(format!("drop projection {}: {error}", projection.id));
                    continue;
                },
            }
            match self.workspace_layout.remove_file_path_sync(&path) {
                Ok(()) => {
                    projections_deleted = projections_deleted.saturating_add(1);
                    self.metrics.record(ProjectionEvent::RowsPurged);
                },
                Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                },
                Err(error) => errors.push(format!("delete projection {path:?}: {error}")),
            }
        }
        if errors.is_empty() {
            Ok((projections_deleted, rows_deleted))
        } else {
            Err(ProjectionOriginPurgeError {
                projections_deleted,
                rows_deleted,
                errors,
            })
        }
    }

    /// List every projection in this scope. Used by `GET
    /// /api-mining/projections` for the Forge "Learned Resources" tab.
    pub fn list(&self) -> Vec<ProjectionIndexEntry> {
        let mut entries = Vec::new();
        let dir_iter = match self.workspace_layout.read_dir_path_sync(&self.records_dir) {
            Ok(it) => it,
            Err(_) => return entries,
        };
        for entry in dir_iter {
            if !entry.is_file {
                continue;
            }
            let path = self.entry_path(&entry);
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(Some(projection)) = self.load_record(&path) {
                let row_count = self
                    .store
                    .query_rows(&projection, None, &[])
                    .map(|rows| rows.len())
                    .unwrap_or(0);
                entries.push(ProjectionIndexEntry {
                    id: projection.id.clone(),
                    capability_id: projection.capability_id.clone(),
                    origin: projection.origin.clone(),
                    resource_label: projection.resource_label.clone(),
                    lifecycle: projection.lifecycle,
                    last_ingested_at: projection.last_ingested_at,
                    row_count,
                });
            }
        }
        entries
    }

    fn create_pending(
        &self,
        capability_id: &str,
        origin: &str,
        resource_label: String,
        response: &Value,
        record_path: &Path,
        expected_id: String,
    ) -> IngestOutcome {
        let inferred =
            match infer_projection_from_sample(capability_id, origin, resource_label, response) {
                Some(p) => p,
                None => return IngestOutcome::NotProjectable,
            };
        if inferred.id != expected_id {
            // Defensive: the id generation should agree, but if some
            // future change diverges, prefer the inferred id (it's
            // the canonical source) and rewrite the path.
        }
        if let Err(e) = self.store.create_table_for_projection(&inferred) {
            return IngestOutcome::Failed {
                reason: format!("create_table: {e}"),
            };
        }
        let projection_id = inferred.id.clone();
        let final_path = self.records_dir.join(format!("{projection_id}.json"));
        if let Err(e) = self.save_record(
            // Prefer canonical-id path so list() finds it cleanly.
            if final_path == *record_path {
                record_path
            } else {
                &final_path
            },
            &inferred,
        ) {
            return IngestOutcome::Failed { reason: e };
        }
        self.metrics.record(ProjectionEvent::PendingCreated);
        IngestOutcome::PendingCreated { projection_id }
    }

    fn ingest_into_existing(
        &self,
        projection: &mut ResourceProjection,
        projection_id: String,
        _capability_id: &str,
        response: &Value,
        record_path: &Path,
    ) -> IngestOutcome {
        if matches!(projection.lifecycle, ProjectionLifecycle::Pending) {
            return IngestOutcome::PendingExisting { projection_id };
        }
        // Schema convergence: derive columns from this sample, diff
        // against stored columns, additive-only migrate.
        let inferred = match infer_projection_from_sample(
            &projection.capability_id,
            &projection.origin,
            projection.resource_label.clone(),
            response,
        ) {
            Some(p) => p,
            None => return IngestOutcome::NotProjectable,
        };
        let schema_added = match self.store.migrate_table(projection, &inferred.columns) {
            Ok(n) => n,
            Err(e) => {
                self.metrics.record(ProjectionEvent::MigrationRejected);
                return IngestOutcome::MigrationRejected {
                    projection_id,
                    reason: e,
                };
            },
        };
        let now = chrono::Utc::now().timestamp_millis();
        let mut needs_save = false;
        if schema_added > 0 {
            // Union the new columns into the stored projection record
            // so subsequent samples see them. Without this, the next
            // ingest would re-attempt `ALTER TABLE ADD COLUMN` for the
            // same column (because the projection record still shows
            // the pre-convergence schema) — SQLite errors with
            // "duplicate column name" and the operator sees a confusing
            // MigrationRejected.
            let mut by_name: HashMap<String, _> = projection
                .columns
                .iter()
                .map(|c| (c.name.clone(), c.clone()))
                .collect();
            for col in &inferred.columns {
                by_name
                    .entry(col.name.clone())
                    .or_insert_with(|| col.clone());
            }
            let mut merged: Vec<_> = by_name.into_values().collect();
            merged.sort_by(|a, b| a.name.cmp(&b.name));
            projection.columns = merged;
            projection.updated_at = now;
            needs_save = true;
            self.metrics.record(ProjectionEvent::SchemaConverged);
        }
        let rows = extract_rows(projection, response);
        let row_count = match self.store.insert_rows(projection, &rows) {
            Ok(n) => n,
            Err(e) => return IngestOutcome::Failed { reason: e },
        };
        if row_count > 0 {
            self.metrics.record(ProjectionEvent::RowsIngested);
            projection.last_ingested_at = Some(now);
            projection.updated_at = now;
            if matches!(projection.lifecycle, ProjectionLifecycle::Approved) {
                projection.lifecycle = ProjectionLifecycle::Live;
            }
            needs_save = true;
        }
        // Persist whenever ANY observable state changed (schema OR
        // rows OR lifecycle). Skipping the save when only the schema
        // converged was the bug — the SQLite table would have new
        // columns while the on-disk projection JSON still showed the
        // old schema, desyncing the next migrate_table call.
        if needs_save {
            if let Err(e) = self.save_record(record_path, projection) {
                return IngestOutcome::Failed { reason: e };
            }
        }
        IngestOutcome::Ingested {
            projection_id,
            row_count,
            schema_added_columns: schema_added,
        }
    }

    fn load_record(&self, path: &Path) -> Result<Option<ResourceProjection>, String> {
        let raw = match self.workspace_layout.read_to_string_path_sync(path) {
            Ok(raw) => raw,
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            },
            Err(error) => return Err(format!("read {path:?}: {error}")),
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| format!("parse {path:?}: {e}"))
    }

    fn save_record(&self, path: &Path, projection: &ResourceProjection) -> Result<(), String> {
        let raw = serde_json::to_string_pretty(projection)
            .map_err(|e| format!("serialize projection: {e}"))?;
        self.workspace_layout
            .write_atomic_path_sync(path, raw.as_bytes())
            .map_err(|e| format!("write {path:?}: {e}"))
    }

    fn entry_path(&self, entry: &WorkspaceFileEntry) -> PathBuf {
        self.records_dir.join(&entry.relative_path)
    }
}

/// Process-wide per-(principal, workspace) cache of constructed
/// `ProjectionPipelineState` handles. Both the executor's hot-path
/// ingest hook AND the HTTP API resolve through this registry so
/// writes (replay → ingest) and reads (`query_known_resource`,
/// `/api-mining/projections`) hit the same pipeline + SQLite
/// connection. Without this, the executor would construct its own
/// pipeline and the HTTP API would never see the rows it ingests.
static SCOPED_PIPELINE_REGISTRY: OnceLock<
    RwLock<HashMap<(String, String), Arc<ProjectionPipelineState>>>,
> = OnceLock::new();

fn pipeline_registry() -> &'static RwLock<HashMap<(String, String), Arc<ProjectionPipelineState>>> {
    SCOPED_PIPELINE_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Resolve (or lazily construct) the shared `ProjectionPipelineState`
/// for a scope. `records_dir` is used only on first construction —
/// subsequent calls with the same `(principal, workspace)` return
/// the cached handle regardless of `records_dir`. Callers should
/// resolve via the standard `<scope_root>/api_mining/projections`
/// path to guarantee everyone agrees on the records directory.
pub fn pipeline_for_scope(
    principal: &str,
    workspace: &str,
    records_dir: &Path,
) -> Result<Arc<ProjectionPipelineState>, String> {
    let key = (principal.to_string(), workspace.to_string());
    {
        let read = pipeline_registry()
            .read()
            .map_err(|_| "pipeline registry RwLock poisoned".to_string())?;
        if let Some(p) = read.get(&key) {
            return Ok(Arc::clone(p));
        }
    }
    let mut write = pipeline_registry()
        .write()
        .map_err(|_| "pipeline registry RwLock poisoned".to_string())?;
    if let Some(p) = write.get(&key) {
        return Ok(Arc::clone(p));
    }
    let metrics = super::metrics::projection_metrics_for_scope(principal, workspace);
    let pipeline = Arc::new(ProjectionPipelineState::open(
        records_dir.to_path_buf(),
        metrics,
    )?);
    write.insert(key, Arc::clone(&pipeline));
    Ok(pipeline)
}

/// Evict a scope's cached SQLite handle after destructive cleanup.
///
/// Disable-and-purge removes the projection database itself. Keeping the old
/// connection in the process registry would make a later re-enable write into
/// an unlinked SQLite file on Unix, so the next access must open a fresh handle.
pub fn forget_pipeline_for_scope(principal: &str, workspace: &str) -> bool {
    pipeline_registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(principal.to_owned(), workspace.to_owned()))
        .is_some()
}

fn extract_origin(url_template: &str) -> String {
    if let Some(scheme_end) = url_template.find("://") {
        let after_scheme = &url_template[scheme_end + 3..];
        if let Some(slash) = after_scheme.find('/') {
            return url_template[..scheme_end + 3 + slash].to_string();
        }
        return url_template.to_string();
    }
    url_template
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or(url_template)
        .to_string()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pending_then_approved_ingest_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let metrics = Arc::new(ProjectionMetrics::default());
        let pipeline =
            ProjectionPipelineState::open(tmp.path().to_path_buf(), Arc::clone(&metrics)).unwrap();
        let payload = json!({"items": [{"id": "a", "n": 1}, {"id": "b", "n": 2}]});

        // Visit 1: pending created.
        let outcome = pipeline.ingest_response("cap-1", "https://x.com/api/items", &payload);
        let id = match outcome {
            IngestOutcome::PendingCreated { projection_id } => projection_id,
            other => panic!("unexpected first visit: {other:?}"),
        };
        assert_eq!(metrics.snapshot().pending_created, 1);

        // Visit 2 before approval: still pending.
        let outcome2 = pipeline.ingest_response("cap-1", "https://x.com/api/items", &payload);
        assert!(matches!(outcome2, IngestOutcome::PendingExisting { .. }));

        // Approve, then ingest.
        pipeline.approve_projection(&id).unwrap();
        let outcome3 = pipeline.ingest_response("cap-1", "https://x.com/api/items", &payload);
        assert!(matches!(
            outcome3,
            IngestOutcome::Ingested { row_count: 2, .. }
        ));
        assert_eq!(metrics.snapshot().rows_ingested, 1);

        // Query.
        let (rows, served) = pipeline
            .query_known_resource("https://x.com", "items", None, &[])
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(served, ServedFrom::Projection);
    }

    #[test]
    fn origin_purge_removes_rows_and_records_without_touching_other_sites() {
        let tmp = tempfile::tempdir().unwrap();
        let pipeline = ProjectionPipelineState::open(
            tmp.path().to_path_buf(),
            Arc::new(ProjectionMetrics::default()),
        )
        .unwrap();
        let payload = json!({"items": [{"id": "a"}, {"id": "b"}]});
        let x_id = match pipeline.ingest_response("cap-x", "https://x.example/api/items", &payload)
        {
            IngestOutcome::PendingCreated { projection_id } => projection_id,
            other => panic!("unexpected x projection outcome: {other:?}"),
        };
        pipeline.approve_projection(&x_id).unwrap();
        assert!(matches!(
            pipeline.ingest_response("cap-x", "https://x.example/api/items", &payload),
            IngestOutcome::Ingested { row_count: 2, .. }
        ));
        assert!(matches!(
            pipeline.ingest_response("cap-y", "https://y.example/api/items", &payload),
            IngestOutcome::PendingCreated { .. }
        ));

        assert_eq!(pipeline.purge_origin("https://x.example").unwrap(), (1, 2));
        let connection = rusqlite::Connection::open(pipeline.store.db_path()).unwrap();
        let x_table_count: usize = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [x_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            x_table_count, 0,
            "origin purge must remove its table schema"
        );
        assert!(matches!(
            pipeline.query_known_resource("https://x.example", "items", None, &[]),
            Err(QueryKnownResourceError::NoProjection { .. })
        ));
        assert!(pipeline
            .list()
            .iter()
            .any(|projection| projection.origin == "https://y.example"));
    }

    #[test]
    fn destructive_cleanup_evicts_the_cached_sqlite_handle() {
        let first_root = tempfile::tempdir().unwrap();
        let second_root = tempfile::tempdir().unwrap();
        let unique = ulid::Ulid::new().to_string();
        let principal = format!("projection-purge-{unique}");
        let workspace = "default";
        let first = pipeline_for_scope(&principal, workspace, first_root.path()).unwrap();
        assert!(forget_pipeline_for_scope(&principal, workspace));
        assert!(!forget_pipeline_for_scope(&principal, workspace));
        let reopened = pipeline_for_scope(&principal, workspace, second_root.path()).unwrap();
        assert!(!Arc::ptr_eq(&first, &reopened));
        assert_eq!(reopened.records_dir, second_root.path());
        forget_pipeline_for_scope(&principal, workspace);
    }
}
