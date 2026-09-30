use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::Utc;
use duckdb::{params, AccessMode, Config, Connection};
use fs2::FileExt;

use crate::magician_v2::{artifact_v2::workspace::ArtifactV2Workspace, history::HistoryLane};

use super::types::{UiThreadPage, UiThreadRecord, UiThreadSearchCandidate, UiThreadUpdate};

const BOOTSTRAP_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS ui_threads (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    sort_order BIGINT NOT NULL DEFAULT 0,
    memory_summary TEXT NULL,
    memory_updated_at BIGINT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    display_mode TEXT NOT NULL DEFAULT 'chat',
    plan_mode BOOLEAN NOT NULL DEFAULT FALSE,
    history_lane TEXT NOT NULL DEFAULT 'personal',
    deleted_at BIGINT NULL,
    PRIMARY KEY (principal, workspace, id)
);
CREATE INDEX IF NOT EXISTS idx_ui_threads_scope_order
    ON ui_threads (principal, workspace, deleted_at, archived, sort_order ASC, updated_at DESC, id ASC);
CREATE INDEX IF NOT EXISTS idx_ui_threads_scope_history
    ON ui_threads (principal, workspace, deleted_at, history_lane, updated_at DESC, id ASC);
"#;

/// Idempotent migration to add `display_mode` and `plan_mode` to
/// pre-existing rows on upgrade. DuckDB does not support adding columns
/// with constraints, so the migration adds nullable columns and then
/// backfills the values used by fresh databases.
const MIGRATIONS: &str = r#"
ALTER TABLE ui_threads ADD COLUMN IF NOT EXISTS display_mode TEXT;
ALTER TABLE ui_threads ADD COLUMN IF NOT EXISTS plan_mode BOOLEAN;
ALTER TABLE ui_threads ADD COLUMN IF NOT EXISTS history_lane TEXT;
ALTER TABLE ui_threads ADD COLUMN IF NOT EXISTS deleted_at BIGINT;
UPDATE ui_threads SET display_mode = 'chat' WHERE display_mode IS NULL;
UPDATE ui_threads SET plan_mode = FALSE WHERE plan_mode IS NULL;
UPDATE ui_threads
SET history_lane = CASE
    WHEN id = 'general' THEN 'personal'
    ELSE 'automated'
END
WHERE history_lane IS NULL OR history_lane NOT IN ('personal', 'automated');
CREATE INDEX IF NOT EXISTS idx_ui_threads_scope_history
    ON ui_threads (principal, workspace, deleted_at, history_lane, updated_at DESC, id ASC);
"#;

#[derive(Debug, Clone)]
pub struct UiThreadStore {
    base_root: PathBuf,
    workspace_layout: ArtifactV2Workspace,
    scoped: Arc<Mutex<HashMap<(String, String), Arc<UiThreadStoreInner>>>>,
    scope_gates: Arc<tokio::sync::Mutex<HashMap<(String, String), Arc<tokio::sync::Mutex<()>>>>>,
}

struct UiThreadStoreInner {
    db_path: PathBuf,
    write_conn: Mutex<Connection>,
    write_lock: Mutex<File>,
    last_checkpoint_at: Mutex<Instant>,
}

impl std::fmt::Debug for UiThreadStoreInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiThreadStoreInner")
            .field("db_path", &self.db_path)
            .finish_non_exhaustive()
    }
}

const CHECKPOINT_THROTTLE: Duration = Duration::from_secs(30);

struct CrossProcessWriteGuard<'a> {
    file: std::sync::MutexGuard<'a, File>,
}

impl UiThreadStore {
    pub fn open(base_root: &Path) -> Result<Self> {
        Self::open_workspace(ArtifactV2Workspace::new(base_root))
    }

    pub fn open_workspace(workspace_layout: ArtifactV2Workspace) -> Result<Self> {
        let base_root = workspace_layout.base_root().to_path_buf();
        workspace_layout
            .ensure_root_sync()
            .with_context(|| format!("creating ui thread base root: {}", base_root.display()))?;
        Ok(Self {
            base_root,
            workspace_layout,
            scoped: Arc::new(Mutex::new(HashMap::new())),
            scope_gates: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        })
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

    async fn spawn_ui_thread_blocking<T, F>(
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
            .with_context(|| format!("ui thread {label} task panicked"))?
    }

    pub async fn materialize_scope(&self, principal: &str, workspace: &str) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        self.spawn_ui_thread_blocking(
            principal.clone(),
            workspace.clone(),
            "materialize_scope",
            move || {
                let _ = store.scope_inner(&principal, &workspace)?;
                Ok(())
            },
        )
        .await
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

    fn scope_inner(&self, principal: &str, workspace: &str) -> Result<Arc<UiThreadStoreInner>> {
        let key = (principal.to_string(), workspace.to_string());
        if let Some(existing) = self
            .scoped
            .lock()
            .expect("ui thread scoped store mutex poisoned")
            .get(&key)
            .cloned()
        {
            return Ok(existing);
        }

        let data_dir = self.workspace_layout.ui_threads_dir(principal, workspace);
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("creating ui thread data dir: {}", data_dir.display()))?;
        let db_path = crate::magician_v2::database_owners::database_file_path(
            &self.workspace_layout,
            principal,
            workspace,
            crate::magician_v2::database_owners::DatabaseOwner::UiThreadsDuckdb,
        );
        let lock_path = self
            .workspace_layout
            .ui_threads_lock_path(principal, workspace);
        self.ensure_template_seeded()?;
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening ui thread lock file at {}", lock_path.display()))?;
        lock_file
            .lock_exclusive()
            .context("acquiring ui thread scope bootstrap lock")?;
        if let Some(existing) = self
            .scoped
            .lock()
            .expect("ui thread scoped store mutex poisoned")
            .get(&key)
            .cloned()
        {
            let _ = lock_file.unlock();
            return Ok(existing);
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening ui thread store at {}", db_path.display()))?;
        let template_schema =
            std::fs::read_to_string(self.workspace_layout.ui_threads_db_template_schema_path())
                .with_context(|| {
                    format!(
                        "reading ui thread template schema: {}",
                        self.workspace_layout
                            .ui_threads_db_template_schema_path()
                            .display()
                    )
                })?;
        apply_schema_and_migrations(&conn, &template_schema)?;
        checkpoint_connection(&conn).context("checkpointing ui thread schema bootstrap")?;
        lock_file
            .unlock()
            .context("releasing ui thread scope bootstrap lock")?;
        let inner = Arc::new(UiThreadStoreInner {
            db_path,
            write_conn: Mutex::new(conn),
            write_lock: Mutex::new(lock_file),
            last_checkpoint_at: Mutex::new(Instant::now()),
        });
        self.scoped
            .lock()
            .expect("ui thread scoped store mutex poisoned")
            .insert(key, Arc::clone(&inner));
        Ok(inner)
    }

    fn ensure_template_seeded(&self) -> Result<()> {
        // A read-only seed (container/deployment) ships the schema; never write into it.
        if self.workspace_layout.templates_are_read_only() {
            return Ok(());
        }
        let template_dir = self.workspace_layout.ui_threads_db_template_dir();
        std::fs::create_dir_all(&template_dir).with_context(|| {
            format!(
                "creating ui thread template dir: {}",
                template_dir.display()
            )
        })?;
        let schema_path = self.workspace_layout.ui_threads_db_template_schema_path();
        let should_seed = match std::fs::read_to_string(&schema_path) {
            Ok(existing) => template_schema_is_stale(&existing),
            Err(error) if error.kind() == ErrorKind::NotFound => true,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "reading ui thread template schema: {}",
                        schema_path.display()
                    )
                });
            },
        };
        if should_seed {
            std::fs::write(&schema_path, BOOTSTRAP_DDL).with_context(|| {
                format!(
                    "writing ui thread template schema: {}",
                    schema_path.display()
                )
            })?;
        }
        Ok(())
    }

    pub async fn get_thread(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
    ) -> Result<Option<UiThreadRecord>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let id = id.to_string();
        self.spawn_ui_thread_blocking(principal.clone(), workspace.clone(), "get_thread", move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT principal, workspace, id, name, archived, sort_order, memory_summary,
                        memory_updated_at, created_at, updated_at, display_mode, plan_mode, history_lane
                 FROM ui_threads
                 WHERE principal = ? AND workspace = ? AND id = ? AND deleted_at IS NULL",
            )?;
            let mut rows = stmt.query(params![principal, workspace, id])?;
            if let Some(row) = rows.next()? {
                Ok(Some(map_row(row)?))
            } else {
                Ok(None)
            }
        })
        .await
    }

    pub async fn list_threads(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<UiThreadRecord>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        self.spawn_ui_thread_blocking(principal.clone(), workspace.clone(), "list_threads", move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT principal, workspace, id, name, archived, sort_order, memory_summary,
                        memory_updated_at, created_at, updated_at, display_mode, plan_mode, history_lane
                 FROM ui_threads
                 WHERE principal = ? AND workspace = ? AND deleted_at IS NULL
                 ORDER BY CASE WHEN id = 'general' THEN 0 ELSE 1 END,
                          archived ASC,
                          sort_order ASC,
                          updated_at DESC,
                          id ASC",
            )?;
            let mut rows = stmt.query(params![principal, workspace])?;
            let mut records = Vec::new();
            while let Some(row) = rows.next()? {
                records.push(map_row(row)?);
            }
            Ok(records)
        })
        .await
    }

    pub async fn list_threads_page(
        &self,
        principal: &str,
        workspace: &str,
        lane: Option<HistoryLane>,
        search: &str,
        limit: usize,
        offset: usize,
    ) -> Result<UiThreadPage> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let lane = lane.map(|value| value.as_str().to_string());
        let search = search.trim().to_ascii_lowercase();
        self.spawn_ui_thread_blocking(principal.clone(), workspace.clone(), "list_threads_page", move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let lane_filter = lane.clone();
            let total = conn.query_row(
                "SELECT COUNT(*)
                 FROM ui_threads
                 WHERE principal = ? AND workspace = ? AND deleted_at IS NULL
                   AND id NOT IN ('agent-personal-assistant', 'system-meta-agent')
                   AND (? IS NULL OR history_lane = ?)
                   AND (? = '' OR contains(lower(name), ?) OR contains(lower(id), ?))",
                params![
                    principal.clone(),
                    workspace.clone(),
                    lane_filter.clone(),
                    lane_filter,
                    search.clone(),
                    search.clone(),
                    search.clone(),
                ],
                |row| row.get::<_, i64>(0),
            )? as usize;
            let mut stmt = conn.prepare(
                "SELECT principal, workspace, id, name, archived, sort_order, memory_summary,
                        memory_updated_at, created_at, updated_at, display_mode, plan_mode, history_lane
                 FROM ui_threads
                 WHERE principal = ? AND workspace = ? AND deleted_at IS NULL
                   AND id NOT IN ('agent-personal-assistant', 'system-meta-agent')
                   AND (? IS NULL OR history_lane = ?)
                   AND (? = '' OR contains(lower(name), ?) OR contains(lower(id), ?))
                 ORDER BY CASE WHEN id = 'general' THEN 0 ELSE 1 END,
                          archived ASC,
                          sort_order ASC,
                          updated_at DESC,
                          id ASC
                 LIMIT ? OFFSET ?",
            )?;
            let lane_filter = lane.clone();
            let mut rows = stmt.query(params![
                principal,
                workspace,
                lane_filter.clone(),
                lane_filter,
                search.clone(),
                search.clone(),
                search,
                limit as i64,
                offset as i64,
            ])?;
            let mut threads = Vec::new();
            while let Some(row) = rows.next()? {
                threads.push(map_row(row)?);
            }
            Ok(UiThreadPage {
                threads,
                total,
                limit,
                offset,
            })
        })
        .await
    }

    /// Search materialized thread metadata without running the legacy scope
    /// synchronizer or loading full thread records.
    pub async fn search_thread_candidates(
        &self,
        principal: &str,
        workspace: &str,
        search: &str,
    ) -> Result<Vec<UiThreadSearchCandidate>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let search = search.trim().to_ascii_lowercase();
        self.spawn_ui_thread_blocking(
            principal.clone(),
            workspace.clone(),
            "search_thread_candidates",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let conn = inner.read_connection()?;
                let mut stmt = conn.prepare(
                    "SELECT id, updated_at
                 FROM ui_threads
                 WHERE principal = ? AND workspace = ? AND deleted_at IS NULL
                   AND id NOT IN ('agent-personal-assistant', 'system-meta-agent')
                   AND (contains(lower(name), ?) OR contains(lower(id), ?))
                 ORDER BY updated_at DESC, id ASC",
                )?;
                let mut rows =
                    stmt.query(params![principal, workspace, search.clone(), search,])?;
                let mut candidates = Vec::new();
                while let Some(row) = rows.next()? {
                    candidates.push(UiThreadSearchCandidate {
                        id: row.get(0)?,
                        updated_at: row.get(1)?,
                    });
                }
                Ok(candidates)
            },
        )
        .await
    }

    pub async fn upsert_thread(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        name: &str,
    ) -> Result<UiThreadRecord> {
        self.upsert_thread_with_lane(principal, workspace, id, name, HistoryLane::Personal)
            .await
    }

    pub async fn upsert_thread_with_lane(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        name: &str,
        history_lane: HistoryLane,
    ) -> Result<UiThreadRecord> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let id = id.to_string();
        let name = name.to_string();
        let history_lane = history_lane.as_str().to_string();
        self.spawn_ui_thread_blocking(principal.clone(), workspace.clone(), "upsert", move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let now = Utc::now().timestamp_millis();
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("ui thread write connection mutex poisoned");

            let existing: Option<(i64, i64, Option<i64>)> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT sort_order, created_at, deleted_at
                         FROM ui_threads
                         WHERE principal = ? AND workspace = ? AND id = ?",
                    )
                    .context("preparing ui thread existence query")?;
                let mut rows = stmt
                    .query(params![principal.clone(), workspace.clone(), id.clone()])
                    .context("querying existing ui thread")?;
                if let Some(row) = rows.next().context("reading existing ui thread row")? {
                    Some((
                        row.get::<_, i64>(0)
                            .context("reading existing sort_order")?,
                        row.get::<_, i64>(1)
                            .context("reading existing created_at")?,
                        row.get::<_, Option<i64>>(2)
                            .context("reading existing deleted_at")?,
                    ))
                } else {
                    None
                }
            };

            if let Some((_, _, deleted_at)) = existing {
                if deleted_at.is_some() {
                    conn.execute(
                        "UPDATE ui_threads
                         SET name = ?, archived = FALSE, history_lane = ?, deleted_at = NULL, updated_at = ?
                         WHERE principal = ? AND workspace = ? AND id = ?",
                        params![name, history_lane, now, principal.clone(), workspace.clone(), id.clone(),],
                    )
                    .context("restoring deleted ui thread during upsert")?;
                } else {
                    conn.execute(
                        "UPDATE ui_threads
                         SET name = ?, updated_at = ?
                         WHERE principal = ? AND workspace = ? AND id = ?",
                        params![name, now, principal.clone(), workspace.clone(), id.clone(),],
                    )
                    .context("updating existing ui thread during upsert")?;
                }
            } else {
                let sort_order = {
                    let mut stmt = conn.prepare(
                        "SELECT COALESCE(MAX(sort_order), -1) + 1
                         FROM ui_threads
                         WHERE principal = ? AND workspace = ?",
                    )?;
                    stmt.query_row(params![principal.clone(), workspace.clone()], |row| {
                        row.get::<_, i64>(0)
                    })?
                };

                conn.execute(
                    "INSERT INTO ui_threads (
                        principal, workspace, id, name, archived, sort_order,
                        memory_summary, memory_updated_at, created_at, updated_at, history_lane
                    ) VALUES (?, ?, ?, ?, FALSE, ?, NULL, NULL, ?, ?, ?)",
                    params![
                        principal.clone(),
                        workspace.clone(),
                        id.clone(),
                        name,
                        sort_order,
                        now,
                        now,
                        history_lane,
                    ],
                )
                .context("inserting new ui thread during upsert")?;
            }

            inner
                .maybe_checkpoint(&conn)
                .context("checkpointing ui thread upsert")?;
            let mut stmt = conn.prepare(
                "SELECT principal, workspace, id, name, archived, sort_order, memory_summary,
                        memory_updated_at, created_at, updated_at, display_mode, plan_mode, history_lane
                 FROM ui_threads
                 WHERE principal = ? AND workspace = ? AND id = ? AND deleted_at IS NULL",
            )?;
            let mut rows = stmt.query(params![principal, workspace, id])?;
            let row = rows.next()?.context("ui thread missing after upsert")?;
            map_row(row)
        })
        .await
    }

    pub async fn update_thread(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        update: UiThreadUpdate,
    ) -> Result<Option<UiThreadRecord>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let id = id.to_string();
        self.spawn_ui_thread_blocking(principal.clone(), workspace.clone(), "update", move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let existing = {
                let conn = inner.read_connection()?;
                let mut stmt = conn.prepare(
                    "SELECT principal, workspace, id, name, archived, sort_order, memory_summary,
                            memory_updated_at, created_at, updated_at, display_mode, plan_mode, history_lane
                     FROM ui_threads
                     WHERE principal = ? AND workspace = ? AND id = ? AND deleted_at IS NULL",
                )?;
                let mut rows = stmt.query(params![principal.clone(), workspace.clone(), id.clone()])?;
                if let Some(row) = rows.next()? {
                    Some(map_row(row)?)
                } else {
                    None
                }
            };

            let Some(existing) = existing else {
                return Ok(None);
            };

            let now = Utc::now().timestamp_millis();
            let name = update.name.unwrap_or(existing.name);
            let archived = if existing.id == "general" {
                false
            } else {
                update.archived.unwrap_or(existing.archived)
            };
            let sort_order = update.sort_order.unwrap_or(existing.sort_order);
            let memory_summary = update.memory_summary.unwrap_or(existing.memory_summary);
            let memory_updated_at = update.memory_updated_at.unwrap_or(existing.memory_updated_at);
            // display_mode is validated by the service layer; if the
            // store sees an unexpected value we fall back to the
            // existing value rather than persist garbage. Treats `None`
            // as "no change".
            let display_mode = update
                .display_mode
                .filter(|v| matches!(v.as_str(), "chat" | "dev"))
                .unwrap_or(existing.display_mode);
            let plan_mode = update.plan_mode.unwrap_or(existing.plan_mode);
            let history_lane = update.history_lane.unwrap_or(existing.history_lane);

            let conn = inner
                .write_conn
                .lock()
                .expect("ui thread write connection mutex poisoned");
            conn.execute(
                "UPDATE ui_threads
                 SET name = ?, archived = ?, sort_order = ?, memory_summary = ?, memory_updated_at = ?, display_mode = ?, plan_mode = ?, history_lane = ?, updated_at = ?
                 WHERE principal = ? AND workspace = ? AND id = ?",
                params![
                    name,
                    archived,
                    sort_order,
                    memory_summary,
                    memory_updated_at,
                    display_mode,
                    plan_mode,
                    history_lane.as_str(),
                    now,
                    principal.clone(),
                    workspace.clone(),
                    id.clone(),
                ],
            )
            .context("updating ui thread")?;
            inner
                .maybe_checkpoint(&conn)
                .context("checkpointing ui thread update")?;
            let mut stmt = conn.prepare(
                "SELECT principal, workspace, id, name, archived, sort_order, memory_summary,
                        memory_updated_at, created_at, updated_at, display_mode, plan_mode, history_lane
                 FROM ui_threads
                 WHERE principal = ? AND workspace = ? AND id = ? AND deleted_at IS NULL",
            )?;
            let mut rows = stmt.query(params![principal, workspace, id])?;
            let row = rows
                .next()?
                .context("ui thread missing after update")?;
            Ok(Some(map_row(row)?))
        })
        .await
    }

    pub async fn reorder_threads(
        &self,
        principal: &str,
        workspace: &str,
        ordered_ids: &[String],
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let ordered_ids = ordered_ids.to_vec();
        self.spawn_ui_thread_blocking(principal.clone(), workspace.clone(), "reorder", move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let existing_ids = {
                let conn = inner.read_connection()?;
                let mut stmt = conn.prepare(
                    "SELECT id FROM ui_threads
                     WHERE principal = ? AND workspace = ? AND deleted_at IS NULL
                     ORDER BY CASE WHEN id = 'general' THEN 0 ELSE 1 END,
                              sort_order ASC,
                              updated_at DESC,
                              id ASC",
                )?;
                let mut rows = stmt.query(params![principal.clone(), workspace.clone()])?;
                let mut ids = Vec::new();
                while let Some(row) = rows.next()? {
                    ids.push(row.get::<_, String>(0)?);
                }
                ids
            };

            let mut final_ids = Vec::new();
            final_ids.push("general".to_string());
            for id in ordered_ids {
                if id != "general"
                    && existing_ids.iter().any(|existing| existing == &id)
                    && !final_ids.contains(&id)
                {
                    final_ids.push(id);
                }
            }
            for id in existing_ids {
                if !final_ids.contains(&id) {
                    final_ids.push(id);
                }
            }

            let conn = inner
                .write_conn
                .lock()
                .expect("ui thread write connection mutex poisoned");
            conn.execute_batch("BEGIN TRANSACTION")
                .context("starting ui thread reorder transaction")?;
            for (index, id) in final_ids.into_iter().enumerate() {
                if let Err(error) = conn.execute(
                    "UPDATE ui_threads
                     SET sort_order = ?, updated_at = ?
                     WHERE principal = ? AND workspace = ? AND id = ?",
                    params![
                        index as i64,
                        Utc::now().timestamp_millis(),
                        principal.clone(),
                        workspace.clone(),
                        id,
                    ],
                ) {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(error).context("reordering ui threads");
                }
            }
            conn.execute_batch("COMMIT")
                .context("committing ui thread reorder transaction")?;
            inner
                .maybe_checkpoint(&conn)
                .context("checkpointing ui thread reorder")?;
            Ok(())
        })
        .await
    }

    pub async fn thread_ids_including_deleted(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<HashSet<String>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        self.spawn_ui_thread_blocking(
            principal.clone(),
            workspace.clone(),
            "list ids",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let conn = inner.read_connection()?;
                let mut stmt = conn.prepare(
                    "SELECT id FROM ui_threads
                 WHERE principal = ? AND workspace = ?",
                )?;
                let mut rows = stmt.query(params![principal, workspace])?;
                let mut ids = HashSet::new();
                while let Some(row) = rows.next()? {
                    ids.insert(row.get::<_, String>(0)?);
                }
                Ok(ids)
            },
        )
        .await
    }

    pub async fn delete_thread(&self, principal: &str, workspace: &str, id: &str) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let id = id.to_string();
        self.spawn_ui_thread_blocking(principal.clone(), workspace.clone(), "delete", move || {
            if id == "general" {
                return Ok(false);
            }
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("ui thread write connection mutex poisoned");
            let now = Utc::now().timestamp_millis();
            let changed = conn
                .execute(
                    "UPDATE ui_threads
                     SET archived = FALSE, deleted_at = ?, updated_at = ?
                     WHERE principal = ? AND workspace = ? AND id = ? AND deleted_at IS NULL",
                    params![now, now, principal, workspace, id],
                )
                .context("deleting ui thread")?;
            if changed > 0 {
                inner
                    .maybe_checkpoint(&conn)
                    .context("checkpointing ui thread delete")?;
            }
            Ok(changed > 0)
        })
        .await
    }

    pub fn base_root(&self) -> &Path {
        &self.base_root
    }

    pub async fn compact_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<crate::magician_v2::storage_governance::DuckDbCompactionReport> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        self.spawn_ui_thread_blocking(
            principal.clone(),
            workspace.clone(),
            "compaction",
            move || {
                let inner = store.scope_inner(&principal, &workspace)?;
                let template_schema = std::fs::read_to_string(
                    store.workspace_layout.ui_threads_db_template_schema_path(),
                )
                .context("reading UI thread template for compaction")?;
                let _write_guard = inner.acquire_write_guard()?;
                let mut connection = inner
                    .write_conn
                    .lock()
                    .expect("ui thread write connection mutex poisoned");
                let report = crate::magician_v2::storage_governance::compact_open_database(
                    &mut connection,
                    &inner.db_path,
                    |published| {
                        apply_schema_and_migrations(published, &template_schema)
                            .context("restoring UI thread schema after compaction")
                    },
                )?;
                *inner
                    .last_checkpoint_at
                    .lock()
                    .expect("ui thread checkpoint timestamp mutex poisoned") = Instant::now();
                Ok(report)
            },
        )
        .await
    }
}

impl UiThreadStoreInner {
    fn acquire_write_guard(&self) -> Result<CrossProcessWriteGuard<'_>> {
        let file = self
            .write_lock
            .lock()
            .expect("ui thread file lock mutex poisoned");
        file.lock_exclusive()
            .context("acquiring cross-process ui thread write lock")?;
        Ok(CrossProcessWriteGuard { file })
    }

    fn read_connection(&self) -> Result<Connection> {
        let config = Config::default()
            .access_mode(AccessMode::ReadOnly)
            .context("configuring read-only ui thread access mode")?;
        Connection::open_with_flags(&self.db_path, config).with_context(|| {
            format!(
                "opening ui thread store read connection at {}",
                self.db_path.display()
            )
        })
    }

    fn maybe_checkpoint(&self, conn: &Connection) -> Result<()> {
        let mut last = self
            .last_checkpoint_at
            .lock()
            .expect("ui thread checkpoint timestamp mutex poisoned");
        if last.elapsed() < CHECKPOINT_THROTTLE {
            return Ok(());
        }
        checkpoint_connection(conn)?;
        *last = Instant::now();
        Ok(())
    }
}

impl Drop for CrossProcessWriteGuard<'_> {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn checkpoint_connection(conn: &Connection) -> Result<()> {
    conn.execute_batch("CHECKPOINT")
        .context("running duckdb checkpoint")
}

fn apply_schema_and_migrations(conn: &Connection, template_schema: &str) -> Result<()> {
    let table_exists = conn
        .query_row(
            "SELECT COUNT(*) > 0
             FROM information_schema.tables
             WHERE table_name = 'ui_threads'",
            params![],
            |row| row.get::<_, bool>(0),
        )
        .context("checking for an existing ui thread table")?;

    // A current template can define indexes over columns that an older
    // database does not have yet. Upgrade the table before applying that
    // template so index creation cannot make the whole store unavailable.
    if table_exists {
        conn.execute_batch(MIGRATIONS)
            .context("running ui thread migrations before template schema")?;
    }
    conn.execute_batch(template_schema)
        .context("running ui thread template schema")?;
    conn.execute_batch(MIGRATIONS)
        .context("running ui thread migrations")?;
    Ok(())
}

fn template_schema_is_stale(schema: &str) -> bool {
    !schema.contains("display_mode")
        || !schema.contains("plan_mode")
        || !schema.contains("deleted_at")
        || !schema.contains("history_lane")
}

fn map_row(row: &duckdb::Row<'_>) -> Result<UiThreadRecord> {
    Ok(UiThreadRecord {
        principal: row.get(0)?,
        workspace: row.get(1)?,
        id: row.get(2)?,
        name: row.get(3)?,
        archived: row.get(4)?,
        sort_order: row.get(5)?,
        memory_summary: row.get(6)?,
        memory_updated_at: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        display_mode: row
            .get::<_, Option<String>>(10)
            .unwrap_or(None)
            .unwrap_or_else(|| "chat".to_string()),
        plan_mode: row
            .get::<_, Option<bool>>(11)
            .unwrap_or(None)
            .unwrap_or(false),
        history_lane: match row.get::<_, Option<String>>(12).unwrap_or(None).as_deref() {
            Some("automated") => HistoryLane::Automated,
            _ => HistoryLane::Personal,
        },
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{future::Future, time::Duration};

    use tempfile::TempDir;
    use tokio::{runtime::Builder, task::JoinHandle};

    use super::*;

    fn run_store_test(future: impl Future<Output = ()>) {
        build_test_runtime().block_on(future);
    }

    fn build_test_runtime() -> tokio::runtime::Runtime {
        for attempt in 0..12 {
            match Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => return runtime,
                Err(error) if matches!(error.raw_os_error(), Some(23 | 24)) && attempt < 11 => {
                    let multiplier = 1_u64 << attempt.min(6);
                    std::thread::sleep(Duration::from_millis(10 * multiplier));
                },
                Err(error) => panic!("failed building ui thread store test runtime: {error}"),
            }
        }
        unreachable!("runtime build retry loop returns before exhausting attempts")
    }

    #[test]
    fn upsert_and_list_threads_is_scope_isolated() {
        run_store_test(async {
            let tmp = TempDir::new().unwrap();
            let store = UiThreadStore::open(tmp.path()).unwrap();
            store
                .upsert_thread("alpha", "prod", "general", "general")
                .await
                .unwrap();
            store
                .upsert_thread("beta", "prod", "general", "general")
                .await
                .unwrap();

            let threads = store.list_threads("alpha", "prod").await.unwrap();
            assert_eq!(threads.len(), 1);
            assert_eq!(threads[0].principal, "alpha");
        });
    }

    #[test]
    fn history_page_filters_searches_and_reports_total() {
        run_store_test(async {
            let tmp = TempDir::new().unwrap();
            let store = UiThreadStore::open(tmp.path()).unwrap();
            store
                .upsert_thread("alpha", "prod", "general", "general")
                .await
                .unwrap();
            store
                .upsert_thread("alpha", "prod", "travel", "Travel Plans")
                .await
                .unwrap();
            store
                .upsert_thread("alpha", "prod", "discounts", "100% savings")
                .await
                .unwrap();
            store
                .upsert_thread_with_lane(
                    "alpha",
                    "prod",
                    "tabs",
                    "Observed tabs",
                    HistoryLane::Automated,
                )
                .await
                .unwrap();

            let personal = store
                .list_threads_page(
                    "alpha",
                    "prod",
                    Some(HistoryLane::Personal),
                    "travel",
                    10,
                    0,
                )
                .await
                .unwrap();
            assert_eq!(personal.total, 1);
            assert_eq!(personal.threads[0].id, "travel");

            let literal_wildcard = store
                .list_threads_page("alpha", "prod", Some(HistoryLane::Personal), "%", 10, 0)
                .await
                .unwrap();
            assert_eq!(literal_wildcard.total, 1);
            assert_eq!(literal_wildcard.threads[0].id, "discounts");

            let automated = store
                .list_threads_page("alpha", "prod", Some(HistoryLane::Automated), "", 1, 0)
                .await
                .unwrap();
            assert_eq!(automated.total, 1);
            assert_eq!(automated.threads[0].id, "tabs");

            let candidates = store
                .search_thread_candidates("alpha", "prod", "plan")
                .await
                .unwrap();
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].id, "travel");
        });
    }

    #[test]
    fn current_template_upgrades_legacy_table_before_creating_new_indexes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE ui_threads (
                principal TEXT NOT NULL,
                workspace TEXT NOT NULL,
                id TEXT NOT NULL,
                name TEXT NOT NULL,
                archived BOOLEAN NOT NULL DEFAULT FALSE,
                sort_order BIGINT NOT NULL DEFAULT 0,
                memory_summary TEXT NULL,
                memory_updated_at BIGINT NULL,
                created_at BIGINT NOT NULL,
                updated_at BIGINT NOT NULL,
                display_mode TEXT,
                plan_mode BOOLEAN,
                deleted_at BIGINT NULL,
                PRIMARY KEY (principal, workspace, id)
            );
            INSERT INTO ui_threads (
                principal, workspace, id, name, created_at, updated_at
            ) VALUES ('alpha', 'prod', 'agent-daily-brief', 'Daily brief', 1, 1);
            "#,
        )
        .unwrap();

        apply_schema_and_migrations(&conn, BOOTSTRAP_DDL).unwrap();

        let lane = conn
            .query_row(
                "SELECT history_lane FROM ui_threads WHERE id = 'agent-daily-brief'",
                params![],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(lane, "automated");
    }

    #[test]
    fn update_thread_preserves_general_unarchived() {
        run_store_test(async {
            let tmp = TempDir::new().unwrap();
            let store = UiThreadStore::open(tmp.path()).unwrap();
            store
                .upsert_thread("alpha", "prod", "general", "general")
                .await
                .unwrap();

            let updated = store
                .update_thread(
                    "alpha",
                    "prod",
                    "general",
                    UiThreadUpdate {
                        archived: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .unwrap()
                .unwrap();

            assert!(!updated.archived);
        });
    }

    #[test]
    fn delete_thread_hides_row_and_upsert_restores_it() {
        run_store_test(async {
            let tmp = TempDir::new().unwrap();
            let store = UiThreadStore::open(tmp.path()).unwrap();
            store
                .upsert_thread("alpha", "prod", "travel", "Travel")
                .await
                .unwrap();

            assert!(store
                .delete_thread("alpha", "prod", "travel")
                .await
                .unwrap());
            assert!(store
                .get_thread("alpha", "prod", "travel")
                .await
                .unwrap()
                .is_none());
            assert!(store
                .thread_ids_including_deleted("alpha", "prod")
                .await
                .unwrap()
                .contains("travel"));

            let restored = store
                .upsert_thread("alpha", "prod", "travel", "Travel Again")
                .await
                .unwrap();
            assert_eq!(restored.name, "Travel Again");
            assert!(!restored.archived);
            assert!(store
                .get_thread("alpha", "prod", "travel")
                .await
                .unwrap()
                .is_some());
        });
    }

    #[test]
    fn delete_thread_protects_general() {
        run_store_test(async {
            let tmp = TempDir::new().unwrap();
            let store = UiThreadStore::open(tmp.path()).unwrap();
            store
                .upsert_thread("alpha", "prod", "general", "general")
                .await
                .unwrap();

            assert!(!store
                .delete_thread("alpha", "prod", "general")
                .await
                .unwrap());
            assert!(store
                .get_thread("alpha", "prod", "general")
                .await
                .unwrap()
                .is_some());
        });
    }

    #[test]
    fn concurrent_upsert_thread_does_not_duplicate_primary_key() {
        run_store_test(async {
            let tmp = TempDir::new().unwrap();
            let store = UiThreadStore::open(tmp.path()).unwrap();

            let mut handles: Vec<JoinHandle<Result<UiThreadRecord>>> = Vec::new();
            for _ in 0..8 {
                let store = store.clone();
                handles.push(tokio::spawn(async move {
                    store
                        .upsert_thread("anonymous", "default", "general", "general")
                        .await
                }));
            }

            for handle in handles {
                handle.await.unwrap().unwrap();
            }

            let threads = store.list_threads("anonymous", "default").await.unwrap();
            assert_eq!(threads.len(), 1);
            assert_eq!(threads[0].id, "general");
        });
    }

    #[test]
    fn mutation_checkpoint_is_throttled_but_runs_after_the_window() {
        run_store_test(async {
            let tmp = TempDir::new().unwrap();
            let store = UiThreadStore::open(tmp.path()).unwrap();
            store
                .upsert_thread("alpha", "prod", "general", "general")
                .await
                .unwrap();
            let inner = store.scope_inner("alpha", "prod").unwrap();
            let first = *inner.last_checkpoint_at.lock().unwrap();

            store
                .upsert_thread("alpha", "prod", "notes", "Notes")
                .await
                .unwrap();
            assert_eq!(*inner.last_checkpoint_at.lock().unwrap(), first);

            *inner.last_checkpoint_at.lock().unwrap() =
                Instant::now() - CHECKPOINT_THROTTLE - Duration::from_secs(1);
            let expired = *inner.last_checkpoint_at.lock().unwrap();
            store
                .upsert_thread("alpha", "prod", "work", "Work")
                .await
                .unwrap();
            assert!(*inner.last_checkpoint_at.lock().unwrap() > expired);
        });
    }

    #[test]
    fn copy_compaction_preserves_active_rows_and_soft_delete_tombstones() {
        run_store_test(async {
            let tmp = TempDir::new().unwrap();
            let store = UiThreadStore::open(tmp.path()).unwrap();
            store
                .upsert_thread("alpha", "prod", "general", "general")
                .await
                .unwrap();
            store
                .update_thread(
                    "alpha",
                    "prod",
                    "general",
                    UiThreadUpdate {
                        display_mode: Some("dev".to_string()),
                        plan_mode: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            store
                .upsert_thread("alpha", "prod", "old", "Old thread")
                .await
                .unwrap();
            store.delete_thread("alpha", "prod", "old").await.unwrap();

            let report = store.compact_scope("alpha", "prod").await.unwrap();
            assert_eq!(report.row_count, 2);
            let general = store
                .get_thread("alpha", "prod", "general")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(general.display_mode, "dev");
            assert!(general.plan_mode);
            assert!(store
                .get_thread("alpha", "prod", "old")
                .await
                .unwrap()
                .is_none());
            assert!(store
                .thread_ids_including_deleted("alpha", "prod")
                .await
                .unwrap()
                .contains("old"));
        });
    }

    #[test]
    fn first_scoped_write_seeds_template_and_materializes_scoped_db() {
        run_store_test(async {
            let tmp = TempDir::new().unwrap();
            let store = UiThreadStore::open(tmp.path()).unwrap();
            store
                .upsert_thread("anonymous", "default", "general", "general")
                .await
                .unwrap();

            let workspace = ArtifactV2Workspace::new(tmp.path());
            assert!(workspace.ui_threads_db_template_schema_path().exists());
            assert!(workspace
                .ui_threads_db_path("anonymous", "default")
                .exists());
            assert!(workspace
                .ui_threads_lock_path("anonymous", "default")
                .exists());
        });
    }
}
