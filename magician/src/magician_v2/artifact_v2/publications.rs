use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use tracing::warn;

use super::{
    models::{PublishedSurfaceIndexEntry, PublishedSurfaceIndexRecord, PublishedSurfaceRecord},
    service::ArtifactV2Error,
    workspace::ArtifactV2Workspace,
};
use crate::magician_v2::gaui::MuijStorage;

const MAX_PUBLISHED_SURFACE_RECORD_BYTES: u64 = 1024 * 1024;
const MAX_PUBLISHED_SURFACE_RECORD_DEPTH: usize = 16;
const MAX_PUBLISHED_SURFACE_RECORD_NODES: usize = 4_096;
const MAX_PUBLISHED_SURFACE_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PUBLISHED_SURFACE_INDEX_DEPTH: usize = 16;
const MAX_PUBLISHED_SURFACE_INDEX_NODES: usize = 131_072;
pub const MAX_PUBLISHED_SURFACE_RECORD_FILES_PER_SCOPE: usize = 16_384;

#[derive(Debug, Clone)]
pub struct FilesystemPublishedSurfaceStore {
    workspace: ArtifactV2Workspace,
}

/// One index lock per scope, for the life of the process.
///
/// The surfaces index is a read-modify-write: load it, add or replace one
/// entry, write the whole thing back. The store is constructed per call — there
/// are seventeen construction sites and it holds nothing but a workspace — so
/// there was no lock anywhere and nothing serialised those three steps. Two
/// concurrent publishes meant the second one loaded the index before the first
/// wrote it, and its write dropped the first's entry.
///
/// The failure is quiet in a specific way: **the record file is written
/// atomically and survives, so nothing is lost on disk** — but the surface is
/// gone from the index, which is what the UI lists. It looks like the publish
/// silently did nothing. The boot reconciler rebuilds the index from the
/// records, so it comes back on the next restart, which makes it look transient
/// rather than like a bug.
///
/// A `tokio::sync::Mutex` because the guarded section awaits. Keyed by scope so
/// two workspaces never wait on each other. One process owns a data root
/// (decided 2026-08-12), which is why an in-process lock is the whole fix.
static SURFACE_INDEX_LOCKS: std::sync::OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<(String, String), std::sync::Arc<tokio::sync::Mutex<()>>>,
    >,
> = std::sync::OnceLock::new();

fn surface_index_lock(principal: &str, workspace: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    let key = (principal.to_string(), workspace.to_string());
    let mut registry = SURFACE_INDEX_LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::sync::Arc::clone(registry.entry(key).or_default())
}

pub const PUBLISHED_SURFACE_CHANGED_EVENT_TYPE: &str = "published_surface.changed";

#[derive(Debug, Clone, Default, Serialize)]
pub struct LoadedPublishedSurfaceRecords {
    pub records: Vec<PublishedSurfaceRecord>,
    pub unreadable_records: usize,
    pub index_changed: bool,
}

pub fn published_surface_changed_payload(record: &PublishedSurfaceRecord) -> Value {
    json!({
        "surface_id": record.surface_id,
        "principal": record.principal,
        "workspace": record.workspace,
        "surface_kind": record.surface_kind,
        "status": record.status,
        "logical_surface_id": record.logical_surface_id,
        "route": record.route,
        "document_key": record.document_key,
        "task_id": record.task_id,
        "ui_thread_id": record.ui_thread_id,
        "source_output_id": record.source_output_id,
        "execution_id": record.source_execution_id,
        "published_at": record.published_at,
        "updated_at": record.updated_at,
    })
}

impl FilesystemPublishedSurfaceStore {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    pub async fn get_surface(
        &self,
        principal: &str,
        workspace: &str,
        surface_id: &str,
    ) -> Result<Option<PublishedSurfaceRecord>, ArtifactV2Error> {
        let path = self
            .workspace
            .published_surface_path(principal, workspace, surface_id);
        match self
            .workspace
            .read_json_bounded_stream_path(
                &path,
                MAX_PUBLISHED_SURFACE_RECORD_BYTES,
                MAX_PUBLISHED_SURFACE_RECORD_DEPTH,
                MAX_PUBLISHED_SURFACE_RECORD_NODES,
            )
            .await
        {
            Ok(record) => Ok(Some(record)),
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub async fn list_surfaces(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<PublishedSurfaceRecord>, ArtifactV2Error> {
        let index = self.load_index(principal, workspace).await?;
        let mut records = Vec::with_capacity(index.surfaces.len());
        for entry in index.surfaces {
            if let Some(record) = self
                .get_surface(principal, workspace, &entry.surface_id)
                .await?
            {
                records.push(record);
            }
        }
        records.sort_by(|left, right| right.published_at.cmp(&left.published_at));
        Ok(records)
    }

    pub async fn load_surface_records_from_files(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<LoadedPublishedSurfaceRecords, ArtifactV2Error> {
        self.load_surface_records_from_files_bounded(principal, workspace, usize::MAX)
            .await
    }

    /// Load authoritative record files with a hard cap on JSON bodies read.
    /// Directory names are admitted before any body is opened, preventing a
    /// damaged or hostile scope from turning repair into unbounded file I/O.
    pub async fn load_surface_records_from_files_bounded(
        &self,
        principal: &str,
        workspace: &str,
        max_record_files: usize,
    ) -> Result<LoadedPublishedSurfaceRecords, ArtifactV2Error> {
        if max_record_files == 0 {
            return Err(ArtifactV2Error::Runtime(
                "published_surface_record_limit_must_be_positive".to_owned(),
            ));
        }
        let dir = self.workspace.published_surfaces_dir(principal, workspace);
        let entries = match self.workspace.read_dir_path(&dir).await {
            Ok(entries) => entries,
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LoadedPublishedSurfaceRecords::default());
            },
            Err(err) => return Err(err),
        };

        let record_entries = entries
            .into_iter()
            .filter(|entry| {
                entry.is_file
                    && std::path::Path::new(&entry.file_name)
                        .extension()
                        .and_then(|value| value.to_str())
                        == Some("json")
            })
            .collect::<Vec<_>>();
        if record_entries.len() > max_record_files {
            return Err(ArtifactV2Error::Runtime(format!(
                "published_surface_record_limit_exceeded:{}>{max_record_files}",
                record_entries.len()
            )));
        }

        let mut loaded = LoadedPublishedSurfaceRecords::default();
        for entry in record_entries {
            let path = dir.join(&entry.file_name);

            match self
                .workspace
                .read_json_bounded_stream_path::<PublishedSurfaceRecord, _>(
                    &path,
                    MAX_PUBLISHED_SURFACE_RECORD_BYTES,
                    MAX_PUBLISHED_SURFACE_RECORD_DEPTH,
                    MAX_PUBLISHED_SURFACE_RECORD_NODES,
                )
                .await
            {
                Ok(record) => loaded.records.push(record),
                Err(err) => {
                    loaded.unreadable_records += 1;
                    warn!(
                        path = %path.display(),
                        error = %err,
                        "failed to read published surface record during reconciliation"
                    );
                },
            }
        }

        loaded
            .records
            .sort_by(|left, right| right.published_at.cmp(&left.published_at));
        Ok(loaded)
    }

    /// Atomically repair the shared index from authoritative record files and
    /// return that exact bounded snapshot. The shared scope lock protects task,
    /// agent and app publishers from a lost index update while the scan runs.
    pub async fn reconcile_index_from_record_files_bounded(
        &self,
        principal: &str,
        workspace: &str,
        max_record_files: usize,
    ) -> Result<LoadedPublishedSurfaceRecords, ArtifactV2Error> {
        self.reconcile_index_from_record_files_bounded_validated(
            principal,
            workspace,
            max_record_files,
            |_| Ok(()),
        )
        .await
    }

    /// Variant for subsystem-owned records. The validator runs after the
    /// bounded authoritative read and while the same index lock remains held,
    /// so a hostile ownership marker cannot enter navigation before its owner
    /// has admitted it and no writer can swap a record between validation and
    /// index replacement.
    pub async fn reconcile_index_from_record_files_bounded_validated<E, F>(
        &self,
        principal: &str,
        workspace: &str,
        max_record_files: usize,
        validate: F,
    ) -> Result<LoadedPublishedSurfaceRecords, E>
    where
        E: From<ArtifactV2Error> + Send,
        F: FnOnce(&[PublishedSurfaceRecord]) -> Result<(), E> + Send,
    {
        let index_lock = surface_index_lock(principal, workspace);
        let _index_guard = index_lock.lock().await;
        let loaded = self
            .load_surface_records_from_files_bounded(principal, workspace, max_record_files)
            .await
            .map_err(E::from)?;
        if loaded.unreadable_records != 0 {
            return Err(E::from(ArtifactV2Error::Runtime(format!(
                "published_surface_authoritative_records_unreadable:{}",
                loaded.unreadable_records
            ))));
        }
        if loaded
            .records
            .iter()
            .any(|record| record.principal != principal || record.workspace != workspace)
        {
            return Err(E::from(ArtifactV2Error::Runtime(
                "published_surface_authoritative_record_scope_mismatch".to_owned(),
            )));
        }
        validate(&loaded.records)?;
        let index_changed = self
            .write_index_from_records_locked(principal, workspace, &loaded.records)
            .await
            .map_err(E::from)?;
        let mut loaded = loaded;
        loaded.index_changed = index_changed;
        Ok(loaded)
    }

    /// Remove authoritative records and replace their shared index projection
    /// while holding the same scope lock used by every publisher. The returned
    /// records let callers clean up secondary payloads after the durable
    /// record/index pair has converged.
    pub async fn remove_surface_records_where<F>(
        &self,
        principal: &str,
        workspace: &str,
        max_record_files: usize,
        should_remove: F,
    ) -> Result<Vec<PublishedSurfaceRecord>, ArtifactV2Error>
    where
        F: Fn(&PublishedSurfaceRecord) -> bool + Send,
    {
        let index_lock = surface_index_lock(principal, workspace);
        let _index_guard = index_lock.lock().await;
        let loaded = self
            .load_surface_records_from_files_bounded(principal, workspace, max_record_files)
            .await?;
        if loaded.unreadable_records != 0 {
            return Err(ArtifactV2Error::Runtime(format!(
                "published_surface_authoritative_records_unreadable:{}",
                loaded.unreadable_records
            )));
        }
        if loaded
            .records
            .iter()
            .any(|record| record.principal != principal || record.workspace != workspace)
        {
            return Err(ArtifactV2Error::Runtime(
                "published_surface_authoritative_record_scope_mismatch".to_owned(),
            ));
        }

        let (removed, retained): (Vec<_>, Vec<_>) = loaded
            .records
            .into_iter()
            .partition(|record| should_remove(record));
        if removed.is_empty() {
            return Ok(removed);
        }
        for record in &removed {
            let path =
                self.workspace
                    .published_surface_path(principal, workspace, &record.surface_id);
            match self.workspace.remove_file_path(&path).await {
                Ok(()) => {},
                Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {},
                Err(err) => {
                    // Some prior removals may already be durable. Rebuild from
                    // the files that actually remain before surfacing failure.
                    if let Ok(current) = self
                        .load_surface_records_from_files_bounded(
                            principal,
                            workspace,
                            max_record_files,
                        )
                        .await
                    {
                        if current.unreadable_records == 0
                            && current.records.iter().all(|record| {
                                record.principal == principal && record.workspace == workspace
                            })
                        {
                            let _ = self
                                .write_index_from_records_locked(
                                    principal,
                                    workspace,
                                    &current.records,
                                )
                                .await;
                        }
                    }
                    return Err(err);
                },
            }
        }
        self.write_index_from_records_locked(principal, workspace, &retained)
            .await?;
        Ok(removed)
    }

    pub async fn upsert_surface(
        &self,
        record: &PublishedSurfaceRecord,
    ) -> Result<PublishedSurfaceRecord, ArtifactV2Error> {
        self.upsert_surfaces(std::slice::from_ref(record)).await?;
        Ok(record.clone())
    }

    /// Upsert one same-scope projection set with a single index rewrite.
    /// Individual record files remain atomic and authoritative; the shared
    /// index lock protects the one read-modify-write across all candidates.
    pub async fn upsert_surfaces(
        &self,
        records: &[PublishedSurfaceRecord],
    ) -> Result<Vec<PublishedSurfaceRecord>, ArtifactV2Error> {
        let Some(first) = records.first() else {
            return Ok(Vec::new());
        };
        if records.iter().any(|record| {
            record.principal != first.principal || record.workspace != first.workspace
        }) {
            return Err(ArtifactV2Error::Runtime(
                "published_surface_batch_scope_mismatch".to_owned(),
            ));
        }
        let mut identities = std::collections::HashSet::with_capacity(records.len());
        if records
            .iter()
            .any(|record| !identities.insert(record.surface_id.as_str()))
        {
            return Err(ArtifactV2Error::Runtime(
                "published_surface_batch_duplicate_identity".to_owned(),
            ));
        }
        self.workspace.ensure_root().await?;
        self.workspace
            .create_dir_all_path(
                self.workspace
                    .published_surfaces_dir(&first.principal, &first.workspace),
            )
            .await?;
        self.workspace
            .create_dir_all_path(
                self.workspace
                    .ui_indexes_dir(&first.principal, &first.workspace),
            )
            .await?;

        // Same-surface writers must serialize the authoritative record and its
        // index projection as one ordered unit. Locking only the index permits
        // record A -> record B but index B -> index A, leaving the pair split.
        let index_lock = surface_index_lock(&first.principal, &first.workspace);
        let _index_guard = index_lock.lock().await;

        for record in records {
            let path = self.workspace.published_surface_path(
                &record.principal,
                &record.workspace,
                &record.surface_id,
            );
            self.workspace.write_json_atomic_path(&path, record).await?;
        }

        let mut index = self.load_index(&first.principal, &first.workspace).await?;
        if index.surfaces.is_empty() {
            index.updated_at = records
                .iter()
                .map(|record| record.updated_at.as_str())
                .max()
                .unwrap_or(index.updated_at.as_str())
                .to_owned();
        }
        for record in records {
            if record.updated_at > index.updated_at {
                index.updated_at = record.updated_at.clone();
            }
            upsert_index_entry(&mut index.surfaces, index_entry(record));
        }
        let index_path = self
            .workspace
            .published_surfaces_index_path(&first.principal, &first.workspace);
        self.workspace
            .write_json_atomic_path(&index_path, &index)
            .await?;
        Ok(records.to_vec())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn rewrite_index_from_records(
        &self,
        principal: &str,
        workspace: &str,
        records: &[PublishedSurfaceRecord],
    ) -> Result<bool, ArtifactV2Error> {
        let index_lock = surface_index_lock(principal, workspace);
        let _index_guard = index_lock.lock().await;

        self.write_index_from_records_locked(principal, workspace, records)
            .await
    }

    async fn write_index_from_records_locked(
        &self,
        principal: &str,
        workspace: &str,
        records: &[PublishedSurfaceRecord],
    ) -> Result<bool, ArtifactV2Error> {
        self.workspace.ensure_root().await?;
        self.workspace
            .create_dir_all_path(self.workspace.ui_indexes_dir(principal, workspace))
            .await?;

        let existing = self.load_index(principal, workspace).await?;
        let mut rebuilt = PublishedSurfaceIndexRecord {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            surfaces: records.iter().map(index_entry).collect(),
            updated_at: records
                .iter()
                .map(|record| record.updated_at.as_str())
                .max()
                .unwrap_or(existing.updated_at.as_str())
                .to_string(),
        };
        rebuilt
            .surfaces
            .sort_by(|left, right| right.published_at.cmp(&left.published_at));

        if rebuilt == existing {
            return Ok(false);
        }

        let index_path = self
            .workspace
            .published_surfaces_index_path(principal, workspace);
        self.workspace
            .write_json_atomic_path(&index_path, &rebuilt)
            .await?;
        Ok(true)
    }

    pub async fn mark_surface_superseded(
        &self,
        principal: &str,
        workspace: &str,
        surface_id: &str,
    ) -> Result<Option<PublishedSurfaceRecord>, ArtifactV2Error> {
        self.mark_surface_status(principal, workspace, surface_id, "superseded")
            .await
    }

    pub async fn mark_surface_unpublished(
        &self,
        principal: &str,
        workspace: &str,
        surface_id: &str,
    ) -> Result<Option<PublishedSurfaceRecord>, ArtifactV2Error> {
        self.mark_surface_status(principal, workspace, surface_id, "unpublished")
            .await
    }

    pub async fn document_key_is_referenced(
        &self,
        principal: &str,
        workspace: &str,
        document_key: &str,
        excluded_surface_id: Option<&str>,
    ) -> Result<bool, ArtifactV2Error> {
        let loaded = self
            .load_surface_records_from_files(principal, workspace)
            .await?;
        Ok(loaded.records.iter().any(|record| {
            if excluded_surface_id == Some(record.surface_id.as_str()) {
                return false;
            }
            referenced_materialized_document_key(record).as_deref() == Some(document_key)
        }))
    }

    pub async fn delete_orphaned_materialized_layouts(
        &self,
        muij_storage: &MuijStorage,
        referenced_document_keys: &std::collections::HashSet<String>,
    ) -> Result<usize, ArtifactV2Error> {
        let document_keys = muij_storage
            .list_surface_document_keys()
            .await
            .map_err(|err| {
                ArtifactV2Error::Runtime(format!("published_surface_layout_list_failed:{err}"))
            })?;

        let mut removed = 0_usize;
        for document_key in document_keys {
            if referenced_document_keys.contains(&document_key) {
                continue;
            }
            muij_storage
                .delete_surface_layout(&document_key)
                .await
                .map_err(|err| {
                    ArtifactV2Error::Runtime(format!(
                        "published_surface_layout_delete_failed:{document_key}:{err}"
                    ))
                })?;
            removed += 1;
        }

        Ok(removed)
    }

    async fn load_index(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<PublishedSurfaceIndexRecord, ArtifactV2Error> {
        let path = self
            .workspace
            .published_surfaces_index_path(principal, workspace);
        match self
            .workspace
            .read_json_bounded_stream_path(
                &path,
                MAX_PUBLISHED_SURFACE_INDEX_BYTES,
                MAX_PUBLISHED_SURFACE_INDEX_DEPTH,
                MAX_PUBLISHED_SURFACE_INDEX_NODES,
            )
            .await
        {
            Ok(index) => Ok(index),
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(PublishedSurfaceIndexRecord {
                    principal: principal.to_string(),
                    workspace: workspace.to_string(),
                    surfaces: Vec::new(),
                    updated_at: Utc::now().to_rfc3339(),
                })
            },
            Err(err) => Err(err),
        }
    }

    async fn mark_surface_status(
        &self,
        principal: &str,
        workspace: &str,
        surface_id: &str,
        status: &str,
    ) -> Result<Option<PublishedSurfaceRecord>, ArtifactV2Error> {
        let Some(mut record) = self.get_surface(principal, workspace, surface_id).await? else {
            return Ok(None);
        };
        let now = Utc::now().to_rfc3339();
        record.status = status.to_string();
        if record.unpublished_at.is_none() {
            record.unpublished_at = Some(now.clone());
        }
        record.updated_at = now;
        self.upsert_surface(&record).await?;
        Ok(Some(record))
    }
}

fn index_entry(record: &PublishedSurfaceRecord) -> PublishedSurfaceIndexEntry {
    PublishedSurfaceIndexEntry {
        surface_id: record.surface_id.clone(),
        surface_kind: record.surface_kind.clone(),
        status: record.status.clone(),
        logical_surface_id: record.logical_surface_id.clone(),
        route: record.route.clone(),
        document_key: record.document_key.clone(),
        task_id: record.task_id.clone(),
        ui_thread_id: record.ui_thread_id.clone(),
        source_output_id: record.source_output_id.clone(),
        materialized_render_kind: record.materialized_render_kind.clone(),
        materialized_document_key: record.materialized_document_key.clone(),
        title: record.title.clone(),
        summary: record.summary.clone(),
        placement: record.placement.clone(),
        published_at: record.published_at.clone(),
        unpublished_at: record.unpublished_at.clone(),
        updated_at: record.updated_at.clone(),
    }
}

fn upsert_index_entry(
    entries: &mut Vec<PublishedSurfaceIndexEntry>,
    entry: PublishedSurfaceIndexEntry,
) {
    if let Some(existing) = entries
        .iter_mut()
        .find(|existing| existing.surface_id == entry.surface_id)
    {
        *existing = entry;
    } else {
        entries.push(entry);
    }
    entries.sort_by(|left, right| right.published_at.cmp(&left.published_at));
}

fn referenced_materialized_document_key(record: &PublishedSurfaceRecord) -> Option<String> {
    if record.materialized_render_kind.as_deref() != Some("muij_surface") {
        return None;
    }
    record
        .materialized_document_key
        .clone()
        .or_else(|| Some(record.document_key.clone()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashSet;

    use chrono::Utc;
    use tempfile::tempdir;

    use super::{
        FilesystemPublishedSurfaceStore, MAX_PUBLISHED_SURFACE_INDEX_BYTES,
        MAX_PUBLISHED_SURFACE_RECORD_BYTES, MAX_PUBLISHED_SURFACE_RECORD_DEPTH,
    };
    use crate::magician_v2::{
        artifact_v2::{
            models::{
                PublishedSurfaceIndexRecord, PublishedSurfacePlacement, PublishedSurfaceRecord,
            },
            workspace::ArtifactV2Workspace,
        },
        gaui::{MuijDocument, MuijStorage},
    };

    fn sample_record(surface_id: &str) -> PublishedSurfaceRecord {
        let now = Utc::now().to_rfc3339();
        PublishedSurfaceRecord {
            surface_id: surface_id.to_string(),
            principal: "principal-a".to_string(),
            workspace: "workspace-a".to_string(),
            surface_kind: "dashboard".to_string(),
            status: "active".to_string(),
            logical_surface_id: Some("logical-dashboard".to_string()),
            route: "/briefing".to_string(),
            document_key: format!("doc-{surface_id}"),
            task_id: Some("task-1".to_string()),
            ui_thread_id: Some("thread-1".to_string()),
            source_output_id: Some("out-task-user-1".to_string()),
            source_execution_id: Some("exec-1".to_string()),
            media_type: Some("text/markdown".to_string()),
            materialized_render_kind: Some("muij_surface".to_string()),
            materialized_document_key: Some(format!("doc-{surface_id}")),
            materialized_at: Some(now.clone()),
            title: "Dashboard".to_string(),
            summary: Some("summary".to_string()),
            placement: PublishedSurfacePlacement {
                placement_kind: "workspace".to_string(),
                placement_id: Some("workspace-a".to_string()),
                pinned: true,
            },
            manifest_artifact_uid: None,
            manifest_name: None,
            input_artifact_ids: Vec::new(),
            published_at: now.clone(),
            unpublished_at: None,
            updated_at: now,
        }
    }

    /// The index is a read-modify-write over one file that every publish
    /// rewrites whole. Unguarded, the second publish loads the index before the
    /// first has written it, and its write drops the first's entry — the record
    /// file survives, so nothing is lost on disk, but the surface disappears
    /// from the list the UI renders.
    #[tokio::test]
    async fn concurrent_publishes_both_survive_in_the_index() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = std::sync::Arc::new(FilesystemPublishedSurfaceStore::new(workspace));

        // Seed the scope so both writers race the index rather than the
        // directory creation that precedes it.
        store
            .upsert_surface(&sample_record("surface-seed"))
            .await
            .expect("seed publish");

        let mut joins = Vec::new();
        for index in 0..8 {
            let store = std::sync::Arc::clone(&store);
            joins.push(tokio::spawn(async move {
                store
                    .upsert_surface(&sample_record(&format!("surface-{index}")))
                    .await
                    .expect("concurrent publish")
            }));
        }
        for join in joins {
            join.await.expect("publish task");
        }

        let listed = store
            .list_surfaces("principal-a", "workspace-a")
            .await
            .expect("list");
        let ids: std::collections::BTreeSet<_> = listed
            .iter()
            .map(|record| record.surface_id.as_str())
            .collect();
        for index in 0..8 {
            assert!(
                ids.contains(format!("surface-{index}").as_str()),
                "surface-{index} is missing from the index; ids = {ids:?}"
            );
        }
        assert!(ids.contains("surface-seed"), "the seed publish was dropped");
    }

    #[tokio::test]
    async fn batch_upsert_preserves_existing_task_surface_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = FilesystemPublishedSurfaceStore::new(workspace);
        let task = sample_record("task-surface");
        store.upsert_surface(&task).await.unwrap();

        let mut first_app = sample_record("app-surface:first");
        first_app.surface_kind = "app".to_owned();
        first_app.task_id = None;
        let mut second_app = first_app.clone();
        second_app.surface_id = "app-surface:second".to_owned();
        second_app.document_key = "app:second".to_owned();
        store
            .upsert_surfaces(&[first_app, second_app])
            .await
            .unwrap();

        let listed = store
            .list_surfaces("principal-a", "workspace-a")
            .await
            .unwrap();
        assert_eq!(listed.len(), 3);
        assert!(listed.iter().any(|record| {
            record.surface_id == task.surface_id && record.task_id == task.task_id
        }));
    }

    #[tokio::test]
    async fn scoped_removal_keeps_unrelated_authoritative_publications_in_the_index() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = FilesystemPublishedSurfaceStore::new(workspace.clone());
        let task = sample_record("task-surface");
        let mut app = sample_record("app-surface");
        app.task_id = None;
        app.surface_kind = "app".to_owned();
        store
            .upsert_surfaces(&[task.clone(), app.clone()])
            .await
            .unwrap();

        let removed = store
            .remove_surface_records_where(&task.principal, &task.workspace, 8, |record| {
                record.task_id.as_deref() == Some("task-1")
            })
            .await
            .unwrap();

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].surface_id, task.surface_id);
        assert!(store
            .get_surface(&task.principal, &task.workspace, &task.surface_id)
            .await
            .unwrap()
            .is_none());
        let listed = store
            .list_surfaces(&app.principal, &app.workspace)
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].surface_id, app.surface_id);
    }

    #[tokio::test]
    async fn rewrite_index_from_records_repairs_stale_index() {
        let tempdir = tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tempdir.path());
        let store = FilesystemPublishedSurfaceStore::new(workspace.clone());
        let record = sample_record("surface-1");
        store.upsert_surface(&record).await.unwrap();

        let stale_index = PublishedSurfaceIndexRecord {
            principal: record.principal.clone(),
            workspace: record.workspace.clone(),
            surfaces: Vec::new(),
            updated_at: record.updated_at.clone(),
        };
        workspace
            .write_json_atomic_path(
                workspace.published_surfaces_index_path(&record.principal, &record.workspace),
                &stale_index,
            )
            .await
            .unwrap();

        let loaded = store
            .load_surface_records_from_files(&record.principal, &record.workspace)
            .await
            .unwrap();
        assert_eq!(loaded.records.len(), 1);

        let changed = store
            .rewrite_index_from_records(&record.principal, &record.workspace, &loaded.records)
            .await
            .unwrap();
        assert!(changed);

        let repaired: PublishedSurfaceIndexRecord = workspace
            .read_json_path(
                workspace.published_surfaces_index_path(&record.principal, &record.workspace),
            )
            .await
            .unwrap();
        assert_eq!(repaired.surfaces.len(), 1);
        assert_eq!(repaired.surfaces[0].surface_id, record.surface_id);
    }

    #[tokio::test]
    async fn authoritative_repair_fails_before_reads_or_index_write_past_its_cap() {
        let tempdir = tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tempdir.path());
        let store = FilesystemPublishedSurfaceStore::new(workspace.clone());
        store
            .upsert_surface(&sample_record("surface-1"))
            .await
            .unwrap();
        store
            .upsert_surface(&sample_record("surface-2"))
            .await
            .unwrap();
        let index_path = workspace.published_surfaces_index_path("principal-a", "workspace-a");
        let before: PublishedSurfaceIndexRecord =
            workspace.read_json_path(&index_path).await.unwrap();

        let error = store
            .reconcile_index_from_record_files_bounded("principal-a", "workspace-a", 1)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("limit"));
        let after: PublishedSurfaceIndexRecord =
            workspace.read_json_path(&index_path).await.unwrap();
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn authoritative_record_loader_bounds_each_body_before_typed_deserialization() {
        let tempdir = tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tempdir.path());
        let store = FilesystemPublishedSurfaceStore::new(workspace.clone());
        let record = sample_record("surface-hostile");
        store.upsert_surface(&record).await.unwrap();
        let path = workspace.published_surface_path(
            &record.principal,
            &record.workspace,
            &record.surface_id,
        );

        std::fs::write(
            &path,
            vec![b' '; MAX_PUBLISHED_SURFACE_RECORD_BYTES as usize + 1],
        )
        .unwrap();
        let oversized = store
            .load_surface_records_from_files_bounded(&record.principal, &record.workspace, 1)
            .await
            .unwrap();
        assert!(oversized.records.is_empty());
        assert_eq!(oversized.unreadable_records, 1);

        let deeply_nested = format!(
            "{}0{}",
            "[".repeat(MAX_PUBLISHED_SURFACE_RECORD_DEPTH + 1),
            "]".repeat(MAX_PUBLISHED_SURFACE_RECORD_DEPTH + 1),
        );
        std::fs::write(&path, deeply_nested).unwrap();
        let deep = store
            .load_surface_records_from_files_bounded(&record.principal, &record.workspace, 1)
            .await
            .unwrap();
        assert!(deep.records.is_empty());
        assert_eq!(deep.unreadable_records, 1);
    }

    #[tokio::test]
    async fn direct_surface_and_index_reads_are_bounded_before_typed_deserialization() {
        let tempdir = tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tempdir.path());
        let store = FilesystemPublishedSurfaceStore::new(workspace.clone());
        let record = sample_record("surface-direct-hostile");
        store.upsert_surface(&record).await.unwrap();

        let record_path = workspace.published_surface_path(
            &record.principal,
            &record.workspace,
            &record.surface_id,
        );
        std::fs::write(
            record_path,
            vec![b' '; MAX_PUBLISHED_SURFACE_RECORD_BYTES as usize + 1],
        )
        .unwrap();
        assert!(store
            .get_surface(&record.principal, &record.workspace, &record.surface_id)
            .await
            .is_err());

        let index_path =
            workspace.published_surfaces_index_path(&record.principal, &record.workspace);
        std::fs::write(
            index_path,
            vec![b' '; MAX_PUBLISHED_SURFACE_INDEX_BYTES as usize + 1],
        )
        .unwrap();
        assert!(store
            .list_surfaces(&record.principal, &record.workspace)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn delete_orphaned_materialized_layouts_prunes_unreferenced_keys() {
        let tempdir = tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tempdir.path().join("artifact_v3"));
        let store = FilesystemPublishedSurfaceStore::new(workspace);
        let muij_storage = MuijStorage::new(tempdir.path().join("muij"));

        let keep_doc = MuijDocument::new("keep-doc");
        let drop_doc = MuijDocument::new("drop-doc");
        muij_storage
            .write_surface_layout("keep-doc", &keep_doc)
            .await
            .unwrap();
        muij_storage
            .write_surface_layout("drop-doc", &drop_doc)
            .await
            .unwrap();

        let removed = store
            .delete_orphaned_materialized_layouts(
                &muij_storage,
                &HashSet::from([String::from("keep-doc")]),
            )
            .await
            .unwrap();
        assert_eq!(removed, 1);
        assert!(muij_storage
            .surface_layout_exists("keep-doc")
            .await
            .unwrap());
        assert!(!muij_storage
            .surface_layout_exists("drop-doc")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn concurrent_same_surface_writes_leave_record_and_index_on_the_same_version() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = std::sync::Arc::new(FilesystemPublishedSurfaceStore::new(workspace.clone()));
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(16));
        let mut joins = Vec::new();
        for index in 0..16 {
            let store = std::sync::Arc::clone(&store);
            let barrier = std::sync::Arc::clone(&barrier);
            joins.push(tokio::spawn(async move {
                let mut record = sample_record("surface-shared");
                record.title = format!("version-{index}");
                record.updated_at = format!("2026-08-17T00:00:{index:02}Z");
                barrier.wait().await;
                store
                    .upsert_surface(&record)
                    .await
                    .expect("same-id publish");
            }));
        }
        for join in joins {
            join.await.expect("publish task");
        }

        let record = store
            .get_surface("principal-a", "workspace-a", "surface-shared")
            .await
            .unwrap()
            .unwrap();
        let index: PublishedSurfaceIndexRecord = workspace
            .read_json_path(workspace.published_surfaces_index_path("principal-a", "workspace-a"))
            .await
            .unwrap();
        let entry = index
            .surfaces
            .iter()
            .find(|entry| entry.surface_id == "surface-shared")
            .unwrap();
        assert_eq!(entry.title, record.title);
        assert_eq!(entry.updated_at, record.updated_at);
        assert_eq!(entry.status, record.status);
    }
}
