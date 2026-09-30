use crate::magician_v2::storage_governance::database_maintenance::{
    configure_connection, inspect_fragmentation, DatabaseGate, DatabaseMaintenanceConfig,
    DatabasePermit, DatabaseReadConnection, Fragmentation,
};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use duckdb::{params, params_from_iter, types::Value as DuckValue, Connection};
use fs2::FileExt;

use super::types::{FeedItem, FeedItemStatus, FeedItemType};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

const BOOTSTRAP_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS feed_items (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    item_type TEXT NOT NULL,
    task_id TEXT NULL,
    ui_thread_id TEXT NULL,
    agent_id TEXT NULL,
    title TEXT NOT NULL,
    summary TEXT NULL,
    status TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    actions_json JSON NOT NULL,
    metadata_json JSON NOT NULL,
    attention_lane TEXT NULL,
    PRIMARY KEY (principal, workspace, id)
);
CREATE INDEX IF NOT EXISTS idx_feed_scope_updated
    ON feed_items (principal, workspace, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_feed_scope_updated_keyset
    ON feed_items (principal, workspace, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_feed_scope_status_updated
    ON feed_items (principal, workspace, status, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_feed_scope_status_updated_keyset
    ON feed_items (principal, workspace, status, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_feed_scope_thread_updated
    ON feed_items (principal, workspace, ui_thread_id, updated_at DESC, id);
CREATE INDEX IF NOT EXISTS idx_feed_scope_agent_updated
    ON feed_items (principal, workspace, agent_id, updated_at DESC, id);
CREATE INDEX IF NOT EXISTS idx_feed_scope_task_updated
    ON feed_items (principal, workspace, task_id, updated_at DESC, id);
CREATE INDEX IF NOT EXISTS idx_feed_scope_attention_lane_updated
    ON feed_items (principal, workspace, attention_lane, updated_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS feed_attention_items (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    lane TEXT NOT NULL,
    projection_source TEXT NOT NULL,
    projection_group TEXT NOT NULL,
    item_type TEXT NOT NULL,
    task_id TEXT NULL,
    ui_thread_id TEXT NULL,
    agent_id TEXT NULL,
    title TEXT NOT NULL,
    summary TEXT NULL,
    status TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    actions_json JSON NOT NULL,
    metadata_json JSON NOT NULL,
    PRIMARY KEY (principal, workspace, id)
);
CREATE INDEX IF NOT EXISTS idx_feed_attention_scope_lane_updated
    ON feed_attention_items (principal, workspace, lane, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_feed_attention_scope_projection_group
    ON feed_attention_items (principal, workspace, projection_source, projection_group);

CREATE TABLE IF NOT EXISTS feed_attention_projection_groups (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    projection_source TEXT NOT NULL,
    projection_group TEXT NOT NULL,
    source_generation BIGINT NOT NULL,
    recorded_at BIGINT NOT NULL,
    PRIMARY KEY (principal, workspace, projection_source, projection_group)
);

CREATE TABLE IF NOT EXISTS feed_attention_dismissals (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    dismissed BOOLEAN NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (principal, workspace, id)
);
"#;

// FeedStore is a derived UI read model. Multi-gigabyte files indicate the
// historical checkpoint/page-churn failure mode and can terminate inside
// DuckDB before a recoverable Rust error is returned. Quarantine these before
// opening them; authoritative task/HITL sources rebuild the active projection.
const MAX_FEED_QUARANTINE_FILES: usize = 1;
const MAX_FEED_QUARANTINE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FeedStore {
    maintenance: DatabaseMaintenanceConfig,
    base_root: PathBuf,
    workspace_layout: ArtifactV2Workspace,
    scoped: Arc<Mutex<HashMap<(String, String), Arc<FeedStoreInner>>>>,
    scope_gates: Arc<tokio::sync::Mutex<HashMap<(String, String), Arc<tokio::sync::Mutex<()>>>>>,
}

struct FeedStoreInner {
    admission: Arc<DatabaseGate>,
    db_path: PathBuf,
    write_conn: Mutex<Connection>,
    write_lock: Mutex<File>,
}

impl std::fmt::Debug for FeedStoreInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedStoreInner")
            .field("db_path", &self.db_path)
            .finish_non_exhaustive()
    }
}

struct CrossProcessWriteGuard<'a> {
    _admission: Option<DatabasePermit>,
    file: std::sync::MutexGuard<'a, File>,
}

#[derive(Debug, Clone, Default)]
pub struct FeedQuery {
    pub principal: String,
    pub workspace: String,
    pub before: Option<i64>,
    pub after: Option<i64>,
    pub limit: usize,
    pub task_id: Option<String>,
    pub ui_thread_id: Option<String>,
    pub item_type: Option<FeedItemType>,
    pub status: Option<FeedItemStatus>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedPageCursor {
    pub updated_at: i64,
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct FeedPage {
    pub items: Vec<FeedItem>,
    pub has_more: bool,
    pub next_cursor: Option<FeedPageCursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedAttentionLane {
    Requests,
    Approvals,
    Escalations,
    Failed,
    Running,
}

impl FeedAttentionLane {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Requests => "requests",
            Self::Approvals => "approvals",
            Self::Escalations => "escalations",
            Self::Failed => "failed",
            Self::Running => "running",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FeedAttentionPageQuery {
    pub principal: String,
    pub workspace: String,
    pub lane: FeedAttentionLane,
    pub ui_thread_id: Option<String>,
    pub cursor: Option<FeedPageCursor>,
    pub offset: usize,
    pub limit: usize,
    /// Task-owned rows whose task_id is in this set are excluded — used to
    /// keep INTERNAL tasks out of user-facing lanes (their failures are
    /// internal-task-lane material, not operator attention). Empty = no
    /// exclusion.
    pub exclude_task_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FeedAttentionPage {
    pub items: Vec<FeedItem>,
    pub total: u64,
    pub request_hitl_total: Option<u64>,
    pub has_more: bool,
    pub next_cursor: Option<FeedPageCursor>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttentionProjectionReconcile {
    pub upserted: usize,
    pub removed: usize,
    pub skipped_stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FeedCounts {
    pub total: u64,
    pub running: u64,
    pub needs_action: u64,
    pub failed: u64,
    pub done: u64,
    pub info: u64,
}

use serde::{Deserialize, Serialize};

impl FeedStore {
    pub fn with_database_maintenance(mut self, config: DatabaseMaintenanceConfig) -> Self {
        self.maintenance = config;
        self
    }

    pub fn open(base_root: &Path) -> Result<Self> {
        Self::open_workspace(ArtifactV2Workspace::new(base_root))
    }

    pub fn open_workspace(workspace_layout: ArtifactV2Workspace) -> Result<Self> {
        let base_root = workspace_layout.base_root().to_path_buf();
        workspace_layout
            .ensure_root_sync()
            .with_context(|| format!("creating feed workspace root: {}", base_root.display()))?;
        Ok(Self {
            maintenance: DatabaseMaintenanceConfig::default(),
            base_root,
            workspace_layout,
            scoped: Arc::new(Mutex::new(HashMap::new())),
            scope_gates: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        })
    }

    async fn scope_arc(&self, principal: &str, workspace: &str) -> Result<Arc<FeedStoreInner>> {
        let gate = self.scope_io_gate(principal, workspace).await;
        let _gate = gate.lock().await;
        let key = (principal.to_string(), workspace.to_string());
        if let Some(existing) = self
            .scoped
            .lock()
            .expect("feed scoped store mutex poisoned")
            .get(&key)
            .cloned()
        {
            return Ok(existing);
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        magician_core::blocking_admission::spawn_blocking_admitted(move || {
            store.scope_inner(&principal, &workspace)
        })
        .await
        .context("feed scope open task panicked")?
    }

    async fn scope_io_gate(&self, principal: &str, workspace: &str) -> Arc<tokio::sync::Mutex<()>> {
        let key = (principal.to_string(), workspace.to_string());
        let mut gates = self.scope_gates.lock().await;
        Arc::clone(
            gates
                .entry(key)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    async fn spawn_feed_blocking<T, F>(
        &self,
        principal: String,
        workspace: String,
        label: &'static str,
        f: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let gate = self.scope_io_gate(&principal, &workspace).await;
        let _gate = gate.lock().await;
        magician_core::blocking_admission::spawn_blocking_admitted(f)
            .await
            .with_context(|| format!("feed {label} task panicked"))?
    }

    pub async fn materialize_scope(&self, principal: &str, workspace: &str) -> Result<()> {
        self.scope_arc(principal, workspace).await?;
        Ok(())
    }

    pub async fn materialize_existing_scopes(&self) -> Result<()> {
        // A reserved sink has no reader, so materialising a feed or a thread
        // store in one creates a DuckDB — and a scheduler pool sized to the
        // core count — for a surface nobody can open.
        let scopes = self
            .workspace_layout
            .list_tenant_scope_segments()
            .await
            .map_err(anyhow::Error::from)?;
        for (principal, workspace) in scopes {
            self.materialize_scope(&principal, &workspace).await?;
        }
        Ok(())
    }

    fn scope_inner(&self, principal: &str, workspace: &str) -> Result<Arc<FeedStoreInner>> {
        let key = (principal.to_string(), workspace.to_string());
        if let Some(existing) = self
            .scoped
            .lock()
            .expect("feed scoped store mutex poisoned")
            .get(&key)
            .cloned()
        {
            return Ok(existing);
        }

        let data_dir = self.workspace_layout.ui_feed_dir(principal, workspace);
        self.workspace_layout
            .create_dir_all_path_sync(&data_dir)
            .with_context(|| format!("creating scoped feed data dir: {}", data_dir.display()))?;
        let db_path = crate::magician_v2::database_owners::database_file_path(
            &self.workspace_layout,
            principal,
            workspace,
            crate::magician_v2::database_owners::DatabaseOwner::FeedDuckdb,
        );
        let lock_path = self
            .workspace_layout
            .ui_feed_lock_path(principal, workspace);
        self.ensure_template_seeded()?;
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening feed lock file at {}", lock_path.display()))?;
        lock_file
            .lock_exclusive()
            .context("acquiring feed scope bootstrap lock")?;
        if let Some(existing) = self
            .scoped
            .lock()
            .expect("feed scoped store mutex poisoned")
            .get(&key)
            .cloned()
        {
            let _ = lock_file.unlock();
            return Ok(existing);
        }
        crate::magician_v2::storage_governance::duckdb_compaction::recover_interrupted_compaction(
            &db_path,
        )?;
        prune_feed_quarantines(
            db_path
                .parent()
                .context("feed database path has no parent directory")?,
            MAX_FEED_QUARANTINE_FILES,
            MAX_FEED_QUARANTINE_BYTES,
        )?;
        // Bootstrap template schemas live in the seed root (the repo
        // `magician_data_v3/system/...`), which is OUTSIDE the runtime file
        // provider root. Read it directly via `std::fs` — routing it through the
        // provider rejects it as "outside workspace file provider root". This
        // mirrors `ui_threads::store` and the trust-policy template reads.
        let template_schema =
            std::fs::read_to_string(self.workspace_layout.feed_db_template_schema_path())
                .with_context(|| {
                    format!(
                        "reading feed template schema: {}",
                        self.workspace_layout
                            .feed_db_template_schema_path()
                            .display()
                    )
                })?;
        let conn = match open_initialized_feed_connection(
            &db_path,
            &template_schema,
            self.maintenance.feed_memory_mib,
        ) {
            Ok(conn) => conn,
            Err(error) if is_recoverable_feed_corruption(&error) => {
                let quarantine_path = quarantine_corrupt_feed_database(&db_path)?;
                prune_feed_quarantines(
                    db_path
                        .parent()
                        .context("feed database path has no parent directory")?,
                    MAX_FEED_QUARANTINE_FILES,
                    MAX_FEED_QUARANTINE_BYTES,
                )?;
                tracing::warn!(
                    database = %db_path.display(),
                    quarantine = %quarantine_path.display(),
                    quarantine_retained = quarantine_path.exists(),
                    error = %format!("{error:#}"),
                    "quarantined corrupt derived feed database; rebuilding an empty projection"
                );
                open_initialized_feed_connection(
                    &db_path,
                    &template_schema,
                    self.maintenance.feed_memory_mib,
                )
                .with_context(|| {
                    format!(
                        "rebuilding feed store after quarantining {}",
                        quarantine_path.display()
                    )
                })?
            },
            Err(error) => return Err(error),
        };
        lock_file
            .unlock()
            .context("releasing feed scope bootstrap lock")?;
        configure_connection(&conn, self.maintenance.feed_memory_mib)?;
        let inner = Arc::new(FeedStoreInner {
            admission: Arc::new(DatabaseGate::default()),
            db_path,
            write_conn: Mutex::new(conn),
            write_lock: Mutex::new(lock_file),
        });
        self.scoped
            .lock()
            .expect("feed scoped store mutex poisoned")
            .insert(key, Arc::clone(&inner));
        Ok(inner)
    }

    fn ensure_template_seeded(&self) -> Result<()> {
        // A read-only seed (container/deployment) ships the schema; never write into
        // it.
        if self.workspace_layout.templates_are_read_only() {
            return Ok(());
        }
        // Template schemas are seed-root files, read/written via `std::fs`
        // (not the runtime file provider) — see `scope_inner` above.
        let template_dir = self.workspace_layout.feed_db_template_dir();
        std::fs::create_dir_all(&template_dir)
            .with_context(|| format!("creating feed template dir: {}", template_dir.display()))?;
        let schema_path = self.workspace_layout.feed_db_template_schema_path();
        let exists = match std::fs::metadata(&schema_path) {
            Ok(_) => true,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("checking feed template schema: {}", schema_path.display())
                });
            },
        };
        if !exists {
            std::fs::write(&schema_path, BOOTSTRAP_DDL).with_context(|| {
                format!("writing feed template schema: {}", schema_path.display())
            })?;
        }
        Ok(())
    }

    pub async fn get_item(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
    ) -> Result<Option<FeedItem>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let id = id.to_string();
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "get_item",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let conn = inner.read_connection()?;
                let mut stmt = conn.prepare(
                    "SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id, \
                 title, summary, status, created_at, updated_at, actions_json, metadata_json FROM \
                 feed_items WHERE principal = ? AND workspace = ? AND id = ?",
                )?;
                let mut rows = stmt.query(params![principal, workspace, id])?;
                if let Some(row) = rows.next()? {
                    Ok(Some(map_feed_item_row(row)?))
                } else {
                    Ok(None)
                }
            },
        )
        .await
    }

    pub async fn upsert_item(&self, item: FeedItem) -> Result<Option<FeedItem>> {
        let store = self.clone();
        self.spawn_feed_blocking(
            item.principal.clone(),
            item.workspace.clone(),
            "upsert_item",
            move || {
                let inner = store.scope_inner(&item.principal, &item.workspace)?;
                let _write_guard = inner.acquire_write_guard()?;
                let conn = inner
                    .write_conn
                    .lock()
                    .expect("feed write connection mutex poisoned");
                let previous = {
                    let mut stmt = conn.prepare(
                    "SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id, \
                     title, summary, status, created_at, updated_at, actions_json, metadata_json \
                     FROM feed_items WHERE principal = ? AND workspace = ? AND id = ?",
                )?;
                    let mut rows = stmt.query(params![
                        item.principal.clone(),
                        item.workspace.clone(),
                        item.id.clone()
                    ])?;
                    if let Some(row) = rows.next()? {
                        Some(map_feed_item_row(row)?)
                    } else {
                        None
                    }
                };

                if previous
                    .as_ref()
                    .is_some_and(|persisted| feed_item_upsert_is_unchanged(persisted, &item))
                {
                    return Ok(previous);
                }

                let actions_json = serde_json::to_string(&item.actions)
                    .context("serializing feed item actions")?;
                let metadata_json = serde_json::to_string(&item.metadata)
                    .context("serializing feed item metadata")?;
                let attention_lane = attention_lane_for_item(&item)
                    .map(FeedAttentionLane::as_db_str)
                    .unwrap_or("");

                conn.execute(
                    "INSERT INTO feed_items (
                    principal, workspace, id, item_type, task_id, ui_thread_id, agent_id,
                    title, summary, status, created_at, updated_at, actions_json, metadata_json,
                    attention_lane
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (principal, workspace, id) DO UPDATE SET
                    item_type = excluded.item_type,
                    task_id = excluded.task_id,
                    ui_thread_id = excluded.ui_thread_id,
                    agent_id = excluded.agent_id,
                    title = excluded.title,
                    summary = excluded.summary,
                    status = excluded.status,
                    created_at = feed_items.created_at,
                    updated_at = excluded.updated_at,
                    actions_json = excluded.actions_json,
                    metadata_json = excluded.metadata_json,
                    attention_lane = excluded.attention_lane",
                    params![
                        item.principal,
                        item.workspace,
                        item.id,
                        item.item_type.as_db_str(),
                        item.task_id,
                        item.ui_thread_id,
                        item.agent_id,
                        item.title,
                        item.summary,
                        item.status.as_db_str(),
                        item.created_at,
                        item.updated_at,
                        actions_json,
                        metadata_json,
                        attention_lane,
                    ],
                )
                .context("upserting feed item")?;
                Ok(previous)
            },
        )
        .await
    }

    pub async fn remove_item(&self, principal: &str, workspace: &str, id: &str) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let id = id.to_string();
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "remove_item",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let _write_guard = inner.acquire_write_guard()?;
                let conn = inner
                    .write_conn
                    .lock()
                    .expect("feed write connection mutex poisoned");
                let changed = conn
                    .execute(
                        "DELETE FROM feed_items WHERE principal = ? AND workspace = ? AND id = ?",
                        params![principal, workspace, id],
                    )
                    .context("deleting feed item")?;
                Ok(changed > 0)
            },
        )
        .await
    }

    /// Removes every task-bound feed item in the scope whose `task_id`
    /// is set but does not appear in `valid_task_ids`. Durable delivery
    /// rows can carry `task_id` as provenance after the source task is
    /// archived/deleted, so they are intentionally preserved here.
    /// Returns the deleted items so callers can broadcast
    /// `FeedItemRemoved` events to connected clients.
    ///
    /// Used to garbage-collect legacy orphans that predate the
    /// archive_task → remove_task_summary cascade. The orphan filter on
    /// `feed_counts`/`list_feed` already hides them from the UI; this
    /// physically removes the rows so they stop counting against scope
    /// storage.
    pub async fn purge_orphan_items(
        &self,
        principal: &str,
        workspace: &str,
        valid_task_ids: &std::collections::HashSet<String>,
    ) -> Result<Vec<FeedItem>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let valid: Vec<String> = valid_task_ids.iter().cloned().collect();
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "purge_orphan_items",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let candidates: Vec<FeedItem> = {
                    let conn = inner.read_connection()?;
                    let mut stmt = conn.prepare(
                    "SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id, \
                     title, summary, status, created_at, updated_at, actions_json, metadata_json \
                     FROM feed_items
                     WHERE principal = ? AND workspace = ? AND task_id IS NOT NULL
                       AND item_type IN ('task', 'approval', 'escalation')",
                )?;
                    let mut rows = stmt.query(params![principal.clone(), workspace.clone()])?;
                    let mut items = Vec::new();
                    while let Some(row) = rows.next()? {
                        items.push(map_feed_item_row(row)?);
                    }
                    items
                };
                let valid_set: std::collections::HashSet<&str> =
                    valid.iter().map(String::as_str).collect();
                let orphans: Vec<FeedItem> = candidates
                    .into_iter()
                    .filter(|item| match item.task_id.as_deref() {
                        Some(task_id) => !valid_set.contains(task_id),
                        None => false,
                    })
                    .collect();
                if orphans.is_empty() {
                    return Ok(orphans);
                }
                let _write_guard = inner.acquire_write_guard()?;
                let conn = inner
                    .write_conn
                    .lock()
                    .expect("feed write connection mutex poisoned");
                let mut stmt = conn.prepare(
                    "DELETE FROM feed_items WHERE principal = ? AND workspace = ? AND id = ?",
                )?;
                for item in &orphans {
                    stmt.execute(params![principal, workspace, item.id])
                        .context("deleting orphan feed item")?;
                }
                drop(stmt);
                Ok(orphans)
            },
        )
        .await
    }

    pub async fn remove_task_items(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> Result<Vec<FeedItem>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let task_id = task_id.to_string();
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "remove_task_items",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let removed = {
                    let conn = inner.read_connection()?;
                    let mut stmt = conn.prepare(
                    "SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id,
                            title, summary, status, created_at, updated_at, actions_json, \
                     metadata_json
                     FROM feed_items
                     WHERE principal = ? AND workspace = ? AND task_id = ?
                       AND item_type IN ('task', 'approval', 'escalation')
                     UNION ALL
                     SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id,
                            title, summary, status, created_at, updated_at, actions_json, \
                     metadata_json
                     FROM feed_attention_items
                     WHERE principal = ? AND workspace = ? AND task_id = ?",
                )?;
                    let mut rows = stmt.query(params![
                        principal.clone(),
                        workspace.clone(),
                        task_id.clone(),
                        principal.clone(),
                        workspace.clone(),
                        task_id.clone(),
                    ])?;
                    let mut items = Vec::new();
                    while let Some(row) = rows.next()? {
                        items.push(map_feed_item_row(row)?);
                    }
                    items
                };

                let _write_guard = inner.acquire_write_guard()?;
                let conn = inner
                    .write_conn
                    .lock()
                    .expect("feed write connection mutex poisoned");
                conn.execute(
                    "DELETE FROM feed_items
                 WHERE principal = ? AND workspace = ? AND task_id = ?
                   AND item_type IN ('task', 'approval', 'escalation')",
                    params![principal, workspace, task_id],
                )
                .context("deleting task-scoped feed items")?;
                conn.execute(
                    "DELETE FROM feed_attention_items
                 WHERE principal = ? AND workspace = ? AND task_id = ?",
                    params![principal.clone(), workspace.clone(), task_id.clone()],
                )
                .context("deleting task-scoped attention projection items")?;
                conn.execute(
                    "DELETE FROM feed_attention_projection_groups
                 WHERE principal = ? AND workspace = ? AND projection_group = ?",
                    params![principal, workspace, task_id],
                )
                .context("deleting task-scoped attention projection generation")?;
                Ok(removed)
            },
        )
        .await
    }

    pub async fn list_items(&self, query: FeedQuery) -> Result<Vec<FeedItem>> {
        let store = self.clone();
        self.spawn_feed_blocking(
            query.principal.clone(),
            query.workspace.clone(),
            "list_items",
            move || {
                let inner = store.scope_inner(&query.principal, &query.workspace)?;
                let conn = inner.read_connection()?;
                let mut stmt = conn.prepare(
                    "SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id, \
                 title, summary, status, created_at, updated_at, actions_json, metadata_json FROM \
                 feed_items
                 WHERE principal = ?
                   AND workspace = ?
                   AND (? IS NULL OR task_id = ?)
                   AND (? IS NULL OR ui_thread_id = ?)
                   AND (? IS NULL OR item_type = ?)
                   AND (? IS NULL OR status = ?)
                   AND (? IS NULL OR agent_id = ?)
                   AND (? IS NULL OR updated_at < ?)
                   AND (? IS NULL OR updated_at > ?)
                 ORDER BY updated_at DESC, id DESC
                 LIMIT ?",
                )?;
                let item_type = query.item_type.as_ref().map(FeedItemType::as_db_str);
                let status = query.status.as_ref().map(FeedItemStatus::as_db_str);
                let limit = query.limit.clamp(1, 1_000) as i64;
                let mut rows = stmt.query(params![
                    query.principal,
                    query.workspace,
                    query.task_id.clone(),
                    query.task_id,
                    query.ui_thread_id.clone(),
                    query.ui_thread_id,
                    item_type,
                    item_type,
                    status,
                    status,
                    query.agent_id.clone(),
                    query.agent_id,
                    query.before,
                    query.before,
                    query.after,
                    query.after,
                    limit,
                ])?;
                let mut items = Vec::new();
                while let Some(row) = rows.next()? {
                    items.push(map_feed_item_row(row)?);
                }
                Ok(items)
            },
        )
        .await
    }

    /// Returns a stable newest-first keyset page. Unlike the legacy `before`
    /// timestamp filter, the composite cursor cannot skip rows that share the
    /// page-boundary millisecond.
    pub async fn list_items_page(
        &self,
        query: FeedQuery,
        cursor: Option<FeedPageCursor>,
    ) -> Result<FeedPage> {
        let store = self.clone();
        self.spawn_feed_blocking(
            query.principal.clone(),
            query.workspace.clone(),
            "list_items_page",
            move || {
                let inner = store.scope_inner(&query.principal, &query.workspace)?;
                let conn = inner.read_connection()?;
                let mut stmt = conn.prepare(
                    "SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id, \
                 title, summary, status, created_at, updated_at, actions_json, metadata_json FROM \
                 feed_items
                 WHERE principal = ?
                   AND workspace = ?
                   AND (? IS NULL OR task_id = ?)
                   AND (? IS NULL OR ui_thread_id = ?)
                   AND (? IS NULL OR item_type = ?)
                   AND (? IS NULL OR status = ?)
                   AND (? IS NULL OR agent_id = ?)
                   AND (? IS NULL OR updated_at < ?)
                   AND (? IS NULL OR updated_at > ?)
                   AND (? IS NULL OR updated_at < ? OR (updated_at = ? AND id < ?))
                 ORDER BY updated_at DESC, id DESC
                 LIMIT ?",
                )?;
                let item_type = query.item_type.as_ref().map(FeedItemType::as_db_str);
                let status = query.status.as_ref().map(FeedItemStatus::as_db_str);
                let cursor_updated_at = cursor.as_ref().map(|value| value.updated_at);
                let cursor_id = cursor.as_ref().map(|value| value.id.as_str());
                let limit = query.limit.clamp(1, 1_000);
                let fetch_limit = limit.saturating_add(1) as i64;
                let mut rows = stmt.query(params![
                    query.principal,
                    query.workspace,
                    query.task_id.clone(),
                    query.task_id,
                    query.ui_thread_id.clone(),
                    query.ui_thread_id,
                    item_type,
                    item_type,
                    status,
                    status,
                    query.agent_id.clone(),
                    query.agent_id,
                    query.before,
                    query.before,
                    query.after,
                    query.after,
                    cursor_updated_at,
                    cursor_updated_at,
                    cursor_updated_at,
                    cursor_id,
                    fetch_limit,
                ])?;
                let mut items = Vec::with_capacity(fetch_limit as usize);
                while let Some(row) = rows.next()? {
                    items.push(map_feed_item_row(row)?);
                }
                let has_more = items.len() > limit;
                if has_more {
                    items.truncate(limit);
                }
                let next_cursor =
                    has_more
                        .then(|| items.last())
                        .flatten()
                        .map(|item| FeedPageCursor {
                            updated_at: item.updated_at,
                            id: item.id.clone(),
                        });
                Ok(FeedPage {
                    items,
                    has_more,
                    next_cursor,
                })
            },
        )
        .await
    }

    pub async fn list_attention_lane_page(
        &self,
        query: FeedAttentionPageQuery,
    ) -> Result<FeedAttentionPage> {
        let store = self.clone();
        self.spawn_feed_blocking(
            query.principal.clone(),
            query.workspace.clone(),
            "list_attention_lane_page",
            move || {
                let inner = store.scope_inner(&query.principal, &query.workspace)?;
                let conn = inner.read_connection()?;
                conn.execute_batch("BEGIN TRANSACTION")?;
                let result = (|| -> Result<FeedAttentionPage> {
                    let (where_sql, base_params) = attention_lane_filter(&query);
                    let count_sql = format!(
                        "{ATTENTION_LANE_CTE} SELECT COUNT(*) FROM lane_items AS item {where_sql}"
                    );
                    let total =
                        conn.query_row(&count_sql, params_from_iter(base_params.iter()), |row| {
                            row.get::<_, i64>(0)
                        })? as u64;
                    let request_hitl_total = if query.lane == FeedAttentionLane::Requests {
                        let hitl_sql = format!(
                            "{ATTENTION_LANE_CTE}
                         SELECT COUNT(*) FROM lane_items AS item {where_sql}
                         AND json_extract_string(item.metadata_json, '$.attention_kind') IN (
                             'input.requested', 'user_request.pending',
                             'waiting_for_confirmation', 'max_iterations_reached',
                             'hitl.requested'
                         )"
                        );
                        Some(conn.query_row(
                            &hitl_sql,
                            params_from_iter(base_params.iter()),
                            |row| row.get::<_, i64>(0),
                        )? as u64)
                    } else {
                        None
                    };

                    let mut page_where_sql = where_sql;
                    let mut page_params = base_params;
                    if let Some(cursor) = query.cursor.as_ref() {
                        page_where_sql.push_str(
                            " AND (item.updated_at < ? OR (item.updated_at = ? AND item.id < ?))",
                        );
                        page_params.push(DuckValue::BigInt(cursor.updated_at));
                        page_params.push(DuckValue::BigInt(cursor.updated_at));
                        page_params.push(DuckValue::Text(cursor.id.clone()));
                    }
                    let limit = query.limit.clamp(1, 200);
                    page_params.push(DuckValue::BigInt(limit.saturating_add(1) as i64));
                    page_params.push(DuckValue::BigInt(query.offset as i64));
                    let page_sql = format!(
                        "{ATTENTION_LANE_CTE}
                     SELECT item.principal, item.workspace, item.id, item.item_type, item.task_id,
                            item.ui_thread_id, item.agent_id, item.title, item.summary, \
                     item.status,
                            item.created_at, item.updated_at, item.actions_json, item.metadata_json
                     FROM lane_items AS item {page_where_sql}
                     ORDER BY item.updated_at DESC, item.id DESC
                     LIMIT ? OFFSET ?"
                    );
                    let mut stmt = conn.prepare(&page_sql)?;
                    let mut rows = stmt.query(params_from_iter(page_params.iter()))?;
                    let mut items = Vec::with_capacity(limit.saturating_add(1));
                    while let Some(row) = rows.next()? {
                        items.push(map_feed_item_row(row)?);
                    }
                    drop(rows);
                    drop(stmt);
                    let has_more = items.len() > limit;
                    if has_more {
                        items.truncate(limit);
                    }
                    let next_cursor =
                        has_more
                            .then(|| items.last())
                            .flatten()
                            .map(|item| FeedPageCursor {
                                updated_at: item.updated_at,
                                id: item.id.clone(),
                            });
                    Ok(FeedAttentionPage {
                        items,
                        total,
                        request_hitl_total,
                        has_more,
                        next_cursor,
                    })
                })();
                match result {
                    Ok(page) => {
                        conn.execute_batch("COMMIT")?;
                        Ok(page)
                    },
                    Err(error) => {
                        rollback_after(&conn, "list_attention_lane_page", &error);
                        Err(error)
                    },
                }
            },
        )
        .await
    }

    /// Resolve one currently-visible Attention item by its raw feed id or one
    /// of the canonical HITL aliases carried in metadata.
    ///
    /// This reads the same projected + ordinary Attention union used by lane
    /// pagination, but does not depend on the item's position in a capped
    /// page. Scope and durable-dismissal predicates remain part of the query,
    /// so an alias cannot cross a principal/workspace boundary or resurrect a
    /// dismissed row.
    pub async fn get_attention_item_by_alias(
        &self,
        principal: &str,
        workspace: &str,
        item_id: &str,
    ) -> Result<Option<FeedItem>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let item_id = item_id.to_string();
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "get_attention_item_by_alias",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let conn = inner.read_connection()?;
                let mut stmt = conn.prepare(
                    "WITH attention_items AS (
                    SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id,
                           title, summary, status, created_at, updated_at, actions_json,
                           metadata_json
                    FROM feed_attention_items AS projected
                    WHERE projected.principal = ? AND projected.workspace = ?
                    UNION ALL
                    SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id,
                           title, summary, status, created_at, updated_at, actions_json,
                           metadata_json
                    FROM feed_items AS ordinary
                    WHERE ordinary.principal = ? AND ordinary.workspace = ?
                      AND COALESCE(ordinary.attention_lane, '') <> ''
                      AND NOT EXISTS (
                          SELECT 1 FROM feed_attention_items AS projected_priority
                          WHERE projected_priority.principal = ordinary.principal
                            AND projected_priority.workspace = ordinary.workspace
                            AND projected_priority.id = ordinary.id
                      )
                )
                SELECT item.principal, item.workspace, item.id, item.item_type, item.task_id,
                       item.ui_thread_id, item.agent_id, item.title, item.summary, item.status,
                       item.created_at, item.updated_at, item.actions_json, item.metadata_json
                FROM attention_items AS item
                WHERE NOT EXISTS (
                    SELECT 1 FROM feed_attention_dismissals AS dismissal
                    WHERE dismissal.principal = item.principal
                      AND dismissal.workspace = item.workspace
                      AND dismissal.id = item.id
                      AND dismissal.dismissed = TRUE
                )
                  AND (
                      item.id = ?
                      OR json_extract_string(item.metadata_json, '$.correlation_id') = ?
                      OR json_extract_string(item.metadata_json, '$.pause_state_id') = ?
                      OR json_extract_string(item.metadata_json, '$.approval_id') = ?
                      OR json_extract_string(item.metadata_json, '$.request_id') = ?
                  )
                ORDER BY CASE WHEN item.id = ? THEN 0 ELSE 1 END,
                         item.updated_at DESC, item.id DESC
                LIMIT 1",
                )?;
                let mut rows = stmt.query(params![
                    principal, workspace, principal, workspace, item_id, item_id, item_id, item_id,
                    item_id, item_id,
                ])?;
                if let Some(row) = rows.next()? {
                    Ok(Some(map_feed_item_row(row)?))
                } else {
                    Ok(None)
                }
            },
        )
        .await
    }

    pub async fn attention_item_is_dismissed(
        &self,
        principal: &str,
        workspace: &str,
        item_id: &str,
    ) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let item_id = item_id.to_string();
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "attention_item_is_dismissed",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let conn = inner.read_connection()?;
                conn.query_row(
                    "SELECT COALESCE((
                    SELECT dismissed FROM feed_attention_dismissals
                    WHERE principal = ? AND workspace = ? AND id = ?
                    LIMIT 1
                ), FALSE)",
                    params![principal, workspace, item_id],
                    |row| row.get::<_, bool>(0),
                )
                .context("reading attention item dismissal")
            },
        )
        .await
    }

    /// Atomically replaces one durable attention projection group. Older
    /// source generations, and recovery snapshots racing a newer write, are
    /// ignored without touching current rows.
    pub async fn reconcile_attention_projection(
        &self,
        principal: &str,
        workspace: &str,
        lane: FeedAttentionLane,
        projection_source: &str,
        projection_group: &str,
        source_generation: i64,
        recovery_cutoff: Option<i64>,
        items: Vec<FeedItem>,
    ) -> Result<AttentionProjectionReconcile> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let projection_source = projection_source.to_string();
        let projection_group = projection_group.to_string();
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "reconcile_attention_projection",
            move || {
                for item in &items {
                    if item.principal != principal || item.workspace != workspace {
                        anyhow::bail!(
                            "attention projection item scope does not match reconciliation scope"
                        );
                    }
                }
                let prepared = items
                    .into_iter()
                    .map(|item| {
                        let actions_json = serde_json::to_string(&item.actions)
                            .context("serializing attention projection actions")?;
                        let metadata_json = serde_json::to_string(&item.metadata)
                            .context("serializing attention projection metadata")?;
                        Ok((item, actions_json, metadata_json))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let active_ids = prepared
                    .iter()
                    .map(|(item, _, _)| item.id.clone())
                    .collect::<HashSet<_>>();
                let inner = store.scope_inner(&principal, &workspace)?;
                let _write_guard = inner.acquire_write_guard()?;
                let conn = inner
                    .write_conn
                    .lock()
                    .expect("feed write connection mutex poisoned");
                conn.execute_batch("BEGIN TRANSACTION")?;
                let result = (|| -> Result<AttentionProjectionReconcile> {
                    let existing_generation = {
                        let mut stmt = conn.prepare(
                            "SELECT source_generation, recorded_at
                         FROM feed_attention_projection_groups
                         WHERE principal = ? AND workspace = ?
                           AND projection_source = ? AND projection_group = ?",
                        )?;
                        let mut rows = stmt.query(params![
                            principal.clone(),
                            workspace.clone(),
                            projection_source.clone(),
                            projection_group.clone(),
                        ])?;
                        if let Some(row) = rows.next()? {
                            Some((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                        } else {
                            None
                        }
                    };
                    if existing_generation.is_some_and(|(generation, recorded_at)| {
                        source_generation <= generation
                            || recovery_cutoff.is_some_and(|cutoff| recorded_at > cutoff)
                    }) {
                        return Ok(AttentionProjectionReconcile {
                            skipped_stale: true,
                            ..Default::default()
                        });
                    }
                    let existing_ids = {
                        let mut stmt = conn.prepare(
                            "SELECT id FROM feed_attention_items
                         WHERE principal = ? AND workspace = ? AND projection_source = ?
                           AND projection_group = ?",
                        )?;
                        let mut rows = stmt.query(params![
                            principal.clone(),
                            workspace.clone(),
                            projection_source.clone(),
                            projection_group.clone(),
                        ])?;
                        let mut ids = Vec::new();
                        while let Some(row) = rows.next()? {
                            ids.push(row.get::<_, String>(0)?);
                        }
                        ids
                    };

                    for (item, actions_json, metadata_json) in &prepared {
                        conn.execute(
                            "INSERT INTO feed_attention_items (
                            principal, workspace, id, lane, projection_source, projection_group,
                            item_type, task_id, ui_thread_id, agent_id, title, summary, status,
                            created_at, updated_at, actions_json, metadata_json
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                         ON CONFLICT (principal, workspace, id) DO UPDATE SET
                            lane = excluded.lane,
                            projection_source = excluded.projection_source,
                            projection_group = excluded.projection_group,
                            item_type = excluded.item_type,
                            task_id = excluded.task_id,
                            ui_thread_id = excluded.ui_thread_id,
                            agent_id = excluded.agent_id,
                            title = excluded.title,
                            summary = excluded.summary,
                            status = excluded.status,
                            created_at = feed_attention_items.created_at,
                            updated_at = excluded.updated_at,
                            actions_json = excluded.actions_json,
                            metadata_json = excluded.metadata_json",
                            params![
                                item.principal,
                                item.workspace,
                                item.id,
                                lane.as_db_str(),
                                projection_source,
                                projection_group,
                                item.item_type.as_db_str(),
                                item.task_id,
                                item.ui_thread_id,
                                item.agent_id,
                                item.title,
                                item.summary,
                                item.status.as_db_str(),
                                item.created_at,
                                item.updated_at,
                                actions_json,
                                metadata_json,
                            ],
                        )?;
                    }

                    let stale_ids = existing_ids
                        .into_iter()
                        .filter(|id| !active_ids.contains(id))
                        .collect::<Vec<_>>();
                    for id in &stale_ids {
                        conn.execute(
                            "DELETE FROM feed_attention_items
                         WHERE principal = ? AND workspace = ? AND id = ?",
                            params![principal, workspace, id],
                        )?;
                    }
                    conn.execute(
                        "INSERT INTO feed_attention_projection_groups (
                        principal, workspace, projection_source, projection_group,
                        source_generation, recorded_at
                     ) VALUES (?, ?, ?, ?, ?, ?)
                     ON CONFLICT (
                        principal, workspace, projection_source, projection_group
                     ) DO UPDATE SET
                        source_generation = excluded.source_generation,
                        recorded_at = excluded.recorded_at",
                        params![
                            principal,
                            workspace,
                            projection_source,
                            projection_group,
                            source_generation,
                            chrono::Utc::now().timestamp_millis(),
                        ],
                    )?;
                    Ok(AttentionProjectionReconcile {
                        upserted: prepared.len(),
                        removed: stale_ids.len(),
                        skipped_stale: false,
                    })
                })();
                match result {
                    Ok(summary) => {
                        conn.execute_batch("COMMIT")?;
                        Ok(summary)
                    },
                    Err(error) => {
                        rollback_after(&conn, "reconcile_attention_projection", &error);
                        Err(error)
                    },
                }
            },
        )
        .await
    }

    pub async fn set_attention_dismissed(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        dismissed: bool,
        updated_at: i64,
    ) -> Result<()> {
        self.merge_attention_dismissals(
            principal,
            workspace,
            vec![(id.to_string(), dismissed, updated_at)],
        )
        .await?;
        Ok(())
    }

    /// Merges persisted dismissal state by timestamp. Undismissals remain as
    /// tombstones so a stale startup import cannot re-hide an item.
    ///
    /// Returns the number of rows actually written. Only a record STRICTLY
    /// newer than the stored one is written, so the count is a change count
    /// rather than a count of records looked at.
    ///
    /// **A tie keeps the stored row**, and that is what makes the tombstone
    /// hold. `updated_at` is a millisecond stamp and a bulk startup import
    /// replays many records at once, so carrying the same value as the row it
    /// merges against is ordinary rather than exotic; letting the incoming
    /// record win there would be precisely the re-hiding the line above
    /// promises not to do. The cost is that two writes inside one millisecond
    /// keep the first — for a user toggle that is self-correcting on the next
    /// click, and for an import it is the safe direction.
    pub async fn merge_attention_dismissals(
        &self,
        principal: &str,
        workspace: &str,
        records: Vec<(String, bool, i64)>,
    ) -> Result<usize> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "merge_attention_dismissals",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let _write_guard = inner.acquire_write_guard()?;
                let conn = inner
                    .write_conn
                    .lock()
                    .expect("feed write connection mutex poisoned");
                conn.execute_batch("BEGIN TRANSACTION")?;
                let result = (|| -> Result<usize> {
                    let mut changed = 0usize;
                    for (id, dismissed, updated_at) in records {
                        let existing_updated_at = {
                            let mut stmt = conn.prepare(
                                "SELECT updated_at FROM feed_attention_dismissals
                             WHERE principal = ? AND workspace = ? AND id = ?",
                            )?;
                            let mut rows = stmt.query(params![
                                principal.clone(),
                                workspace.clone(),
                                id.clone(),
                            ])?;
                            rows.next()?.map(|row| row.get::<_, i64>(0)).transpose()?
                        };
                        // `>=`, not `>`: a tie keeps the stored row. See the doc
                        // comment — an equal millisecond is the common case for a
                        // bulk import, not a rare one, and letting it through is
                        // how an undismissal tombstone gets overwritten.
                        if existing_updated_at.is_some_and(|existing| existing >= updated_at) {
                            continue;
                        }
                        if existing_updated_at.is_some() {
                            // Keep this as a plain UPDATE. DuckDB implements
                            // INSERT .. ON CONFLICT as MERGE INTO; its buffered
                            // index-replay path can segfault while rebinding a
                            // persisted ART index. The write lock makes the
                            // preceding existence check and this update atomic.
                            conn.execute(
                                "UPDATE feed_attention_dismissals
                             SET dismissed = ?, updated_at = ?
                             WHERE principal = ? AND workspace = ? AND id = ?",
                                params![dismissed, updated_at, &principal, &workspace, id],
                            )?;
                        } else {
                            conn.execute(
                                "INSERT INTO feed_attention_dismissals (
                                principal, workspace, id, dismissed, updated_at
                             ) VALUES (?, ?, ?, ?, ?)",
                                params![&principal, &workspace, id, dismissed, updated_at],
                            )?;
                        }
                        changed += 1;
                    }
                    Ok(changed)
                })();
                match result {
                    Ok(changed) => {
                        conn.execute_batch("COMMIT")?;
                        Ok(changed)
                    },
                    Err(error) => {
                        rollback_after(&conn, "merge_attention_dismissals", &error);
                        Err(error)
                    },
                }
            },
        )
        .await
    }

    /// Removes recovery-only projection groups absent from the authoritative
    /// task snapshot, but never groups recorded after recovery began.
    pub async fn purge_attention_projection_groups_not_in(
        &self,
        principal: &str,
        workspace: &str,
        projection_source: &str,
        valid_groups: &HashSet<String>,
        recovery_started_at: i64,
    ) -> Result<usize> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let projection_source = projection_source.to_string();
        let valid_groups = valid_groups.clone();
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "purge_attention_projection_groups_not_in",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let _write_guard = inner.acquire_write_guard()?;
                let conn = inner
                    .write_conn
                    .lock()
                    .expect("feed write connection mutex poisoned");
                conn.execute_batch("BEGIN TRANSACTION")?;
                let result = (|| -> Result<usize> {
                    let stale_groups = {
                        let mut stmt = conn.prepare(
                            "SELECT projection_group FROM feed_attention_projection_groups
                         WHERE principal = ? AND workspace = ? AND projection_source = ?
                           AND recorded_at <= ?",
                        )?;
                        let mut rows = stmt.query(params![
                            principal.clone(),
                            workspace.clone(),
                            projection_source.clone(),
                            recovery_started_at,
                        ])?;
                        let mut groups = Vec::new();
                        while let Some(row) = rows.next()? {
                            let group = row.get::<_, String>(0)?;
                            if !valid_groups.contains(&group) {
                                groups.push(group);
                            }
                        }
                        groups
                    };
                    for group in &stale_groups {
                        conn.execute(
                            "DELETE FROM feed_attention_items
                         WHERE principal = ? AND workspace = ?
                           AND projection_source = ? AND projection_group = ?",
                            params![principal, workspace, projection_source, group],
                        )?;
                        conn.execute(
                            "DELETE FROM feed_attention_projection_groups
                         WHERE principal = ? AND workspace = ?
                           AND projection_source = ? AND projection_group = ?",
                            params![principal, workspace, projection_source, group],
                        )?;
                    }
                    Ok(stale_groups.len())
                })();
                match result {
                    Ok(removed) => {
                        conn.execute_batch("COMMIT")?;
                        Ok(removed)
                    },
                    Err(error) => {
                        rollback_after(&conn, "purge_attention_projection_groups_not_in", &error);
                        Err(error)
                    },
                }
            },
        )
        .await
    }

    pub async fn list_items_by_id_prefixes(
        &self,
        principal: &str,
        workspace: &str,
        prefixes: &[&str],
    ) -> Result<Vec<FeedItem>> {
        if prefixes.is_empty() {
            return Ok(Vec::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let prefixes = prefixes
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "list_items_by_id_prefixes",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let conn = inner.read_connection()?;
                let prefix_sql = std::iter::repeat_n("starts_with(id, ?)", prefixes.len())
                    .collect::<Vec<_>>()
                    .join(" OR ");
                let sql = format!(
                    "SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id,
                        title, summary, status, created_at, updated_at, actions_json, metadata_json
                 FROM feed_items
                 WHERE principal = ? AND workspace = ? AND ({prefix_sql})
                 ORDER BY updated_at DESC, id DESC"
                );
                let mut values = vec![DuckValue::Text(principal), DuckValue::Text(workspace)];
                values.extend(prefixes.into_iter().map(DuckValue::Text));
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(values.iter()))?;
                let mut items = Vec::new();
                while let Some(row) = rows.next()? {
                    items.push(map_feed_item_row(row)?);
                }
                Ok(items)
            },
        )
        .await
    }

    pub async fn counts(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: Option<&str>,
    ) -> Result<FeedCounts> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let ui_thread_id = ui_thread_id.map(str::to_string);
        self.spawn_feed_blocking(principal.clone(), workspace.clone(), "counts", move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT
                    COUNT(*) AS total,
                    COALESCE(SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END), 0) AS running,
                    COALESCE(SUM(CASE WHEN status = 'needs_action' THEN 1 ELSE 0 END), 0) AS \
                 needs_action,
                    COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS failed,
                    COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0) AS done,
                    COALESCE(SUM(CASE WHEN status = 'info' THEN 1 ELSE 0 END), 0) AS info
                 FROM feed_items
                 WHERE principal = ?
                   AND workspace = ?
                   AND (? IS NULL OR ui_thread_id = ?)",
            )?;
            let counts = stmt.query_row(
                params![principal, workspace, ui_thread_id.clone(), ui_thread_id,],
                |row| {
                    Ok(FeedCounts {
                        total: row.get::<_, i64>(0)? as u64,
                        running: row.get::<_, i64>(1)? as u64,
                        needs_action: row.get::<_, i64>(2)? as u64,
                        failed: row.get::<_, i64>(3)? as u64,
                        done: row.get::<_, i64>(4)? as u64,
                        info: row.get::<_, i64>(5)? as u64,
                    })
                },
            )?;
            Ok(counts)
        })
        .await
    }

    pub async fn count_items(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: Option<&str>,
        item_type: Option<FeedItemType>,
        status: Option<FeedItemStatus>,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let ui_thread_id = ui_thread_id.map(str::to_string);
        self.spawn_feed_blocking(
            principal.clone(),
            workspace.clone(),
            "count_items",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let conn = inner.read_connection()?;
                let item_type = item_type.as_ref().map(FeedItemType::as_db_str);
                let status = status.as_ref().map(FeedItemStatus::as_db_str);
                let mut stmt = conn.prepare(
                    "SELECT COUNT(*)
                 FROM feed_items
                 WHERE principal = ?
                   AND workspace = ?
                   AND (? IS NULL OR ui_thread_id = ?)
                   AND (? IS NULL OR item_type = ?)
                   AND (? IS NULL OR status = ?)",
                )?;
                let count = stmt.query_row(
                    params![
                        principal,
                        workspace,
                        ui_thread_id.clone(),
                        ui_thread_id,
                        item_type,
                        item_type,
                        status,
                        status,
                    ],
                    |row| row.get::<_, i64>(0),
                )?;
                Ok(count as u64)
            },
        )
        .await
    }

    pub async fn inspect_maintenance(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Fragmentation> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            inspect_fragmentation(
                &conn,
                std::fs::metadata(&inner.db_path)?.len(),
                &store.maintenance,
            )
        })
        .await?
    }

    pub async fn compact_scope_with_wait(
        &self,
        principal: &str,
        workspace: &str,
        wait: std::time::Duration,
    ) -> Result<crate::magician_v2::storage_governance::DuckDbCompactionReport> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _exclusive = inner.admission.maintain(wait)?;
            let _writer = inner.acquire_file_guard(None)?;
            let schema = std::fs::read_to_string(store.workspace_layout.feed_db_template_schema_path())?;
            let mut conn = inner.write_conn.lock().map_err(|_| anyhow::anyhow!("feed connection lock poisoned"))?;
            configure_connection(&conn, store.maintenance.maintenance_memory_mib)?;
            let result = crate::magician_v2::storage_governance::duckdb_compaction::compact_recoverable_database(&mut conn, &inner.db_path, |published| {
                configure_connection(published, store.maintenance.maintenance_memory_mib)?;
                published.execute_batch(&schema)?;
                ensure_attention_projection_schema(published)?;
                validate_feed_database(published)
            });
            if result.as_ref().err().is_some_and(|error| error.is::<crate::magician_v2::storage_governance::duckdb_compaction::RecoveryRequired>()) {
                inner.admission.require_recovery();
                return result;
            }
            if let Err(error) = configure_connection(&conn, store.maintenance.feed_memory_mib) {
                inner.admission.require_recovery();
                return Err(error);
            }
            result
        }).await?
    }

    pub fn base_root(&self) -> &Path {
        &self.base_root
    }
}

fn open_initialized_feed_connection(
    db_path: &Path,
    template_schema: &str,
    memory_mib: u64,
) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening feed store at {}", db_path.display()))?;
    configure_connection(&conn, memory_mib)?;
    ensure_attention_lane_column_if_table_exists(&conn)?;
    conn.execute_batch(template_schema)
        .context("running feed template schema")?;
    ensure_attention_projection_schema(&conn)
        .context("migrating feed attention projection schema")?;
    validate_feed_database(&conn).context("validating feed database after schema bootstrap")?;
    Ok(conn)
}

/// Rolls back a failed write, and says so when the rollback itself fails.
///
/// Every attention write path shares one long-lived `write_conn`. A rollback
/// that does not land leaves that connection inside a transaction, after which
/// every later write on it fails with "already in a transaction" — an error
/// naming neither the operation that failed nor the one that poisoned the
/// connection. Discarding the rollback result put the first visible symptom an
/// arbitrary distance from its cause, which is the expensive kind of quiet.
fn rollback_after(conn: &Connection, operation: &str, error: &anyhow::Error) {
    if let Err(rollback_error) = conn.execute_batch("ROLLBACK") {
        tracing::warn!(
            operation,
            error = %format!("{error:#}"),
            rollback_error = %rollback_error,
            "feed write could not roll back; the write connection may still be in a transaction"
        );
    }
}

fn validate_feed_database(conn: &Connection) -> Result<()> {
    for table in [
        "feed_items",
        "feed_attention_items",
        "feed_attention_projection_groups",
        "feed_attention_dismissals",
    ] {
        let sql = format!("PRAGMA storage_info('{table}')");
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
    }

    let mut status_stmt =
        conn.prepare("SELECT status, COUNT(*) FROM feed_items GROUP BY status")?;
    let mut status_rows = status_stmt.query([])?;
    while status_rows.next()?.is_some() {}
    drop(status_rows);
    drop(status_stmt);

    let mut stmt = conn.prepare(
        "SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id,
                title, summary, status, created_at, updated_at, actions_json, metadata_json
         FROM feed_items ORDER BY updated_at DESC, id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        map_feed_item_row(row)?;
    }
    Ok(())
}

fn is_recoverable_feed_corruption(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    [
        "serialization error: failed to deserialize",
        "field id mismatch",
        "failed to scan dictionary string",
        "not a valid duckdb database",
        "invalid duckdb database",
        "database file is corrupted",
        "corrupt database",
        "checksum mismatch",
        "invalid block checksum",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn quarantine_corrupt_feed_database(db_path: &Path) -> Result<PathBuf> {
    let parent = db_path
        .parent()
        .context("feed database path has no parent directory")?;
    let quarantine_path = parent.join(format!(
        "feed.corrupt-{}-{}.duckdb",
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4()
    ));
    std::fs::rename(db_path, &quarantine_path).with_context(|| {
        format!(
            "quarantining corrupt feed database {} as {}",
            db_path.display(),
            quarantine_path.display()
        )
    })?;

    for suffix in [".wal", ".tmp"] {
        let source = path_with_appended_suffix(db_path, suffix);
        let destination = path_with_appended_suffix(&quarantine_path, suffix);
        match std::fs::rename(&source, &destination) {
            Ok(()) => {},
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "quarantining corrupt feed sidecar {} as {}",
                        source.display(),
                        destination.display()
                    )
                });
            },
        }
    }
    Ok(quarantine_path)
}

fn prune_feed_quarantines(parent: &Path, max_files: usize, max_bytes: u64) -> Result<()> {
    let mut quarantines = std::fs::read_dir(parent)
        .with_context(|| format!("listing feed quarantines in {}", parent.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("feed.corrupt-") || !name.ends_with(".duckdb") {
                return None;
            }
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            let modified = metadata.modified().ok();
            let sidecar_bytes = [".wal", ".tmp"]
                .into_iter()
                .filter_map(|suffix| {
                    std::fs::metadata(path_with_appended_suffix(&path, suffix)).ok()
                })
                .map(|metadata| metadata.len())
                .sum::<u64>();
            Some((path, modified, metadata.len().saturating_add(sidecar_bytes)))
        })
        .collect::<Vec<_>>();
    quarantines.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.0.file_name().cmp(&left.0.file_name()))
    });

    let mut kept_files = 0usize;
    let mut kept_bytes = 0u64;
    for (path, _, bytes) in quarantines {
        let fits = kept_files < max_files
            && kept_bytes
                .checked_add(bytes)
                .is_some_and(|total| total <= max_bytes);
        if fits {
            kept_files += 1;
            kept_bytes += bytes;
            continue;
        }
        for candidate in [
            path.clone(),
            path_with_appended_suffix(&path, ".wal"),
            path_with_appended_suffix(&path, ".tmp"),
        ] {
            match std::fs::remove_file(&candidate) {
                Ok(()) => {},
                Err(error) if error.kind() == ErrorKind::NotFound => {},
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("removing stale feed quarantine {}", candidate.display())
                    });
                },
            }
        }
    }
    Ok(())
}

fn path_with_appended_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn feed_item_upsert_is_unchanged(persisted: &FeedItem, incoming: &FeedItem) -> bool {
    let mut expected = incoming.clone();
    expected.created_at = persisted.created_at;
    persisted == &expected
}

const ATTENTION_LANE_CTE: &str = r#"
WITH lane_items AS (
    SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id,
           title, summary, status, created_at, updated_at, actions_json, metadata_json,
           lane
    FROM feed_attention_items AS projected
    WHERE projected.principal = ? AND projected.workspace = ? AND projected.lane = ?
    UNION ALL
    SELECT principal, workspace, id, item_type, task_id, ui_thread_id, agent_id,
           title, summary, status, created_at, updated_at, actions_json, metadata_json,
           attention_lane AS lane
    FROM feed_items AS ordinary
    WHERE ordinary.principal = ? AND ordinary.workspace = ? AND ordinary.attention_lane = ?
      AND NOT EXISTS (
          SELECT 1 FROM feed_attention_items AS projected_priority
          WHERE projected_priority.principal = ordinary.principal
            AND projected_priority.workspace = ordinary.workspace
            AND projected_priority.id = ordinary.id
      )
)
"#;

fn attention_lane_filter(query: &FeedAttentionPageQuery) -> (String, Vec<DuckValue>) {
    let mut sql = "WHERE NOT EXISTS (
            SELECT 1 FROM feed_attention_dismissals AS dismissal
            WHERE dismissal.principal = item.principal
              AND dismissal.workspace = item.workspace
              AND dismissal.id = item.id
              AND dismissal.dismissed = TRUE
        )"
    .to_string();
    let mut values = vec![
        DuckValue::Text(query.principal.clone()),
        DuckValue::Text(query.workspace.clone()),
        DuckValue::Text(query.lane.as_db_str().to_string()),
        DuckValue::Text(query.principal.clone()),
        DuckValue::Text(query.workspace.clone()),
        DuckValue::Text(query.lane.as_db_str().to_string()),
    ];
    if let Some(ui_thread_id) = query.ui_thread_id.as_ref() {
        sql.push_str(" AND item.ui_thread_id = ?");
        values.push(DuckValue::Text(ui_thread_id.clone()));
    }
    if !query.exclude_task_ids.is_empty() {
        let placeholders = vec!["?"; query.exclude_task_ids.len()].join(", ");
        sql.push_str(&format!(
            " AND (item.task_id IS NULL OR item.task_id NOT IN ({placeholders}))"
        ));
        values.extend(query.exclude_task_ids.iter().cloned().map(DuckValue::Text));
    }
    (sql, values)
}

fn ensure_attention_lane_column_if_table_exists(conn: &Connection) -> Result<()> {
    let table_exists = conn.query_row(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = 'feed_items'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !table_exists {
        return Ok(());
    }
    let has_column = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('feed_items') WHERE name = 'attention_lane'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !has_column {
        conn.execute_batch("ALTER TABLE feed_items ADD COLUMN attention_lane TEXT")?;
    }
    Ok(())
}

fn ensure_attention_projection_schema(conn: &Connection) -> Result<()> {
    ensure_attention_lane_column_if_table_exists(conn)?;
    conn.execute_batch(
        // Dismissals are looked up by their primary key. The old secondary
        // index included `dismissed`, so every acknowledgement changed an
        // indexed value and exposed DuckDB's unsafe buffered index-replay
        // path. Retire it before any startup import writes occur.
        "DROP INDEX IF EXISTS idx_feed_attention_dismissed_item;
         CREATE INDEX IF NOT EXISTS idx_feed_scope_updated_keyset
             ON feed_items (principal, workspace, updated_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_feed_scope_status_updated_keyset
             ON feed_items (principal, workspace, status, updated_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_feed_scope_attention_lane_updated
             ON feed_items (principal, workspace, attention_lane, updated_at DESC, id DESC);
         CREATE TABLE IF NOT EXISTS feed_attention_items (
             principal TEXT NOT NULL,
             workspace TEXT NOT NULL,
             id TEXT NOT NULL,
             lane TEXT NOT NULL,
             projection_source TEXT NOT NULL,
             projection_group TEXT NOT NULL,
             item_type TEXT NOT NULL,
             task_id TEXT NULL,
             ui_thread_id TEXT NULL,
             agent_id TEXT NULL,
             title TEXT NOT NULL,
             summary TEXT NULL,
             status TEXT NOT NULL,
             created_at BIGINT NOT NULL,
             updated_at BIGINT NOT NULL,
             actions_json JSON NOT NULL,
             metadata_json JSON NOT NULL,
             PRIMARY KEY (principal, workspace, id)
         );
         CREATE INDEX IF NOT EXISTS idx_feed_attention_scope_lane_updated
             ON feed_attention_items (principal, workspace, lane, updated_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_feed_attention_scope_projection_group
             ON feed_attention_items (
                 principal, workspace, projection_source, projection_group
             );
         CREATE TABLE IF NOT EXISTS feed_attention_projection_groups (
             principal TEXT NOT NULL,
             workspace TEXT NOT NULL,
             projection_source TEXT NOT NULL,
             projection_group TEXT NOT NULL,
             source_generation BIGINT NOT NULL,
             recorded_at BIGINT NOT NULL,
             PRIMARY KEY (principal, workspace, projection_source, projection_group)
         );
         CREATE TABLE IF NOT EXISTS feed_attention_dismissals (
             principal TEXT NOT NULL,
             workspace TEXT NOT NULL,
             id TEXT NOT NULL,
             dismissed BOOLEAN NOT NULL,
             updated_at BIGINT NOT NULL,
             PRIMARY KEY (principal, workspace, id)
         );
         INSERT INTO feed_attention_projection_groups (
             principal, workspace, projection_source, projection_group,
             source_generation, recorded_at
         )
         SELECT principal, workspace, projection_source, projection_group,
                MAX(updated_at), 0
         FROM feed_attention_items
         GROUP BY principal, workspace, projection_source, projection_group
         ON CONFLICT DO NOTHING;",
    )?;

    let pending = {
        let mut stmt = conn.prepare(
            "SELECT principal, workspace, id, item_type, status, metadata_json
             FROM feed_items WHERE attention_lane IS NULL",
        )?;
        let mut rows = stmt.query([])?;
        let mut values = Vec::new();
        while let Some(row) = rows.next()? {
            values.push((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ));
        }
        values
    };
    let mut update = conn.prepare(
        "UPDATE feed_items SET attention_lane = ?
         WHERE principal = ? AND workspace = ? AND id = ?",
    )?;
    for (principal, workspace, id, item_type, status, metadata_json) in pending {
        let metadata = serde_json::from_str(&metadata_json).unwrap_or(serde_json::Value::Null);
        let lane = attention_lane_from_parts(&item_type, &status, &metadata)
            .map(FeedAttentionLane::as_db_str)
            .unwrap_or("");
        update.execute(params![lane, principal, workspace, id])?;
    }
    Ok(())
}

fn attention_lane_for_item(item: &FeedItem) -> Option<FeedAttentionLane> {
    attention_lane_from_parts(
        item.item_type.as_db_str(),
        item.status.as_db_str(),
        &item.metadata,
    )
}

fn attention_lane_from_parts(
    item_type: &str,
    status: &str,
    metadata: &serde_json::Value,
) -> Option<FeedAttentionLane> {
    match (item_type, status) {
        ("approval", "needs_action") => Some(FeedAttentionLane::Approvals),
        ("escalation", "needs_action") => Some(FeedAttentionLane::Escalations),
        ("task" | "approval" | "escalation", "failed") => Some(FeedAttentionLane::Failed),
        ("task", "running") if feed_task_is_currently_active(metadata) => {
            Some(FeedAttentionLane::Running)
        },
        ("approval" | "escalation", "running") => Some(FeedAttentionLane::Running),
        _ => None,
    }
}

fn feed_task_is_currently_active(metadata: &serde_json::Value) -> bool {
    let object = metadata.as_object();
    let task_status = object
        .and_then(|value| value.get("task_status"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let has_task_status = task_status.is_some();
    let has_active_root_key =
        object.is_some_and(|value| value.contains_key("active_root_execution_id"));
    if !has_task_status && !has_active_root_key {
        return true;
    }
    if task_status.is_some_and(|status| {
        !matches!(
            status.to_ascii_lowercase().as_str(),
            "planning" | "running" | "waiting_for_children" | "paused"
        )
    }) {
        return false;
    }
    if has_active_root_key
        && object
            .and_then(|value| value.get("active_root_execution_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        return false;
    }
    true
}

impl FeedStoreInner {
    fn acquire_write_guard(&self) -> Result<CrossProcessWriteGuard<'_>> {
        self.acquire_file_guard(Some(self.admission.enter()?))
    }
    fn acquire_file_guard(
        &self,
        permit: Option<DatabasePermit>,
    ) -> Result<CrossProcessWriteGuard<'_>> {
        let file = self
            .write_lock
            .lock()
            .expect("feed file lock mutex poisoned");
        file.lock_exclusive()
            .context("acquiring cross-process feed write lock")?;
        Ok(CrossProcessWriteGuard {
            file,
            _admission: permit,
        })
    }

    fn read_connection(&self) -> Result<DatabaseReadConnection> {
        let permit = self.admission.enter()?;
        let conn = self
            .write_conn
            .lock()
            .map_err(|_| anyhow::anyhow!("feed connection lock poisoned"))?
            .try_clone()?;
        Ok(DatabaseReadConnection::new(conn, permit))
    }
}

impl Drop for CrossProcessWriteGuard<'_> {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn map_feed_item_row(row: &duckdb::Row<'_>) -> Result<FeedItem> {
    let item_type: String = row.get(3)?;
    let status: String = row.get(9)?;
    let actions_json: String = row.get(12)?;
    let metadata_json: String = row.get(13)?;
    Ok(FeedItem {
        id: row.get(2)?,
        principal: row.get(0)?,
        workspace: row.get(1)?,
        item_type: FeedItemType::from_db_str(&item_type)?,
        task_id: row.get(4)?,
        ui_thread_id: row.get(5)?,
        agent_id: row.get(6)?,
        title: row.get(7)?,
        summary: row.get(8)?,
        status: FeedItemStatus::from_db_str(&status)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        actions: serde_json::from_str(&actions_json).context("parsing feed actions JSON")?,
        metadata: serde_json::from_str(&metadata_json).context("parsing feed metadata JSON")?,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::feed::types::{FeedItem, FeedItemStatus, FeedItemType};

    fn sample_item(principal: &str, workspace: &str, id: &str, updated_at: i64) -> FeedItem {
        FeedItem {
            id: id.to_string(),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            item_type: FeedItemType::Task,
            task_id: Some(id.to_string()),
            ui_thread_id: Some("general".to_string()),
            agent_id: Some("atlas".to_string()),
            title: format!("Task {id}"),
            summary: Some("summary".to_string()),
            status: FeedItemStatus::Running,
            created_at: updated_at - 1,
            updated_at,
            actions: Vec::new(),
            metadata: serde_json::json!({"k":"v"}),
        }
    }

    #[test]
    fn known_duckdb_corruption_errors_are_recoverable() {
        let error = anyhow::anyhow!(
            "Serialization Error: Failed to deserialize: field id mismatch, expected: 101, got: 65535"
        );
        assert!(is_recoverable_feed_corruption(&error));
        assert!(!is_recoverable_feed_corruption(&anyhow::anyhow!(
            "Binder Error: referenced column does not exist"
        )));
    }

    #[tokio::test]
    async fn feed_compaction_drains_readers_preserves_rows_and_restores_budget() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        store
            .upsert_item(sample_item("alpha", "prod", "task:1", 100))
            .await
            .unwrap();
        let inner = store.scope_inner("alpha", "prod").unwrap();
        let read = inner.read_connection().unwrap();
        assert!(store
            .compact_scope_with_wait("alpha", "prod", std::time::Duration::ZERO)
            .await
            .is_err());
        drop(read);
        let report = store
            .compact_scope_with_wait("alpha", "prod", std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(report.row_count >= 1);
        store
            .upsert_item(sample_item("alpha", "prod", "task:2", 101))
            .await
            .unwrap();
        let read = inner.read_connection().unwrap();
        assert_eq!(
            read.query_row("SELECT count(*) FROM feed_items", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            read.query_row("SELECT current_setting('threads')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn feed_quarantine_retention_is_count_and_size_bounded() {
        let tmp = TempDir::new().unwrap();
        let old = tmp.path().join("feed.corrupt-100-old.duckdb");
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(path_with_appended_suffix(&old, ".wal"), b"wal").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let newest = tmp.path().join("feed.corrupt-200-new.duckdb");
        std::fs::write(&newest, b"newer").unwrap();

        prune_feed_quarantines(tmp.path(), 1, 16).unwrap();
        assert!(!old.exists());
        assert!(!path_with_appended_suffix(&old, ".wal").exists());
        assert!(newest.exists());

        prune_feed_quarantines(tmp.path(), 1, 4).unwrap();
        assert!(!newest.exists());
    }

    #[test]
    fn feed_upsert_ignores_incoming_created_at_but_not_visible_changes() {
        let persisted = sample_item("alpha", "prod", "task:1", 100);
        let mut same = persisted.clone();
        same.created_at += 500;
        assert!(feed_item_upsert_is_unchanged(&persisted, &same));

        same.updated_at += 1;
        assert!(!feed_item_upsert_is_unchanged(&persisted, &same));
    }

    #[tokio::test]
    async fn corrupt_derived_database_is_quarantined_and_rebuilt_on_materialize() {
        let tmp = TempDir::new().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());
        let db_path = workspace_layout.ui_feed_db_path("alpha", "prod");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(&db_path, b"not a duckdb database").unwrap();

        let store = FeedStore::open_workspace(workspace_layout).unwrap();
        store.materialize_scope("alpha", "prod").await.unwrap();

        assert!(store
            .list_items(FeedQuery {
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap()
            .is_empty());
        let quarantine_count = std::fs::read_dir(db_path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("feed.corrupt-")
            })
            .count();
        assert_eq!(quarantine_count, 1);
    }

    #[tokio::test]
    async fn list_items_is_scope_isolated() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        store
            .upsert_item(sample_item("alpha", "prod", "task:1", 100))
            .await
            .unwrap();
        store
            .upsert_item(sample_item("beta", "prod", "task:2", 200))
            .await
            .unwrap();

        let items = store
            .list_items(FeedQuery {
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "task:1");
    }

    #[tokio::test]
    async fn list_items_can_filter_by_task_id() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let mut item_one = sample_item("alpha", "prod", "item:1", 100);
        item_one.task_id = Some("task-a".to_string());
        let mut item_two = sample_item("alpha", "prod", "item:2", 200);
        item_two.task_id = Some("task-b".to_string());
        store.upsert_item(item_one).await.unwrap();
        store.upsert_item(item_two).await.unwrap();

        let items = store
            .list_items(FeedQuery {
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                task_id: Some("task-b".to_string()),
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "item:2");
        assert_eq!(items[0].task_id.as_deref(), Some("task-b"));
    }

    #[tokio::test]
    async fn keyset_page_does_not_skip_more_than_one_page_with_identical_timestamps() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        for index in 0..205 {
            store
                .upsert_item(sample_item(
                    "alpha",
                    "prod",
                    &format!("task:{index:03}"),
                    1_700_000_000_000,
                ))
                .await
                .unwrap();
        }

        let mut cursor = None;
        let mut ids = Vec::new();
        loop {
            let page = store
                .list_items_page(
                    FeedQuery {
                        principal: "alpha".to_string(),
                        workspace: "prod".to_string(),
                        limit: 50,
                        ..Default::default()
                    },
                    cursor,
                )
                .await
                .unwrap();
            ids.extend(page.items.into_iter().map(|item| item.id));
            if !page.has_more {
                break;
            }
            cursor = page.next_cursor;
        }

        assert_eq!(ids.len(), 205);
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 205);
        assert_eq!(ids.first().map(String::as_str), Some("task:204"));
        assert_eq!(ids.last().map(String::as_str), Some("task:000"));
    }

    #[tokio::test]
    async fn attention_lane_page_excludes_internal_task_rows() {
        // Internal tasks keep their failures out of operator attention: rows
        // owned by an excluded task_id must vanish from the page, the total
        // AND has_more, while unowned rows in the same lane stay.
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        for index in 0..3 {
            let mut item = sample_item("alpha", "prod", &format!("task:{index}"), 100 + index);
            item.item_type = FeedItemType::Task;
            item.status = FeedItemStatus::Failed;
            item.task_id = Some(format!("internal-{index}"));
            store.upsert_item(item).await.unwrap();
        }
        let mut user_item = sample_item("alpha", "prod", "task:user", 300);
        user_item.item_type = FeedItemType::Task;
        user_item.status = FeedItemStatus::Failed;
        user_item.task_id = Some("user-task".to_string());
        store.upsert_item(user_item).await.unwrap();
        let mut orphan_item = sample_item("alpha", "prod", "task:orphan", 400);
        orphan_item.item_type = FeedItemType::Escalation;
        orphan_item.status = FeedItemStatus::Failed;
        store.upsert_item(orphan_item).await.unwrap();

        let page = store
            .list_attention_lane_page(FeedAttentionPageQuery {
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Failed,
                ui_thread_id: None,
                cursor: None,
                offset: 0,
                limit: 10,
                exclude_task_ids: vec![
                    "internal-0".to_string(),
                    "internal-1".to_string(),
                    "internal-2".to_string(),
                ],
            })
            .await
            .unwrap();
        let ids: Vec<String> = page.items.iter().map(|item| item.id.clone()).collect();
        assert!(ids.contains(&"task:user".to_string()));
        assert!(ids.contains(&"task:orphan".to_string()));
        assert_eq!(page.total, 2);
        assert!(!page.has_more);
    }

    #[tokio::test]
    async fn attention_lane_page_has_exact_filtered_total_and_has_more() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        for index in 0..5 {
            let mut item = sample_item("alpha", "prod", &format!("approval:{index}"), 100 + index);
            item.item_type = FeedItemType::Approval;
            item.status = FeedItemStatus::NeedsAction;
            item.task_id = Some(format!("task-{index}"));
            store.upsert_item(item).await.unwrap();
        }
        store
            .remove_task_items("alpha", "prod", "task-4")
            .await
            .unwrap();
        store
            .set_attention_dismissed("alpha", "prod", "approval:3", true, 200)
            .await
            .unwrap();

        let first = store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Approvals,
                ui_thread_id: None,
                cursor: None,
                offset: 0,
                limit: 2,
            })
            .await
            .unwrap();

        assert_eq!(first.total, 3);
        assert_eq!(first.items.len(), 2);
        assert!(first.has_more);
        let second = store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Approvals,
                ui_thread_id: None,
                cursor: first.next_cursor,
                offset: 0,
                limit: 2,
            })
            .await
            .unwrap();
        assert_eq!(second.total, 3);
        assert_eq!(second.items.len(), 1);
        assert!(!second.has_more);
    }

    #[tokio::test]
    async fn exact_attention_lookup_resolves_alias_beyond_capped_page_and_honors_scope() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let projected = (0..=200)
            .map(|index| {
                let mut item =
                    sample_item("alpha", "prod", &format!("v3:attention:{index:03}"), index);
                item.status = FeedItemStatus::NeedsAction;
                item.metadata = serde_json::json!({
                    "attention_kind": "hitl.requested",
                    "pause_state_id": if index == 0 {
                        "pause/id with spaces"
                    } else {
                        "another-pause"
                    },
                });
                item
            })
            .collect::<Vec<_>>();
        store
            .reconcile_attention_projection(
                "alpha",
                "prod",
                FeedAttentionLane::Requests,
                "test",
                "capped-page",
                1,
                None,
                projected,
            )
            .await
            .unwrap();

        let first_page = store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Requests,
                ui_thread_id: None,
                cursor: None,
                offset: 0,
                limit: 200,
            })
            .await
            .unwrap();
        assert!(first_page.has_more);
        assert!(first_page
            .items
            .iter()
            .all(|item| item.id != "v3:attention:000"));

        let exact = store
            .get_attention_item_by_alias("alpha", "prod", "pause/id with spaces")
            .await
            .unwrap()
            .expect("metadata alias should resolve outside the first page");
        assert_eq!(exact.id, "v3:attention:000");
        assert!(store
            .get_attention_item_by_alias("alpha", "other-workspace", "pause/id with spaces")
            .await
            .unwrap()
            .is_none());

        store
            .set_attention_dismissed("alpha", "prod", &exact.id, true, 300)
            .await
            .unwrap();
        assert!(store
            .attention_item_is_dismissed("alpha", "prod", &exact.id)
            .await
            .unwrap());
        assert!(!store
            .attention_item_is_dismissed("alpha", "other-workspace", &exact.id)
            .await
            .unwrap());
        assert!(store
            .get_attention_item_by_alias("alpha", "prod", "pause/id with spaces")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn attention_projection_reconciliation_removes_obsolete_rows() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let mut request = sample_item("alpha", "prod", "v3:attention:request-1", 100);
        request.status = FeedItemStatus::NeedsAction;
        request.task_id = Some("task-1".to_string());
        request.metadata = serde_json::json!({"attention_kind": "hitl.requested"});
        let recovery_started_at = chrono::Utc::now().timestamp_millis() - 1;

        let first = store
            .reconcile_attention_projection(
                "alpha",
                "prod",
                FeedAttentionLane::Requests,
                "v3_task_attention",
                "task-1",
                100,
                None,
                vec![request],
            )
            .await
            .unwrap();
        assert_eq!(first.upserted, 1);
        assert_eq!(first.removed, 0);
        let first_page = store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Requests,
                ui_thread_id: None,
                cursor: None,
                offset: 0,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(first_page.request_hitl_total, Some(1));

        let duplicate = store
            .reconcile_attention_projection(
                "alpha",
                "prod",
                FeedAttentionLane::Requests,
                "v3_task_attention",
                "task-1",
                100,
                None,
                first_page.items.clone(),
            )
            .await
            .unwrap();
        assert!(duplicate.skipped_stale);

        let older_publish = store
            .reconcile_attention_projection(
                "alpha",
                "prod",
                FeedAttentionLane::Requests,
                "v3_task_attention",
                "task-1",
                50,
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        assert!(older_publish.skipped_stale);

        let stale = store
            .reconcile_attention_projection(
                "alpha",
                "prod",
                FeedAttentionLane::Requests,
                "v3_task_attention",
                "task-1",
                200,
                Some(recovery_started_at),
                Vec::new(),
            )
            .await
            .unwrap();
        assert!(stale.skipped_stale);

        let second = store
            .reconcile_attention_projection(
                "alpha",
                "prod",
                FeedAttentionLane::Requests,
                "v3_task_attention",
                "task-1",
                200,
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        assert_eq!(second.removed, 1);
        let page = store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Requests,
                ui_thread_id: None,
                cursor: None,
                offset: 0,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(page.total, 0);
        assert!(page.items.is_empty());
    }

    #[tokio::test]
    async fn attention_union_prefers_projection_for_duplicate_scoped_id() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let mut ordinary = sample_item("alpha", "prod", "shared-id", 200);
        ordinary.item_type = FeedItemType::Approval;
        ordinary.status = FeedItemStatus::NeedsAction;
        store.upsert_item(ordinary).await.unwrap();

        let mut projected = sample_item("alpha", "prod", "shared-id", 100);
        projected.status = FeedItemStatus::NeedsAction;
        projected.metadata = serde_json::json!({"attention_kind": "hitl.requested"});
        store
            .reconcile_attention_projection(
                "alpha",
                "prod",
                FeedAttentionLane::Requests,
                "v3_task_attention",
                "task-1",
                100,
                None,
                vec![projected],
            )
            .await
            .unwrap();

        let requests = store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Requests,
                ui_thread_id: None,
                cursor: None,
                offset: 0,
                limit: 10,
            })
            .await
            .unwrap();
        let approvals = store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Approvals,
                ui_thread_id: None,
                cursor: None,
                offset: 0,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(requests.total, 1);
        assert_eq!(requests.items[0].updated_at, 100);
        assert_eq!(approvals.total, 0);
    }

    #[tokio::test]
    async fn newer_undismiss_tombstone_beats_stale_startup_import() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let mut item = sample_item("alpha", "prod", "approval:1", 100);
        item.item_type = FeedItemType::Approval;
        item.status = FeedItemStatus::NeedsAction;
        store.upsert_item(item).await.unwrap();
        store
            .set_attention_dismissed("alpha", "prod", "approval:1", false, 200)
            .await
            .unwrap();
        store
            .merge_attention_dismissals(
                "alpha",
                "prod",
                vec![("approval:1".to_string(), true, 100)],
            )
            .await
            .unwrap();
        let page = store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Approvals,
                ui_thread_id: None,
                cursor: None,
                offset: 0,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(page.total, 1);
    }

    /// **The UPDATE branch**, which is the one that replaced the
    /// `INSERT .. ON CONFLICT` that segfaulted DuckDB. Nothing else reached it:
    /// every other dismissal test writes an id exactly once, and
    /// `newer_undismiss_tombstone_beats_stale_startup_import` takes the skip.
    /// Reverting the UPDATE to `ON CONFLICT` therefore left the whole suite
    /// green, which is the state this test exists to end.
    #[tokio::test]
    async fn a_newer_merge_rewrites_an_existing_dismissal() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let mut item = sample_item("alpha", "prod", "approval:1", 100);
        item.item_type = FeedItemType::Approval;
        item.status = FeedItemStatus::NeedsAction;
        store.upsert_item(item).await.unwrap();

        store
            .set_attention_dismissed("alpha", "prod", "approval:1", false, 100)
            .await
            .unwrap();
        assert!(
            !store
                .attention_item_is_dismissed("alpha", "prod", "approval:1")
                .await
                .unwrap(),
            "the row has to already exist, or the merge below is an INSERT and proves nothing"
        );

        let changed = store
            .merge_attention_dismissals(
                "alpha",
                "prod",
                vec![("approval:1".to_string(), true, 200)],
            )
            .await
            .unwrap();

        assert_eq!(changed, 1, "one row was rewritten");
        assert!(
            store
                .attention_item_is_dismissed("alpha", "prod", "approval:1")
                .await
                .unwrap(),
            "a strictly newer record replaces the stored state"
        );
    }

    /// A tie keeps the stored row, and that is what makes the tombstone hold.
    /// `updated_at` is milliseconds and a startup import replays in bulk, so an
    /// equal stamp is ordinary; under `>` this item would silently re-hide.
    #[tokio::test]
    async fn a_tied_timestamp_cannot_rehide_an_undismissed_item() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let mut item = sample_item("alpha", "prod", "approval:1", 100);
        item.item_type = FeedItemType::Approval;
        item.status = FeedItemStatus::NeedsAction;
        store.upsert_item(item).await.unwrap();

        store
            .set_attention_dismissed("alpha", "prod", "approval:1", false, 200)
            .await
            .unwrap();

        let changed = store
            .merge_attention_dismissals(
                "alpha",
                "prod",
                vec![("approval:1".to_string(), true, 200)],
            )
            .await
            .unwrap();

        assert_eq!(
            changed, 0,
            "a tie writes nothing, so it is not counted as a change either"
        );
        assert!(
            !store
                .attention_item_is_dismissed("alpha", "prod", "approval:1")
                .await
                .unwrap(),
            "an import carrying the same millisecond must not re-hide the item"
        );
    }

    #[tokio::test]
    async fn reopening_scope_retires_legacy_dismissal_secondary_index() {
        let tmp = TempDir::new().unwrap();
        {
            let store = FeedStore::open(tmp.path()).unwrap();
            store.materialize_scope("alpha", "prod").await.unwrap();
            let inner = store.scope_inner("alpha", "prod").unwrap();
            let conn = inner.read_connection().unwrap();
            conn.execute_batch(
                "CREATE INDEX idx_feed_attention_dismissed_item
                 ON feed_attention_dismissals (principal, workspace, dismissed, id)",
            )
            .unwrap();
        }

        let reopened = FeedStore::open(tmp.path()).unwrap();
        reopened.materialize_scope("alpha", "prod").await.unwrap();
        let inner = reopened.scope_inner("alpha", "prod").unwrap();
        let conn = inner.read_connection().unwrap();
        let index_count = conn
            .query_row(
                "SELECT COUNT(*) FROM duckdb_indexes()
                 WHERE index_name = 'idx_feed_attention_dismissed_item'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();

        assert_eq!(index_count, 0);
    }

    #[tokio::test]
    async fn counts_group_by_status() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let mut running = sample_item("alpha", "prod", "task:1", 100);
        running.status = FeedItemStatus::Running;
        let mut needs_action = sample_item("alpha", "prod", "approval:1", 200);
        needs_action.item_type = FeedItemType::Approval;
        needs_action.status = FeedItemStatus::NeedsAction;
        let mut failed = sample_item("alpha", "prod", "task:2", 300);
        failed.status = FeedItemStatus::Failed;
        store.upsert_item(running).await.unwrap();
        store.upsert_item(needs_action).await.unwrap();
        store.upsert_item(failed).await.unwrap();

        let counts = store.counts("alpha", "prod", None).await.unwrap();
        assert_eq!(counts.total, 3);
        assert_eq!(counts.running, 1);
        assert_eq!(counts.needs_action, 1);
        assert_eq!(counts.failed, 1);
    }

    #[tokio::test]
    async fn counts_returns_zeroes_for_empty_scope() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();

        let counts = store.counts("alpha", "prod", None).await.unwrap();
        assert_eq!(counts.total, 0);
        assert_eq!(counts.running, 0);
        assert_eq!(counts.needs_action, 0);
        assert_eq!(counts.failed, 0);
        assert_eq!(counts.done, 0);
        assert_eq!(counts.info, 0);
    }

    #[tokio::test]
    async fn count_items_filters_by_type_and_status() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let mut approval = sample_item("alpha", "prod", "approval:1", 100);
        approval.item_type = FeedItemType::Approval;
        approval.status = FeedItemStatus::NeedsAction;
        let mut escalation = sample_item("alpha", "prod", "escalation:1", 200);
        escalation.item_type = FeedItemType::Escalation;
        escalation.status = FeedItemStatus::NeedsAction;
        let mut failed = sample_item("alpha", "prod", "task:2", 300);
        failed.status = FeedItemStatus::Failed;
        store.upsert_item(approval).await.unwrap();
        store.upsert_item(escalation).await.unwrap();
        store.upsert_item(failed).await.unwrap();

        let approval_count = store
            .count_items(
                "alpha",
                "prod",
                None,
                Some(FeedItemType::Approval),
                Some(FeedItemStatus::NeedsAction),
            )
            .await
            .unwrap();
        let escalation_count = store
            .count_items(
                "alpha",
                "prod",
                None,
                Some(FeedItemType::Escalation),
                Some(FeedItemStatus::NeedsAction),
            )
            .await
            .unwrap();
        let failed_count = store
            .count_items("alpha", "prod", None, None, Some(FeedItemStatus::Failed))
            .await
            .unwrap();

        assert_eq!(approval_count, 1);
        assert_eq!(escalation_count, 1);
        assert_eq!(failed_count, 1);
    }

    #[tokio::test]
    async fn remove_task_items_preserves_delivery_rows_with_task_provenance() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let mut task = sample_item("alpha", "prod", "v3:task:task-1", 100);
        task.task_id = Some("task-1".to_string());
        let mut delivery = sample_item("alpha", "prod", "data_delivery:surface-1", 200);
        delivery.item_type = FeedItemType::DataDelivery;
        delivery.status = FeedItemStatus::Done;
        delivery.task_id = Some("task-1".to_string());
        store.upsert_item(task).await.unwrap();
        store.upsert_item(delivery.clone()).await.unwrap();

        let removed = store
            .remove_task_items("alpha", "prod", "task-1")
            .await
            .unwrap();

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].id, "v3:task:task-1");
        assert!(store
            .get_item("alpha", "prod", "data_delivery:surface-1")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn purge_orphan_items_preserves_delivery_rows_with_task_provenance() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        let mut task = sample_item("alpha", "prod", "v3:task:stale", 100);
        task.task_id = Some("stale-task".to_string());
        let mut delivery = sample_item("alpha", "prod", "data_delivery:surface-1", 200);
        delivery.item_type = FeedItemType::DataDelivery;
        delivery.status = FeedItemStatus::Done;
        delivery.task_id = Some("stale-task".to_string());
        store.upsert_item(task).await.unwrap();
        store.upsert_item(delivery).await.unwrap();

        let removed = store
            .purge_orphan_items("alpha", "prod", &std::collections::HashSet::new())
            .await
            .unwrap();

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].id, "v3:task:stale");
        assert!(store
            .get_item("alpha", "prod", "data_delivery:surface-1")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn first_scoped_write_seeds_template_and_materializes_scoped_db() {
        let tmp = TempDir::new().unwrap();
        let store = FeedStore::open(tmp.path()).unwrap();
        store
            .upsert_item(sample_item("anonymous", "default", "task:1", 100))
            .await
            .unwrap();

        let workspace = ArtifactV2Workspace::new(tmp.path());
        assert!(workspace.feed_db_template_schema_path().exists());
        assert!(workspace.ui_feed_db_path("anonymous", "default").exists());
    }
}
